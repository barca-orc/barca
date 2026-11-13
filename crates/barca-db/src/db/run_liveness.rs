//! Who is running a `running` run: the rows whose owner is gone (recorded `interrupted` as
//! a run starts), alive here ([`alive_running_runs`]) or unknowable from this machine
//! ([`crate::run_owner`] judges the owners).

use super::*;
#[cfg(test)]
use barca_model::process::local_host;
use barca_model::process::pid_alive;

/// The `running` runs whose process is known to be gone (killed, out of memory, power loss):
/// they will never record an outcome themselves.
///
/// A run that recorded who started it is judged by [`crate::run_owner::judge`], which says
/// "gone" only when this process can establish it, also in a container, where the pid is 1
/// on every start and the host name changes. When it cannot (a run on another machine, in
/// another container whose marker cannot be consulted, on the host of this container) the
/// run is not listed: it may be going.
///
/// A run that recorded nothing of the kind (an older barca) is judged as before: gone when
/// it was started on this host and its pid does not exist. Best effort: empty when nothing
/// can be read.
pub(super) async fn interrupted_runs(conn: &libsql::Connection, db_path: &str) -> Vec<String> {
    let on = crate::run_owner::System::for_db(db_path);
    interrupted_among(&running_runs(conn).await, &on, pid_alive)
}

/// A `running` row, as much of it as says who is running it and what.
#[derive(Debug, Clone)]
struct RunningRun {
    run_id: String,
    pid: Option<i64>,
    host: String,
    /// The `owner` column: empty on rows from older versions.
    owner: String,
    command: String,
    /// The `job` column: empty on rows from older versions.
    job: String,
}

async fn running_runs(conn: &libsql::Connection) -> Vec<RunningRun> {
    // A database this version has not migrated (history read from one an older barca wrote)
    // has no `owner` or `job` column yet. `idx_runs_status` serves the filter.
    let with_owner = "SELECT run_id, pid, COALESCE(host, ''), COALESCE(owner, ''), command, COALESCE(job, '') FROM runs WHERE status = 'running' ORDER BY id";
    let without = "SELECT run_id, pid, COALESCE(host, ''), '', command, '' FROM runs WHERE status = 'running' ORDER BY id";
    let mut rows = match conn.query(with_owner, ()).await {
        Ok(rows) => rows,
        Err(_) => match conn.query(without, ()).await {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        },
    };
    let mut running = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(run_id) = row.get::<String>(0) {
            running.push(RunningRun {
                run_id,
                pid: row.get::<i64>(1).ok(),
                host: row.get::<String>(2).unwrap_or_default(),
                owner: row.get::<String>(3).unwrap_or_default(),
                command: row.get::<String>(4).unwrap_or_default(),
                job: row.get::<String>(5).unwrap_or_default(),
            });
        }
    }
    running
}

/// [`interrupted_runs`] with what it asks of the machine passed in.
fn interrupted_among(
    running: &[RunningRun],
    on: &dyn crate::run_owner::Observer,
    pid_alive: impl Fn(i64) -> bool,
) -> Vec<String> {
    use crate::run_owner::{Owner, judge_json};
    let this_host = on.here().host;
    running
        .iter()
        .filter(|run| {
            if run.owner.is_empty() {
                let on_this_host = !this_host.is_empty() && run.host == this_host;
                on_this_host && run.pid.is_some_and(|pid| !pid_alive(pid))
            } else {
                judge_json(run.pid, &run.owner, on) == Owner::Gone
            }
        })
        .map(|run| run.run_id.clone())
        .collect()
}

/// Record [`interrupted_runs`] as `interrupted`. Their `finished_at` and `elapsed_seconds`
/// stay NULL, since nobody saw them end. Only a run does this (it writes anyway, and with
/// shared state pushes what it wrote); reading history reports the same status without
/// writing it (see [`get_recent_runs`]). Both go by the same rule: a run is recorded as
/// interrupted only by a process that has established that its owner is gone.
///
/// Afterwards the markers that no `running` row names any more are removed
/// ([`crate::run_owner::sweep`]).
pub(super) async fn mark_interrupted_runs(conn: &libsql::Connection, db_path: &str) {
    for run_id in interrupted_runs(conn, db_path).await {
        conn.execute(
            "UPDATE runs SET status = 'interrupted' WHERE run_id = ?1 AND status = 'running'",
            [run_id],
        )
        .await
        .ok();
    }
    let named = running_runs(conn)
        .await
        .iter()
        .filter_map(|run| crate::run_owner::token_of(&run.owner))
        .collect();
    crate::run_owner::sweep(db_path, &named, SystemTime::now());
}

/// The `running` runs whose owner is not known to be gone: in progress, here or elsewhere, or
/// unknowable (see [`interrupted_runs`]). Best effort: empty when nothing can be read.
pub async fn live_runs(conn: &libsql::Connection, db_path: &str) -> Vec<String> {
    let running = running_runs(conn).await;
    let gone: std::collections::HashSet<String> = interrupted_among(
        &running,
        &crate::run_owner::System::for_db(db_path),
        pid_alive,
    )
    .into_iter()
    .collect();
    running
        .into_iter()
        .map(|run| run.run_id)
        .filter(|id| !gone.contains(id))
        .collect()
}

/// The `running` runs whose owner this process can see is alive: on this machine, a process
/// that exists. A run whose owner cannot be judged from here (another machine, an older row
/// from another host) is not listed: unlike [`live_runs`], which keeps those, this is for a
/// refusal that must not hold forever because of a row nobody will finish (`barca state
/// reset`, and the scheduler's skip of a node a CLI run covers). Best effort: empty when
/// nothing can be read.
pub async fn runs_alive_here(conn: &libsql::Connection, db_path: &str) -> Vec<String> {
    alive_here(running_runs(conn).await, db_path)
        .into_iter()
        .map(|run| run.run_id)
        .collect()
}

/// The rows of `running` whose owner this process can see is alive (see [`runs_alive_here`]).
fn alive_here(running: Vec<RunningRun>, db_path: &str) -> Vec<RunningRun> {
    use crate::run_owner::{Owner, judge_json};
    let on = crate::run_owner::System::for_db(db_path);
    let this_host = crate::run_owner::Observer::here(&on).host;
    running
        .into_iter()
        .filter(|run| {
            if run.owner.is_empty() {
                !this_host.is_empty()
                    && run.host == this_host
                    && run.pid.is_some_and(|pid| pid > 0 && pid_alive(pid))
            } else {
                judge_json(run.pid, &run.owner, &on) == Owner::Alive
            }
        })
        .collect()
}

/// A `running` run of another process whose owner is alive, and what it runs: see
/// [`alive_running_runs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliveRun {
    pub run_id: String,
    /// `get` or `run`.
    pub command: String,
    /// The canonical job id (`all`, or the resolved node ids joined by `,`); empty when the
    /// run was started by a barca that wrote it only at the end.
    pub job: String,
}

/// The `running` runs of other processes whose owner this process can see is alive
/// ([`runs_alive_here`]), oldest first: a `barca get` or `barca run` in progress on this
/// machine. `barca serve` skips a tick of a node one of them covers (#479).
///
/// This process's own rows are left out: the server knows its runs from memory, and a row of
/// its own left `running` (its outcome could not be written) must not hold a schedule back.
/// A row whose owner cannot be judged from here (another machine's, restored from a backup)
/// is left out too, so it never holds a schedule back for ever. One query of the `running`
/// rows (`idx_runs_status`); best effort: empty when nothing can be read.
pub async fn alive_running_runs(db_path: &str) -> Vec<AliveRun> {
    alive_running_runs_in(db_path, db_path).await
}

/// [`alive_running_runs`], with the `running` rows read from `read_path`: a snapshot of the
/// metadata database at `db_path` ([`DbSnapshot`]), so a polled reader (`GET /state`) never
/// opens the real one. The owners are judged against `db_path`'s marker directory.
pub async fn alive_running_runs_in(read_path: &str, db_path: &str) -> Vec<AliveRun> {
    let running = {
        let _g = db_guard_for(db_path).await;
        let Ok((_db, conn)) = open_conn(read_path).await else {
            return Vec::new();
        };
        running_runs(&conn).await
    };
    let others = running
        .into_iter()
        .filter(|run| !crate::run_owner::is_this_process(run.pid, &run.host, &run.owner))
        .collect();
    alive_here(others, db_path)
        .into_iter()
        .map(|run| AliveRun {
            run_id: run.run_id,
            command: run.command,
            job: run.job,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_running_run_whose_process_is_gone_is_reported_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();

        // A pid that existed and is certainly gone: a child that has been reaped.
        let mut child =
            barca_model::spawn_lock::spawn_std(&mut std::process::Command::new("true")).unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();

        // Ours, still going (create_run records this process and host).
        create_run(&db_path, "alive", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            let host = local_host();
            for (run_id, pid, host, status) in [
                ("dead", Some(dead_pid), host.as_str(), "running"),
                ("elsewhere", Some(dead_pid), "another-machine", "running"),
                ("done", Some(dead_pid), host.as_str(), "success"),
                ("old-version", None, "", "running"),
            ] {
                conn.execute(
                    "INSERT INTO runs (run_id, command, files, status, pid, host) VALUES (?1, 'get', 'f.py', ?2, NULLIF(?3, ''), NULLIF(?4, ''))",
                    [
                        run_id.to_string(),
                        status.to_string(),
                        pid.map(|p| p.to_string()).unwrap_or_default(),
                        host.to_string(),
                    ],
                )
                .await
                .unwrap();
            }
        }

        let status_of: HashMap<String, RunRecord> = get_recent_runs(&db_path, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.run_id.clone(), r))
            .collect();
        assert_eq!(status_of["dead"].status, "interrupted");
        // Nobody saw it end, so it has no finish time.
        assert_eq!(status_of["dead"].finished_at, None);
        assert_eq!(status_of["dead"].elapsed_seconds, None);
        assert_eq!(status_of["alive"].status, "running");
        // Another machine's pid means nothing here; neither does a row with no pid.
        assert_eq!(status_of["elsewhere"].status, "running");
        assert_eq!(status_of["old-version"].status, "running");
        assert_eq!(status_of["done"].status, "success");

        // Reading history reported it without writing; the next run to start records it.
        let stored = |run_id: &'static str| {
            let db_path = db_path.clone();
            async move {
                let _g = db_guard().await;
                let (_db, conn) = open_conn(&db_path).await.unwrap();
                let mut rows = conn
                    .query("SELECT status FROM runs WHERE run_id = ?1", [run_id])
                    .await
                    .unwrap();
                rows.next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<String>(0)
                    .unwrap()
            }
        };
        assert_eq!(stored("dead").await, "running");
        create_run(&db_path, "next", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored("dead").await, "interrupted");
        assert_eq!(stored("elsewhere").await, "running");
        assert_eq!(stored("alive").await, "running");
    }

    // ─── who is running a `running` run (#290) ───────────────────────────────

    use crate::run_owner::{self, Here, Identity, Marker, Observer, Owner, Process};

    /// Insert a `running` run as some process recorded it.
    async fn add_running(db_path: &str, run_id: &str, pid: u32, host: &str, owner: &str) {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(db_path).await.unwrap();
        conn.execute(
            "INSERT INTO runs (run_id, command, files, status, pid, host, owner) VALUES (?1, 'get', 'f.py', 'running', ?2, ?3, NULLIF(?4, ''))",
            [
                run_id.to_string(),
                pid.to_string(),
                host.to_string(),
                owner.to_string(),
            ],
        )
        .await
        .unwrap();
    }

    async fn stored_status(db_path: &str, run_id: &str) -> String {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(db_path).await.unwrap();
        let mut rows = conn
            .query("SELECT status FROM runs WHERE run_id = ?1", [run_id])
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get::<String>(0).unwrap()
    }

    async fn reported(db_path: &str) -> HashMap<String, String> {
        get_recent_runs(db_path, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.run_id, r.status))
            .collect()
    }

    /// A pid that existed and is certainly gone: a child that has been reaped.
    fn a_dead_pid() -> u32 {
        let mut child =
            barca_model::spawn_lock::spawn_std(&mut std::process::Command::new("true")).unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    async fn metadata_db(dir: &tempfile::TempDir) -> String {
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        db_path
    }

    fn marker_files(dir: &tempfile::TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path().join("run-owners"))
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// What the real machine says of itself, as an identity a run here would record.
    fn this_process(fifo: Option<run_owner::FifoId>) -> Identity {
        let scratch = tempfile::tempdir().unwrap();
        let db = scratch.path().join("metadata.db").display().to_string();
        let owner = run_owner::claim(&db, "probe");
        run_owner::release(&db, "probe");
        let mut id: Identity = serde_json::from_str(&owner).expect("this kernel has a boot id");
        id.fifo = fifo;
        id
    }

    fn json(id: &Identity) -> String {
        serde_json::to_string(id).unwrap()
    }

    #[tokio::test]
    async fn a_run_killed_in_a_container_is_interrupted_after_the_restart() {
        // #290. The coordinator was pid 1 of a container; the container that replaces it has
        // another host name and a pid 1 of its own. By pid and host the killed run looked
        // alive and foreign, and stayed `running` for ever. Here: the same kernel, the pid
        // namespace number handed out again, and a pid 1 that started later.
        let run = Identity {
            boot: "the-kernel".into(),
            pidns: "pid:[4026532980]".into(),
            timens: "time:[4026531834]".into(),
            start: Some(1_000),
            ..Identity::default()
        };
        let rows = [RunningRun {
            run_id: "killed".into(),
            pid: Some(1),
            host: "container-1".into(),
            owner: json(&run),
            command: "get".into(),
            job: String::new(),
        }];
        let restarted = run_owner::tests::Fake {
            here: Here {
                boot: "the-kernel".into(),
                machine: String::new(),
                host: "container-2".into(),
                pidns: "pid:[4026532980]".into(),
                timens: "time:[4026531834]".into(),
                handles: true,
            },
            process: Process::Present(Some(9_000)),
            marker: Marker::Missing,
        };
        // `pid_alive` is what the old rule asked; it says the pid exists.
        assert_eq!(interrupted_among(&rows, &restarted, |_| true), ["killed"]);

        // The same container still going, seen from inside (`docker exec`): the same pid 1.
        let mut inside = restarted.clone();
        inside.process = Process::Present(Some(1_000));
        assert!(interrupted_among(&rows, &inside, |_| false).is_empty());

        // A new container that got another namespace number cannot look the pid up. It
        // takes the marker, where the filesystem lets it be verified...
        let mut with_marker = run.clone();
        with_marker.fifo = Some(run_owner::FifoId {
            token: run_owner::tests::T.into(),
            dev: 5,
            ino: 6,
            handle: Some("1:00ff".into()),
            fs: "volume".into(),
            local: true,
        });
        let rows = [RunningRun {
            owner: json(&with_marker),
            ..rows[0].clone()
        }];
        let mut elsewhere = restarted.clone();
        elsewhere.here.pidns = "pid:[4026533111]".into();
        let seen = |handle: Option<&str>, reader| Marker::Present {
            dev: 5,
            ino: 6,
            handle: handle.map(String::from),
            reader: Some(reader),
            fs: "volume".into(),
            local: true,
        };
        elsewhere.marker = seen(Some("1:00ff"), false);
        assert_eq!(interrupted_among(&rows, &elsewhere, |_| true), ["killed"]);
        // ...not while the first container is still running next to it...
        elsewhere.marker = seen(Some("1:00ff"), true);
        assert!(interrupted_among(&rows, &elsewhere, |_| false).is_empty());
        // ...and not where the file cannot be shown to be the one the owner held.
        elsewhere.marker = seen(None, false);
        assert!(interrupted_among(&rows, &elsewhere, |_| false).is_empty());
    }

    #[tokio::test]
    async fn a_live_run_that_cannot_be_observed_stays_running_and_keeps_its_marker() {
        // The review's failure, with the real files. A run is going on the host; its marker
        // is in the project directory, which a container mounts. The container is another
        // kernel: to it the marker FIFO has no reader and the pid does not exist. It must
        // report the run as `running`, record nothing, and leave the marker alone, also
        // when it starts a run of its own, and also long after.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        // The host's process: alive for the whole test.
        let (fifo, _host_process) = run_owner::tests::other_process(&db_path);
        let marker = format!("{}.fifo", fifo.token);
        let mut host_run = this_process(Some(fifo));
        // This test process stands for the container: the run is from another kernel. As
        // in the review's attack, the reader has the owner's machine id and host name
        // (`this_process` recorded this machine's), and the pid is dead here.
        host_run.boot = "the-host-kernel".into();
        add_running(
            &db_path,
            "host-run",
            a_dead_pid(),
            &local_host(),
            &json(&host_run),
        )
        .await;

        assert_eq!(reported(&db_path).await["host-run"], "running");
        create_run(&db_path, "container-run", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored_status(&db_path, "host-run").await, "running");
        assert!(marker_files(&dir).contains(&marker));

        // The same with every answer the container's kernel could give about a process and
        // a file it sees through a mount that is not a local disk: no such pid, nobody at
        // the other end of the marker, whatever inode numbers and filesystem id it shows.
        let rows = {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            running_runs(&conn).await
        };
        let host_row: Vec<RunningRun> = rows
            .into_iter()
            .filter(|r| r.run_id == "host-run")
            .collect();
        let real = run_owner::System::for_db(&db_path);
        let Marker::Present {
            dev,
            ino,
            handle,
            fs,
            ..
        } = real.marker(&host_run.fifo.as_ref().unwrap().token)
        else {
            panic!("the marker is there");
        };
        for process in [
            Process::Absent,
            Process::Present(None),
            Process::Present(Some(1)),
        ] {
            for reader in [Some(false), None] {
                for (handle, fs) in [(handle.clone(), fs.clone()), (None, String::new())] {
                    let blind = run_owner::tests::Fake {
                        here: real.here(),
                        process,
                        marker: Marker::Present {
                            dev,
                            ino,
                            handle,
                            reader,
                            fs,
                            local: false,
                        },
                    };
                    assert!(interrupted_among(&host_row, &blind, |_| false).is_empty());
                }
            }
        }

        // Hours later the container starts another run: the marker is still named by a
        // `running` row, so the sweep leaves it.
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            let named = running_runs(&conn)
                .await
                .iter()
                .filter_map(|run| run_owner::token_of(&run.owner))
                .collect();
            let later = SystemTime::now() + Duration::from_secs(48 * 3600);
            run_owner::sweep(&db_path, &named, later);
        }
        assert!(marker_files(&dir).contains(&marker));
        finish_run(&db_path, "container-run", "success", 1, 0, 0.1)
            .await
            .unwrap();
        assert_eq!(
            marker_files(&dir),
            [marker],
            "the container removed its own, only"
        );
        assert_eq!(reported(&db_path).await["host-run"], "running");
    }

    #[tokio::test]
    async fn a_run_whose_owner_died_next_door_is_interrupted_once_its_marker_has_no_reader() {
        // Another pid namespace on this kernel (a second container on the same volume): the
        // pid cannot be looked up, the marker can. Real files, real kernel.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        let (fifo, process) = run_owner::tests::other_process(&db_path);
        // Where this filesystem gives no file handle (Linux), the marker proves nothing
        // and the run must simply stay `running`.
        let provable = if cfg!(target_os = "macos") {
            fifo.local
        } else {
            fifo.handle.is_some()
        };
        let marker = format!("{}.fifo", fifo.token);
        let mut next_door = this_process(Some(fifo));
        next_door.pidns = "pid:[another-container]".into();
        add_running(
            &db_path,
            "theirs",
            std::process::id(),
            "container-1",
            &json(&next_door),
        )
        .await;

        assert_eq!(reported(&db_path).await["theirs"], "running");
        create_run(&db_path, "ours", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored_status(&db_path, "theirs").await, "running");

        // It is killed.
        drop(process);
        if !provable {
            assert_eq!(reported(&db_path).await["theirs"], "running");
            return;
        }
        let system = run_owner::System::for_db(&db_path);
        run_owner::tests::eventually("the owner is seen to be gone", || {
            run_owner::judge(None, &next_door, &system) == Owner::Gone
        });
        assert_eq!(reported(&db_path).await["theirs"], "interrupted");
        // Reading reported it and wrote nothing; the next run to start records it.
        assert_eq!(stored_status(&db_path, "theirs").await, "running");
        create_run(&db_path, "next", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        assert_eq!(stored_status(&db_path, "theirs").await, "interrupted");
        assert_eq!(stored_status(&db_path, "ours").await, "running");
        // Its marker is not named by a `running` row any more: it goes once it is old.
        assert!(marker_files(&dir).contains(&marker));
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            let named = running_runs(&conn)
                .await
                .iter()
                .filter_map(|run| run_owner::token_of(&run.owner))
                .collect();
            run_owner::sweep(
                &db_path,
                &named,
                SystemTime::now() + Duration::from_secs(7200),
            );
        }
        assert_eq!(marker_files(&dir).len(), 1, "this process's own marker");
        assert!(!marker_files(&dir).contains(&marker));
    }

    #[tokio::test]
    async fn a_run_from_before_a_restart_of_this_machine_is_interrupted() {
        // What main reports by host name and pid, established by observation instead: the
        // marker is on this machine's local disk, the same filesystem and inode the owner
        // recorded under another boot id, and nobody on this kernel holds it.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        let (fifo, process) = run_owner::tests::other_process(&db_path);
        // A temp dir that is not on a local disk (a container's overlay) proves nothing.
        let provable = fifo.local && !fifo.fs.is_empty();
        let mut before = this_process(Some(fifo));
        before.boot = "the-boot-before".into();
        add_running(
            &db_path,
            "then",
            std::process::id(),
            "any-host",
            &json(&before),
        )
        .await;

        // While somebody holds the marker, the kernel it was created on is still running
        // (this one, in the test): no restart can have happened.
        assert_eq!(reported(&db_path).await["then"], "running");
        drop(process);
        let system = run_owner::System::for_db(&db_path);
        if provable {
            run_owner::tests::eventually("the marker has no reader", || {
                run_owner::judge(None, &before, &system) == Owner::Gone
            });
            assert_eq!(reported(&db_path).await["then"], "interrupted");
        } else {
            assert_eq!(reported(&db_path).await["then"], "running");
        }
    }

    #[tokio::test]
    async fn only_runs_whose_owner_is_alive_hold_a_schedule_back() {
        // #479: `barca serve` skips a tick while a CLI run of the node is going. A run
        // records its job when it starts, not only at the end.
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        create_run_with(
            &db_path,
            NewRun {
                run_id: "mine",
                command: "get",
                files: "[]",
                target: Some("a"),
                steps_total: Some(1),
                job: Some("p.py:a"),
                trigger: &barca_model::trigger::Trigger::default(),
                traced: false,
            },
        )
        .await
        .unwrap();
        // Another process on this host, still going (a row as an older barca writes it).
        let mut other =
            barca_model::spawn_lock::spawn_std(std::process::Command::new("sleep").arg("30"))
                .unwrap();
        add_running(&db_path, "cli", other.id(), &local_host(), "").await;
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            conn.execute("UPDATE runs SET job = 'p.py:a' WHERE run_id = 'cli'", ())
                .await
                .unwrap();
        }
        // Killed without recording an outcome: its owner is gone.
        add_running(
            &db_path,
            "stale",
            a_dead_pid(),
            &local_host(),
            &json(&this_process(None)),
        )
        .await;
        // Another machine's row, restored from a backup: nobody here can tell.
        let mut theirs = this_process(None);
        theirs.boot = "a-kernel-somewhere".into();
        add_running(&db_path, "foreign", 1, "elsewhere", &json(&theirs)).await;
        // An older barca's row, its pid gone.
        add_running(&db_path, "old", a_dead_pid(), &local_host(), "").await;
        // This process's own row ("mine") is left out: a server knows its runs from memory.
        assert_eq!(
            alive_running_runs(&db_path).await,
            [AliveRun {
                run_id: "cli".into(),
                command: "get".into(),
                job: "p.py:a".into(),
            }]
        );
        other.kill().ok();
        other.wait().ok();
        assert!(alive_running_runs(&db_path).await.is_empty());
        finish_run(&db_path, "mine", "success", 1, 0, 0.1)
            .await
            .ok();
    }

    #[tokio::test]
    async fn a_run_from_another_machine_is_never_marked_interrupted() {
        // A restored backup can bring another machine's `running` rows into this database,
        // and such a run may be going right now. Even with this machine's host name and a pid
        // that is dead here (which by pid and host alone read as interrupted).
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        let mut theirs = this_process(None);
        theirs.boot = "a-kernel-somewhere".into();
        for (run_id, machine) in [("twin", theirs.machine.clone()), ("other", "m-2".into())] {
            theirs.machine = machine;
            add_running(
                &db_path,
                run_id,
                a_dead_pid(),
                &local_host(),
                &json(&theirs),
            )
            .await;
        }
        create_run(&db_path, "ours", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        for run_id in ["twin", "other"] {
            assert_eq!(reported(&db_path).await[run_id], "running");
            assert_eq!(stored_status(&db_path, run_id).await, "running");
        }
    }

    #[tokio::test]
    async fn a_run_records_who_started_it_and_gives_up_its_marker_when_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = metadata_db(&dir).await;
        create_run(&db_path, "r1", "get", "f.py", None, Some(1))
            .await
            .unwrap();
        let rows = {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            running_runs(&conn).await
        };
        assert_eq!(rows.len(), 1);
        let id: Identity = serde_json::from_str(&rows[0].owner).unwrap();
        assert!(!id.boot.is_empty());
        let token = id.fifo.as_ref().expect("a marker").token.clone();
        assert_eq!(marker_files(&dir), [format!("{token}.fifo")]);
        let system = run_owner::System::for_db(&db_path);
        assert_eq!(run_owner::judge(rows[0].pid, &id, &system), Owner::Alive);
        assert_eq!(reported(&db_path).await["r1"], "running");

        finish_run(&db_path, "r1", "success", 1, 0, 0.1)
            .await
            .unwrap();
        assert!(marker_files(&dir).is_empty());
    }

    #[tokio::test]
    async fn history_of_a_database_an_older_barca_wrote_is_read_without_the_owner_column() {
        // `barca history` does not migrate what it reads. A 0.18 database has pid and host
        // and no owner: its killed run is still reported, by pid and host.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metadata.db").to_string_lossy().to_string();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&db_path).await.unwrap();
            conn.execute(
                "CREATE TABLE runs (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT UNIQUE NOT NULL, \
                 command TEXT NOT NULL, files TEXT NOT NULL, target TEXT, \
                 status TEXT NOT NULL DEFAULT 'running', steps_total INTEGER, \
                 steps_executed INTEGER DEFAULT 0, steps_cached INTEGER DEFAULT 0, \
                 started_at TEXT DEFAULT (datetime('now')), finished_at TEXT, elapsed_seconds REAL, \
                 pid INTEGER, host TEXT)",
                (),
            )
            .await
            .unwrap();
            for (run_id, pid) in [("killed", a_dead_pid()), ("going", std::process::id())] {
                conn.execute(
                    "INSERT INTO runs (run_id, command, files, pid, host) VALUES (?1, 'get', 'f.py', ?2, ?3)",
                    [run_id.to_string(), pid.to_string(), local_host()],
                )
                .await
                .unwrap();
            }
        }
        let status = reported(&db_path).await;
        assert_eq!(status["killed"], "interrupted");
        assert_eq!(status["going"], "running");
        let history = get_recent_runs(&db_path, 10).await.unwrap();
        assert!(history.iter().all(|run| run.attempts.is_none()));
    }
}
