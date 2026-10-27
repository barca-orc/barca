//! Republishing the sinks of cache hits (#662).
//!
//! A step served from cache does not run, so its `@sink`s are not written by a worker. Each
//! sink records the run hash it last published (`crate::sink_publications`); a cache hit whose
//! run hash differs (code A -> B -> A) is republished from the cached artifact by
//! `python -m barca._sinks republish`, without running the step. One that matches writes
//! nothing. A sink that cannot be republished fails its step, as it does when the step runs.

use super::{RunSession, Work, note};
use crate::BarcaError;
use crate::recover;
use crate::sink_publications::{Publication, Verdict};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, serde::Deserialize)]
struct Answer {
    items: Vec<ItemAnswer>,
}

#[derive(Debug, serde::Deserialize)]
struct ItemAnswer {
    node_id: String,
    sinks: Vec<SinkAnswer>,
}

#[derive(Debug, serde::Deserialize)]
struct SinkAnswer {
    path: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    warning: Option<String>,
}

/// Republish the stale sinks of the cached outputs `ids`.
pub(super) async fn republish_sinks<'p>(
    session: &mut RunSession<'_, '_>,
    mut ids: Vec<String>,
    queue: &mut VecDeque<Work<'p>>,
) -> Result<(), BarcaError> {
    // One may have been computed again since this was queued: running wrote its sinks.
    ids.retain(|id| session.state.cached_node_ids.contains(id));
    if ids.is_empty() {
        return Ok(());
    }
    let dag = &session.prepared.dag;
    let mut wanted: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for id in &ids {
        let base = recover::base_of(id);
        if let Some(node) = dag.get_node(base) {
            let pairs = wanted.entry(base.to_string()).or_default();
            pairs.extend(
                node.extracted
                    .sinks
                    .iter()
                    .map(|s| (id.clone(), s.path.clone())),
            );
        }
    }
    let published =
        crate::sink_publications::published(&session.request.cfg.db_path, &wanted).await?;
    let host = crate::db::local_host();
    let run_hashes = session.state.decide_state.run_hashes();
    let mut stale: Vec<(String, Vec<crate::model::SinkDecl>)> = Vec::new();
    for id in &ids {
        let (Some(run_hash), Some(node)) = (run_hashes.get(id), dag.get_node(recover::base_of(id)))
        else {
            continue;
        };
        let mut sinks = Vec::new();
        for sink in &node.extracted.sinks {
            match published.verdict(&host, id, &sink.path, run_hash) {
                Verdict::Current => {}
                Verdict::Adopt => session.state.results.publications.push(Publication {
                    node_id: id.clone(),
                    sink: sink.path.clone(),
                    run_hash: run_hash.clone(),
                }),
                Verdict::Stale => sinks.push(sink.clone()),
            }
        }
        if !sinks.is_empty() {
            stale.push((id.clone(), sinks));
        }
    }
    if stale.is_empty() {
        return Ok(());
    }

    // The cached artifacts are read: fetched from the store when this machine lacks them, and
    // computed again (which writes the sinks) when they are nowhere.
    let paths: Vec<String> = stale
        .iter()
        .filter_map(|(id, _)| session.state.results.outputs.get(id))
        .map(|o| o.path.clone())
        .collect();
    let lost = recover::lost(
        &mut session.store,
        &paths,
        &paths,
        session.pb.as_ref(),
        &session.state.results.outputs,
        &session.state.cached_node_ids,
        &mut session.state.cached_steps,
    )
    .await;
    match lost {
        Ok(lost) if lost.is_empty() => {}
        Ok(lost) => {
            queue.push_front(Work::Republish(
                stale.into_iter().map(|(id, _)| id).collect(),
            ));
            queue.push_front(Work::Recompute(
                session.state.cached_steps.first_layer(&lost),
            ));
            return Ok(());
        }
        Err(e) => {
            session.state.transfer_error = Some(e);
            return Ok(());
        }
    }
    session.state.read_paths.extend(paths);

    let items: Vec<serde_json::Value> = stale
        .iter()
        .filter_map(|(id, sinks)| {
            let output = session.state.results.outputs.get(id)?;
            let node = dag.get_node(recover::base_of(id))?;
            let partition: BTreeMap<String, String> = crate::StepId::parse(id).partition.0;
            Some(serde_json::json!({
                "node_id": id,
                "path": output.path,
                "format": output.format,
                "frame_type": node.extracted.return_type.map(|t| t.as_str()),
                "source_file": node.source_file(),
                "partition": partition,
                "sinks": sinks,
            }))
        })
        .collect();
    let mut cmd = crate::helper_proc::python_module(
        session.request.python,
        "barca._sinks",
        session.request.cfg.storage_options_json.as_deref(),
    );
    cmd.arg("republish").kill_on_drop(true);
    let answer =
        crate::helper_proc::run_json::<Answer>(cmd, &serde_json::json!({ "items": items }), None)
            .await;
    if session.started.cancel.is_cancelled() {
        return Ok(());
    }
    let answers: Vec<ItemAnswer> = match answer {
        Ok(answer) => answer.items,
        Err(e) => {
            let why = match e {
                crate::helper_proc::JsonFailed::Spawn(e)
                | crate::helper_proc::JsonFailed::Io(e) => e.to_string(),
                crate::helper_proc::JsonFailed::TimedOut(limit) => {
                    format!("it did not finish within {}s", limit.as_secs())
                }
                crate::helper_proc::JsonFailed::Exited(stderr) => {
                    crate::helper_proc::helper_cause(&stderr)
                }
                crate::helper_proc::JsonFailed::Invalid(e) => e.to_string(),
            };
            stale
                .iter()
                .map(|(id, sinks)| ItemAnswer {
                    node_id: id.clone(),
                    sinks: sinks
                        .iter()
                        .map(|s| SinkAnswer {
                            path: s.path.clone(),
                            error: Some(format!("the sink helper failed: {why}")),
                            warning: None,
                        })
                        .collect(),
                })
                .collect()
        }
    };

    // (sink, node) -> how many of the node's steps it was republished for.
    let mut done: BTreeMap<(String, String), usize> = BTreeMap::new();
    for item in answers {
        let Some(run_hash) = session
            .state
            .decide_state
            .run_hashes()
            .get(&item.node_id)
            .cloned()
        else {
            continue;
        };
        let mut errors = Vec::new();
        for sink in item.sinks {
            if let Some(warning) = &sink.warning {
                note(&session.pb, &format!("[barca] warning: {warning}"));
            }
            match sink.error {
                Some(error) => errors.push(format!("sink {}: {error}", sink.path)),
                None => {
                    *done
                        .entry((
                            sink.path.clone(),
                            recover::base_of(&item.node_id).to_string(),
                        ))
                        .or_default() += 1;
                    session.state.results.publications.push(Publication {
                        node_id: item.node_id.clone(),
                        sink: sink.path,
                        run_hash: run_hash.clone(),
                    });
                }
            }
        }
        if !errors.is_empty() {
            fail_republish(session, &item.node_id, errors.join("; "));
        }
    }
    for ((sink, base), count) in done {
        let name = crate::targets::short_name(&base);
        let what = if session
            .prepared
            .dag
            .get_node(&base)
            .is_some_and(|n| !n.extracted.partitions.is_empty())
        {
            format!(
                "{count} cached {} of {name}",
                if count == 1 { "key" } else { "keys" }
            )
        } else {
            format!("the cached result of {name}")
        };
        note(
            &session.pb,
            &format!("[barca] sink {sink}: republished {what}"),
        );
    }
    Ok(())
}

/// A cache hit whose sink could not be republished fails its step, as a sink that fails when
/// the step runs does: exit 1, a `step_failed` envelope, and its downstream does not run.
fn fail_republish(session: &mut RunSession<'_, '_>, id: &str, message: String) {
    let message = format!("SinkError: {message}");
    if session.request.agent_mode {
        crate::run_line!(Agent, "{}", crate::report::failed_step_line(id, &message));
    }
    session
        .state
        .failed_bases
        .insert(recover::base_of(id).to_string());
    session.state.sink_failed = true;
    if session.state.step_failure.is_none() {
        session.state.step_failure = Some((id.to_string(), message.clone()));
        session.state.step_failure_phase = Some(crate::dispatch::ErrorPhase::Sink);
    }
    session.state.results.record_failure(
        id.to_string(),
        "SinkError",
        message,
        1,
        None,
        Some(crate::dispatch::ErrorPhase::Sink),
    );
}
