//! Warnings barca itself raises: the plan-time warnings a command reports, and the one place
//! that prints a `[barca] warning: ...` line at most once per process.
//!
//! (Warnings that come from user code in a worker, library `logging` and `warnings`, are
//! collapsed in the worker: `python/barca/_dedupe.py`.)

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::dag::Dag;
use crate::model::{DagNode, NodeKind};
use crate::planner::ExecutionPlan;

/// Print `[barca] warning: <message>` on stderr unless this process already printed the same
/// message. Returns whether it printed.
///
/// A one-shot command plans once, so this only matters there as a guarantee; `barca serve`
/// plans and configures on every run it starts, and the same warning on every tick would bury
/// its log.
pub fn warn_once(message: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = SEEN
        .get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(message.to_string()))
        .unwrap_or(true);
    if first {
        eprintln!("[barca] warning: {message}");
    }
    first
}

/// The `kind` of the unused-input warning.
pub const UNUSED_INPUT: &str = "unused_input";

/// A plan-time warning about one step, found by static analysis (no user code is imported).
///
/// `get`, `run`, their `--dry-run` and `plan` report the same list for the same plan: one line
/// each on stderr and the `warnings` array in JSON output (`barca docs contract`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PlanWarning {
    /// What was found. `unused_input` is the only kind so far.
    pub kind: String,
    /// The step's node id (`file.py:name`).
    pub node: String,
    /// The parameter concerned.
    pub param: String,
    /// The warning in words, with what to do about it.
    pub message: String,
}

/// The warnings for the steps of `plan`, in plan order: one per (step, unused input).
///
/// Only planned steps are looked at, so a command warns about the cone it was asked for and
/// not about the rest of the file. A partitioned step is one step here, whatever its key
/// count, and whether a step will be served from cache makes no difference: the list is a
/// function of the source and the targets only.
pub fn for_plan(dag: &Dag, plan: &ExecutionPlan) -> Vec<PlanWarning> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for step in plan
        .phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
    {
        let id = step.step_id.base_id();
        if !seen.insert(id) {
            continue;
        }
        if let Some(node) = dag.get_node(id) {
            out.extend(unused_input_warnings(dag, node));
        }
    }
    out
}

/// One warning per data input `node`'s function never uses.
///
/// An input whose upstream is a sensor is not reported: depending on a sensor without reading
/// its value is how a step is made to re-run when outside state changes (`barca docs cache`).
pub fn unused_input_warnings(dag: &Dag, node: &DagNode) -> Vec<PlanWarning> {
    let from_sensor = |param: &String| {
        node.resolved_inputs
            .get(param)
            .or_else(|| node.resolved_collected.get(param))
            .and_then(|upstream| dag.get_node(upstream))
            .is_some_and(|upstream| upstream.kind() == NodeKind::Sensor)
    };
    node.extracted
        .unused_inputs
        .iter()
        .filter(|param| !from_sensor(param))
        .map(|param| {
            // What the unused input costs depends on how it would have been read.
            let cost = match node.extracted.param_types.get(param) {
                Some(t) if t.is_lazy() => format!(
                    "It is annotated as a lazy input ({}), so a parquet artifact is opened but \
                     not read; it still counts toward the step's cache key",
                    t.as_str()
                ),
                _ => "It is still loaded in full each time the step runs, and it counts toward \
                      the step's cache key"
                    .to_string(),
            };
            PlanWarning {
                kind: UNUSED_INPUT.to_string(),
                node: node.id.clone(),
                param: param.clone(),
                message: format!(
                    "{id} never uses its input `{param}`. {cost}. Use it, remove it from \
                     inputs=, or rename the parameter `_{param}` if it is there for ordering \
                     only (a `_` input is not loaded and never flagged)",
                    id = node.id
                ),
            }
        })
        .collect()
}

/// Print each warning on stderr as `[barca] warning: <message>`, once per process.
pub fn print(warnings: &[PlanWarning]) {
    for w in warnings {
        warn_once(&w.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::plan_for_targets;
    use crate::parse::extract_nodes;
    use crate::planner::{ResourceConfig, plan_from_dag};

    const SRC: &str = r#"
import duckdb
import polars as pl
from barca import asset, collect, partitions, sensor

@asset
def raw(): return 1

@sensor
def etag(): return True, "v1"

@asset(inputs={"etag": etag, "raw": raw})
def bronze(etag, raw):
    return raw

@asset(inputs={"raw": raw})
def eager(raw: dict):
    return 1

@asset(inputs={"raw": raw})
def duck(raw: duckdb.DuckDBPyRelation):
    return duckdb.sql("select * from raw")

@asset(inputs={"raw": raw})
def lazy_polars(raw: pl.LazyFrame):
    return 1

@asset(inputs={"raw": raw}, partitions={"k": partitions(["a", "b", "c"])})
def per_key(raw, k):
    return k

@asset(inputs={"parts": collect(per_key), "raw": raw})
def total(parts, raw):
    return raw

@asset(inputs={"raw": raw})
def unrelated(raw):
    return raw
"#;

    fn dag() -> Dag {
        Dag::build(&extract_nodes(SRC, "p.py").unwrap()).unwrap()
    }

    fn config() -> ResourceConfig {
        ResourceConfig {
            pool_size: 4,
            concurrency_groups: Default::default(),
        }
    }

    fn pairs(warnings: &[PlanWarning]) -> Vec<(&str, &str)> {
        warnings
            .iter()
            .map(|w| (w.node.as_str(), w.param.as_str()))
            .collect()
    }

    #[test]
    fn whole_file_plan_reports_every_unused_input_once() {
        let dag = dag();
        let mut got = for_plan(&dag, &plan_from_dag(&dag, &config()));
        got.sort_by(|a, b| a.node.cmp(&b.node));
        assert_eq!(
            pairs(&got),
            [
                ("p.py:eager", "raw"),
                ("p.py:lazy_polars", "raw"),
                // three partition keys, one warning
                ("p.py:per_key", "raw"),
                ("p.py:total", "parts"),
            ]
        );
        assert!(got.iter().all(|w| w.kind == UNUSED_INPUT));
    }

    #[test]
    fn only_the_targets_cone_is_reported() {
        let dag = dag();
        let plan = plan_for_targets(&dag, &["p.py:unrelated"], &config(), "get");
        assert_eq!(for_plan(&dag, &plan), []);
        let plan = plan_for_targets(&dag, &["p.py:total"], &config(), "get");
        let got = for_plan(&dag, &plan);
        let mut p = pairs(&got);
        p.sort();
        assert_eq!(p, [("p.py:per_key", "raw"), ("p.py:total", "parts")]);
    }

    #[test]
    fn the_message_is_true_for_the_inputs_type_and_says_what_to_do() {
        let dag = dag();
        let message = |id: &str| {
            unused_input_warnings(&dag, dag.get_node(id).unwrap())[0]
                .message
                .clone()
        };

        let eager = message("p.py:eager");
        assert!(
            eager.starts_with("p.py:eager never uses its input `raw`."),
            "{eager}"
        );
        assert!(
            eager.contains("loaded in full each time the step runs"),
            "{eager}"
        );
        for fix in [
            "Use it",
            "remove it from inputs=",
            "rename the parameter `_raw`",
        ] {
            assert!(eager.contains(fix), "{eager}");
        }

        let lazy = message("p.py:lazy_polars");
        assert!(!lazy.contains("loaded in full"), "{lazy}");
        assert!(lazy.contains("lazy input (polars_lazy)"), "{lazy}");
        assert!(lazy.contains("not read"), "{lazy}");
        assert!(lazy.contains("rename the parameter `_raw`"), "{lazy}");
    }

    #[test]
    fn an_unused_sensor_input_is_a_cache_trigger_not_a_warning() {
        let dag = dag();
        let bronze = dag.get_node("p.py:bronze").unwrap();
        assert_eq!(bronze.extracted.unused_inputs, ["etag"]);
        assert_eq!(unused_input_warnings(&dag, bronze), []);
    }

    #[test]
    fn warn_once_prints_a_message_once_per_process() {
        assert!(warn_once("warnings.rs test: only once"));
        assert!(!warn_once("warnings.rs test: only once"));
        assert!(warn_once("warnings.rs test: another message"));
    }
}
