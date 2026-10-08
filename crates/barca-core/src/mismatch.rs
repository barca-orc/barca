//! Reporting a store copy that is not the one its result was recorded with (#247).
//!
//! With an artifact store, a fetched artifact is checked against the hash recorded when it
//! was written (`barca docs remote`). A copy with other bytes is still used, because an
//! artifact's path names the computation and not the bytes: a refresh, or another machine
//! computing the same step, overwrites the object. The run says so on stderr, and in its JSON
//! result with a marker a caller can test without reading message text:
//! `steps[].artifact_mismatch: true`, with the reason in `steps[].warning`, on
//!
//! - the step the artifact belongs to, whatever its status, and
//! - every step that read the artifact as an input in this run (it ran, or failed).
//!
//! The key is absent everywhere else. It is a finding of this run, made when an artifact is
//! fetched; a dry run, which does not contact the store, never has it.

use crate::dag::Dag;
use crate::recover::base_of;
use crate::results::StepReport;
use std::collections::HashMap;

/// The finding in words, without the `[barca] warning: <step>:` prefix of the stderr line:
/// `at` is the store location, `more` the number of further partitions of the step it holds
/// for.
pub(crate) fn describe(base: &str, at: &str, more: usize) -> String {
    let others = match more {
        0 => String::new(),
        n => format!(" (and {n} more of its partitions)"),
    };
    format!(
        "the copy at {at}{others} is not the one this result was recorded with (another run \
         overwrote it, or it was changed). Using it. Recompute with --refresh {base}."
    )
}

/// The upstream node ids `id` reads as data: its inputs and `collect()` inputs, less the
/// `_`-prefixed ones, which are there for ordering only and are never loaded.
fn data_inputs<'d>(dag: &'d Dag, id: &str) -> Vec<&'d str> {
    let Some(node) = dag.get_node(id) else {
        return Vec::new();
    };
    let mut ups: Vec<&str> = node
        .resolved_inputs
        .iter()
        .chain(&node.resolved_collected)
        .filter(|(param, _)| !param.starts_with('_'))
        .map(|(_, up)| up.as_str())
        .collect();
    ups.sort_unstable();
    ups.dedup();
    ups
}

/// Put each mismatch (base step id -> [`describe`] text) on the report of the step it belongs
/// to and on the reports of the steps that read it in this run.
pub(crate) fn mark(dag: &Dag, reports: &mut [StepReport], mismatched: &HashMap<String, String>) {
    if mismatched.is_empty() {
        return;
    }
    for r in reports {
        let base = base_of(&r.id);
        let read_inputs = matches!(r.status.as_deref(), Some("ran" | "partial" | "failed"));
        let mut notes: Vec<String> = Vec::new();
        if let Some(w) = mismatched.get(base) {
            notes.push(w.clone());
        }
        if read_inputs {
            for up in data_inputs(dag, base) {
                if let Some(w) = mismatched.get(up) {
                    notes.push(format!("input {}: {w}", crate::targets::short_name(up)));
                }
            }
        }
        if notes.is_empty() {
            continue;
        }
        r.artifact_mismatch = Some(true);
        let joined = notes.join(" ");
        r.warning = Some(match r.warning.take() {
            Some(w) => format!("{w} {joined}"),
            None => joined,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::extract_nodes;

    const SRC: &str = r#"
from barca import asset, collect, partitions, partitions_from, task

@asset
def numbers(): return [1]

@asset
def side(): return 1

@asset(inputs={"numbers": numbers})
def total(numbers): return sum(numbers)

@asset(inputs={"_numbers": numbers, "side": side})
def ordered(_numbers, side): return side

@asset(partitions={"k": partitions(["a", "b"])})
def parts(k): return k

@asset(inputs={"p": parts}, partitions={"k": partitions_from(parts)})
def each(k, p): return p

@asset(inputs={"ps": collect(parts)})
def gathered(ps): return ps

@task(inputs={"total": total})
def publish(total): return None
"#;

    fn dag() -> Dag {
        Dag::build(&extract_nodes(SRC, "p.py").unwrap()).unwrap()
    }

    fn report(name: &str, status: &str) -> StepReport {
        StepReport {
            id: format!("p.py:{name}"),
            kind: "asset".to_string(),
            status: Some(status.to_string()),
            ..Default::default()
        }
    }

    fn found(names: &[&str]) -> HashMap<String, String> {
        names
            .iter()
            .map(|n| {
                let id = format!("p.py:{n}");
                (id.clone(), describe(&id, &format!("/store/{n}.json"), 0))
            })
            .collect()
    }

    fn marked(reports: &[StepReport]) -> Vec<&str> {
        reports
            .iter()
            .filter(|r| r.artifact_mismatch == Some(true))
            .map(|r| r.id.as_str())
            .collect()
    }

    #[test]
    fn nothing_is_marked_when_nothing_differs() {
        let mut reports = vec![report("numbers", "cached"), report("total", "ran")];
        mark(&dag(), &mut reports, &HashMap::new());
        assert!(
            reports
                .iter()
                .all(|r| r.artifact_mismatch.is_none() && r.warning.is_none())
        );
    }

    #[test]
    fn the_owner_is_marked_whatever_its_status_and_a_reader_only_if_it_read() {
        let mut reports = vec![
            report("numbers", "cached"),
            report("side", "cached"),
            report("total", "ran"),
            report("publish", "ran"),
        ];
        mark(&dag(), &mut reports, &found(&["numbers"]));
        // `publish` reads `total`, not `numbers`: the marker does not travel downstream.
        assert_eq!(marked(&reports), ["p.py:numbers", "p.py:total"]);
        let owner = reports[0].warning.as_deref().unwrap();
        assert!(owner.starts_with("the copy at /store/numbers.json is not the one"));
        assert!(owner.ends_with("Recompute with --refresh p.py:numbers."));
        let reader = reports[2].warning.as_deref().unwrap();
        assert!(reader.starts_with("input numbers: the copy at /store/numbers.json"));
    }

    #[test]
    fn a_cached_or_skipped_reader_did_not_read_it() {
        for status in ["cached", "skipped"] {
            let mut reports = vec![report("numbers", "cached"), report("total", status)];
            mark(&dag(), &mut reports, &found(&["numbers"]));
            assert_eq!(marked(&reports), ["p.py:numbers"], "{status}");
        }
    }

    #[test]
    fn a_reader_that_failed_is_marked_too() {
        let mut reports = vec![report("numbers", "cached"), report("total", "failed")];
        mark(&dag(), &mut reports, &found(&["numbers"]));
        assert_eq!(marked(&reports), ["p.py:numbers", "p.py:total"]);
    }

    #[test]
    fn an_ordering_only_input_is_not_a_read() {
        let mut reports = vec![report("numbers", "cached"), report("ordered", "ran")];
        mark(&dag(), &mut reports, &found(&["numbers"]));
        assert_eq!(marked(&reports), ["p.py:numbers"]);
        // Its real input is still reported.
        let mut reports = vec![report("side", "cached"), report("ordered", "ran")];
        mark(&dag(), &mut reports, &found(&["side"]));
        assert_eq!(marked(&reports), ["p.py:side", "p.py:ordered"]);
    }

    #[test]
    fn partitioned_and_collecting_readers_are_matched_by_base_id() {
        let mut reports = vec![
            report("parts", "cached"),
            report("each", "partial"),
            report("gathered", "ran"),
        ];
        mark(&dag(), &mut reports, &found(&["parts"]));
        assert_eq!(
            marked(&reports),
            ["p.py:parts", "p.py:each", "p.py:gathered"]
        );
    }

    #[test]
    fn the_text_joins_a_warning_the_step_already_has() {
        let mut reports = vec![report("numbers", "cached")];
        reports[0].warning = Some("served from cache.".to_string());
        mark(&dag(), &mut reports, &found(&["numbers"]));
        let w = reports[0].warning.as_deref().unwrap();
        assert!(w.starts_with("served from cache. the copy at /store/numbers.json"));
    }

    #[test]
    fn the_description_counts_further_partitions() {
        assert!(describe("p.py:parts", "/s/a.json", 2).contains("(and 2 more of its partitions)"));
        assert!(!describe("p.py:parts", "/s/a.json", 0).contains("more of its partitions"));
    }
}
