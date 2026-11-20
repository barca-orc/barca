//! The modules of `barca-run` (#1056; `docs/engineering/crate-split-2.md`). Cargo keeps this
//! crate off the read models and below the service layer; it checks nothing between the modules
//! of one crate. These are not ordered yet: the size of the largest group that all reach each
//! other is a number that may only shrink.

use barca_model::source_scan::module_graph::cycle_problem;
use std::path::Path;

/// The number of modules in the largest group that all reach each other through `crate::`
/// paths in non-test code: `persist` and `telemetry`.
const LARGEST_CYCLE: usize = 2;

#[test]
fn the_largest_module_cycle_does_not_grow() {
    let problem = cycle_problem(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        LARGEST_CYCLE,
    );
    assert!(
        problem.is_none(),
        "modules of barca-run (LARGEST_CYCLE in crates/barca-run/src/layering_tests.rs): {}",
        problem.unwrap_or_default()
    );
}
