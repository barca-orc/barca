//! The order inside `barca-compile` (#1056, track D and C6; `docs/engineering/compile-layering.md`).
//! This crate is planning: a graph in (`barca_model::dag`, made of `barca_model::ir`), an
//! execution plan out. The Python frontend is `barca-python`, a dev-dependency only, so Cargo
//! keeps planning from naming it. Cargo checks nothing between the modules of one crate, so this
//! test does: a module names only the modules of the rows before its own, never its own row or
//! a later one, so they cannot form a cycle. No exception is listed.

use barca_model::source_scan::module_graph::rows_problems;
use std::path::Path;

/// First row first. A new module gets a row here.
const ROWS: &[&[&str]] = &[
    // The value of `--partition`.
    &["partition_selection"],
    // The execution plan.
    &["planner"],
    // Narrowing a plan to `--partition`; plan-time warnings.
    &["selection", "warnings"],
    // The targets a command names, and their outcomes.
    &["targets"],
    // Expanding a planned phase: `partitions_from` keys, the inputs a phase provides.
    &["dispatch"],
];

#[test]
fn modules_name_only_the_rows_before_their_own() {
    let problems = rows_problems(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), ROWS);
    assert!(
        problems.is_empty(),
        "the order inside barca-compile (ROWS in crates/barca-compile/src/layering_tests.rs). \
         Move the item to a row before both of its users, or pass it in:\n{}",
        problems.join("\n")
    );
}
