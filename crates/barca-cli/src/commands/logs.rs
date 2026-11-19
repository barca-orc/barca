//! `barca logs`: a run's recorded step output (stdout and stderr), independently of progress
//! and result rendering. Locally it reads the metadata DB, and the spool of a run still in
//! progress (`barca_service::run_logs`); against a server, `client::logs`.

use crate::print::{follow_ended, follow_exit_code, name_run};
use crate::request::LogsArgs;
use barca_service::BarcaError;
use barca_service::db::LogEntry;
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
    barca_service::term::exit(follow_exit_code(&status));
}

/// `-f` on this machine: print the run's lines from its spool as they arrive, and once the
/// run is recorded as ended, the rest from the database. Returns the run's final status.
async fn follow_local(
    cfg: &barca_service::config::ResolvedConfig,
    run_id: &str,
    args: LogsArgs<'_>,
) -> Result<String, BarcaError> {
    let Some(run) = barca_service::queries::run_record(cfg, run_id).await? else {
        return Err(BarcaError::Usage(format!(
            "run '{run_id}' was not found; use barca history --json to find a run id"
        )));
    };
    if run.status != "running" {
        // Ended already: print what it recorded, checked as without -f.
        let lines = barca_service::queries::logs(cfg, run_id, args.step, args.stream).await?;
        args.print_all(&lines)?;
        return Ok(run.status);
    }
    let spool = barca_service::run_logs::spool_path(&cfg.db_path, run_id);
    // Lines seen so far (printed or not), which is the seq of the next one.
    let mut seen = 0usize;
    let mut offset = 0u64;
    let mut ended_at: Option<Instant> = None;
    loop {
        let status = barca_service::db::get_run(&cfg.db_path, run_id)
            .await?
            .map_or_else(|| "interrupted".to_string(), |r| r.status);
        let (lines, at) = barca_service::run_logs::read_spool(&spool, offset)?;
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
            for entry in barca_service::db::get_logs(&cfg.db_path, run_id).await? {
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
