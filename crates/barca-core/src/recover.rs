//! Recomputing a cached step whose artifact is gone (#252).
//!
//! A cache hit is a row in the metadata DB; the artifact it names can be deleted behind barca's
//! back (a disk cleanup, a container restarted without a volume, `rm -r .barca/artifacts`). The
//! rule, shared by a real run and `--dry-run` / `barca status`:
//!
//! - A missing artifact matters only when something is about to **read** it: a step that is
//!   going to run takes it as an input, a `partitions_from` step is expanded from it, or it is
//!   an output the command was asked for (see [`requested`]). A pruned intermediate that
//!   nothing reads stays cached and costs nothing.
//! - "Missing" means not on this machine's disk and not fetchable from the artifact store. For
//!   a row recorded in the store, the readable copy is its local mirror; the recorded store
//!   location is never checked directly.
//! - The step that produced a needed, missing artifact is run again, with reason
//!   `artifact_missing`. Its run hash is unchanged, so the artifact lands at the same path and
//!   nothing downstream is invalidated. Its own inputs are subject to the same rule.
//!
//! This module holds the parts both callers share and that can be tested without a run: which
//! artifacts are on disk, which steps to run again, and how a step's report line changes.
//! `commands::execute` applies it where a phase's inputs are made available;
//! `commands::explain_dag` predicts it with [`predict_lost`].

use crate::commands::{PartitionSummary, RunReason, StepReport};
use crate::dispatch::{OutputRef, ProvidedInput, build_provided_inputs};
use crate::model::NodeKind;
use crate::planner::{ExecutionPlan, Phase, PhaseReason, StreamStep, WorkerStream};
use std::collections::{HashMap, HashSet};

/// Whether the artifact recorded at `path` can be read from this machine as it is.
///
/// A file path (plain or `file://`) is looked up on disk. A remote URI cannot be: with a
/// separate artifact store (`separate_store`) workers read only the local artifact directory,
/// so a URI that is not in that store is out of reach; without one, the worker reads the URI
/// itself and reports what it finds, so it is taken as readable.
///
/// A directory at the path counts as present: whether that is a valid artifact is not decided
/// here.
pub(crate) fn on_disk(path: &str, separate_store: bool) -> bool {
    match crate::transfer::local_path(path) {
        Some(file) => file.exists(),
        None => !separate_store,
    }
}

/// The base node id of a step or output id (`p.py:a[k=1]` -> `p.py:a`).
fn base_of(id: &str) -> &str {
    id.split('[').next().unwrap_or(id)
}

/// The base ids of the outputs a command was asked for: its targets, or with no target every
/// planned asset that no other planned step reads (the ends of the pipeline). These are read by
/// whoever ran the command, so their artifacts are needed even though no step consumes them.
pub(crate) fn requested(plan: &ExecutionPlan, target_ids: &[&str]) -> HashSet<String> {
    if !target_ids.is_empty() {
        return target_ids.iter().map(|id| id.to_string()).collect();
    }
    let steps = || {
        plan.phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
    };
    let read: HashSet<&str> = steps()
        .flat_map(|st| st.inputs.values())
        .map(|up| base_of(up))
        .collect();
    steps()
        .filter(|st| st.kind == NodeKind::Asset && !read.contains(st.step_id.base_id()))
        .map(|st| st.step_id.base_id().to_string())
        .collect()
}

/// The paths a phase's provided inputs are read from.
pub(crate) fn input_paths(provided: &HashMap<String, ProvidedInput>) -> Vec<&str> {
    provided
        .values()
        .flat_map(|p| match p {
            ProvidedInput::Single(o) => std::slice::from_ref(o),
            ProvidedInput::Collected(v) => v.as_slice(),
        })
        .map(|o| o.path.as_str())
        .collect()
}

/// The ids (sorted) of the outputs among `outputs` that are at one of `paths` and that
/// `is_cached` says were served from cache: the steps to run again when those paths are gone.
pub(crate) fn outputs_at(
    paths: &HashSet<&str>,
    outputs: &HashMap<String, OutputRef>,
    is_cached: impl Fn(&str) -> bool,
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
#[derive(Default)]
pub(crate) struct CachedSteps {
    /// Base node id -> the planned steps of that node (a partitioned node is planned as
    /// several, each holding some of its keys).
    by_base: HashMap<String, Vec<StreamStep>>,
}

impl CachedSteps {
    /// Remember a step that was served from cache, wholly or for some of its partition keys.
    pub(crate) fn remember(&mut self, step: StreamStep) {
        self.by_base
            .entry(step.step_id.base_id().to_string())
            .or_default()
            .push(step);
    }

    /// Whether the step that produced output `id` is remembered.
    pub(crate) fn knows(&self, id: &str) -> bool {
        self.find(id).is_some()
    }

    /// The remembered step that produced output `id`, and the index of its partition key when
    /// the step is expanded by key.
    fn find(&self, id: &str) -> Option<(&StreamStep, Option<usize>)> {
        let base = base_of(id);
        self.by_base.get(base)?.iter().find_map(|step| {
            if step.partition_keys.is_empty() {
                (step.step_id.display() == id).then_some((step, None))
            } else {
                step.partition_keys
                    .iter()
                    .position(|pk| pk.display_id(base) == id)
                    .map(|i| (step, Some(i)))
            }
        })
    }

    /// Of the cached outputs `ids`, those whose step reads none of the others. Run these
    /// first: a step never shares a phase with the producer of one of its inputs, so each
    /// reads complete inputs (a `collect()` consumer would otherwise be handed only the keys
    /// that were still cached).
    pub(crate) fn first_layer(&self, ids: &[String]) -> Vec<String> {
        let bases: HashSet<&str> = ids.iter().map(|id| base_of(id)).collect();
        let first: Vec<String> = ids
            .iter()
            .filter(|id| {
                let own = base_of(id);
                self.find(id).is_none_or(|(step, _)| {
                    !step
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
    pub(crate) fn phase_for(&self, ids: &[String]) -> Option<Phase> {
        let mut order: Vec<&str> = Vec::new();
        let mut steps: HashMap<&str, StreamStep> = HashMap::new();
        for id in ids {
            let Some((found, key)) = self.find(id) else {
                continue;
            };
            let base = base_of(id);
            let Some(key) = key else {
                // Not expanded by key: the step is its own unit of work.
                if !steps.contains_key(id.as_str()) {
                    order.push(id.as_str());
                    steps.insert(id.as_str(), found.clone());
                }
                continue;
            };
            let step = steps.entry(base).or_insert_with(|| {
                order.push(base);
                StreamStep {
                    partition_keys: Vec::new(),
                    run_hashes: HashMap::new(),
                    ..found.clone()
                }
            });
            let pk = &found.partition_keys[key];
            if !step.partition_keys.contains(pk) {
                step.partition_keys.push(pk.clone());
                if let Some(h) = found.run_hashes.get(id) {
                    step.run_hashes.insert(id.clone(), h.clone());
                }
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

/// Dry run: the cached outputs that a run reading `needed` (artifact paths) would find missing
/// and compute again, followed through the inputs of each step it would then run. They are
/// removed from `outputs`, which holds the cached outputs only. `gone` says whether the
/// artifact of an output is unavailable.
pub(crate) fn predict_lost(
    cached: &CachedSteps,
    outputs: &mut HashMap<String, OutputRef>,
    needed: &[String],
    gone: impl Fn(&OutputRef) -> bool,
) -> Vec<String> {
    let mut lost: Vec<String> = Vec::new();
    let mut needed: HashSet<String> = needed.iter().cloned().collect();
    loop {
        let gone_paths: HashSet<&str> = outputs
            .values()
            .filter(|o| needed.contains(&o.path) && gone(o))
            .map(|o| o.path.as_str())
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
                    .map(str::to_string)
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

    #[test]
    fn a_directory_at_the_path_counts_as_present() {
        let dir = tempfile::tempdir().unwrap();
        assert!(on_disk(dir.path().to_str().unwrap(), false));
    }

    #[test]
    fn with_targets_the_targets_are_requested() {
        let plan = ExecutionPlan {
            phases: vec![],
            total_steps: 0,
        };
        assert_eq!(
            requested(&plan, &["p.py:a", "p.py:b"]),
            HashSet::from(["p.py:a".to_string(), "p.py:b".to_string()])
        );
    }

    #[test]
    fn without_a_target_the_assets_nothing_reads_are_requested() {
        let mut task = step("p.py:publish", &[("m", "p.py:mid")], &[]);
        task.kind = NodeKind::Task;
        let plan = ExecutionPlan {
            total_steps: 5,
            phases: vec![Phase {
                reason: PhaseReason::Initial,
                streams: vec![WorkerStream {
                    stream_id: "s".to_string(),
                    steps: vec![
                        step("p.py:src", &[], &[]),
                        step("p.py:mid", &[("src", "p.py:src")], &[]),
                        step("p.py:report", &[("mid", "p.py:mid")], &[]),
                        step("p.py:parts", &[("src", "p.py:src")], &["a", "b"]),
                        task,
                    ],
                }],
            }],
        };
        // Not `src` or `mid` (read by others), and not the task (never cached).
        assert_eq!(
            requested(&plan, &[]),
            HashSet::from(["p.py:report".to_string(), "p.py:parts".to_string()])
        );
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
        let lost = predict_lost(&cached, &mut outputs, &needed, |o| {
            o.path == "/a/report.json"
        });
        assert_eq!(lost, ids(&["p.py:report"]));
        assert!(outputs.contains_key("p.py:mid") && !outputs.contains_key("p.py:report"));

        // Everything is gone: each step's input is needed in turn.
        let mut outputs = all();
        let lost = predict_lost(&cached, &mut outputs, &needed, |_| true);
        assert_eq!(lost, ids(&["p.py:report", "p.py:mid", "p.py:src"]));
        assert!(outputs.is_empty());

        // `mid` is gone but nothing that runs reads it: nothing is lost.
        let mut outputs = all();
        let lost = predict_lost(&cached, &mut outputs, &needed, |o| o.path == "/a/mid.json");
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
