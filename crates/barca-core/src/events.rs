//! Live run events emitted during execution.
//!
//! The coordinator emits these as a run progresses so a caller (e.g. the HTTP
//! server) can stream them to clients in real time. They are advisory and live —
//! durable history lives in the DB (runs, materializations, logs tables).
//!
//! The server's `GET /runs/{run_id}/events` sends each one as the `data:` of a server-sent
//! event, with a per-run sequence number as its `id:`. A client reads them back with
//! [`RunEvent::parse`].

use crate::lines::LineWhen;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// A single event in a run's lifecycle. Serialized as the SSE payload.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    /// The run has begun. `run_id` is the database run id.
    RunStarted { run_id: String },
    /// A progress line exactly as a local run prints it on stderr: `--agent` step lines
    /// (`when: agent`), warnings, `[barca]` notes and the end-of-run line (`when: always`).
    Line { line: String, when: LineWhen },
    /// A line of user stdout captured while `node_id` was executing.
    Log { node_id: String, line: String },
    /// A step finished: it succeeded, failed, or was served from cache (`cached: true`).
    StepFinished {
        node_id: String,
        ok: bool,
        /// The step's result was reused from cache; its function did not run.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(optional, as = "Option<bool>"))]
        cached: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        elapsed_seconds: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        import_seconds: Option<f64>,
    },
    /// The run finished. Terminal: nothing follows it. `outcome` is the run's full result
    /// ([`crate::outcome`]), absent only from events recorded by an older server.
    RunFinished {
        run_id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "Record<string, unknown>"))]
        outcome: Option<Box<RawValue>>,
    },
}

impl RunEvent {
    /// Read an event back from its JSON (the `data:` of a server-sent event). The outcome of
    /// `run_finished` is kept as the exact text the server sent, so the order of its keys
    /// (the targets of a multi-target run) survives.
    pub fn parse(data: &str) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        struct Kind<'a> {
            #[serde(rename = "type", borrow)]
            kind: std::borrow::Cow<'a, str>,
        }
        #[derive(Deserialize)]
        struct Started {
            run_id: String,
        }
        #[derive(Deserialize)]
        struct LineEvent {
            line: String,
            when: LineWhen,
        }
        #[derive(Deserialize)]
        struct Log {
            node_id: String,
            line: String,
        }
        #[derive(Deserialize)]
        struct Step {
            node_id: String,
            ok: bool,
            #[serde(default)]
            cached: bool,
            elapsed_seconds: Option<f64>,
            error: Option<String>,
            import_seconds: Option<f64>,
        }
        #[derive(Deserialize)]
        struct Finished {
            run_id: String,
            ok: bool,
            outcome: Option<Box<RawValue>>,
        }
        let Kind { kind } = serde_json::from_str(data)?;
        Ok(match kind.as_ref() {
            "run_started" => {
                let Started { run_id } = serde_json::from_str(data)?;
                RunEvent::RunStarted { run_id }
            }
            "line" => {
                let LineEvent { line, when } = serde_json::from_str(data)?;
                RunEvent::Line { line, when }
            }
            "log" => {
                let Log { node_id, line } = serde_json::from_str(data)?;
                RunEvent::Log { node_id, line }
            }
            "step_finished" => {
                let s: Step = serde_json::from_str(data)?;
                RunEvent::StepFinished {
                    node_id: s.node_id,
                    ok: s.ok,
                    cached: s.cached,
                    elapsed_seconds: s.elapsed_seconds,
                    error: s.error,
                    import_seconds: s.import_seconds,
                }
            }
            "run_finished" => {
                let Finished {
                    run_id,
                    ok,
                    outcome,
                } = serde_json::from_str(data)?;
                RunEvent::RunFinished {
                    run_id,
                    ok,
                    outcome,
                }
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "unknown run event type '{other}'"
                )));
            }
        })
    }

    /// Whether this event ends the run's stream.
    pub fn is_terminal(&self) -> bool {
        matches!(self, RunEvent::RunFinished { .. })
    }
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
            cached: false,
            elapsed_seconds: Some(1.5),
            error: None,
            import_seconds: None,
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"type": "step_finished", "node_id": "a.py:load", "ok": true, "elapsed_seconds": 1.5})
        );

        let failed = RunEvent::StepFinished {
            node_id: "a.py:load".into(),
            ok: false,
            cached: false,
            elapsed_seconds: None,
            error: Some("boom".into()),
            import_seconds: None,
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
            outcome: None,
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            json!({"type": "run_finished", "run_id": "abc", "ok": true})
        );
    }

    #[test]
    fn cached_steps_and_lines_have_their_own_shapes() {
        let cached = RunEvent::StepFinished {
            node_id: "a.py:load".into(),
            ok: true,
            cached: true,
            elapsed_seconds: None,
            error: None,
            import_seconds: None,
        };
        assert_eq!(
            serde_json::to_value(&cached).unwrap(),
            json!({"type": "step_finished", "node_id": "a.py:load", "ok": true, "cached": true})
        );
        let line = RunEvent::Line {
            line: "[barca] step:a.py:load cached".into(),
            when: LineWhen::Agent,
        };
        assert_eq!(
            serde_json::to_value(&line).unwrap(),
            json!({"type": "line", "line": "[barca] step:a.py:load cached", "when": "agent"})
        );
    }

    #[test]
    fn events_parse_back_and_keep_the_outcome_text() {
        let outcome =
            r#"{"status":"success","targets":{"z":{"status":"success"},"a":{"status":"success"}}}"#;
        let events = [
            RunEvent::RunStarted { run_id: "r".into() },
            RunEvent::Line {
                line: "[barca] 1/1 steps | done in 0.1s".into(),
                when: LineWhen::Always,
            },
            RunEvent::Log {
                node_id: "a.py:x".into(),
                line: "hi".into(),
            },
            RunEvent::StepFinished {
                node_id: "a.py:x".into(),
                ok: true,
                cached: true,
                elapsed_seconds: Some(0.5),
                error: None,
                import_seconds: None,
            },
            RunEvent::RunFinished {
                run_id: "r".into(),
                ok: true,
                outcome: Some(RawValue::from_string(outcome.into()).unwrap()),
            },
        ];
        for event in events {
            let text = serde_json::to_string(&event).unwrap();
            let back = RunEvent::parse(&text).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), text);
        }
        let RunEvent::RunFinished {
            outcome: Some(raw), ..
        } = RunEvent::parse(&format!(
            r#"{{"type":"run_finished","run_id":"r","ok":true,"outcome":{outcome}}}"#
        ))
        .unwrap()
        else {
            panic!("not a run_finished event");
        };
        assert_eq!(raw.get(), outcome, "key order kept");
        assert!(RunEvent::parse(r#"{"type":"invented"}"#).is_err());
    }
}
