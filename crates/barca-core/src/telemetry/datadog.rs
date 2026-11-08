//! Datadog: one APM trace per step execution and one per run, sent to the local Datadog
//! Agent as each finishes, and `barca serve`'s lifecycle as DogStatsD metrics.
//!
//! An asset DAG has thousands of steps and partitions; a run of it is not one request but
//! thousands. So a step execution is the unit of a trace, as a request is for a web service,
//! and the run is a small trace of its own:
//!
//! ```text
//! barca.step       orders                 the step (root), one trace per step that ran or failed
//!   barca.execute  orders                 Python, from ddtrace in the worker (optional)
//!     library spans                       opted in with DD_PATCH_MODULES and friends
//!
//! barca.run        get daily_revenue      the run: status, counts, the first failure
//! ```
//!
//! Every span reports as one service, `DD_SERVICE` (default `barca`), with `type: worker` and
//! `span.kind: internal`. A partitioned node is one trace per key, the key in the tag
//! `barca.partition` and never in the resource, so a node with 20,000 keys is one resource
//! with 20,000 traces behind it. A cached or skipped step sends nothing; the run span counts
//! them. A run's traces share `barca.run_id`.
//!
//! A step's trace is sent when the step ends ([`Dispatch::step_finished`],
//! [`Dispatch::step_failed`]), in the background, so a failure is in Datadog seconds after it
//! happens and a killed run has sent every step that finished. The run's trace goes at the end
//! of the run ([`Integration::export`]), which also sends any step the live path did not. Each
//! goes to the Agent's trace intake as JSON over plain HTTP, the same place `ddtrace` sends
//! to, so nothing but the Agent is needed.
//!
//! Settings are Datadog's own environment variables:
//!
//! - `DD_TRACE_AGENT_URL` (`http://host:port` or `unix:///path/to/apm.socket`),
//!   else `DD_AGENT_HOST` (default `localhost`) and `DD_TRACE_AGENT_PORT`
//!   (default `8126`);
//! - `DD_TRACE_ENABLED`: when set to anything other than `true` or `1`, this integration is
//!   off, as with Python's `ddtrace`, so one environment file can turn tracing off for a
//!   whole stack;
//! - `DD_SERVICE` (default `barca`), `DD_ENV`, `DD_VERSION`;
//! - `DD_TAGS` (`key:value` pairs separated by commas or spaces), added to every span;
//! - DogStatsD for `barca serve` metrics: see [`super::dogstatsd`].

use super::dogstatsd;
use super::{Integration, NodeContext, RunReport, ServerReport, StepOutcome, StepReport};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The trace intake. It takes JSON (by `Content-Type`) as well as `ddtrace`'s msgpack.
const INTAKE_PATH: &str = "/v0.4/traces";
/// Error text longer than this is cut. An exception message can be arbitrarily large, and an
/// oversized payload makes the Agent refuse the trace, which loses the failed step.
const MAX_MESSAGE_CHARS: usize = 2000;
const MAX_STACK_CHARS: usize = 4000;
/// barca's own version, the `barca.version` tag of every span and metric.
pub(crate) const BARCA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The first `max_chars` characters of `text`, marked when something was dropped.
fn cut(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}… [cut]", &text[..end]),
        None => text.to_string(),
    }
}

/// The last `max_chars` characters of `text`: a traceback ends with the frame that raised.
fn cut_keeping_the_end(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let start = text
        .char_indices()
        .nth(total - max_chars)
        .map_or(0, |(i, _)| i);
    format!("[cut] …{}", &text[start..])
}

/// Where the Agent's trace intake listens.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Agent {
    Tcp { host: String, port: u16 },
    Unix { path: String },
}

#[derive(Debug, Clone)]
pub struct Datadog {
    agent: Agent,
    service: String,
    env: Option<String>,
    version: Option<String>,
    tags: Vec<(String, String)>,
    /// `barca.project`: the name of the project root directory.
    project: Option<String>,
    /// Where `barca serve` metrics go, or why that is not configured right.
    statsd: Result<dogstatsd::Target, String>,
}

pub(crate) fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The Agent address from `DD_TRACE_AGENT_URL`, or from host and port.
fn agent_from(url: Option<&str>, host: Option<&str>, port: Option<&str>) -> Result<Agent, String> {
    let parse_port = |raw: &str| {
        raw.parse::<u16>()
            .map_err(|_| "Agent port is not a port number".to_string())
    };
    if let Some(url) = url {
        if let Some(path) = url.strip_prefix("unix://") {
            if path.is_empty() {
                return Err("DD_TRACE_AGENT_URL=unix:// names no socket path".to_string());
            }
            return Ok(Agent::Unix {
                path: path.to_string(),
            });
        }
        if !url
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
        {
            return Err(
                "DD_TRACE_AGENT_URL is not supported: use http://host:port or unix:///path"
                    .to_string(),
            );
        }
        let authority = url[7..].split(['/', '?', '#']).next().unwrap_or("");
        if authority.contains('@') {
            // Not echoed: it may hold a password.
            return Err("DD_TRACE_AGENT_URL must not contain credentials (user@host)".to_string());
        }
        // An IPv6 literal is written in brackets: http://[::1]:8126.
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            let Some((host, after)) = rest.split_once(']') else {
                return Err("DD_TRACE_AGENT_URL has an unclosed '['".to_string());
            };
            match after.strip_prefix(':') {
                Some(p) => (host, parse_port(p)?),
                None if after.is_empty() => (host, 8126),
                None => {
                    return Err("DD_TRACE_AGENT_URL has unexpected text after ']'".to_string());
                }
            }
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, parse_port(p)?),
                None => (authority, 8126),
            }
        };
        if host.is_empty() {
            return Err("DD_TRACE_AGENT_URL names no host".to_string());
        }
        return Ok(Agent::Tcp {
            host: host.to_string(),
            port,
        });
    }
    Ok(Agent::Tcp {
        host: host.unwrap_or("localhost").to_string(),
        port: port.map(parse_port).transpose()?.unwrap_or(8126),
    })
}

/// `DD_TAGS`: `key:value` pairs separated by commas or spaces. A tag with no value is skipped.
pub(crate) fn parse_tags(raw: &str) -> Vec<(String, String)> {
    raw.split([',', ' '])
        .filter_map(|pair| pair.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

/// A non-zero 63-bit id derived from `parts`, so a run's ids are the same however often
/// it is exported.
pub(crate) fn span_id(parts: &[&str]) -> u64 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) >> 1).max(1)
}

/// `DD_TRACE_ENABLED`: on when unset, and when set only for `true` or `1` (any case). This is
/// what Python's `ddtrace` does, so `false`, `0`, `no`, `off` and an empty value all silence
/// barca together with the services around it.
fn tracing_enabled(raw: Option<&str>) -> bool {
    raw.is_none_or(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

/// A non-zero 64-bit id derived from `parts`: the upper half of a 128-bit trace id.
fn wide_id(parts: &[&str]) -> u64 {
    (span_id(parts) << 1 | 1).max(1)
}

/// The lower 64 bits of a run's trace id: the `trace_id` of its `barca.run` span, and the
/// `dd.trace_id` of a `BARCA_LOG=json` line with no node.
pub(crate) fn trace_id(run_id: &str) -> u64 {
    span_id(&["trace", run_id])
}

/// The upper 64 bits of a run's 128-bit trace id, sent as `_dd.p.tid` (16 hex digits).
pub(crate) fn trace_id_high(run_id: &str) -> u64 {
    wide_id(&["trace-high", run_id])
}

/// The id of a run's `barca.run` span.
pub(crate) fn run_span_id(run_id: &str) -> u64 {
    span_id(&["run", run_id])
}

/// The lower 64 bits of the trace id of the step `node` (a node id, with its partition key
/// when it has one) in a run: deterministic in the run and the step, so a worker's Python
/// spans, the step's log lines and the coordinator's root span agree without a message.
pub(crate) fn step_trace_id(run_id: &str, node: &str) -> u64 {
    span_id(&["step-trace", run_id, node])
}

/// The upper 64 bits of a step's 128-bit trace id.
pub(crate) fn step_trace_id_high(run_id: &str, node: &str) -> u64 {
    wide_id(&["step-trace-high", run_id, node])
}

/// The id of the `barca.step` span of `node` in a run: the root of the step's trace.
pub(crate) fn step_span_id(run_id: &str, node: &str) -> u64 {
    span_id(&["step", run_id, node])
}

/// The ids of the trace the step `node` (its display id) of a run was sent as, as Datadog's
/// UI writes them: the 128-bit trace id in 32 hex digits (what `https://app.<site>/apm/trace/`
/// takes) and the root `barca.step` span's id in decimal. Derived, never stored: a run's row
/// records only that it was traced (`runs.trace`), so these must stay the ids the run sent.
pub fn step_trace_ref(run_id: &str, node: &str) -> (String, String) {
    (
        format!(
            "{:016x}{:016x}",
            step_trace_id_high(run_id, node),
            step_trace_id(run_id, node)
        ),
        step_span_id(run_id, node).to_string(),
    )
}

/// The Datadog web app for `site` (`DD_SITE`, `[telemetry] datadog_site`): `app.` before a
/// two-label site (`datadoghq.com`, `datadoghq.eu`, `ddog-gov.com`), a regional site
/// (`us3.datadoghq.com`, `ap1.datadoghq.com`) as it is. `None` for a value that is not a host
/// name.
pub fn app_url(site: &str) -> Option<String> {
    let site = site.trim().trim_end_matches('/').to_ascii_lowercase();
    let host_like = !site.is_empty()
        && site.contains('.')
        && site.split('.').all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !host_like {
        return None;
    }
    if site.starts_with("app.") || site.split('.').count() > 2 {
        Some(format!("https://{site}"))
    } else {
        Some(format!("https://app.{site}"))
    }
}

/// The trace and span a `BARCA_LOG=json` line belongs to, as `(trace_id, span_id)`: the
/// step's own trace and its root span for a line about `node`; the run's for a line with no
/// node.
pub(crate) fn log_ids(run_id: &str, node: Option<&str>) -> (u64, u64) {
    match node {
        Some(node) => (step_trace_id(run_id, node), step_span_id(run_id, node)),
        None => (trace_id(run_id), run_span_id(run_id)),
    }
}

/// The steps of a run whose traces the live path sent (or is sending), so the end-of-run
/// export sends only the run's own trace and the steps nobody sent.
#[derive(Default)]
struct Sent {
    steps: HashSet<String>,
    deliveries: Vec<tokio::task::JoinHandle<()>>,
}

fn sent() -> &'static Mutex<HashMap<String, Sent>> {
    static SENT: OnceLock<Mutex<HashMap<String, Sent>>> = OnceLock::new();
    SENT.get_or_init(Default::default)
}

fn note_sent(run_id: &str, node_id: String, delivery: tokio::task::JoinHandle<()>) {
    if let Ok(mut all) = sent().lock() {
        let run = all.entry(run_id.to_string()).or_default();
        run.steps.insert(node_id);
        run.deliveries.push(delivery);
    }
}

/// What the live path sent for run `run_id`; forgets the run.
fn take_sent(run_id: &str) -> Sent {
    sent()
        .lock()
        .ok()
        .and_then(|mut all| all.remove(run_id))
        .unwrap_or_default()
}

/// The steps of `run` whose traces the end of the run sends: those that ran or failed and
/// are not in `sent`. A cached or skipped step has no trace.
fn to_send<'a>(
    run: &'a RunReport,
    sent: &'a HashSet<String>,
) -> impl Iterator<Item = &'a StepReport> {
    run.steps.iter().filter(move |s| {
        matches!(s.outcome, StepOutcome::Ran | StepOutcome::Failed) && !sent.contains(&s.node_id)
    })
}

/// `DD_SERVICE`, or `barca`: the one service every span reports as.
pub(crate) fn service_from_env() -> String {
    env_var("DD_SERVICE").unwrap_or_else(|| "barca".to_string())
}

/// One run's live exporter: it tells each worker the trace its `barca.execute` span joins,
/// and sends a step's trace the moment the step ends.
#[derive(Debug, Clone)]
pub(crate) struct Dispatch {
    run_id: String,
    service: String,
    project: Option<String>,
    job: String,
    nodes: HashMap<String, Arc<NodeContext>>,
    /// Each node's direct upstream node ids, sorted, by base id.
    upstream: HashMap<String, Vec<String>>,
    /// `None` when `DD_TRACE_ENABLED` switches tracing off or the settings are malformed;
    /// the configured integration has already said so.
    exporter: Option<Arc<Datadog>>,
}

impl Dispatch {
    pub fn new(dag: &crate::dag::Dag, job: &str, run_id: &str) -> Self {
        let upstream = dag
            .topo_order()
            .into_iter()
            .map(|id| {
                let mut ids: Vec<String> =
                    dag.upstream(id).into_iter().map(str::to_string).collect();
                ids.sort();
                ids.dedup();
                (id.to_string(), ids)
            })
            .collect();
        Self {
            run_id: run_id.to_string(),
            service: service_from_env(),
            project: project_from_cwd(),
            job: job.to_string(),
            nodes: super::node_contexts(dag)
                .into_iter()
                .map(|(id, ctx)| (id, Arc::new(ctx)))
                .collect(),
            upstream,
            exporter: Datadog::from_env().ok().flatten().map(Arc::new),
        }
    }

    fn node(&self, base: &str) -> Arc<NodeContext> {
        self.nodes
            .get(base)
            .cloned()
            .unwrap_or_else(|| Arc::new(NodeContext::unknown(base)))
    }

    /// The `datadog` object of a step sent to a worker: the step's trace and its root span.
    /// `reported` is the step whose trace the execution belongs in (itself, or a `parallel()`
    /// call's nearest reported ancestor).
    pub fn context(&self, reported: &crate::StepId, attempt: u32) -> Value {
        let base = reported.base_id();
        let display = reported.display();
        let node = self.node(base);
        let mut tags = Map::new();
        tags.insert("barca.run_id".into(), json!(self.run_id));
        tags.insert("barca.job".into(), json!(self.job));
        tags.insert("barca.node_id".into(), json!(display));
        node_meta(&mut tags, &node);
        if !reported.partition.is_empty() {
            tags.insert("barca.partition".into(), json!(reported.partition.suffix()));
        }
        tags.insert("barca.version".into(), json!(BARCA_VERSION));
        if let Some(project) = &self.project {
            tags.insert("barca.project".into(), json!(project));
        }
        json!({
            "trace_id": step_trace_id(&self.run_id, &display),
            "trace_id_high": step_trace_id_high(&self.run_id, &display),
            "parent_id": step_span_id(&self.run_id, &display),
            "service": self.service,
            "resource": node.name,
            "tags": tags,
            "attempt": attempt,
        })
    }

    fn report(&self, node_id: &str, outcome: StepOutcome, start: u64, duration: u64) -> StepReport {
        let id = crate::StepId::parse(node_id);
        StepReport {
            node_id: node_id.to_string(),
            node: self.node(id.base_id()),
            partition: (!id.partition.is_empty()).then(|| id.partition.suffix()),
            outcome,
            start_unix_ns: start,
            duration_ns: duration,
            attempts: None,
            run_hash: None,
            upstream: self.upstream.get(id.base_id()).cloned().unwrap_or_default(),
            size_bytes: None,
            digest: None,
            cpu_seconds: None,
            max_rss_bytes: None,
            error_type: None,
            error_message: None,
            error_traceback: None,
        }
    }

    /// A step ran: `artifact` is what its worker reported. Sends the step's trace.
    pub(crate) fn step_finished(
        &self,
        node_id: &str,
        artifact: &Value,
        attempts: u32,
        run_hash: &str,
    ) {
        self.send(self.finished_report(node_id, artifact, attempts, run_hash));
    }

    /// The report of a step that ran, from the artifact its worker reported.
    fn finished_report(
        &self,
        node_id: &str,
        artifact: &Value,
        attempts: u32,
        run_hash: &str,
    ) -> StepReport {
        let ns = |seconds: f64| (seconds.max(0.0) * 1e9) as u64;
        let number = |key: &str| artifact.get(key).and_then(|v| v.as_f64());
        let now = crate::time::unix_nanos(std::time::SystemTime::now()) as u64;
        // Placed by the worker's clock: the function and writing its result.
        let (start, duration) = match (number("finished_at"), number("wall_seconds")) {
            (Some(finished), Some(wall)) => (ns(finished - wall), ns(wall)),
            _ => {
                let wall = ns(number("elapsed_seconds").unwrap_or(0.0));
                (now.saturating_sub(wall), wall)
            }
        };
        let mut step = self.report(node_id, StepOutcome::Ran, start, duration);
        step.attempts = Some(attempts);
        step.run_hash = Some(run_hash.to_string());
        step.size_bytes = artifact.get("size_bytes").and_then(|v| v.as_u64());
        step.digest = artifact
            .get("content_hash")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        step.cpu_seconds = number("cpu_seconds");
        step.max_rss_bytes = artifact.get("max_rss_bytes").and_then(|v| v.as_u64());
        step
    }

    /// A step failed for good (its retries spent). Sends the step's trace.
    pub(crate) fn step_failed(&self, failure: &crate::dispatch::StepFailure) {
        self.send(self.failed_report(failure));
    }

    /// The report of a step that failed for good.
    fn failed_report(&self, failure: &crate::dispatch::StepFailure) -> StepReport {
        let (error_type, error_message, error_traceback) =
            crate::persist::exception_of(&failure.error);
        let now = crate::time::unix_nanos(std::time::SystemTime::now()) as u64;
        // From the first attempt's start to the failure, retries and backoff inside.
        let (start, duration) = match (failure.error.started_unix_ns, failure.error.elapsed_seconds)
        {
            (Some(started), Some(wall)) => (started, (wall.max(0.0) * 1e9) as u64),
            _ => (now, 0),
        };
        let mut step = self.report(&failure.node_id, StepOutcome::Failed, start, duration);
        step.attempts = Some(failure.error.attempts);
        step.error_type = Some(error_type);
        step.error_message = Some(error_message);
        step.error_traceback = error_traceback;
        step
    }

    /// Send `step`'s trace in the background. Without a runtime to run it on (a test's sync
    /// caller) nothing is sent here, and the end of the run sends it.
    fn send(&self, step: StepReport) {
        let Some(exporter) = self.exporter.clone() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let body = exporter.step_trace(&self.run_id, &self.job, &step);
        let what = format!("step {}", step.node_id);
        let delivery = runtime.spawn(async move {
            let result = tokio::time::timeout(super::EXPORT_TIMEOUT, exporter.send(&body))
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "no reply within {}s",
                        super::EXPORT_TIMEOUT.as_secs()
                    ))
                });
            super::delivered("datadog", &what, result);
        });
        note_sent(&self.run_id, step.node_id, delivery);
    }
}

/// The tags that place a node in the pipeline.
fn node_meta(meta: &mut Map<String, Value>, node: &NodeContext) {
    meta.insert("barca.asset".into(), json!(node.name));
    meta.insert("barca.file".into(), json!(node.file));
    meta.insert("barca.kind".into(), json!(node.kind));
    meta.insert("barca.freshness".into(), json!(node.freshness));
    if let Some(group) = &node.group {
        meta.insert("barca.group".into(), json!(group));
    }
    if let Some(cron) = &node.schedule {
        meta.insert("barca.schedule".into(), json!(cron));
    }
}

/// The name of the directory barca runs in: the project root, which the CLI changes into.
fn project_from_cwd() -> Option<String> {
    std::env::current_dir()
        .ok()?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// A span's fields before the tags every span shares are added.
struct Span {
    trace_id: u64,
    id: u64,
    parent: Option<u64>,
    name: &'static str,
    resource: String,
    start: u64,
    duration: u64,
    error: bool,
    meta: Map<String, Value>,
    metrics: Map<String, Value>,
}

/// Error tags for a failed step: the type, the message cut to its first characters, and the
/// end of the traceback.
fn error_meta(meta: &mut Map<String, Value>, step: &StepReport) {
    if let Some(t) = &step.error_type {
        meta.insert("error.type".into(), json!(t));
    }
    if let Some(m) = &step.error_message {
        meta.insert("error.message".into(), json!(cut(m, MAX_MESSAGE_CHARS)));
    }
    if let Some(trace) = &step.error_traceback {
        meta.insert(
            "error.stack".into(),
            json!(cut_keeping_the_end(trace, MAX_STACK_CHARS)),
        );
    }
}

/// Size, CPU and memory of a step.
fn cost_metrics(metrics: &mut Map<String, Value>, step: &StepReport) {
    if let Some(bytes) = step.size_bytes {
        metrics.insert("barca.bytes".into(), json!(bytes));
    }
    if let Some(cpu) = step.cpu_seconds {
        metrics.insert("barca.cpu_seconds".into(), json!(cpu));
    }
    if let Some(rss) = step.max_rss_bytes {
        metrics.insert("barca.max_rss_bytes".into(), json!(rss));
    }
    if let Some(attempts) = step.attempts {
        metrics.insert("barca.attempts".into(), json!(attempts));
    }
}

impl Datadog {
    /// Tags every span and metric carries, in a fixed order: `DD_TAGS`, `env`, `version`,
    /// `barca.version`, `barca.project`.
    fn deployment_tags(&self) -> Vec<(String, String)> {
        let mut tags = self.tags.clone();
        if let Some(env) = &self.env {
            tags.push(("env".to_string(), env.clone()));
        }
        if let Some(version) = &self.version {
            tags.push(("version".to_string(), version.clone()));
        }
        tags.push(("barca.version".to_string(), BARCA_VERSION.to_string()));
        if let Some(project) = &self.project {
            tags.push(("barca.project".to_string(), project.clone()));
        }
        tags
    }

    fn finish(&self, run_id: &str, span: Span) -> Value {
        let mut meta: Map<String, Value> = self
            .deployment_tags()
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect();
        meta.insert("component".into(), json!("barca"));
        meta.insert("span.kind".into(), json!("internal"));
        meta.insert("barca.run_id".into(), json!(run_id));
        meta.extend(span.meta);
        let mut value = json!({
            "trace_id": span.trace_id,
            "span_id": span.id,
            "name": span.name,
            "resource": span.resource,
            "service": self.service,
            "type": "worker",
            "start": span.start,
            "duration": span.duration,
            "error": i32::from(span.error),
            "meta": meta,
            "metrics": span.metrics,
        });
        if let Some(parent) = span.parent {
            value["parent_id"] = json!(parent);
        }
        value
    }

    /// `barca.run`: what ran, how it ended and, when it failed, which asset failed first.
    fn root_span(&self, run: &RunReport) -> Value {
        let subject = if !run.targets.is_empty() {
            run.targets.join(",")
        } else if !run.files.is_empty() {
            run.files.join(",")
        } else {
            "all".to_string()
        };
        let mut meta = Map::new();
        meta.insert("barca.command".into(), json!(run.command));
        meta.insert("barca.status".into(), json!(run.status));
        meta.insert("barca.job".into(), json!(run.job));
        if let Some(target) = &run.target {
            meta.insert("barca.target".into(), json!(target));
        }
        if !run.targets.is_empty() {
            meta.insert("barca.targets".into(), json!(run.targets.join(",")));
        }
        if !run.files.is_empty() {
            meta.insert("barca.files".into(), json!(run.files.join(",")));
        }
        if let Some(reason) = &run.reason {
            meta.insert("barca.reason".into(), json!(reason));
        }
        if let Some(handle) = &run.in_flight_run {
            meta.insert("barca.in_flight_run".into(), json!(handle));
        }
        // What started the run, and for a scheduled run the tick and the schedule (#393).
        meta.insert(
            "barca.trigger".into(),
            json!(run.trigger.triggered_by.as_str()),
        );
        if let Some(at) = &run.trigger.scheduled_at {
            meta.insert("barca.scheduled_at".into(), json!(at));
        }
        if let Some(id) = &run.trigger.schedule_id {
            meta.insert("barca.schedule_id".into(), json!(id));
        }
        // The trace list shows a root span's error: say which asset failed, and why.
        let first_failure = run
            .steps
            .iter()
            .filter(|s| s.outcome == StepOutcome::Failed)
            .min_by_key(|s| (s.start_unix_ns.saturating_add(s.duration_ns), &s.node_id));
        let error = !matches!(run.status.as_str(), "success" | "skipped");
        if let Some(step) = first_failure {
            let at = match &step.partition {
                Some(key) => format!("{}[{key}]", step.node.name),
                None => step.node.name.clone(),
            };
            meta.insert("barca.failed_asset".into(), json!(step.node.name));
            meta.insert("barca.failed_node_id".into(), json!(step.node_id));
            if let Some(t) = &step.error_type {
                meta.insert("error.type".into(), json!(t));
            }
            let message = step.error_message.as_deref().unwrap_or("failed");
            meta.insert(
                "error.message".into(),
                json!(cut(&format!("{at}: {message}"), MAX_MESSAGE_CHARS)),
            );
        } else if run.status == "cancelled" {
            meta.insert("error.type".into(), json!("Cancelled"));
            let why = run.reason.as_deref().unwrap_or("the run was cancelled");
            meta.insert("error.message".into(), json!(why));
        } else if error {
            meta.insert("error.type".into(), json!("RunFailed"));
            meta.insert(
                "error.message".into(),
                json!(format!(
                    "the run failed outside its steps; see `barca logs {}`",
                    run.run_id
                )),
            );
        }
        let count = |outcome| run.steps.iter().filter(|s| s.outcome == outcome).count();
        let mut metrics = Map::new();
        metrics.insert("barca.steps.total".into(), json!(run.steps_total));
        metrics.insert("barca.steps.executed".into(), json!(run.steps_executed));
        metrics.insert("barca.steps.cached".into(), json!(run.steps_cached));
        metrics.insert(
            "barca.steps.failed".into(),
            json!(count(StepOutcome::Failed)),
        );
        metrics.insert(
            "barca.steps.skipped".into(),
            json!(count(StepOutcome::Skipped)),
        );
        self.finish(
            &run.run_id,
            Span {
                trace_id: trace_id(&run.run_id),
                id: run_span_id(&run.run_id),
                parent: None,
                name: "barca.run",
                resource: format!("{} {subject}", run.command),
                start: run.start_unix_ns,
                duration: run.duration_ns,
                // A skipped tick is not an error: the run in flight brings the node up to date.
                error,
                meta,
                metrics,
            },
        )
    }

    /// `barca.step`, the root of a step's trace: the node, its place in the pipeline, how the
    /// step ended and what it cost. `job` is the run's `barca.job`.
    fn step_span(&self, run_id: &str, job: &str, step: &StepReport) -> Value {
        let node = &step.node;
        let mut meta = Map::new();
        meta.insert("barca.job".into(), json!(job));
        meta.insert("barca.node_id".into(), json!(step.node_id));
        node_meta(&mut meta, node);
        if let Some(partition) = &step.partition {
            meta.insert("barca.partition".into(), json!(partition));
        }
        meta.insert("barca.outcome".into(), json!(step.outcome.as_str()));
        if let Some(hash) = &step.run_hash {
            meta.insert("barca.run_hash".into(), json!(hash));
        }
        // Span tags are strings: the ids, comma-separated.
        if !step.upstream.is_empty() {
            meta.insert("barca.upstream".into(), json!(step.upstream.join(",")));
        }
        if let Some(digest) = &step.digest {
            meta.insert("barca.digest".into(), json!(digest));
        }
        error_meta(&mut meta, step);
        let mut metrics = Map::new();
        cost_metrics(&mut metrics, step);
        self.finish(
            run_id,
            Span {
                trace_id: step_trace_id(run_id, &step.node_id),
                id: step_span_id(run_id, &step.node_id),
                parent: None,
                name: "barca.step",
                resource: node.name.clone(),
                start: step.start_unix_ns,
                duration: step.duration_ns,
                error: step.outcome == StepOutcome::Failed,
                meta,
                metrics,
            },
        )
    }

    /// One trace as a request body, `[[root, ...]]`. The root carries what Datadog reads from
    /// a trace's first span: the sampling decision (keep: these are jobs, not sampled
    /// requests) and the trace id's upper half.
    fn trace_body(&self, mut spans: Vec<Value>, trace_id_high: u64) -> Vec<u8> {
        if let Some(root) = spans.first_mut() {
            root["meta"]["_dd.p.tid"] = json!(format!("{trace_id_high:016x}"));
            root["meta"]["_dd.p.dm"] = json!("-4");
            root["metrics"]["_sampling_priority_v1"] = json!(2);
        }
        serde_json::to_vec(&json!([spans])).unwrap_or_default()
    }

    /// The run's own trace: its `barca.run` span.
    pub(crate) fn run_trace(&self, run: &RunReport) -> Vec<u8> {
        self.trace_body(vec![self.root_span(run)], trace_id_high(&run.run_id))
    }

    /// A step's trace: its `barca.step` span (the worker's Python spans join it on their own).
    pub(crate) fn step_trace(&self, run_id: &str, job: &str, step: &StepReport) -> Vec<u8> {
        self.trace_body(
            vec![self.step_span(run_id, job, step)],
            step_trace_id_high(run_id, &step.node_id),
        )
    }

    /// The root span of a trace body (tests).
    #[cfg(test)]
    fn root_of(body: &[u8]) -> Value {
        let traces: Value = serde_json::from_slice(body).unwrap();
        traces[0][0].clone()
    }
}

impl Datadog {
    /// `Ok(None)` when `DD_TRACE_ENABLED` switches tracing off.
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        // Read raw: an empty value is a setting too, and it means off.
        let enabled = std::env::var("DD_TRACE_ENABLED").ok();
        if !tracing_enabled(enabled.as_deref().map(str::trim)) {
            return Ok(None);
        }
        Ok(Some(Self {
            agent: agent_from(
                env_var("DD_TRACE_AGENT_URL").as_deref(),
                env_var("DD_AGENT_HOST").as_deref(),
                env_var("DD_TRACE_AGENT_PORT").as_deref(),
            )?,
            service: service_from_env(),
            env: env_var("DD_ENV"),
            version: env_var("DD_VERSION"),
            tags: env_var("DD_TAGS")
                .map(|raw| parse_tags(&raw))
                .unwrap_or_default(),
            project: project_from_cwd(),
            statsd: dogstatsd::Target::from_env(),
        }))
    }
}

/// `PUT` the body to the Agent over an open connection and check it was accepted.
async fn put<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    host: &str,
    body: &[u8],
) -> Result<(), String> {
    let head = format!(
        "PUT {INTAKE_PATH} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nX-Datadog-Trace-Count: 1\r\nDatadog-Meta-Lang: rust\r\n\
         Datadog-Meta-Tracer-Version: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        env!("CARGO_PKG_VERSION")
    );
    let io = |e: std::io::Error| e.to_string();
    stream.write_all(head.as_bytes()).await.map_err(io)?;
    stream.write_all(body).await.map_err(io)?;
    stream.flush().await.map_err(io)?;

    let mut reply = Vec::new();
    let mut chunk = [0u8; 256];
    while !reply.contains(&b'\n') {
        let n = stream.read(&mut chunk).await.map_err(io)?;
        if n == 0 {
            break;
        }
        reply.extend_from_slice(&chunk[..n]);
    }
    let status_line = String::from_utf8_lossy(&reply);
    let status_line = status_line.lines().next().unwrap_or("").trim();
    match status_line.split_whitespace().nth(1) {
        Some(code) if code.starts_with('2') => Ok(()),
        Some(_) => Err(format!("the Agent answered '{status_line}'")),
        None => Err("the Agent closed the connection without answering".to_string()),
    }
}

impl Datadog {
    /// `barca serve`'s state as DogStatsD gauges, and an event when it starts or stops.
    fn server_packets(&self, server: &ServerReport) -> Vec<String> {
        use super::ServerPhase;
        let mut tags: Vec<(String, String)> = vec![("service".into(), self.service.clone())];
        tags.extend(self.deployment_tags());
        tags.push(("barca.read_only".into(), server.read_only.to_string()));
        tags.push(("barca.watch".into(), server.watch.to_string()));
        tags.push(("barca.scheduling".into(), server.scheduling.to_string()));
        let up = if server.phase == ServerPhase::Stop {
            0
        } else {
            1
        };
        let mut packets = vec![
            dogstatsd::gauge("barca.serve.up", up as f64, &tags),
            dogstatsd::gauge("barca.serve.schedules", server.schedules as f64, &tags),
            dogstatsd::gauge("barca.serve.files", server.files as f64, &tags),
        ];
        if let Some(nodes) = server.nodes {
            packets.push(dogstatsd::gauge("barca.serve.nodes", nodes as f64, &tags));
        }
        let what = match server.phase {
            ServerPhase::Start => Some("started"),
            ServerPhase::Stop => Some("stopped"),
            ServerPhase::Heartbeat => None,
        };
        if let Some(what) = what {
            let text = format!(
                "barca {BARCA_VERSION} serve {what}: {} files, {} schedules{}",
                server.files,
                server.schedules,
                if server.read_only { ", read-only" } else { "" }
            );
            packets.push(dogstatsd::event(
                &format!("barca serve {what}"),
                &text,
                &tags,
            ));
        }
        packets
    }

    async fn send(&self, body: &[u8]) -> Result<(), String> {
        match &self.agent {
            Agent::Tcp { host, port } => {
                let stream = tokio::net::TcpStream::connect((host.as_str(), *port))
                    .await
                    .map_err(|e| format!("cannot reach the Datadog Agent at {host}:{port}: {e}"))?;
                let authority = if host.contains(':') {
                    format!("[{host}]:{port}")
                } else {
                    format!("{host}:{port}")
                };
                put(stream, &authority, body).await
            }
            Agent::Unix { path } => {
                let stream = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(|e| format!("cannot reach the Datadog Agent at {path}: {e}"))?;
                put(stream, "localhost", body).await
            }
        }
    }
}

impl Integration for Datadog {
    fn export_server<'a>(&'a self, server: &'a ServerReport) -> super::Export<'a> {
        Box::pin(async move {
            let target = self.statsd.as_ref().map_err(Clone::clone)?;
            dogstatsd::send(target, &self.server_packets(server)).await
        })
    }

    /// The run's trace, then the trace of every step that ran or failed and that the live
    /// path did not send (a failure recorded without a worker, a run without a runtime).
    /// Cached and skipped steps send nothing.
    fn export<'a>(
        &'a self,
        run: &'a RunReport,
        _history: &'a super::JobHistory,
    ) -> super::Export<'a> {
        Box::pin(async move {
            let Sent { steps, deliveries } = take_sent(&run.run_id);
            for delivery in deliveries {
                let _ = delivery.await;
            }
            self.send(&self.run_trace(run)).await?;
            for step in to_send(run, &steps) {
                self.send(&self.step_trace(&run.run_id, &run.job, step))
                    .await?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
