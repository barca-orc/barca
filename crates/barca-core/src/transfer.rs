//! Background artifact transfer between the local artifact dir and a separate
//! artifact store (remote URI or shared directory).
//!
//! Workers only ever write and read local files under
//! [`ResolvedConfig::local_artifact_dir`]. When the store is separate
//! ([`ResolvedConfig::remote_artifacts`]), one long-lived helper process —
//! `python -m barca._transfer`, so fsspec and its credential chains stay the
//! only cloud-auth surface — moves bytes in the background:
//!
//! - **upload**: after a step completes, its artifact is queued for upload
//!   while downstream steps keep running against the local copy;
//! - **fetch**: a cache hit recorded by another machine is downloaded to its
//!   local mirror path before any step consumes it;
//! - **drain**: before run results are persisted, every upload is awaited, so
//!   the metadata DB never references an artifact missing from the store.
//!
//! Local and store paths map by prefix substitution ([`ArtifactLayout`]): the
//! store keeps the worker's `{node}/{run_hash}{ext}` layout verbatim.

use crate::BarcaError;
use crate::config::ResolvedConfig;
use crate::protocol::{TransferReply, TransferRequest, read_frame, write_frame};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::net::UnixListener;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

// ─── Path mapping ────────────────────────────────────────────────────────────

/// Maps artifact paths between the local artifact dir and the store root.
#[derive(Debug, Clone)]
pub struct ArtifactLayout {
    /// Absolute, symlink-resolved local root (what workers are given).
    local_root: PathBuf,
    /// Store root without a trailing slash.
    store_root: String,
}

impl ArtifactLayout {
    /// `local_root` must be absolute; [`TransferClient::start`] canonicalizes it.
    pub fn new(local_root: impl Into<PathBuf>, store_root: &str) -> Self {
        Self {
            local_root: local_root.into(),
            store_root: store_root.trim_end_matches('/').to_string(),
        }
    }

    pub fn local_root(&self) -> &Path {
        &self.local_root
    }

    /// Store location of a local artifact path, or None when the path is not
    /// under the local root.
    pub fn store_for(&self, local: &str) -> Option<String> {
        let rel = Path::new(local).strip_prefix(&self.local_root).ok()?;
        let parts: Vec<&str> = rel
            .components()
            .map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect::<Option<_>>()?;
        if parts.is_empty() {
            return None;
        }
        Some(format!("{}/{}", self.store_root, parts.join("/")))
    }

    /// Local mirror path of a store location, or None when it is not under
    /// the store root (or would escape the local root).
    pub fn local_for(&self, stored: &str) -> Option<PathBuf> {
        let rel = stored.strip_prefix(&self.store_root)?.strip_prefix('/')?;
        let mut out = self.local_root.clone();
        for seg in rel.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." {
                return None;
            }
            out.push(seg);
        }
        Some(out)
    }
}

// ─── Client ──────────────────────────────────────────────────────────────────

/// A transfer that did not complete. `key` is the caller's label (node id).
#[derive(Debug, Clone)]
pub struct TransferFailure {
    pub key: String,
    pub store: String,
    pub message: String,
    /// Attempts made before giving up (0 if the request never reached a
    /// running helper).
    pub attempts: u32,
}

/// Outcome of [`TransferClient::drain`] / [`TransferClient::await_fetches`].
#[derive(Debug, Default)]
pub struct TransferReport {
    pub transferred: usize,
    pub bytes: u64,
    pub failures: Vec<TransferFailure>,
}

/// Bytes transferred, or the error message and attempts made.
type Outcome = Result<u64, (String, u32)>;
type ReplyRx = oneshot::Receiver<Outcome>;
type ReplyTx = oneshot::Sender<Outcome>;

struct Pending {
    key: String,
    store: String,
    rx: ReplyRx,
}

/// Handle to the transfer helper process.
pub struct TransferClient {
    layout: ArtifactLayout,
    child: Child,
    req_tx: mpsc::UnboundedSender<(TransferRequest, ReplyTx)>,
    io_task: JoinHandle<()>,
    socket_path: PathBuf,
    next_id: u64,
    uploads: Vec<Pending>,
    /// In-flight fetches keyed by local mirror path, so an artifact consumed
    /// by several steps is downloaded once.
    fetches: HashMap<PathBuf, Pending>,
}

impl TransferClient {
    /// Spawn `python -m barca._transfer` for this run's artifact store.
    pub async fn start(
        python: &Path,
        cfg: &ResolvedConfig,
        run_id: &str,
    ) -> Result<Self, BarcaError> {
        std::fs::create_dir_all(&cfg.local_artifact_dir)?;
        let local_root = std::fs::canonicalize(&cfg.local_artifact_dir)?;
        let layout = ArtifactLayout::new(local_root, &cfg.artifact_root);

        let mut cmd = Command::new(python);
        cmd.args(["-m", "barca._transfer"])
            .env(
                "BARCA_TRANSFER_CONCURRENCY",
                cfg.transfer_concurrency.to_string(),
            )
            .env(
                "BARCA_TRANSFER_TIMEOUT",
                cfg.transfer_timeout_secs.to_string(),
            );
        if let Some(ref opts) = cfg.storage_options_json {
            cmd.env("BARCA_STORAGE_OPTIONS", opts);
        }
        Self::spawn(
            cmd,
            crate::protocol::socket_path(run_id, "transfer"),
            layout,
        )
        .await
    }

    /// Spawn `cmd` as the helper (it receives `BARCA_SOCKET`) and wait for it
    /// to connect.
    pub async fn spawn(
        mut cmd: Command,
        socket_path: PathBuf,
        layout: ArtifactLayout,
    ) -> Result<Self, BarcaError> {
        std::fs::remove_file(&socket_path).ok();
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| BarcaError::Other(format!("transfer socket bind: {e}")))?;
        cmd.env("BARCA_SOCKET", &socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| BarcaError::Other(format!("failed to spawn transfer helper: {e}")))?;

        let stream = tokio::select! {
            accepted = tokio::time::timeout(Duration::from_secs(10), listener.accept()) => {
                accepted
                    .map_err(|_| BarcaError::Other("timeout waiting for transfer helper to connect".into()))?
                    .map_err(|e| BarcaError::Other(format!("transfer helper accept: {e}")))?
                    .0
            }
            status = child.wait() => {
                std::fs::remove_file(&socket_path).ok();
                return Err(BarcaError::Other(format!(
                    "transfer helper exited before connecting ({}) — is barca installed \
                     with the extras for this artifact store?",
                    status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string())
                )));
            }
        };

        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let io_task = tokio::spawn(io_task(stream, req_rx));
        Ok(Self {
            layout,
            child,
            req_tx,
            io_task,
            socket_path,
            next_id: 0,
            uploads: Vec::new(),
            fetches: HashMap::new(),
        })
    }

    pub fn layout(&self) -> &ArtifactLayout {
        &self.layout
    }

    fn send(&mut self, build: impl FnOnce(u64) -> TransferRequest) -> ReplyRx {
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        if let Err(mpsc::error::SendError((_, tx))) = self.req_tx.send((build(self.next_id), tx)) {
            let _ = tx.send(Err(("transfer helper is not running".to_string(), 0)));
        }
        rx
    }

    /// Queue an upload of a worker-reported local artifact. Returns its store
    /// location, or None when `local` is not under the local artifact dir.
    pub fn upload(&mut self, key: &str, local: &str) -> Option<String> {
        let store = self.layout.store_for(local)?;
        let rx = self.send(|id| TransferRequest::Put {
            id,
            local: local.to_string(),
            remote: store.clone(),
        });
        self.uploads.push(Pending {
            key: key.to_string(),
            store: store.clone(),
            rx,
        });
        Some(store)
    }

    /// Ensure the artifact stored at `store` is present at its local mirror
    /// path, queueing a download if needed. Returns the local path, or None
    /// when `store` is not under the store root.
    pub fn fetch(&mut self, key: &str, store: &str) -> Option<PathBuf> {
        let local = self.layout.local_for(store)?;
        if local.exists() || self.fetches.contains_key(&local) {
            return Some(local);
        }
        let rx = self.send(|id| TransferRequest::Get {
            id,
            remote: store.to_string(),
            local: local.to_string_lossy().into_owned(),
        });
        self.fetches.insert(
            local.clone(),
            Pending {
                key: key.to_string(),
                store: store.to_string(),
                rx,
            },
        );
        Some(local)
    }

    /// Wait for the fetches of `locals` (paths not being fetched are ready).
    pub async fn await_fetches(&mut self, locals: &[PathBuf]) -> TransferReport {
        let mut report = TransferReport::default();
        for local in locals {
            let Some(p) = self.fetches.remove(local) else {
                continue;
            };
            match settle(p.rx).await {
                Ok(bytes) => {
                    report.transferred += 1;
                    report.bytes += bytes;
                }
                Err((message, attempts)) => report.failures.push(TransferFailure {
                    key: p.key,
                    store: p.store,
                    message,
                    attempts,
                }),
            }
        }
        report
    }

    /// Number of uploads queued since the last drain.
    pub fn pending_uploads(&self) -> usize {
        self.uploads.len()
    }

    /// Await every upload queued since the last drain.
    pub async fn drain(&mut self) -> TransferReport {
        let mut report = TransferReport::default();
        let mut inflight: FuturesUnordered<_> = self
            .uploads
            .drain(..)
            .map(|p| async move {
                let r = settle(p.rx).await;
                (p.key, p.store, r)
            })
            .collect();
        while let Some((key, store, r)) = inflight.next().await {
            match r {
                Ok(bytes) => {
                    report.transferred += 1;
                    report.bytes += bytes;
                }
                Err((message, attempts)) => report.failures.push(TransferFailure {
                    key,
                    store,
                    message,
                    attempts,
                }),
            }
        }
        report
    }

    /// Finish in-flight transfers and stop the helper.
    pub async fn shutdown(mut self) {
        let _ = self
            .req_tx
            .send((TransferRequest::Shutdown, oneshot::channel().0));
        if tokio::time::timeout(Duration::from_secs(30), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        self.io_task.abort();
        std::fs::remove_file(&self.socket_path).ok();
    }

    /// Stop the helper immediately, abandoning queued transfers. Returns the
    /// keys of uploads not confirmed complete — their artifacts may be
    /// missing from the store, so they must not be recorded.
    pub async fn abort(mut self) -> Vec<String> {
        let unconfirmed = self
            .uploads
            .iter_mut()
            .filter_map(|p| (!matches!(p.rx.try_recv(), Ok(Ok(_)))).then(|| p.key.clone()))
            .collect();
        let _ = self.child.kill().await;
        self.io_task.abort();
        std::fs::remove_file(&self.socket_path).ok();
        unconfirmed
    }
}

async fn settle(rx: ReplyRx) -> Outcome {
    rx.await
        .unwrap_or_else(|_| Err(("transfer helper exited".to_string(), 1)))
}

/// Owns the socket: writes requests, routes replies to their waiters. On
/// disconnect, every outstanding waiter is failed (dropping its sender), so
/// no caller can hang on a dead helper.
async fn io_task(
    mut stream: tokio::net::UnixStream,
    mut req_rx: mpsc::UnboundedReceiver<(TransferRequest, ReplyTx)>,
) {
    let trace = std::env::var("BARCA_TRACE_TIMING").is_ok();
    let mut pending: HashMap<u64, (ReplyTx, Instant, String)> = HashMap::new();
    loop {
        tokio::select! {
            req = req_rx.recv() => {
                let Some((req, tx)) = req else { break };
                let entry = match &req {
                    TransferRequest::Put { id, remote, .. } => Some((*id, format!("put {remote}"))),
                    TransferRequest::Get { id, remote, .. } => Some((*id, format!("get {remote}"))),
                    TransferRequest::Shutdown => None,
                };
                if write_frame(&mut stream, &req).await.is_err() {
                    let _ = tx.send(Err(("transfer helper exited".to_string(), 1)));
                    break;
                }
                if let Some((id, what)) = entry {
                    pending.insert(id, (tx, Instant::now(), what));
                }
            }
            reply = read_frame::<_, TransferReply>(&mut stream) => {
                let (id, result) = match reply {
                    Ok(Some(TransferReply::Done { id, size_bytes })) => (id, Ok(size_bytes)),
                    Ok(Some(TransferReply::Error { id, message, attempts })) => {
                        (id, Err((message, attempts)))
                    }
                    Ok(None) | Err(_) => break,
                };
                if let Some((tx, t0, what)) = pending.remove(&id) {
                    if trace {
                        eprintln!(
                            "[trace]  transfer {what} {} in {:.1}ms",
                            if result.is_ok() { "done" } else { "FAILED" },
                            t0.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                    let _ = tx.send(result);
                }
            }
        }
    }
    // Fail everything still waiting, plus requests that arrive later.
    drop(pending);
    req_rx.close();
    while let Some((_, tx)) = req_rx.recv().await {
        let _ = tx.send(Err(("transfer helper exited".to_string(), 1)));
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ArtifactLayout ──────────────────────────────────────────────────────

    #[test]
    fn layout_round_trips_local_and_store_paths() {
        let l = ArtifactLayout::new("/w/.barca/artifacts", "s3://b/p/default/artifacts");
        let store = l
            .store_for("/w/.barca/artifacts/mod.py__numbers/abc123.json")
            .unwrap();
        assert_eq!(
            store,
            "s3://b/p/default/artifacts/mod.py__numbers/abc123.json"
        );
        assert_eq!(
            l.local_for(&store).unwrap(),
            PathBuf::from("/w/.barca/artifacts/mod.py__numbers/abc123.json")
        );
    }

    #[test]
    fn layout_handles_trailing_slash_store_root_and_plain_path_store() {
        let l = ArtifactLayout::new("/w/a", "/mnt/shared/default/artifacts/");
        assert_eq!(
            l.store_for("/w/a/n/h.pkl").unwrap(),
            "/mnt/shared/default/artifacts/n/h.pkl"
        );
        assert_eq!(
            l.local_for("/mnt/shared/default/artifacts/n/h.pkl")
                .unwrap(),
            PathBuf::from("/w/a/n/h.pkl")
        );
    }

    #[test]
    fn layout_keeps_partitioned_leaf_names() {
        let l = ArtifactLayout::new("/w/a", "gs://b/x");
        let local = "/w/a/m.py__daily[date=2026-01-01]/h.parquet";
        let store = l.store_for(local).unwrap();
        assert_eq!(store, "gs://b/x/m.py__daily[date=2026-01-01]/h.parquet");
        assert_eq!(l.local_for(&store).unwrap(), PathBuf::from(local));
    }

    #[test]
    fn layout_rejects_paths_outside_either_root() {
        let l = ArtifactLayout::new("/w/a", "s3://b/x");
        assert!(l.store_for("/elsewhere/n/h.json").is_none());
        assert!(l.store_for("/w/a").is_none());
        assert!(l.store_for("/w/ab/n/h.json").is_none()); // prefix, not a parent
        assert!(l.local_for("s3://b/y/n/h.json").is_none());
        assert!(l.local_for("s3://b/xy/n/h.json").is_none());
        assert!(l.local_for("/w/a/n/h.json").is_none()); // legacy local row
        assert!(l.local_for("s3://b/x/../../etc/passwd").is_none());
        assert!(l.local_for("s3://b/x//h").is_none());
    }

    // ── TransferClient vs a fake helper ─────────────────────────────────────
    //
    // A stdlib-only stand-in speaking the same protocol, so these tests pin
    // the Rust side independent of barca._transfer. The store is a plain
    // directory. A store path containing "fail" errors; "die" makes the helper
    // exit without replying; "slow" sleeps first.

    const FAKE_HELPER: &str = r#"
import json, os, shutil, socket, struct, sys, threading, time
s = socket.socket(socket.AF_UNIX); s.connect(os.environ["BARCA_SOCKET"])
lock = threading.Lock()
log = os.environ.get("FAKE_LOG")
def recv():
    h = b""
    while len(h) < 4:
        c = s.recv(4 - len(h))
        if not c: return None
        h += c
    n = struct.unpack(">I", h)[0]; b = b""
    while len(b) < n: b += s.recv(n - len(b))
    return json.loads(b)
def send(m):
    b = json.dumps(m).encode()
    with lock: s.sendall(struct.pack(">I", len(b)) + b)
def handle(m):
    src, dst = (m["local"], m["remote"]) if m["type"] == "put" else (m["remote"], m["local"])
    if "slow" in m["remote"]: time.sleep(0.3)
    if "die" in m["remote"]: os._exit(3)
    if "fail" in m["remote"]:
        return send({"type": "error", "id": m["id"], "message": "PermissionError: denied", "attempts": 2})
    os.makedirs(os.path.dirname(dst), exist_ok=True); shutil.copyfile(src, dst)
    send({"type": "done", "id": m["id"], "size_bytes": os.path.getsize(dst)})
threads = []
while True:
    m = recv()
    if m is None: break
    if log:
        with open(log, "a") as f: f.write(m["type"] + "\n")
    if m["type"] == "shutdown": break
    t = threading.Thread(target=handle, args=(m,)); t.start(); threads.append(t)
for t in threads: t.join()
"#;

    struct Fixture {
        _dir: tempfile::TempDir,
        local: PathBuf,
        store: PathBuf,
        log: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let local = root.join("local");
            let store = root.join("store");
            std::fs::create_dir_all(&local).unwrap();
            std::fs::create_dir_all(&store).unwrap();
            let log = root.join("requests.log");
            Self {
                _dir: dir,
                local,
                store,
                log,
            }
        }

        fn layout(&self) -> ArtifactLayout {
            ArtifactLayout::new(&self.local, self.store.to_str().unwrap())
        }

        fn write_local(&self, rel: &str, body: &[u8]) -> String {
            let p = self.local.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p.to_string_lossy().into_owned()
        }

        fn write_store(&self, rel: &str, body: &[u8]) -> String {
            let p = self.store.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p.to_string_lossy().into_owned()
        }

        fn requests(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        async fn client(&self) -> TransferClient {
            let mut cmd = Command::new("python3");
            cmd.args(["-c", FAKE_HELPER]).env("FAKE_LOG", &self.log);
            let sock = crate::protocol::socket_path(
                &format!("xfer-test-{}-{}", std::process::id(), rand_suffix()),
                "transfer",
            );
            TransferClient::spawn(cmd, sock, self.layout())
                .await
                .unwrap()
        }
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(10), f)
            .await
            .expect("transfer client hung")
    }

    #[tokio::test]
    async fn uploads_drain_clean_and_land_in_the_store() {
        let fx = Fixture::new();
        let mut c = fx.client().await;
        for i in 0..6 {
            let local = fx.write_local(&format!("n{i}/h.json"), &vec![b'x'; i + 1]);
            let store = c.upload(&format!("node{i}"), &local).unwrap();
            assert_eq!(
                store,
                fx.store.join(format!("n{i}/h.json")).to_str().unwrap()
            );
        }
        assert_eq!(c.pending_uploads(), 6);
        let report = within(c.drain()).await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.transferred, 6);
        assert_eq!(report.bytes, (1..=6).sum::<u64>());
        assert_eq!(c.pending_uploads(), 0);
        for i in 0..6 {
            assert!(fx.store.join(format!("n{i}/h.json")).exists());
        }
        within(c.shutdown()).await;
    }

    #[tokio::test]
    async fn upload_outside_local_root_is_not_queued() {
        let fx = Fixture::new();
        let mut c = fx.client().await;
        assert!(c.upload("p", "/tmp/elsewhere/x.json").is_none());
        assert_eq!(c.pending_uploads(), 0);
        within(c.shutdown()).await;
    }

    #[tokio::test]
    async fn failed_upload_is_reported_with_key_and_store() {
        let fx = Fixture::new();
        let mut c = fx.client().await;
        let ok = fx.write_local("ok/h.json", b"1");
        let bad = fx.write_local("fail/h.json", b"1");
        c.upload("good", &ok).unwrap();
        c.upload("bad", &bad).unwrap();
        let report = within(c.drain()).await;
        assert_eq!(report.transferred, 1);
        assert_eq!(report.failures.len(), 1);
        let f = &report.failures[0];
        assert_eq!(f.key, "bad");
        assert!(f.store.ends_with("fail/h.json"));
        assert_eq!(f.message, "PermissionError: denied");
        assert_eq!(f.attempts, 2);
        within(c.shutdown()).await;
    }

    #[tokio::test]
    async fn fetch_downloads_to_local_mirror() {
        let fx = Fixture::new();
        let store = fx.write_store("n/h.json", b"{\"v\":1}");
        let mut c = fx.client().await;
        let local = c.fetch("n", &store).unwrap();
        assert_eq!(local, fx.local.join("n/h.json"));
        let failures = within(c.await_fetches(std::slice::from_ref(&local)))
            .await
            .failures;
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(std::fs::read(&local).unwrap(), b"{\"v\":1}");
        within(c.shutdown()).await;
    }

    #[tokio::test]
    async fn fetch_of_existing_local_file_sends_nothing() {
        let fx = Fixture::new();
        let store = fx.write_store("n/h.json", b"1");
        fx.write_local("n/h.json", b"1");
        let mut c = fx.client().await;
        let local = c.fetch("n", &store).unwrap();
        assert!(within(c.await_fetches(&[local])).await.failures.is_empty());
        within(c.shutdown()).await;
        assert_eq!(fx.requests(), vec!["shutdown"]);
    }

    #[tokio::test]
    async fn duplicate_fetches_are_requested_once() {
        let fx = Fixture::new();
        let store = fx.write_store("n/h.json", b"1");
        let mut c = fx.client().await;
        let a = c.fetch("x", &store).unwrap();
        let b = c.fetch("y", &store).unwrap();
        assert_eq!(a, b);
        assert!(within(c.await_fetches(&[a, b])).await.failures.is_empty());
        within(c.shutdown()).await;
        assert_eq!(fx.requests(), vec!["get", "shutdown"]);
    }

    #[tokio::test]
    async fn fetch_outside_store_root_is_none() {
        let fx = Fixture::new();
        let mut c = fx.client().await;
        assert!(c.fetch("n", "/somewhere/else/h.json").is_none());
        within(c.shutdown()).await;
    }

    #[tokio::test]
    async fn failed_fetch_is_reported() {
        let fx = Fixture::new();
        let store = fx.write_store("fail/h.json", b"1");
        let mut c = fx.client().await;
        let local = c.fetch("n", &store).unwrap();
        let failures = within(c.await_fetches(std::slice::from_ref(&local)))
            .await
            .failures;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].key, "n");
        assert!(!local.exists());
        within(c.shutdown()).await;
    }

    #[tokio::test]
    async fn helper_death_fails_pending_and_later_requests_instead_of_hanging() {
        let fx = Fixture::new();
        let mut c = fx.client().await;
        let ok = fx.write_local("slow/h.json", b"1");
        let dying = fx.write_local("die/h.json", b"1");
        c.upload("slow", &ok).unwrap();
        c.upload("die", &dying).unwrap();
        // Give the helper time to exit, then queue more work against it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let late = fx.write_local("late/h.json", b"1");
        c.upload("late", &late).unwrap();
        let report = within(c.drain()).await;
        let failed: std::collections::HashSet<_> =
            report.failures.iter().map(|f| f.key.as_str()).collect();
        assert!(failed.contains("die"));
        assert!(failed.contains("late"));
        assert!(
            report
                .failures
                .iter()
                .all(|f| f.message.contains("transfer helper")),
            "{:?}",
            report.failures
        );
        within(c.abort()).await;
    }

    #[tokio::test]
    async fn abort_returns_uploads_not_confirmed() {
        let fx = Fixture::new();
        let mut c = fx.client().await;
        let fast = fx.write_local("fast/h.json", b"1");
        let slow = fx.write_local("slow/h.json", b"1");
        let bad = fx.write_local("fail/h.json", b"1");
        c.upload("fast", &fast).unwrap();
        c.upload("bad", &bad).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        c.upload("slow", &slow).unwrap();
        let mut un = within(c.abort()).await;
        un.sort();
        assert_eq!(un, vec!["bad", "slow"]);
    }

    #[tokio::test]
    async fn drain_after_earlier_drain_only_waits_for_new_uploads() {
        let fx = Fixture::new();
        let mut c = fx.client().await;
        let a = fx.write_local("a/h.json", b"1");
        c.upload("a", &a).unwrap();
        assert_eq!(within(c.drain()).await.transferred, 1);
        let b = fx.write_local("b/h.json", b"22");
        c.upload("b", &b).unwrap();
        let r = within(c.drain()).await;
        assert_eq!((r.transferred, r.bytes), (1, 2));
        within(c.shutdown()).await;
    }

    #[tokio::test]
    async fn spawn_reports_a_helper_that_exits_before_connecting() {
        let fx = Fixture::new();
        let mut cmd = Command::new("python3");
        cmd.args(["-c", "import sys; sys.exit(2)"]);
        let sock = crate::protocol::socket_path(
            &format!("xfer-test-dead-{}", std::process::id()),
            "transfer",
        );
        let started = Instant::now();
        let err = match TransferClient::spawn(cmd, sock, fx.layout()).await {
            Ok(_) => panic!("spawn should fail"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("exited before connecting"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should not wait for the accept timeout"
        );
    }

    // ── TransferClient vs the real barca._transfer helper ───────────────────

    /// The repo venv's python, when barca is importable there.
    fn repo_python() -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.venv/bin/python");
        let ok = std::process::Command::new(&p)
            .args(["-c", "import barca._transfer"])
            .output()
            .ok()?
            .status
            .success();
        ok.then_some(p)
    }

    #[tokio::test]
    async fn real_helper_round_trip_through_plain_path_store() {
        let Some(python) = repo_python() else {
            eprintln!("SKIP: no .venv with barca installed");
            return;
        };
        let fx = Fixture::new();
        let mut cmd = Command::new(python);
        cmd.args(["-m", "barca._transfer"]);
        let sock = crate::protocol::socket_path(
            &format!("xfer-test-real-{}", std::process::id()),
            "transfer",
        );
        let mut c = TransferClient::spawn(cmd, sock, fx.layout()).await.unwrap();

        let local = fx.write_local("m.py__a/h1.json", b"[1,2,3]");
        let store = c.upload("a", &local).unwrap();
        let report = within(c.drain()).await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(std::fs::read(&store).unwrap(), b"[1,2,3]");

        std::fs::remove_file(&local).unwrap();
        let back = c.fetch("a", &store).unwrap();
        let fetched = within(c.await_fetches(std::slice::from_ref(&back))).await;
        assert!(fetched.failures.is_empty());
        assert_eq!((fetched.transferred, fetched.bytes), (1, 7));
        assert_eq!(std::fs::read(&back).unwrap(), b"[1,2,3]");

        let missing = c
            .fetch("gone", &format!("{}/gone/h.json", fx.store.display()))
            .unwrap();
        let failures = within(c.await_fetches(&[missing])).await.failures;
        assert_eq!(failures.len(), 1);
        assert!(
            failures[0].message.contains("FileNotFoundError"),
            "{failures:?}"
        );
        within(c.shutdown()).await;
    }
}
