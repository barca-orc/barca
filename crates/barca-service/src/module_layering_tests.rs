//! The modules of `barca-service` (#1056; `docs/engineering/crate-split-2.md`). The layers that
//! were modules of `barca-core` are crates now, and Cargo orders them. What is left here is the
//! service layer: one operation per command, its typed result, its text. Cargo checks nothing
//! between the modules of one crate, so this test does: no group of them reaches itself through
//! `crate::` paths in non-test code.

use barca_model::source_scan::module_graph::cycle_problem;
use std::path::Path;

/// The number of modules in the largest group that all reach each other: 1, which is no cycle.
/// It was 5 (`outcome`, `queries`, `report`, `run_explanation`, `run_list`) until the run view's
/// conversion moved beside its type.
const LARGEST_CYCLE: usize = 1;

#[test]
fn no_group_of_modules_reaches_itself() {
    let problem = cycle_problem(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        LARGEST_CYCLE,
    );
    assert!(
        problem.is_none(),
        "modules of barca-service (crates/barca-service/src/module_layering_tests.rs): {}. Move \
         the item down a crate, or pass it in as a value",
        problem.unwrap_or_default()
    );
}
