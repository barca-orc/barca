//! `barca logs`: a run's recorded step output (stdout and stderr), independently of progress
//! and result rendering. Locally it reads the metadata DB, and the spool of a run still in
//! progress (`barca_service::run_logs`); against a server, `client::logs`.

use crate::print::{follow_ended, follow_exit_code, name_run};
use crate::request::LogsArgs;
use barca_service::BarcaError;
use std::time::{Duration, Instant};

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
    let cfg = barca_service::config::resolve(env)?;
    let run_id = match run_id {
        Some(id) => id.to_string(),
        None => {
            let run = barca_service::queries::run_to_read(&cfg, args.follow).await?;
            name_run(&run, args.follow);
            run.run_id
        }
    };
    if !args.follow {
        let lines = barca_service::queries::logs(&cfg, &run_id, args.step, args.stream).await?;
        return args.print_all(&lines);
    }
    let status = follow_local(&cfg, &run_id, args).await?;
    follow_ended(&run_id, &status);
    crate::exit(follow_exit_code(&status));
}

/// `-f` on this machine: print the run's lines from its spool as they arrive, and once the
/// run is recorded as ended, the rest from the database. Returns the run's final status.
async fn follow_local(
    cfg: &barca_service::config::ResolvedConfig,
    run_id: &str,
    args: LogsArgs<'_>,
) -> Result<String, BarcaError> {
    let read = barca_service::db::read_metadata(cfg, true).await?;
    let mut seen = 0usize;
    let mut ended_at: Option<Instant> = None;
    let mut waiting = false;
    loop {
        // Queue ownership lasts through recording the stopped outcome and requeueing it.
        // A cancelled/interrupted attempt is not the accepted request's final outcome.
        let queue = barca_service::serve::run_queue::list(read.path()).await?;
        let queued = queue.iter().find(|r| r.run_id == run_id);
        let run = barca_service::db::get_run(read.path(), run_id).await?;
        if queued.is_none() && run.is_none() {
            return Err(BarcaError::Usage(format!(
                "run '{run_id}' was not found; use barca history --json to find a run id"
            )));
        }
        let status = if queued.is_some() {
            "running"
        } else {
            run.as_ref().unwrap().status.as_str()
        };
        let is_waiting =
            queued.is_some_and(|r| r.state == barca_service::serve::run_queue::QueueState::Queued);
        if is_waiting && !waiting {
            barca_service::errln!("[barca] run {run_id} requeued; waiting for the server");
        }
        waiting = is_waiting;
        // Archived streams followed by the current one form one append-only sequence,
        // including attempt boundaries. A new spool's offsets never reuse the old cursor.
        let lines =
            barca_service::queries::logs_at(&cfg.db_path, read.path(), run_id, None, None).await?;
        for entry in lines.iter().skip(seen) {
            if args.selects(&entry.node_id, entry.stream) {
                args.print_one(entry);
            }
        }
        seen = seen.max(lines.len());
        if status != "running" {
            let since = *ended_at.get_or_insert_with(Instant::now);
            let spool = barca_service::run_logs::spool_path(&cfg.db_path, run_id);
            if !spool.exists() || since.elapsed() >= WRAP_UP {
                return Ok(status.to_string());
            }
        } else {
            ended_at = None;
        }
        tokio::time::sleep(POLL).await;
    }
}
