//! The order inside `barca-python` (#1056, track D and C6; `docs/engineering/compile-layering.md`).
//! This crate is the Python frontend: project files in, a graph out (`barca_model::dag`). It
//! names `barca-model` and no other barca crate, so nothing of planning. Cargo checks nothing
//! between the modules of one crate, so this test does: a module names only the modules of the
//! rows before its own, never its own row or a later one, so they cannot form a cycle. No
//! exception is listed. The rows are an order the code has, not a design: a row can be merged or
//! reordered when a move needs it.

use barca_model::source_scan::module_graph::rows_problems;
use std::path::Path;

/// First row first. A new module gets a row here.
const ROWS: &[&[&str]] = &[
    // The accepted decorator arguments, reading one argument value, what the parser produces
    // for a function, where a file stops parsing, environment reads, the modules the
    // unused-input check ignores.
    &[
        "arg_values",
        "decorator_args",
        "env_reads",
        "extracted",
        "syntax_problem",
        "unrelated_modules",
    ],
    // `batch=`, what a file's names are bound to, the per-decorator checks, which node a
    // reference means.
    &[
        "batch_spec",
        "file_names",
        "input_params",
        "node_refs",
        "sink_args",
        "unused_inputs",
    ],
    // Which barca decorator an expression is.
    &["decorators"],
    // The definition fingerprint; group declarations.
    &["definition", "groups"],
    // The dependency cone of a function.
    &["cone"],
    // Extracted nodes of a file; the project's modules and their cones.
    &["parse", "project_modules"],
    // From extracted nodes to the graph: references resolved, fingerprints computed.
    &["link"],
    // Project files in, a graph out.
    &["discover", "load"],
];

#[test]
fn modules_name_only_the_rows_before_their_own() {
    let problems = rows_problems(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), ROWS);
    assert!(
        problems.is_empty(),
        "the order inside barca-python (ROWS in crates/barca-python/src/layering_tests.rs). Move \
         the item to a row before both of its users, or pass it in:\n{}",
        problems.join("\n")
    );
}
