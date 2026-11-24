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
pub(crate) use crate::run_model::RunSpec;
use crate::state::AppState;
use barca_service::BarcaError;
use barca_service::envelope::Context;
use barca_service::outcome::RunCommand;
use std::time::Duration;

mod admission;
mod handlers;
mod lifecycle;
mod projection;
#[cfg(test)]
mod tests;

pub(crate) use admission::{admit, admit_as, dispatch, ensure_db, resume_at_startup};
pub(crate) use handlers::{
    cancel, create, detail, events, get_target, in_flight_now, list, logs, output, replay_run,
    run_all, run_target, status,
};
#[cfg(test)]
pub(crate) use lifecycle::start;
pub(crate) use lifecycle::{evict_finished_runs, launch, record_tick_finished, start_as};

/// How long a run may take, and which setting said so: `Schedule(..., timeout_seconds=N)` for
/// a scheduled tick > `--run-timeout` > `[server] run_timeout_seconds` > no limit (#628). 0 at
/// any of them means no limit. The time counts from when the run leaves the queue.
pub(crate) fn run_timeout(state: &AppState, spec: &RunSpec) -> Option<(Duration, String)> {
    let (secs, source) = match (spec.timeout, &spec.rules.tick) {
        (Some(secs), Some(node)) => (
            secs,
            format!(
                "Schedule(timeout_seconds=) of {}",
                barca_service::telemetry::short_name(node)
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

/// Check a trigger's target before a run is started for it, with the function `barca get`
/// and `barca run` use (`barca_service::targets::resolve_target_among`), so the server refuses what the
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
    let dag = match if !state.config.python.is_absolute() || state.config.python.is_file() {
        barca_service::load::build_static_dag(&state.config.files).await
    } else {
        Err(BarcaError::Other("Python unavailable".into()))
    } {
        Ok(dag) => std::sync::Arc::new(dag),
        // Preserve partial-deployment diagnostics when an unrelated source is broken.
        Err(_) => state.reads.loaded_dag().await?,
    };
    let nodes: Vec<(String, barca_service::NodeKind)> =
        barca_service::queries::list_assets_from_dag(&dag)
            .into_iter()
            .map(|n| (n.id, n.kind))
            .collect();
    let nodes = nodes.iter().map(|(id, kind)| (id.as_str(), *kind));
    match barca_service::targets::resolve_target_among(nodes, name, verb) {
        Ok(node) => Ok(node),
        // The CLI's error and remediation (`Run `barca list` ...`).
        Err(barca_service::targets::TargetError::NotFound { name, available }) => {
            Err(ApiError::run(
                BarcaError::AssetNotFound(name, available.join(", ")),
                Context {
                    command: verb_command(verb),
                    files: Vec::new(),
                },
            ))
        }
        // The CLI's error and remediation: `barca list` for an ambiguous name, `barca <cmd>
        // --help` for the wrong kind (#492).
        Err(e @ barca_service::targets::TargetError::Ambiguous { .. }) => Err(ApiError::run(
            BarcaError::from(e),
            Context {
                command: verb_command(verb),
                files: Vec::new(),
            },
        )),
        Err(e @ barca_service::targets::TargetError::WrongKind { .. })
            if wording == Wording::Cli =>
        {
            Err(ApiError::run(
                BarcaError::from(e),
                Context {
                    command: verb_command(verb),
                    files: Vec::new(),
                },
            ))
        }
        Err(barca_service::targets::TargetError::WrongKind { name, kind }) => {
            let (what, endpoint) = match kind {
                barca_service::NodeKind::Task => ("a task", "run"),
                _ => ("an asset", "get"),
            };
            Err(ApiError::BadRequest(format!(
                "'{name}' is {what}: use POST /{endpoint}/{name}"
            )))
        }
    }
}

pub(crate) fn verb_command(verb: &str) -> &'static str {
    match verb {
        "run" => "run",
        _ => "get",
    }
}

/// Refuse a run of every asset and sensor when no asset or sensor loaded.
pub(crate) async fn check_any_asset(state: &AppState) -> Result<(), ApiError> {
    let dag = match if !state.config.python.is_absolute() || state.config.python.is_file() {
        barca_service::load::build_static_dag(&state.config.files).await
    } else {
        Err(BarcaError::Other("Python unavailable".into()))
    } {
        Ok(dag) => std::sync::Arc::new(dag),
        // Preserve partial-deployment diagnostics when an unrelated source is broken.
        Err(_) => state.reads.loaded_dag().await?,
    };
    if !barca_service::queries::list_assets_from_dag(&dag)
        .iter()
        .any(|n| n.kind != barca_service::NodeKind::Task)
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

/// The envelope context of a local command with these files.
fn context(command: RunCommand, files: &[String]) -> Context {
    Context {
        command: command.as_str(),
        files: files.to_vec(),
    }
}
