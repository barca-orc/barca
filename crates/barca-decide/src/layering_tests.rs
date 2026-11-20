//! The modules of `barca-decide` (#1056; `docs/engineering/crate-split-2.md`). Cargo keeps this
//! crate below the run orchestrator and the read models and off `barca-engine`; it checks
//! nothing between the modules of one crate. These are not ordered yet: `cache`, `changes`,
//! `freshness` and `plan_reasons` name each other. Until they are, the size of that group is a
//! number that may only shrink.

use barca_model::source_scan::module_graph::cycle_problem;
use std::path::Path;

/// The number of modules in the largest group that all reach each other through `crate::`
/// paths in non-test code: `cache`, `changes`, `freshness`, `plan_reasons`.
const LARGEST_CYCLE: usize = 4;

#[test]
fn the_largest_module_cycle_does_not_grow() {
    let problem = cycle_problem(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        LARGEST_CYCLE,
    );
    assert!(
        problem.is_none(),
        "modules of barca-decide (LARGEST_CYCLE in crates/barca-decide/src/layering_tests.rs): {}",
        problem.unwrap_or_default()
    );
}
