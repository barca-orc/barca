//! The layers still inside `barca-core` while it is being dissolved (#1056, C6): each layer
//! leaves for a crate of its own, where Cargo checks what this test checks here. A module may
//! name a module of its own layer or of a layer below it.

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

const LARGEST_CYCLE: usize = 2;

fn layer_of(module: &str) -> Option<(&'static str, u8)> {
    LAYERS
        .iter()
        .find(|(_, _, modules)| modules.contains(&module))
        .map(|(name, rank, _)| (*name, *rank))
}

fn allowed(from: &str, to: &str) -> bool {
    match (layer_of(from), layer_of(to)) {
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
