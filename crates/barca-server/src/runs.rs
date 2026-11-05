//! The run contract (RFC-0007 §3.4 "Runs"): `POST /runs` starts a run on this server,
//! `GET /runs/{run_id}` shows it (from the metadata DB once it has finished, so the answer
//! survives a restart), `GET /runs/{run_id}/events` streams it, `DELETE /runs/{run_id}`
//! cancels it and `GET /runs/{run_id}/output` serves its final artifact. The run id is the
//! database run id, allocated when the run is accepted.
//!
//! `GET /runs` lists durable history joined with this server's live runs. The older trigger
//! routes (`POST /run`, `POST /run/{target}`, `POST /get/{target}`, `GET /status/{run_id}`,
//! `GET /events/{run_id}`, `DELETE /run/{run_id}`) are deprecated aliases retained for Python Client compatibility.
//!
//! This file holds what every part shares: the run spec, the refusals and the target checks.
//! The HTTP handlers are [`handlers`], a run's life from the queue to its recorded outcome is
//! [`lifecycle`], and what the routes answer with (one run, the run list) is [`projection`].

use crate::error::ApiError;
use crate::state::AppState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use barca_core::BarcaError;
use barca_core::cache::CachePolicy;
use barca_core::envelope::Context;
use barca_core::outcome::{RunCommand, Trigger};
use std::time::Duration;

mod handlers;
mod lifecycle;
mod projection;
#[cfg(test)]
mod tests;

pub(crate) use handlers::{
    cancel, create, detail, events, get_target, list, logs, output, run_all, run_target, status,
};
pub(crate) use lifecycle::{evict_finished_runs, launch, start};
pub(crate) use projection::{durable_timestamp, timestamp};

/// How long a run may take, and which setting said so: `Schedule(..., timeout_seconds=N)` for
/// a scheduled tick > `--run-timeout` > `[server] run_timeout_seconds` > no limit (#628). 0 at
/// any of them means no limit. The time counts from when the run leaves the queue.
pub(crate) fn run_timeout(state: &AppState, spec: &RunSpec) -> Option<(Duration, String)> {
    let (secs, source) = match (spec.timeout, &spec.rules.tick) {
        (Some(secs), Some(node)) => (
            secs,
            format!(
                "Schedule(timeout_seconds=) of {}",
                barca_core::telemetry::short_name(node)
            ),
        ),
        _ => {
            let (secs, source) = state.config.resolved.server_run_timeout_secs?;
            (secs, source.to_string())
        }
    };
    (secs > 0).then(|| (Duration::from_secs(secs), source))
}

// ─── starting runs ──────────────────────────────────────────────────────────

/// A run to start: what to run, how, and what asked for it.
pub(crate) struct RunSpec {
    pub command: RunCommand,
    /// As given (`[]`: every asset and sensor).
    pub targets: Vec<String>,
    /// The full node ids the targets resolved to.
    pub nodes: Vec<String>,
    /// As given in the request (`[]`: the deployed set).
    pub files: Vec<String>,
    pub policy: CachePolicy,
    pub trigger: Trigger,
    /// `get -o value`: the outcome's `final_output` holds values, not pointers.
    pub values: bool,
    /// `get --partition`.
    pub partition: Option<barca_core::selection::PartitionSelection>,
    /// `--batch` (RFC-0013), each as the flag takes it.
    pub batch: Vec<String>,
    /// The freshness rules (RFC-0008): a `Manual` node read as it is (`POST /runs`, as the CLI
    /// does), and whether this is a scheduled tick.
    pub rules: barca_core::freshness::RunRules,
    /// `Schedule(..., timeout_seconds=N)` of the tick that started it: how long the run may
    /// take (0: no limit). `None`: the server's run timeout ([`run_timeout`]).
    pub timeout: Option<u64>,
    /// How many times a server killed during its drain left this run unfinished, and the next
    /// server put it back in the queue ([`crate::queue::resume_at_startup`]).
    pub kill_requeues: u32,
}

impl RunSpec {
    /// A run started over HTTP.
    pub fn api(command: RunCommand, targets: Vec<String>, nodes: Vec<String>) -> Self {
        Self {
            command,
            targets,
            nodes,
            files: Vec::new(),
            policy: CachePolicy::CacheAware,
            trigger: Trigger::api(),
            values: false,
            partition: None,
            batch: Vec::new(),
            // The older trigger routes compute what they are asked for, as before RFC-0008.
            rules: barca_core::freshness::RunRules::default(),
            timeout: None,
            kill_requeues: 0,
        }
    }
}

/// Refuse a request while the server drains before it stops: `503` with `Retry-After`
/// (`POST /state/reset`). Runs are not refused: they join the queue, which the next server
/// resumes.
pub(crate) fn refuse_if_draining(state: &AppState) -> Result<(), ApiError> {
    if state.is_draining() {
        return Err(ApiError::Unavailable {
            message: "this server is shutting down: it finishes the runs in flight and \
                      accepts no new ones; retry against the server that replaces it"
                .to_string(),
            retry_after: state.retry_after_secs(),
        });
    }
    Ok(())
}

/// Refuse a request that would run or cancel work on a `--read-only` server.
pub(crate) fn refuse_if_read_only(state: &AppState) -> Result<(), ApiError> {
    if state.config.read_only {
        return Err(ApiError::Forbidden(
            "this server is read-only (`barca serve --read-only`): it does not run or cancel work"
                .to_string(),
        ));
    }
    Ok(())
}

/// Check a trigger's target before a run is started for it, with the function `barca get`
/// and `barca run` use (`barca_core::targets::resolve_target_among`), so the server refuses what the
/// command line refuses, in the same words: an unknown name is `404`, a name that matches
/// several nodes `409`, the wrong verb for the node's kind `400`. So is source that does not
/// parse or a DAG that cannot be built. Returns the full node id the name resolved to.
pub(crate) async fn check_target(
    state: &AppState,
    name: &str,
    verb: &str,
) -> Result<String, ApiError> {
    check_target_as(state, name, verb, Wording::Cli).await
}

/// How a wrong-kind refusal names the fix: the CLI's words (`use barca run instead`, which a
/// `--server` client prints as a local run would), or the older routes' (`use POST /run/x`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wording {
    Cli,
    Route,
}

async fn check_target_as(
    state: &AppState,
    name: &str,
    verb: &str,
    wording: Wording,
) -> Result<String, ApiError> {
    let dag = state.loaded_dag().await?;
    let nodes: Vec<(String, barca_core::NodeKind)> =
        barca_core::queries::list_assets_from_dag(&dag)
            .into_iter()
            .map(|n| (n.id, n.kind))
            .collect();
    let nodes = nodes.iter().map(|(id, kind)| (id.as_str(), *kind));
    match barca_core::targets::resolve_target_among(nodes, name, verb) {
        Ok(node) => Ok(node),
        // The CLI's error and remediation (`Run `barca list` ...`).
        Err(barca_core::targets::TargetError::NotFound { name, available }) => Err(ApiError::run(
            BarcaError::AssetNotFound(name, available.join(", ")),
            Context {
                command: verb_command(verb),
                files: Vec::new(),
            },
        )),
        // The CLI's error and remediation: `barca list` for an ambiguous name, `barca <cmd>
        // --help` for the wrong kind (#492).
        Err(e @ barca_core::targets::TargetError::Ambiguous { .. }) => Err(ApiError::run(
            BarcaError::from(e),
            Context {
                command: verb_command(verb),
                files: Vec::new(),
            },
        )),
        Err(e @ barca_core::targets::TargetError::WrongKind { .. }) if wording == Wording::Cli => {
            Err(ApiError::run(
                BarcaError::from(e),
                Context {
                    command: verb_command(verb),
                    files: Vec::new(),
                },
            ))
        }
        Err(barca_core::targets::TargetError::WrongKind { name, kind }) => {
            let (what, endpoint) = match kind {
                barca_core::NodeKind::Task => ("a task", "run"),
                _ => ("an asset", "get"),
            };
            Err(ApiError::BadRequest(format!(
                "'{name}' is {what}: use POST /{endpoint}/{name}"
            )))
        }
    }
}

fn verb_command(verb: &str) -> &'static str {
    match verb {
        "run" => "run",
        _ => "get",
    }
}

/// Refuse a run of every asset and sensor when no asset or sensor loaded.
pub(crate) async fn check_any_asset(state: &AppState) -> Result<(), ApiError> {
    let dag = state.loaded_dag().await?;
    if !barca_core::queries::list_assets_from_dag(&dag)
        .iter()
        .any(|n| n.kind != barca_core::NodeKind::Task)
    {
        return Err(ApiError::BadRequest(
            "no loaded assets or sensors to run; inspect /health load_errors".into(),
        ));
    }
    Ok(())
}

/// The request's files, each one of the deployed files, spelled as the deployment spells it (so
/// node ids are the deployment's); a usage error listing the deployed files otherwise.
fn check_files(state: &AppState, files: &[String]) -> Result<Vec<String>, ApiError> {
    let canonical = |f: &str| std::fs::canonicalize(f).ok();
    let deployed: Vec<(std::path::PathBuf, &String)> = state
        .config
        .files
        .iter()
        .filter_map(|f| canonical(f).map(|c| (c, f)))
        .collect();
    let mut chosen = Vec::new();
    for file in files {
        let found = canonical(file).and_then(|c| {
            deployed
                .iter()
                .find(|(d, _)| *d == c)
                .map(|(_, f)| (*f).clone())
        });
        match found {
            Some(f) if !chosen.contains(&f) => chosen.push(f),
            Some(_) => {}
            None => {
                return Err(ApiError::BadRequest(format!(
                    "'{file}' is not one of the deployed files\nThe server runs its deployed files: {}. Leave `files` empty to use all of them.",
                    state.config.files.join(", ")
                )));
            }
        }
    }
    Ok(chosen)
}

/// The files a run reads: those the request named, or every deployed file.
fn run_files<'a>(state: &'a AppState, files: &'a [String]) -> &'a [String] {
    if files.is_empty() {
        &state.config.files
    } else {
        files
    }
}

/// The stdout text of a rendering, without its final newline.
pub(crate) fn stdout_of(rendered: barca_core::report::RenderedOutput) -> String {
    let mut text: String = rendered
        .chunks
        .into_iter()
        .filter_map(|c| match c {
            barca_core::report::OutputChunk::Stdout(s) => Some(s),
            barca_core::report::OutputChunk::Stderr(_) => None,
        })
        .collect();
    if text.ends_with('\n') {
        text.pop();
    }
    text
}

/// A JSON body sent as it is (its keys in the order the CLI prints them).
pub(crate) fn json_text(status: StatusCode, text: String) -> Response {
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        text,
    )
        .into_response()
}

/// The envelope context of a local command with these files.
fn context(command: RunCommand, files: &[String]) -> Context {
    Context {
        command: command.as_str(),
        files: files.to_vec(),
    }
}
