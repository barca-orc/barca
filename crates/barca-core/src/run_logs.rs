//! A run's recorded output: what its steps wrote to stdout and stderr, line by line.
//!
//! While a run goes on, each line is kept in memory (it lands in the `logs` table, in one
//! transaction, when the run is recorded) and appended to a spool file,
//! `<metadata dir>/live-logs/<run_id>.jsonl`, so another process (`barca logs -f`, `barca serve`
//! answering `GET /runs/{id}/logs`) can read the lines of a run still in progress. The spool is
//! written by a thread of its own: lines that arrive together go out in one write, and a line
//! that arrives alone goes out at once. The run deletes the file once its lines are in the
//! database; a run whose process died leaves it, and readers fall back to it.
//!
//! A run records at most [`LOG_CAP_BYTES`] of line text. The line that reaches the cap is
//! followed by one marker line ([`cap_marker`]) and nothing more is recorded; every line still
//! reaches the terminal (and a `barca serve` client) live.

use crate::LogStream;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// The most line text one run records: 16 MiB (some 300,000 lines of 50 characters).
pub const LOG_CAP_BYTES: u64 = 16 * 1024 * 1024;

/// One recorded line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedLine {
    pub node_id: String,
    pub line: String,
    #[serde(default)]
    pub stream: LogStream,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
}

impl RecordedLine {
    /// A note of barca's own in the run's log (no node, on stderr), stamped now.
    pub fn barca(line: String) -> Self {
        Self {
            node_id: String::new(),
            line,
            stream: LogStream::Stderr,
            ts: Some(crate::events::log_timestamp()),
        }
    }
}

/// The line recorded in place of everything past the cap.
pub fn cap_marker() -> String {
    format!(
        "[barca] log cap reached: this run recorded {} MiB of output; later lines were shown live \
         but not recorded",
        LOG_CAP_BYTES / (1024 * 1024)
    )
}

/// Where a run's lines are spooled while it runs, beside the metadata DB at `db_path`.
pub fn spool_path(db_path: &str, run_id: &str) -> PathBuf {
    Path::new(db_path)
        .parent()
        .unwrap_or(Path::new("."))
        .join("live-logs")
        .join(format!("{run_id}.jsonl"))
}

/// Read a spool file's complete lines from byte `offset`. Returns the lines and the offset
/// after the last complete one (a line still being written is left for the next read). A
/// missing file reads as nothing.
pub fn read_spool(path: &Path, offset: u64) -> std::io::Result<(Vec<RecordedLine>, u64)> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), offset)),
        Err(e) => return Err(e),
    };
    let mut reader = std::io::BufReader::new(file);
    std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(offset))?;
    let mut out = Vec::new();
    let mut at = offset;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        at += n as u64;
        if let Ok(line) = serde_json::from_slice::<RecordedLine>(&buf) {
            out.push(line);
        }
    }
    Ok((out, at))
}

/// Collects a run's lines: in memory for the database, and in the spool file for readers.
#[derive(Default)]
pub struct Recorder {
    lines: Vec<RecordedLine>,
    bytes: u64,
    capped: bool,
    spool: Option<Spool>,
}

impl Recorder {
    /// Spool this run's lines to `path` as they arrive (created on the first line).
    pub fn spool_to(&mut self, path: PathBuf) {
        if self.spool.is_none() {
            self.spool = Some(Spool::start(path));
        }
    }

    /// Record a line, unless the run is past its cap. The line that crosses the cap is
    /// recorded, then the marker.
    pub fn push(&mut self, line: RecordedLine) {
        if self.capped {
            return;
        }
        self.bytes += line.line.len() as u64;
        let node_id = line.node_id.clone();
        self.record(line);
        if self.bytes >= LOG_CAP_BYTES {
            self.capped = true;
            let mut marker = RecordedLine::barca(cap_marker());
            marker.node_id = node_id;
            self.record(marker);
        }
    }

    /// Record a note of barca's own, cap or not.
    pub fn push_note(&mut self, line: String) {
        self.record(RecordedLine::barca(line));
    }

    fn record(&mut self, line: RecordedLine) {
        if let Some(spool) = &self.spool {
            spool.send(&line);
        }
        self.lines.push(line);
    }

    pub fn lines(&self) -> &[RecordedLine] {
        &self.lines
    }

    /// Stop spooling (every line sent is written) and return the spool's path, to delete
    /// once the lines are in the database.
    pub fn close_spool(&mut self) -> Option<PathBuf> {
        self.spool.take().map(Spool::close)
    }
}

/// The spool writer: a thread that appends what it is sent.
struct Spool {
    path: PathBuf,
    tx: Option<std::sync::mpsc::Sender<String>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Spool {
    fn start(path: PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let at = path.clone();
        let thread = std::thread::Builder::new()
            .name("barca-log-spool".into())
            .spawn(move || write_spool(&at, rx))
            .ok();
        Self {
            path,
            tx: thread.is_some().then_some(tx),
            thread,
        }
    }

    fn send(&self, line: &RecordedLine) {
        if let (Some(tx), Ok(mut text)) = (&self.tx, serde_json::to_string(line)) {
            text.push('\n');
            let _ = tx.send(text);
        }
    }

    fn close(mut self) -> PathBuf {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        std::mem::take(&mut self.path)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The spool thread: block for a line, write it and whatever else is already waiting, flush.
/// A spool that cannot be written is given up quietly: the lines still reach the database.
fn write_spool(path: &Path, rx: std::sync::mpsc::Receiver<String>) {
    let Ok(first) = rx.recv() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, file);
    let mut next = Some(first);
    while let Some(text) = next {
        if out.write_all(text.as_bytes()).is_err() {
            return;
        }
        next = match rx.try_recv() {
            Ok(text) => Some(text),
            Err(_) => {
                if out.flush().is_err() {
                    return;
                }
                rx.recv().ok()
            }
        };
    }
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(node: &str, text: &str, stream: LogStream) -> RecordedLine {
        RecordedLine {
            node_id: node.into(),
            line: text.into(),
            stream,
            ts: Some("2026-10-22T10:00:00.000Z".into()),
        }
    }

    #[test]
    fn spooled_lines_read_back_in_order_and_from_an_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live-logs").join("r.jsonl");
        let mut recorder = Recorder::default();
        recorder.spool_to(path.clone());
        recorder.push(line("p.py:a", "one", LogStream::Stdout));
        recorder.push(line("p.py:a", "two", LogStream::Stderr));
        assert_eq!(recorder.close_spool(), Some(path.clone()));
        let (all, end) = read_spool(&path, 0).unwrap();
        assert_eq!(all, recorder.lines());
        let (first, _) = read_spool(&path, 0).unwrap();
        let first_len = serde_json::to_string(&first[0]).unwrap().len() as u64 + 1;
        let (rest, at) = read_spool(&path, first_len).unwrap();
        assert_eq!(rest, &recorder.lines()[1..]);
        assert_eq!(at, end);
        // A half-written line is left for the next read; a missing file is empty.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"node_id\":")
            .unwrap();
        assert_eq!(read_spool(&path, end).unwrap(), (Vec::new(), end));
        assert_eq!(
            read_spool(&dir.path().join("none"), 0).unwrap(),
            (Vec::new(), 0)
        );
    }

    #[test]
    fn the_cap_records_one_marker_then_nothing() {
        let mut recorder = Recorder::default();
        let big = "x".repeat((LOG_CAP_BYTES / 2) as usize);
        recorder.push(line("p.py:a", &big, LogStream::Stdout));
        recorder.push(line("p.py:a", &big, LogStream::Stdout));
        recorder.push(line("p.py:a", "lost", LogStream::Stdout));
        let lines = recorder.lines();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].line, cap_marker());
        assert_eq!(lines[2].node_id, "p.py:a");
        assert_eq!(lines[2].stream, LogStream::Stderr);
        // barca's own notes are still recorded.
        recorder.push_note("[barca] run cancelled: test".into());
        assert_eq!(recorder.lines().len(), 4);
    }
}
