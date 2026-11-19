//! The order inside `barca-analysis` (#1056; `docs/engineering/crate-split-2.md`). Cargo keeps
//! this crate off the run orchestrator and the engine; it checks nothing between the modules of
//! one crate, so this test does. A read model names only the read models of the rows before its
//! own, never its own row or a later one, so they cannot form a cycle. No exception is listed.
//! Moving the item to a row before both of its users, or passing it in as a value, is the fix
//! for a failure here.

use barca_model::source_scan::module_graph::rows_problems;
use std::path::Path;

/// First row first. A new module gets a row here.
const ROWS: &[&[&str]] = &[
    // The shared view types, and the modules that name no other read model.
    &["views", "history_filter", "key_query", "run_explanation"],
    &["schedule_standing", "partition_grid"],
    &["liveness"],
    &["refresh_path"],
    &["estimates"],
    &["causes"],
    &["rollup"],
    &["health", "run_list"],
    &["status"],
];

#[test]
fn read_models_name_only_the_rows_before_their_own() {
    let problems = rows_problems(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), ROWS);
    assert!(
        problems.is_empty(),
        "the order inside barca-analysis (ROWS in crates/barca-analysis/src/layering_tests.rs). \
         Move the item to a row before both of its users, or pass it in:\n{}",
        problems.join("\n")
    );
}
