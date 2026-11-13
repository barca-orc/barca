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

#[test]
fn diagnostic_locations_hide_passwords_and_signed_queries() {
    assert_eq!(
        diagnostic_uri("s3://user:password@bucket/path?token=secret"),
        "s3://<redacted>@bucket/path"
    );
    assert_eq!(
        diagnostic_uri("abfs://container@account/path?sig=secret"),
        "abfs://container@account/path"
    );
    assert_eq!(diagnostic_uri("/local/path"), "/local/path");
    assert_eq!(diagnostic_uri("/local/path?part#1"), "/local/path?part#1");
}

#[test]
fn local_path_is_a_plain_path_or_a_file_uri() {
    assert_eq!(
        local_path("/w/a/n/h.json"),
        Some(Path::new("/w/a/n/h.json"))
    );
    assert_eq!(
        local_path(".barca/a/h.json"),
        Some(Path::new(".barca/a/h.json"))
    );
    assert_eq!(
        local_path("file:///mnt/s/h.json"),
        Some(Path::new("/mnt/s/h.json"))
    );
    assert_eq!(local_path("s3://b/p/h.json"), None);
    assert_eq!(local_path("abfss://c@a.dfs.core.windows.net/h.json"), None);
}

// ── TransferClient vs a fake helper ─────────────────────────────────────
//
// A stdlib-only stand-in speaking the same protocol, so these tests pin
// the Rust side independent of barca._transfer. The store is a plain
// directory. A store path containing "fail" errors; "die" makes the helper
// exit without replying; "slow" sleeps first; "hold" never finishes; "hashed"
// reports a hash, as the real helper always does.

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
send({"type": "hello", "role": "transfer", "protocol_version": 5})
assert recv() == {"type": "hello", "role": "coordinator", "protocol_version": 5}
def handle(m):
    src, dst = (m["local"], m["remote"]) if m["type"] == "put" else (m["remote"], m["local"])
    if "slow" in m["remote"]: time.sleep(0.3)
    while "hold" in m["remote"]: time.sleep(0.05)
    if "die" in m["remote"]: os._exit(3)
    if "fail" in m["remote"]:
        return send({"type": "error", "id": m["id"], "message": "PermissionError: denied", "attempts": 2})
    os.makedirs(os.path.dirname(dst), exist_ok=True); shutil.copyfile(src, dst)
    done = {"type": "done", "id": m["id"], "size_bytes": os.path.getsize(dst)}
    if "hashed" in m["remote"]: done["sha256"] = "sha-of-" + os.path.basename(os.path.dirname(dst))
    send(done)
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
        )
        .unwrap();
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

fn fresh_socket() -> PathBuf {
    crate::protocol::socket_path(
        &format!("xfer-test-{}-{}", std::process::id(), rand_suffix()),
        "transfer",
    )
    .unwrap()
}

#[tokio::test]
async fn a_deferred_client_starts_its_helper_at_the_first_request() {
    let fx = Fixture::new();
    let mut cmd = Command::new("python3");
    cmd.args(["-c", FAKE_HELPER]).env("FAKE_LOG", &fx.log);
    let mut c = TransferClient::deferred_with(cmd, fresh_socket(), fx.layout());
    // A local copy with the recorded hash is checked here, with no helper.
    let local = fx.write_local("n/h.json", b"same");
    let store = fx.layout().store_for(&local).unwrap();
    let sha = crate::hash::file_sha256(Path::new(&local)).unwrap();
    assert!(c.has_local_copy(&store, Some(&sha)));
    assert!(c.has_local_copy(&store, None));
    assert!(!c.has_local_copy(&store, Some("other")));
    assert!(!c.started());
    assert_eq!(within(c.drain()).await.transferred, 0);
    assert!(!c.started() && fx.requests().is_empty());

    c.upload("node", &local).unwrap();
    assert!(c.started());
    let report = within(c.drain()).await;
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(std::fs::read(fx.store.join("n/h.json")).unwrap(), b"same");
    within(c.shutdown()).await;
    assert_eq!(fx.requests(), ["put", "shutdown"]);
}

#[tokio::test]
async fn a_deferred_client_never_used_stops_at_once() {
    let fx = Fixture::new();
    let c = TransferClient::deferred_with(Command::new("python3"), fresh_socket(), fx.layout());
    within(c.shutdown()).await;
    let c = TransferClient::deferred_with(Command::new("python3"), fresh_socket(), fx.layout());
    let (unconfirmed, _) = within(c.abort()).await;
    assert!(unconfirmed.is_empty());
}

#[tokio::test]
async fn a_deferred_helper_that_cannot_start_fails_its_requests_with_the_reason() {
    let fx = Fixture::new();
    let cmd = Command::new(fx.local.join("no-such-python"));
    let mut c = TransferClient::deferred_with(cmd, fresh_socket(), fx.layout());
    let local = fx.write_local("n/h.json", b"x");
    c.upload("node", &local).unwrap();
    let report = within(c.drain()).await;
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0]
            .message
            .contains("failed to spawn transfer helper"),
        "{:?}",
        report.failures
    );
    // Later requests fail the same way rather than hang.
    assert!(within(c.probe()).await.is_err());
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
    let local = c.fetch("n", &store, None).unwrap();
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
    let local = c.fetch("n", &store, None).unwrap();
    assert!(within(c.await_fetches(&[local])).await.failures.is_empty());
    within(c.shutdown()).await;
    assert_eq!(fx.requests(), vec!["shutdown"]);
}

#[tokio::test]
async fn duplicate_fetches_are_requested_once() {
    let fx = Fixture::new();
    let store = fx.write_store("n/h.json", b"1");
    let mut c = fx.client().await;
    let a = c.fetch("x", &store, None).unwrap();
    let b = c.fetch("y", &store, None).unwrap();
    assert_eq!(a, b);
    assert!(within(c.await_fetches(&[a, b])).await.failures.is_empty());
    within(c.shutdown()).await;
    assert_eq!(fx.requests(), vec!["get", "shutdown"]);
}

#[tokio::test]
async fn fetch_outside_store_root_is_none() {
    let fx = Fixture::new();
    let mut c = fx.client().await;
    assert!(c.fetch("n", "/somewhere/else/h.json", None).is_none());
    within(c.shutdown()).await;
}

#[tokio::test]
async fn failed_fetch_is_reported() {
    let fx = Fixture::new();
    let store = fx.write_store("fail/h.json", b"1");
    let mut c = fx.client().await;
    let local = c.fetch("n", &store, None).unwrap();
    let failures = within(c.await_fetches(std::slice::from_ref(&local)))
        .await
        .failures;
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].key, "n");
    // A failure that is not "the object does not exist" is not reported as missing.
    assert!(!failures[0].missing);
    assert!(!local.exists());
    within(c.shutdown()).await;
}

#[tokio::test]
async fn confirmed_uploads_notify_before_an_earlier_stalled_upload_drains() {
    let fx = Fixture::new();
    let mut c = fx.client().await;
    let held = fx.write_local("hold/h.json", b"1");
    let done = fx.write_local("hashed/h.json", b"42");
    c.upload("held", &held).unwrap();
    let (tx, rx) = oneshot::channel();
    c.upload_confirmed(
        "done",
        &done,
        &done,
        Box::new(move |receipt| {
            tx.send((receipt.bytes, receipt.sha256.clone())).unwrap();
        }),
    )
    .unwrap();
    assert_eq!(within(rx).await.unwrap(), (2, Some("sha-of-hashed".into())));
    assert_eq!(
        c.pending_uploads(),
        2,
        "notification must not consume the drain report"
    );
    let (unconfirmed, hashes) = within(c.abort()).await;
    assert_eq!(unconfirmed, ["held"]);
    assert_eq!(
        hashes.get("done").map(String::as_str),
        Some("sha-of-hashed")
    );
}

#[tokio::test]
async fn failed_and_abandoned_uploads_never_notify_as_confirmed() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let fx = Fixture::new();
    let mut c = fx.client().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let notify = || {
        let calls = calls.clone();
        Box::new(move |_: &Transferred| {
            calls.fetch_add(1, Ordering::Relaxed);
        }) as UploadNotice
    };
    let denied = fx.write_local("fail/h.json", b"1");
    c.upload_confirmed("denied", &denied, &denied, notify())
        .unwrap();
    assert_eq!(within(c.drain()).await.failures.len(), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(
        c.upload_confirmed("outside", "/outside/h.json", "/outside/h.json", notify())
            .is_none()
    );
    let held = fx.write_local("hold/h.json", b"1");
    c.upload_confirmed("held", &held, &held, notify()).unwrap();
    let (unconfirmed, _) = within(c.abort()).await;
    assert_eq!(unconfirmed, ["held"]);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_includes_a_success_notified_before_its_reply_was_delivered() {
    let fx = Fixture::new();
    let mut c = fx.client().await;
    let pid = c.child.as_ref().unwrap().id().unwrap();
    let done = fx.write_local("hashed/h.json", b"42");
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    c.upload_confirmed(
        "done",
        &done,
        &done,
        Box::new(move |_| {
            started_tx.send(()).unwrap();
            // Pause exactly between notification and oneshot receipt delivery.
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }),
    )
    .unwrap();
    within(started_rx).await.unwrap();
    let abort = tokio::spawn(c.abort());
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(pid as i32, 0) } == 0 {
        assert!(Instant::now() < deadline, "abort never stopped the helper");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    release_tx.send(()).unwrap();
    let (unconfirmed, hashes) = within(abort).await.unwrap();
    assert!(
        unconfirmed.is_empty(),
        "a notified success must remain recorded"
    );
    assert_eq!(
        hashes.get("done").map(String::as_str),
        Some("sha-of-hashed")
    );
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
    let (mut un, hashes) = within(c.abort()).await;
    un.sort();
    assert_eq!(un, vec!["bad", "slow"]);
    // The fake helper reports no hash; a real one does (python/tests/test_transfer.py).
    assert!(hashes.is_empty());
}

/// Ctrl-C during the end-of-run wait drops `drain` midway. The uploads it had not seen
/// finish must still be reported as unconfirmed by `abort`, or their steps would be
/// recorded pointing at objects that are not in the store.
#[tokio::test]
async fn a_drain_dropped_midway_loses_no_upload() {
    let fx = Fixture::new();
    let mut c = fx.client().await;
    let done = fx.write_local("hashed/h.json", b"1");
    let stuck = fx.write_local("hold/h.json", b"1");
    c.upload("done", &done).unwrap();
    c.upload("stuck", &stuck).unwrap();
    // Start draining and give up, again and again, until the first upload has been
    // collected. The second never finishes, so no drain ever completes.
    let deadline = Instant::now() + Duration::from_secs(10);
    while c.pending_uploads() == 2 {
        assert!(Instant::now() < deadline, "the first upload never finished");
        let gave_up = tokio::time::timeout(Duration::from_millis(20), c.drain()).await;
        assert!(
            gave_up.is_err(),
            "drain returned with an upload still in flight"
        );
    }
    assert_eq!(c.pending_uploads(), 1);
    let (unconfirmed, hashes) = within(c.abort()).await;
    assert_eq!(unconfirmed, vec!["stuck"]);
    assert_eq!(
        hashes,
        HashMap::from([("done".to_string(), "sha-of-hashed".to_string())])
    );
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

/// A helper that is started and never used must not outlive the launch: a command that
/// ended early would otherwise leave it to find the socket gone (#249).
#[tokio::test]
async fn a_helper_that_never_connected_is_gone_after_stop_and_after_drop() {
    let fx = Fixture::new();
    for stopped in [true, false] {
        let pid_file = fx.local.join(format!("pid-{stopped}"));
        let mut cmd = Command::new("python3");
        cmd.args([
            "-c",
            "import os, sys, time; open(sys.argv[1], 'w').write(str(os.getpid())); \
                 time.sleep(60)",
        ])
        .arg(&pid_file);
        let sock = crate::protocol::socket_path(
            &format!("xfer-test-idle-{}-{}", std::process::id(), rand_suffix()),
            "transfer",
        )
        .unwrap();
        let launching = Launching::start(cmd, sock.clone(), fx.layout()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid: i64 = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                .unwrap_or_default()
                .parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the helper never started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        if stopped {
            within(launching.stop()).await;
            assert!(
                !crate::process::pid_alive(pid),
                "alive after stop() returned"
            );
            assert!(!sock.exists());
        } else {
            drop(launching);
            // Killed on the spot; the runtime reaps it a moment later.
            while crate::process::pid_alive(pid) {
                assert!(
                    Instant::now() < deadline,
                    "alive after its launch was dropped"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

#[tokio::test]
async fn spawn_reports_a_helper_that_exits_before_connecting() {
    let fx = Fixture::new();
    let mut cmd = Command::new("python3");
    cmd.args(["-c", "import sys; sys.exit(2)"]);
    let sock = crate::protocol::socket_path(
        &format!("xfer-test-dead-{}", std::process::id()),
        "transfer",
    )
    .unwrap();
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
    let mut probe = std::process::Command::new(&p);
    probe.args(["-c", "import barca._transfer"]);
    let ok = crate::helper_proc::output_std(&mut probe)
        .ok()?
        .status
        .success();
    ok.then_some(p)
}

#[tokio::test]
async fn hello_is_accepted_before_connect_even_after_slow_planning() {
    let Some(python) = repo_python() else {
        crate::errln!("SKIP: no .venv with barca installed");
        return;
    };
    let fx = Fixture::new();
    let mut cmd = Command::new(python);
    cmd.args(["-m", "barca._transfer"]);
    let sock = crate::protocol::socket_path(
        &format!("xfer-test-slow-plan-{}", std::process::id()),
        "transfer",
    )
    .unwrap();
    let launching = Launching::start(cmd, sock, fx.layout()).unwrap();
    // Longer than the helper's socket hello timeout if the coordinator never answers (#702).
    tokio::time::sleep(Duration::from_secs(15)).await;
    let mut c = launching.connect().await.unwrap();
    assert_eq!(within(c.probe()).await, Ok(()));
    within(c.shutdown()).await;
}

#[tokio::test]
async fn real_helper_round_trip_through_plain_path_store() {
    let Some(python) = repo_python() else {
        crate::errln!("SKIP: no .venv with barca installed");
        return;
    };
    let fx = Fixture::new();
    let mut cmd = Command::new(python);
    cmd.args(["-m", "barca._transfer"]);
    let sock = crate::protocol::socket_path(
        &format!("xfer-test-real-{}", std::process::id()),
        "transfer",
    )
    .unwrap();
    let mut c = TransferClient::spawn(cmd, sock, fx.layout()).await.unwrap();

    let local = fx.write_local("m.py__a/h1.json", b"[1,2,3]");
    let store = c.upload("a", &local).unwrap();
    let report = within(c.drain()).await;
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(std::fs::read(&store).unwrap(), b"[1,2,3]");

    std::fs::remove_file(&local).unwrap();
    let back = c.fetch("a", &store, None).unwrap();
    let fetched = within(c.await_fetches(std::slice::from_ref(&back))).await;
    assert!(fetched.failures.is_empty());
    assert_eq!((fetched.transferred, fetched.bytes), (1, 7));
    assert_eq!(std::fs::read(&back).unwrap(), b"[1,2,3]");

    let missing = c
        .fetch("gone", &format!("{}/gone/h.json", fx.store.display()), None)
        .unwrap();
    let failures = within(c.await_fetches(&[missing])).await.failures;
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].message.contains("FileNotFoundError"),
        "{failures:?}"
    );
    // The helper says the object is not there. Together with a store that answers a
    // probe, that is what lets the run compute the step again instead of failing (#252).
    assert!(failures[0].missing, "{failures:?}");
    assert_eq!(within(c.probe()).await, Ok(()));

    // The same "not found" from a store that is gone: the probe fails, and creates
    // nothing.
    std::fs::rename(&fx.store, fx.store.with_extension("unmounted")).unwrap();
    let orphan = c
        .fetch(
            "gone2",
            &format!("{}/gone2/h.json", fx.store.display()),
            None,
        )
        .unwrap();
    let failures = within(c.await_fetches(&[orphan])).await.failures;
    assert!(failures[0].missing, "{failures:?}");
    let probe = within(c.probe()).await;
    assert!(
        probe
            .as_ref()
            .is_err_and(|e| e.contains("FileNotFoundError")),
        "{probe:?}"
    );
    assert!(!fx.store.exists());
    within(c.shutdown()).await;
}
