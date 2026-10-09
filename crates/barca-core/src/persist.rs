//! Recording a run: the run row, each step's row as it finishes, the final write, and the push of
//! shared history. Also the telemetry report a finished run hands to the configured integration.

use crate::BarcaError;
use crate::dag::Dag;
use crate::db;
use crate::dispatch;
use crate::state_sync;
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

/// The upload of one run's record to the shared history.
pub(crate) struct SharedPush<'a> {
    pub(crate) python: &'a std::path::Path,
    pub(crate) cfg: &'a crate::config::ResolvedConfig,
    pub(crate) db_path: &'a str,
    pub(crate) run_id: &'a str,
    pub(crate) logs: &'a [(String, String)],
    /// The token of the shared history the local one was last brought up to.
    pub(crate) token: state_sync::StateToken,
}

impl SharedPush<'_> {
    /// Upload the local history. On conflict (another machine pushed first): pull the fresh
    /// history, replay this run's ledger onto it, and upload again, up to `push_retries`
    /// times. Returns the number of retries. `Err(BarcaError::Cancelled)` when `until` stopped
    /// it; the shared history is then the old one or the new one, never part of one.
    pub(crate) async fn run(
        &mut self,
        ledger: &RunLedger<'_>,
        until: state_sync::Until<'_>,
    ) -> Result<u32, BarcaError> {
        self.sync(Some(ledger), until).await
    }

    async fn checkpoint(&mut self, until: state_sync::Until<'_>) -> Result<u32, BarcaError> {
        self.sync(None, until).await
    }

    async fn sync(
        &mut self,
        ledger: Option<&RunLedger<'_>>,
        until: state_sync::Until<'_>,
    ) -> Result<u32, BarcaError> {
        let (python, cfg) = (self.python, self.cfg);
        let mut attempt = 0u32;
        let mut pushed_again = false;
        loop {
            let again = match state_sync::push_state(python, cfg, &self.token, until).await? {
                // Uploaded, but another process wrote to the local database (or replaced
                // it) while the upload was on its way. Treated like a conflict, once: pull
                // what was just uploaded, which keeps those rows, and push again. Only once,
                // because a run going in the same project writes during every upload, and
                // chasing it would cost a pull and an upload each time for rows that run
                // pushes itself when it ends. The upload stands either way; rows written
                // after it go with the next push from this machine.
                state_sync::PushOutcome::Pushed {
                    token,
                    local_unchanged,
                } => {
                    // Keep the acknowledgement even if a subsequent pull/replay fails.
                    self.token = state_sync::StateToken(Some(token));
                    !local_unchanged
                        && !std::mem::replace(&mut pushed_again, true)
                        && attempt < cfg.push_retries
                }
                state_sync::PushOutcome::Conflict => {
                    if attempt >= cfg.push_retries {
                        return Err(BarcaError::Other(format!(
                            "shared state push conflicted {attempt} times — results were \
                             computed but the shared state was not updated; re-run to retry"
                        )));
                    }
                    true
                }
            };
            if !again {
                return Ok(attempt);
            }
            attempt += 1;
            // The pull carries this run's rows over with the rest of the local database; the
            // ledger then adds whatever is still missing (both are idempotent), so the run
            // is whole however much of it made the trip.
            self.token = state_sync::pull_state(python, cfg, until).await?.token;
            db::init_db(self.db_path).await?;
            if let Some(ledger) = ledger {
                persist_run(self.db_path, ledger).await?;
                db::insert_logs(self.db_path, self.run_id, self.logs).await?;
            }
        }
    }
}

/// Record as `cancelled` a run that was already recorded with its outcome, because Ctrl-C
/// arrived while its record was being shared. The steps it recorded stay: they finished.
pub(crate) async fn cancel_recorded_run(
    db_path: &str,
    l: &RunLedger<'_>,
) -> Result<(), BarcaError> {
    db::finish_run(
        db_path,
        l.run_id,
        "cancelled",
        l.steps_executed,
        l.steps_cached,
        l.elapsed,
    )
    .await
}

/// Everything one run wants written to the metadata DB, held in memory so a
/// state-push conflict can replay it onto a freshly pulled database.
pub(crate) struct RunLedger<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) status: &'a str,
    pub(crate) command: &'a str,
    pub(crate) files: String,
    pub(crate) target: Option<&'a str>,
    pub(crate) steps_total: usize,
    pub(crate) steps_executed: usize,
    pub(crate) steps_cached: usize,
    pub(crate) elapsed: f64,
    pub(crate) all_outputs: &'a HashMap<String, dispatch::OutputRef>,
    pub(crate) all_failures: &'a [dispatch::StepFailure],
    pub(crate) all_sinks: &'a HashMap<String, String>,
    pub(crate) all_attempts: &'a HashMap<String, u32>,
    /// Per-node worker self-timing: (cpu_seconds, max_rss_bytes).
    pub(crate) all_timings: &'a HashMap<String, (Option<f64>, Option<u64>)>,
    pub(crate) cached_node_ids: &'a std::collections::HashSet<String>,
    pub(crate) run_hashes: &'a HashMap<String, String>,
    /// Sensor step -> content hash of the output it returned in this run. Other steps
    /// record the hash on their `OutputRef`, when the artifact went through a store.
    pub(crate) output_hashes: &'a HashMap<String, String>,
    /// Artifact-store location of each uploaded output, recorded instead of
    /// its local path so cache hits resolve on every machine.
    pub(crate) store_paths: &'a HashMap<String, String>,
    /// Run-end snapshot of the measured-cost EWMA, seeding the next run.
    pub(crate) cost_snapshot: &'a [(String, crate::cost::NodeEstimate)],
}

/// One successful step as a `materializations` row. Built when the step finishes (for the
/// [`StepRecorder`]) and again from the ledger at the end of the run.
#[derive(Debug, Clone)]
pub(crate) struct StepRow {
    node_id: String,
    run_hash: String,
    path: String,
    format: String,
    size_bytes: u64,
    elapsed_seconds: Option<f64>,
    attempts: u32,
    sinks_json: Option<String>,
    cpu_seconds: Option<f64>,
    max_rss_bytes: Option<u64>,
    /// Worker sensor content hash, or the confirmed artifact-transfer hash.
    output_hash: Option<String>,
}

impl StepRow {
    /// From the artifact a worker reported for a finished step. Reads the same fields the
    /// end-of-run ledger is built from, so both writers produce the same row.
    pub(crate) fn from_artifact(
        node_id: &str,
        run_hash: &str,
        artifact: &serde_json::Value,
        attempts: u32,
    ) -> Self {
        let str_of = |key: &str| artifact.get(key).and_then(|v| v.as_str());
        Self {
            node_id: node_id.to_string(),
            run_hash: run_hash.to_string(),
            path: str_of("path").unwrap_or("").to_string(),
            format: str_of("format").unwrap_or("json").to_string(),
            size_bytes: artifact
                .get("size_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            elapsed_seconds: artifact.get("elapsed_seconds").and_then(|v| v.as_f64()),
            attempts,
            sinks_json: artifact
                .get("sinks")
                .and_then(|v| v.as_array())
                .filter(|sinks| !sinks.is_empty())
                .map(|sinks| serde_json::Value::from(sinks.clone()).to_string()),
            cpu_seconds: artifact.get("cpu_seconds").and_then(|v| v.as_f64()),
            max_rss_bytes: artifact.get("max_rss_bytes").and_then(|v| v.as_u64()),
            output_hash: str_of("content_hash").map(str::to_string),
        }
    }

    async fn insert(&self, conn: &turso::Connection, run_id: &str) -> Result<u64, turso::Error> {
        let opt = |v: Option<String>| v.unwrap_or_default();
        // Turso otherwise intersects the node and run indexes, scanning this run
        // for each partition. Bound deduplication to the node's own history.
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds, status, attempts, sinks_json, cpu_seconds, max_rss_bytes, output_hash, run_id) SELECT ?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), 'success', ?7, NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), ?12 WHERE NOT EXISTS (SELECT 1 FROM materializations INDEXED BY idx_mat_node_run WHERE node_id = ?1 AND run_id = ?12)",
            [
                self.node_id.clone(),
                self.run_hash.clone(),
                self.path.clone(),
                self.format.clone(),
                self.size_bytes.to_string(),
                opt(self.elapsed_seconds.map(|e| e.to_string())),
                self.attempts.to_string(),
                opt(self.sinks_json.clone()),
                opt(self.cpu_seconds.map(|c| c.to_string())),
                opt(self.max_rss_bytes.map(|r| r.to_string())),
                opt(self.output_hash.clone()),
                run_id.to_string(),
            ],
        )
        .await
    }
}

/// How often, at most, the recorder writes finished steps to the metadata DB. A finished step
/// is recorded within about this long of finishing; a run shorter than this records everything
/// in the one end-of-run write, exactly as before, so short runs pay nothing.
const RECORD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Writes finished steps to the local metadata DB while a run is still going (#214), so
/// `barca status` in another process sees them and a killed run keeps them.
///
/// The run loop only sends rows down a channel; a background task batches whatever has
/// arrived and writes it with a short-lived connection, at most once per [`RECORD_INTERVAL`].
/// The loop therefore never waits on the database (another barca process may be holding it),
/// and a phase of thousands of quick steps costs a few writes, not thousands.
///
/// It is an optimisation of *when* rows land, never the only writer: the end-of-run
/// [`persist_run`] writes every row of the run that is not already there (rows carry the run
/// id). Failed batches are retained and retried at the existing interval. Rows still pending
/// at [`StepRecorder::finish`] are made good at the end. A pull of shared state by another
/// process mid-run keeps the written rows (see `state_carry`).
const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const CHECKPOINT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Startup's shared token belongs to this context until the recorder is stopped.
pub(crate) struct ProgressPublication {
    pub(crate) python: std::path::PathBuf,
    pub(crate) cfg: crate::config::ResolvedConfig,
    pub(crate) token: state_sync::StateToken,
    pub(crate) cancel: CancellationToken,
}

pub(crate) struct StepRecorder {
    tx: tokio::sync::mpsc::UnboundedSender<StepRow>,
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<Option<state_sync::StateToken>>>,
}

impl StepRecorder {
    #[cfg(test)]
    pub(crate) fn start(db_path: String, run_id: String) -> Self {
        Self::start_with_publication(db_path, run_id, None)
    }

    pub(crate) fn start_with_publication(
        db_path: String,
        run_id: String,
        publication: Option<ProgressPublication>,
    ) -> Self {
        Self::start_with_intervals(
            db_path,
            run_id,
            publication,
            RECORD_INTERVAL,
            CHECKPOINT_INTERVAL,
        )
    }

    // Private timing injection exercises real publication without a public test knob.
    fn start_with_intervals(
        db_path: String,
        run_id: String,
        mut publication: Option<ProgressPublication>,
        record_interval: std::time::Duration,
        checkpoint_interval: std::time::Duration,
    ) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StepRow>();
        let stop = publication
            .as_ref()
            .map_or_else(CancellationToken::new, |p| p.cancel.child_token());
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let mut next_write = tokio::time::Instant::now() + record_interval;
            let mut pending = Vec::new();
            let mut generation = 0u64;
            let mut published = 0u64;
            let mut checkpoint = tokio::time::interval_at(
                tokio::time::Instant::now() + checkpoint_interval,
                checkpoint_interval,
            );
            checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    _ = tokio::time::sleep_until(next_write), if !pending.is_empty() => {
                        while let Ok(row) = rx.try_recv() { pending.push(row); }
                        // A successful no-op replay never marks unchanged state dirty.
                        if let Ok(inserted) = record_steps(&db_path, &run_id, &pending).await {
                            generation += inserted;
                            pending.clear();
                        }
                        next_write = tokio::time::Instant::now() + record_interval;
                    }
                    _ = checkpoint.tick(), if publication.is_some() => {
                        if generation != published {
                            let snapshot_generation = generation;
                            let p = publication.as_mut().expect("publication enabled");
                            let mut push = SharedPush {
                                python: &p.python, cfg: &p.cfg, db_path: &db_path,
                                run_id: &run_id, logs: &[], token: p.token.clone(),
                            };
                            let outcome = push.checkpoint(state_sync::Until {
                                cancel: Some(&stopped),
                                deadline: Some(std::time::Instant::now() + CHECKPOINT_BUDGET),
                            }).await;
                            // Acknowledged uploads remain known even if a later retry fails.
                            p.token = push.token;
                            match outcome {
                                Ok(_) => published = snapshot_generation,
                                Err(error) if !stopped.is_cancelled() => {
                                    let mut message = error.to_string();
                                    if let Some(uri) = &p.cfg.state_uri {
                                        message = message.replace(uri, &crate::transfer::diagnostic_uri(uri));
                                    }
                                    crate::errln!("[barca] progress checkpoint failed; recorded results remain local: {message}");
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    row = rx.recv() => match row {
                        Some(row) => pending.push(row),
                        None => break,
                    },
                }
            }
            publication.map(|p| p.token)
        });
        Self {
            tx,
            stop,
            task: Some(task),
        }
    }

    /// Queue a finished step. Never blocks.
    pub(crate) fn record(&self, row: StepRow) {
        self.tx.send(row).ok();
    }

    /// Prepare a nonblocking receipt handler. Only the transfer owner calls it after
    /// confirming the stored bytes; an enqueued or abandoned upload records nothing.
    pub(crate) fn after_upload(
        &self,
        mut row: StepRow,
        store: String,
    ) -> impl FnOnce(Option<String>) + Send + 'static {
        let tx = self.tx.clone();
        move |hash| {
            row.path = store;
            // Match terminal persistence: a sensor already has its worker content hash.
            // Transfer hashes fill ordinary outputs, never replace that sensor identity.
            if row.output_hash.is_none() {
                row.output_hash = hash;
            }
            tx.send(row).ok();
        }
    }

    /// Stop the background task and wait for it, so no connection is left open. Rows it had
    /// not written yet are left to [`persist_run`].
    #[cfg(test)]
    pub(crate) async fn finish(self) {
        self.finish_with_token(&mut None).await.unwrap();
    }

    pub(crate) async fn finish_with_token(
        mut self,
        token: &mut Option<state_sync::StateToken>,
    ) -> Result<(), BarcaError> {
        self.stop.cancel();
        let returned = self
            .task
            .take()
            .expect("recorder task owned")
            .await
            .map_err(|e| {
                BarcaError::Other(format!("progress recorder stopped unexpectedly: {e}"))
            })?;
        if let Some(returned) = returned {
            *token = Some(returned);
        }
        Ok(())
    }
}

impl Drop for StepRecorder {
    fn drop(&mut self) {
        // Cancellation lets an in-flight state helper clean its staged snapshot.
        self.stop.cancel();
    }
}

/// Append `rows` for a run that is still in progress, and advance its `steps_executed` so
/// `barca history` shows how far it has got.
async fn record_steps(db_path: &str, run_id: &str, rows: &[StepRow]) -> Result<u64, BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;
    conn.execute("BEGIN", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to begin progress batch: {e}")))?;
    let result = async {
        let mut written = 0u64;
        for row in rows {
            written += row.insert(&conn, run_id).await.map_err(|e| {
                BarcaError::Db(format!("failed to record progress step: {e}"))
            })?;
        }
        let updated = conn.execute(
            "UPDATE runs SET steps_executed = steps_executed + ?1 WHERE run_id = ?2 AND status = 'running'",
            [written.to_string(), run_id.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to update progress count: {e}")))?;
        if written > 0 && updated != 1 {
            return Err(BarcaError::Db("cannot record new progress for a missing or finished run".to_string()));
        }
        conn.execute("COMMIT", ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to commit progress batch: {e}")))?;
        Ok(written)
    }.await;
    if result.is_err() {
        // Rollback cannot conceal the original failure. Dropping this short-lived
        // connection also closes any transaction if rollback itself fails.
        conn.execute("ROLLBACK", ()).await.ok();
    }
    result
}

/// The exception a step failure carries, as (type, message, traceback). A worker reports a
/// Python exception as the generic `WorkerError` whose text is `Type: message` followed by the
/// traceback frames; the type is what groups errors in a telemetry backend.
fn exception_of(error: &dispatch::StepError) -> (String, String, Option<String>) {
    // The frames are the trailing run of `  File "..."` lines and their indented source lines.
    // Taking only that run keeps a message that itself quotes a traceback in one piece.
    let lines: Vec<&str> = error.message.split('\n').collect();
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
        (error.message.as_str(), None)
    };
    let stack = Some(error.traceback.clone())
        .filter(|t| !t.is_empty())
        .or(frames);
    if error.error_type == "WorkerError"
        && let Some((head, rest)) = text.split_once(": ")
        && !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return (head.to_string(), rest.to_string(), stack);
    }
    (error.error_type.clone(), text.to_string(), stack)
}

/// Group resolved ids consistently in APM. The run ledger retains the
/// original target spelling for CLI/history compatibility.
pub(crate) fn canonical_job(target_ids: &[&str]) -> String {
    let mut ids = target_ids.to_vec();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        "all".to_string()
    } else {
        ids.join(",")
    }
}

/// The run as telemetry integrations see it: every step that ran, was served from cache, or
/// failed. A step that ran is placed by the worker's own clock; a cached step is a zero-length
/// mark at the start of the run and a failed one at its end, since neither reports a time.
pub(crate) fn telemetry_report(
    l: &RunLedger<'_>,
    dag: &Dag,
    started: std::time::SystemTime,
    clocks: &HashMap<String, (f64, f64)>,
    job: &str,
) -> crate::telemetry::RunReport {
    use crate::telemetry::{RunReport, StepOutcome, StepReport};

    let ns = |seconds: f64| (seconds.max(0.0) * 1e9) as u64;
    let start_unix_ns = started
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let duration_ns = ns(l.elapsed);
    let kind = |node_id: &str| match dag
        .get_node(crate::StepId::parse(node_id).base_id())
        .map(|n| n.kind())
    {
        Some(crate::NodeKind::Task) => "task",
        Some(crate::NodeKind::Sensor) => "sensor",
        Some(crate::NodeKind::Asset) | None => "asset",
    };
    // Attempts are counted per step, not per partition key, so they are only attributed to
    // an unpartitioned step.
    let attempts = |node_id: &str| {
        let id = crate::StepId::parse(node_id);
        (id.display() == id.base_id())
            .then(|| l.all_attempts.get(id.base_id()).copied().unwrap_or(1))
    };

    let mut steps: Vec<StepReport> = Vec::new();
    for (node_id, oref) in l.all_outputs {
        let cached = l.cached_node_ids.contains(node_id);
        let (step_start, step_duration) = match clocks.get(node_id) {
            _ if cached => (start_unix_ns, 0),
            Some((finished, wall)) => (ns(finished - wall), ns(*wall)),
            None => (start_unix_ns, ns(oref.elapsed_seconds.unwrap_or(0.0))),
        };
        let (cpu_seconds, max_rss_bytes) =
            l.all_timings.get(node_id).copied().unwrap_or((None, None));
        steps.push(StepReport {
            node_id: node_id.clone(),
            kind: kind(node_id),
            outcome: if cached {
                StepOutcome::Cached
            } else {
                StepOutcome::Ran
            },
            start_unix_ns: step_start,
            duration_ns: step_duration,
            attempts: if cached { None } else { attempts(node_id) },
            run_hash: l.run_hashes.get(node_id).cloned(),
            size_bytes: Some(oref.size_bytes),
            cpu_seconds,
            max_rss_bytes,
            error_type: None,
            error_message: None,
            error_traceback: None,
        });
    }
    for failure in l.all_failures {
        let (error_type, error_message, error_traceback) = exception_of(&failure.error);
        steps.push(StepReport {
            node_id: failure.node_id.clone(),
            kind: kind(&failure.node_id),
            outcome: StepOutcome::Failed,
            start_unix_ns: start_unix_ns + duration_ns,
            duration_ns: 0,
            attempts: Some(failure.error.attempts),
            run_hash: l.run_hashes.get(&failure.node_id).cloned(),
            size_bytes: None,
            cpu_seconds: None,
            max_rss_bytes: None,
            error_type: Some(error_type),
            error_message: Some(error_message),
            error_traceback,
        });
    }
    steps.sort_by(|a, b| (a.start_unix_ns, &a.node_id).cmp(&(b.start_unix_ns, &b.node_id)));

    RunReport {
        run_id: l.run_id.to_string(),
        command: l.command.to_string(),
        target: l.target.map(str::to_string),
        job: job.to_string(),
        status: l.status.to_string(),
        start_unix_ns,
        duration_ns,
        steps_total: l.steps_total,
        steps_executed: l.steps_executed,
        steps_cached: l.steps_cached,
        steps,
    }
}

/// Write a run's ledger with a short-lived connection. Idempotent, so it is safe after the
/// [`StepRecorder`] has written some of the steps and on a replay: the run row is INSERT OR
/// IGNORE + terminal UPDATE, and a step is appended only if this run has no row for it yet
/// (a step has one outcome per run).
pub(crate) async fn persist_run(db_path: &str, l: &RunLedger<'_>) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;

    conn.execute("BEGIN", ())
        .await
        .map_err(|e| BarcaError::Db(format!("failed to begin terminal ledger: {e}")))?;
    let result = async {
        write_terminal_ledger(&conn, l).await?;
        conn.execute("COMMIT", ())
            .await
            .map_err(|e| BarcaError::Db(format!("failed to commit terminal ledger: {e}")))?;
        Ok::<(), BarcaError>(())
    }
    .await;
    if let Err(error) = result {
        conn.execute("ROLLBACK", ()).await.ok();
        return Err(error);
    }
    // Only a committed outcome replaces the process-liveness witness.
    crate::run_owner::release(db_path, l.run_id);

    // Persist the measured-cost EWMA so the next run starts pre-warmed and
    // skips the cold-start probe entirely. (Inline — this fn already holds
    // the process-wide DB guard.)
    for (node_id, est) in l.cost_snapshot {
        let base = crate::StepId::parse(node_id).base_id().to_string();
        conn.execute(
            "INSERT INTO cost_estimates (node_id, base_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(node_id) DO UPDATE SET
                 estimate_seconds = ?3, cpu_seconds = ?4, max_rss_bytes = ?5,
                 samples = ?6, updated_at = datetime('now')",
            [
                node_id.clone(),
                base,
                est.estimate_seconds.to_string(),
                est.cpu_seconds.to_string(),
                est.max_rss_bytes.to_string(),
                est.samples.to_string(),
            ],
        )
        .await
        .ok();
    }

    Ok(())
}

/// Required outcome rows and terminal status, inside the caller's transaction.
async fn write_terminal_ledger(
    conn: &turso::Connection,
    l: &RunLedger<'_>,
) -> Result<(), BarcaError> {
    conn.execute(
            "INSERT OR IGNORE INTO runs (run_id, command, files, target, status, steps_total, pid, host) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7)",
            [
                l.run_id.to_string(),
                l.command.to_string(),
                l.files.clone(),
                l.target.unwrap_or("").to_string(),
                l.steps_total.to_string(),
                std::process::id().to_string(),
                db::local_host(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to create terminal run: {e}")))?;
    // What the [`StepRecorder`] wrote during the run, or, on a replay after a shared-state
    // conflict, what the pulled database holds of this run.
    let already = crate::state_carry::steps_of_run(conn, l.run_id).await?;

    for (node_id, oref) in l.all_outputs {
        if l.cached_node_ids.contains(node_id) || already.contains(node_id) {
            continue;
        }
        let Some(run_h) = l.run_hashes.get(node_id) else {
            continue;
        };
        let base = crate::StepId::parse(node_id).base_id().to_string();
        let (cpu, rss) = l.all_timings.get(node_id).copied().unwrap_or((None, None));
        let mut row = StepRow::from_artifact(node_id, run_h, &serde_json::Value::Null, 1);
        row.path = l.store_paths.get(node_id).unwrap_or(&oref.path).clone();
        row.format = oref.format.clone();
        row.size_bytes = oref.size_bytes;
        row.elapsed_seconds = oref.elapsed_seconds;
        row.attempts = l.all_attempts.get(&base).copied().unwrap_or(1);
        row.sinks_json = l.all_sinks.get(node_id).cloned();
        row.cpu_seconds = cpu;
        row.max_rss_bytes = rss;
        row.output_hash = l
            .output_hashes
            .get(node_id)
            .or(oref.content_hash.as_ref())
            .cloned();
        row.insert(conn, l.run_id)
            .await
            .map_err(|e| BarcaError::Db(format!("failed to record terminal step: {e}")))?;
    }

    // Persist permanently-failed steps as `status='failed'` rows (artifact
    // columns NULL). Failed rows are never served as cache hits.
    for failure in l.all_failures {
        let node_id = &failure.node_id;
        if already.contains(node_id) {
            continue;
        }
        let run_h = l.run_hashes.get(node_id).cloned().unwrap_or_default();
        // Each failure carries its own attempt count: dispatches for a worker
        // failure, transfer attempts for an upload failure.
        conn.execute(
                "INSERT INTO materializations (node_id, run_hash, status, error_type, error_message, error_traceback, attempts, run_id) VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6, ?7)",
                [
                    node_id.clone(),
                    run_h,
                    failure.error.error_type.clone(),
                    failure.error.message.clone(),
                    failure.error.traceback.clone(),
                    failure.error.attempts.to_string(),
                    l.run_id.to_string(),
                ],
            )
            .await
            .map_err(|e| BarcaError::Db(format!("failed to record failed step: {e}")))?;
    }
    let updated = conn.execute(
            "UPDATE runs SET status = ?1, steps_executed = ?2, steps_cached = ?3, elapsed_seconds = ?4, finished_at = datetime('now') WHERE run_id = ?5",
            [
                l.status.to_string(),
                l.steps_executed.to_string(),
                l.steps_cached.to_string(),
                l.elapsed.to_string(),
                l.run_id.to_string(),
            ],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to finish run: {e}")))?;
    if updated != 1 {
        return Err(BarcaError::Db(
            "terminal run row was not written".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod persist_tests {
    use super::*;
    use crate::dispatch::OutputRef;
    use std::collections::HashSet;
    use std::time::Instant;

    /// A run that executed `a`, `part[k=1]` and `part[k=2]`, found `cached` in the cache, and
    /// saw `bad` fail.
    struct Fixture {
        outputs: HashMap<String, OutputRef>,
        failures: Vec<dispatch::StepFailure>,
        cached: HashSet<String>,
        run_hashes: HashMap<String, String>,
        empty: HashMap<String, String>,
        attempts: HashMap<String, u32>,
        timings: HashMap<String, (Option<f64>, Option<u64>)>,
    }

    const EXECUTED: [&str; 3] = ["f.py:a", "f.py:part[k=1]", "f.py:part[k=2]"];

    fn oref(node: &str) -> OutputRef {
        OutputRef {
            path: format!(".barca/artifacts/{node}/h.json"),
            format: "json".to_string(),
            size_bytes: 2,
            elapsed_seconds: Some(0.5),
            content_hash: None,
        }
    }

    impl Fixture {
        fn new() -> Self {
            let mut outputs = HashMap::new();
            let mut run_hashes = HashMap::new();
            for node in EXECUTED.iter().chain(&["f.py:cached", "f.py:bad"]) {
                run_hashes.insert(node.to_string(), format!("hash-{node}"));
                if *node != "f.py:bad" {
                    outputs.insert(node.to_string(), oref(node));
                }
            }
            Self {
                outputs,
                failures: vec![dispatch::StepFailure {
                    node_id: "f.py:bad".to_string(),
                    error: dispatch::StepError {
                        error_type: "WorkerError".to_string(),
                        message: "boom".to_string(),
                        traceback: String::new(),
                        attempts: 1,
                    },
                }],
                cached: HashSet::from(["f.py:cached".to_string()]),
                run_hashes,
                empty: HashMap::new(),
                attempts: HashMap::new(),
                timings: HashMap::new(),
            }
        }

        fn ledger<'a>(&'a self, run_id: &'a str) -> RunLedger<'a> {
            RunLedger {
                run_id,
                status: "failed",
                command: "get",
                files: db::encode_files(&["f.py".to_string()]),
                target: None,
                steps_total: 5,
                steps_executed: 4,
                steps_cached: 1,
                elapsed: 1.5,
                all_outputs: &self.outputs,
                all_failures: &self.failures,
                all_sinks: &self.empty,
                all_attempts: &self.attempts,
                all_timings: &self.timings,
                cached_node_ids: &self.cached,
                run_hashes: &self.run_hashes,
                output_hashes: &self.empty,
                store_paths: &self.empty,
                cost_snapshot: &[],
            }
        }

        /// The row the recorder would write for `node` when it finishes.
        fn row(&self, node: &str) -> StepRow {
            let artifact = serde_json::json!({
                "path": self.outputs[node].path, "format": "json", "size_bytes": 2,
                "elapsed_seconds": 0.5,
            });
            StepRow::from_artifact(node, &self.run_hashes[node], &artifact, 1)
        }
    }

    async fn fresh_db(dir: &tempfile::TempDir, name: &str) -> String {
        let db_path = dir.path().join(name).to_string_lossy().to_string();
        db::init_db(&db_path).await.unwrap();
        db_path
    }

    /// Every materialization row as `(run_id, node_id, status)`, sorted.
    async fn rows(db_path: &str) -> Vec<(String, String, String)> {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await.unwrap();
        let mut found = conn
            .query(
                "SELECT COALESCE(run_id, ''), node_id, status FROM materializations",
                (),
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(row) = found.next().await.unwrap() {
            out.push((
                row.get::<String>(0).unwrap(),
                row.get::<String>(1).unwrap(),
                row.get::<String>(2).unwrap(),
            ));
        }
        out.sort();
        out
    }

    /// What one complete write of the fixture's run looks like: each executed step once, the
    /// failure once, and nothing for the cache hit.
    fn complete(run_id: &str) -> Vec<(String, String, String)> {
        let mut want: Vec<_> = EXECUTED
            .iter()
            .map(|n| (run_id.to_string(), n.to_string(), "success".to_string()))
            .collect();
        want.push((
            run_id.to_string(),
            "f.py:bad".to_string(),
            "failed".to_string(),
        ));
        want.sort();
        want
    }

    async fn run_record(db_path: &str, run_id: &str) -> db::RunRecord {
        db::get_recent_runs(db_path, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.run_id == run_id)
            .expect("run row")
    }

    async fn owner_marker(db_path: &str, run_id: &str) -> Option<std::path::PathBuf> {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await.unwrap();
        let mut rows = conn
            .query("SELECT owner FROM runs WHERE run_id = ?1", [run_id])
            .await
            .unwrap();
        let owner = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        crate::run_owner::token_of(&owner).map(|token| {
            std::path::Path::new(db_path)
                .parent()
                .unwrap()
                .join("run-owners")
                .join(format!("{token}.fifo"))
        })
    }

    #[tokio::test]
    async fn cancellation_status_write_errors_retain_the_owner_and_previous_status() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        db::create_run(&db_path, "earlier", "get", "[]", None, Some(1))
            .await
            .unwrap();
        db::finish_run(&db_path, "earlier", "cancelled", 0, 0, 0.1)
            .await
            .unwrap();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(1))
            .await
            .unwrap();
        let marker = owner_marker(&db_path, "r1").await;
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("CREATE UNIQUE INDEX status_fault ON runs(status)", ())
                .await
                .unwrap();
        }
        assert!(
            db::finish_run(&db_path, "r1", "cancelled", 0, 0, 0.1)
                .await
                .is_err()
        );
        assert_eq!(run_record(&db_path, "r1").await.status, "running");
        if let Some(marker) = &marker {
            assert!(marker.exists());
        }
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("DROP INDEX status_fault", ()).await.unwrap();
        }
        db::finish_run(&db_path, "r1", "cancelled", 0, 0, 0.1)
            .await
            .unwrap();
        assert_eq!(run_record(&db_path, "r1").await.status, "cancelled");
        if let Some(marker) = &marker {
            assert!(!marker.exists());
        }
        assert!(
            db::finish_run(&db_path, "missing", "cancelled", 0, 0, 0.1)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_failed_success_row_cannot_finalize_a_partial_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(5))
            .await
            .unwrap();
        db::create_run(&db_path, "earlier", "get", "[]", None, Some(1))
            .await
            .unwrap();
        record_steps(&db_path, "earlier", &[fx.row("f.py:a")])
            .await
            .unwrap();
        db::finish_run(&db_path, "earlier", "success", 1, 0, 0.1)
            .await
            .unwrap();
        let marker = owner_marker(&db_path, "r1").await;
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute(
                "CREATE UNIQUE INDEX unique_node_fault ON materializations(node_id)",
                (),
            )
            .await
            .unwrap();
        }
        assert!(persist_run(&db_path, &fx.ledger("r1")).await.is_err());
        if let Some(marker) = &marker {
            assert!(marker.exists());
        }
        assert_eq!(
            rows(&db_path).await,
            [(
                "earlier".to_string(),
                "f.py:a".to_string(),
                "success".to_string()
            )]
        );
        let mid = run_record(&db_path, "r1").await;
        assert_eq!(
            (mid.status.as_str(), mid.steps_executed, mid.finished_at),
            ("running", 0, None)
        );
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("DROP INDEX unique_node_fault", ())
                .await
                .unwrap();
        }
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await.len(), complete("r1").len() + 1);
        assert_eq!(run_record(&db_path, "r1").await.status, "failed");
        if let Some(marker) = &marker {
            assert!(!marker.exists());
        }
    }

    #[tokio::test]
    async fn a_failed_failure_row_rolls_back_new_success_rows_and_terminal_status() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        persist_run(&db_path, &fx.ledger("earlier")).await.unwrap();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(5))
            .await
            .unwrap();
        // Already acknowledged progress survives terminal rollback.
        record_steps(&db_path, "r1", &[fx.row("f.py:a")])
            .await
            .unwrap();
        let before = rows(&db_path).await;
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("CREATE UNIQUE INDEX unique_failure_fault ON materializations(status) WHERE status = 'failed'", ()).await.unwrap();
        }
        assert!(persist_run(&db_path, &fx.ledger("r1")).await.is_err());
        assert_eq!(rows(&db_path).await, before);
        let mid = run_record(&db_path, "r1").await;
        assert_eq!(
            (mid.status.as_str(), mid.steps_executed, mid.finished_at),
            ("running", 1, None)
        );
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("DROP INDEX unique_failure_fault", ())
                .await
                .unwrap();
        }
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await.len(), 2 * complete("r1").len());
    }

    #[tokio::test]
    async fn upload_receipts_preserve_worker_hashes_and_fill_ordinary_outputs() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let recorder = StepRecorder {
            tx,
            stop: CancellationToken::new(),
            task: Some(tokio::spawn(async { None })),
        };
        let fx = Fixture::new();
        let mut sensor = fx.row("f.py:a");
        sensor.output_hash = Some("worker-sensor-hash".to_string());
        recorder.after_upload(sensor, "s3://store/sensor".to_string())(Some(
            "later-local-file-hash".to_string(),
        ));
        let saved = rx.recv().await.unwrap();
        assert_eq!(saved.path, "s3://store/sensor");
        assert_eq!(saved.output_hash.as_deref(), Some("worker-sensor-hash"));

        recorder.after_upload(fx.row("f.py:a"), "s3://store/asset".to_string())(Some(
            "confirmed-asset-hash".to_string(),
        ));
        let saved = rx.recv().await.unwrap();
        assert_eq!(saved.path, "s3://store/asset");
        assert_eq!(saved.output_hash.as_deref(), Some("confirmed-asset-hash"));
        recorder.finish().await;
    }

    /// Exercise the actual stdlib directory backend in a child with its own
    /// source path. The gate holds a completed upload before acknowledgement.
    fn state_test_config(
        dir: &tempfile::TempDir,
        gate: bool,
        lose_ack: bool,
    ) -> (crate::config::ResolvedConfig, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../python")
            .canonicalize()
            .unwrap();
        let root = dir.path();
        let runner = root.join("state_runner.py");
        std::fs::write(
            &runner,
            format!(
                r#"
import os, signal, sys, time
from pathlib import Path
sys.path.insert(0, {source:?})
from barca import _state
root = Path({root:?})
original = _state.push
def push(uri, local, token):
    with (root / 'attempts').open('a') as f:
        f.write(str(time.monotonic()) + '\n')
    if (root / 'outage').exists():
        raise OSError('test state store unavailable')
    result = original(uri, local, token)
    with (root / 'uploads').open('a') as f:
        f.write(result + '\n')
    if {gate} and not (root / 'release').exists():
        (root / 'uploaded').touch()
        deadline = time.monotonic() + 15
        while not (root / 'release').exists():
            if time.monotonic() > deadline:
                raise TimeoutError('test upload gate expired')
            time.sleep(.01)
    if {lose_ack} and not (root / 'ack_lost').exists():
        (root / 'ack_lost').touch()
        sys.stdout.write('{{invalid acknowledgement')
        sys.stdout.flush()
        os._exit(0)
    return result
_state.push = push
original_pull = _state.pull
def pull(uri, local):
    result = original_pull(uri, local)
    if (root / 'fail_pull').exists():
        raise OSError('test interruption after real download')
    return result
_state.pull = pull
sys.argv = sys.argv[2:]
from barca import _lifeline
signal.signal(signal.SIGTERM, _state._stop)
_lifeline.watch()
sys.exit(_state.main())
"#,
                source = source.to_string_lossy(),
                root = root.to_string_lossy(),
                gate = if gate { "True" } else { "False" },
                lose_ack = if lose_ack { "True" } else { "False" }
            ),
        )
        .unwrap();
        let python = root.join("python");
        std::fs::write(
            &python,
            format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", runner.display()),
        )
        .unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = crate::config::resolve_in(None, root).unwrap();
        cfg.db_path = root.join("local.db").to_string_lossy().into_owned();
        cfg.state_uri = Some(root.join("shared.db").to_string_lossy().into_owned());
        cfg.state = crate::config::StateMode::Optimistic;
        cfg.push_retries = 3;
        (cfg, python)
    }

    async fn wait_for_state_file(path: &std::path::Path) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual state helper did not reach gate");
    }

    #[tokio::test]
    async fn checkpoint_retries_a_lost_real_upload_ack_and_keeps_unrelated_history() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, python) = state_test_config(&dir, false, true);
        db::init_db(&cfg.db_path).await.unwrap();
        db::create_run(&cfg.db_path, "r1", "get", "[]", None, Some(2))
            .await
            .unwrap();
        let fx = Fixture::new();
        let mut first = fx.row("f.py:a");
        first.path = super::super::state_carry::testing::artifact(&dir, "a.json");
        record_steps(&cfg.db_path, "r1", &[first.clone()])
            .await
            .unwrap();
        let mut push = SharedPush {
            python: &python,
            cfg: &cfg,
            db_path: &cfg.db_path,
            run_id: "r1",
            logs: &[],
            token: state_sync::StateToken(None),
        };
        assert!(push.checkpoint(state_sync::Until::done()).await.is_err());
        assert!(
            dir.path().join("shared.db").exists(),
            "bytes really reached the store"
        );
        assert!(push.token.0.is_none(), "an unacknowledged write is unknown");
        let shared = cfg.state_uri.as_ref().unwrap();
        db::create_run(shared, "other", "get", "[]", None, Some(1))
            .await
            .unwrap();
        record_steps(shared, "other", &[first]).await.unwrap();
        db::finish_run(shared, "other", "success", 1, 0, 0.5)
            .await
            .unwrap();
        state_sync::checkpoint_truncate(shared).await.unwrap();
        let mut second = fx.row("f.py:part[k=1]");
        second.path = super::super::state_carry::testing::artifact(&dir, "b.json");
        record_steps(&cfg.db_path, "r1", &[second]).await.unwrap();
        assert!(push.checkpoint(state_sync::Until::done()).await.unwrap() >= 1);
        assert!(push.token.0.is_some());
        assert_eq!(rows(shared).await.len(), 3);
        let active = run_record(shared, "r1").await;
        assert_eq!(
            (
                active.status.as_str(),
                active.steps_executed,
                active.finished_at
            ),
            ("running", 2, None)
        );
        assert_eq!(run_record(shared, "other").await.status, "success");
        assert_eq!(rows(&cfg.db_path).await, rows(shared).await);
    }

    #[tokio::test]
    async fn outage_keeps_committed_progress_dirty_until_a_later_tick_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, python) = state_test_config(&dir, false, false);
        db::init_db(&cfg.db_path).await.unwrap();
        db::create_run(&cfg.db_path, "r1", "get", "[]", None, Some(1))
            .await
            .unwrap();
        std::fs::write(dir.path().join("outage"), b"").unwrap();
        let recorder = StepRecorder::start_with_intervals(
            cfg.db_path.clone(),
            "r1".into(),
            Some(ProgressPublication {
                python,
                cfg: cfg.clone(),
                token: state_sync::StateToken(None),
                cancel: CancellationToken::new(),
            }),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(100),
        );
        let mut first = Fixture::new().row("f.py:a");
        first.path = super::super::state_carry::testing::artifact(&dir, "a.json");
        recorder.record(first);
        wait_for_state_file(&dir.path().join("attempts")).await;
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        let attempts: Vec<f64> = std::fs::read_to_string(dir.path().join("attempts"))
            .unwrap()
            .lines()
            .map(|line| line.parse().unwrap())
            .collect();
        assert!(
            attempts.len() >= 2,
            "dirty progress retries without a new row"
        );
        assert!(
            attempts.windows(2).all(|pair| pair[1] - pair[0] >= 0.05),
            "failure must not cause busy retries: {attempts:?}"
        );
        assert!(!dir.path().join("shared.db").exists());
        assert_eq!(run_record(&cfg.db_path, "r1").await.steps_executed, 1);
        std::fs::remove_file(dir.path().join("outage")).unwrap();
        wait_for_state_file(&dir.path().join("uploads")).await;
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        let mut token = None;
        recorder.finish_with_token(&mut token).await.unwrap();
        assert!(token.unwrap().0.is_some());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("uploads"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let recovered = run_record(cfg.state_uri.as_ref().unwrap(), "r1").await;
        assert_eq!(
            (
                recovered.status.as_str(),
                recovered.steps_executed,
                recovered.finished_at
            ),
            ("running", 1, None)
        );
    }

    #[tokio::test]
    async fn checkpoint_retains_an_acknowledged_token_when_followup_pull_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, python) = state_test_config(&dir, true, false);
        db::init_db(&cfg.db_path).await.unwrap();
        db::create_run(&cfg.db_path, "r1", "get", "[]", None, Some(2))
            .await
            .unwrap();
        let fx = Fixture::new();
        let mut first = fx.row("f.py:a");
        first.path = super::super::state_carry::testing::artifact(&dir, "a.json");
        record_steps(&cfg.db_path, "r1", &[first]).await.unwrap();
        let mut push = SharedPush {
            python: &python,
            cfg: &cfg,
            db_path: &cfg.db_path,
            run_id: "r1",
            logs: &[],
            token: state_sync::StateToken(None),
        };
        let (outcome, ()) = tokio::join!(push.checkpoint(state_sync::Until::done()), async {
            wait_for_state_file(&dir.path().join("uploaded")).await;
            let mut second = fx.row("f.py:part[k=1]");
            second.path = super::super::state_carry::testing::artifact(&dir, "b.json");
            record_steps(&cfg.db_path, "r1", &[second]).await.unwrap();
            std::fs::write(dir.path().join("fail_pull"), b"").unwrap();
            std::fs::write(dir.path().join("release"), b"").unwrap();
        });
        assert!(outcome.is_err());
        let receipts = std::fs::read_to_string(dir.path().join("uploads")).unwrap();
        assert_eq!(
            push.token.0.as_deref(),
            receipts.lines().last(),
            "keep the acknowledged CAS token despite the later failure"
        );
        assert_eq!(
            rows(&cfg.db_path).await.len(),
            2,
            "failed pull preserves local rows"
        );
        assert_eq!(run_record(&cfg.db_path, "r1").await.finished_at, None);
        assert!(
            std::fs::read_dir(dir.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".pull-")
            }),
            "failed real download leaves no coordinator stage"
        );
        std::fs::remove_file(dir.path().join("fail_pull")).unwrap();
        push.checkpoint(state_sync::Until::done()).await.unwrap();
        let shared = cfg.state_uri.as_ref().unwrap();
        assert_eq!(run_record(shared, "r1").await.steps_executed, 2);
        assert_eq!(rows(shared).await.len(), 2);
    }

    #[tokio::test]
    async fn queued_progress_is_published_after_one_blocked_upload_without_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, python) = state_test_config(&dir, true, false);
        db::init_db(&cfg.db_path).await.unwrap();
        db::create_run(&cfg.db_path, "r1", "get", "[]", None, Some(2))
            .await
            .unwrap();
        let recorder = StepRecorder::start_with_intervals(
            cfg.db_path.clone(),
            "r1".into(),
            Some(ProgressPublication {
                python,
                cfg: cfg.clone(),
                token: state_sync::StateToken(None),
                cancel: CancellationToken::new(),
            }),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(100),
        );
        let fx = Fixture::new();
        let mut first = fx.row("f.py:a");
        first.path = super::super::state_carry::testing::artifact(&dir, "a.json");
        recorder.record(first);
        wait_for_state_file(&dir.path().join("uploaded")).await;
        let mut second = fx.row("f.py:part[k=1]");
        second.path = super::super::state_carry::testing::artifact(&dir, "b.json");
        recorder.record(second);
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        let uploads = dir.path().join("uploads");
        assert_eq!(
            std::fs::read_to_string(&uploads).unwrap().lines().count(),
            1
        );
        assert_eq!(
            rows(&cfg.db_path).await.len(),
            1,
            "queued rows are not falsely committed/published"
        );
        std::fs::write(dir.path().join("release"), b"").unwrap();
        let shared = cfg.state_uri.as_ref().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if run_record(shared, "r1").await.steps_executed == 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        assert_eq!(
            std::fs::read_to_string(&uploads).unwrap().lines().count(),
            2,
            "missed/clean ticks must not cause extra uploads"
        );
        let mut token = None;
        recorder.finish_with_token(&mut token).await.unwrap();
        assert!(token.unwrap().0.is_some());
        assert_eq!(rows(shared).await.len(), 2);
        assert_eq!(run_record(shared, "r1").await.finished_at, None);
    }

    #[tokio::test]
    async fn progress_batch_replay_keeps_one_row_and_one_count_per_node() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(2))
            .await
            .unwrap();
        let batch = [fx.row("f.py:a"), fx.row("f.py:a"), fx.row("f.py:part[k=1]")];
        record_steps(&db_path, "r1", &batch).await.unwrap();
        record_steps(&db_path, "r1", &batch).await.unwrap();
        assert_eq!(rows(&db_path).await.len(), 2);
        assert_eq!(run_record(&db_path, "r1").await.steps_executed, 2);
    }

    #[tokio::test]
    async fn a_failed_insert_rolls_back_the_entire_progress_batch() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(2))
            .await
            .unwrap();
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute(
                "CREATE UNIQUE INDEX unique_progress_path ON materializations(artifact_path)",
                (),
            )
            .await
            .unwrap();
        }
        let first = fx.row("f.py:a");
        let mut second = fx.row("f.py:part[k=1]");
        second.path = first.path.clone();
        let batch = [first, second];
        assert!(record_steps(&db_path, "r1", &batch).await.is_err());
        assert!(rows(&db_path).await.is_empty());
        assert_eq!(run_record(&db_path, "r1").await.steps_executed, 0);
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("DROP INDEX unique_progress_path", ())
                .await
                .unwrap();
        }
        record_steps(&db_path, "r1", &batch).await.unwrap();
        assert_eq!(rows(&db_path).await.len(), 2);
        assert_eq!(run_record(&db_path, "r1").await.steps_executed, 2);
    }

    #[tokio::test]
    async fn a_failed_counter_update_rolls_back_progress_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        for id in ["r1", "other"] {
            db::create_run(&db_path, id, "get", "[]", None, Some(2))
                .await
                .unwrap();
        }
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute(
                "UPDATE runs SET steps_executed = 1 WHERE run_id = 'other'",
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "CREATE UNIQUE INDEX unique_progress_count ON runs(steps_executed)",
                (),
            )
            .await
            .unwrap();
        }
        let batch = [fx.row("f.py:a")];
        let error = record_steps(&db_path, "r1", &batch).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to update progress count")
        );
        assert!(rows(&db_path).await.is_empty());
        assert_eq!(run_record(&db_path, "r1").await.steps_executed, 0);
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("DROP INDEX unique_progress_count", ())
                .await
                .unwrap();
        }
        assert_eq!(record_steps(&db_path, "r1", &batch).await.unwrap(), 1);
        assert_eq!(run_record(&db_path, "other").await.steps_executed, 1);
    }

    #[tokio::test]
    async fn new_progress_requires_a_running_run() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        assert!(
            record_steps(&db_path, "absent", &[fx.row("f.py:a")])
                .await
                .is_err()
        );
        assert!(rows(&db_path).await.is_empty());
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        let before = rows(&db_path).await;
        let mut late = fx.row("f.py:a");
        late.node_id = "f.py:late".to_string();
        assert!(record_steps(&db_path, "r1", &[late]).await.is_err());
        assert_eq!(rows(&db_path).await, before);
        assert_eq!(run_record(&db_path, "r1").await.status, "failed");
    }

    #[tokio::test]
    async fn failed_progress_is_retried_without_another_worker_result() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(2))
            .await
            .unwrap();
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            // Any new row fails while this constraint is present.
            conn.execute(
                "CREATE UNIQUE INDEX blocked_progress ON materializations((1))",
                (),
            )
            .await
            .unwrap();
            fx.row("f.py:part[k=1]")
                .insert(&conn, "blocker")
                .await
                .unwrap();
        }
        let recorder = StepRecorder::start(db_path.clone(), "r1".to_string());
        recorder.record(fx.row("f.py:a"));
        tokio::time::sleep(RECORD_INTERVAL * 3).await;
        assert_eq!(run_record(&db_path, "r1").await.steps_executed, 0);
        {
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            conn.execute("DROP INDEX blocked_progress", ())
                .await
                .unwrap();
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while run_record(&db_path, "r1").await.steps_executed != 1 {
            assert!(
                Instant::now() < deadline,
                "failed batch was discarded instead of retried"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        recorder.finish().await;
        assert_eq!(rows(&db_path).await.len(), 2); // unrelated blocker remains intact
    }

    #[tokio::test]
    async fn aborted_recorder_keeps_known_token_for_terminal_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let recorder = StepRecorder::start(db_path.clone(), "r1".into());
        recorder.task.as_ref().unwrap().abort();
        let mut token = Some(state_sync::StateToken(Some(
            "known-before-checkpoint".into(),
        )));
        assert!(recorder.finish_with_token(&mut token).await.is_err());
        assert_eq!(token.unwrap().0.as_deref(), Some("known-before-checkpoint"));
        // Mid-run failure must not remove the complete terminal writer's fallback.
        let fx = Fixture::new();
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
        assert_eq!(run_record(&db_path, "r1").await.status, "failed");
    }

    #[tokio::test]
    async fn the_ledger_adds_only_what_the_recorder_has_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        db::create_run(&db_path, "r1", "get", "[\"f.py\"]", None, Some(5))
            .await
            .unwrap();

        // Mid-run: two steps recorded; the run is `running` and counts them.
        record_steps(
            &db_path,
            "r1",
            &[fx.row("f.py:a"), fx.row("f.py:part[k=1]")],
        )
        .await
        .unwrap();
        assert_eq!(rows(&db_path).await.len(), 2);
        let mid = run_record(&db_path, "r1").await;
        assert_eq!((mid.status.as_str(), mid.steps_executed), ("running", 2));
        assert_eq!(mid.finished_at, None);

        // End of run: the rest is added, nothing twice, and the counts are the final ones.
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
        let end = run_record(&db_path, "r1").await;
        assert_eq!(
            (end.status.as_str(), end.steps_executed, end.steps_cached),
            ("failed", 4, 1)
        );

        // Writing the ledger again (a replay onto a database that already has it) is a no-op.
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
    }

    #[tokio::test]
    async fn a_replay_onto_a_freshly_pulled_db_carries_the_whole_run() {
        let dir = tempfile::tempdir().unwrap();
        let fx = Fixture::new();

        // The local DB, with steps recorded mid-run, is replaced by a pulled one that has
        // never heard of this run: the replay must not assume the recorder's rows survived.
        let local = fresh_db(&dir, "local.db").await;
        db::create_run(&local, "r1", "get", "[\"f.py\"]", None, Some(5))
            .await
            .unwrap();
        record_steps(&local, "r1", &[fx.row("f.py:a")])
            .await
            .unwrap();
        let pulled = fresh_db(&dir, "pulled.db").await;
        persist_run(&pulled, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&pulled).await, complete("r1"));
        let run = run_record(&pulled, "r1").await;
        assert_eq!((run.status.as_str(), run.steps_executed), ("failed", 4));

        // A pulled DB that already holds part of this run (another process on this machine
        // pushed the shared local DB mid-run) gets the rest, and keeps another run's rows.
        let partial = fresh_db(&dir, "partial.db").await;
        for id in ["other", "r1"] {
            db::create_run(&partial, id, "get", "[]", None, Some(5))
                .await
                .unwrap();
        }
        record_steps(&partial, "other", &[fx.row("f.py:a")])
            .await
            .unwrap();
        record_steps(
            &partial,
            "r1",
            &[fx.row("f.py:a"), fx.row("f.py:part[k=2]")],
        )
        .await
        .unwrap();
        persist_run(&partial, &fx.ledger("r1")).await.unwrap();
        let mut want = complete("r1");
        want.push((
            "other".to_string(),
            "f.py:a".to_string(),
            "success".to_string(),
        ));
        want.sort();
        assert_eq!(rows(&partial).await, want);
    }

    /// A pull, as `state_sync::pull_state` does it once the blob is downloaded: a fresh copy
    /// of `shared` is swapped in for `local`, which keeps its unpushed rows.
    async fn pull(dir: &tempfile::TempDir, shared: &str, local: &str) {
        state_sync::checkpoint_truncate(shared).await.unwrap();
        let staged = dir.path().join("staged.db");
        std::fs::copy(shared, &staged).unwrap();
        db::pull_for_tests(local, &staged).await;
    }

    #[tokio::test]
    async fn a_pull_in_the_middle_of_a_run_leaves_the_run_whole_and_nothing_twice() {
        // Another process in the same project (a second `barca get`, a `barca status`) pulls
        // while this run is going; later the run's own push conflicts and it pulls again.
        let dir = tempfile::tempdir().unwrap();
        let mut fx = Fixture::new();
        // The pull only carries steps whose artifact is there.
        for (node, oref) in fx.outputs.iter_mut() {
            let file = dir.path().join(crate::safe_node_id(node));
            std::fs::write(&file, b"1").unwrap();
            oref.path = file.to_string_lossy().to_string();
        }
        let shared = fresh_db(&dir, "shared.db").await;
        db::create_run(&shared, "theirs", "get", "[\"g.py\"]", None, Some(1))
            .await
            .unwrap();
        let local = fresh_db(&dir, "local.db").await;
        db::create_run(&local, "r1", "get", "[\"f.py\"]", None, Some(5))
            .await
            .unwrap();
        record_steps(&local, "r1", &[fx.row("f.py:a")])
            .await
            .unwrap();

        // Mid-run pull: the run row and the recorded step are still there afterwards, so
        // `barca status` goes on showing the step and `barca history` the run.
        pull(&dir, &shared, &local).await;
        let mid = run_record(&local, "r1").await;
        assert_eq!((mid.status.as_str(), mid.steps_executed), ("running", 1));
        assert_eq!(rows(&local).await.len(), 1);
        run_record(&local, "theirs").await;

        // The run goes on recording, ends, and writes its ledger and its log.
        record_steps(&local, "r1", &[fx.row("f.py:part[k=1]")])
            .await
            .unwrap();
        persist_run(&local, &fx.ledger("r1")).await.unwrap();
        let log = [("f.py:a".to_string(), "hello".to_string())];
        db::insert_logs(&local, "r1", &log).await.unwrap();
        assert_eq!(rows(&local).await, complete("r1"));

        // Its push conflicts: pull again, replay the ledger and the log.
        db::create_run(&shared, "theirs-2", "get", "[\"g.py\"]", None, Some(1))
            .await
            .unwrap();
        pull(&dir, &shared, &local).await;
        db::init_db(&local).await.unwrap();
        persist_run(&local, &fx.ledger("r1")).await.unwrap();
        db::insert_logs(&local, "r1", &log).await.unwrap();

        assert_eq!(rows(&local).await, complete("r1"));
        assert_eq!(db::get_logs(&local, "r1").await.unwrap().len(), 1);
        let end = run_record(&local, "r1").await;
        assert_eq!((end.status.as_str(), end.steps_executed), ("failed", 4));
        assert_eq!(db::count_runs(&local).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn the_recorder_and_the_ledger_write_the_same_row() {
        let dir = tempfile::tempdir().unwrap();
        let fx = Fixture::new();
        let columns = "node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, \
                       elapsed_seconds, status, attempts, run_id";
        let mut seen = Vec::new();
        for (name, by_recorder) in [("recorder.db", true), ("ledger.db", false)] {
            let db_path = fresh_db(&dir, name).await;
            if by_recorder {
                db::create_run(&db_path, "r1", "get", "[]", None, Some(5))
                    .await
                    .unwrap();
                record_steps(&db_path, "r1", &[fx.row("f.py:a")])
                    .await
                    .unwrap();
            } else {
                persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
            }
            let _g = db::db_guard().await;
            let (_db, conn) = db::open_conn(&db_path).await.unwrap();
            let mut found = conn
                .query(
                    &format!("SELECT {columns} FROM materializations WHERE node_id = 'f.py:a'"),
                    (),
                )
                .await
                .unwrap();
            let row = found.next().await.unwrap().expect("a row for f.py:a");
            seen.push(format!(
                "{:?}",
                (0..9)
                    .map(|i| row.get_value(i).unwrap())
                    .collect::<Vec<_>>()
            ));
        }
        assert_eq!(seen[0], seen[1]);
    }

    #[tokio::test]
    async fn the_recorder_writes_during_the_run_and_leaves_the_rest_to_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();

        db::create_run(&db_path, "r1", "get", "[]", None, Some(5))
            .await
            .unwrap();
        let recorder = StepRecorder::start(db_path.clone(), "r1".to_string());
        recorder.record(fx.row("f.py:a"));
        recorder.record(fx.row("f.py:part[k=1]"));
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while rows(&db_path).await.len() < 2 {
            assert!(Instant::now() < deadline, "the recorder never wrote");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // Queued just before the run ends: finish() does not wait a full interval to write it.
        recorder.record(fx.row("f.py:part[k=2]"));
        let stopping = Instant::now();
        recorder.finish().await;
        assert!(stopping.elapsed() < RECORD_INTERVAL);

        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
    }
}

#[cfg(test)]
mod telemetry_report_tests {
    use super::*;

    fn worker_error(message: &str) -> dispatch::StepError {
        dispatch::StepError {
            error_type: "WorkerError".to_string(),
            message: message.to_string(),
            traceback: String::new(),
            attempts: 1,
        }
    }

    #[test]
    fn the_exception_type_message_and_frames_are_separated() {
        let (ty, msg, stack) = exception_of(&worker_error(
            "ValueError: cannot publish\n  File \"p.py\", line 3, in publish\n    raise ValueError(\"cannot publish\")",
        ));
        assert_eq!(
            (ty.as_str(), msg.as_str()),
            ("ValueError", "cannot publish")
        );
        assert_eq!(
            stack.as_deref(),
            Some("  File \"p.py\", line 3, in publish\n    raise ValueError(\"cannot publish\")")
        );
    }

    #[test]
    fn a_message_that_quotes_a_traceback_stays_whole() {
        let (ty, msg, stack) = exception_of(&worker_error(
            "RuntimeError: bad config:\n  File \"/etc/x.conf\" is missing\nplease fix\n  File \"p.py\", line 9, in load\n    raise RuntimeError(m)",
        ));
        assert_eq!(ty, "RuntimeError");
        assert_eq!(
            msg,
            "bad config:\n  File \"/etc/x.conf\" is missing\nplease fix"
        );
        assert_eq!(
            stack.as_deref(),
            Some("  File \"p.py\", line 9, in load\n    raise RuntimeError(m)")
        );
    }

    #[test]
    fn an_error_without_frames_or_a_python_type_is_passed_through() {
        let mut upload = worker_error("upload to s3://b/x failed: ConnectionError: reset");
        upload.error_type = "UploadError".to_string();
        let (ty, msg, stack) = exception_of(&upload);
        assert_eq!(ty, "UploadError");
        assert_eq!(msg, "upload to s3://b/x failed: ConnectionError: reset");
        assert_eq!(stack, None);
        assert_eq!(
            exception_of(&worker_error("worker disconnected")).0,
            "WorkerError"
        );
    }
}
