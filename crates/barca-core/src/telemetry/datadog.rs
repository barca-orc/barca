//! Datadog: one trace per run, sent to the local Datadog Agent.
//!
//! The run is the root span (`barca.run`) and every step is a child span
//! (`barca.step`), so a run opens in APM as a timeline of its steps. The trace
//! goes to the Agent's trace intake as JSON over plain HTTP, the same place
//! `ddtrace` sends to, so nothing but the Agent is needed.
//!
//! Settings are Datadog's own environment variables:
//!
//! - `DD_TRACE_AGENT_URL` (`http://host:port` or `unix:///path/to/apm.socket`),
//!   else `DD_AGENT_HOST` (default `localhost`) and `DD_TRACE_AGENT_PORT`
//!   (default `8126`);
//! - `DD_SERVICE` (default `barca`), `DD_ENV`, `DD_VERSION`;
//! - `DD_TAGS` (`key:value` pairs separated by commas or spaces), added to every span.

use super::{Integration, RunReport, StepOutcome};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const INTAKE_PATH: &str = "/v0.3/traces";
/// A traceback longer than this is cut: the Agent drops oversized tag values.
const MAX_STACK_CHARS: usize = 4000;

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
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The Agent address from `DD_TRACE_AGENT_URL`, or from host and port.
fn agent_from(url: Option<&str>, host: Option<&str>, port: Option<&str>) -> Result<Agent, String> {
    let parse_port = |raw: &str| {
        raw.parse::<u16>()
            .map_err(|_| format!("'{raw}' is not a port number"))
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
        let Some(rest) = url.strip_prefix("http://") else {
            return Err(format!(
                "DD_TRACE_AGENT_URL={url} is not supported: use http://host:port or unix:///path"
            ));
        };
        let authority = rest.split('/').next().unwrap_or("");
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, parse_port(p)?),
            None => (authority, 8126),
        };
        if host.is_empty() {
            return Err(format!("DD_TRACE_AGENT_URL={url} names no host"));
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
fn parse_tags(raw: &str) -> Vec<(String, String)> {
    raw.split([',', ' '])
        .filter_map(|pair| pair.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

/// A non-zero 63-bit id derived from `parts`, so a run's ids are the same however often
/// it is exported.
fn span_id(parts: &[&str]) -> u64 {
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

impl Datadog {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            agent: agent_from(
                env_var("DD_TRACE_AGENT_URL").as_deref(),
                env_var("DD_AGENT_HOST").as_deref(),
                env_var("DD_TRACE_AGENT_PORT").as_deref(),
            )?,
            service: env_var("DD_SERVICE").unwrap_or_else(|| "barca".to_string()),
            env: env_var("DD_ENV"),
            version: env_var("DD_VERSION"),
            tags: env_var("DD_TAGS")
                .map(|raw| parse_tags(&raw))
                .unwrap_or_default(),
        })
    }

    /// Tags every span of the trace carries.
    fn common_meta(&self, run: &RunReport) -> Map<String, Value> {
        let mut meta = Map::new();
        for (k, v) in &self.tags {
            meta.insert(k.clone(), json!(v));
        }
        if let Some(env) = &self.env {
            meta.insert("env".to_string(), json!(env));
        }
        if let Some(version) = &self.version {
            meta.insert("version".to_string(), json!(version));
        }
        meta.insert("barca.run_id".to_string(), json!(run.run_id));
        meta
    }

    /// The run as one Datadog trace: `[[root span, step spans...]]`.
    fn trace(&self, run: &RunReport) -> Value {
        let trace_id = span_id(&["trace", &run.run_id]);
        let root_id = span_id(&["run", &run.run_id]);
        let resource = match &run.target {
            Some(target) => format!("{} {target}", run.command),
            None => run.command.clone(),
        };

        let mut meta = self.common_meta(run);
        meta.insert("barca.command".to_string(), json!(run.command));
        meta.insert("barca.status".to_string(), json!(run.status));
        if let Some(target) = &run.target {
            meta.insert("barca.target".to_string(), json!(target));
        }
        let mut spans = vec![json!({
            "trace_id": trace_id,
            "span_id": root_id,
            "name": "barca.run",
            "resource": resource,
            "service": self.service,
            "type": "custom",
            "start": run.start_unix_ns,
            "duration": run.duration_ns,
            "error": i32::from(run.status != "success"),
            "meta": meta,
            "metrics": {
                // Keep every run: these are jobs, not sampled requests.
                "_sampling_priority_v1": 1,
                "barca.steps.total": run.steps_total,
                "barca.steps.executed": run.steps_executed,
                "barca.steps.cached": run.steps_cached,
            },
        })];

        for step in &run.steps {
            let mut meta = self.common_meta(run);
            meta.insert("barca.node".to_string(), json!(step.node_id));
            meta.insert("barca.kind".to_string(), json!(step.kind));
            meta.insert("barca.outcome".to_string(), json!(step.outcome.as_str()));
            if let Some(hash) = &step.run_hash {
                meta.insert("barca.run_hash".to_string(), json!(hash));
            }
            if let Some(t) = &step.error_type {
                meta.insert("error.type".to_string(), json!(t));
            }
            if let Some(m) = &step.error_message {
                meta.insert("error.message".to_string(), json!(m));
            }
            if let Some(stack) = &step.error_traceback {
                let cut: String = stack.chars().take(MAX_STACK_CHARS).collect();
                meta.insert("error.stack".to_string(), json!(cut));
            }
            let mut metrics = Map::new();
            metrics.insert("barca.attempts".to_string(), json!(step.attempts));
            if let Some(bytes) = step.size_bytes {
                metrics.insert("barca.bytes".to_string(), json!(bytes));
            }
            if let Some(cpu) = step.cpu_seconds {
                metrics.insert("barca.cpu_seconds".to_string(), json!(cpu));
            }
            if let Some(rss) = step.max_rss_bytes {
                metrics.insert("barca.max_rss_bytes".to_string(), json!(rss));
            }
            spans.push(json!({
                "trace_id": trace_id,
                "span_id": span_id(&["step", &run.run_id, &step.node_id]),
                "parent_id": root_id,
                "name": "barca.step",
                "resource": step.node_id,
                "service": self.service,
                "type": "custom",
                "start": step.start_unix_ns,
                "duration": step.duration_ns,
                "error": i32::from(step.outcome == StepOutcome::Failed),
                "meta": meta,
                "metrics": metrics,
            }));
        }
        json!([spans])
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

impl Integration for Datadog {
    fn export<'a>(&'a self, run: &'a RunReport) -> super::Export<'a> {
        Box::pin(async move {
            let body = serde_json::to_vec(&self.trace(run)).map_err(|e| e.to_string())?;
            match &self.agent {
                Agent::Tcp { host, port } => {
                    let stream = tokio::net::TcpStream::connect((host.as_str(), *port))
                        .await
                        .map_err(|e| {
                            format!("cannot reach the Datadog Agent at {host}:{port}: {e}")
                        })?;
                    put(stream, &format!("{host}:{port}"), &body).await
                }
                Agent::Unix { path } => {
                    let stream = tokio::net::UnixStream::connect(path)
                        .await
                        .map_err(|e| format!("cannot reach the Datadog Agent at {path}: {e}"))?;
                    put(stream, "localhost", &body).await
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::StepReport;
    use super::*;

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
        }
    }

    fn step(node: &str, outcome: StepOutcome) -> StepReport {
        StepReport {
            node_id: node.to_string(),
            kind: "asset",
            outcome,
            start_unix_ns: 1_000,
            duration_ns: 500,
            attempts: 1,
            run_hash: Some("abc".to_string()),
            size_bytes: Some(42),
            cpu_seconds: None,
            max_rss_bytes: None,
            error_type: None,
            error_message: None,
            error_traceback: None,
        }
    }

    fn run(status: &str, steps: Vec<StepReport>) -> RunReport {
        RunReport {
            run_id: "r1".to_string(),
            command: "run".to_string(),
            target: Some("p.py:publish".to_string()),
            status: status.to_string(),
            start_unix_ns: 900,
            duration_ns: 2_000,
            steps_total: steps.len(),
            steps_executed: 1,
            steps_cached: 1,
            steps,
        }
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
        assert!(agent_from(Some("https://agent:8126"), None, None).is_err());
        assert!(agent_from(None, None, Some("many")).is_err());
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
    fn a_run_is_one_trace_with_a_root_span_and_a_child_per_step() {
        let mut failed = step("p.py:publish", StepOutcome::Failed);
        failed.kind = "task";
        failed.error_type = Some("ValueError".to_string());
        failed.error_message = Some("bad".to_string());
        let payload = datadog().trace(&run(
            "failed",
            vec![step("p.py:orders", StepOutcome::Cached), failed],
        ));

        let traces = payload.as_array().unwrap();
        assert_eq!(traces.len(), 1);
        let spans = traces[0].as_array().unwrap();
        assert_eq!(spans.len(), 3);

        let root = &spans[0];
        assert_eq!(root["name"], "barca.run");
        assert_eq!(root["resource"], "run p.py:publish");
        assert_eq!(root["service"], "planning");
        assert_eq!(root["error"], 1);
        assert_eq!(root["meta"]["env"], "staging");
        assert_eq!(root["meta"]["version"], "1.2.3");
        assert_eq!(root["meta"]["team"], "data");
        assert_eq!(root["meta"]["barca.status"], "failed");
        assert_eq!(root["metrics"]["barca.steps.cached"], 1);
        assert!(root.get("parent_id").is_none());

        for child in &spans[1..] {
            assert_eq!(child["trace_id"], root["trace_id"]);
            assert_eq!(child["parent_id"], root["span_id"]);
            assert_eq!(child["name"], "barca.step");
            assert_eq!(child["meta"]["barca.run_id"], "r1");
        }
        assert_eq!(spans[1]["resource"], "p.py:orders");
        assert_eq!(spans[1]["meta"]["barca.outcome"], "cached");
        assert_eq!(spans[1]["error"], 0);
        assert_eq!(spans[1]["metrics"]["barca.bytes"], 42);
        assert_eq!(spans[2]["meta"]["barca.kind"], "task");
        assert_eq!(spans[2]["error"], 1);
        assert_eq!(spans[2]["meta"]["error.type"], "ValueError");
        assert_ne!(spans[1]["span_id"], spans[2]["span_id"]);
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
        assert!(request.starts_with("PUT /v0.3/traces HTTP/1.1\r\nHost: agent:8126\r\n"));
        assert!(request.contains("Content-Length: 2\r\n"));

        let (bad, _) = exchange("HTTP/1.1 415 Unsupported Media Type\r\n\r\n").await;
        assert!(bad.unwrap_err().contains("415"));
    }
}
