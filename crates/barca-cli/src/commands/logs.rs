//! `barca logs`: a run's recorded step output (stdout and stderr), independently of progress
//! and result rendering. Locally it reads the metadata DB, and the spool of a run still in
//! progress (`barca_core::run_logs`); against a server, `client::logs`.

use barca_core::db::LogEntry;
use barca_core::{BarcaError, LogStream};
use std::time::{Duration, Instant};

/// What `barca logs` prints, wherever the lines come from.
#[derive(Clone, Copy)]
pub(crate) struct LogsArgs<'a> {
    pub(crate) step: Option<&'a str>,
    pub(crate) stream: Option<LogStream>,
    pub(crate) follow: bool,
    pub(crate) timestamps: bool,
    pub(crate) json: bool,
}

impl LogsArgs<'_> {
    /// Whether a line arriving while following is one to print. Nothing is checked against
    /// the run's steps: a step that has not started yet has no lines.
    pub(crate) fn selects(&self, node_id: &str, stream: LogStream) -> bool {
        self.stream.is_none_or(|s| s == stream)
            && self
                .step
                .is_none_or(|step| barca_core::queries::step_matches(node_id, step))
    }

    /// Print lines read all at once: a JSON array, or JSON Lines while following.
    pub(crate) fn print_all(&self, lines: &[LogEntry]) -> Result<(), BarcaError> {
        if self.json && !self.follow {
            let entries: Vec<_> = lines.iter().map(entry_json).collect();
            barca_core::outln!("[{}]", entries.join(","));
        } else {
            for line in lines {
                self.print_one(line);
            }
        }
        Ok(())
    }

    /// Print one line as it arrives: its text (after its time with --timestamps), or with
    /// --json one object on a line of its own.
    pub(crate) fn print_one(&self, entry: &LogEntry) {
        if self.json {
            barca_core::outln!("{}", entry_json(entry));
        } else if self.timestamps {
            barca_core::outln!(
                "{} {}",
                entry.ts.as_deref().unwrap_or(UNKNOWN_TS),
                entry.line
            );
        } else {
            barca_core::outln!("{}", entry.line);
        }
    }
}

/// The time column of a line recorded before barca stamped lines.
const UNKNOWN_TS: &str = "-                       ";

/// One line in `--json`: `{node_id, stream, ts, line}`, in that order.
#[derive(serde::Serialize)]
struct EntryJson<'a> {
    node_id: &'a str,
    stream: LogStream,
    ts: Option<&'a str>,
    line: &'a str,
}

fn entry_json(entry: &LogEntry) -> String {
    serde_json::to_string(&EntryJson {
        node_id: &entry.node_id,
        stream: entry.stream,
        ts: entry.ts.as_deref(),
        line: &entry.line,
    })
    .unwrap_or_default()
}

/// The exit code `-f` ends with, from the run's status: 0 succeeded, 130 cancelled, 1 otherwise.
pub(crate) fn follow_exit_code(status: &str) -> i32 {
    match status {
        "success" => 0,
        "cancelled" => 130,
        _ => 1,
    }
}

/// The stderr line that ends `-f`.
pub(crate) fn follow_ended(run_id: &str, status: &str) {
    barca_core::errln!("barca: run {run_id} ended: {status}");
}

/// The stderr line naming the run read without a run id.
pub(crate) fn name_run(run: &barca_core::db::RunRecord, follow: bool) {
    let what = if follow && run.status == "running" {
        "following the newest running run"
    } else {
        "logs of the most recent run"
    };
    barca_core::errln!(
        "barca: {what}: {} ({} {}, {}, started {})",
        run.run_id,
        run.command,
        barca_core::report::history_target(run),
        run.status,
        barca_core::report::utc_cell(&run.started_at)
    );
}

/// How often `-f` looks for new lines and for the run's end.
const POLL: Duration = Duration::from_millis(200);

/// How long `-f` waits, after the run is recorded as ended, for its lines to move from the
/// spool into the database.
const WRAP_UP: Duration = Duration::from_secs(10);

/// Without a run id, reads the most recent run (with `-f`, the newest running one) and names
/// it on stderr.
pub(crate) async fn logs_cmd(
    env: Option<&str>,
    run_id: Option<&str>,
    args: LogsArgs<'_>,
) -> Result<(), BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let run_id = match run_id {
        Some(id) => id.to_string(),
        None => {
            let run = barca_core::queries::run_to_read(&cfg, args.follow).await?;
            name_run(&run, args.follow);
            run.run_id
        }
    };
    if !args.follow {
        let lines = barca_core::queries::logs(&cfg, &run_id, args.step, args.stream).await?;
        return args.print_all(&lines);
    }
    let status = follow_local(&cfg, &run_id, args).await?;
    follow_ended(&run_id, &status);
    barca_core::term::exit(follow_exit_code(&status));
}

/// `-f` on this machine: print the run's lines from its spool as they arrive, and once the
/// run is recorded as ended, the rest from the database. Returns the run's final status.
async fn follow_local(
    cfg: &barca_core::config::ResolvedConfig,
    run_id: &str,
    args: LogsArgs<'_>,
) -> Result<String, BarcaError> {
    let Some(run) = barca_core::queries::run_record(cfg, run_id).await? else {
        return Err(BarcaError::Usage(format!(
            "run '{run_id}' was not found; use barca history --json to find a run id"
        )));
    };
    if run.status != "running" {
        // Ended already: print what it recorded, checked as without -f.
        let lines = barca_core::queries::logs(cfg, run_id, args.step, args.stream).await?;
        args.print_all(&lines)?;
        return Ok(run.status);
    }
    let spool = barca_core::run_logs::spool_path(&cfg.db_path, run_id);
    // Lines seen so far (printed or not), which is the seq of the next one.
    let mut seen = 0usize;
    let mut offset = 0u64;
    let mut ended_at: Option<Instant> = None;
    loop {
        let status = barca_core::db::get_run(&cfg.db_path, run_id)
            .await?
            .map_or_else(|| "interrupted".to_string(), |r| r.status);
        let (lines, at) = barca_core::run_logs::read_spool(&spool, offset)?;
        offset = at;
        for line in lines {
            let entry = LogEntry::at(seen, line);
            seen += 1;
            if args.selects(&entry.node_id, entry.stream) {
                args.print_one(&entry);
            }
        }
        if status != "running" {
            let since = *ended_at.get_or_insert_with(Instant::now);
            // The run deletes its spool once its lines are in the database. One whose process
            // died leaves it, and everything it holds has been printed.
            if spool.exists() && since.elapsed() < WRAP_UP {
                tokio::time::sleep(POLL).await;
                continue;
            }
            for entry in barca_core::db::get_logs(&cfg.db_path, run_id).await? {
                if entry.seq as usize >= seen {
                    seen = entry.seq as usize + 1;
                    if args.selects(&entry.node_id, entry.stream) {
                        args.print_one(&entry);
                    }
                }
            }
            return Ok(status);
        }
        tokio::time::sleep(POLL).await;
    }
}
