//! Recording a run: the run row, each step's row as it finishes, the final write, and the minute
//! backup of the metadata DB (RFC-0009). Also the telemetry report a finished run hands to the
//! configured integration.

use crate::BarcaError;
use crate::dag::Dag;
use crate::db;
use crate::dispatch;
use crate::helper_proc;
use std::collections::{HashMap, HashSet};
use tokio_util::sync::CancellationToken;

/// Record as `cancelled` a run that was already recorded with its outcome, because Ctrl-C
/// arrived while its history was being backed up. The steps it recorded stay: they finished.
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

/// Everything one run wants written to the metadata DB at its end, held in memory until then.
pub(crate) struct RunLedger<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) status: &'a str,
    pub(crate) command: &'a str,
    pub(crate) files: String,
    pub(crate) target: Option<&'a str>,
    /// Canonical resolved job ids (`RunReport.job`), recorded with the outcome.
    pub(crate) job: &'a str,
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
    /// Executed step -> its run hash's components as JSON ([`crate::changes`]).
    pub(crate) components: &'a HashMap<String, String>,
    /// Sensor step -> content hash of the output it returned in this run. Other steps
    /// record the hash on their `OutputRef`, when the artifact went through a store.
    pub(crate) output_hashes: &'a HashMap<String, String>,
    /// Artifact-store location of each uploaded output, recorded instead of
    /// its local path so cache hits resolve on every machine.
    pub(crate) store_paths: &'a HashMap<String, String>,
    /// Run-end snapshot of the measured-cost EWMA, seeding the next run.
    pub(crate) cost_snapshot: &'a [(String, crate::cost::NodeEstimate)],
    /// Recorded artifact paths the run read (its inputs, and the outputs it returns): their
    /// `last_read_at` is set first, on the ledger's connection, so the run's backup carries it
    /// (RFC-0012). It was a connection of its own (#560).
    pub(crate) touched: &'a [String],
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
    /// Digest of the producer-owned serialized bytes.
    output_hash: Option<String>,
    /// The run hash's components ([`crate::changes::step_components_json`]).
    pub(crate) components: Option<String>,
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
            components: None,
        }
    }

    /// With the run hash's components recorded ([`crate::changes`]).
    pub(crate) fn with_components(mut self, components: Option<String>) -> Self {
        self.components = components;
        self
    }
}

/// Appends successful steps' rows, unless the run already has a row for the node. Prepared once
/// per transaction and rebound for each row (#460).
struct StepInsert(turso::Statement);

impl StepInsert {
    // Turso otherwise intersects the node and run indexes, scanning this run
    // for each partition. Bound deduplication to the node's own history.
    const SQL: &'static str = "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, artifact_size_bytes, elapsed_seconds, status, attempts, sinks_json, cpu_seconds, max_rss_bytes, output_hash, bytes_changed, run_id, components) SELECT ?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), 'success', ?7, NULLIF(?8, ''), NULLIF(?9, ''), NULLIF(?10, ''), NULLIF(?11, ''), COALESCE((SELECT output_hash FROM materializations INDEXED BY idx_mat_node_run WHERE node_id = ?1 AND run_hash = ?2 AND status = 'success' AND output_hash IS NOT NULL ORDER BY id DESC LIMIT 1) != NULLIF(?11, ''), 0), ?12, NULLIF(?13, '') WHERE NOT EXISTS (SELECT 1 FROM materializations INDEXED BY idx_mat_node_run WHERE node_id = ?1 AND run_id = ?12)";

    async fn prepare(conn: &turso::Connection) -> Result<Self, turso::Error> {
        db::prepare(conn, Self::SQL).await.map(Self)
    }

    /// Rows written: 1, or 0 when the run already recorded the node.
    async fn run(&mut self, row: &StepRow, run_id: &str) -> Result<u64, turso::Error> {
        let opt = |v: Option<String>| v.unwrap_or_default();
        self.0
            .execute([
                row.node_id.clone(),
                row.run_hash.clone(),
                row.path.clone(),
                row.format.clone(),
                row.size_bytes.to_string(),
                opt(row.elapsed_seconds.map(|e| e.to_string())),
                row.attempts.to_string(),
                opt(row.sinks_json.clone()),
                opt(row.cpu_seconds.map(|c| c.to_string())),
                opt(row.max_rss_bytes.map(|r| r.to_string())),
                opt(row.output_hash.clone()),
                run_id.to_string(),
                opt(row.components.clone()),
            ])
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
/// at [`StepRecorder::finish`] are made good at the end.
///
/// With a store, the recorder also backs the database up on a minute checkpoint
/// ([`crate::backup::take`]) when it has recorded something since the last one.
const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const CHECKPOINT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// How many checkpoints in a row may fail before the run says so, once (RFC-0009 section 4.5).
const CHECKPOINT_FAILURES_REPORTED: u32 = 3;

/// What the recorder needs to back the database up on the minute checkpoint.
pub(crate) struct BackupCheckpoints {
    pub(crate) python: std::path::PathBuf,
    pub(crate) cfg: crate::config::ResolvedConfig,
    pub(crate) cancel: CancellationToken,
}

pub(crate) struct StepRecorder {
    tx: tokio::sync::mpsc::UnboundedSender<StepRow>,
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl StepRecorder {
    #[cfg(test)]
    pub(crate) fn start(db_path: String, run_id: String) -> Self {
        Self::start_with_backups(db_path, run_id, None)
    }

    pub(crate) fn start_with_backups(
        db_path: String,
        run_id: String,
        backups: Option<BackupCheckpoints>,
    ) -> Self {
        Self::start_with_intervals(
            db_path,
            run_id,
            backups,
            RECORD_INTERVAL,
            CHECKPOINT_INTERVAL,
        )
    }

    // Private timing injection exercises real backups without a public test knob.
    fn start_with_intervals(
        db_path: String,
        run_id: String,
        backups: Option<BackupCheckpoints>,
        record_interval: std::time::Duration,
        checkpoint_interval: std::time::Duration,
    ) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StepRow>();
        let stop = backups
            .as_ref()
            .map_or_else(CancellationToken::new, |p| p.cancel.child_token());
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let mut next_write = tokio::time::Instant::now() + record_interval;
            let mut pending = Vec::new();
            let mut generation = 0u64;
            let mut backed_up = 0u64;
            // Consecutive checkpoints that did not reach the store, and whether that was said.
            let mut failures = 0u32;
            let mut warned = false;
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
                        if let Ok(inserted) = record_steps_with_until(
                            &db_path, &run_id, &pending,
                            helper_proc::Until::cancelled(&stopped),
                        ).await {
                            generation += inserted;
                            pending.clear();
                        }
                        next_write = tokio::time::Instant::now() + record_interval;
                    }
                    _ = checkpoint.tick(), if backups.is_some() => {
                        // Only when something was recorded since, or the last copy has not
                        // reached the store yet.
                        if generation == backed_up && failures == 0 {
                            continue;
                        }
                        let snapshot_generation = generation;
                        let b = backups.as_ref().expect("backups enabled");
                        let outcome = crate::backup::take(&b.python, &b.cfg, Some(&run_id), helper_proc::Until {
                            cancel: Some(&stopped),
                            deadline: Some(std::time::Instant::now() + CHECKPOINT_BUDGET),
                        }).await;
                        let failed = match outcome {
                            Ok(taken) => {
                                backed_up = snapshot_generation;
                                match taken.upload {
                                    Some(Err(why)) => Some(why),
                                    _ => None,
                                }
                            }
                            Err(_) if stopped.is_cancelled() => None,
                            Err(error) => Some(error.to_string()),
                        };
                        match failed {
                            None => failures = 0,
                            Some(why) => {
                                failures += 1;
                                if failures >= CHECKPOINT_FAILURES_REPORTED && !std::mem::replace(&mut warned, true) {
                                    crate::errln!(
                                        "[barca] warning: the metadata backup failed {failures} checkpoints in a row ({why}); the run goes on, its results are recorded on this machine, and the next checkpoint tries again"
                                    );
                                }
                            }
                        }
                    }
                    row = rx.recv() => match row {
                        Some(row) => pending.push(row),
                        None => break,
                    },
                }
            }
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
    pub(crate) async fn finish(mut self) -> Result<(), BarcaError> {
        self.stop.cancel();
        self.task
            .take()
            .expect("recorder task owned")
            .await
            .map_err(|e| BarcaError::Other(format!("progress recorder stopped unexpectedly: {e}")))
    }
}

impl Drop for StepRecorder {
    fn drop(&mut self) {
        // Cancellation lets an in-flight backup helper clean its staged copy.
        self.stop.cancel();
    }
}

/// Append `rows` for a run that is still in progress, and advance its `steps_executed` so
/// `barca history` shows how far it has got.
#[cfg(test)]
async fn record_steps(db_path: &str, run_id: &str, rows: &[StepRow]) -> Result<u64, BarcaError> {
    record_steps_with_until(db_path, run_id, rows, helper_proc::Until::done()).await
}

async fn record_steps_with_until(
    db_path: &str,
    run_id: &str,
    rows: &[StepRow],
    until: helper_proc::Until<'_>,
) -> Result<u64, BarcaError> {
    let _g = until.admit(db::db_guard()).await?;
    let (_db, conn) = db::open_conn_with_until(db_path, until).await?;
    until.check()?;
    // Once admitted, finish the transaction even if cancellation arrives during it.
    db::with_tx(&conn, "BEGIN", "progress batch", async {
        let failed = |e| BarcaError::Db(format!("failed to record progress step: {e}"));
        let mut written = 0u64;
        let mut insert = StepInsert::prepare(&conn).await.map_err(failed)?;
        for row in rows {
            written += insert.run(row, run_id).await.map_err(failed)?;
        }
        drop(insert);
        let updated = conn.execute(
            "UPDATE runs SET steps_executed = steps_executed + ?1 WHERE run_id = ?2 AND status = 'running'",
            [written.to_string(), run_id.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to update progress count: {e}")))?;
        if written > 0 && updated != 1 {
            return Err(BarcaError::Db("cannot record new progress for a missing or finished run".to_string()));
        }
        Ok(written)
    })
    .await
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

/// What the engine knows about a run besides its ledger, for [`telemetry_report`].
pub(crate) struct TelemetryRun<'a> {
    /// When the run started.
    pub started: std::time::SystemTime,
    /// Per step: when its worker finished it (Unix seconds) and how long it took.
    pub clocks: &'a HashMap<String, (f64, f64)>,
    /// Nodes that were planned but did not run (an upstream failed, or the run stopped).
    pub skipped: &'a std::collections::HashSet<String>,
    /// The pipeline files the command was given.
    pub files: &'a [String],
    /// The barca environment the run used (`--env`, `BARCA_ENV`, ...), when known.
    pub env: Option<&'a str>,
}

/// The run as telemetry integrations see it: every step that ran, was served from cache,
/// failed, or was skipped. A step that ran is placed by the worker's own clock; a cached step
/// is a zero-length mark at the start of the run. A failed step spans from its first attempt
/// to its permanent failure, retries included; one whose failure was not timed (an upload that
/// failed after the step ran) is placed by the worker's clock, or as a zero-length mark at the
/// end of the run, where a skipped step is marked too.
pub(crate) fn telemetry_report(
    l: &RunLedger<'_>,
    dag: &Dag,
    run: TelemetryRun<'_>,
) -> crate::telemetry::RunReport {
    use crate::telemetry::{NodeContext, RunReport, StepOutcome, StepReport};
    use std::sync::Arc;

    let TelemetryRun {
        started,
        clocks,
        skipped,
        files,
        env,
    } = run;
    let job = l.job;
    let ns = |seconds: f64| (seconds.max(0.0) * 1e9) as u64;
    let start_unix_ns = started
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let duration_ns = ns(l.elapsed);
    let contexts: HashMap<String, Arc<NodeContext>> = crate::telemetry::node_contexts(dag)
        .into_iter()
        .map(|(id, ctx)| (id, Arc::new(ctx)))
        .collect();
    let context = |base: &str| {
        contexts
            .get(base)
            .cloned()
            .unwrap_or_else(|| Arc::new(NodeContext::unknown(base)))
    };
    // Attempts are counted per step, not per partition key, so they are only attributed to
    // an unpartitioned step.
    let attempts = |node_id: &str| {
        let id = crate::StepId::parse(node_id);
        (id.display() == id.base_id()).then(|| {
            l.all_attempts
                .get(&id.display())
                .or_else(|| l.all_attempts.get(id.base_id()))
                .copied()
                .unwrap_or(1)
        })
    };

    let upstream = |base: &str| {
        let mut ids: Vec<String> = dag.upstream(base).into_iter().map(str::to_string).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let step = |node_id: &str, outcome, start, duration| {
        let id = crate::StepId::parse(node_id);
        StepReport {
            node_id: node_id.to_string(),
            node: context(id.base_id()),
            partition: (!id.partition.is_empty()).then(|| id.partition.suffix()),
            outcome,
            start_unix_ns: start,
            duration_ns: duration,
            attempts: None,
            run_hash: l.run_hashes.get(node_id).cloned(),
            upstream: upstream(id.base_id()),
            size_bytes: None,
            digest: None,
            cpu_seconds: None,
            max_rss_bytes: None,
            error_type: None,
            error_message: None,
            error_traceback: None,
        }
    };

    let mut steps: Vec<StepReport> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (node_id, oref) in l.all_outputs {
        let cached = l.cached_node_ids.contains(node_id);
        let (step_start, step_duration) = match clocks.get(node_id) {
            _ if cached => (start_unix_ns, 0),
            Some((finished, wall)) => (ns(finished - wall), ns(*wall)),
            None => (start_unix_ns, ns(oref.elapsed_seconds.unwrap_or(0.0))),
        };
        let (cpu_seconds, max_rss_bytes) =
            l.all_timings.get(node_id).copied().unwrap_or((None, None));
        let outcome = if cached {
            StepOutcome::Cached
        } else {
            StepOutcome::Ran
        };
        steps.push(StepReport {
            attempts: if cached { None } else { attempts(node_id) },
            size_bytes: Some(oref.size_bytes),
            digest: if cached {
                None
            } else {
                l.output_hashes
                    .get(node_id)
                    .or(oref.content_hash.as_ref())
                    .cloned()
            },
            cpu_seconds,
            max_rss_bytes,
            ..step(node_id, outcome, step_start, step_duration)
        });
        seen.insert(crate::StepId::parse(node_id).base_id().to_string());
    }
    for failure in l.all_failures {
        let (error_type, error_message, error_traceback) = exception_of(&failure.error);
        let (step_start, step_duration) = match (
            failure.error.started_unix_ns,
            failure.error.elapsed_seconds,
            clocks.get(&failure.node_id),
        ) {
            (Some(started), Some(wall), _) => (started, ns(wall)),
            (_, _, Some((finished, wall))) => (ns(finished - wall), ns(*wall)),
            _ => (start_unix_ns + duration_ns, 0),
        };
        steps.push(StepReport {
            attempts: Some(failure.error.attempts),
            error_type: Some(error_type),
            error_message: Some(error_message),
            error_traceback,
            ..step(
                &failure.node_id,
                StepOutcome::Failed,
                step_start,
                step_duration,
            )
        });
        seen.insert(crate::StepId::parse(&failure.node_id).base_id().to_string());
    }
    // A node that did not run at all; one with some keys done is reported by those keys.
    let mut not_run: Vec<&String> = skipped.iter().filter(|b| !seen.contains(*b)).collect();
    not_run.sort();
    for base in not_run {
        steps.push(step(
            base,
            StepOutcome::Skipped,
            start_unix_ns + duration_ns,
            0,
        ));
    }
    steps.sort_by(|a, b| (a.start_unix_ns, &a.node_id).cmp(&(b.start_unix_ns, &b.node_id)));

    let mut targets: Vec<String> = if job == "all" {
        Vec::new()
    } else {
        job.split(',').map(|id| context(id).name.clone()).collect()
    };
    targets.sort();
    targets.dedup();

    RunReport {
        run_id: l.run_id.to_string(),
        command: l.command.to_string(),
        target: l.target.map(str::to_string),
        job: job.to_string(),
        env: env.map(str::to_string),
        targets,
        files: files.to_vec(),
        status: l.status.to_string(),
        reason: None,
        in_flight_run: None,
        start_unix_ns,
        duration_ns,
        steps_total: l.steps_total,
        steps_executed: l.steps_executed,
        steps_cached: l.steps_cached,
        steps,
        trigger: Default::default(),
    }
}

/// Write a run's ledger with a short-lived connection. Idempotent, so it is safe after the
/// [`StepRecorder`] has written some of the steps: the run row is INSERT OR
/// IGNORE + terminal UPDATE, and a step is appended only if this run has no row for it yet
/// (a step has one outcome per run).
pub(crate) async fn persist_run(db_path: &str, l: &RunLedger<'_>) -> Result<(), BarcaError> {
    let _g = db::db_guard().await;
    let (_db, conn) = db::open_conn(db_path).await?;

    let result = db::with_tx(&conn, "BEGIN", "terminal ledger", async {
        // In the ledger's transaction: one commit for both. Best effort, as on its own.
        db::touch_read_in(&conn, l.touched).await;
        write_terminal_ledger(&conn, l).await
    })
    .await;
    if let Err(error) = result {
        // The reads happened, whatever became of the ledger.
        db::touch_read_on(&conn, l.touched).await;
        return Err(error);
    }
    // Only a committed outcome replaces the process-liveness witness.
    crate::run_owner::release(db_path, l.run_id);

    // Persist the measured-cost EWMA so the next run starts pre-warmed and
    // skips the cold-start probe entirely. (Inline — this fn already holds
    // the process-wide DB guard.) Best effort, in one transaction of its own after the
    // ledger's: one commit instead of one per partition key (#460).
    if !l.cost_snapshot.is_empty() {
        let _ = db::with_tx(&conn, "BEGIN", "cost estimates", async {
        if let Ok(mut upsert) = db::prepare(
            &conn,
            "INSERT INTO cost_estimates (node_id, base_id, estimate_seconds, cpu_seconds, max_rss_bytes, samples, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(node_id) DO UPDATE SET
                 estimate_seconds = ?3, cpu_seconds = ?4, max_rss_bytes = ?5,
                 samples = ?6, updated_at = datetime('now')",
        )
        .await
        {
            for (node_id, est) in l.cost_snapshot {
                let base = crate::StepId::parse(node_id).base_id().to_string();
                upsert
                    .execute([
                        node_id.clone(),
                        base,
                        est.estimate_seconds.to_string(),
                        est.cpu_seconds.to_string(),
                        est.max_rss_bytes.to_string(),
                        est.samples.to_string(),
                    ])
                    .await
                    .ok();
            }
        }
        Ok(())
        })
        .await;
    }

    Ok(())
}

/// The steps of `run_id` already recorded, by node id.
async fn steps_of_run(
    conn: &turso::Connection,
    run_id: &str,
) -> Result<HashSet<String>, BarcaError> {
    let failed = |e| BarcaError::Db(format!("failed to read recorded steps: {e}"));
    let mut out = HashSet::new();
    let mut rows = conn
        .query(
            "SELECT node_id FROM materializations WHERE run_id = ?1",
            [run_id.to_string()],
        )
        .await
        .map_err(failed)?;
    while let Some(row) = rows.next().await.map_err(failed)? {
        out.insert(row.get::<String>(0).map_err(failed)?);
    }
    Ok(out)
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
    // What the [`StepRecorder`] wrote during the run.
    let already = steps_of_run(conn, l.run_id).await?;

    let mut insert = None;
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
        row.attempts = l
            .all_attempts
            .get(node_id)
            .or_else(|| l.all_attempts.get(&base))
            .copied()
            .unwrap_or(1);
        row.sinks_json = l.all_sinks.get(node_id).cloned();
        row.cpu_seconds = cpu;
        row.max_rss_bytes = rss;
        row.output_hash = l
            .output_hashes
            .get(node_id)
            .or(oref.content_hash.as_ref())
            .cloned();
        row.components = l.components.get(node_id).cloned();
        let failed = |e| BarcaError::Db(format!("failed to record terminal step: {e}"));
        let insert = match &mut insert {
            Some(insert) => insert,
            None => insert.insert(StepInsert::prepare(conn).await.map_err(failed)?),
        };
        insert.run(&row, l.run_id).await.map_err(failed)?;
    }
    drop(insert);

    // Persist permanently-failed steps as `status='failed'` rows (artifact
    // columns NULL). Failed rows are never served as cache hits.
    let mut insert_failed = None;
    for failure in l.all_failures {
        let node_id = &failure.node_id;
        if already.contains(node_id) {
            continue;
        }
        let run_h = l.run_hashes.get(node_id).cloned().unwrap_or_default();
        let failed = |e| BarcaError::Db(format!("failed to record failed step: {e}"));
        let insert = match &mut insert_failed {
            Some(insert) => insert,
            None => insert_failed.insert(
                db::prepare(
                    conn,
                    "INSERT INTO materializations (node_id, run_hash, status, error_type, error_message, error_traceback, attempts, run_id) VALUES (?1, ?2, 'failed', ?3, ?4, ?5, ?6, ?7)",
                )
                .await
                .map_err(failed)?,
            ),
        };
        // Each failure carries its own attempt count: dispatches for a worker
        // failure, transfer attempts for an upload failure.
        insert
            .execute([
                node_id.clone(),
                run_h,
                failure.error.error_type.clone(),
                failure.error.message.clone(),
                failure.error.traceback.clone(),
                failure.error.attempts.to_string(),
                l.run_id.to_string(),
            ])
            .await
            .map_err(failed)?;
    }
    drop(insert_failed);
    let updated = conn.execute(
            "UPDATE runs SET status = ?1, steps_executed = ?2, steps_cached = ?3, elapsed_seconds = ?4, finished_at = datetime('now'), job = NULLIF(?6, '') WHERE run_id = ?5",
            [
                l.status.to_string(),
                l.steps_executed.to_string(),
                l.steps_cached.to_string(),
                l.elapsed.to_string(),
                l.run_id.to_string(),
                l.job.to_string(),
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

    fn no_clocks() -> HashMap<String, (f64, f64)> {
        HashMap::new()
    }

    /// A `get` of everything in `f.py`, started at `started`.
    fn at(
        started: std::time::SystemTime,
        clocks: &HashMap<String, (f64, f64)>,
    ) -> TelemetryRun<'_> {
        static NONE: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
        static FILES: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec!["f.py".to_string()]);
        TelemetryRun {
            started,
            clocks,
            skipped: &NONE,
            files: &FILES,
            env: None,
        }
    }

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
                        started_unix_ns: None,
                        elapsed_seconds: None,
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
                job: "f.py:a",
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
                touched: &[],
                run_hashes: &self.run_hashes,
                components: &self.empty,
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
            task: Some(tokio::spawn(async {})),
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
        recorder.finish().await.unwrap();
    }

    /// A configuration whose store is a directory in `dir`, and the real `barca._state`
    /// helper from this checkout. `outage` makes every backup upload fail while it exists.
    fn backup_test_config(
        dir: &tempfile::TempDir,
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
import signal, sys, time
from pathlib import Path
sys.path.insert(0, {source:?})
from barca import _state, _lifeline
root = Path({root:?})
original = _state.backup
def backup(*args):
    with (root / 'attempts').open('a') as f:
        f.write(str(time.monotonic()) + '\n')
    if (root / 'outage').exists():
        raise OSError('test store unavailable')
    return original(*args)
_state.backup = backup
sys.argv = sys.argv[2:]
signal.signal(signal.SIGTERM, _state._stop)
_lifeline.watch()
sys.exit(_state.main())
"#,
                source = source.to_string_lossy(),
                root = root.to_string_lossy(),
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
        cfg.db_path = root.join("meta/metadata.db").to_string_lossy().into_owned();
        cfg.backup_uri = Some(root.join("store").to_string_lossy().into_owned());
        std::fs::create_dir_all(root.join("meta")).unwrap();
        std::fs::create_dir_all(root.join("store")).unwrap();
        (cfg, python)
    }

    fn backups_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| crate::backup::is_backup_name(n))
            .collect();
        names.sort();
        names
    }

    /// The minute checkpoint backs the database up when the run recorded something since the
    /// last one, and only then; a store outage is retried at the next tick (not in a busy
    /// loop) and the copy reaches the store once it is back.
    #[tokio::test]
    async fn the_checkpoint_backs_up_new_progress_and_retries_an_outage() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, python) = backup_test_config(&dir);
        db::init_db(&cfg.db_path).await.unwrap();
        db::create_run(&cfg.db_path, "r1", "get", "[]", None, Some(2))
            .await
            .unwrap();
        std::fs::write(dir.path().join("outage"), b"").unwrap();
        let recorder = StepRecorder::start_with_intervals(
            cfg.db_path.clone(),
            "r1".into(),
            Some(BackupCheckpoints {
                python,
                cfg: cfg.clone(),
                cancel: CancellationToken::new(),
            }),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(150),
        );
        recorder.record(Fixture::new().row("f.py:a"));
        let attempts = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let text = std::fs::read_to_string(dir.path().join("attempts")).unwrap_or_default();
                if text.ends_with('\n')
                    && let Ok(attempts) = text
                        .lines()
                        .map(str::parse::<f64>)
                        .collect::<Result<Vec<_>, _>>()
                    && attempts.len() >= 2
                {
                    break attempts;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a failed backup was not retried without a new row");
        assert!(
            attempts.windows(2).all(|pair| pair[1] - pair[0] >= 0.1),
            "failure must not cause busy retries: {attempts:?}"
        );
        // The copy was taken here even though the store refused it.
        let local = cfg.local_backup_dir();
        assert_eq!(backups_in(&local).len(), 1);
        assert!(backups_in(&dir.path().join("store")).is_empty());
        std::fs::remove_file(dir.path().join("outage")).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while backups_in(&dir.path().join("store")).is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the copy never reached the store");
        // Nothing new recorded: later ticks take nothing.
        let settled = std::fs::read_to_string(dir.path().join("attempts")).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        recorder.finish().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("attempts")).unwrap(),
            settled
        );
        assert_eq!(backups_in(&local), backups_in(&dir.path().join("store")));
        assert_eq!(run_record(&cfg.db_path, "r1").await.steps_executed, 1);
    }

    async fn count(db_path: &str, sql: &str) -> i64 {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await.unwrap();
        let mut rows = conn.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    /// Statements prepared by recording `keys` partitions of `part` through the recorder, the
    /// terminal ledger (`keys` more outputs and `keys` failures, with a cost snapshot per key),
    /// and the read stamps of every recorded path.
    async fn prepares_to_record(keys: usize) -> (u64, u64, u64) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let mut fx = Fixture::new();
        let mut recorded = Vec::new();
        for k in 0..keys {
            for (step, node) in [
                ("p", format!("f.py:p[k={k}]")),
                ("q", format!("f.py:q[k={k}]")),
            ] {
                fx.run_hashes
                    .insert(node.clone(), format!("hash-{step}{k}"));
                fx.outputs.insert(node.clone(), oref(&node));
            }
            let failed = format!("f.py:r[k={k}]");
            fx.run_hashes.insert(failed.clone(), format!("hash-r{k}"));
            let mut failure = fx.failures[0].clone();
            failure.node_id = failed;
            fx.failures.push(failure);
            recorded.push(fx.row(&format!("f.py:p[k={k}]")));
        }
        let estimate = crate::cost::NodeEstimate {
            estimate_seconds: 0.1,
            cpu_seconds: 0.1,
            max_rss_bytes: 1,
            samples: 1,
        };
        let snapshot: Vec<_> = (0..keys)
            .map(|k| (format!("f.py:q[k={k}]"), estimate))
            .collect();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(3 * keys + 5))
            .await
            .unwrap();

        let before = db::prepares();
        assert_eq!(
            record_steps(&db_path, "r1", &recorded).await.unwrap(),
            keys as u64
        );
        let recorder = db::prepares() - before;

        let before = db::prepares();
        let mut ledger = fx.ledger("r1");
        ledger.cost_snapshot = &snapshot;
        persist_run(&db_path, &ledger).await.unwrap();
        let terminal = db::prepares() - before;

        let paths: Vec<String> = fx.outputs.values().map(|o| o.path.clone()).collect();
        let before = db::prepares();
        db::touch_read(&db_path, &paths).await;
        let touched = db::prepares() - before;

        // Every row landed: the cached node has none, `bad` and each `r` key a failed one.
        let success = count(
            &db_path,
            "SELECT COUNT(*) FROM materializations WHERE status = 'success'",
        )
        .await;
        assert_eq!(success, (EXECUTED.len() + 2 * keys) as i64);
        let failed = count(
            &db_path,
            "SELECT COUNT(*) FROM materializations WHERE status = 'failed'",
        )
        .await;
        assert_eq!(failed, (1 + keys) as i64);
        let estimates = count(&db_path, "SELECT COUNT(*) FROM cost_estimates").await;
        assert_eq!(estimates, keys as i64);
        let read = count(
            &db_path,
            "SELECT COUNT(*) FROM materializations WHERE last_read_at IS NOT NULL",
        )
        .await;
        assert_eq!(read, success);
        (recorder, terminal, touched)
    }

    /// Turso parses the SQL of every `execute` again; at 20k keys that was most of the run's
    /// metadata time (#460). Each row-writing statement is prepared once per write, however
    /// many rows it writes: a regression to per-row `execute` (or per-row prepare) changes
    /// these counts without a wall-clock threshold.
    #[tokio::test]
    async fn recording_a_run_prepares_each_statement_once_whatever_the_key_count() {
        // Recorder: the success insert. Ledger: success insert, failure insert and the cost
        // upsert. Read stamps: the update.
        assert_eq!(prepares_to_record(2).await, (1, 3, 1));
        assert_eq!(prepares_to_record(300).await, (1, 3, 1));
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
            StepInsert::prepare(&conn)
                .await
                .unwrap()
                .run(&fx.row("f.py:part[k=1]"), "blocker")
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
        recorder.finish().await.unwrap();
        assert_eq!(rows(&db_path).await.len(), 2); // unrelated blocker remains intact
    }

    #[tokio::test]
    async fn an_aborted_recorder_leaves_the_terminal_ledger_whole() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let recorder = StepRecorder::start(db_path.clone(), "r1".into());
        recorder.task.as_ref().unwrap().abort();
        assert!(recorder.finish().await.is_err());
        // Mid-run failure must not remove the complete terminal writer's fallback.
        let fx = Fixture::new();
        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
        assert_eq!(run_record(&db_path, "r1").await.status, "failed");
    }

    #[tokio::test]
    async fn recorder_stop_cancels_batch_admission_and_leaves_terminal_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        let fx = Fixture::new();
        db::create_run(&db_path, "r1", "get", "[]", None, Some(5))
            .await
            .unwrap();
        let held = db::db_guard().await;
        let recorder = StepRecorder::start(db_path.clone(), "r1".into());
        recorder.record(fx.row("f.py:a"));
        // Run the real batch timer with the mutex held, so admission is pending.
        tokio::time::sleep(RECORD_INTERVAL * 2).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), recorder.finish())
            .await
            .expect("recorder stop waited for an unrelated DB owner")
            .unwrap();
        drop(held);
        assert_eq!(run_record(&db_path, "r1").await.steps_executed, 0);
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
        recorder.finish().await.unwrap();
        assert!(stopping.elapsed() < RECORD_INTERVAL);

        persist_run(&db_path, &fx.ledger("r1")).await.unwrap();
        assert_eq!(rows(&db_path).await, complete("r1"));
    }

    #[test]
    fn step_reports_name_their_upstream_and_the_digest_of_what_ran() {
        let mut fx = Fixture::new();
        fx.outputs.get_mut("f.py:a").unwrap().content_hash = Some("sha-a".to_string());
        fx.outputs.get_mut("f.py:cached").unwrap().content_hash = Some("sha-c".to_string());
        let source = "from barca import asset\n@asset()\ndef raw(): return 1\n@asset()\ndef rates(): return 2\n\
                      @asset(inputs={\"rates\": rates, \"raw\": raw})\ndef a(rates, raw): return raw\n\
                      @asset(inputs={\"raw\": raw})\ndef cached(raw): return raw\n\
                      @asset(inputs={\"a\": a})\ndef bad(a): return a\n";
        let dag = crate::Dag::build(&crate::parse::extract_nodes(source, "f.py").unwrap()).unwrap();
        let report = telemetry_report(
            &fx.ledger("r1"),
            &dag,
            at(std::time::UNIX_EPOCH, &no_clocks()),
        );
        let step = |id: &str| report.steps.iter().find(|s| s.node_id == id).unwrap();
        assert_eq!(step("f.py:a").upstream, ["f.py:rates", "f.py:raw"]);
        assert_eq!(step("f.py:a").digest.as_deref(), Some("sha-a"));
        // A cached step names its upstream but carries no digest: nothing was written.
        assert_eq!(step("f.py:cached").upstream, ["f.py:raw"]);
        assert_eq!(step("f.py:cached").digest, None);
        assert_eq!(step("f.py:bad").upstream, ["f.py:a"]);
        assert_eq!(step("f.py:bad").digest, None);
    }

    #[test]
    fn step_reports_place_each_node_in_the_pipeline_and_mark_what_was_skipped() {
        let fx = Fixture::new();
        let source = "from barca import asset, partitions, Schedule\n\
                      @asset(freshness=Schedule(\"0 6 * * *\"))\ndef a(): return 1\n\
                      @asset(partitions={\"k\": partitions([\"1\", \"2\"])})\ndef part(k): return 1\n\
                      @asset()\ndef cached(): return 1\n\
                      @asset(inputs={\"a\": a})\ndef bad(a): return a\n\
                      @asset(inputs={\"bad\": bad})\ndef after(bad): return bad\n";
        let dag = crate::Dag::build(&crate::parse::extract_nodes(source, "f.py").unwrap()).unwrap();
        let skipped = HashSet::from(["f.py:after".to_string(), "f.py:part".to_string()]);
        let clocks = no_clocks();
        let files = vec!["f.py".to_string()];
        let mut ledger = fx.ledger("r1");
        ledger.job = "f.py:after,f.py:a";
        let report = telemetry_report(
            &ledger,
            &dag,
            TelemetryRun {
                started: std::time::UNIX_EPOCH,
                clocks: &clocks,
                skipped: &skipped,
                files: &files,
                env: None,
            },
        );
        assert_eq!(report.targets, ["a", "after"]);
        assert_eq!(report.files, ["f.py"]);
        let step = |id: &str| report.steps.iter().find(|s| s.node_id == id).unwrap();
        assert_eq!(step("f.py:a").node.name, "a");
        assert_eq!(step("f.py:a").node.freshness, "schedule");
        assert_eq!(step("f.py:a").node.schedule.as_deref(), Some("0 6 * * *"));
        assert_eq!(step("f.py:a").partition, None);
        assert_eq!(step("f.py:part[k=1]").node.base_id, "f.py:part");
        assert_eq!(step("f.py:part[k=1]").partition.as_deref(), Some("k=1"));
        // `after` never ran: it is a skipped step. `part` ran some keys: those report it.
        let after = step("f.py:after");
        assert_eq!(after.outcome, crate::telemetry::StepOutcome::Skipped);
        assert_eq!(after.duration_ns, 0);
        assert_eq!(after.upstream, ["f.py:bad"]);
        assert!(!report.steps.iter().any(|s| s.node_id == "f.py:part"));
    }

    #[test]
    fn a_failed_step_span_lasts_its_recorded_elapsed_time() {
        let mut fx = Fixture::new();
        let started = 1_700_000_000_000_000_000u64;
        fx.failures[0].error.started_unix_ns = Some(started);
        fx.failures[0].error.elapsed_seconds = Some(270.25);
        fx.failures[0].error.attempts = 3;
        let dag = crate::Dag::build(&[]).unwrap();
        let run_started = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(started);
        let report = telemetry_report(&fx.ledger("r1"), &dag, at(run_started, &no_clocks()));
        let bad = report
            .steps
            .iter()
            .find(|s| s.node_id == "f.py:bad")
            .unwrap();
        assert_eq!(bad.outcome, crate::telemetry::StepOutcome::Failed);
        assert_eq!(bad.start_unix_ns, started);
        assert_eq!(bad.duration_ns, 270_250_000_000);
        assert_eq!(bad.attempts, Some(3));

        // A failure with no time of its own (an upload) is still a mark at the end of the run.
        fx.failures[0].error.started_unix_ns = None;
        fx.failures[0].error.elapsed_seconds = None;
        let report = telemetry_report(&fx.ledger("r1"), &dag, at(run_started, &no_clocks()));
        let bad = report
            .steps
            .iter()
            .find(|s| s.node_id == "f.py:bad")
            .unwrap();
        assert_eq!(bad.start_unix_ns, started + 1_500_000_000);
        assert_eq!(bad.duration_ns, 0);

        // ... unless its worker reported a clock: the function ran, and its upload failed.
        let finished = started as f64 / 1e9 + 1.0;
        let clocks = HashMap::from([("f.py:bad".to_string(), (finished, 0.25))]);
        let report = telemetry_report(&fx.ledger("r1"), &dag, at(run_started, &clocks));
        let bad = report
            .steps
            .iter()
            .find(|s| s.node_id == "f.py:bad")
            .unwrap();
        assert_eq!(bad.duration_ns, 250_000_000);
        assert!(
            bad.start_unix_ns.abs_diff(started + 750_000_000) < 1_000,
            "{}",
            bad.start_unix_ns
        );
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
            started_unix_ns: None,
            elapsed_seconds: None,
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
