//! Datadog: one APM trace per run, sent to the local Datadog Agent, and `barca serve`'s
//! lifecycle as DogStatsD metrics.
//!
//! A trace is the pipeline as it ran. Every span reports as one service, `DD_SERVICE`
//! (default `barca`), with `type: worker` and `span.kind: internal`:
//!
//! ```text
//! barca.run          get daily_revenue      the run (root)
//!   barca.step       orders                 one per node, resource = the node's short name
//!     barca.partition  orders               one per partition key that ran (capped) or failed
//!       barca.execute  orders               Python, from ddtrace in the worker (optional)
//!         library spans                     opted in with DD_PATCH_MODULES and friends
//! ```
//!
//! Partition keys are the tag `barca.partition`, never part of a resource, so a node with
//! 20,000 keys is still one resource. The trace goes to the Agent's trace intake as JSON over
//! plain HTTP, the same place `ddtrace` sends to, so nothing but the Agent is needed.
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
/// oversized payload makes the Agent refuse the whole trace, which loses the failed run.
const MAX_MESSAGE_CHARS: usize = 2000;
const MAX_STACK_CHARS: usize = 4000;
/// A node's failed partitions after this many carry a short message and no stack: a change
/// that breaks every key of a 20,000-key node fails them all the same way.
const DETAILED_FAILURES_PER_NODE: usize = 20;
const BRIEF_MESSAGE_CHARS: usize = 200;
/// Partition keys that ran get a `barca.partition` span (and a Python `barca.execute` span)
/// up to this many per node, and this many per run. A failed key always gets one. Past the
/// cap the node's `barca.step` span counts its keys (`barca.partitions.*`) instead. 200 keys
/// is a readable waterfall and about 150 KB of trace; 2,000 per run keeps the largest
/// payload (with failures) well inside the Agent's 25 MB request limit and the 3 seconds a
/// delivery may take.
pub const KEY_SPANS_PER_NODE: usize = 200;
pub const KEY_SPANS_PER_RUN: usize = 2_000;
/// One request to the Agent carries at most this much; a larger trace goes in several
/// requests (trace chunks with the same trace id, as a tracer's partial flush sends them).
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// barca's own version, the `barca.version` tag of every span and metric.
pub const BARCA_VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// The lower 64 bits of a run's trace id: the `trace_id` field of every span, and the
/// `dd.trace_id` of a `BARCA_LOG=json` line.
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

/// The id of the span of `node` in a run: the `barca.step` span for a node id without a
/// partition key, the `barca.partition` span for one with a key.
pub(crate) fn step_span_id(run_id: &str, node: &str) -> u64 {
    span_id(&["step", run_id, node])
}

/// The span a `BARCA_LOG=json` line about `node` belongs to: the key's `barca.partition`
/// span when it has one, else the node's `barca.step` span; the run's span for no node.
pub(crate) fn log_span_id(run_id: &str, node: Option<&str>) -> u64 {
    let Some(node) = node else {
        return run_span_id(run_id);
    };
    let id = crate::StepId::parse(node);
    if id.partition.is_empty() || has_key_span(run_id, node) {
        step_span_id(run_id, node)
    } else {
        step_span_id(run_id, id.base_id())
    }
}

/// Which partition keys of a run have a span of their own, decided when each is first sent
/// to a worker, so the worker's `barca.execute` span, the log lines and the trace agree.
#[derive(Default)]
struct KeySpans {
    total: usize,
    per_node: HashMap<String, usize>,
    keys: HashSet<String>,
}

fn key_spans() -> &'static Mutex<HashMap<String, KeySpans>> {
    static KEYS: OnceLock<Mutex<HashMap<String, KeySpans>>> = OnceLock::new();
    KEYS.get_or_init(Default::default)
}

/// Give partition `key` of node `base` a span in run `run_id` if the caps allow. True when
/// it has one (also when it was given one before: a retry or a `parallel()` child).
pub(crate) fn claim_key_span(run_id: &str, base: &str, key: &str) -> bool {
    let Ok(mut all) = key_spans().lock() else {
        return false;
    };
    let run = all.entry(run_id.to_string()).or_default();
    if run.keys.contains(key) {
        return true;
    }
    let count = run.per_node.entry(base.to_string()).or_default();
    if *count >= KEY_SPANS_PER_NODE || run.total >= KEY_SPANS_PER_RUN {
        return false;
    }
    *count += 1;
    run.total += 1;
    run.keys.insert(key.to_string());
    true
}

fn has_key_span(run_id: &str, key: &str) -> bool {
    key_spans()
        .lock()
        .is_ok_and(|all| all.get(run_id).is_some_and(|run| run.keys.contains(key)))
}

/// The keys of run `run_id` that have a span; forgets the run.
pub(crate) fn take_key_spans(run_id: &str) -> HashSet<String> {
    key_spans()
        .lock()
        .ok()
        .and_then(|mut all| all.remove(run_id))
        .map(|run| run.keys)
        .unwrap_or_default()
}

/// `DD_SERVICE`, or `barca`: the one service every span reports as.
pub fn service_from_env() -> String {
    env_var("DD_SERVICE").unwrap_or_else(|| "barca".to_string())
}

/// What the coordinator tells a worker so its `barca.execute` span joins the run's trace.
#[derive(Debug, Clone)]
pub struct Dispatch {
    service: String,
    project: Option<String>,
    job: String,
    nodes: HashMap<String, Arc<NodeContext>>,
}

impl Dispatch {
    pub fn new(dag: &crate::dag::Dag, job: &str) -> Self {
        Self {
            service: service_from_env(),
            project: project_from_cwd(),
            job: job.to_string(),
            nodes: super::node_contexts(dag)
                .into_iter()
                .map(|(id, ctx)| (id, Arc::new(ctx)))
                .collect(),
        }
    }

    /// The `datadog` object of a step sent to a worker. `reported` is the step whose span the
    /// execution belongs under (itself, or a `parallel()` call's nearest reported ancestor).
    /// `None` for a partition key past the cap: it gets no Python span.
    pub fn context(&self, run_id: &str, reported: &crate::StepId, attempt: u32) -> Option<Value> {
        let base = reported.base_id();
        let display = reported.display();
        let parent = if reported.partition.is_empty() {
            step_span_id(run_id, base)
        } else if claim_key_span(run_id, base, &display) {
            step_span_id(run_id, &display)
        } else {
            return None;
        };
        let node = self
            .nodes
            .get(base)
            .cloned()
            .unwrap_or_else(|| Arc::new(NodeContext::unknown(base)));
        let mut tags = Map::new();
        tags.insert("barca.run_id".into(), json!(run_id));
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
        Some(json!({
            "trace_id": trace_id(run_id),
            "trace_id_high": trace_id_high(run_id),
            "parent_id": parent,
            "service": self.service,
            "resource": node.name,
            "tags": tags,
            "attempt": attempt,
        }))
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

/// Error tags for a failed step: the type, the message cut to `message_chars`, and with
/// `stack` the end of the traceback.
fn error_meta(meta: &mut Map<String, Value>, step: &StepReport, message_chars: usize, stack: bool) {
    if let Some(t) = &step.error_type {
        meta.insert("error.type".into(), json!(t));
    }
    if let Some(m) = &step.error_message {
        meta.insert("error.message".into(), json!(cut(m, message_chars)));
    }
    if stack && let Some(trace) = &step.error_traceback {
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

/// The steps of a run grouped by node, in the order their first step appears.
fn by_node(steps: &[StepReport]) -> Vec<Vec<&StepReport>> {
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut groups: Vec<Vec<&StepReport>> = Vec::new();
    for step in steps {
        let at = *index.entry(step.node.base_id.as_str()).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[at].push(step);
    }
    groups
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

    fn finish(&self, run: &RunReport, span: Span) -> Value {
        let mut meta: Map<String, Value> = self
            .deployment_tags()
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect();
        meta.insert("component".into(), json!("barca"));
        meta.insert("span.kind".into(), json!("internal"));
        meta.insert("barca.run_id".into(), json!(run.run_id));
        meta.extend(span.meta);
        let mut value = json!({
            "trace_id": trace_id(&run.run_id),
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

    /// Every span of the run's trace, root first. `key_spans` are the partition keys that
    /// were given a span when they ran ([`claim_key_span`]).
    fn spans(&self, run: &RunReport, key_spans: &HashSet<String>) -> Vec<Value> {
        let root_id = run_span_id(&run.run_id);
        let mut spans = vec![self.root_span(run, root_id)];
        for steps in by_node(&run.steps) {
            if let [step] = steps.as_slice()
                && step.partition.is_none()
            {
                spans.push(self.step_span(run, root_id, step));
            } else {
                self.partitioned_spans(run, root_id, &steps, key_spans, &mut spans);
            }
        }
        spans
    }

    /// `barca.run`: what ran, how it ended and, when it failed, which asset failed first.
    fn root_span(&self, run: &RunReport, root_id: u64) -> Value {
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
            run,
            Span {
                id: root_id,
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

    /// The tags of a `barca.step` span that every node has.
    fn step_meta(&self, run: &RunReport, node: &NodeContext) -> Map<String, Value> {
        let mut meta = Map::new();
        meta.insert("barca.job".into(), json!(run.job));
        meta.insert("barca.node_id".into(), json!(node.base_id));
        node_meta(&mut meta, node);
        meta
    }

    /// `barca.step` for a node without partitions.
    fn step_span(&self, run: &RunReport, root_id: u64, step: &StepReport) -> Value {
        let mut meta = self.step_meta(run, &step.node);
        meta.insert("barca.node_id".into(), json!(step.node_id));
        meta.insert("barca.outcome".into(), json!(step.outcome.as_str()));
        meta.insert(
            "barca.cached".into(),
            json!((step.outcome == StepOutcome::Cached).to_string()),
        );
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
        error_meta(&mut meta, step, MAX_MESSAGE_CHARS, true);
        let mut metrics = Map::new();
        // Ask Datadog to compute trace metrics (hits, errors, duration) for step spans
        // too; by default only a service's entry span gets them.
        metrics.insert("_dd.measured".into(), json!(1));
        cost_metrics(&mut metrics, step);
        self.finish(
            run,
            Span {
                id: step_span_id(&run.run_id, &step.node.base_id),
                parent: Some(root_id),
                name: "barca.step",
                resource: step.node.name.clone(),
                start: step.start_unix_ns,
                duration: step.duration_ns,
                error: step.outcome == StepOutcome::Failed,
                meta,
                metrics,
            },
        )
    }

    /// One `barca.step` for a partitioned node, summing its keys, and a `barca.partition`
    /// child for each key in `key_spans` and each key that failed.
    fn partitioned_spans(
        &self,
        run: &RunReport,
        root_id: u64,
        keys: &[&StepReport],
        key_spans: &HashSet<String>,
        out: &mut Vec<Value>,
    ) {
        let node = &keys[0].node;
        let step_id = step_span_id(&run.run_id, &node.base_id);
        let failed: Vec<&StepReport> = keys
            .iter()
            .copied()
            .filter(|k| k.outcome == StepOutcome::Failed)
            .collect();
        let count = |outcome| keys.iter().filter(|k| k.outcome == outcome).count();
        let (ran, cached) = (count(StepOutcome::Ran), count(StepOutcome::Cached));
        let outcome = if !failed.is_empty() {
            StepOutcome::Failed
        } else if ran > 0 {
            StepOutcome::Ran
        } else if cached > 0 {
            StepOutcome::Cached
        } else {
            StepOutcome::Skipped
        };
        // The span covers the keys that did work; cached keys are marks at the run's start.
        let worked: Vec<&&StepReport> = keys
            .iter()
            .filter(|k| matches!(k.outcome, StepOutcome::Ran | StepOutcome::Failed))
            .collect();
        let (start, duration) = match (
            worked.iter().map(|k| k.start_unix_ns).min(),
            worked
                .iter()
                .map(|k| k.start_unix_ns.saturating_add(k.duration_ns))
                .max(),
        ) {
            (Some(start), Some(end)) => (start, end.saturating_sub(start)),
            _ => (keys[0].start_unix_ns, 0),
        };

        let mut meta = self.step_meta(run, node);
        meta.insert("barca.outcome".into(), json!(outcome.as_str()));
        meta.insert(
            "barca.cached".into(),
            json!((outcome == StepOutcome::Cached).to_string()),
        );
        if !keys[0].upstream.is_empty() {
            meta.insert("barca.upstream".into(), json!(keys[0].upstream.join(",")));
        }
        if let Some(first) = failed
            .iter()
            .min_by_key(|k| (k.start_unix_ns.saturating_add(k.duration_ns), &k.node_id))
        {
            error_meta(&mut meta, first, MAX_MESSAGE_CHARS, true);
            let key = first.partition.as_deref().unwrap_or("");
            let message = first.error_message.as_deref().unwrap_or("failed");
            let message = if failed.len() == 1 {
                format!("[{key}] {message}")
            } else {
                format!(
                    "{} of {} partitions failed; first [{key}] {message}",
                    failed.len(),
                    keys.len()
                )
            };
            meta.insert(
                "error.message".into(),
                json!(cut(&message, MAX_MESSAGE_CHARS)),
            );
        }
        let mut metrics = Map::new();
        metrics.insert("_dd.measured".into(), json!(1));
        metrics.insert("barca.partitions.total".into(), json!(keys.len()));
        metrics.insert("barca.partitions.executed".into(), json!(ran));
        metrics.insert("barca.partitions.cached".into(), json!(cached));
        metrics.insert("barca.partitions.failed".into(), json!(failed.len()));
        let sum_u64 = |f: fn(&StepReport) -> Option<u64>| {
            keys.iter().filter_map(|k| f(k)).reduce(|a, b| a + b)
        };
        if let Some(bytes) = sum_u64(|k| k.size_bytes) {
            metrics.insert("barca.bytes".into(), json!(bytes));
        }
        if let Some(cpu) = keys
            .iter()
            .filter_map(|k| k.cpu_seconds)
            .reduce(|a, b| a + b)
        {
            metrics.insert("barca.cpu_seconds".into(), json!(cpu));
        }
        if let Some(rss) = keys.iter().filter_map(|k| k.max_rss_bytes).max() {
            metrics.insert("barca.max_rss_bytes".into(), json!(rss));
        }
        out.push(self.finish(
            run,
            Span {
                id: step_id,
                parent: Some(root_id),
                name: "barca.step",
                resource: node.name.clone(),
                start,
                duration,
                error: outcome == StepOutcome::Failed,
                meta,
                metrics,
            },
        ));

        let mut failures_seen = 0;
        for key in keys {
            let is_failed = key.outcome == StepOutcome::Failed;
            if !is_failed && !key_spans.contains(&key.node_id) {
                continue;
            }
            let mut meta = Map::new();
            meta.insert("barca.node_id".into(), json!(key.node_id));
            meta.insert("barca.asset".into(), json!(node.name));
            if let Some(partition) = &key.partition {
                meta.insert("barca.partition".into(), json!(partition));
            }
            meta.insert("barca.outcome".into(), json!(key.outcome.as_str()));
            if let Some(hash) = &key.run_hash {
                meta.insert("barca.run_hash".into(), json!(hash));
            }
            if let Some(digest) = &key.digest {
                meta.insert("barca.digest".into(), json!(digest));
            }
            if is_failed {
                let detailed = failures_seen < DETAILED_FAILURES_PER_NODE;
                failures_seen += 1;
                let chars = if detailed {
                    MAX_MESSAGE_CHARS
                } else {
                    BRIEF_MESSAGE_CHARS
                };
                error_meta(&mut meta, key, chars, detailed);
            }
            let mut metrics = Map::new();
            cost_metrics(&mut metrics, key);
            out.push(self.finish(
                run,
                Span {
                    id: step_span_id(&run.run_id, &key.node_id),
                    parent: Some(step_id),
                    name: "barca.partition",
                    resource: node.name.clone(),
                    start: key.start_unix_ns,
                    duration: key.duration_ns,
                    error: is_failed,
                    meta,
                    metrics,
                },
            ));
        }
    }

    /// Request bodies for the run's trace, each under [`MAX_REQUEST_BYTES`]. Each is one trace
    /// chunk whose first span carries what Datadog reads from a chunk's root: the sampling
    /// decision (keep: these are jobs, not sampled requests) and the trace id's upper half.
    fn bodies(&self, run: &RunReport, key_spans: &HashSet<String>) -> Vec<Vec<u8>> {
        let high = format!("{:016x}", trace_id_high(&run.run_id));
        let mark_chunk_root = |span: &mut Value| {
            span["meta"]["_dd.p.tid"] = json!(high);
            span["meta"]["_dd.p.dm"] = json!("-4");
            span["metrics"]["_sampling_priority_v1"] = json!(2);
        };
        let mut bodies = Vec::new();
        let mut chunk: Vec<u8> = Vec::new();
        for mut span in self.spans(run, key_spans) {
            if chunk.is_empty() {
                mark_chunk_root(&mut span);
            }
            let mut encoded = serde_json::to_vec(&span).unwrap_or_default();
            if !chunk.is_empty() && chunk.len() + encoded.len() + 3 > MAX_REQUEST_BYTES {
                chunk.extend_from_slice(b"]]");
                bodies.push(std::mem::take(&mut chunk));
                mark_chunk_root(&mut span);
                encoded = serde_json::to_vec(&span).unwrap_or_default();
            }
            chunk.extend_from_slice(if chunk.is_empty() { b"[[" } else { b"," });
            chunk.extend_from_slice(&encoded);
        }
        if !chunk.is_empty() {
            chunk.extend_from_slice(b"]]");
            bodies.push(chunk);
        }
        bodies
    }

    /// The run as one trace, `[[spans...]]`, when it fits one request (tests).
    #[cfg(test)]
    fn trace(&self, run: &RunReport, key_spans: &HashSet<String>) -> Value {
        let bodies = self.bodies(run, key_spans);
        assert_eq!(bodies.len(), 1, "the trace takes {} requests", bodies.len());
        serde_json::from_slice(&bodies[0]).unwrap()
    }
}

impl Datadog {
    /// `Ok(None)` when `DD_TRACE_ENABLED` switches tracing off.
    pub fn from_env() -> Result<Option<Self>, String> {
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

    fn export<'a>(&'a self, run: &'a RunReport) -> super::Export<'a> {
        Box::pin(async move {
            let key_spans = take_key_spans(&run.run_id);
            for body in self.bodies(run, &key_spans) {
                self.send(&body).await?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_agent_url_diagnostics_do_not_echo_secret_values() {
        for url in [
            "https://user:secret@host/?token=secret",
            "http://[broken?token=secret",
            "http://host:secret",
            "http://?token=secret",
        ] {
            let error = agent_from(Some(url), None, None).unwrap_err();
            assert!(!error.contains("secret"), "{error}");
        }
    }

    fn datadog() -> Datadog {
        Datadog {
            agent: Agent::Tcp {
                host: "localhost".to_string(),
                port: 8126,
            },
            service: "planning".to_string(),
            env: Some("staging".to_string()),
            version: Some("1.2.3".to_string()),
            tags: vec![("team".to_string(), "data".to_string())],
            project: Some("shop".to_string()),
            statsd: Ok(dogstatsd::Target::Udp {
                host: "localhost".to_string(),
                port: 8125,
            }),
        }
    }

    fn node(base: &str) -> Arc<NodeContext> {
        Arc::new(NodeContext::unknown(base))
    }

    fn step(node_id: &str, outcome: StepOutcome) -> StepReport {
        let id = crate::StepId::parse(node_id);
        StepReport {
            node_id: node_id.to_string(),
            node: node(id.base_id()),
            partition: (!id.partition.is_empty()).then(|| id.partition.suffix()),
            outcome,
            start_unix_ns: 1_000,
            duration_ns: 500,
            attempts: Some(1),
            run_hash: Some("abc".to_string()),
            upstream: Vec::new(),
            size_bytes: Some(42),
            digest: None,
            cpu_seconds: None,
            max_rss_bytes: None,
            error_type: None,
            error_message: None,
            error_traceback: None,
        }
    }

    fn failed(node_id: &str, message: &str) -> StepReport {
        let mut s = step(node_id, StepOutcome::Failed);
        s.error_type = Some("ValueError".to_string());
        s.error_message = Some(message.to_string());
        s.error_traceback = Some(format!(
            "  File \"p.py\", line 3\n    raise ValueError({message:?})"
        ));
        s
    }

    fn run(status: &str, steps: Vec<StepReport>) -> RunReport {
        RunReport {
            run_id: "r1".to_string(),
            command: "run".to_string(),
            target: Some("publish".to_string()),
            job: "p.py:publish".to_string(),
            targets: vec!["publish".to_string()],
            files: vec!["p.py".to_string()],
            status: status.to_string(),
            reason: None,
            in_flight_run: None,
            start_unix_ns: 900,
            duration_ns: 2_000,
            steps_total: steps.len(),
            steps_executed: 1,
            steps_cached: 1,
            steps,
        }
    }

    fn spans(run: &RunReport, key_spans: &HashSet<String>) -> Vec<Value> {
        datadog().trace(run, key_spans)[0]
            .as_array()
            .unwrap()
            .clone()
    }

    fn named<'a>(spans: &'a [Value], name: &str) -> Vec<&'a Value> {
        spans.iter().filter(|s| s["name"] == name).collect()
    }

    #[test]
    fn agent_address_comes_from_the_url_or_host_and_port() {
        let tcp = |host: &str, port| Agent::Tcp {
            host: host.to_string(),
            port,
        };
        assert_eq!(agent_from(None, None, None), Ok(tcp("localhost", 8126)));
        assert_eq!(
            agent_from(None, Some("dd"), Some("9000")),
            Ok(tcp("dd", 9000))
        );
        assert_eq!(
            agent_from(Some("http://agent:8127/"), Some("ignored"), None),
            Ok(tcp("agent", 8127))
        );
        assert_eq!(
            agent_from(Some("http://agent"), None, None),
            Ok(tcp("agent", 8126))
        );
        assert_eq!(
            agent_from(Some("unix:///var/run/datadog/apm.socket"), None, None),
            Ok(Agent::Unix {
                path: "/var/run/datadog/apm.socket".to_string()
            })
        );
        assert_eq!(
            agent_from(Some("HTTP://agent:8127"), None, None),
            Ok(tcp("agent", 8127))
        );
        assert_eq!(
            agent_from(Some("http://[::1]:8127/x"), None, None),
            Ok(tcp("::1", 8127))
        );
        assert_eq!(
            agent_from(Some("http://[::1]"), None, None),
            Ok(tcp("::1", 8126))
        );
        assert!(agent_from(Some("http://[::1"), None, None).is_err());
        assert!(agent_from(Some("http://[::1]junk:8127"), None, None).is_err());
        assert_eq!(
            agent_from(Some("http://agent:8127#frag"), None, None),
            Ok(tcp("agent", 8127))
        );
        let with_password = agent_from(Some("http://user:secret@agent:8127"), None, None);
        assert!(!with_password.unwrap_err().contains("secret"));
        assert!(agent_from(Some("https://agent:8126"), None, None).is_err());
        assert!(agent_from(None, None, Some("many")).is_err());
    }

    #[test]
    fn dd_trace_enabled_is_on_when_unset_and_otherwise_only_for_true_or_one() {
        for off in ["false", "FALSE", "False", "0", "", "no", "off", "\"false\""] {
            assert!(!tracing_enabled(Some(off)), "{off:?}");
        }
        for on in ["true", "TRUE", "True", "1"] {
            assert!(tracing_enabled(Some(on)), "{on:?}");
        }
        assert!(tracing_enabled(None));
    }

    #[test]
    fn tags_split_on_commas_and_spaces() {
        assert_eq!(
            parse_tags("team:data, client:acme tier:1 novalue"),
            vec![
                ("team".to_string(), "data".to_string()),
                ("client".to_string(), "acme".to_string()),
                ("tier".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn ids_are_stable_non_zero_and_fit_in_63_bits() {
        let a = span_id(&["step", "r1", "p.py:orders"]);
        assert_eq!(a, span_id(&["step", "r1", "p.py:orders"]));
        assert_ne!(a, span_id(&["step", "r1", "p.py:other"]));
        assert_ne!(a, span_id(&["step", "r1p.py:", "orders"]));
        assert!(a > 0 && a < (1 << 63));
    }

    #[tokio::test]
    async fn put_accepts_a_2xx_and_reports_anything_else() {
        async fn exchange(reply: &'static str) -> (Result<(), String>, String) {
            let (client, mut server) = tokio::io::duplex(4096);
            let agent = tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut buf = [0u8; 1024];
                while !seen.ends_with(b"{}") {
                    let n = server.read(&mut buf).await.unwrap();
                    seen.extend_from_slice(&buf[..n]);
                }
                server.write_all(reply.as_bytes()).await.unwrap();
                String::from_utf8(seen).unwrap()
            });
            let result = put(client, "agent:8126", b"{}").await;
            (result, agent.await.unwrap())
        }

        let (ok, request) = exchange("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        assert_eq!(ok, Ok(()));
        assert!(request.starts_with("PUT /v0.4/traces HTTP/1.1\r\nHost: agent:8126\r\n"));
        assert!(request.contains("Content-Length: 2\r\n"));

        let (bad, _) = exchange("HTTP/1.1 415 Unsupported Media Type\r\n\r\n").await;
        assert!(bad.unwrap_err().contains("415"));
    }
    #[test]
    fn every_span_is_one_service_a_worker_and_carries_the_deployment_tags() {
        let payload = spans(
            &run(
                "success",
                vec![
                    step("p.py:orders", StepOutcome::Ran),
                    step("p.py:weekly[week=w1]", StepOutcome::Ran),
                ],
            ),
            &["p.py:weekly[week=w1]".to_string()].into(),
        );
        assert_eq!(
            payload
                .iter()
                .map(|s| s["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["barca.run", "barca.step", "barca.step", "barca.partition"]
        );
        for span in &payload {
            assert_eq!(span["service"], "planning", "{span}");
            assert_eq!(span["type"], "worker");
            let meta = &span["meta"];
            assert_eq!(meta["span.kind"], "internal");
            assert_eq!(meta["component"], "barca");
            assert_eq!(meta["env"], "staging");
            assert_eq!(meta["version"], "1.2.3");
            assert_eq!(meta["team"], "data");
            assert_eq!(meta["barca.version"], BARCA_VERSION);
            assert_eq!(meta["barca.project"], "shop");
            assert_eq!(meta["barca.run_id"], "r1");
            assert_eq!(span["trace_id"], payload[0]["trace_id"]);
        }
        // The chunk root carries the keep decision and the trace id's upper half; only it.
        let root = &payload[0];
        assert_eq!(root["metrics"]["_sampling_priority_v1"], 2);
        assert_eq!(root["meta"]["_dd.p.dm"], "-4");
        let tid = root["meta"]["_dd.p.tid"].as_str().unwrap();
        assert_eq!(tid.len(), 16);
        assert_eq!(u64::from_str_radix(tid, 16).unwrap(), trace_id_high("r1"));
        assert!(
            payload[1..]
                .iter()
                .all(|s| s["meta"].get("_dd.p.tid").is_none())
        );
    }

    #[test]
    fn the_run_resource_is_the_command_and_the_targets_short_names_or_the_files() {
        let one = spans(&run("success", Vec::new()), &HashSet::new());
        assert_eq!(one[0]["resource"], "run publish");
        assert_eq!(one[0]["meta"]["barca.targets"], "publish");
        assert_eq!(one[0]["meta"]["barca.files"], "p.py");
        assert_eq!(one[0]["meta"]["barca.job"], "p.py:publish");
        assert_eq!(one[0]["meta"]["barca.target"], "publish");
        assert!(one[0].get("parent_id").is_none());

        let mut two = run("success", Vec::new());
        two.command = "get".to_string();
        two.targets = vec!["daily_revenue".to_string(), "orders".to_string()];
        assert_eq!(
            spans(&two, &HashSet::new())[0]["resource"],
            "get daily_revenue,orders"
        );

        let mut whole = run("success", Vec::new());
        whole.command = "get".to_string();
        whole.targets = Vec::new();
        whole.target = None;
        whole.job = "all".to_string();
        let payload = spans(&whole, &HashSet::new());
        assert_eq!(payload[0]["resource"], "get p.py");
        assert!(payload[0]["meta"].get("barca.targets").is_none());
        whole.files = Vec::new();
        assert_eq!(spans(&whole, &HashSet::new())[0]["resource"], "get all");
    }

    #[test]
    fn a_step_is_named_by_its_node_and_carries_its_place_in_the_pipeline() {
        let mut orders = step("p.py:orders", StepOutcome::Cached);
        orders.node = Arc::new(NodeContext {
            group: Some("Sales/Daily".to_string()),
            freshness: "schedule",
            schedule: Some("0 6 * * *".to_string()),
            ..NodeContext::unknown("p.py:orders")
        });
        orders.upstream = vec!["p.py:raw".to_string(), "p.py:rates".to_string()];
        orders.digest = Some("sha".to_string());
        orders.cpu_seconds = Some(0.5);
        orders.max_rss_bytes = Some(1024);
        let payload = spans(&run("success", vec![orders]), &HashSet::new());
        let step = &payload[1];
        assert_eq!(step["name"], "barca.step");
        assert_eq!(step["resource"], "orders");
        assert_eq!(step["parent_id"], payload[0]["span_id"]);
        assert_eq!(step["span_id"], step_span_id("r1", "p.py:orders"));
        assert_eq!(step["error"], 0);
        let meta = &step["meta"];
        assert_eq!(meta["barca.asset"], "orders");
        assert_eq!(meta["barca.node_id"], "p.py:orders");
        assert_eq!(meta["barca.file"], "p.py");
        assert_eq!(meta["barca.group"], "Sales/Daily");
        assert_eq!(meta["barca.kind"], "asset");
        assert_eq!(meta["barca.freshness"], "schedule");
        assert_eq!(meta["barca.schedule"], "0 6 * * *");
        assert_eq!(meta["barca.outcome"], "cached");
        assert_eq!(meta["barca.cached"], "true");
        assert_eq!(meta["barca.run_hash"], "abc");
        assert_eq!(meta["barca.upstream"], "p.py:raw,p.py:rates");
        assert_eq!(meta["barca.digest"], "sha");
        assert_eq!(meta["barca.job"], "p.py:publish");
        assert!(meta.get("barca.partition").is_none());
        let metrics = &step["metrics"];
        assert_eq!(metrics["_dd.measured"], 1);
        assert_eq!(metrics["barca.bytes"], 42);
        assert_eq!(metrics["barca.attempts"], 1);
        assert_eq!(metrics["barca.cpu_seconds"], 0.5);
        assert_eq!(metrics["barca.max_rss_bytes"], 1024);
    }

    #[test]
    fn node_names_are_short_unless_two_files_share_one() {
        let source = |names: &[&str]| {
            names
                .iter()
                .map(|n| format!("@asset\ndef {n}():\n    return 1\n"))
                .collect::<String>()
        };
        let mut nodes = crate::parse::extract_nodes(
            &format!("from barca import asset\n{}", source(&["orders", "rates"])),
            "a.py",
        )
        .unwrap();
        nodes.extend(
            crate::parse::extract_nodes(
                &format!("from barca import asset\n{}", source(&["orders"])),
                "b.py",
            )
            .unwrap(),
        );
        let mut dag = crate::dag::Dag::build(&nodes).unwrap();
        dag.groups = vec![
            crate::groups::NodeGroup {
                id: "group:a.py:outer".into(),
                name: "Outer".into(),
                description: String::new(),
                members: vec!["group:a.py:inner".into()],
                output: "a.py:rates".into(),
            },
            crate::groups::NodeGroup {
                id: "group:a.py:inner".into(),
                name: "Inner".into(),
                description: String::new(),
                members: vec!["a.py:rates".into()],
                output: "a.py:rates".into(),
            },
        ];
        let contexts = crate::telemetry::node_contexts(&dag);
        assert_eq!(contexts["a.py:rates"].name, "rates");
        assert_eq!(contexts["a.py:orders"].name, "a.py:orders");
        assert_eq!(contexts["b.py:orders"].name, "b.py:orders");
        assert_eq!(contexts["a.py:rates"].group.as_deref(), Some("Outer/Inner"));
        assert_eq!(contexts["a.py:orders"].group, None);
        assert_eq!(contexts["a.py:rates"].file, "a.py");
        assert_eq!(contexts["a.py:rates"].freshness, "always");
    }

    #[test]
    fn a_failed_run_names_its_first_failed_asset_and_error_on_the_root() {
        let mut first = failed("p.py:publish", &"x".repeat(MAX_MESSAGE_CHARS + 500));
        first.error_traceback = Some(format!("{}  File \"p.py\", line 3", "y".repeat(5000)));
        first.attempts = None;
        let mut later = failed("p.py:audit", "later");
        later.start_unix_ns = 5_000;
        let mut skipped = step("p.py:report", StepOutcome::Skipped);
        skipped.duration_ns = 0;
        let payload = spans(&run("failed", vec![first, later, skipped]), &HashSet::new());
        let root = &payload[0];
        assert_eq!(root["error"], 1);
        assert_eq!(root["meta"]["barca.status"], "failed");
        assert_eq!(root["meta"]["barca.failed_asset"], "publish");
        assert_eq!(root["meta"]["barca.failed_node_id"], "p.py:publish");
        assert_eq!(root["meta"]["error.type"], "ValueError");
        let message = root["meta"]["error.message"].as_str().unwrap();
        assert!(message.starts_with("publish: xxx") && message.ends_with("[cut]"));
        assert!(root["meta"].get("error.stack").is_none());
        assert_eq!(root["metrics"]["barca.steps.failed"], 2);
        assert_eq!(root["metrics"]["barca.steps.skipped"], 1);

        let steps = named(&payload, "barca.step");
        let publish = steps.iter().find(|s| s["resource"] == "publish").unwrap();
        assert_eq!(publish["error"], 1);
        assert_eq!(publish["meta"]["barca.outcome"], "failed");
        assert_eq!(publish["meta"]["error.type"], "ValueError");
        let stack = publish["meta"]["error.stack"].as_str().unwrap();
        assert!(stack.starts_with("[cut]") && stack.ends_with("  File \"p.py\", line 3"));
        assert!(stack.chars().count() < MAX_STACK_CHARS + 20);
        assert!(publish["metrics"].get("barca.attempts").is_none());
        let report = steps.iter().find(|s| s["resource"] == "report").unwrap();
        assert_eq!(report["meta"]["barca.outcome"], "skipped");
        assert_eq!(report["error"], 0);
    }

    #[test]
    fn a_cancelled_or_skipped_run_says_why() {
        let mut cancelled = run("cancelled", Vec::new());
        cancelled.reason = Some("server shutdown".to_string());
        let root = &spans(&cancelled, &HashSet::new())[0];
        assert_eq!(root["error"], 1);
        assert_eq!(root["meta"]["barca.reason"], "server shutdown");
        assert_eq!(root["meta"]["error.type"], "Cancelled");
        assert_eq!(root["meta"]["error.message"], "server shutdown");

        let mut skipped = run("skipped", Vec::new());
        skipped.reason = Some("in_flight".to_string());
        skipped.in_flight_run = Some("abc123".to_string());
        let payload = spans(&skipped, &HashSet::new());
        assert_eq!(payload.len(), 1);
        assert_eq!(payload[0]["error"], 0);
        assert_eq!(payload[0]["meta"]["barca.status"], "skipped");
        assert_eq!(payload[0]["meta"]["barca.reason"], "in_flight");
        assert_eq!(payload[0]["meta"]["barca.in_flight_run"], "abc123");
        assert!(payload[0]["meta"].get("error.type").is_none());

        let ok = &spans(&run("success", Vec::new()), &HashSet::new())[0];
        assert!(ok["meta"].get("barca.reason").is_none());
        assert!(ok["meta"].get("error.message").is_none());
        let broken = &spans(&run("failed", Vec::new()), &HashSet::new())[0];
        assert_eq!(broken["meta"]["error.type"], "RunFailed");
    }

    /// A partitioned node with `n` keys: the first `ran` ran, the next `failed` failed, the
    /// rest were cached.
    fn partitioned(n: usize, ran: usize, failures: usize) -> Vec<StepReport> {
        (0..n)
            .map(|i| {
                let id = format!("p.py:weekly[week=w{i:05}]");
                let mut s = if i < ran {
                    step(&id, StepOutcome::Ran)
                } else if i < ran + failures {
                    failed(&id, &format!("bad week {i}"))
                } else {
                    step(&id, StepOutcome::Cached)
                };
                s.start_unix_ns = 1_000 + i as u64 * 10;
                s.duration_ns = 100;
                s
            })
            .collect()
    }

    #[test]
    fn a_partitioned_node_is_one_step_with_a_child_per_key_that_has_a_span() {
        let keys = partitioned(5, 3, 1);
        let traced: HashSet<String> = keys[..2].iter().map(|k| k.node_id.clone()).collect();
        let payload = spans(&run("failed", keys), &traced);
        let steps = named(&payload, "barca.step");
        assert_eq!(steps.len(), 1);
        let step = steps[0];
        assert_eq!(step["resource"], "weekly");
        assert_eq!(step["span_id"], step_span_id("r1", "p.py:weekly"));
        assert_eq!(step["error"], 1);
        assert_eq!(step["meta"]["barca.outcome"], "failed");
        assert_eq!(step["meta"]["barca.node_id"], "p.py:weekly");
        assert!(step["meta"].get("barca.partition").is_none());
        assert_eq!(
            step["meta"]["error.message"], "[week=w00003] bad week 3",
            "the one failed key names itself"
        );
        assert_eq!(step["metrics"]["barca.partitions.total"], 5);
        assert_eq!(step["metrics"]["barca.partitions.executed"], 3);
        assert_eq!(step["metrics"]["barca.partitions.cached"], 1);
        assert_eq!(step["metrics"]["barca.partitions.failed"], 1);
        assert_eq!(step["metrics"]["barca.bytes"], 42 * 5);
        // The span covers the keys that did work, not the cached ones.
        assert_eq!(step["start"], 1_000);
        assert_eq!(step["duration"], 30 + 100);

        // Two traced keys that ran, and the failed key, which was not traced.
        let keys = named(&payload, "barca.partition");
        assert_eq!(keys.len(), 3);
        for key in &keys {
            assert_eq!(key["resource"], "weekly", "no partition key in a resource");
            assert_eq!(key["parent_id"], step["span_id"]);
            assert_eq!(key["meta"]["barca.asset"], "weekly");
            let id = key["meta"]["barca.node_id"].as_str().unwrap();
            assert_eq!(key["span_id"], step_span_id("r1", id));
            assert!(key["metrics"].get("_dd.measured").is_none());
        }
        assert_eq!(keys[0]["meta"]["barca.partition"], "week=w00000");
        assert_eq!(keys[2]["meta"]["barca.partition"], "week=w00003");
        assert_eq!(keys[2]["error"], 1);
        assert_eq!(keys[2]["meta"]["error.message"], "bad week 3");
        assert!(keys[2]["meta"]["error.stack"].is_string());
        assert_eq!(
            payload[0]["meta"]["error.message"],
            "weekly[week=w00003]: bad week 3"
        );
        // Resources stay bounded however many keys there are.
        let resources: HashSet<&str> = payload
            .iter()
            .map(|s| s["resource"].as_str().unwrap())
            .collect();
        assert_eq!(resources, ["run publish", "weekly"].into());
    }

    #[test]
    fn many_failed_keys_each_get_a_span_and_only_the_first_ones_carry_detail() {
        let keys = partitioned(20_000, 0, 20_000);
        let report = run("failed", keys);
        let bodies = datadog().bodies(&report, &HashSet::new());
        let all: Vec<Value> = bodies
            .iter()
            .flat_map(|b| {
                assert!(b.len() <= MAX_REQUEST_BYTES, "{} bytes", b.len());
                let chunk: Value = serde_json::from_slice(b).unwrap();
                let spans = chunk[0].as_array().unwrap().clone();
                // Each request is a chunk whose first span says how to keep the trace.
                assert_eq!(spans[0]["metrics"]["_sampling_priority_v1"], 2);
                assert!(spans[0]["meta"]["_dd.p.tid"].is_string());
                spans
            })
            .collect();
        let keys = named(&all, "barca.partition");
        assert_eq!(keys.len(), 20_000);
        let detailed = keys
            .iter()
            .filter(|k| k["meta"].get("error.stack").is_some());
        assert_eq!(detailed.count(), DETAILED_FAILURES_PER_NODE);
        let step = named(&all, "barca.step")[0];
        assert_eq!(step["metrics"]["barca.partitions.failed"], 20_000);
        assert!(
            step["meta"]["error.message"]
                .as_str()
                .unwrap()
                .starts_with("20000 of 20000 partitions failed; first [week=w00000] bad week 0")
        );
        assert!(bodies.len() > 1);
        let total: usize = bodies.iter().map(Vec::len).sum();
        assert!(total < 25 << 20, "{total} bytes");
    }

    #[test]
    fn key_spans_are_capped_per_node_and_per_run_and_remembered_for_retries() {
        let run_id = "cap-test";
        let claimed = (0..KEY_SPANS_PER_NODE + 50)
            .filter(|i| claim_key_span(run_id, "p.py:a", &format!("p.py:a[k={i}]")))
            .count();
        assert_eq!(claimed, KEY_SPANS_PER_NODE);
        assert!(
            claim_key_span(run_id, "p.py:a", "p.py:a[k=0]"),
            "a retry keeps its span"
        );
        assert!(!claim_key_span(run_id, "p.py:a", "p.py:a[k=999]"));
        let mut total = KEY_SPANS_PER_NODE;
        for node in 0.. {
            let base = format!("p.py:n{node}");
            let got = (0..KEY_SPANS_PER_NODE)
                .filter(|i| claim_key_span(run_id, &base, &format!("{base}[k={i}]")))
                .count();
            total += got;
            if got < KEY_SPANS_PER_NODE {
                break;
            }
        }
        assert_eq!(total, KEY_SPANS_PER_RUN);
        // A line about a key with a span joins that span; about one without, the node's.
        assert_eq!(
            log_span_id(run_id, Some("p.py:a[k=0]")),
            step_span_id(run_id, "p.py:a[k=0]")
        );
        assert_eq!(
            log_span_id(run_id, Some("p.py:a[k=999]")),
            step_span_id(run_id, "p.py:a")
        );
        assert_eq!(
            log_span_id(run_id, Some("p.py:b")),
            step_span_id(run_id, "p.py:b")
        );
        assert_eq!(log_span_id(run_id, None), run_span_id(run_id));
        let taken = take_key_spans(run_id);
        assert_eq!(taken.len(), KEY_SPANS_PER_RUN);
        assert!(take_key_spans(run_id).is_empty());
    }

    #[test]
    fn a_worker_gets_the_parent_resource_and_tags_of_its_step() {
        let nodes: HashMap<String, Arc<NodeContext>> = [
            ("p.py:orders".to_string(), node("p.py:orders")),
            ("p.py:weekly".to_string(), node("p.py:weekly")),
        ]
        .into();
        let dispatch = Dispatch {
            service: "planning".to_string(),
            project: Some("shop".to_string()),
            job: "p.py:orders".to_string(),
            nodes,
        };
        let run_id = "dispatch-test";
        let ctx = dispatch
            .context(run_id, &crate::StepId::parse("p.py:orders"), 2)
            .unwrap();
        assert_eq!(ctx["trace_id"], trace_id(run_id));
        assert_eq!(ctx["trace_id_high"], trace_id_high(run_id));
        assert_eq!(ctx["parent_id"], step_span_id(run_id, "p.py:orders"));
        assert_eq!(ctx["resource"], "orders");
        assert_eq!(ctx["service"], "planning");
        assert_eq!(ctx["attempt"], 2);
        assert_eq!(ctx["tags"]["barca.asset"], "orders");
        assert_eq!(ctx["tags"]["barca.node_id"], "p.py:orders");
        assert_eq!(ctx["tags"]["barca.version"], BARCA_VERSION);
        assert_eq!(ctx["tags"]["barca.project"], "shop");
        assert!(ctx["tags"].get("barca.partition").is_none());

        let key = crate::StepId::parse("p.py:weekly[week=w1]");
        let ctx = dispatch.context(run_id, &key, 1).unwrap();
        assert_eq!(
            ctx["parent_id"],
            step_span_id(run_id, "p.py:weekly[week=w1]")
        );
        assert_eq!(ctx["resource"], "weekly");
        assert_eq!(ctx["tags"]["barca.partition"], "week=w1");
        for i in 0..KEY_SPANS_PER_NODE {
            dispatch.context(
                run_id,
                &crate::StepId::parse(&format!("p.py:weekly[week=x{i}]")),
                1,
            );
        }
        let past_cap = crate::StepId::parse("p.py:weekly[week=late]");
        assert!(dispatch.context(run_id, &past_cap, 1).is_none());
        take_key_spans(run_id);
    }

    #[test]
    fn serve_lifecycle_is_gauges_and_events_tagged_like_spans() {
        use super::super::ServerPhase;
        let report = |phase| ServerReport {
            phase,
            start_unix_ns: 123,
            files: 2,
            nodes: None,
            schedules: 3,
            read_only: false,
            watch: true,
            scheduling: true,
        };
        let start = datadog().server_packets(&report(ServerPhase::Start));
        let tags = "#service:planning,team:data,env:staging,version:1.2.3,\
                    barca.version:"
            .to_string()
            + BARCA_VERSION
            + ",barca.project:shop,barca.read_only:false,barca.watch:true,barca.scheduling:true";
        assert_eq!(start[0], format!("barca.serve.up:1|g|{tags}"));
        assert_eq!(start[1], format!("barca.serve.schedules:3|g|{tags}"));
        assert_eq!(start[2], format!("barca.serve.files:2|g|{tags}"));
        assert!(start[3].starts_with("_e{19,") && start[3].contains(":barca serve started|"));
        assert_eq!(start.len(), 4);
        let beat = datadog().server_packets(&report(ServerPhase::Heartbeat));
        assert_eq!(beat.len(), 3, "a heartbeat is gauges only");
        let stop = datadog().server_packets(&report(ServerPhase::Stop));
        assert!(stop[0].starts_with("barca.serve.up:0|g|"));
        assert!(stop[3].contains("barca serve stopped"));
        let mut known = report(ServerPhase::Heartbeat);
        known.nodes = Some(7);
        assert!(datadog().server_packets(&known)[3].starts_with("barca.serve.nodes:7|g"));
        // No run, source path or address leaks into a lifecycle metric.
        assert!(
            start
                .iter()
                .all(|p| !p.contains("run_id") && !p.contains(".py"))
        );
    }

    #[tokio::test]
    async fn serve_lifecycle_reaches_a_dogstatsd_listener() {
        use super::super::ServerPhase;
        let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut dd = datadog();
        dd.statsd = Ok(dogstatsd::Target::Udp {
            host: "127.0.0.1".to_string(),
            port: listener.local_addr().unwrap().port(),
        });
        let report = ServerReport {
            phase: ServerPhase::Stop,
            start_unix_ns: 1,
            files: 1,
            nodes: None,
            schedules: 0,
            read_only: true,
            watch: false,
            scheduling: false,
        };
        dd.export_server(&report).await.unwrap();
        let mut buf = [0u8; 1024];
        let n = listener.recv(&mut buf).await.unwrap();
        assert!(
            std::str::from_utf8(&buf[..n])
                .unwrap()
                .starts_with("barca.serve.up:0|g|#")
        );

        dd.statsd = Err("DD_DOGSTATSD_URL is not supported".to_string());
        assert!(dd.export_server(&report).await.is_err());
    }
}
