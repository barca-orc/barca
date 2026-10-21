//! Structured log lines for `barca serve` (`BARCA_LOG=json`, #434).
//!
//! Off by default, and only `barca serve` switches it on. While it is on, every line barca
//! writes through [`errln!`](crate::errln) (and [`term::stderr_str`](crate::term::stderr_str))
//! becomes one JSON object on **stdout** instead of a text line on stderr:
//!
//! ```json
//! {"ts":"2026-10-20T09:00:01.123Z","level":"info","msg":"step:p.py:orders completed 2.3s (1/2)","run_id":"0179afe0cb58","node":"p.py:orders"}
//! ```
//!
//! - `ts` is UTC with milliseconds; `level` is `info`, `warning` or `error`; `msg` is the
//!   line as barca would have printed it, without its `[barca] ` prefix.
//! - `run_id` is the run's id (the one `barca.run` spans and `barca get --json` carry), present
//!   on lines written while a run is going ([`in_run`]). `node` is the step the line is about.
//! - `dd.trace_id` and `dd.span_id` are the ids of the run's trace (its lower 64 bits) and of
//!   the step's span (a partition key's own span when it has one, else its node's; the run span
//!   when there is no node) when the Datadog integration is on, so Datadog joins the line to
//!   its trace.
//!
//! A step's captured output (its `print()`) is written as `info` lines with the run and node.
//! What a step writes to its stderr is not barca's: it stays on stderr as it is.

use std::fmt::Write as _;
use std::future::Future;
use std::sync::atomic::{AtomicU8, Ordering};

/// The environment variable that switches structured lines on.
pub const ENV_VAR: &str = "BARCA_LOG";

const OFF: u8 = 0;
const ON: u8 = 1;
/// On, with Datadog trace ids.
const ON_WITH_TRACE_IDS: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(OFF);

/// How bad a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

/// What `BARCA_LOG` asks for: `Ok(true)` for `json`, `Ok(false)` when unset, empty or `text`,
/// and `Err` with the value otherwise.
pub fn requested(raw: Option<&str>) -> Result<bool, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(false),
        Some(v) if v.eq_ignore_ascii_case("text") => Ok(false),
        Some(v) if v.eq_ignore_ascii_case("json") => Ok(true),
        Some(other) => Err(other.to_string()),
    }
}

/// Switch structured lines on for the rest of the process. `trace_ids`: add the Datadog ids.
pub fn enable(trace_ids: bool) {
    MODE.store(
        if trace_ids { ON_WITH_TRACE_IDS } else { ON },
        Ordering::Relaxed,
    );
}

/// Whether lines are written as JSON objects.
pub fn enabled() -> bool {
    MODE.load(Ordering::Relaxed) != OFF
}

tokio::task_local! {
    static RUN_ID: String;
}

/// Run `fut` with `run_id` as the run every line it writes belongs to. Lines written from tasks
/// it spawns do not inherit it.
pub async fn in_run<F: Future>(run_id: String, fut: F) -> F::Output {
    RUN_ID.scope(run_id, fut).await
}

fn current_run() -> Option<String> {
    RUN_ID.try_with(Clone::clone).ok()
}

/// A line's level, read from the line itself: barca's warnings say `warning:`, and failures
/// say `failed` or `error`.
fn level_of(msg: &str) -> Level {
    let lower = msg.trim_start().to_ascii_lowercase();
    let has_word = |word: &str| {
        lower.match_indices(word).any(|(i, _)| {
            let before = lower[..i].chars().next_back();
            let after = lower[i + word.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    if lower.starts_with("warning")
        || lower.contains(" warning:")
        || has_word("skipped")
        || has_word("cancelled")
        || has_word("disabled")
    {
        Level::Warning
    } else if lower.starts_with("error")
        || has_word("failed")
        || has_word("error")
        || lower.starts_with("not loaded")
    {
        Level::Error
    } else {
        Level::Info
    }
}

/// The step a line names: `step:<id> ...`.
fn node_of(msg: &str) -> Option<&str> {
    let rest = msg.strip_prefix("step:")?;
    let id = rest.split(' ').next()?;
    (!id.is_empty()).then_some(id)
}

fn quoted(out: &mut String, text: &str) {
    // A `String` serializes infallibly.
    out.push_str(&serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into()));
}

/// One JSON object (no newline) for a line.
fn object(
    ts: chrono::DateTime<chrono::Utc>,
    level: Level,
    msg: &str,
    run_id: Option<&str>,
    node: Option<&str>,
    trace_ids: bool,
) -> String {
    let mut out = String::with_capacity(msg.len() + 128);
    let _ = write!(
        out,
        "{{\"ts\":\"{}\",\"level\":\"{}\",\"msg\":",
        ts.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        level.as_str()
    );
    quoted(&mut out, msg);
    if let Some(run_id) = run_id {
        out.push_str(",\"run_id\":");
        quoted(&mut out, run_id);
    }
    if let Some(node) = node {
        out.push_str(",\"node\":");
        quoted(&mut out, node);
    }
    if trace_ids && let Some(run_id) = run_id {
        use crate::telemetry::datadog::{log_span_id, trace_id};
        let _ = write!(
            out,
            ",\"dd.trace_id\":\"{}\",\"dd.span_id\":\"{}\"",
            trace_id(run_id),
            log_span_id(run_id, node)
        );
    }
    out.push('}');
    out
}

/// A text line barca would have written to stderr, as a JSON object. `level` and `node` are
/// read from the line when not given.
pub(crate) fn render(text: &str, level: Option<Level>, node: Option<&str>) -> String {
    let msg = text.strip_prefix("[barca] ").unwrap_or(text);
    let run_id = current_run();
    object(
        chrono::Utc::now(),
        level.unwrap_or_else(|| level_of(msg)),
        msg,
        run_id.as_deref(),
        node.or_else(|| node_of(msg)),
        MODE.load(Ordering::Relaxed) == ON_WITH_TRACE_IDS,
    )
}

/// Write a line at `level` about `node`: a JSON object on stdout when structured lines are on,
/// else the text on stderr, exactly as [`errln!`](crate::errln) would.
pub fn line(level: Level, node: Option<&str>, text: &str) {
    if enabled() {
        crate::term::stdout_line(format_args!("{}", render(text, Some(level), node)));
    } else {
        crate::term::stderr_line(format_args!("{text}"));
    }
}

/// A line of a step's captured output, under `barca serve` with structured lines on: an `info`
/// line with the run and the node. Without structured lines nothing is written: the output is
/// kept with the run (`barca logs`).
pub fn step_output(node: &str, text: &str) {
    if enabled() {
        let rendered = object(
            chrono::Utc::now(),
            Level::Info,
            text,
            current_run().as_deref(),
            Some(node),
            MODE.load(Ordering::Relaxed) == ON_WITH_TRACE_IDS,
        );
        crate::term::stdout_line(format_args!("{rendered}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis(1_792_400_401_123).unwrap()
    }

    #[test]
    fn barca_log_accepts_json_and_text_and_names_anything_else() {
        assert_eq!(requested(None), Ok(false));
        assert_eq!(requested(Some("")), Ok(false));
        assert_eq!(requested(Some("text")), Ok(false));
        assert_eq!(requested(Some(" JSON ")), Ok(true));
        assert_eq!(requested(Some("yaml")), Err("yaml".to_string()));
    }

    #[test]
    fn levels_come_from_the_line() {
        for (line, level) in [
            ("step:p.py:a completed 1.0s (1/2)", Level::Info),
            ("step:p.py:a cached", Level::Info),
            ("2/2 steps | done in 1.3s", Level::Info),
            ("serving on http://127.0.0.1:8400  (1 file)", Level::Info),
            ("step:p.py:a failed: ValueError: boom", Level::Error),
            ("0/3 steps | failed in 0.0s", Level::Error),
            ("error: no such target", Level::Error),
            ("not loaded: p.py: invalid syntax", Level::Error),
            ("failed to spawn worker: x", Level::Error),
            (
                "warning: telemetry 'datadog' did not receive run r1",
                Level::Warning,
            ),
            ("1/3 steps | cancelled after 2.0s", Level::Warning),
            (
                "scheduled run p.py:a skipped — run h1 still in flight",
                Level::Warning,
            ),
            ("watch disabled: no inotify", Level::Warning),
            // A word inside a name is not the word.
            ("step:p.py:failed_rows completed 1.0s (1/1)", Level::Info),
            ("step:p.py:errors cached", Level::Info),
        ] {
            assert_eq!(level_of(line), level, "{line}");
        }
    }

    #[test]
    fn an_object_has_the_fields_in_order_and_escapes_the_text() {
        let line = object(
            ts(),
            Level::Info,
            "say \"hi\"\n\tbye",
            Some("r1"),
            Some("p.py:a"),
            false,
        );
        assert_eq!(
            line,
            r#"{"ts":"2026-10-19T09:00:01.123Z","level":"info","msg":"say \"hi\"\n\tbye","run_id":"r1","node":"p.py:a"}"#
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["msg"], "say \"hi\"\n\tbye");
        // Nothing known: no run, no node, no trace ids even when asked for.
        let bare = object(ts(), Level::Warning, "x", None, None, true);
        assert_eq!(
            bare,
            r#"{"ts":"2026-10-19T09:00:01.123Z","level":"warning","msg":"x"}"#
        );
    }

    #[test]
    fn trace_ids_are_the_ids_the_run_and_step_spans_use() {
        use crate::telemetry::datadog::span_id;
        let step: serde_json::Value = serde_json::from_str(&object(
            ts(),
            Level::Info,
            "x",
            Some("r1"),
            Some("p.py:a"),
            true,
        ))
        .unwrap();
        assert_eq!(
            step["dd.trace_id"],
            span_id(&["trace", "r1"]).to_string().as_str()
        );
        assert_eq!(
            step["dd.span_id"],
            span_id(&["step", "r1", "p.py:a"]).to_string().as_str()
        );
        let run: serde_json::Value =
            serde_json::from_str(&object(ts(), Level::Info, "x", Some("r1"), None, true)).unwrap();
        assert_eq!(
            run["dd.span_id"],
            span_id(&["run", "r1"]).to_string().as_str()
        );
        // A partition key without a span of its own (past the cap) joins its node's step span.
        let key: serde_json::Value = serde_json::from_str(&object(
            ts(),
            Level::Info,
            "x",
            Some("r1"),
            Some("p.py:a[k=1]"),
            true,
        ))
        .unwrap();
        assert_eq!(
            key["dd.span_id"],
            span_id(&["step", "r1", "p.py:a"]).to_string().as_str()
        );
    }

    #[tokio::test]
    async fn a_line_carries_the_run_it_was_written_in_and_the_step_it_names() {
        let outside: serde_json::Value =
            serde_json::from_str(&render("[barca] serving", None, None)).unwrap();
        assert!(outside.get("run_id").is_none());
        let inside = in_run("r9".to_string(), async {
            render("[barca] step:p.py:b failed: boom", None, None)
        })
        .await;
        let inside: serde_json::Value = serde_json::from_str(&inside).unwrap();
        assert_eq!(inside["run_id"], "r9");
        assert_eq!(inside["node"], "p.py:b");
        assert_eq!(inside["level"], "error");
        assert_eq!(inside["msg"], "step:p.py:b failed: boom");
    }
}
