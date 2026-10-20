//! A run's progress lines: printed on stderr as before, and also handed to whoever streams
//! the run (`barca serve` sends each one as a `line` event, so a `--server` client prints
//! exactly what a local run prints).
//!
//! The run's event sender is held in a task-local for the duration of `execute`, so the
//! modules that print a progress line need no new parameter. Code that runs on another task
//! (a callback, a spawned task) takes [`Lines::current`] before it leaves and prints through
//! that handle.

use crate::RunEvent;
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio::sync::mpsc::UnboundedSender;

/// Which runs print a line: `agent` lines only with `--agent`, `always` lines in every mode.
/// A client streaming a served run prints the lines its own mode would.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum LineWhen {
    Agent,
    Always,
}

/// Where a run's lines go besides stderr. The default sends them nowhere else.
#[derive(Clone, Default)]
pub struct Lines {
    events: Option<UnboundedSender<RunEvent>>,
}

tokio::task_local! {
    static CURRENT: Lines;
}

impl Lines {
    pub(crate) fn new(events: Option<UnboundedSender<RunEvent>>) -> Self {
        Self { events }
    }

    /// The lines of the run this task belongs to; nowhere else outside a run.
    pub fn current() -> Self {
        CURRENT.try_with(Clone::clone).unwrap_or_default()
    }

    /// Run `fut` with these as the current lines.
    pub(crate) async fn scope<F: std::future::Future>(self, fut: F) -> F::Output {
        CURRENT.scope(self, fut).await
    }

    /// Hand `line` to the stream without printing it.
    pub fn emit(&self, when: LineWhen, line: &str) {
        if let Some(events) = &self.events {
            let _ = events.send(RunEvent::Line {
                line: line.to_string(),
                when,
            });
        }
    }

    /// Tell the stream that `node_id` was served from cache.
    pub(crate) fn cached_step(&self, node_id: &str) {
        if let Some(events) = &self.events {
            let _ = events.send(RunEvent::StepFinished {
                node_id: node_id.to_string(),
                ok: true,
                cached: true,
                elapsed_seconds: None,
                error: None,
                import_seconds: None,
            });
        }
    }

    /// Print a line on stderr and hand it to the stream.
    pub fn print(&self, when: LineWhen, args: fmt::Arguments<'_>) {
        let text = fmt::format(args);
        crate::errln!("{text}");
        self.emit(when, &text);
    }

    /// Print a line above the progress bar (or on stderr when there is no visible bar) and
    /// hand it to the stream.
    pub(crate) fn note(&self, pb: Option<&indicatif::ProgressBar>, msg: &str) {
        match pb {
            Some(bar) if !bar.is_hidden() => bar.println(msg),
            _ => crate::errln!("{msg}"),
        }
        self.emit(LineWhen::Always, msg);
    }
}

/// Print one of the current run's progress lines: `run_line!(Always, "...")` or
/// `run_line!(Agent, "...")`.
#[macro_export]
#[doc(hidden)]
macro_rules! run_line {
    ($when:ident, $($arg:tt)*) => {
        $crate::lines::Lines::current().print($crate::lines::LineWhen::$when, format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_reach_the_stream_inside_the_scope_only() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        Lines::new(Some(tx))
            .scope(async {
                crate::run_line!(Always, "[barca] one {}", 1);
                crate::run_line!(Agent, "[barca] step:a cached");
            })
            .await;
        crate::run_line!(Always, "outside");
        let mut got = Vec::new();
        while let Ok(event) = rx.try_recv() {
            got.push(serde_json::to_value(event).unwrap());
        }
        assert_eq!(
            got,
            vec![
                serde_json::json!({"type": "line", "line": "[barca] one 1", "when": "always"}),
                serde_json::json!({"type": "line", "line": "[barca] step:a cached", "when": "agent"}),
            ]
        );
    }
}
