//! Live run events emitted during execution.
//!
//! The coordinator emits these as a run progresses so a caller (e.g. the HTTP
//! server) can stream them to clients in real time. They are advisory and live —
//! durable history lives in the DB (runs, materializations, logs tables).

use serde::Serialize;

/// A single event in a run's lifecycle. Serialized as the SSE payload.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    /// The run has begun (emitted by the server before execution starts).
    RunStarted { run_id: String },
    /// A line of user stdout captured while `node_id` was executing.
    Log { node_id: String, line: String },
    /// A step finished — either succeeded or failed.
    StepFinished {
        node_id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        elapsed_seconds: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        error: Option<String>,
    },
    /// The run finished (emitted by the server once execution completes).
    RunFinished { run_id: String, ok: bool },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // These shapes are the SSE wire contract the frontend's RunEvent union
    // depends on — keep them in lockstep.

    #[test]
    fn run_started_shape() {
        let ev = RunEvent::RunStarted {
            run_id: "abc".into(),
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"type": "run_started", "run_id": "abc"})
        );
    }

    #[test]
    fn log_shape() {
        let ev = RunEvent::Log {
            node_id: "a.py:load".into(),
            line: "hi".into(),
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"type": "log", "node_id": "a.py:load", "line": "hi"})
        );
    }

    #[test]
    fn step_finished_omits_none_fields() {
        let ev = RunEvent::StepFinished {
            node_id: "a.py:load".into(),
            ok: true,
            elapsed_seconds: Some(1.5),
            error: None,
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"type": "step_finished", "node_id": "a.py:load", "ok": true, "elapsed_seconds": 1.5})
        );

        let failed = RunEvent::StepFinished {
            node_id: "a.py:load".into(),
            ok: false,
            elapsed_seconds: None,
            error: Some("boom".into()),
        };
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            json!({"type": "step_finished", "node_id": "a.py:load", "ok": false, "error": "boom"})
        );
    }

    #[test]
    fn run_finished_shape() {
        let ev = RunEvent::RunFinished {
            run_id: "abc".into(),
            ok: true,
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"type": "run_finished", "run_id": "abc", "ok": true})
        );
    }
}
