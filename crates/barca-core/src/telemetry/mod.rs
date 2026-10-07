//! Run telemetry: what a finished run looked like, handed to whichever
//! integrations are switched on.
//!
//! The engine builds one [`RunReport`] per run and knows nothing about where it
//! goes. An integration is a module that implements [`Integration`] and is
//! listed in the `integrations!` table below; `BARCA_TELEMETRY=<name>[,<name>]`
//! switches it on. Exporting never fails a run: an integration that cannot
//! deliver gets one warning line on stderr.

pub mod datadog;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// How long one integration may take to deliver a run before it is abandoned.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(3);

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
    /// `success`, `failed` or `cancelled`.
    pub status: String,
    pub start_unix_ns: u64,
    pub duration_ns: u64,
    pub steps_total: usize,
    pub steps_executed: usize,
    pub steps_cached: usize,
    pub steps: Vec<StepReport>,
}

pub(crate) type Export<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// Somewhere a [`RunReport`] can be sent.
pub trait Integration: Send + Sync {
    /// Deliver one finished run. An `Err` is reported as a warning; it never fails the run.
    fn export<'a>(&'a self, run: &'a RunReport) -> Export<'a>;
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

/// The integrations `BARCA_TELEMETRY` switches on. An unknown name or a
/// misconfigured integration is a warning, not an error: telemetry must not
/// stop a run.
pub fn configured() -> Vec<(String, Box<dyn Integration>)> {
    let Ok(raw) = std::env::var("BARCA_TELEMETRY") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in parse_names(&raw) {
        match build(&name) {
            Some(Ok(Some(integration))) => out.push((name, integration)),
            // Switched off by its own settings: nothing to send, nothing to say.
            Some(Ok(None)) => {}
            Some(Err(e)) => warn_once(format!("telemetry '{name}' is off: {e}")),
            None => warn_once(format!(
                "unknown telemetry integration '{name}' in BARCA_TELEMETRY (known: {})",
                KNOWN.join(", ")
            )),
        }
    }
    out
}

/// A configuration warning, printed once per process: `barca serve` configures telemetry
/// for every run it starts, and a bad setting should not be repeated on every tick.
fn warn_once(message: String) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if seen
        .lock()
        .map(|mut s| s.insert(message.clone()))
        .unwrap_or(true)
    {
        eprintln!("[barca] warning: {message}");
    }
}

/// Send `run` to every integration, each bounded by [`EXPORT_TIMEOUT`].
pub async fn export(integrations: &[(String, Box<dyn Integration>)], run: &RunReport) {
    for (name, integration) in integrations {
        let outcome = tokio::time::timeout(EXPORT_TIMEOUT, integration.export(run)).await;
        let problem = match outcome {
            Ok(Ok(())) => continue,
            Ok(Err(e)) => e,
            Err(_) => format!("no reply within {}s", EXPORT_TIMEOUT.as_secs()),
        };
        eprintln!(
            "[barca] warning: telemetry '{name}' did not receive run {}: {problem}",
            run.run_id
        );
    }
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
