//! The layers still inside `barca-core` while it is being dissolved (#1056, C6): each layer
//! leaves for a crate of its own, where Cargo checks what this test checks here. A module may
//! name a module of its own layer or of a layer below it; `engine` and `analysis` sit side by
//! side and never name each other; an analysis module names only the rows before its own.

use barca_model::source_scan::module_graph::{cycle_problem, line_of, scan};
use std::path::Path;

const LAYERS: &[(&str, u8, &[&str])] = &[
    // The facade over `barca_db::db`.
    ("leaf", 0, &["db"]),
    (
        "engine",
        3,
        &[
            "commands",
            "execution",
            "incremental",
            "mismatch",
            "persist",
            "telemetry",
        ],
    ),
    (
        "analysis",
        3,
        &[
            "causes",
            "estimates",
            "health",
            "history_filter",
            "key_query",
            "liveness",
            "partition_grid",
            "refresh_path",
            "rollup",
            "run_explanation",
            "run_list",
            "schedule_standing",
            "status",
            "views",
        ],
    ),
    (
        "service",
        4,
        &[
            "diagnostics",
            "outcome",
            "queries",
            "report",
            "serve",
            "sql",
            "values",
            "served",
        ],
    ),
];

const ANALYSIS_ROWS: &[&[&str]] = &[
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

const LARGEST_CYCLE: usize = 2;

fn layer_of(module: &str) -> Option<(&'static str, u8)> {
    LAYERS
        .iter()
        .find(|(_, _, modules)| modules.contains(&module))
        .map(|(name, rank, _)| (*name, *rank))
}

fn analysis_row(module: &str) -> Option<usize> {
    ANALYSIS_ROWS.iter().position(|row| row.contains(&module))
}

fn allowed(from: &str, to: &str) -> bool {
    match (layer_of(from), layer_of(to)) {
        (Some(("analysis", _)), Some(("analysis", _))) => {
            match (analysis_row(from), analysis_row(to)) {
                (Some(a), Some(b)) => b < a,
                _ => false,
            }
        }
        (Some((a, ra)), Some((b, rb))) => a == b || rb < ra,
        _ => false,
    }
}

#[test]
fn modules_name_only_their_own_layer_and_the_layers_below() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (modules, edges) = scan(&src_root);
    let mut problems = Vec::new();
    for module in &modules {
        if layer_of(module).is_none() {
            problems.push(format!("  `mod {module}` has no layer: add it to LAYERS"));
        }
    }
    for module in LAYERS.iter().flat_map(|(_, _, listed)| listed.iter()) {
        if !modules.contains(*module) {
            problems.push(format!(
                "  LAYERS names `{module}`, which lib.rs does not declare"
            ));
        }
    }
    for ((from, to), items) in &edges {
        if !allowed(from, to) {
            problems.push(format!("  wrong-way edge: {}", line_of(from, to, items)));
        }
    }
    problems.extend(cycle_problem(&src_root, LARGEST_CYCLE).map(|p| format!("  {p}")));
    assert!(
        problems.is_empty(),
        "module layers of barca-core (#1056):\n{}",
        problems.join("\n")
    );
}
