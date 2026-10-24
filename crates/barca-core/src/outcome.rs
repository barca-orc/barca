//! The run contract of server mode (RFC-0007 §3.4): what starts a run, how it ended, and how
//! a finished run is shown. `barca serve` builds these; a `--server` client reads them back
//! and renders them with the same [`crate::report`] functions a local run uses, so both print
//! the same thing.

use crate::BarcaError;
use crate::cache::CachePolicy;
use crate::envelope::{Context, ErrorEnvelope, ErrorKind};
use crate::report::{self, RenderedOutput, ResultFormat};
use crate::results::{GetResult, MultiResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

/// The major version of the server's HTTP API, reported by `GET /health`. A client refuses
/// another major; additive fields do not bump it.
pub const API_VERSION: u32 = 1;

/// What started a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum TriggeredBy {
    /// A cron tick of `barca serve`.
    Schedule,
    /// An HTTP request to `barca serve` (`POST /runs`, the web UI, the older trigger routes).
    Api,
    /// A run a laptop computed and posted to the server's ledger.
    Client,
    /// A command typed on this machine (`barca get`, `barca run`).
    #[default]
    Manual,
}

impl TriggeredBy {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggeredBy::Schedule => "schedule",
            TriggeredBy::Api => "api",
            TriggeredBy::Client => "client",
            TriggeredBy::Manual => "manual",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "schedule" => TriggeredBy::Schedule,
            "api" => TriggeredBy::Api,
            "client" => TriggeredBy::Client,
            "manual" => TriggeredBy::Manual,
            _ => return None,
        })
    }
}

/// How a run was started, recorded on its `runs` row and its `barca.run` span.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    pub triggered_by: TriggeredBy,
    /// For a scheduled run: the tick it was started for, RFC 3339 in UTC.
    pub scheduled_at: Option<String>,
    /// For a scheduled run: the schedule's id, the full id of the scheduled node.
    pub schedule_id: Option<String>,
}

impl Trigger {
    /// A run started over HTTP.
    pub fn api() -> Self {
        Self {
            triggered_by: TriggeredBy::Api,
            ..Self::default()
        }
    }

    /// A run started by the cron tick of `schedule_id` at `epoch` (unix seconds).
    pub fn schedule(schedule_id: &str, epoch: i64) -> Self {
        Self {
            triggered_by: TriggeredBy::Schedule,
            scheduled_at: Some(rfc3339(epoch)),
            schedule_id: Some(schedule_id.to_string()),
        }
    }
}

/// Unix seconds as RFC 3339 in UTC (`2026-10-20T05:00:00Z`).
pub fn rfc3339(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// What a run is given besides its targets: the id to record it under (allocated by the
/// caller, so the id a server answers with is the database id) and what started it.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// The run id to use; a new one when `None`.
    pub run_id: Option<String>,
    pub trigger: Trigger,
    /// The capacity every run of a `barca serve` shares (`[server] capacity`): the run's steps
    /// are admitted against it as well as against the run's own (#628).
    pub shared_capacity: Option<std::sync::Arc<crate::capacity::SharedCapacity>>,
    /// `--batch` (RFC-0013): the run's override of the `batch=` of the nodes that declare one.
    pub batch: crate::batch::BatchOverride,
}

/// `get` or `run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RunCommand {
    Get,
    Run,
}

impl RunCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            RunCommand::Get => "get",
            RunCommand::Run => "run",
        }
    }
}

fn yes() -> bool {
    true
}

/// The body of `POST /runs`: the CLI's flags for `barca get` / `barca run`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    pub command: RunCommand,
    /// As given on the command line; `[]` means every asset and sensor (`get` only).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Vec<String>>"))]
    pub targets: Vec<String>,
    /// As typed, relative to the server's project root: the run reads only these deployed
    /// files. `[]` means the deployed set.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Vec<String>>"))]
    pub files: Vec<String>,
    /// `--refresh a,b`.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Vec<String>>"))]
    pub refresh: Vec<String>,
    /// `--refresh-all`.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<bool>"))]
    pub refresh_all: bool,
    /// `false` is `--no-cascade`.
    #[serde(default = "yes")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<bool>"))]
    pub cascade: bool,
    /// `--dry-run`: answered at once with the dry-run JSON; nothing starts.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<bool>"))]
    pub dry_run: bool,
    /// `--agent`: which stderr lines the client will print. The server streams every line,
    /// tagged with `when`, whatever this says.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<bool>"))]
    pub agent: bool,
    /// `get -o value`: the outcome's `final_output` holds the values (read on the server, as
    /// `barca get -o value --json` prints them) instead of the artifact pointers.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<bool>"))]
    pub values: bool,
    /// `--partition` of `get` or `run`: the partition keys the run computes across its cone, as
    /// the flag takes them (`"region=emea,amer"`, or a JSON object for compound keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub partition: Option<String>,
    /// `--batch` of `get` or `run` (RFC-0013), each as the flag takes it: `"all"`, `"none"`,
    /// `"20"`, or `"scored=all"` for one node. Experimental.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Vec<String>>"))]
    pub batch: Vec<String>,
}

impl RunRequest {
    pub fn get(targets: &[&str]) -> Self {
        Self::new(RunCommand::Get, targets)
    }

    pub fn run(targets: &[&str]) -> Self {
        Self::new(RunCommand::Run, targets)
    }

    fn new(command: RunCommand, targets: &[&str]) -> Self {
        Self {
            command,
            targets: targets.iter().map(|t| t.to_string()).collect(),
            files: Vec::new(),
            refresh: Vec::new(),
            refresh_all: false,
            cascade: true,
            dry_run: false,
            agent: false,
            values: false,
            partition: None,
            batch: Vec::new(),
        }
    }

    /// The cache policy the flags ask for, as the CLI derives it.
    pub fn policy(&self) -> CachePolicy {
        if self.refresh_all {
            CachePolicy::RefreshAll
        } else if !self.refresh.is_empty() {
            CachePolicy::RefreshSelective {
                names: self.refresh.clone(),
                cascade: self.cascade,
            }
        } else {
            CachePolicy::CacheAware
        }
    }

    /// `partition`, parsed as `--partition` is.
    pub fn partition_selection(
        &self,
    ) -> Result<Option<crate::selection::PartitionSelection>, String> {
        self.partition
            .as_deref()
            .map(crate::selection::PartitionSelection::parse)
            .transpose()
    }

    /// The target label `barca history` shows for this run: the targets, comma-separated.
    pub fn target_label(&self) -> Option<String> {
        (!self.targets.is_empty()).then(|| self.targets.join(","))
    }
}

/// How a run ended, the `status` of its [`Outcome`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// Every target succeeded. The outcome is the `--json` stdout body.
    Success,
    /// A step failed. The outcome is the failed-run stdout body plus `envelope`.
    Failed,
    /// Cancelled. The outcome is `envelope` only.
    Cancelled,
    /// barca or the request failed (`usage` or `infra`). The outcome is `envelope` only.
    Error,
}

impl OutcomeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeStatus::Success => "success",
            OutcomeStatus::Failed => "failed",
            OutcomeStatus::Cancelled => "cancelled",
            OutcomeStatus::Error => "error",
        }
    }
}

/// A finished run's full result, as JSON text tagged by `status`:
///
/// - `success`: the exact `barca get --json` / `barca run --json` body;
/// - `failed`: the CLI's failed-run stdout body plus `envelope`, the stderr error envelope;
/// - `cancelled`, `error`: `{"status", "envelope"}`.
///
/// Kept as text, not a parsed value, so the order of a multi-target run's `targets` (the order
/// they were given in) is what the client prints.
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct Outcome(Box<RawValue>);

impl Outcome {
    /// Read an outcome back from its JSON text.
    pub fn from_text(text: impl Into<String>) -> Result<Self, String> {
        let raw = RawValue::from_string(text.into()).map_err(|e| e.to_string())?;
        let outcome = Self(raw);
        outcome.parsed()?;
        Ok(outcome)
    }

    /// The JSON text.
    pub fn text(&self) -> &str {
        self.0.get()
    }

    pub fn raw(&self) -> &RawValue {
        &self.0
    }

    pub fn into_raw(self) -> Box<RawValue> {
        self.0
    }

    fn of_text(text: String) -> Self {
        Self(RawValue::from_string(text).expect("outcome text is JSON"))
    }

    fn parsed(&self) -> Result<Value, String> {
        let value: Value = serde_json::from_str(self.text()).map_err(|e| e.to_string())?;
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| "an outcome has a string `status`".to_string())?;
        if !["success", "failed", "cancelled", "error"].contains(&status) {
            return Err(format!("unknown outcome status '{status}'"));
        }
        Ok(value)
    }

    /// A successful single-target run: the `--json` body of the command. `get` returns the
    /// artifact pointer (paths by default, #404), `run` the value.
    pub fn success(result: &GetResult, command: RunCommand) -> Self {
        let output = match command {
            RunCommand::Get => report::pointer_output(
                result.final_output.as_ref(),
                result.partitions.as_deref(),
                result.key_summary.as_ref(),
            ),
            RunCommand::Run => report::read_task_output(
                result.final_output.as_ref(),
                result.partitions.as_deref(),
                result.key_summary.as_ref(),
            ),
        };
        Self::success_with(result, output)
    }

    /// A successful single-target run whose `final_output` the caller read (a `get` asked for
    /// values, `RunRequest.values`).
    pub fn success_with(result: &GetResult, final_output: Option<Value>) -> Self {
        Self::of_text(report::run_body(result, final_output).to_string())
    }

    /// A multi-target run: `success`, or `failed` with the envelope of its first failure.
    pub fn multi(result: &MultiResult, command: RunCommand, ctx: &Context) -> Self {
        let values: Vec<_> = result
            .targets
            .iter()
            .map(|(_, t)| match command {
                RunCommand::Get => report::pointer_output(
                    t.final_output.as_ref(),
                    t.partitions.as_deref(),
                    t.key_summary.as_ref(),
                ),
                RunCommand::Run => report::read_task_output(
                    t.final_output.as_ref(),
                    t.partitions.as_deref(),
                    t.key_summary.as_ref(),
                ),
            })
            .collect();
        Self::multi_with(result, &values, ctx)
    }

    /// A multi-target run whose targets' `final_output`s the caller read, in target order.
    pub fn multi_with(result: &MultiResult, values: &[Option<Value>], ctx: &Context) -> Self {
        let (run, per_target) = report::multi_parts(result, values);
        let mut body = report::multi_json(&run, &per_target);
        if let Some(error) = report::first_multi_failure(result) {
            body = with_envelope(body, &envelope_of(error, ctx));
        }
        Self::of_text(body)
    }

    /// A run that ended with `error`: a failed step (`failed`, with the stdout body when the
    /// run got that far), a cancel, or any other error.
    pub fn of_error(error: BarcaError, ctx: &Context) -> Self {
        Self::of_failed(error, ctx, None)
    }

    /// [`Self::of_error`], with the `final_output` a failed partitioned `get` returns: the keys
    /// that succeeded (as pointers, or values when the request asked for them).
    pub fn of_failed(error: BarcaError, ctx: &Context, final_output: Option<Value>) -> Self {
        let mut body = report::failed_run_body(&error);
        if let (Some(Value::Object(obj)), Some(out)) = (body.as_mut(), final_output) {
            obj.insert("final_output".into(), out);
        }
        let envelope = envelope_of(error, ctx);
        match (envelope.kind, body) {
            (ErrorKind::StepFailed, Some(body)) => {
                Self::of_text(with_envelope(body.to_string(), &envelope))
            }
            (ErrorKind::StepFailed, None) => Self::ended(OutcomeStatus::Failed, &envelope),
            (ErrorKind::Cancelled, _) => Self::ended(OutcomeStatus::Cancelled, &envelope),
            _ => Self::ended(OutcomeStatus::Error, &envelope),
        }
    }

    /// `{"status", "envelope"}`.
    pub fn ended(status: OutcomeStatus, envelope: &ErrorEnvelope) -> Self {
        Self::of_text(
            serde_json::json!({"status": status.as_str(), "envelope": envelope.to_json()})
                .to_string(),
        )
    }

    pub fn status(&self) -> OutcomeStatus {
        match self
            .parsed()
            .ok()
            .as_ref()
            .and_then(|v| v["status"].as_str())
        {
            Some("success") => OutcomeStatus::Success,
            Some("failed") => OutcomeStatus::Failed,
            Some("cancelled") => OutcomeStatus::Cancelled,
            _ => OutcomeStatus::Error,
        }
    }

    /// The error envelope, for every status but `success`.
    pub fn envelope(&self) -> Option<ErrorEnvelope> {
        let value = self.parsed().ok()?;
        serde_json::from_value(value.get("envelope")?.clone()).ok()
    }

    /// The exit code a local run ending this way exits with.
    pub fn exit_code(&self) -> i32 {
        match self.status() {
            OutcomeStatus::Success => 0,
            _ => self
                .envelope()
                .map_or(ErrorKind::Infra.exit_code(), |e| e.code()),
        }
    }

    /// What a local run that ended this way prints on stdout, and the error it ends with
    /// (with the one-line run summary a failed step ends with, as a local run prints it).
    /// `command` and `targets` are those of the request; `mode` and `fields` are the client's.
    pub fn render(
        &self,
        command: RunCommand,
        targets: &[String],
        mode: ResultFormat,
        values: bool,
        fields: Option<&[String]>,
    ) -> Result<(RenderedOutput, Option<ErrorEnvelope>), String> {
        let mut value = self.parsed()?;
        // What a `get` returned: values when the request asked for them (`-o value`), else
        // the pointers, whose paths print.
        let returned_of = |output: Option<&Value>| {
            if values {
                report::Returned {
                    final_output: output.filter(|v| !v.is_null()).cloned(),
                    values: true,
                    shown: Vec::new(),
                    key_summary: output.and_then(report::parse_key_summary),
                }
            } else {
                report::returned_from_pointers(output)
            }
        };
        let skipped = value["steps"].as_array().map_or(0, |steps| {
            steps
                .iter()
                .filter(|s| s["status"].as_str() == Some("skipped"))
                .count()
        });
        let envelope = match value.as_object_mut().and_then(|o| o.remove("envelope")) {
            Some(e) => Some(
                serde_json::from_value::<ErrorEnvelope>(e)
                    .map_err(|e| e.to_string())?
                    .with_failed_run_summary(skipped),
            ),
            None => None,
        };
        let status = self.status();
        let multi = value.get("targets").is_some();
        let rendered = match status {
            OutcomeStatus::Cancelled | OutcomeStatus::Error => RenderedOutput::default(),
            _ if multi => {
                #[derive(Deserialize)]
                struct Targets {
                    targets: OrderedMap,
                }
                let Targets { targets } =
                    serde_json::from_str(self.text()).map_err(|e| e.to_string())?;
                if let Some(o) = value.as_object_mut() {
                    o.remove("targets");
                }
                match command {
                    RunCommand::Get => {
                        let returned: Vec<report::Returned> = targets
                            .0
                            .iter()
                            .map(|(_, t)| returned_of(t.get("final_output")))
                            .collect();
                        let returned: Vec<&report::Returned> = returned.iter().collect();
                        report::render_get_multi_parts(
                            &value,
                            &targets.0,
                            &returned,
                            get_shape(mode),
                            fields,
                        )
                    }
                    RunCommand::Run => {
                        report::render_multi_parts(&value, &targets.0, mode, "ran", fields)
                    }
                }
            }
            OutcomeStatus::Success if command == RunCommand::Get => {
                let target = targets.first().map(String::as_str);
                let returned = returned_of(target.and(value.get("final_output")));
                report::render_get_body(&value, target, &returned, get_shape(mode), fields)
            }
            OutcomeStatus::Success => {
                let target = targets.first().map(String::as_str);
                // The body says `null` both for a run with no value and for a target whose
                // value is `None`; only a run with a target has one.
                let final_output = target.and(value.get("final_output"));
                report::render_run_body(
                    &value,
                    &report::run_label(command.as_str(), target),
                    final_output,
                    mode,
                    fields,
                )
            }
            OutcomeStatus::Failed => {
                let mut out = RenderedOutput::default();
                if mode == ResultFormat::Json && value.get("run_id").is_some() {
                    out.chunks
                        .push(report::OutputChunk::Stdout(format!("{value}\n")));
                } else if command == RunCommand::Get
                    && let Some(output) = value.get("final_output")
                {
                    // A partitioned target that stopped at a failed key: the keys that succeeded.
                    out = report::render_failed_get_returned(
                        &returned_of(Some(output)),
                        get_shape(mode),
                    );
                }
                out
            }
        };
        Ok((rendered, envelope))
    }
}

/// How a `get` prints for a client's output mode: `--json` the body, a terminal the summary
/// and the paths, and otherwise the paths alone (as `-o path`).
fn get_shape(mode: ResultFormat) -> report::GetShape {
    match mode {
        ResultFormat::Json => report::GetShape::Json,
        ResultFormat::Pretty => report::GetShape::Pretty,
        ResultFormat::Value => report::GetShape::Bare,
    }
}

/// The envelope a local `barca get` / `barca run` prints for `error`.
/// Only an unknown or ambiguous target ends with the `barca list` hint (#492).
pub fn envelope_of(error: BarcaError, ctx: &Context) -> ErrorEnvelope {
    ErrorEnvelope::from_barca(error, ctx)
}

/// `body` with `"envelope": ...` added as its last key.
fn with_envelope(body: String, envelope: &ErrorEnvelope) -> String {
    format!(
        "{},\"envelope\":{}}}",
        &body[..body.len() - 1],
        envelope.to_json()
    )
}

/// A JSON object's entries in the order they were written.
struct OrderedMap(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for OrderedMap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OrderedMap;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<OrderedMap, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, Value>()? {
                    entries.push(entry);
                }
                Ok(OrderedMap(entries))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

/// One run as `GET /runs/{run_id}` shows it: what started it, where it is, and, once it has
/// finished, its [`Outcome`]. A finished run is read from the metadata DB, so it is the same
/// after the server restarts.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunView {
    /// The database run id, the one `barca history` shows.
    pub run_id: String,
    /// `pending`, `running`, `success`, `failed`, `cancelled`, `error` or `interrupted`.
    pub status: String,
    /// `get` or `run`.
    pub command: String,
    /// The targets as given, comma-separated; `null` for every asset.
    pub target: Option<String>,
    pub files: Vec<String>,
    /// `null` for runs recorded before barca recorded it.
    pub triggered_by: Option<TriggeredBy>,
    pub scheduled_at: Option<String>,
    pub schedule_id: Option<String>,
    /// RFC 3339, UTC.
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub steps_total: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_executed: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub steps_cached: i64,
    /// The run's job: `all`, or its resolved node ids sorted and comma-separated; `null` while
    /// a live run has not recorded it, and for runs recorded before 0.29.0.
    pub job: Option<String>,
    /// Total recorded attempts (`barca history`); `null` while the run has recorded none.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub attempts: Option<i64>,
    /// For a run a laptop posted to the ledger: its run id on that laptop.
    pub client_run_id: Option<String>,
    /// The first recorded failed step and the first line of its error (`barca history`).
    pub failure: Option<crate::db::RunFailure>,
    /// The run's full result once it finished; `null` while it runs, and for runs not started
    /// through `barca serve`.
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown> | null"))]
    pub outcome: Option<Outcome>,
    /// The first error of the run, in one line, for a list or a page title.
    pub error: Option<String>,
    /// Each step the run recorded, then each step it streamed and has not recorded yet: at
    /// most `?limit=N` of them (default 1000, failed recorded steps first), every one with
    /// `?all=true`.
    pub steps: Vec<crate::db::RunStep>,
    /// How many steps there are in all, and whether `steps` lists fewer (#623).
    pub steps_page: crate::db::StepsPage,
    /// The lines the run's steps printed.
    pub logs: Vec<crate::db::LogEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::TargetOutcome;

    fn result() -> GetResult {
        GetResult {
            run_id: "r1".into(),
            elapsed_seconds: 1.25,
            steps_executed: 1,
            phases: 1,
            final_output: None,
            partitions: None,
            key_summary: None,
            steps: vec![],
            warnings: vec![],
            cache: None,
            uploads: None,
            partition_selection: None,
        }
    }

    fn rendered(out: &RenderedOutput) -> String {
        out.chunks
            .iter()
            .map(|c| match c {
                report::OutputChunk::Stdout(s) => format!("out:{s}"),
                report::OutputChunk::Stderr(s) => format!("err:{s}"),
            })
            .collect()
    }

    #[test]
    fn a_success_renders_as_the_local_run_does_in_every_mode() {
        let r = result();
        let outcome = Outcome::success(&r, RunCommand::Get);
        assert_eq!(outcome.status(), OutcomeStatus::Success);
        assert_eq!(outcome.exit_code(), 0);
        let fields = vec!["id".to_string()];
        for (mode, shape) in [
            (ResultFormat::Json, report::GetShape::Json),
            (ResultFormat::Value, report::GetShape::Bare),
            (ResultFormat::Pretty, report::GetShape::Pretty),
        ] {
            for fields in [None, Some(fields.as_slice())] {
                let local =
                    report::render_get(&r, None, &report::Returned::default(), shape, fields);
                let (served, error) = outcome
                    .render(RunCommand::Get, &[], mode, false, fields)
                    .unwrap();
                assert_eq!(rendered(&served), rendered(&local), "{mode:?}");
                assert!(error.is_none());
            }
        }
    }

    #[test]
    fn a_multi_target_run_keeps_the_order_of_its_targets() {
        let r = result();
        let multi = MultiResult {
            run_id: r.run_id,
            elapsed_seconds: r.elapsed_seconds,
            steps_executed: 2,
            phases: 1,
            steps: vec![],
            warnings: vec![],
            cache: None,
            uploads: None,
            targets: vec![
                (
                    "zeta".into(),
                    TargetOutcome {
                        status: "success".into(),
                        final_output: None,
                        partitions: None,
                        key_summary: None,
                        error: None,
                        failed_node: None,
                    },
                ),
                (
                    "alpha".into(),
                    TargetOutcome {
                        status: "failed".into(),
                        final_output: None,
                        partitions: None,
                        key_summary: None,
                        error: Some("ValueError: no".into()),
                        failed_node: Some("p.py:alpha".into()),
                    },
                ),
            ],
            partition_selection: None,
        };
        let ctx = Context {
            command: "get",
            files: vec![],
        };
        let outcome = Outcome::multi(&multi, RunCommand::Get, &ctx);
        assert_eq!(outcome.status(), OutcomeStatus::Failed);
        assert_eq!(outcome.exit_code(), 1);
        let targets = ["zeta".to_string(), "alpha".to_string()];
        for (mode, shape) in [
            (ResultFormat::Json, report::GetShape::Json),
            (ResultFormat::Value, report::GetShape::Bare),
            (ResultFormat::Pretty, report::GetShape::Pretty),
        ] {
            let local = report::render_get_multi(&multi, &[], shape, None);
            let (served, error) = outcome
                .render(RunCommand::Get, &targets, mode, false, None)
                .unwrap();
            assert_eq!(rendered(&served), rendered(&local), "{mode:?}");
            assert_eq!(error.unwrap().node.as_deref(), Some("p.py:alpha"));
        }
    }

    #[test]
    fn a_failed_step_carries_the_stdout_body_and_the_envelope() {
        let error = BarcaError::WorkerFailed(Box::new(crate::FailedStep {
            node: "p.py:a".into(),
            message: "ValueError: bad\n  File \"p.py\", line 4, in a\n    raise".into(),
            artifact_dir: None,
            run: Some(Box::new(crate::PartialRun {
                run_id: "r1".into(),
                elapsed_seconds: 0.5,
                steps_executed: 0,
                phases: 1,
                steps: vec![],
                warnings: vec![],
                partitions: None,
                key_summary: None,
            })),
        }));
        let local = report::render_failed_run(&error, ResultFormat::Json);
        let ctx = Context {
            command: "get",
            files: vec![],
        };
        let outcome = Outcome::of_error(error, &ctx);
        assert_eq!(outcome.status(), OutcomeStatus::Failed);
        assert_eq!(outcome.exit_code(), 1);
        let (served, envelope) = outcome
            .render(
                RunCommand::Get,
                &["a".into()],
                ResultFormat::Json,
                false,
                None,
            )
            .unwrap();
        assert_eq!(rendered(&served), rendered(&local));
        let envelope = envelope.unwrap();
        assert_eq!(envelope.kind, ErrorKind::StepFailed);
        assert!(envelope.traceback.unwrap().contains("line 4"));
    }

    #[test]
    fn cancels_and_errors_carry_only_the_envelope() {
        let ctx = Context {
            command: "get",
            files: vec![],
        };
        let cancelled = Outcome::of_error(BarcaError::Cancelled, &ctx);
        assert_eq!(cancelled.status(), OutcomeStatus::Cancelled);
        assert_eq!(cancelled.exit_code(), 130);
        let usage = Outcome::of_error(BarcaError::Usage("no such thing".into()), &ctx);
        assert_eq!(usage.status(), OutcomeStatus::Error);
        assert_eq!(usage.exit_code(), 2);
        let value: Value = serde_json::from_str(usage.text()).unwrap();
        assert_eq!(value["envelope"]["kind"], "usage");
        // Only a target error points at `barca list` (#492).
        assert_eq!(value["envelope"]["remediation"], "See `barca get --help`.");
        // A name far from every node: no "Did you mean" (#556) before the pointer.
        let unknown = Outcome::of_error(
            BarcaError::AssetNotFound("missing".into(), "a.py:y".into()),
            &ctx,
        );
        let value: Value = serde_json::from_str(unknown.text()).unwrap();
        assert_eq!(
            value["envelope"]["remediation"],
            "Run `barca list` to see available assets and tasks."
        );
        let (out, _) = usage
            .render(RunCommand::Get, &[], ResultFormat::Json, false, None)
            .unwrap();
        assert!(out.chunks.is_empty());
        assert!(Outcome::from_text(r#"{"status":"invented"}"#).is_err());
        assert!(Outcome::from_text(usage.text()).is_ok());
    }

    #[test]
    fn a_request_maps_to_the_policy_the_cli_derives() {
        let mut request: RunRequest =
            serde_json::from_str(r#"{"command":"get","targets":["a"]}"#).unwrap();
        assert!(request.cascade);
        assert!(matches!(request.policy(), CachePolicy::CacheAware));
        request.refresh = vec!["a".into()];
        request.cascade = false;
        assert!(matches!(
            request.policy(),
            CachePolicy::RefreshSelective { cascade: false, .. }
        ));
        request.refresh_all = true;
        assert!(matches!(request.policy(), CachePolicy::RefreshAll));
        assert!(serde_json::from_str::<RunRequest>(r#"{"command":"get","invented":1}"#).is_err());
        assert!(serde_json::from_str::<RunRequest>(r#"{"command":"sql"}"#).is_err());
        assert_eq!(
            Trigger::schedule("p.py:a", 0).scheduled_at.unwrap(),
            "1970-01-01T00:00:00Z"
        );
    }
}
