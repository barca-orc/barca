//! One run in full ([`run_view`]) and the run list ([`run_list`], [`in_flight`]), from the
//! durable record. A server adds the runs it holds in memory as an overlay ([`LiveRun`]) and
//! the events a run streamed and has not recorded yet; `barca history` on a machine that
//! serves nothing passes none. The durable half of the list is
//! [`crate::queries::history_at`], which `barca history` and `GET /history` print as it is.

use crate::BarcaError;
use crate::RunEvent;
use crate::db;
use crate::history_filter::HistoryFilter;
use crate::outcome::{Outcome, RunControl, RunView};
use crate::run_explain::{Cancellation, WaitingFor};
use crate::run_list::RunningStep;
use crate::step_status::StepStatus;
use crate::time::{rfc3339_millis as timestamp, sql_datetime_rfc3339 as durable_timestamp};
use crate::trigger::{Trigger, TriggeredBy};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ─── the overlay ────────────────────────────────────────────────────────────

/// Where a run a server holds in memory is. Its words ([`LiveStatus::as_str`]) are the run
/// list's: a finished run reads `success`, `failed` or `cancelled` until its row says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStatus {
    Pending,
    Running,
    Complete,
    Skipped,
    Failed,
    Cancelled,
}

impl LiveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LiveStatus::Pending => "pending",
            LiveStatus::Running => "running",
            LiveStatus::Complete => "success",
            LiveStatus::Skipped => "skipped",
            LiveStatus::Failed => "failed",
            LiveStatus::Cancelled => "cancelled",
        }
    }

    /// Accepted and not finished: waiting for a run slot, or executing.
    pub fn in_progress(self) -> bool {
        matches!(self, LiveStatus::Pending | LiveStatus::Running)
    }
}

/// What a finished run's result counts, for a run not recorded yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveSteps {
    pub total: i64,
    pub executed: i64,
    pub cached: i64,
}

/// A run a server holds in memory, as plain data: what the metadata database does not say of
/// it yet, or says later than the server knows. The overlay of [`run_view`] and [`run_list`].
#[derive(Clone, Debug)]
pub struct LiveRun {
    /// The run id the server allocated when it accepted the run.
    pub handle: String,
    /// The same id, once the run has written its database row.
    pub db_run_id: Option<String>,
    pub status: LiveStatus,
    pub command: String,
    /// The targets as given, comma-separated; `None` for every asset.
    pub target: Option<String>,
    /// The files the request named; empty for the deployed set.
    pub files: Vec<String>,
    pub trigger: Trigger,
    /// Unix seconds when the run was accepted, and when it finished.
    pub started_at: f64,
    pub finished_at: Option<f64>,
    /// The run's full result once it finished.
    pub outcome: Option<Outcome>,
    pub error: Option<String>,
    pub cancellation: Option<Cancellation>,
    /// What else a waiting run waits for (a draining server).
    pub waiting_for: Option<WaitingFor>,
    /// Where the request asked to join the queue.
    pub queue: crate::outcome::QueuePlace,
    /// Its result's step counts, once it has one.
    pub steps: Option<LiveSteps>,
}

// ─── one run ────────────────────────────────────────────────────────────────

/// How many of a live run's lines [`run_view`] reads from its spool: the newest ones.
const LIVE_LOG_LINES: usize = 5_000;

/// What [`run_view`] reads.
pub struct RunViewRequest<'a> {
    /// The metadata database to read: the real one, or a snapshot of it.
    pub read_path: &'a str,
    /// The real metadata database: a run in progress spools its lines beside it.
    pub db_path: &'a str,
    pub run_id: &'a str,
    /// At most this many steps (every one with `None`), from the `offset`-th.
    pub limit: Option<usize>,
    pub offset: usize,
    /// The run as the answering server holds it; `None` where no server does.
    pub live: Option<&'a LiveRun>,
    /// The events it streamed, when it still has a stream: the steps and lines not recorded
    /// yet are read from them.
    pub events: Option<Vec<RunEvent>>,
    /// The files of a live run that named none: the deployed set.
    pub default_files: &'a [String],
    /// Read the run's lines too (`GET /runs/{run_id}`); `barca history RUN_ID` prints none.
    pub logs: bool,
}

/// A run read by [`run_view`], before its planned nodes are counted: the host decides which
/// graph plans them ([`RunDraft::plans`], [`RunDraft::plan`]) and then [`RunDraft::finish`]es.
pub struct RunDraft {
    view: RunView,
    record: Option<db::RunRecord>,
    steps_page: db::StepsPage,
    worker_failures: Vec<barca_model::run_explain::WorkerFailure>,
    read_path: String,
    /// The live run has not recorded how it ended.
    unfinished: bool,
}

/// One run, read: as `GET /runs/{run_id}` answers it, and its row as recorded (`None` for a
/// run a server accepted and has not written yet).
pub struct RunViewed {
    pub view: RunView,
    pub record: Option<db::RunRecord>,
}

/// Cancellation authority comes only from the answering server's live registry.
/// Recorded PIDs describe other owners; they are never cancellation targets.
pub async fn run_control(
    read_path: &str,
    id: &str,
    status: &str,
    server_owned: bool,
) -> Result<RunControl, BarcaError> {
    if server_owned || !matches!(status, "pending" | "running" | "interrupted") {
        return Ok(RunControl::server(status));
    }
    let (pid, host) = db::runs::run_process(read_path, id).await?;
    let owner = match (pid, host) {
        (Some(pid), Some(host)) => format!("process {pid} on {host}"),
        (Some(pid), None) => format!("process {pid} (host unknown)"),
        (None, Some(host)) => format!("unknown process on {host}"),
        (None, None) => "unknown".into(),
    };
    let reason = match status {
        "interrupted" => "This execution was interrupted. Inspect or reconcile it with barca history --reconcile.".into(),
        "pending" | "running" if owner == "unknown" => "This server does not own the run and its process is unknown. Inspect barca history before reconciling it.".into(),
        "pending" | "running" => format!("This server does not own the run; stop it at its owner: {owner}."),
        _ => format!("The run already finished ({status})."),
    };
    Ok(RunControl {
        cancellable: false,
        owner,
        reason: Some(reason),
    })
}

/// The run `req` names: its row, the outcome it stored, its steps (the recorded ones, then
/// the ones it streamed and has not recorded yet) and its lines. `None` when neither the
/// database nor the overlay knows it.
pub async fn run_view(req: RunViewRequest<'_>) -> Result<Option<RunDraft>, BarcaError> {
    let RunViewRequest {
        read_path,
        run_id: id,
        limit,
        offset,
        live,
        ..
    } = req;
    let mut record = db::get_run(read_path, id).await?;
    if record.is_none()
        && live.is_none()
        && barca_store::run_queue::list(read_path)
            .await?
            .iter()
            .any(|r| r.run_id == id)
    {
        let previous = db::runs::previous_attempts(read_path, id).await?;
        if let Some(last) = previous.last()
            && let Some(mut queued) = db::get_run(read_path, &last.run_id).await?
        {
            queued.run_id = id.to_string();
            queued.status = "pending".to_string();
            queued.parent_run_id = None;
            queued.run_attempt = None;
            queued.stop_reason = None;
            queued.previous_attempts = previous;
            queued.finished_at = None;
            queued.elapsed_seconds = None;
            queued.steps_total = None;
            queued.steps_executed = 0;
            queued.steps_cached = 0;
            queued.steps_failed = None;
            queued.steps_skipped = None;
            queued.skip_reason = None;
            queued.attempts = None;
            queued.failure = None;
            queued.cancellation = None;
            queued.plan_reasons = None;
            queued.retries = None;
            record = Some(queued);
        }
    }
    let stored = match live {
        Some(LiveRun {
            outcome: Some(_), ..
        }) => None,
        _ => db::get_run_outcome(read_path, id)
            .await?
            .and_then(|text| Outcome::from_text(text).ok()),
    };
    let outcome = live.and_then(|r| r.outcome.clone()).or(stored);
    let mut view = match (&record, live) {
        (Some(record), _) => RunView {
            control: RunControl::default(),
            previous_attempts: record.previous_attempts.clone(),
            parent_run_id: record.parent_run_id.clone(),
            run_attempt: record.run_attempt,
            stop_reason: record.stop_reason.clone(),
            run_id: record.run_id.clone(),
            status: record.status.clone(),
            skip_reason: record.skip_reason.clone(),
            command: record.command.clone(),
            target: record.target.clone(),
            files: record.files.clone(),
            triggered_by: record.triggered_by,
            scheduled_at: record.scheduled_at.clone(),
            schedule_id: record.schedule_id.clone(),
            started_at: durable_timestamp(&record.started_at),
            finished_at: record.finished_at.as_deref().map(durable_timestamp),
            elapsed_seconds: record.elapsed_seconds,
            steps_total: record.steps_total,
            steps_executed: record.steps_executed,
            steps_cached: record.steps_cached,
            job: record.job.clone(),
            attempts: record.attempts,
            client_run_id: record.client_run_id.clone(),
            failure: record.failure.clone(),
            principal: record.principal.clone(),
            trigger_reason: record.trigger_reason.clone(),
            retries: record.retries,
            plan_reasons: record.plan_reasons.clone(),
            cancellation: record
                .cancellation
                .clone()
                .or_else(|| live.and_then(|r| r.cancellation.clone())),
            waiting_for: None,
            queue: None,
            explanation: Default::default(),
            outcome: None,
            error: None,
            steps: Vec::new(),
            steps_page: db::StepsPage::default(),
            logs: Vec::new(),
        },
        (None, Some(run)) => RunView {
            control: RunControl::default(),
            previous_attempts: db::runs::previous_attempts(read_path, id).await?,
            parent_run_id: None,
            run_attempt: None,
            stop_reason: None,
            run_id: run.handle.clone(),
            status: run.status.as_str().into(),
            skip_reason: None,
            command: run.command.clone(),
            target: run.target.clone(),
            files: if run.files.is_empty() {
                req.default_files.to_vec()
            } else {
                run.files.clone()
            },
            triggered_by: Some(run.trigger.triggered_by),
            scheduled_at: run.trigger.scheduled_at.clone(),
            schedule_id: run.trigger.schedule_id.clone(),
            started_at: timestamp(run.started_at),
            finished_at: run.finished_at.map(timestamp),
            elapsed_seconds: run.finished_at.map(|t| t - run.started_at),
            steps_total: None,
            steps_executed: 0,
            steps_cached: 0,
            job: None,
            attempts: None,
            client_run_id: None,
            failure: None,
            principal: run.trigger.principal.clone(),
            trigger_reason: run.trigger.reason.clone(),
            retries: None,
            plan_reasons: None,
            cancellation: run.cancellation.clone(),
            waiting_for: None,
            queue: None,
            explanation: Default::default(),
            outcome: None,
            error: None,
            steps: Vec::new(),
            steps_page: db::StepsPage::default(),
            logs: Vec::new(),
        },
        (None, None) => return Ok(None),
    };
    // A live run is reported as live until it has recorded its outcome, even when its row
    // already says how it ended.
    let in_progress = live.is_some_and(|run| run.status.in_progress());
    if let Some(run) = live
        && in_progress
    {
        view.status = run.status.as_str().into();
        view.waiting_for = run.waiting_for;
        view.queue = run.queue.shown();
    }
    if let Some(outcome) = &outcome {
        if view.stop_reason.is_none() {
            view.status = outcome.status().as_str().into();
        }
        view.error = outcome.envelope().map(|e| e.error);
    }
    view.control = run_control(read_path, id, &view.status, in_progress).await?;
    view.outcome = outcome;
    let (steps, mut steps_page) = db::get_run_steps_window(read_path, id, limit, offset).await?;
    view.steps = steps;
    if req.logs {
        view.logs = db::get_logs(read_path, id).await?;
        // A run in progress has its lines in its spool until it records them (#925): its
        // newest.
        if view.logs.is_empty() && record.is_some() && in_progress {
            view.logs = crate::queries::live_log_tail(req.db_path, id, LIVE_LOG_LINES);
        }
    }
    if let Some(events) = req.events {
        // The steps it streamed and has not recorded yet are listed after the recorded ones.
        let mut recorded: HashSet<String> = if steps_page.truncated {
            db::get_run_steps_page(read_path, id, None)
                .await?
                .0
                .into_iter()
                .map(|s| s.node_id)
                .collect()
        } else {
            view.steps.iter().map(|s| s.node_id.clone()).collect()
        };
        // Replay contains only its retained tail. Recorded logs remain canonical at wrap-up.
        let live_logs = req.logs && view.logs.is_empty();
        for event in events {
            match event {
                RunEvent::Log {
                    node_id,
                    line,
                    stream,
                    ts,
                } if live_logs => view.logs.push(db::LogEntry {
                    node_id,
                    seq: view.logs.len() as i64,
                    line,
                    stream,
                    ts,
                }),
                RunEvent::StepSkipped {
                    node_id,
                    skip_reason,
                    elapsed_seconds,
                } if recorded.insert(node_id.clone()) => {
                    steps_page.total += 1;
                    if offset > 0 || limit.is_some_and(|n| view.steps.len() >= n) {
                        continue;
                    }
                    view.steps.push(db::RunStep {
                        node_id,
                        status: "skipped".into(),
                        skip_reason: Some(skip_reason),
                        elapsed_seconds: Some(elapsed_seconds),
                        ..Default::default()
                    });
                }
                RunEvent::StepFinished {
                    node_id,
                    ok,
                    cached,
                    elapsed_seconds,
                    error,
                    ..
                } if recorded.insert(node_id.clone()) => {
                    steps_page.total += 1;
                    // Listed after the recorded steps: on the page that reaches past them.
                    if offset > 0 || limit.is_some_and(|n| view.steps.len() >= n) {
                        continue;
                    }
                    view.steps.push(db::RunStep {
                        node_id,
                        status: match (cached, ok) {
                            (true, _) => StepStatus::Cached,
                            (_, true) => StepStatus::Success,
                            _ => StepStatus::Failed,
                        }
                        .as_str()
                        .into(),
                        elapsed_seconds,
                        error,
                        ..Default::default()
                    })
                }
                _ => {}
            }
        }
    }
    steps_page.truncated = view.steps.len() < steps_page.total;
    let listed_to = offset + view.steps.len();
    steps_page.next_offset = (limit.is_some() && listed_to < steps_page.total).then_some(listed_to);
    // The steps streamed and not yet recorded get their trace ids as the recorded ones did.
    if record.is_some() && db::get_run_traced(read_path, &view.run_id).await? {
        db::attach_trace_ids(
            &mut view.steps,
            view.parent_run_id.as_deref().unwrap_or(&view.run_id),
        );
    }
    let worker_failures = barca_db::db::runs::get_worker_failures(read_path, id).await?;
    Ok(Some(RunDraft {
        worker_failures,
        view,
        record,
        steps_page,
        read_path: read_path.to_string(),
        unfinished: live.is_some_and(|run| run.outcome.is_none()),
    }))
}

impl RunDraft {
    /// The run's row when its planned nodes are to be counted: a recorded, finished run that
    /// recorded its job (#758). A live run streams its steps instead. The host gives
    /// [`RunDraft::plan`] the graph that plans it: a server its deployed one, `barca history`
    /// the files the run ran.
    pub fn plans(&self) -> Option<&db::RunRecord> {
        self.record.as_ref().filter(|record| {
            !self.unfinished
                && !matches!(self.view.status.as_str(), "pending" | "running")
                && record.job.is_some()
        })
    }

    /// Count the run's planned nodes over `dag`, the skipped and cached ones included, when
    /// `dag` still plans what the run ran ([`crate::queries::planned_run_nodes`]).
    pub async fn plan(&mut self, dag: &crate::dag::Dag) -> Result<(), BarcaError> {
        let Some(record) = self.plans() else {
            return Ok(());
        };
        let Some(job) = &record.job else {
            return Ok(());
        };
        let targets: Vec<&str> = if job == "all" {
            Vec::new()
        } else {
            job.split(',').collect()
        };
        let recorded = if self.steps_page.nodes.is_empty() {
            db::run_node_summaries(&self.read_path, &record.run_id).await?
        } else {
            self.steps_page.nodes.clone()
        };
        if let Some(nodes) = crate::queries::planned_run_nodes(
            dag,
            &record.command,
            &targets,
            &self.view.status,
            record.steps_total,
            recorded,
        ) {
            self.steps_page.nodes = nodes;
        }
        Ok(())
    }

    /// The run in words and what its targets returned, over the steps and nodes read.
    pub fn finish(self) -> RunViewed {
        let RunDraft {
            mut view,
            record,
            steps_page,
            worker_failures,
            ..
        } = self;
        view.steps_page = steps_page;
        if view.error.is_none() {
            view.error = view.steps.iter().find_map(|step| step.error.clone());
        }
        view.explanation = crate::run_explanation::explain(
            crate::run_explanation::RunFacts {
                steps_failed: record.as_ref().and_then(|r| r.steps_failed),
                steps_skipped: record.as_ref().and_then(|r| r.steps_skipped),
                ..(&view).into()
            },
            &view.steps_page.nodes,
            &view.steps,
            crate::time::now_secs_f64(),
        );
        view.explanation.worker_failures = worker_failures;
        // What the targets returned, as rows: nothing prints the raw pointers (#1042).
        let outcome = view
            .outcome
            .as_ref()
            .and_then(|o| serde_json::from_str::<serde_json::Value>(o.text()).ok());
        view.explanation.outputs = crate::run_explanation::run_outputs(
            &view.status,
            outcome.as_ref(),
            &crate::run_explanation::output_targets(view.target.as_deref(), view.job.as_deref()),
            &view.steps,
        );
        RunViewed { view, record }
    }
}

/// [`crate::queries::RunDetail`] of `run_id` in this environment; a usage error when it is not recorded.
/// At most `limit` of its recorded steps (every one with `None`). A finished run's planned
/// nodes, the skipped and cached ones included, are counted when the files it ran still plan
/// what it ran, as a server counts them ([`crate::queries::planned_run_nodes`]).
pub async fn run_detail(
    cfg: &crate::config::ResolvedConfig,
    run_id: &str,
    limit: Option<usize>,
    python: &std::path::Path,
) -> Result<crate::queries::RunDetail, BarcaError> {
    // In place: reading a run neither waits for another's writes nor makes one wait.
    let read = crate::db::read_metadata(cfg, true).await?;
    // The one definition of a run's detail ([`run_view`]), with no server's
    // overlay: what `GET /runs/{run_id}` answers for a run only the database knows.
    let draft = run_view(RunViewRequest {
        read_path: read.path(),
        db_path: &cfg.db_path,
        run_id,
        limit,
        offset: 0,
        live: None,
        events: None,
        default_files: &[],
        logs: false,
    })
    .await?;
    let Some(mut draft) = draft else {
        return Err(BarcaError::Usage(format!(
            "run '{run_id}' was not found; use barca history --json to find a run id"
        )));
    };
    // Planned over the files the run ran: no graph is deployed here.
    if let Some(files) = draft.plans().map(|run| run.files.clone())
        && !files.is_empty()
        && let Ok(dag) = crate::load::build_dag(&files, python).await
    {
        draft.plan(&dag).await?;
    }
    let RunViewed { view, record } = draft.finish();
    Ok(crate::queries::RunDetail {
        run: record.expect("a run read without an overlay is a recorded one"),
        steps: view.steps,
        steps_page: view.steps_page,
        outputs: view.explanation.outputs,
        worker_failures: view.explanation.worker_failures,
    })
}

/// The words `RunView.status` and a run list row's `status` take: live while `pending` or
/// `running`, terminal otherwise. The fields themselves are strings (a row recorded by another
/// release is served as it is); this is their vocabulary, for a reader that switches on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RunViewStatus {
    Skipped,
    Pending,
    Running,
    Success,
    Failed,
    Cancelled,
    Error,
    Interrupted,
}

/// `POST /runs` — `202`: the database run id, and whether the run started or waits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunHandle {
    /// Its place in the queue, 1 = next; `null` when it started.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub position: Option<usize>,
    /// `last` when the request asked to queue last; `null` otherwise. Experimental.
    #[serde(default)]
    pub queue: Option<crate::outcome::QueuePlace>,
    pub run_id: String,
    /// `running` when it started at once, `pending` while it waits in the queue.
    pub status: RunViewStatus,
    /// What else a waiting run waits for: `restart` while the server drains. Experimental.
    #[serde(default)]
    pub waiting_for: Option<WaitingFor>,
}

/// `DELETE /runs/{run_id}` of a run in progress: it was told to stop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunCancelling {
    pub run_id: String,
    /// `cancelled` after a queued cancellation completes; `cancelling` while a run stops.
    pub status: Cancelling,
}

/// The status of a cancellation request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Cancelling {
    #[default]
    Cancelling,
    Cancelled,
}

impl RunCancelling {
    pub fn cancelled(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            status: Cancelling::Cancelled,
        }
    }

    pub fn of(run_id: impl Into<String>) -> Self {
        RunCancelling {
            run_id: run_id.into(),
            status: Cancelling::Cancelling,
        }
    }
}

/// `GET /runs/{run_id}/logs` — the run's recorded lines and where the run is.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunLogs {
    pub logs: Vec<db::LogEntry>,
    pub run_id: String,
    pub status: String,
}

// ─── the run list ───────────────────────────────────────────────────────────

/// One run of `GET /runs`.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunSummary {
    pub control: RunControl,
    pub id: String,
    pub handle: Option<String>,
    pub run_id: Option<String>,
    pub command: String,
    pub files: Vec<String>,
    pub target: Option<String>,
    pub status: String,
    pub triggered_by: Option<TriggeredBy>,
    pub scheduled_at: Option<String>,
    pub schedule_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub steps_total: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_executed: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_cached: i64,
    /// Recorded steps that failed, a partitioned node's keys each counted (#758); absent for
    /// a run not yet recorded. Experimental.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null", optional))]
    pub steps_failed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null", optional))]
    pub steps_skipped: Option<i64>,
    pub error: Option<String>,
    /// The run's first failed step and its error; `null` when no step failed.
    pub first_failure: Option<FirstFailure>,
    /// Who asked for the run (`token:<name>`), when the server knows. Experimental.
    pub principal: Option<String>,
    /// The free-text `reason` its request gave. Experimental.
    pub trigger_reason: Option<String>,
    /// For a cancelled run: who cancelled it and whether it had started. Experimental.
    pub cancellation: Option<Cancellation>,
    /// With `?status=running` only: the nodes the run is working on now.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub active_steps: Option<Vec<ActiveStep>>,
    /// With `?status=running` only: `active_steps` as a run's panel lists them, the nodes
    /// executing (furthest from done first), how many wait, and the same by group.
    /// Experimental.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub now: Option<crate::run_list::RunNow>,
    /// The run in the words of a list row: its steps by outcome and what went wrong, the
    /// `line` of `barca history --json`. Experimental.
    pub line: crate::run_list::RunLine,
}

impl RunSummary {
    /// `line` from the row's own fields: set once the row is complete.
    fn worded(mut self) -> Self {
        self.line = crate::run_list::run_line(crate::run_list::RunLineFacts {
            status: &self.status,
            steps_total: self.steps_total,
            steps_executed: self.steps_executed,
            steps_cached: self.steps_cached,
            steps_failed: self.steps_failed,
            steps_skipped: self.steps_skipped,
            error: self.error.as_deref(),
            first_failure: self
                .first_failure
                .as_ref()
                .map(|f| (f.node_id.as_str(), f.error.as_str())),
            target: self.target.as_deref(),
        });
        self
    }

    /// The row of a recorded run, with what the server holding it (`live`) knows of it.
    fn recorded(record: db::RunRecord, live: Option<&LiveRun>) -> Self {
        RunSummary {
            control: match live {
                Some(run) => RunControl::server(run.status.as_str()),
                None if matches!(record.status.as_str(), "pending" | "running") => {
                    RunControl::elsewhere()
                }
                None => RunControl::server(&record.status),
            },
            id: record.run_id.clone(),
            handle: live.map(|r| r.handle.clone()),
            run_id: Some(record.run_id),
            command: record.command,
            files: record.files,
            target: record.target,
            status: live.map_or(record.status, |r| r.status.as_str().into()),
            triggered_by: record.triggered_by,
            scheduled_at: record.scheduled_at,
            schedule_id: record.schedule_id,
            started_at: durable_timestamp(&record.started_at),
            finished_at: record.finished_at.map(|t| durable_timestamp(&t)),
            elapsed_seconds: record.elapsed_seconds,
            steps_total: record.steps_total,
            steps_executed: record.steps_executed,
            steps_cached: record.steps_cached,
            steps_failed: record.steps_failed,
            steps_skipped: record.steps_skipped,
            error: live.and_then(|r| r.error.clone()),
            first_failure: record.failure.map(|f| FirstFailure {
                node_id: f.node,
                error: f.error,
            }),
            principal: record.principal,
            trigger_reason: record.trigger_reason,
            cancellation: record
                .cancellation
                .or_else(|| live.and_then(|r| r.cancellation.clone())),
            active_steps: None,
            now: None,
            line: Default::default(),
        }
        .worded()
    }

    /// The row of a run a server accepted and has not recorded: everything is the overlay's.
    fn unrecorded(run: &LiveRun, files: &[String]) -> Self {
        let steps = run.steps;
        RunSummary {
            control: RunControl::server(run.status.as_str()),
            id: run.handle.clone(),
            handle: Some(run.handle.clone()),
            run_id: run.db_run_id.clone(),
            command: run.command.clone(),
            files: files.to_vec(),
            target: run.target.clone(),
            status: run.status.as_str().into(),
            triggered_by: Some(run.trigger.triggered_by),
            scheduled_at: run.trigger.scheduled_at.clone(),
            schedule_id: run.trigger.schedule_id.clone(),
            started_at: timestamp(run.started_at),
            finished_at: run.finished_at.map(timestamp),
            elapsed_seconds: run.finished_at.map(|t| t - run.started_at),
            steps_total: steps.map(|s| s.total),
            steps_executed: steps.map_or(0, |s| s.executed),
            steps_cached: steps.map_or(0, |s| s.cached),
            steps_failed: None,
            steps_skipped: None,
            error: run.error.clone(),
            // Not in the database yet: nothing has recorded a failed step.
            first_failure: None,
            principal: run.trigger.principal.clone(),
            trigger_reason: run.trigger.reason.clone(),
            cancellation: run.cancellation.clone(),
            active_steps: None,
            now: None,
            line: Default::default(),
        }
        .worded()
    }

    /// What a search looks in for a run the database is not asked about: its ids and target.
    fn searched(&self) -> Vec<&str> {
        [
            Some(self.id.as_str()),
            self.run_id.as_deref(),
            self.target.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn in_progress(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "running")
    }
}

/// A run's first failed step (`RunSummary::first_failure`).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct FirstFailure {
    /// The step's node id, with its partition key when it has one.
    pub node_id: String,
    /// `ErrorType: message`, the message's first line only.
    pub error: String,
}

/// The keys of a partitioned node, while a run works on it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionProgress {
    /// Keys not done yet while the node's step is running; 0 while it waits.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub running: usize,
    /// Keys with a result: cached, or finished by this run.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub done: usize,
    /// Every key of the node.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
}

/// A node an in-flight run is working on (`GET /runs?status=running`).
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActiveStep {
    pub node_id: String,
    pub step: RunningStep,
    pub partitions: Option<PartitionProgress>,
}

/// `GET /runs`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunList {
    pub runs: Vec<RunSummary>,
    /// How many runs there are in all; with a filter (`status`, `triggered_by`,
    /// `not_triggered_by`, `since`, `until`, `search`), how many it keeps.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    pub truncated: bool,
    /// The runs started in the last 24 hours, hour by hour, by status and by what started
    /// them, with each hour's median time and what most of its unsuccessful runs say went
    /// wrong. Counted over every recorded run of the window (not the runs listed) that the
    /// filters keep, whatever `since` and `until` say: `barca history --activity --json`.
    /// Experimental.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub activity: Option<crate::run_list::RunActivity>,
}

/// The query of `GET /runs`: the flags of `barca history`, by name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunsQuery {
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub limit: Option<usize>,
    /// Only the runs with one of these statuses (comma-separated), as `barca history
    /// --status`; `not_successful` is every status of a run that ended without succeeding.
    /// `running` alone: the runs in flight (pending or running), this server's and the
    /// CLI's, each with what it is working on now.
    pub status: Option<String>,
    /// Only the runs started this way (comma-separated: `manual`, `api`, `schedule`,
    /// `client`), as `barca history --triggered-by`.
    pub triggered_by: Option<String>,
    /// Without the runs started this way, as `barca history --not-triggered-by`: `schedule`
    /// hides the schedule's runs and keeps the ones that recorded no trigger.
    pub not_triggered_by: Option<String>,
    /// Only the runs started at or after this, as `barca history --since`.
    pub since: Option<String>,
    /// Only the runs started before this, as `barca history --until`.
    pub until: Option<String>,
    /// Only the runs whose run id, target or job holds this text, as `barca history
    /// --search`.
    pub search: Option<String>,
}

impl RunsQuery {
    /// The filters, checked as `barca history` checks its flags. An error names the query
    /// parameter, not the flag.
    pub fn filter(&self) -> Result<HistoryFilter, String> {
        HistoryFilter::parse(
            crate::history_filter::HistoryFlags {
                status: self.status.as_deref(),
                target: None,
                triggered_by: self.triggered_by.as_deref(),
                not_triggered_by: self.not_triggered_by.as_deref(),
                since: self.since.as_deref(),
                until: self.until.as_deref(),
                search: self.search.as_deref(),
            },
            chrono::Utc::now(),
        )
        .map_err(|e| {
            [
                "not-triggered-by",
                "triggered-by",
                "status",
                "since",
                "until",
            ]
            .iter()
            .fold(e, |e, flag| {
                e.replace(&format!("--{flag}"), &flag.replace('-', "_"))
            })
        })
    }
}

/// What [`run_list`] reads.
pub struct RunListRequest<'a> {
    /// The metadata database to read: the real one, or a snapshot of it.
    pub read_path: &'a str,
    /// The real metadata database: the owners of its running rows are judged beside it.
    pub db_path: &'a str,
    /// At most this many runs, the runs in progress aside.
    pub limit: usize,
    pub filter: &'a HistoryFilter,
    /// The runs the answering server holds; none where no server does.
    pub live: &'a [LiveRun],
    /// The files of a live run not recorded yet: the deployed set.
    pub default_files: &'a [String],
    pub now: chrono::DateTime<chrono::Utc>,
}

/// The run list: the newest `limit` runs `filter` keeps ([`crate::queries::history_at`]),
/// joined with the overlay: the runs in progress first, whatever `limit` lists (#941), then
/// the newest. `total` counts the runs the filter keeps, and `activity` the last 24 hours.
pub async fn run_list(req: RunListRequest<'_>) -> Result<RunList, BarcaError> {
    let RunListRequest {
        read_path,
        limit,
        filter,
        live,
        ..
    } = req;
    // Counted by the database over the window, whatever `limit` lists (#991).
    let activity = crate::queries::activity_at(read_path, filter, req.now).await?;
    let by_id: HashMap<_, _> = live.iter().map(|r| (r.handle.as_str(), r)).collect();
    let (records, durable_total) =
        crate::queries::history_at(read_path, Some(limit), filter).await?;
    let mut runs: Vec<_> = records
        .into_iter()
        .map(|r| {
            let handle = by_id.get(r.run_id.as_str()).copied();
            RunSummary::recorded(r, handle)
        })
        .collect();
    let mut extra = 0;
    for run in live {
        if runs.iter().any(|r| r.id == run.handle) {
            continue;
        }
        // A finished run that wrote its row is the database's to list, and counted there:
        // nothing to add, so nothing to look up. A server remembers every run of the last
        // hour, and one lookup for each made a list of 50 take 0.8 s beside 2,000 of them.
        if !run.status.in_progress() && run.db_run_id.is_some() {
            continue;
        }
        match db::get_run(read_path, &run.handle).await? {
            // An older run than the page holds: listed while it is in progress (#941), and
            // already counted.
            Some(mut record) => {
                record.status = "running".into();
                if run.status.in_progress() && filter.matches(&record) {
                    runs.push(RunSummary::recorded(record, Some(run)));
                }
            }
            None => {
                let row = RunSummary::unrecorded(run, req.default_files);
                if filter.keeps_unrecorded(
                    &row.status,
                    row.triggered_by,
                    &row.started_at,
                    &row.searched(),
                ) {
                    runs.push(row);
                    extra += 1;
                }
            }
        }
    }
    // A run of another process on the same `.barca/` whose owner is alive, older than the
    // page holds.
    for alive in db::alive_running_runs_in(read_path, req.db_path).await {
        if runs.iter().any(|r| r.id == alive.run_id) {
            continue;
        }
        if let Some(mut record) = db::get_run(read_path, &alive.run_id).await? {
            record.status = "running".into();
            if filter.matches(&record) {
                runs.push(RunSummary::recorded(record, None));
            }
        }
    }
    // Stable: runs started in the same second stay as the database listed them, the one
    // recorded last first (a run id says nothing of the order).
    runs.sort_by(|a, b| {
        (!a.in_progress())
            .cmp(&!b.in_progress())
            .then_with(|| b.started_at.cmp(&a.started_at))
    });
    // Never fewer than are listed: a run recorded after the count was found by its own
    // lookup above, as a row the count did not see.
    let total = (durable_total + extra).max(runs.len());
    let in_progress = runs.iter().filter(|r| r.in_progress()).count();
    runs.truncate(limit.max(in_progress));
    Ok(RunList {
        truncated: total > runs.len(),
        runs,
        total,
        activity: Some(activity),
    })
}

/// A run in flight, for [`in_flight`]: the host found it (a server's own, or a row whose owner
/// is alive) and says which nodes it is working on now.
pub struct Flight<'a> {
    pub run_id: &'a str,
    /// The run as the answering server holds it; `None` for another process's run.
    pub live: Option<&'a LiveRun>,
    /// The nodes it is working on, in graph order.
    pub active: Vec<ActiveStep>,
}

/// The runs in flight, newest first, each with the nodes it is working on now and the same in
/// a panel's words (`now`): the rows of `GET /runs?status=running`. A flight neither the
/// overlay nor the database at `read_path` knows is left out.
pub async fn in_flight(
    read_path: &str,
    flights: Vec<Flight<'_>>,
    default_files: &[String],
    groups: &[crate::groups::NodeGroup],
) -> Result<Vec<RunSummary>, BarcaError> {
    let mut runs = Vec::with_capacity(flights.len());
    for flight in flights {
        let mut row = match flight.live {
            Some(live) => RunSummary::unrecorded(live, default_files),
            None => match db::get_run(read_path, flight.run_id).await? {
                Some(record) => {
                    let mut row = RunSummary::recorded(record, None);
                    // The row's owner diagnosis is not this machine's: the owner was found
                    // alive.
                    row.status = "running".into();
                    row.worded()
                }
                None => continue,
            },
        };
        let facts: Vec<_> = flight
            .active
            .iter()
            .map(|a| crate::run_list::ActiveNodeFacts {
                node_id: &a.node_id,
                running: a.step == RunningStep::Running,
                keys: a.partitions.as_ref().map(|p| (p.done, p.total)),
            })
            .collect();
        row.now = Some(crate::run_list::run_now(&facts, groups));
        row.active_steps = Some(flight.active);
        runs.push(row);
    }
    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(runs)
}

/// `GET /runs?status=running`: the rows of [`in_flight`] that `filter` keeps, at most `limit`.
pub async fn in_flight_list(
    read_path: &str,
    rows: Vec<RunSummary>,
    filter: &HistoryFilter,
    limit: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<RunList, BarcaError> {
    let activity = crate::queries::activity_at(read_path, filter, now).await?;
    let mut runs: Vec<RunSummary> = rows
        .into_iter()
        .filter(|run| {
            filter.keeps_unrecorded(
                "running",
                run.triggered_by,
                &run.started_at,
                &run.searched(),
            )
        })
        .collect();
    let total = runs.len();
    runs.truncate(limit);
    Ok(RunList {
        truncated: total > runs.len(),
        runs,
        total,
        activity: Some(activity),
    })
}

/// What each run of `rows` ([`in_flight`]) is working on now, by run id (a server run under
/// its handle and its durable id): `now` of `GET /history?status=running`, the same object as
/// on `GET /runs?status=running`.
pub fn in_flight_now(rows: Vec<RunSummary>) -> crate::report::HistoryNow {
    let mut out = crate::report::HistoryNow::new();
    for run in rows {
        let Some(now) = run.now else { continue };
        if let Some(durable) = run.run_id.filter(|id| *id != run.id) {
            out.insert(durable, now.clone());
        }
        out.insert(run.id, now);
    }
    out
}

#[cfg(test)]
mod run_list_tests {
    use super::*;

    fn remembered(handle: &str, status: LiveStatus, recorded: bool) -> LiveRun {
        LiveRun {
            handle: handle.into(),
            db_run_id: recorded.then(|| handle.into()),
            status,
            command: "get".into(),
            target: None,
            files: Vec::new(),
            trigger: Trigger::api(),
            started_at: 1.0,
            finished_at: (!status.in_progress()).then_some(2.0),
            outcome: None,
            error: None,
            cancellation: None,
            waiting_for: None,
            queue: Default::default(),
            steps: None,
        }
    }

    /// A server remembers the runs of the last hour. The list asks the database about one
    /// only when it can add a row: a run still going that the page does not hold, or a run
    /// that never wrote its row. One lookup for each finished run (0.4 ms) made
    /// `GET /runs?limit=50` take 0.8 s on a server that had finished 2,000 runs that hour.
    #[tokio::test]
    async fn a_finished_run_that_wrote_its_row_is_listed_from_the_database_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        db::init_db(&path).await.unwrap();
        for id in ["r1", "r2", "r3"] {
            db::create_run(&path, id, "get", "p.py", None, Some(1))
                .await
                .unwrap();
            db::finish_run(&path, id, "success", 1, 0, 0.1)
                .await
                .unwrap();
        }
        let live = [
            // In the database, and not on a page of two: counted there, not listed again.
            remembered("r1", LiveStatus::Complete, true),
            // Its row is gone (the trim deleted it): it is not listed again from memory.
            remembered("trimmed", LiveStatus::Complete, true),
            // Finished without a row: only the server knows it.
            remembered("unrecorded", LiveStatus::Failed, false),
            // Accepted and waiting: listed whatever the page holds.
            remembered("waiting", LiveStatus::Pending, false),
        ];
        let filter = HistoryFilter::default();
        let list = run_list(RunListRequest {
            read_path: &path,
            db_path: &path,
            limit: 2,
            filter: &filter,
            live: &live,
            default_files: &[],
            now: chrono::Utc::now(),
        })
        .await
        .unwrap();
        // Three recorded, and the two only the server knows.
        assert_eq!(list.total, 5);
        let ids: HashSet<&str> = list.runs.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains("waiting"), "{ids:?}");
        assert!(!ids.contains("trimmed"), "{ids:?}");
        assert!(list.truncated);
    }
}
