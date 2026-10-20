//! Run and server lifecycle telemetry, handed to whichever
//! integrations are switched on.
//!
//! The engine builds one [`RunReport`] per run and knows nothing about where it
//! goes. An integration is a module that implements [`Integration`] and is
//! listed in the `integrations!` table below; `BARCA_TELEMETRY=<name>[,<name>]`
//! switches it on. Slack is the exception: it is not named there, and
//! `BARCA_SLACK_WEBHOOK_URL` alone switches it on. Exporting never fails a run: an
//! integration that cannot deliver gets one warning line on stderr.

pub mod datadog;
pub mod history;
pub mod slack;

pub use history::JobHistory;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// How long one integration may take to deliver a signal before it is abandoned.
pub(crate) const EXPORT_TIMEOUT: Duration = Duration::from_secs(3);

/// What happened to one step of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step's function ran and produced a result.
    Ran,
    /// A cached result was reused; the function did not run.
    Cached,
    /// The step failed (its own error, or its artifact could not be stored).
    Failed,
}

impl StepOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            StepOutcome::Ran => "ran",
            StepOutcome::Cached => "cached",
            StepOutcome::Failed => "failed",
        }
    }
}

/// One step of a finished run. Times are Unix nanoseconds.
#[derive(Debug, Clone)]
pub struct StepReport {
    /// Display id, e.g. `pipeline.py:orders` or `pipeline.py:weekly[week=w1]`.
    pub node_id: String,
    /// `asset`, `task` or `sensor`.
    pub kind: &'static str,
    pub outcome: StepOutcome,
    pub start_unix_ns: u64,
    pub duration_ns: u64,
    /// Attempts made, when known for this step alone. Not known for one partition of a
    /// partitioned step: attempts are counted per step, not per key.
    pub attempts: Option<u32>,
    pub run_hash: Option<String>,
    pub size_bytes: Option<u64>,
    pub cpu_seconds: Option<f64>,
    pub max_rss_bytes: Option<u64>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub error_traceback: Option<String>,
}

/// A finished run. Times are Unix nanoseconds.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub run_id: String,
    /// `get` or `run`.
    pub command: String,
    pub target: Option<String>,
    /// Canonical resolved job ids, independent of how a target was entered.
    pub job: String,
    /// The barca environment the run used (`--env`, `BARCA_ENV`, ...), when known.
    pub env: Option<String>,
    /// `success`, `failed` or `cancelled`; `skipped` for a `barca serve` tick that did not
    /// start a run.
    pub status: String,
    /// Why a run was skipped: `in_flight` when a run of the same node was still going.
    pub reason: Option<String>,
    /// The handle of the run in flight that caused a skip.
    pub in_flight_run: Option<String>,
    pub start_unix_ns: u64,
    pub duration_ns: u64,
    pub steps_total: usize,
    pub steps_executed: usize,
    pub steps_cached: usize,
    pub steps: Vec<StepReport>,
    /// What started the run (`barca.trigger`), and for a scheduled run the tick
    /// (`barca.scheduled_at`) and the schedule (`barca.schedule_id`).
    pub trigger: crate::outcome::Trigger,
}

/// A server lifecycle signal, independent of materialization runs.
#[derive(Debug, Clone)]
pub struct ServerReport {
    pub phase: ServerPhase,
    pub start_unix_ns: u64,
    pub files: usize,
    /// Present only when the server has already cached node metadata.
    pub nodes: Option<usize>,
    pub schedules: usize,
    pub read_only: bool,
    pub watch: bool,
    pub scheduling: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerPhase {
    Start,
    Heartbeat,
    Stop,
}

impl ServerPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Heartbeat => "heartbeat",
            Self::Stop => "stop",
        }
    }
}

pub(crate) type Export<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// Somewhere run reports and optional server lifecycle signals can be sent.
pub trait Integration: Send + Sync {
    /// Deliver one finished run. `history` reads the job's earlier runs and the last post an
    /// integration recorded for it. An `Err` is reported as a warning; it never fails the run,
    /// and it must not contain a secret such as a webhook URL.
    fn export<'a>(&'a self, run: &'a RunReport, history: &'a JobHistory) -> Export<'a>;
    /// Optional server lifecycle support. Integrations can remain run-only.
    fn export_server<'a>(&'a self, _server: &'a ServerReport) -> Export<'a> {
        Box::pin(async { Ok(()) })
    }
}

/// The table of integrations: `"name" => constructor`. A constructor reads its own settings
/// from the environment. It returns `Ok(None)` when those settings switch the integration
/// off, and `Err` with what is missing or malformed.
macro_rules! integrations {
    ($($name:literal => $ctor:path),+ $(,)?) => {
        /// Names accepted in `BARCA_TELEMETRY`.
        pub const KNOWN: &[&str] = &[$($name),+];

        fn build(name: &str) -> Option<Result<Option<Box<dyn Integration>>, String>> {
            match name {
                $($name => Some(
                    $ctor().map(|on| on.map(|i| Box::new(i) as Box<dyn Integration>)),
                ),)+
                _ => None,
            }
        }
    };
}

integrations! {
    "datadog" => datadog::Datadog::from_env,
}

/// The names in a `BARCA_TELEMETRY` value: comma-separated, case-insensitive, blanks ignored.
fn parse_names(raw: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in raw.split(',').map(|n| n.trim().to_ascii_lowercase()) {
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The integrations `BARCA_TELEMETRY` switches on, and Slack when `BARCA_SLACK_WEBHOOK_URL`
/// is set. An unknown name or a misconfigured integration is a warning, not an error:
/// telemetry must not stop a run.
pub fn configured() -> Vec<(String, Box<dyn Integration>)> {
    let mut out = Vec::new();
    let names = std::env::var("BARCA_TELEMETRY")
        .map(|raw| parse_names(&raw))
        .unwrap_or_default();
    for name in names {
        match build(&name) {
            Some(Ok(Some(integration))) => out.push((name, integration)),
            // Switched off by its own settings: nothing to send, nothing to say.
            Some(Ok(None)) => {}
            Some(Err(e)) => {
                crate::warnings::warn_once(&format!("telemetry '{name}' is off: {e}"));
            }
            None => {
                let hint = if name == slack::NAME {
                    format!("; Slack is switched on by setting {}", slack::WEBHOOK_VAR)
                } else {
                    String::new()
                };
                crate::warnings::warn_once(&format!(
                    "unknown telemetry integration '{name}' in BARCA_TELEMETRY (known: {}){hint}",
                    KNOWN.join(", ")
                ));
            }
        }
    }
    // Slack's switch is its own variable, which is barca's alone (see `slack.rs`).
    match slack::Slack::from_env() {
        Ok(Some(slack)) => out.push((slack::NAME.to_string(), Box::new(slack))),
        Ok(None) => {}
        Err(e) => {
            crate::warnings::warn_once(&format!("telemetry '{}' is off: {e}", slack::NAME));
        }
    }
    out
}

/// Send `run` to every integration, each bounded by [`EXPORT_TIMEOUT`].
///
/// A delivery failure is reported when it starts and when it ends, not on every run:
/// `barca serve` exports a run per tick, and a backend that is down for a day should not
/// write a line every few minutes.
pub async fn export(
    integrations: &[(String, Box<dyn Integration>)],
    run: &RunReport,
    history: &JobHistory,
) {
    for (name, integration) in integrations {
        let outcome = tokio::time::timeout(EXPORT_TIMEOUT, integration.export(run, history)).await;
        let problem = match outcome {
            Ok(Ok(())) => {
                if failing()
                    .lock()
                    .map(|mut f| f.remove(name))
                    .unwrap_or(false)
                {
                    crate::errln!("[barca] telemetry '{name}' is receiving runs again");
                }
                continue;
            }
            Ok(Err(e)) => e,
            Err(_) => format!("no reply within {}s", EXPORT_TIMEOUT.as_secs()),
        };
        if failing()
            .lock()
            .map(|mut f| f.insert(name.clone()))
            .unwrap_or(true)
        {
            crate::errln!(
                "[barca] warning: telemetry '{name}' did not receive run {}: {problem}",
                run.run_id
            );
        }
    }
}

/// Lifecycle exports have the same bounded delivery as runs, with independent warning state.
pub async fn export_server(integrations: &[(String, Box<dyn Integration>)], server: &ServerReport) {
    for (name, integration) in integrations {
        let key = format!("{name}:serve");
        let problem = match tokio::time::timeout(EXPORT_TIMEOUT, integration.export_server(server))
            .await
        {
            Ok(Ok(())) => {
                if failing()
                    .lock()
                    .map(|mut f| f.remove(&key))
                    .unwrap_or(false)
                {
                    crate::errln!("[barca] telemetry '{name}' is receiving server signals again");
                }
                continue;
            }
            // Integration error strings may contain addresses or credentials. Lifecycle
            // diagnostics deliberately report only the failure category.
            Ok(Err(_)) => "delivery failed",
            Err(_) => "no reply within 3s",
        };
        if failing().lock().map(|mut f| f.insert(key)).unwrap_or(true) {
            crate::errln!(
                "[barca] warning: telemetry '{name}' did not receive server signal: {problem}"
            );
        }
    }
}

/// Integrations whose last delivery in this process failed.
fn failing() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static FAILING: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    FAILING.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_lowercased_and_deduplicated() {
        assert_eq!(
            parse_names(" Datadog, ,sentry,datadog "),
            vec!["datadog".to_string(), "sentry".to_string()]
        );
        assert!(parse_names("").is_empty());
    }

    #[test]
    fn every_known_name_builds_or_explains_itself() {
        for name in KNOWN {
            assert!(build(name).is_some(), "{name} is listed but not built");
        }
        assert!(build("nope").is_none());
    }
}
