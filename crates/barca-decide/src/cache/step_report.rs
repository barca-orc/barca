//! The report line of a decided step ([`StepReport`]): what a run did with it, or what a dry
//! run predicts it would do. Built where the decision is, so a run (`barca_model::execution`) and a
//! prediction (`crate::predict`) report the same decision the same way.

use super::{Decision, RunReason};
use barca_compile::dag::Dag;
use barca_compile::targets::short_name;
use barca_model::model::kind_str;
use barca_model::step_status::StepStatus;
use barca_store::results::{PartitionSummary, StepReport, key_preview};
use std::collections::HashMap;

/// A partitioned asset plans one step per key; report it as one line with a partition summary.
pub fn merge_partition_reports(reports: Vec<StepReport>) -> Vec<StepReport> {
    let mut out: Vec<StepReport> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for r in reports {
        let Some(p) = r.partitions.clone() else {
            out.push(r);
            continue;
        };
        match index.get(&r.id).copied() {
            None => {
                index.insert(r.id.clone(), out.len());
                out.push(r);
            }
            Some(i) => {
                let m = &mut out[i];
                let mp = m.partitions.get_or_insert_with(PartitionSummary::default);
                mp.total += p.total;
                mp.cached += p.cached;
                mp.will_run += p.will_run;
                mp.will_run_keys = key_preview(
                    std::mem::take(&mut mp.will_run_keys)
                        .into_iter()
                        .chain(p.will_run_keys),
                );
                if let Some(changed) = p.changed {
                    mp.changed
                        .get_or_insert_with(Default::default)
                        .extend(changed);
                }
                if let Some(changed) = &mut mp.changed {
                    let keys = &mp.will_run_keys;
                    changed.retain(|k, _| keys.contains(k));
                }
                if m.reason.is_none() {
                    m.reason = r.reason;
                    m.detail = r.detail;
                } else if m.changed.is_none() && r.changed.is_some() {
                    // What changed in the first key that ran before, whichever chunk it is in.
                    m.detail = r.detail;
                }
                if m.changed.is_none() {
                    m.changed = r.changed;
                }
            }
        }
    }
    // Recompute the verdict of merged lines from their totals.
    for r in &mut out {
        let Some(p) = &r.partitions else { continue };
        let verdict = |dry: bool| match (p.cached, p.will_run) {
            (_, 0) => StepStatus::Cached.as_str(),
            (0, _) => {
                if dry {
                    "run"
                } else {
                    StepStatus::Ran.as_str()
                }
            }
            _ => StepStatus::Partial.as_str(),
        };
        if r.action.is_some() {
            r.action = Some(verdict(true).to_string());
        } else {
            r.status = Some(verdict(false).to_string());
        }
    }
    out
}

pub fn stale_warning(display_id: &str, root: &str, dry: bool) -> String {
    let id = short_name(display_id);
    let (served, reflect) = if dry {
        ("will be served", "will not reflect")
    } else {
        ("was served", "does not reflect")
    };
    format!(
        "'{id}' {served} from cache but depends on refreshed '{root}', so it {reflect} the \
         refresh. Drop --no-cascade, add it to --refresh (for example --refresh {root},{id}) or \
         use --refresh-all."
    )
}

/// The declared env values of `base_id` as reported in output (secrets redacted, unset = null),
/// or `None` when the node declares no env.
pub fn env_report(
    dag: &Dag,
    base_id: &str,
) -> Option<std::collections::BTreeMap<String, Option<String>>> {
    let node = dag.get_node(base_id)?;
    if node.decl.env.is_empty() {
        return None;
    }
    Some(barca_model::envdeps::report(
        &barca_model::envdeps::resolve(&node.decl.env),
    ))
}

/// Build the report line for one decided step. `dry` selects the vocabulary: a dry run says what
/// will happen (`action`), a real run says what happened (`status`).
pub fn report_for(
    dag: &Dag,
    step: &barca_compile::planner::StreamStep,
    decision: &Decision,
    dry: bool,
) -> StepReport {
    let display_id = step.step_id.display();
    let base_id = step.step_id.base_id();
    let mut r = StepReport {
        id: base_id.to_string(),
        kind: kind_str(dag.get_node(base_id).map(|n| n.kind())),
        env: env_report(dag, base_id),
        undeclared_env: dag
            .get_node(base_id)
            .map(|n| n.undeclared_env())
            .filter(|names| !names.is_empty()),
        ..Default::default()
    };
    let (word_cached, word_partial) = (StepStatus::Cached.as_str(), StepStatus::Partial.as_str());
    let word_run = if dry { "run" } else { StepStatus::Ran.as_str() };
    let verdict = match decision {
        Decision::Run(reason) => {
            r.reason = Some(reason.code().to_string());
            r.detail = Some(reason.detail());
            r.run_hash = step.run_hashes.get(&display_id).cloned();
            if step.partition_keys.is_empty() {
                word_run
            } else {
                // A forced (task/refresh) partitioned step runs every key.
                r.partitions = Some(PartitionSummary {
                    total: step.partition_keys.len(),
                    cached: 0,
                    will_run: step.partition_keys.len(),
                    will_run_keys: key_preview(step.partition_keys.iter().map(|k| k.suffix())),
                    changed: None,
                });
                word_run
            }
        }
        Decision::Cached {
            oref,
            stale_root,
            held,
        } => {
            r.run_hash = step.run_hashes.get(&display_id).cloned();
            r.artifact = Some(oref.path.clone());
            if let Some(held) = held {
                r.detail = Some(held.detail().to_string());
                if *held == super::Held::Pinned {
                    r.reason = Some(super::PINNED_REASON.to_string());
                }
            }
            if let Some(root) = stale_root {
                r.warning = Some(stale_warning(&display_id, root, dry));
            }
            word_cached
        }
        Decision::Partitioned {
            cached,
            missing,
            refresh,
            cutoff_stopped,
        } => {
            if *cutoff_stopped {
                r.detail = Some("unchanged cutoff bytes: refresh propagation stopped".to_string());
            }
            let total = cached.len() + missing.len();
            r.partitions = Some(PartitionSummary {
                total,
                cached: cached.len(),
                will_run: missing.len(),
                will_run_keys: key_preview(missing.iter().map(|k| k.suffix())),
                changed: None,
            });
            if !missing.is_empty() {
                let reason = refresh.as_ref().unwrap_or(&RunReason::NotMaterialized);
                r.reason = Some(reason.code().to_string());
                r.detail = Some(reason.detail());
            }
            match (cached.is_empty(), missing.is_empty()) {
                (_, true) => word_cached,
                (true, false) => word_run,
                (false, false) => word_partial,
            }
        }
    };
    if dry {
        r.action = Some(verdict.to_string());
    } else {
        r.status = Some(verdict.to_string());
    }
    r
}

/// What another run of the same server computed while this run waited for it (#654), as this
/// run reports it: a cache hit. `display_id` is the step (with its partition key), `artifact`
/// where its result is. Adjusts the report of its node made when the step was decided.
pub fn computed_by_another_run(
    reports: &mut [StepReport],
    display_id: &str,
    artifact: Option<&str>,
) {
    let id = barca_model::StepId::parse(display_id);
    let base = id.base_id();
    let key = (!id.partition.is_empty()).then(|| id.partition.suffix());
    for r in reports.iter_mut().rev().filter(|r| r.id == base) {
        match (&key, r.partitions.as_mut()) {
            (Some(key), Some(p)) if p.will_run > 0 => {
                p.will_run -= 1;
                p.cached += 1;
                p.will_run_keys.retain(|k| k != key);
                if let Some(changed) = p.changed.as_mut() {
                    changed.remove(key);
                }
                if p.will_run == 0 {
                    r.reason = None;
                    r.detail = Some(COMPUTED_BY_ANOTHER_RUN.to_string());
                    r.status = Some(
                        if p.cached == p.total {
                            StepStatus::Cached
                        } else {
                            StepStatus::Partial
                        }
                        .to_string(),
                    );
                }
                return;
            }
            (None, None) if StepStatus::Ran.is(r.status.as_deref()) => {
                r.status = Some(StepStatus::Cached.to_string());
                r.reason = None;
                r.changed = None;
                r.detail = Some(COMPUTED_BY_ANOTHER_RUN.to_string());
                r.artifact = artifact.map(str::to_string);
                return;
            }
            _ => {}
        }
    }
}

/// The detail of a step another run of the server computed for this one.
pub(crate) const COMPUTED_BY_ANOTHER_RUN: &str =
    "computed by another run of the server at the same time";
