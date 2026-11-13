//! The layering guard (#905 step 1): every top-level module of `barca-core` belongs to one layer,
//! and non-test code may only name (`crate::...`, `super::...` up to the crate root, and bare
//! module paths in `lib.rs`) a module in a layer its own layer may use:
//!
//! ```text
//! model   <  compile  <  engine  <  core
//! model   <  db                  <  core
//! ```
//!
//! `compile` and `db` are siblings: both use `model`, neither uses the other. `engine` (the
//! per-step scheduler and worker loop) uses `model` and `compile` but not `db`. `execution`, the
//! run orchestrator that decides caching, freshness and publication, is `core`. `core` (the
//! remainder) uses everything, and nothing uses `core`. A layer is the crate it becomes in
//! #905 step 4 (`barca-model`, `barca-compile`, `barca-db`, `barca-engine`, `barca-core`).
//!
//! Editing this file:
//! - A new module goes in [`LAYERS`]; a module missing from it fails the test.
//! - A module that straddles two layers can be split: a `parent::child` entry places that file
//!   (or directory) apart from its parent, and wins over it (longest prefix).
//! - [`EXCEPTIONS`] is today's upward edges, one line per (from, to) module pair. A #905 step
//!   that cuts an edge deletes its line (the test fails while a stale line remains); no step
//!   adds one. Move the item down, or pass it in as a value, instead.

use crate::source_scan::{Tok, is_cfg_test, lex_with_lines, non_test_sources, skip_cfg_test_item};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    /// Shared vocabulary: values, ids, hashes, time, the terminal, the config file.
    Model,
    /// Source in, `Dag` and `ExecutionPlan` out. No libsql, no tokio.
    Compile,
    /// The metadata database (`.barca/metadata.db`) and what decides about its rows.
    Db,
    /// Plan in, results out: the coordinator, workers, the wire protocol.
    Engine,
    /// The remainder: cache policy, stores, history, reports, commands, the server's state.
    Core,
}

use Layer::{Compile, Core, Db, Engine, Model};

impl Layer {
    fn may_use(self, other: Layer) -> bool {
        match self {
            Model => other == Model,
            Compile => matches!(other, Model | Compile),
            Db => matches!(other, Model | Db),
            Engine => matches!(other, Model | Compile | Engine),
            Core => true,
        }
    }
}

/// Every top-level module (and `lib`, the crate root's own items), with its layer. Modules the
/// #905 Target table names are placed as it says; the others by what they use and who uses them.
#[rustfmt::skip]
const LAYERS: &[(&str, Layer)] = &[
    // ---- model (barca-model) ----
    ("lib", Model), // `BarcaError`, `FailedStep`, `PartialRun`, `safe_node_id`: used everywhere
    ("model", Model),
    ("keyed", Model),
    ("step_status", Model),
    ("cache_state", Model),
    ("artifact_format", Model),
    ("time", Model),
    ("lines", Model),
    ("suggest", Model),
    ("envdeps", Model),
    ("hash", Model),
    ("term", Model),               // `outln!`/`errln!` are used by every layer
    ("jsonlog", Model),            // `term` renders through it
    ("events", Model),             // `RunEvent`/`LogStream`: `lines` and `db` name them
    ("config", Model), // `barca.toml`: `db` and `dag` read it
    ("cache_size", Model), // `[cache] max_size` values: `config` parses them, `gc` applies them
    ("uri", Model),        // URLs and store URIs as shown (credentials out), recorded local paths
    ("layout", Model), // `.barca/` paths from the config: `db` and `gc` use them
    ("source_observation", Model), // compact observation values over `keyed`
    ("test_hooks", Model), // `BARCA_TEST_*` reads, used by every layer
    // ---- compile (barca-compile) ----
    ("parse", Compile),
    ("cone", Compile),
    ("definition", Compile),
    ("decorator_args", Compile),
    ("groups", Compile),
    ("input_params", Compile),
    ("sink_args", Compile),
    ("env_reads", Compile),
    ("unused_inputs", Compile),
    ("unrelated_modules", Compile), // the allow-list behind `unused_inputs`
    ("project_modules", Compile),
    ("dag", Compile),
    ("targets", Compile),
    ("selection", Compile),
    ("planner", Compile),
    ("discover", Compile), // which `.py` files make up the project
    ("load", Compile),     // project to `Dag` (#905: "source in, Dag out")
    ("warnings", Compile), // plan-time warnings, over `dag` and `planner`
    // ---- db (barca-db) ----
    ("db", Db),
    ("persist::recording", Db),
    ("run_owner", Db),
    ("state_validate", Db),
    ("metadata_size", Db),
    ("sink_publications", Db), // which run hash each sink last published: rows of `db`
    ("run_logs", Db),          // a run's recorded output: the `logs` rows and their spool
    ("pending_uploads", Db),   // uploads still owed: rows `db` reads
    // ---- engine (barca-engine) ----
    ("coordinator", Engine),
    ("dispatch", Engine),
    ("io_loop", Engine),
    ("protocol", Engine),
    ("wire", Engine),
    ("result_journal", Engine),
    ("helper_proc", Engine),
    ("batch", Engine),
    ("run_host", Engine),
    ("interrupt", Engine),     // how a run is told to stop
    ("cost", Engine),          // measured cost and adaptive batch sizing for the dispatcher
    ("dataset_sinks", Engine), // a step's `@sink` writes, through `helper_proc`
    ("observation", Engine),   // a sensor's freshness revision from its result
    // ---- core (barca-core, the remainder) ----
    ("execution", Core), // the run orchestrator: cache, freshness, publication, backup (#905)
    ("persist", Core), // the run's recording driver and telemetry report; `recording` is db
    ("backup", Core),
    ("cache", Core),
    ("changes", Core),
    ("commands", Core),
    ("diagnostics", Core),
    ("envelope", Core),
    ("freshness", Core),
    ("gc", Core),
    ("history_filter", Core),
    ("http", Core), // outgoing HTTPS: only `config`'s upward edge reaches it from below
    ("incremental", Core),
    ("ledger", Core),
    ("mismatch", Core),
    ("outcome", Core),
    ("partition_grid", Core),
    ("pauses", Core),
    ("pins", Core),
    ("queries", Core),
    ("recover", Core),
    ("report", Core),
    ("results", Core),
    ("schedule", Core),
    ("serve", Core),
    ("sql", Core),
    ("status", Core),
    ("store_inspect", Core),
    ("store_sync", Core),
    ("telemetry", Core),
    ("transfer", Core),
    ("values", Core),
];

/// Crate-root names that are not modules: where each lives. A name not here and not a module
/// must be one of `model`'s (`pub use model::*`).
const ROOT_ITEMS: &[(&str, &str)] = &[
    ("BarcaError", "lib"),
    ("FailedStep", "lib"),
    ("PartialRun", "lib"),
    ("safe_node_id", "lib"),
    ("release_free_memory", "lib"),
    ("Dag", "dag"),
    ("LogStream", "events"),
    ("RunEvent", "events"),
    ("sizing", "execution"),
    ("outln", "term"),
    ("errln", "term"),
    ("run_line", "lines"),
    ("CancellationToken", "lib"), // tokio_util's, re-exported
];

/// Today's upward edges: (from, to, what cuts it), grouped by the #905 step that resolves them.
/// Delete a line when its edge is gone; never add one.
#[rustfmt::skip]
const EXCEPTIONS: &[(&str, &str, &str)] = &[
    // ---- step 2, `planner` and `dag`: helpers move to the compile side ----
    ("dag", "changes", "changes::definition_components"),
    ("planner", "cache", "cache::folds_output"),
    // ---- step 2, `dispatch` types: OutputRef, StepFailure, ErrorPhase move to model ----
    ("db", "dispatch", "dispatch::{OutputRef, StepFailure}"),
    ("lib", "dispatch", "FailedStep.phase: dispatch::ErrorPhase; PartialRun: dispatch::OutputRef"),
    ("selection", "dispatch", "dispatch::OutputRef"),
    ("targets", "dispatch", "dispatch::{OutputRef, StepFailure}"),
    // ---- step 2, `db` value types move down; DDL moves into db/schema ----
    ("db", "cache", "cache::READ_STAMP_REFRESH"),
    ("db", "changes", "changes::DEFINITION_KEY"),
    ("db", "cost", "cost::NodeEstimate"),
    ("db", "incremental", "incremental::CREATE (freshness_proofs DDL)"),
    ("db", "ledger", "ledger::{Ledger, LedgerRun, LedgerStep}"),
    ("db", "outcome", "outcome::{Trigger, TriggeredBy}"),
    ("db", "serve", "serve::run_queue::{CREATE_QUEUE, CREATE_SERVER_STATE}"),
    ("db", "telemetry", "telemetry::is_on, telemetry::datadog::step_trace_ref"),
    ("persist::recording", "ledger", "ledger::{LedgerPost, PostAccepted}"),
    // ---- step 2, `pid_alive` and host identity move to a shared process module ----
    ("db", "helper_proc", "helper_proc::Until (the deadline helper beside pid_alive)"),
    ("io_loop", "db", "db::pid_alive"),
    ("protocol", "db", "db::pid_alive"),
    // ---- step 3, the engine and storage seam: the engine reads no metadata ----
    ("observation", "db", "db::CacheReader"),
    // ---- no #905 step names these yet; each needs a decision before step 4 ----
    // `results` holds report types every layer returns: the ones below it use move to model.
    ("batch", "results", "results::{BatchPlan, StepReport}"),
    ("lib", "results", "PartialRun: results::{StepReport, KeySummary}"),
    ("pending_uploads", "results", "results::UploadsReport"),
    ("selection", "results", "results::{ExplainResult, StepReport, PartitionSelectionReport, ...}"),
    ("targets", "results", "results::TargetOutcome"),
    // The crate root's error types name compile-side types.
    ("lib", "dag", "BarcaError::Dag(dag::DagError)"),
    ("lib", "warnings", "PartialRun.warnings: warnings::PlanWarning"),
    // `batch` straddles compile (parsing `batch=`, evaluating groups) and engine (calls).
    ("load", "batch", "batch::{evaluate_groups, check_label_function}"),
    ("parse", "batch", "batch::{FIX, extract, scalar_annotation, BatchOverride}"),
    // The partition-key resolver runs a project subprocess (RFC-0013).
    ("load", "helper_proc", "helper_proc::spawn_std"),
    // `telemetry` straddles: the Datadog dispatch and log ids are used below it.
    ("io_loop", "telemetry", "telemetry::datadog::Dispatch"),
    ("jsonlog", "telemetry", "telemetry::datadog::log_ids"),
    ("model", "env_reads", "env_reads::undeclared"),
    // `db::run_steps::planned_run_nodes` rebuilds a plan: a query, not storage.
    ("db", "dag", "dag::Dag (planned_run_nodes)"),
    ("db", "planner", "planner::ResourceConfig (planned_run_nodes)"),
    ("db", "targets", "targets::{plan_for_targets, blocking_failure} (planned_run_nodes)"),
    // `pending_uploads` straddles: rows (db) and the upload itself (core).
    ("pending_uploads", "execution", "execution::upload_pending (upload_now)"),
    ("persist::recording", "persist", "persist::RunLedger"),
    ("selection", "report", "report::kind_str"),
    ("targets", "recover", "recover::returned_step"),
];

/// One reference: the crate-relative path it names and the line it is on.
type Ref = (Vec<String>, usize);

/// The module path of a source file: `db/runs.rs` is `db::runs`, `db/mod.rs` is `db`, `lib.rs`
/// is the crate root (empty).
fn module_path(rel: &str) -> Vec<String> {
    let rel = rel.strip_suffix(".rs").unwrap_or(rel);
    let mut segs: Vec<String> = rel.split('/').map(str::to_string).collect();
    if segs.last().is_some_and(|s| s == "mod") || segs == ["lib"] {
        segs.pop();
    }
    segs
}

/// Parse a use tree or path after `prefix::` at `i`; each leaf's full path lands in `out`.
/// Returns the index after the tree.
fn parse_tree(
    toks: &[Tok],
    lines: &[usize],
    mut i: usize,
    prefix: Vec<String>,
    out: &mut Vec<Ref>,
) -> usize {
    let mut path = prefix;
    loop {
        match toks.get(i) {
            Some(Tok::Ident(name)) => {
                let line = lines[i];
                if name != "self" {
                    path.push(name.clone());
                }
                i += 1;
                if toks.get(i) == Some(&Tok::Punct(':'))
                    && toks.get(i + 1) == Some(&Tok::Punct(':'))
                {
                    i += 2;
                    continue;
                }
                out.push((path, line));
                return i;
            }
            Some(Tok::Punct('{')) => {
                i += 1;
                while i < toks.len() && toks[i] != Tok::Punct('}') {
                    if toks[i] == Tok::Punct(',') {
                        i += 1;
                        continue;
                    }
                    let next = parse_tree(toks, lines, i, path.clone(), out);
                    i = next.max(i + 1);
                    // Skip `as alias` and anything else up to the next item.
                    while i < toks.len() && !matches!(toks[i], Tok::Punct(',' | '}')) {
                        i += 1;
                    }
                }
                return i + 1;
            }
            _ => {
                // `*`, or a path cut short: the prefix itself is what is named.
                if !path.is_empty() {
                    out.push((path, lines.get(i).copied().unwrap_or(0)));
                }
                return i;
            }
        }
    }
}

fn is_path_sep(toks: &[Tok], i: usize) -> bool {
    toks.get(i) == Some(&Tok::Punct(':')) && toks.get(i + 1) == Some(&Tok::Punct(':'))
}

/// Every crate-relative path non-test code in a file at `module` names: `crate::` (and
/// `$crate::`) paths and use trees, and `super::` paths that climb to the crate root or past
/// the file's top-level module. In the crate root (`module` empty), bare paths that start with
/// one of `root_modules` count too, except in `use` items (re-exports, see [`ROOT_ITEMS`]).
fn references(src: &str, module: &[String], root_modules: &BTreeSet<String>) -> Vec<Ref> {
    let (toks, lines) = lex_with_lines(src);
    let mut out = Vec::new();
    // Inline modules (`mod name { ... }`) open at a brace depth: `super::` starts from them.
    let mut module = module.to_vec();
    let mut inline: Vec<usize> = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < toks.len() {
        if is_cfg_test(&toks, i) {
            i = skip_cfg_test_item(&toks, i).0;
            continue;
        }
        match &toks[i] {
            Tok::Punct('{') => depth += 1,
            Tok::Punct('}') => {
                if inline.last() == Some(&depth) {
                    inline.pop();
                    module.pop();
                }
                depth = depth.saturating_sub(1);
            }
            Tok::Ident(m) if m == "mod" => {
                if let (Some(Tok::Ident(name)), Some(Tok::Punct('{'))) =
                    (toks.get(i + 1), toks.get(i + 2))
                {
                    module.push(name.clone());
                    inline.push(depth + 1);
                }
            }
            _ => {}
        }
        let after_sep = i >= 2 && is_path_sep(&toks, i - 2);
        match &toks[i] {
            Tok::Ident(w) if w == "crate" && is_path_sep(&toks, i + 1) && !after_sep => {
                i = parse_tree(&toks, &lines, i + 3, Vec::new(), &mut out);
                continue;
            }
            Tok::Ident(w) if w == "super" && is_path_sep(&toks, i + 1) && !after_sep => {
                let mut up = 0;
                let mut j = i;
                while toks.get(j) == Some(&Tok::Ident("super".into())) && is_path_sep(&toks, j + 1)
                {
                    up += 1;
                    j += 3;
                }
                let base = module.len().saturating_sub(up);
                let prefix = module[..base].to_vec();
                i = parse_tree(&toks, &lines, j, prefix, &mut out);
                continue;
            }
            Tok::Ident(w) if w == "use" && module.is_empty() => {
                // A re-export in the crate root: resolved through `ROOT_ITEMS` where it is used.
                while i < toks.len() && toks[i] != Tok::Punct(';') {
                    i += 1;
                }
            }
            Tok::Ident(w)
                if module.is_empty()
                    && root_modules.contains(w)
                    && is_path_sep(&toks, i + 1)
                    && !after_sep =>
            {
                i = parse_tree(&toks, &lines, i, Vec::new(), &mut out);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The non-test modules `lib.rs` declares.
fn declared_modules(lib_src: &str) -> BTreeSet<String> {
    let (toks, _) = lex_with_lines(lib_src);
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < toks.len() {
        if is_cfg_test(&toks, i) {
            i = skip_cfg_test_item(&toks, i).0;
            continue;
        }
        if let (Tok::Ident(m), Some(Tok::Ident(name)), Some(Tok::Punct(';'))) =
            (&toks[i], toks.get(i + 1), toks.get(i + 2))
            && m == "mod"
        {
            out.insert(name.clone());
        }
        i += 1;
    }
    out
}

/// The `LAYERS` key a module path falls under: the longest entry that is a prefix of it.
fn key_of(path: &[String]) -> Option<&'static str> {
    (1..=path.len().min(2)).rev().find_map(|n| {
        let joined = path[..n].join("::");
        LAYERS.iter().find(|(k, _)| *k == joined).map(|(k, _)| *k)
    })
}

fn layer_of(key: &str) -> Layer {
    LAYERS.iter().find(|(k, _)| *k == key).unwrap().1
}

#[test]
fn modules_only_use_layers_below_them() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sources = non_test_sources(&root);
    let lib_src = &sources.iter().find(|(rel, _)| rel == "lib.rs").unwrap().1;
    let modules = declared_modules(lib_src);
    let model_src = std::fs::read_to_string(root.join("model.rs")).unwrap();
    let model_names: BTreeSet<String> = {
        let (toks, _) = lex_with_lines(&model_src);
        toks.windows(2)
            .filter_map(|w| match (&w[0], &w[1]) {
                (Tok::Ident(kw), Tok::Ident(name))
                    if matches!(
                        kw.as_str(),
                        "struct" | "enum" | "type" | "fn" | "const" | "trait" | "static"
                    ) =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect()
    };

    let mut problems = Vec::new();
    for m in &modules {
        if !LAYERS.iter().any(|(k, _)| k == m) {
            problems.push(format!(
                "  module `{m}` has no layer: add it to LAYERS in src/layering_tests.rs"
            ));
        }
    }
    for (k, _) in LAYERS {
        let top = k.split("::").next().unwrap();
        let known = *k == "lib"
            || (modules.contains(top)
                && (!k.contains("::") || {
                    let rel = k.replace("::", "/");
                    root.join(format!("{rel}.rs")).exists()
                        || root.join(&rel).join("mod.rs").exists()
                }));
        if !known {
            problems.push(format!(
                "  LAYERS names `{k}`, which is not a module of this crate"
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for (k, _) in LAYERS {
        if !seen.insert(*k) {
            problems.push(format!("  LAYERS names `{k}` twice"));
        }
    }
    assert!(problems.is_empty(), "layer table:\n{}", problems.join("\n"));

    // (from, to) -> the places that name it.
    let mut violations: BTreeMap<(&str, &str), Vec<String>> = BTreeMap::new();
    let mut used = BTreeSet::new();
    for (rel, src) in &sources {
        if rel == "layering_tests.rs" {
            continue;
        }
        let module = module_path(rel);
        let from = if module.is_empty() {
            "lib"
        } else {
            key_of(&module).unwrap_or_else(|| panic!("{rel}: no layer for {module:?}"))
        };
        for (path, line) in references(src, &module, &modules) {
            let Some(first) = path.first() else { continue };
            let to = if modules.contains(first) {
                key_of(&path).unwrap_or_else(|| panic!("{rel}:{line}: no layer for {path:?}"))
            } else if let Some((_, owner)) = ROOT_ITEMS.iter().find(|(n, _)| n == first) {
                key_of(&[owner.to_string()]).unwrap()
            } else if model_names.contains(first) {
                "model"
            } else {
                problems.push(format!(
                    "  {rel}:{line}: `crate::{first}` is neither a module nor a known root item: \
                     add it to ROOT_ITEMS in src/layering_tests.rs"
                ));
                continue;
            };
            if from == to || layer_of(from).may_use(layer_of(to)) {
                continue;
            }
            if EXCEPTIONS.iter().any(|(f, t, _)| *f == from && *t == to) {
                used.insert((from, to));
                continue;
            }
            violations
                .entry((from, to))
                .or_default()
                .push(format!("{rel}:{line} ({})", path.join("::")));
        }
    }
    assert!(
        problems.is_empty(),
        "unresolved references:\n{}",
        problems.join("\n")
    );
    let report: Vec<String> = violations
        .iter()
        .map(|((from, to), places)| {
            format!(
                "  {from} ({:?}) -> {to} ({:?}):\n{}",
                layer_of(from),
                layer_of(to),
                places
                    .iter()
                    .map(|p| format!("      {p}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .collect();
    assert!(
        report.is_empty(),
        "upward module references (#905): move the item into a layer the user may use, or pass \
         it in as a value; do not add an exception. Layers: model < compile < engine < core, \
         model < db < core.\n{}",
        report.join("\n")
    );
    let stale: Vec<String> = EXCEPTIONS
        .iter()
        .filter(|(f, t, _)| !used.contains(&(*f, *t)))
        .map(|(f, t, _)| format!("  ({f:?}, {t:?})"))
        .collect();
    assert!(
        stale.is_empty(),
        "exceptions no longer needed: delete them from EXCEPTIONS in src/layering_tests.rs\n{}",
        stale.join("\n")
    );
}

#[test]
fn the_scan_sees_paths_use_trees_and_super() {
    let src = r#"
        use crate::db;
        use crate::{model::StepId, dag::{Dag, self}, cache as c};
        fn f() { crate::errln!("x"); let _ = super::super::planner::plan(); }
        // crate::comment::ignored
        fn g() { $crate::term::x(); "crate::string::ignored"; }
        pub(crate) fn h() {}
        #[cfg(test)]
        mod tests { use crate::gc; }
        fn i() { other::crate_like::x(); crate::{a, b::*}; }
        mod inner { fn j() { super::super::super::gc::run(); } }
        fn k() { super::super::cost::x(); }
    "#;
    let module = vec!["db".to_string(), "runs".to_string()];
    let got: Vec<(String, usize)> = references(src, &module, &BTreeSet::new())
        .into_iter()
        .map(|(p, l)| (p.join("::"), l))
        .collect();
    let want = [
        ("db", 2),
        ("model::StepId", 3),
        ("dag::Dag", 3),
        ("dag", 3),
        ("cache", 3),
        ("errln", 4),
        ("planner::plan", 4),
        ("term::x", 6),
        ("a", 10),
        ("b", 10),
        ("gc::run", 11),
        ("cost::x", 12),
    ];
    let want: Vec<(String, usize)> = want.iter().map(|(p, l)| (p.to_string(), *l)).collect();
    assert_eq!(got, want);

    let lib = "pub use dag::Dag;\nmod x;\nfn f() -> dispatch::ErrorPhase { todo!() }\n";
    let modules: BTreeSet<String> = ["dag", "dispatch"].iter().map(|s| s.to_string()).collect();
    let got: Vec<String> = references(lib, &[], &modules)
        .into_iter()
        .map(|(p, _)| p.join("::"))
        .collect();
    assert_eq!(got, ["dispatch::ErrorPhase"]);
}
