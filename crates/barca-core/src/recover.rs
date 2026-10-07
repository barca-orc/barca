//! Recomputing a cached step whose artifact is gone (#252).
//!
//! A cache hit is a row in the metadata DB; the artifact it names can be deleted behind barca's
//! back (a disk cleanup, a container restarted without a volume, `rm -r .barca/artifacts`). The
//! rule, shared by a real run and `--dry-run` / `barca status`:
//!
//! - A missing artifact matters only when something is about to **read** it: a step that is
//!   going to run takes it as an input, a `partitions_from` step is expanded from it, or it is
//!   the output the command returns (see [`requested`]). Anything else, a pruned intermediate
//!   or an asset at the end of the pipeline that is not returned, stays cached and costs
//!   nothing.
//! - "Missing" means not on this machine's disk and not in the artifact store. For a row
//!   recorded in the store, the readable copy is its local mirror; the store is asked only when
//!   the mirror is absent. With a store, nothing is recomputed unless the store itself is known
//!   to be there (see [`lost`]): a store that is gone, misnamed or unreachable is a failed run,
//!   never a reason to compute everything again and write it somewhere new.
//! - The step that produced a needed, missing artifact is run again, with reason
//!   `artifact_missing`. Its run hash is unchanged, so the artifact lands at the same path and
//!   nothing downstream is invalidated. Its own inputs are subject to the same rule.
//!
//! `commands::execute` applies it where a phase's inputs are made available;
//! `commands::explain_dag` predicts it with [`predict_recomputes`].

use crate::commands::{ExplainSummary, PartitionSummary, RunReason, StepReport, StoreSync};
use crate::dispatch::{OutputRef, ProvidedInput, build_provided_inputs};
use crate::model::NodeKind;
use crate::planner::{ExecutionPlan, Phase, PhaseReason, StreamStep, WorkerStream};
use crate::transfer::ArtifactLayout;
use std::collections::{HashMap, HashSet};

/// What a run does next (see the loop in `commands::execute`).
pub(crate) enum Work<'p> {
    /// A phase of the plan: expand its partitions, decide each step, run what is not cached.
    Planned(&'p Phase),
    /// Steps already decided to run, waiting for an input to be computed again.
    Ready(Phase),
    /// Cached outputs (by id) whose artifact is missing and needed: run their steps again.
    Recompute(Vec<String>),
    /// Every phase is done: check the output the command returns.
    Returned,
}

/// Whether the artifact recorded at `path` can be read from this machine as it is.
///
/// A file path (plain or `file://`) is looked up on disk. A remote URI cannot be: with a
/// separate artifact store (`separate_store`) workers read only the local artifact directory,
/// so a URI that is not in that store is out of reach; without one, the worker reads the URI
/// itself and reports what it finds, so it is taken as readable.
///
/// An artifact is one file. A directory at the path, or a symlink that leads to one or to
/// nothing, is not an artifact and counts as missing: the step is computed again, and whoever
/// writes the file moves the directory out of the way first (`barca._storage.make_way`).
pub(crate) fn on_disk(path: &str, separate_store: bool) -> bool {
    match crate::transfer::local_path(path) {
        Some(file) => file.is_file(),
        None => !separate_store,
    }
}

/// `test` of every item, in order. A handful is done in place; many are spread over a few
/// threads, because each test is a file lookup and a partitioned asset has one per key.
pub(crate) fn flags<T: Sync>(items: &[T], test: impl Fn(&T) -> bool + Sync) -> Vec<bool> {
    const SERIAL_BELOW: usize = 512;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    if items.len() < SERIAL_BELOW || threads < 2 {
        return items.iter().map(test).collect();
    }
    let chunk = items.len().div_ceil(threads);
    let test = &test;
    std::thread::scope(|scope| {
        let workers: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(test).collect::<Vec<bool>>()))
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("a file lookup does not panic"))
            .collect()
    })
}

/// The base node id of a step or output id (`p.py:a[k=1]` -> `p.py:a`).
pub(crate) fn base_of(id: &str) -> &str {
    id.split('[').next().unwrap_or(id)
}

/// The step whose value a run with no target returns as `final_output`: the last planned asset
/// (a sensor or task only when the plan has no asset).
pub(crate) fn returned_step(plan: &ExecutionPlan) -> Option<&StreamStep> {
    let steps = || {
        plan.phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
    };
    steps()
        .rev()
        .find(|st| st.kind == NodeKind::Asset)
        .or_else(|| steps().next_back())
}

/// The base ids of the outputs a command returns to its caller: its targets, or with no target
/// the one asset whose value is the run's `final_output`. Their artifacts are needed even
/// though no step reads them. Every other asset at the end of a pipeline is treated like a
/// pruned intermediate.
pub(crate) fn requested(plan: &ExecutionPlan, target_ids: &[&str]) -> HashSet<String> {
    if !target_ids.is_empty() {
        return target_ids.iter().map(|id| id.to_string()).collect();
    }
    returned_step(plan)
        .filter(|st| st.kind == NodeKind::Asset)
        .map(|st| st.step_id.base_id().to_string())
        .into_iter()
        .collect()
}

/// The paths a phase's provided inputs are read from.
pub(crate) fn input_paths(provided: &HashMap<String, ProvidedInput>) -> Vec<String> {
    provided
        .values()
        .flat_map(|p| match p {
            ProvidedInput::Single(o) => std::slice::from_ref(o),
            ProvidedInput::Collected(v) => v.as_slice(),
        })
        .map(|o| o.path.clone())
        .collect()
}

/// The ids of the outputs a step produces: its own, or one per partition key.
pub(crate) fn output_ids(step: &StreamStep) -> Vec<String> {
    if step.partition_keys.is_empty() {
        vec![step.step_id.display()]
    } else {
        step.partition_keys
            .iter()
            .map(|pk| pk.display_id(&step.step_id.base))
            .collect()
    }
}

/// Remove from `phase` every step for which `blocked` names a failed step upstream of it
/// (given the step's base id), calling `dropped` with the step and that upstream. Streams left
/// empty are removed. Used when several targets keep going around a failure.
pub(crate) fn drop_blocked<'a>(
    phase: &mut Phase,
    blocked: impl Fn(&str) -> Option<&'a str>,
    mut dropped: impl FnMut(&StreamStep, &'a str),
) {
    for stream in &mut phase.streams {
        stream
            .steps
            .retain(|st| match blocked(st.step_id.base_id()) {
                Some(upstream) => {
                    dropped(st, upstream);
                    false
                }
                None => true,
            });
    }
    phase.streams.retain(|s| !s.steps.is_empty());
}

/// The ids (sorted) of the outputs among `outputs` that are at one of `paths` and that
/// `is_cached` says were served from cache: the steps to run again when those paths are gone.
pub(crate) fn outputs_at(
    paths: &HashSet<&str>,
    outputs: &HashMap<String, OutputRef>,
    mut is_cached: impl FnMut(&str) -> bool,
) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut ids: Vec<String> = outputs
        .iter()
        .filter(|(id, o)| paths.contains(o.path.as_str()) && is_cached(id))
        .map(|(id, _)| id.clone())
        .collect();
    ids.sort();
    ids
}

/// The steps a run served from cache, kept so that one can be run again if its artifact turns
/// out to be missing when something needs it.
///
/// Remembering a step costs nothing beyond keeping it: the lookup table by output id is built
/// only when something is first looked up, so a run that recovers nothing never pays for it,
/// and one that does pays once per step, not once per lookup.
#[derive(Default)]
pub(crate) struct CachedSteps {
    /// The planned steps served from cache (a partitioned node is planned as several steps,
    /// each holding some of its keys).
    steps: Vec<StreamStep>,
    /// Output id -> (index into `steps`, index of the partition key when expanded by key),
    /// for the first `indexed` steps.
    index: HashMap<String, (usize, Option<usize>)>,
    indexed: usize,
}

impl CachedSteps {
    /// Remember a step that was served from cache, wholly or for some of its partition keys.
    pub(crate) fn remember(&mut self, step: StreamStep) {
        self.steps.push(step);
    }

    /// Bring the lookup table up to date with the steps remembered since it was last used.
    fn reindex(&mut self) {
        for (slot, step) in self.steps.iter().enumerate().skip(self.indexed) {
            if step.partition_keys.is_empty() {
                self.index.insert(step.step_id.display(), (slot, None));
            } else {
                for (key, pk) in step.partition_keys.iter().enumerate() {
                    self.index
                        .insert(pk.display_id(&step.step_id.base), (slot, Some(key)));
                }
            }
        }
        self.indexed = self.steps.len();
    }

    /// Whether the step that produced output `id` is remembered.
    pub(crate) fn knows(&mut self, id: &str) -> bool {
        self.reindex();
        self.index.contains_key(id)
    }

    /// Of the cached outputs `ids`, those whose step reads none of the others. Run these
    /// first: a step never shares a phase with the producer of one of its inputs, so each
    /// reads complete inputs (a `collect()` consumer would otherwise be handed only the keys
    /// that were still cached).
    pub(crate) fn first_layer(&mut self, ids: &[String]) -> Vec<String> {
        self.reindex();
        let bases: HashSet<&str> = ids.iter().map(|id| base_of(id)).collect();
        let first: Vec<String> = ids
            .iter()
            .filter(|id| {
                let own = base_of(id);
                self.index.get(id.as_str()).is_none_or(|(slot, _)| {
                    !self.steps[*slot]
                        .inputs
                        .values()
                        .map(|up| base_of(up))
                        .any(|up| up != own && bases.contains(up))
                })
            })
            .cloned()
            .collect();
        // A DAG always has such a step; never return nothing for something.
        if first.is_empty() {
            ids.to_vec()
        } else {
            first
        }
    }

    /// A phase that runs again exactly the steps that produced `ids`: one stream per node, a
    /// partitioned node with only the keys named. Ids that are not remembered are left out;
    /// `None` when that leaves nothing.
    pub(crate) fn phase_for(&mut self, ids: &[String]) -> Option<Phase> {
        self.reindex();
        let mut order: Vec<&str> = Vec::new();
        let mut steps: HashMap<&str, StreamStep> = HashMap::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for id in ids {
            let Some(&(slot, key)) = self.index.get(id.as_str()) else {
                continue;
            };
            if !seen.insert(id.as_str()) {
                continue;
            }
            let found = &self.steps[slot];
            let Some(key) = key else {
                // Not expanded by key: the step is its own unit of work.
                order.push(id.as_str());
                steps.insert(id.as_str(), found.clone());
                continue;
            };
            let step = steps.entry(base_of(id)).or_insert_with(|| {
                order.push(base_of(id));
                // Everything but the keys, which are copied one at a time below.
                StreamStep {
                    step_id: found.step_id.clone(),
                    kind: found.kind,
                    function_name: found.function_name.clone(),
                    source_file: found.source_file.clone(),
                    inputs: found.inputs.clone(),
                    pending_partitions: found.pending_partitions.clone(),
                    serializer: found.serializer.clone(),
                    sinks: found.sinks.clone(),
                    run_hashes: HashMap::new(),
                    timeout_seconds: found.timeout_seconds,
                    retries: found.retries,
                    retry_backoff_seconds: found.retry_backoff_seconds,
                    partition_keys: Vec::new(),
                    param_types: found.param_types.clone(),
                    return_type: found.return_type,
                }
            });
            step.partition_keys.push(found.partition_keys[key].clone());
            if let Some(h) = found.run_hashes.get(id) {
                step.run_hashes.insert(id.clone(), h.clone());
            }
        }
        if order.is_empty() {
            return None;
        }
        Some(Phase {
            reason: PhaseReason::Initial,
            streams: order
                .into_iter()
                .enumerate()
                .map(|(i, key)| WorkerStream {
                    stream_id: format!("recompute-{i}"),
                    steps: vec![steps.remove(key).expect("inserted with its order entry")],
                })
                .collect(),
        })
    }
}

/// Real run: the cached outputs among `check` (artifact paths) that have to be computed again
/// because their artifact cannot be read here: it is not on this disk and, for a result
/// recorded in the artifact store, not in the store either. The store is asked only about
/// `fetch`, which are downloaded as a side effect; any other store-backed artifact is taken to
/// be there, so nothing is downloaded that no one reads.
///
/// With a store, an artifact counts as missing only when the store itself answers: before
/// anything is recomputed the store is confirmed to be there and listable (once per run). A
/// store that is gone, misnamed or unreachable is an error, as is any fetch that fails for
/// another reason; the run then fails without computing or uploading anything on its account.
pub(crate) async fn lost(
    store: &mut Option<StoreSync>,
    check: &[String],
    fetch: &[String],
    pb: Option<&indicatif::ProgressBar>,
    all_outputs: &HashMap<String, OutputRef>,
    cached_ids: &HashSet<String>,
    cached_steps: &mut CachedSteps,
) -> Result<Vec<String>, String> {
    let separate_store = store.is_some();
    let in_store = |path: &str| store.as_ref().is_some_and(|s| s.holds(path));
    let local: Vec<&str> = check
        .iter()
        .map(String::as_str)
        .filter(|path| !in_store(path))
        .collect();
    let here = flags(&local, |path| on_disk(path, separate_store));
    let mut gone: HashSet<&str> = local
        .into_iter()
        .zip(here)
        .filter(|(_, here)| !here)
        .map(|(path, _)| path)
        .collect();
    let missing_from_store = match store.as_mut() {
        Some(s) => {
            s.ensure_local(fetch.iter().map(String::as_str), pb).await?;
            s.take_missing()
        }
        None => Vec::new(),
    };
    gone.extend(missing_from_store.iter().map(String::as_str));
    let lost = outputs_at(&gone, all_outputs, |id| {
        cached_ids.contains(id) && cached_steps.knows(id)
    });
    if !lost.is_empty()
        && let Some(s) = store.as_mut()
    {
        s.confirm_present(&lost).await?;
    }
    Ok(lost)
}

/// The stderr lines that say which cached results are computed again because their artifact
/// is missing: one per node, naming one path.
pub(crate) fn recompute_warnings(lost: &[(String, String)]) -> Vec<String> {
    let mut nodes: Vec<(&str, &str, usize)> = Vec::new();
    let mut slot: HashMap<&str, usize> = HashMap::new();
    for (id, path) in lost {
        let base = base_of(id);
        match slot.get(base) {
            Some(&i) => nodes[i].2 += 1,
            None => {
                slot.insert(base, nodes.len());
                nodes.push((base, path, 1));
            }
        }
    }
    nodes
        .into_iter()
        .map(|(base, path, count)| {
            let others = match count - 1 {
                0 => String::new(),
                n => format!(" (and {n} more of its partitions)"),
            };
            format!(
                "[barca] warning: {base}: the artifact of its cached result is missing: \
                 {path}{others}. Computing it again."
            )
        })
        .collect()
}

/// Dry run: whether the artifact of cached output `oref` would be found missing by a run.
///
/// A dry run does not contact a remote artifact store: a result recorded in one counts as
/// available whether or not its local copy is here, so a real run that finds the object gone
/// computes a step the dry run reported as cached. A store that is a directory is looked at,
/// and as in a real run its artifact is missing only if the store directory itself is there.
fn predicted_gone(oref: &OutputRef, layout: Option<&ArtifactLayout>) -> bool {
    match layout.and_then(|l| l.local_for(&oref.path).map(|mirror| (l, mirror))) {
        // Recorded in the store: read from its local mirror, or fetched from the store.
        Some((layout, mirror)) => {
            !mirror.is_file()
                && crate::transfer::local_path(&oref.path).is_some_and(|stored| !stored.exists())
                && crate::transfer::local_path(layout.store_root())
                    .is_some_and(|root| root.is_dir())
        }
        None => !on_disk(&oref.path, layout.is_some()),
    }
}

/// Dry run: the cached steps a run would compute again because it reads one of the artifacts at
/// `needed` and finds it missing (and, for each such step, what it reads in turn). Their report
/// lines become `run` with reason `artifact_missing`, and they leave `all_outputs`.
pub(crate) fn predict_recomputes(
    needed: Vec<String>,
    cached_steps: &mut CachedSteps,
    all_outputs: &mut HashMap<String, OutputRef>,
    layout: Option<&ArtifactLayout>,
    steps: &mut [StepReport],
    summary: &mut ExplainSummary,
) {
    let gone = |oref: &&OutputRef| predicted_gone(oref, layout);
    for id in predict_lost(cached_steps, all_outputs, &needed, gone) {
        if mark_recomputed(steps, &id, true) {
            summary.cached = summary.cached.saturating_sub(1);
            summary.will_run += 1;
        }
    }
}

/// Dry run: the cached outputs that a run reading `needed` (artifact paths) would find missing
/// and compute again, followed through the inputs of each step it would then run. They are
/// removed from `outputs`, which holds the cached outputs only. `gone` says whether the
/// artifact of an output is unavailable.
pub(crate) fn predict_lost(
    cached: &mut CachedSteps,
    outputs: &mut HashMap<String, OutputRef>,
    needed: &[String],
    gone: impl Fn(&&OutputRef) -> bool + Sync,
) -> Vec<String> {
    let mut lost: Vec<String> = Vec::new();
    let mut needed: HashSet<String> = needed.iter().cloned().collect();
    loop {
        let read: Vec<&OutputRef> = outputs
            .values()
            .filter(|o| needed.contains(&o.path))
            .collect();
        let gone_paths: HashSet<&str> = read
            .iter()
            .zip(flags(&read, &gone))
            .filter(|(_, gone)| *gone)
            .map(|(o, _)| o.path.as_str())
            .collect();
        let layer = outputs_at(&gone_paths, outputs, |_| true);
        if layer.is_empty() {
            return lost;
        }
        for id in &layer {
            outputs.remove(id);
        }
        // What the steps now due to run read is needed in turn.
        needed = cached
            .phase_for(&layer)
            .map(|phase| {
                input_paths(&build_provided_inputs(&phase, outputs))
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
        lost.extend(layer);
    }
}

/// Change the report line of the step that produced cached output `id` to say that it is run
/// again because its artifact is missing. `dry` selects the vocabulary, as in a report: a dry
/// run says `run`, a real run says `ran`. Returns false when no cached line matches.
pub(crate) fn mark_recomputed(reports: &mut [StepReport], id: &str, dry: bool) -> bool {
    let base = base_of(id);
    let key = id
        .split_once('[')
        .map(|(_, rest)| rest.strip_suffix(']').unwrap_or(rest));
    let run = if dry { "run" } else { "ran" };
    let verdict = |r: &mut StepReport, word: &str| {
        if dry {
            r.action = Some(word.to_string());
        } else {
            r.status = Some(word.to_string());
        }
    };
    let reason = RunReason::ArtifactMissing;

    // A node expanded by partition key is one line with counts.
    let by_key =
        |r: &&mut StepReport| r.id == base && r.partitions.as_ref().is_some_and(|p| p.cached > 0);
    if let Some(key) = key
        && let Some(r) = reports.iter_mut().find(by_key)
    {
        let p: &mut PartitionSummary = r.partitions.as_mut().expect("matched on its partitions");
        p.cached -= 1;
        p.will_run += 1;
        if p.will_run_keys.len() < 20 {
            p.will_run_keys.push(key.to_string());
        }
        let word = if p.cached == 0 { run } else { "partial" };
        if r.reason.is_none() {
            r.reason = Some(reason.code().to_string());
            r.detail = Some(reason.detail());
        }
        verdict(r, word);
        return true;
    }

    let cached_line = |r: &&mut StepReport| {
        let said = if dry { &r.action } else { &r.status };
        r.id == base && r.partitions.is_none() && said.as_deref() == Some("cached")
    };
    let Some(r) = reports.iter_mut().find(cached_line) else {
        return false;
    };
    r.reason = Some(reason.code().to_string());
    r.detail = Some(reason.detail());
    r.artifact = None;
    // It is computed in this run after all, so it does reflect a refresh upstream of it.
    r.warning = None;
    verdict(r, run);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PartitionKey, StepId};
    use std::sync::Arc;

    fn step(id: &str, inputs: &[(&str, &str)], keys: &[&str]) -> StreamStep {
        let partition_keys: Vec<PartitionKey> = keys
            .iter()
            .map(|k| PartitionKey([("k".to_string(), k.to_string())].into_iter().collect()))
            .collect();
        let run_hashes = if keys.is_empty() {
            HashMap::from([(id.to_string(), format!("h-{id}"))])
        } else {
            partition_keys
                .iter()
                .map(|pk| (pk.display_id(id), format!("h-{}", pk.display_id(id))))
                .collect()
        };
        StreamStep {
            step_id: StepId::unpartitioned(id),
            kind: NodeKind::Asset,
            function_name: Arc::from(id.rsplit(':').next().unwrap()),
            source_file: Arc::from("p.py"),
            inputs: inputs
                .iter()
                .map(|(p, up)| (p.to_string(), up.to_string()))
                .collect(),
            pending_partitions: HashMap::new(),
            serializer: None,
            sinks: vec![],
            run_hashes,
            timeout_seconds: 0,
            retries: 1,
            retry_backoff_seconds: 0.0,
            partition_keys,
            param_types: HashMap::new(),
            return_type: None,
        }
    }

    fn oref(path: &str) -> OutputRef {
        OutputRef {
            path: path.to_string(),
            format: "json".to_string(),
            size_bytes: 1,
            elapsed_seconds: None,
            content_hash: None,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn cached_line(id: &str, dry: bool) -> StepReport {
        let mut r = StepReport {
            id: id.to_string(),
            kind: "asset".to_string(),
            artifact: Some("/a/x.json".to_string()),
            ..Default::default()
        };
        if dry {
            r.action = Some("cached".to_string());
        } else {
            r.status = Some("cached".to_string());
        }
        r
    }

    #[test]
    fn a_file_is_looked_up_and_a_uri_is_judged_by_who_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("h.json");
        std::fs::write(&here, "1").unwrap();
        let gone = dir.path().join("gone.json");
        for separate_store in [false, true] {
            assert!(on_disk(here.to_str().unwrap(), separate_store));
            assert!(on_disk(
                &format!("file://{}", here.display()),
                separate_store
            ));
            assert!(!on_disk(gone.to_str().unwrap(), separate_store));
        }
        // No separate store: the worker reads the URI itself. With one, it cannot.
        assert!(on_disk("s3://b/p/n/h.json", false));
        assert!(!on_disk("s3://other/p/n/h.json", true));
    }

    /// An artifact is one file (#249). Until 0.18 a directory at the path counted as present,
    /// and the step that read it then failed with `IsADirectoryError`.
    #[test]
    fn only_a_file_is_an_artifact_a_directory_or_a_dangling_link_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("h.json");
        std::fs::write(&file, "1").unwrap();
        let sub = dir.path().join("d.json");
        std::fs::create_dir(&sub).unwrap();
        let to_file = dir.path().join("to-file.json");
        let to_dir = dir.path().join("to-dir.json");
        let dangling = dir.path().join("dangling.json");
        std::os::unix::fs::symlink(&file, &to_file).unwrap();
        std::os::unix::fs::symlink(&sub, &to_dir).unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
        for separate_store in [false, true] {
            assert!(on_disk(file.to_str().unwrap(), separate_store));
            assert!(on_disk(to_file.to_str().unwrap(), separate_store));
            assert!(!on_disk(sub.to_str().unwrap(), separate_store));
            assert!(!on_disk(to_dir.to_str().unwrap(), separate_store));
            assert!(!on_disk(dangling.to_str().unwrap(), separate_store));
        }
    }

    fn plan(steps: Vec<StreamStep>) -> ExecutionPlan {
        ExecutionPlan {
            total_steps: steps.len(),
            phases: vec![Phase {
                reason: PhaseReason::Initial,
                streams: vec![WorkerStream {
                    stream_id: "s".to_string(),
                    steps,
                }],
            }],
        }
    }

    #[test]
    fn with_targets_the_targets_are_requested() {
        assert_eq!(
            requested(&plan(vec![]), &["p.py:a", "p.py:b"]),
            HashSet::from(["p.py:a".to_string(), "p.py:b".to_string()])
        );
    }

    #[test]
    fn without_a_target_only_the_returned_asset_is_requested() {
        let mut task = step("p.py:publish", &[("m", "p.py:mid")], &[]);
        task.kind = NodeKind::Task;
        let plan = plan(vec![
            step("p.py:src", &[], &[]),
            step("p.py:mid", &[("src", "p.py:src")], &[]),
            step("p.py:side", &[("src", "p.py:src")], &["a", "b"]),
            step("p.py:report", &[("mid", "p.py:mid")], &[]),
            task,
        ]);
        // `report` is the last planned asset: its value is the run's `final_output`. `side` is
        // also read by nothing, but it is not returned, so it is treated like an intermediate.
        assert_eq!(
            returned_step(&plan).unwrap().step_id.base_id(),
            "p.py:report"
        );
        assert_eq!(
            requested(&plan, &[]),
            HashSet::from(["p.py:report".to_string()])
        );
    }

    #[test]
    fn a_plan_without_assets_returns_its_last_step_and_requests_nothing() {
        let mut task = step("p.py:publish", &[], &[]);
        task.kind = NodeKind::Task;
        let plan = plan(vec![task]);
        assert_eq!(
            returned_step(&plan).unwrap().step_id.base_id(),
            "p.py:publish"
        );
        assert!(requested(&plan, &[]).is_empty());
        assert!(returned_step(&self::plan(vec![])).is_none());
    }

    #[test]
    fn drop_blocked_removes_the_steps_behind_a_failure_and_reports_each() {
        let mut phase = Phase {
            reason: PhaseReason::Initial,
            streams: vec![
                WorkerStream {
                    stream_id: "a".to_string(),
                    steps: vec![step("p.py:ok", &[], &[]), step("p.py:blocked", &[], &[])],
                },
                WorkerStream {
                    stream_id: "b".to_string(),
                    steps: vec![step("p.py:also_blocked", &[], &["1", "2"])],
                },
            ],
        };
        let mut dropped: Vec<(String, &str)> = Vec::new();
        drop_blocked(
            &mut phase,
            |base| base.contains("blocked").then_some("p.py:failed"),
            |st, up| dropped.push((st.step_id.base_id().to_string(), up)),
        );
        assert_eq!(
            dropped,
            [
                ("p.py:blocked".to_string(), "p.py:failed"),
                ("p.py:also_blocked".to_string(), "p.py:failed")
            ]
        );
        // The stream left empty is gone; the other keeps its unblocked step.
        assert_eq!(phase.streams.len(), 1);
        assert_eq!(phase.streams[0].steps[0].step_id.base_id(), "p.py:ok");
    }

    #[test]
    fn output_ids_are_one_per_key() {
        assert_eq!(output_ids(&step("p.py:a", &[], &[])), ["p.py:a"]);
        assert_eq!(
            output_ids(&step("p.py:a", &[], &["1", "2"])),
            ["p.py:a[k=1]", "p.py:a[k=2]"]
        );
    }

    #[tokio::test]
    async fn without_a_store_what_is_lost_is_cached_not_on_disk_and_known() {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        std::fs::write(at("here.json"), "1").unwrap();
        let outputs = HashMap::from([
            ("p.py:here".to_string(), oref(&at("here.json"))),
            ("p.py:gone".to_string(), oref(&at("gone.json"))),
            ("p.py:ran".to_string(), oref(&at("ran.json"))),
            ("p.py:unknown".to_string(), oref(&at("unknown.json"))),
        ]);
        let cached_ids: HashSet<String> = ids(&["p.py:here", "p.py:gone", "p.py:unknown"])
            .into_iter()
            .collect();
        let mut cached = CachedSteps::default();
        cached.remember(step("p.py:here", &[], &[]));
        cached.remember(step("p.py:gone", &[], &[]));
        let check: Vec<String> = outputs.values().map(|o| o.path.clone()).collect();
        let lost = lost(
            &mut None,
            &check,
            &check,
            None,
            &outputs,
            &cached_ids,
            &mut cached,
        )
        .await;
        // Not `here` (on disk), not `ran` (computed in this run, so not a cache hit), and not
        // `unknown` (no step to run again).
        assert_eq!(lost, Ok(ids(&["p.py:gone"])));
    }

    #[test]
    fn a_recompute_is_announced_once_per_node() {
        let lost = vec![
            ("p.py:part[k=a]".to_string(), "/a/part/1.json".to_string()),
            ("p.py:model".to_string(), "/a/model/2.json".to_string()),
            ("p.py:part[k=b]".to_string(), "/a/part/3.json".to_string()),
        ];
        assert_eq!(
            recompute_warnings(&lost),
            [
                "[barca] warning: p.py:part: the artifact of its cached result is missing: \
                 /a/part/1.json (and 1 more of its partitions). Computing it again.",
                "[barca] warning: p.py:model: the artifact of its cached result is missing: \
                 /a/model/2.json. Computing it again.",
            ]
        );
    }

    #[test]
    fn a_dry_run_looks_at_a_directory_store_only_when_the_store_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let store = dir.path().join("store");
        std::fs::create_dir_all(local.join("n")).unwrap();
        std::fs::create_dir_all(store.join("n")).unwrap();
        let layout = ArtifactLayout::new(&local, store.to_str().unwrap());
        let stored = oref(store.join("n/h.json").to_str().unwrap());

        // Neither copy, and the store directory is there: the object is missing.
        assert!(predicted_gone(&stored, Some(&layout)));
        // A copy in the store is fetched; a local mirror is read as it is.
        std::fs::write(store.join("n/h.json"), "1").unwrap();
        assert!(!predicted_gone(&stored, Some(&layout)));
        std::fs::remove_file(store.join("n/h.json")).unwrap();
        std::fs::write(local.join("n/h.json"), "1").unwrap();
        assert!(!predicted_gone(&stored, Some(&layout)));
        // The store itself is gone (unmounted): nothing is known to be missing, and a run
        // would fail rather than recompute.
        std::fs::remove_file(local.join("n/h.json")).unwrap();
        std::fs::remove_dir_all(&store).unwrap();
        assert!(!predicted_gone(&stored, Some(&layout)));

        // A result in a remote store is never looked up by a dry run.
        let remote = ArtifactLayout::new(&local, "s3://b/p");
        assert!(!predicted_gone(&oref("s3://b/p/n/h.json"), Some(&remote)));
        // A row outside the store is a file on this disk, or nothing.
        assert!(predicted_gone(&oref("/elsewhere/n/h.json"), Some(&remote)));
        assert!(predicted_gone(&oref("/elsewhere/n/h.json"), None));
    }

    #[test]
    fn steps_remembered_after_a_lookup_are_found_too() {
        let mut cached = CachedSteps::default();
        cached.remember(step("p.py:a", &[], &["1"]));
        assert!(cached.knows("p.py:a[k=1]") && !cached.knows("p.py:b"));
        cached.remember(step("p.py:b", &[], &[]));
        cached.remember(step("p.py:a", &[], &["2"]));
        assert!(cached.knows("p.py:b") && cached.knows("p.py:a[k=2]"));
        assert!(!cached.knows("p.py:a[k=3]") && !cached.knows("p.py:a"));
    }

    /// Lookups are by index, not by scanning every key of the node (#252 review): with the
    /// scan, recovering every key of a 20,000-key node formatted 400 million ids and took
    /// minutes; indexed it is a few thousandths of that. The bound is loose on purpose.
    #[test]
    fn recovering_every_key_of_a_large_node_is_linear() {
        const KEYS: usize = 20_000;
        let keys: Vec<String> = (0..KEYS).map(|i| format!("{i:05}")).collect();
        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let mut cached = CachedSteps::default();
        // Planned as several steps, as the planner splits a partitioned node across streams.
        for chunk in key_refs.chunks(KEYS / 8) {
            cached.remember(step("p.py:part", &[], chunk));
        }
        let all: Vec<String> = keys.iter().map(|k| format!("p.py:part[k={k}]")).collect();
        let mut outputs: HashMap<String, OutputRef> = all
            .iter()
            .map(|id| (id.clone(), oref(&format!("/a/{id}.json"))))
            .collect();
        let mut reports = vec![StepReport {
            id: "p.py:part".to_string(),
            kind: "asset".to_string(),
            action: Some("cached".to_string()),
            partitions: Some(PartitionSummary {
                total: KEYS,
                cached: KEYS,
                will_run: 0,
                will_run_keys: vec![],
            }),
            ..Default::default()
        }];

        let started = std::time::Instant::now();
        assert!(all.iter().all(|id| cached.knows(id)));
        assert_eq!(cached.first_layer(&all).len(), KEYS);
        let phase = cached.phase_for(&all).unwrap();
        assert_eq!(phase.streams[0].steps[0].partition_keys.len(), KEYS);
        assert_eq!(phase.streams[0].steps[0].run_hashes.len(), KEYS);
        let needed: Vec<String> = outputs.values().map(|o| o.path.clone()).collect();
        let lost = predict_lost(&mut cached, &mut outputs, &needed, |_| true);
        assert_eq!(lost.len(), KEYS);
        for id in &lost {
            assert!(mark_recomputed(&mut reports, id, true));
        }
        assert_eq!(recompute_warnings(&[]).len(), 0);
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(5),
            "recovering {KEYS} keys took {took:?}"
        );
    }

    #[test]
    fn flags_keep_the_order_of_their_items_few_or_many() {
        for n in [0usize, 3, 511, 512, 5_000] {
            let items: Vec<usize> = (0..n).collect();
            let expected: Vec<bool> = items.iter().map(|i| i % 3 == 0).collect();
            assert_eq!(flags(&items, |i| i % 3 == 0), expected, "{n} items");
        }
    }

    #[test]
    fn outputs_at_names_only_cached_outputs_at_those_paths() {
        let outputs = HashMap::from([
            ("p.py:a".to_string(), oref("/a/a.json")),
            ("p.py:b".to_string(), oref("/a/b.json")),
            ("p.py:c[k=1]".to_string(), oref("/a/c1.json")),
        ]);
        let paths = HashSet::from(["/a/a.json", "/a/b.json", "/a/c1.json", "/a/other.json"]);
        assert_eq!(
            outputs_at(&paths, &outputs, |id| id != "p.py:b"),
            ids(&["p.py:a", "p.py:c[k=1]"])
        );
        assert!(outputs_at(&HashSet::new(), &outputs, |_| true).is_empty());
    }

    #[test]
    fn the_first_layer_leaves_out_steps_that_read_another_lost_output() {
        let mut cached = CachedSteps::default();
        cached.remember(step("p.py:a", &[], &["1", "2"]));
        cached.remember(step("p.py:b", &[("a", "p.py:a")], &[]));
        cached.remember(step("p.py:c", &[], &[]));
        assert_eq!(
            cached.first_layer(&ids(&["p.py:a[k=1]", "p.py:b", "p.py:c"])),
            ids(&["p.py:a[k=1]", "p.py:c"])
        );
        // Keys of one node do not hold each other back.
        assert_eq!(
            cached.first_layer(&ids(&["p.py:a[k=1]", "p.py:a[k=2]"])),
            ids(&["p.py:a[k=1]", "p.py:a[k=2]"])
        );
    }

    #[test]
    fn the_phase_runs_only_the_named_steps_and_keys() {
        let mut cached = CachedSteps::default();
        // A partitioned node is planned as several steps, each with some of its keys.
        cached.remember(step("p.py:part", &[], &["1", "2"]));
        cached.remember(step("p.py:part", &[], &["3"]));
        cached.remember(step("p.py:model", &[], &[]));
        let phase = cached
            .phase_for(&ids(&[
                "p.py:part[k=3]",
                "p.py:model",
                "p.py:part[k=1]",
                "p.py:x",
            ]))
            .expect("two known nodes");
        assert_eq!(phase.streams.len(), 2, "one stream per node");
        let part = &phase.streams[0].steps[0];
        assert_eq!(part.step_id.base_id(), "p.py:part");
        let keys: Vec<String> = part.partition_keys.iter().map(|k| k.suffix()).collect();
        assert_eq!(keys, ["k=3", "k=1"]);
        // Each key keeps the run hash it was decided with, so its artifact lands where the
        // missing one was.
        assert_eq!(
            part.run_hashes,
            HashMap::from([
                ("p.py:part[k=3]".to_string(), "h-p.py:part[k=3]".to_string()),
                ("p.py:part[k=1]".to_string(), "h-p.py:part[k=1]".to_string()),
            ])
        );
        let model = &phase.streams[1].steps[0];
        assert_eq!(model.step_id.display(), "p.py:model");
        assert_eq!(model.run_hashes["p.py:model"], "h-p.py:model");
        assert!(cached.phase_for(&ids(&["p.py:unknown"])).is_none());
    }

    #[test]
    fn a_dry_run_follows_a_lost_output_into_the_inputs_of_its_step() {
        // src -> mid -> report, all cached; `report` is needed.
        let mut cached = CachedSteps::default();
        cached.remember(step("p.py:src", &[], &[]));
        cached.remember(step("p.py:mid", &[("src", "p.py:src")], &[]));
        cached.remember(step("p.py:report", &[("mid", "p.py:mid")], &[]));
        let all = || {
            HashMap::from([
                ("p.py:src".to_string(), oref("/a/src.json")),
                ("p.py:mid".to_string(), oref("/a/mid.json")),
                ("p.py:report".to_string(), oref("/a/report.json")),
            ])
        };
        let needed = ids(&["/a/report.json"]);

        // Only `report` is gone: `mid` is read to compute it again, and is there.
        let mut outputs = all();
        let lost = predict_lost(&mut cached, &mut outputs, &needed, |o| {
            o.path == "/a/report.json"
        });
        assert_eq!(lost, ids(&["p.py:report"]));
        assert!(outputs.contains_key("p.py:mid") && !outputs.contains_key("p.py:report"));

        // Everything is gone: each step's input is needed in turn.
        let mut outputs = all();
        let lost = predict_lost(&mut cached, &mut outputs, &needed, |_| true);
        assert_eq!(lost, ids(&["p.py:report", "p.py:mid", "p.py:src"]));
        assert!(outputs.is_empty());

        // `mid` is gone but nothing that runs reads it: nothing is lost.
        let mut outputs = all();
        let lost = predict_lost(&mut cached, &mut outputs, &needed, |o| {
            o.path == "/a/mid.json"
        });
        assert!(lost.is_empty());
        assert_eq!(outputs.len(), 3);
    }

    #[test]
    fn a_cached_line_becomes_a_run_with_the_reason() {
        for dry in [true, false] {
            let mut reports = vec![
                cached_line("p.py:other", dry),
                cached_line("p.py:model", dry),
            ];
            reports[1].warning = Some("stale".to_string());
            assert!(mark_recomputed(&mut reports, "p.py:model", dry));
            let r = &reports[1];
            let said = if dry { &r.action } else { &r.status };
            assert_eq!(said.as_deref(), Some(if dry { "run" } else { "ran" }));
            assert_eq!(r.reason.as_deref(), Some("artifact_missing"));
            assert!(r.detail.as_deref().unwrap().contains("missing"));
            assert!(r.artifact.is_none() && r.warning.is_none());
            // The other line is untouched, and a line is changed once.
            assert_eq!(reports[0].artifact.as_deref(), Some("/a/x.json"));
            assert!(!mark_recomputed(&mut reports, "p.py:model", dry));
        }
    }

    #[test]
    fn a_partitioned_line_moves_one_key_from_cached_to_run() {
        let mut reports = vec![StepReport {
            id: "p.py:part".to_string(),
            kind: "asset".to_string(),
            action: Some("cached".to_string()),
            partitions: Some(PartitionSummary {
                total: 2,
                cached: 2,
                will_run: 0,
                will_run_keys: vec![],
            }),
            ..Default::default()
        }];
        assert!(mark_recomputed(&mut reports, "p.py:part[k=a]", true));
        let p = reports[0].partitions.as_ref().unwrap();
        assert_eq!((p.cached, p.will_run), (1, 1));
        assert_eq!(p.will_run_keys, ["k=a"]);
        assert_eq!(reports[0].action.as_deref(), Some("partial"));
        assert_eq!(reports[0].reason.as_deref(), Some("artifact_missing"));

        assert!(mark_recomputed(&mut reports, "p.py:part[k=b]", true));
        assert_eq!(reports[0].action.as_deref(), Some("run"));
        assert!(!mark_recomputed(&mut reports, "p.py:part[k=c]", true));
    }

    #[test]
    fn a_key_that_was_never_cached_keeps_its_own_reason() {
        let mut reports = vec![StepReport {
            id: "p.py:part".to_string(),
            kind: "asset".to_string(),
            status: Some("partial".to_string()),
            reason: Some("not_materialized".to_string()),
            detail: Some("no cached result".to_string()),
            partitions: Some(PartitionSummary {
                total: 2,
                cached: 1,
                will_run: 1,
                will_run_keys: vec!["k=new".to_string()],
            }),
            ..Default::default()
        }];
        assert!(mark_recomputed(&mut reports, "p.py:part[k=a]", false));
        assert_eq!(reports[0].reason.as_deref(), Some("not_materialized"));
        assert_eq!(reports[0].status.as_deref(), Some("ran"));
    }
}
