//! The `runs` table: a run's row as it starts ([`start_run`]) and ends ([`finish_run`]), its
//! stored outcome, the history read back ([`RunRecord`]), and deletions from run history (a
//! `runs` row and the `logs` lines recorded under it, and the `outcome_json` of old runs).
//! Recording a run's steps is [`crate::persist`]'s; who is running a `running` run is
//! [`super::run_liveness`]'s.

use super::*;
use libsql::Connection;

pub(crate) async fn delete_run(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute("DELETE FROM runs WHERE run_id = ?1", [run_id])
        .await
}

pub(crate) async fn delete_logs(conn: &Connection, run_id: &str) -> Result<u64, libsql::Error> {
    conn.execute("DELETE FROM logs WHERE run_id = ?1", [run_id])
        .await
}

/// Clear `outcome_json` of up to `batch` of the oldest runs that have one, except the last
/// `kept` runs. The number of runs changed.
pub(crate) async fn clear_old_outcomes(
    conn: &Connection,
    kept: i64,
    batch: i64,
) -> Result<u64, libsql::Error> {
    conn.execute(
        &format!(
            "UPDATE runs SET outcome_json = NULL WHERE id IN (SELECT id FROM runs \
             WHERE outcome_json IS NOT NULL AND id < (SELECT id FROM runs ORDER BY \
             id DESC LIMIT 1 OFFSET {}) ORDER BY id LIMIT {batch})",
            kept - 1
        ),
        (),
    )
    .await
}

/// The `runs.files` column: a JSON array of paths. Rows written before 0.12 hold the paths
/// joined by spaces; those are split on whitespace.
pub fn encode_files(files: &[String]) -> String {
    serde_json::to_string(files).unwrap_or_default()
}

/// See [`encode_files`].
pub fn decode_files(raw: &str) -> Vec<String> {
    if raw.trim_start().starts_with('[')
        && let Ok(files) = serde_json::from_str::<Vec<String>>(raw)
    {
        return files;
    }
    raw.split_whitespace().map(str::to_string).collect()
}

/// Record of a single run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub command: String,
    /// The `.py` files the run was given.
    pub files: Vec<String>,
    pub target: Option<String>,
    /// The run's job: `all` for a run without targets, otherwise its resolved node ids, sorted
    /// and comma-separated (`barca.job` in telemetry). `null` for runs recorded before 0.29.0.
    #[serde(default)]
    pub job: Option<String>,
    pub status: String,
    pub steps_total: Option<i64>,
    pub steps_executed: i64,
    pub steps_cached: i64,
    /// Recorded steps that failed (a partitioned node's keys each count); `null` when not read.
    /// `GET /runs` shows it (#758); `barca history --json` does not yet, so it is not written.
    #[serde(default, skip_serializing)]
    pub steps_failed: Option<i64>,
    /// Total recorded materialization attempts; cached steps contribute zero.
    #[serde(default)]
    pub attempts: Option<i64>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
    /// What started the run: `manual` (a command typed on a machine), `api`, `schedule` or
    /// `client`. `null` for runs recorded before barca recorded it.
    #[serde(default)]
    pub triggered_by: Option<crate::outcome::TriggeredBy>,
    /// For a scheduled run: the tick it was started for (RFC 3339, UTC).
    #[serde(default)]
    pub scheduled_at: Option<String>,
    /// For a scheduled run: the schedule's id (the scheduled node).
    #[serde(default)]
    pub schedule_id: Option<String>,
    /// For a run a laptop posted to a server's ledger (`triggered_by: client`): the run id it
    /// had on that laptop. `null` for every other run.
    #[serde(default)]
    pub client_run_id: Option<String>,
    /// For a run with a failed step: the first step that failed and the first line of its
    /// error. `null` for every other run (also for a run that failed before any step did).
    #[serde(default)]
    pub failure: Option<RunFailure>,
}

/// A recorded step error as (type, message, traceback). A worker reports a Python exception as
/// the generic `WorkerError` whose text is `Type: message` followed by the traceback frames;
/// this takes the type and the frames back out of it.
pub fn split_exception(
    error_type: &str,
    message: &str,
    traceback: &str,
) -> (String, String, Option<String>) {
    // The frames are the trailing run of `  File "..."` lines and their indented source lines.
    // Taking only that run keeps a message that itself quotes a traceback in one piece.
    let lines: Vec<&str> = message.split('\n').collect();
    let mut first_frame = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        if line.starts_with("  File \"") {
            first_frame = i;
        } else if !line.starts_with("    ") {
            break;
        }
    }
    let joined;
    let (text, frames) = if first_frame > 0 && first_frame < lines.len() {
        joined = lines[..first_frame].join("\n");
        (joined.as_str(), Some(lines[first_frame..].join("\n")))
    } else {
        (message, None)
    };
    let stack = Some(traceback.to_string())
        .filter(|t| !t.is_empty())
        .or(frames);
    if error_type == "WorkerError"
        && let Some((head, rest)) = text.split_once(": ")
        && !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return (head.to_string(), rest.to_string(), stack);
    }
    (error_type.to_string(), text.to_string(), stack)
}

/// Why a run failed, in one line: its first failed step and that step's error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunFailure {
    /// The node id of the step, with its partition key when it has one.
    pub node: String,
    /// `ErrorType: message`, the message's first line only.
    pub error: String,
}

impl RunFailure {
    /// From a failed row's `error_type` and `error_message` (see [`split_exception`]).
    pub fn new(node: String, error_type: &str, message: &str) -> Self {
        let (error_type, message, _) = split_exception(error_type, message, "");
        let first = message.lines().next().unwrap_or_default().trim_end();
        let error = match (error_type.is_empty(), first.is_empty()) {
            (true, _) => first.to_string(),
            (false, true) => error_type.to_string(),
            (false, false) => format!("{error_type}: {first}"),
        };
        Self { node, error }
    }
}

/// What a run reads from the metadata DB as it starts ([`start_run`]).
pub struct StartedRun {
    /// Logical-node scheduling priors for the requested plan.
    pub cost_estimates: Vec<(String, crate::cost::NodeEstimate)>,
    /// [`get_avg_elapsed`] of the run's unpartitioned nodes.
    pub avg_elapsed: HashMap<String, f64>,
    /// [`get_avg_elapsed_for_partitioned`] of its partitioned base ids.
    pub partitioned_avg_elapsed: HashMap<String, f64>,
}

/// Start a run in one visit to the metadata DB: initialize it ([`init_db`]), record the run
/// ([`create_run_with`]) and read what the run needs (cost estimates, average step times). These
/// were four opens of the database, each with its lock and schema check (#560).
pub async fn start_run(
    db_path: &str,
    run: NewRun<'_>,
    node_ids: &[String],
    base_ids: &[String],
    definitions: &[(String, String)],
) -> Result<StartedRun, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (handle, conn, _) = initialized(db_path, db_path)
        .await
        .map_err(|e| named(e, db_path))?;
    let started = async {
        insert_run(&conn, db_path, &run).await;
        record_definitions(&conn, definitions).await;
        Ok(StartedRun {
            cost_estimates: {
                let ids: std::collections::BTreeSet<_> =
                    node_ids.iter().chain(base_ids).cloned().collect();
                cost_estimates_on(&conn, Some(&ids.into_iter().collect::<Vec<_>>())).await?
            },
            avg_elapsed: avg_elapsed_on(&conn, node_ids).await?,
            partitioned_avg_elapsed: partitioned_avg_elapsed_on(&conn, base_ids).await?,
        })
    }
    .await;
    drop(conn);
    drop(handle);
    started
}

/// Generate a short run ID from timestamp + random bits (no uuid crate).
pub fn generate_run_id() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Mix in the address of a stack variable for pseudo-randomness.
    let stack_addr = &nanos as *const _ as u64;
    let mixed = nanos as u64 ^ stack_addr;
    format!("{:012x}", mixed & 0xFFFF_FFFF_FFFF)
}

/// Create a new run record at the start of execution, started by hand.
pub async fn create_run(
    db_path: &str,
    run_id: &str,
    command: &str,
    files: &str,
    target: Option<&str>,
    steps_total: Option<usize>,
) -> Result<(), BarcaError> {
    create_run_with(
        db_path,
        NewRun {
            run_id,
            command,
            files,
            target,
            steps_total,
            job: None,
            trigger: &crate::outcome::Trigger::default(),
        },
    )
    .await
}

/// A run about to start.
pub struct NewRun<'a> {
    pub run_id: &'a str,
    pub command: &'a str,
    /// [`encode_files`] of the files.
    pub files: &'a str,
    pub target: Option<&'a str>,
    pub steps_total: Option<usize>,
    /// The canonical job id (`persist::canonical_job`), when known at the start. Written with
    /// the row, so a `barca serve` on the same project sees which nodes a running CLI run
    /// covers and skips their ticks (#479). [`crate::persist`] writes it again at the end.
    pub job: Option<&'a str>,
    pub trigger: &'a crate::outcome::Trigger,
}

/// Create a new run record at the start of execution.
pub async fn create_run_with(db_path: &str, run: NewRun<'_>) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    insert_run(&conn, db_path, &run).await;
    Ok(())
}

/// The row of a run that is starting, and the runs it finds interrupted ([`create_run_with`]).
async fn insert_run(conn: &libsql::Connection, db_path: &str, run: &NewRun<'_>) {
    // The marker exists from before the row does until the run has recorded its outcome
    // ([`finish_run`], or the end of this process).
    let owner = crate::run_owner::claim(db_path, run.run_id);
    conn.execute(
        "INSERT INTO runs (run_id, command, files, target, status, steps_total, pid, host, owner, triggered_by, scheduled_at, schedule_id, job) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, NULLIF(?8, ''), ?9, NULLIF(?10, ''), NULLIF(?11, ''), NULLIF(?12, ''))",
        [
            run.run_id.to_string(),
            run.command.to_string(),
            run.files.to_string(),
            run.target.unwrap_or("").to_string(),
            run.steps_total.map(|n| n.to_string()).unwrap_or_default(),
            std::process::id().to_string(),
            local_host(),
            owner,
            run.trigger.triggered_by.as_str().to_string(),
            run.trigger.scheduled_at.clone().unwrap_or_default(),
            run.trigger.schedule_id.clone().unwrap_or_default(),
            run.job.unwrap_or("").to_string(),
        ],
    )
    .await
    .ok();
    mark_interrupted_runs(conn, db_path).await;
}

/// Record a run that ended before it started executing (its target was refused, it was
/// cancelled while queued, its source no longer loads): a terminal row with its outcome, so
/// the id a server answered with is found in the history like any other run.
pub async fn record_unstarted_run(
    db_path: &str,
    run: NewRun<'_>,
    status: &str,
    outcome_json: &str,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    conn.execute(
        "INSERT INTO runs (run_id, command, files, target, status, steps_total, finished_at, elapsed_seconds, triggered_by, scheduled_at, schedule_id, outcome_json) VALUES (?1, ?2, ?3, ?4, ?5, 0, datetime('now'), 0, ?6, NULLIF(?7, ''), NULLIF(?8, ''), ?9) ON CONFLICT(run_id) DO NOTHING",
        [
            run.run_id.to_string(),
            run.command.to_string(),
            run.files.to_string(),
            run.target.unwrap_or("").to_string(),
            status.to_string(),
            run.trigger.triggered_by.as_str().to_string(),
            run.trigger.scheduled_at.clone().unwrap_or_default(),
            run.trigger.schedule_id.clone().unwrap_or_default(),
            outcome_json.to_string(),
        ],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to record run: {e}")))?;
    Ok(())
}

/// Store a finished run's outcome (`barca serve`): `GET /runs/{run_id}` answers from it after
/// a restart. `status` corrects the row when the run ended before it recorded one.
pub async fn set_run_outcome(
    db_path: &str,
    run_id: &str,
    status: &str,
    outcome_json: &str,
) -> Result<bool, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let changed = conn
        .execute(
            "UPDATE runs SET outcome_json = ?2, status = CASE WHEN status = 'running' THEN ?3 ELSE status END, finished_at = COALESCE(finished_at, datetime('now')) WHERE run_id = ?1",
            [
                run_id.to_string(),
                outcome_json.to_string(),
                status.to_string(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to record run outcome: {e}")))?;
    Ok(changed == 1)
}

/// [`set_run_outcome`] over whatever status the run recorded: a run `barca serve` stopped at its
/// run timeout recorded itself `cancelled` (it was stopped as a cancel is) and is an `error`
/// (#628).
pub async fn replace_run_outcome(
    db_path: &str,
    run_id: &str,
    status: &str,
    outcome_json: &str,
) -> Result<bool, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let changed = conn
        .execute(
            "UPDATE runs SET outcome_json = ?2, status = ?3, finished_at = COALESCE(finished_at, datetime('now')) WHERE run_id = ?1",
            [
                run_id.to_string(),
                outcome_json.to_string(),
                status.to_string(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to record run outcome: {e}")))?;
    Ok(changed == 1)
}

/// The artifact (path, format) `run_id` wrote for `node_id`, if it wrote one.
pub async fn run_artifact(
    db_path: &str,
    run_id: &str,
    node_id: &str,
) -> Result<Option<(String, String)>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query(
            "SELECT artifact_path, COALESCE(artifact_format, '') FROM materializations WHERE run_id = ?1 AND node_id = ?2 AND status = 'success' AND artifact_path IS NOT NULL ORDER BY id DESC LIMIT 1",
            [run_id, node_id],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run artifact: {e}")))?;
    let row = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run artifact: {e}")))?;
    Ok(row.and_then(|row| Some((row.get::<String>(0).ok()?, row.get::<String>(1).ok()?))))
}

/// A run's stored outcome ([`set_run_outcome`]), if it has one.
pub async fn get_run_outcome(db_path: &str, run_id: &str) -> Result<Option<String>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    if !has_column(&conn, "runs", "outcome_json").await? {
        return Ok(None);
    }
    let mut rows = conn
        .query("SELECT outcome_json FROM runs WHERE run_id = ?1", [run_id])
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run outcome: {e}")))?;
    let row = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run outcome: {e}")))?;
    Ok(row
        .and_then(|row| row.get::<String>(0).ok())
        .filter(|s| !s.is_empty()))
}

/// Finalize a run record with status and stats.
pub async fn finish_run(
    db_path: &str,
    run_id: &str,
    status: &str,
    steps_executed: usize,
    steps_cached: usize,
    elapsed_seconds: f64,
) -> Result<(), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let updated = conn.execute(
        "UPDATE runs SET status = ?1, steps_executed = ?2, steps_cached = ?3, elapsed_seconds = ?4, finished_at = datetime('now') WHERE run_id = ?5",
        [
            status.to_string(),
            steps_executed.to_string(),
            steps_cached.to_string(),
            elapsed_seconds.to_string(),
            run_id.to_string(),
        ],
    )
    .await
    .map_err(|e| BarcaError::Db(format!("failed to finish run: {e}")))?;
    if updated != 1 {
        return Err(BarcaError::Db("cannot finish a missing run".to_string()));
    }
    crate::run_owner::release(db_path, run_id);
    Ok(())
}

/// Retrieve recent run records, newest first.
pub async fn get_recent_runs(db_path: &str, limit: usize) -> Result<Vec<RunRecord>, BarcaError> {
    read_runs(db_path, None, limit).await
}

/// Explicit operator reconciliation when a run is known to have stopped but its
/// owner cannot be proven dead (for example, an old row or a VM-shared mount).
/// Positive live-owner evidence always refuses the operation. Preserve successful
/// materializations and all counters; nobody observed an end time or duration.
pub async fn reconcile_run(db_path: &str, run_id: &str) -> Result<RunRecord, BarcaError> {
    {
        let _g = db_guard_for(db_path).await;
        let (_db, conn) = open_conn(db_path).await?;
        let mut rows = conn
            .query(
                "SELECT status, pid, COALESCE(owner, '') FROM runs WHERE run_id = ?1",
                [run_id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read run: {e}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| BarcaError::Db(format!("failed to read run: {e}")))?
        else {
            return Err(BarcaError::Usage(format!(
                "no run '{run_id}' in this environment\nUse barca history --all to find the run id."
            )));
        };
        let status = row
            .get::<String>(0)
            .map_err(|e| BarcaError::Db(e.to_string()))?;
        if status != "running" {
            return Err(BarcaError::Usage(format!(
                "run '{run_id}' is already {status}; only running rows can be reconciled"
            )));
        }
        let owner = row
            .get::<String>(2)
            .map_err(|e| BarcaError::Db(e.to_string()))?;
        let on = crate::run_owner::System::for_db(db_path);
        if crate::run_owner::judge_json(row.get::<i64>(1).ok(), &owner, &on)
            == crate::run_owner::Owner::Alive
        {
            return Err(BarcaError::Usage(format!(
                "run '{run_id}' has a live owner\nWait for or stop that run before reconciling its history."
            )));
        }
        drop(rows);
        let changed = conn
            .execute(
                "UPDATE runs SET status = 'interrupted' WHERE run_id = ?1 AND status = 'running'",
                [run_id],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to reconcile run: {e}")))?;
        if changed == 0 {
            return Err(BarcaError::Usage(format!(
                "run '{run_id}' ended while it was being reconciled; read barca history again"
            )));
        }
    }
    get_run(db_path, run_id)
        .await?
        .ok_or_else(|| BarcaError::Db(format!("reconciled run '{run_id}' could not be read")))
}

/// Read a durable run by id, including the same interrupted-owner diagnosis as history.
pub async fn get_run(db_path: &str, run_id: &str) -> Result<Option<RunRecord>, BarcaError> {
    Ok(read_runs(db_path, Some(run_id), 1)
        .await?
        .into_iter()
        .next())
}

/// A run's first failed step and its error, read from `idx_mat_failed` ([`read_runs`]).
const FAILURE_INDEXED: &str = "(SELECT node_id || char(31) || COALESCE(error_type, '') || char(31) || COALESCE(error_message, '') FROM materializations INDEXED BY idx_mat_failed WHERE run_id = runs.run_id AND status = 'failed' ORDER BY id LIMIT 1)";
/// How many of a run's recorded steps failed, read from `idx_mat_failed` ([`read_runs`]).
const FAILED_COUNT_INDEXED: &str = "(SELECT COUNT(*) FROM materializations INDEXED BY idx_mat_failed WHERE run_id = runs.run_id AND status = 'failed')";

async fn read_runs(
    db_path: &str,
    run_id: Option<&str>,
    limit: usize,
) -> Result<Vec<RunRecord>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    // A run whose process died is reported as `interrupted`, not as still `running`. Reported,
    // not written: reading history must not change the database (the next run records it).
    let interrupted = interrupted_runs(&conn, db_path).await;
    // Inspection also reads legacy snapshots without migrating them. Unknown attempt
    // metadata is null, rather than fabricating a zero or requiring newer columns.
    let mut columns = conn
        .query("PRAGMA table_info(materializations)", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to inspect materialization columns: {e}")))?;
    let mut names = std::collections::HashSet::new();
    while let Some(row) = columns
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read materialization columns: {e}")))?
    {
        names.insert(
            row.get::<String>(1)
                .map_err(|e| BarcaError::Db(e.to_string()))?,
        );
    }
    drop(columns);
    // The sum of a run's attempts, read from indexes only: its row count (`idx_mat_run`),
    // corrected by the few rows whose attempts are not 1 (`idx_mat_attempts`). Summing the
    // column itself reads every row of the run, 200k for a run of a 200k-key node.
    let attempts = if !(names.contains("run_id") && names.contains("attempts")) {
        "NULL"
    } else if has_index(&conn, "idx_mat_attempts").await? {
        "((SELECT COUNT(*) FROM materializations INDEXED BY idx_mat_run WHERE run_id = runs.run_id) \
          - (SELECT COUNT(*) - COALESCE(SUM(attempts), 0) FROM materializations \
             INDEXED BY idx_mat_attempts WHERE run_id = runs.run_id \
             AND (attempts IS NULL OR attempts != 1)))"
    } else {
        "(SELECT COALESCE(SUM(attempts), 0) FROM materializations WHERE run_id = runs.run_id)"
    };
    let trigger = if has_column(&conn, "runs", "triggered_by").await? {
        "triggered_by, scheduled_at, schedule_id"
    } else {
        "NULL, NULL, NULL"
    };
    let client = if has_column(&conn, "runs", "client_run_id").await? {
        "client_run_id"
    } else {
        "NULL"
    };
    let job = if has_column(&conn, "runs", "job").await? {
        "job"
    } else {
        "NULL"
    };
    // The first failed step and its error, joined by the unit separator (a byte no node id,
    // error type or message line holds).
    let (failure, failed) = if !(names.contains("run_id") && names.contains("error_type")) {
        ("NULL", "NULL")
    } else if has_index(&conn, "idx_mat_failed").await? {
        // Named: without statistics SQLite's planner picks `idx_mat_run` and reads every row
        // of the run to check its status.
        (FAILURE_INDEXED, FAILED_COUNT_INDEXED)
    } else {
        (
            "(SELECT node_id || char(31) || COALESCE(error_type, '') || char(31) || COALESCE(error_message, '') FROM materializations WHERE run_id = runs.run_id AND status = 'failed' ORDER BY id LIMIT 1)",
            "(SELECT COUNT(*) FROM materializations WHERE run_id = runs.run_id AND status = 'failed')",
        )
    };
    let mut rows = conn
        .query(
            &format!("SELECT run_id, command, files, target, status, steps_total, steps_executed, steps_cached, started_at, finished_at, elapsed_seconds, {attempts}, {trigger}, {client}, {job}, {failure}, {failed} FROM runs WHERE (?1 = '' OR run_id = ?1) ORDER BY id DESC LIMIT ?2"),
            [run_id.unwrap_or_default().to_string(), limit.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to query runs: {e}")))?;
    let mut records = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
    {
        let run_id = row.get::<String>(0).unwrap_or_default();
        let status = if interrupted.contains(&run_id) {
            "interrupted".to_string()
        } else {
            row.get::<String>(4).unwrap_or_default()
        };
        records.push(RunRecord {
            run_id,
            command: row.get::<String>(1).unwrap_or_default(),
            files: decode_files(&row.get::<String>(2).unwrap_or_default()),
            target: {
                let t = row.get::<String>(3).unwrap_or_default();
                if t.is_empty() { None } else { Some(t) }
            },
            status,
            steps_total: row.get::<i64>(5).ok(),
            steps_executed: row.get::<i64>(6).unwrap_or(0),
            steps_cached: row.get::<i64>(7).unwrap_or(0),
            steps_failed: row.get::<i64>(18).ok(),
            attempts: row.get::<i64>(11).ok(),
            started_at: row.get::<String>(8).unwrap_or_default(),
            finished_at: {
                let t = row.get::<String>(9).unwrap_or_default();
                if t.is_empty() { None } else { Some(t) }
            },
            elapsed_seconds: row.get::<f64>(10).ok(),
            triggered_by: row
                .get::<String>(12)
                .ok()
                .and_then(|t| crate::outcome::TriggeredBy::parse(&t)),
            scheduled_at: row.get::<String>(13).ok().filter(|t| !t.is_empty()),
            schedule_id: row.get::<String>(14).ok().filter(|t| !t.is_empty()),
            client_run_id: row.get::<String>(15).ok().filter(|t| !t.is_empty()),
            job: row.get::<String>(16).ok().filter(|t| !t.is_empty()),
            failure: row.get::<String>(17).ok().and_then(|raw| {
                let mut parts = raw.splitn(3, '\u{1f}');
                let node = parts.next()?.to_string();
                let error_type = parts.next().unwrap_or_default();
                let message = parts.next().unwrap_or_default();
                Some(RunFailure::new(node, error_type, message))
            }),
        });
    }
    Ok(records)
}

/// Total number of recorded runs (for `barca history` truncation reporting).
pub async fn count_runs(db_path: &str) -> Result<usize, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut rows = conn
        .query("SELECT COUNT(*) FROM runs", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to count runs: {e}")))?;
    let n = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read row: {e}")))?
        .map(|r| r.get::<i64>(0).unwrap_or(0))
        .unwrap_or(0);
    Ok(n.max(0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_run_records_its_trigger_and_a_served_run_its_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();
        init_db(&db_path).await.unwrap();
        let trigger = crate::outcome::Trigger::schedule("p.py:a", 0);
        create_run_with(
            &db_path,
            NewRun {
                run_id: "tick",
                command: "get",
                files: "[]",
                target: Some("p.py:a"),
                steps_total: Some(1),
                job: Some("p.py:a"),
                trigger: &trigger,
            },
        )
        .await
        .unwrap();
        let run = get_run(&db_path, "tick").await.unwrap().unwrap();
        assert_eq!(
            run.triggered_by,
            Some(crate::outcome::TriggeredBy::Schedule)
        );
        assert_eq!(run.scheduled_at.as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(run.schedule_id.as_deref(), Some("p.py:a"));
        assert!(get_run_outcome(&db_path, "tick").await.unwrap().is_none());
        assert!(
            set_run_outcome(&db_path, "tick", "success", r#"{"status":"success"}"#)
                .await
                .unwrap()
        );
        assert_eq!(
            get_run(&db_path, "tick").await.unwrap().unwrap().status,
            "success"
        );
        assert_eq!(
            get_run_outcome(&db_path, "tick").await.unwrap().as_deref(),
            Some(r#"{"status":"success"}"#)
        );
        // A run that never started still has a row, with its outcome.
        record_unstarted_run(
            &db_path,
            NewRun {
                run_id: "queued",
                command: "run",
                files: "[]",
                target: Some("t"),
                steps_total: None,
                job: None,
                trigger: &crate::outcome::Trigger::api(),
            },
            "cancelled",
            r#"{"status":"cancelled"}"#,
        )
        .await
        .unwrap();
        let run = get_run(&db_path, "queued").await.unwrap().unwrap();
        assert_eq!(run.status, "cancelled");
        assert_eq!(run.triggered_by, Some(crate::outcome::TriggeredBy::Api));
        assert!(run.finished_at.is_some());
        // An ordinary run is recorded as started by hand.
        create_run(&db_path, "cli", "get", "[]", None, Some(1))
            .await
            .unwrap();
        assert_eq!(
            get_run(&db_path, "cli")
                .await
                .unwrap()
                .unwrap()
                .triggered_by,
            Some(crate::outcome::TriggeredBy::Manual)
        );
    }

    /// The runs list reads each run's first failure from `idx_mat_failed`: the plan names it,
    /// and the answer is the one a scan of the run's rows gives.
    #[tokio::test]
    async fn a_runs_first_failure_is_read_from_its_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        init_db(&path).await.unwrap();
        create_run(&path, "r", "get", "[]", None, Some(3))
            .await
            .unwrap();
        {
            let _g = db_guard().await;
            let (_db, conn) = open_conn(&path).await.unwrap();
            for (node, status, error) in [
                ("p.py:ok", "success", ""),
                ("p.py:first", "failed", "first bad"),
                ("p.py:second", "failed", "second bad"),
            ] {
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, status, error_type, \
                     error_message) VALUES ('r', ?1, ?2, 'ValueError', NULLIF(?3, ''))",
                    [node, status, error],
                )
                .await
                .unwrap();
            }
            let mut plan = conn
                .query(
                    &format!("EXPLAIN QUERY PLAN SELECT {FAILURE_INDEXED} FROM runs"),
                    (),
                )
                .await
                .unwrap();
            let mut details = Vec::new();
            while let Some(row) = plan.next().await.unwrap() {
                details.push(row.get::<String>(3).unwrap());
            }
            assert!(
                details.iter().any(|d| d.contains("idx_mat_failed")),
                "{details:?}"
            );
        }
        let run = get_run(&path, "r").await.unwrap().unwrap();
        let failure = run.failure.expect("the run has a failed step");
        assert_eq!(failure.node, "p.py:first");
        assert_eq!(failure.error, "ValueError: first bad");
    }
}
