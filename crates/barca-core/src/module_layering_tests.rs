//! The layers inside `barca-core` (#1056 track C; `docs/engineering/layering.md` has the table
//! and the reasons). A module may name a module of its own layer or of a layer below it;
//! `engine` and `analysis` sit side by side and never name each other. Cargo does not check
//! this inside one crate, so this test does: it reads every `crate::<module>` path in the
//! crate's non-test source and compares the edges with [`LAYERS`].
//!
//! The edges that point the wrong way today are listed in `module_layering_allowlist.txt`, one
//! per line with the items that cause it. The list may only shrink: a new wrong-way edge fails,
//! a new item on a listed edge fails, and a line (or an item on it) that the source no longer
//! has fails until it is deleted. Moving the item down a layer, or passing it in as a value, is
//! the fix for a failure here; adding a line is not. [`LARGEST_CYCLE`] is the same kind of
//! ratchet for the largest group of modules that all reach each other.
//!
//! What the scan reads: `crate::m::item`, `use crate::m::{a, b}`, `use crate::{m, n::item}`,
//! and, in a file that imports a module by name (`use crate::m;`), its `m::item` paths.
//! `super::` and `self::` paths stay inside one top-level module and are not read. An alias
//! (`use crate::m as x`) is recorded as an edge without the items used through it.

use crate::source_scan::{Tok, is_cfg_test, lex, non_test_sources, skip_cfg_test_item};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Each layer, its rank, and its modules. A module may name a module of a lower rank or of its
/// own layer. Two layers of one rank (`engine`, `analysis`) may not name each other. Every
/// `mod` in `lib.rs` is listed once.
const LAYERS: &[(&str, u8, &[&str])] = &[
    // Plain data and helpers with no module of this crate below them.
    ("leaf", 0, &["db", "envelope", "http", "results", "text"]),
    (
        "storage",
        1,
        &[
            "backup",
            "gc",
            "ledger",
            "pauses",
            "pins",
            "run_queue",
            "store_inspect",
            "store_sync",
            "transfer",
            "verified_copies",
        ],
    ),
    (
        "decisions",
        2,
        &[
            "cache",
            "changes",
            "freshness",
            "plan_reasons",
            "predict",
            "recover",
            "schedule",
            "sizing",
        ],
    ),
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
        ],
    ),
];

/// The order inside the analysis layer (#1056 C3), first row first: an analysis module may
/// name the analysis modules of the rows before its own, and none of its own row or a later
/// one. So the read models cannot form a cycle. Every analysis module of [`LAYERS`] is in one
/// row.
const ANALYSIS_ROWS: &[&[&str]] = &[
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

/// The order inside the storage layer, top row first: a storage module may name the storage
/// modules of the rows above its own, never its own row or a row below. So the storage layer
/// has no cycle of its own, and no entry in the allowlist: an edge this order forbids fails.
const STORAGE_ROWS: &[&[&str]] = &[
    // What the others are built on: the artifact layout and the transfer helper, and the
    // tables one feature owns.
    &["pauses", "run_queue", "transfer", "verified_copies"],
    // The metadata backups (and the launcher of `barca._state`), and a run's link to the store.
    &["backup", "store_sync"],
    // What the store and this machine hold at a path; what `.barca/` holds and may drop.
    &["gc", "store_inspect"],
    // The server's ledger, both halves, and the reader a cache decision asks.
    &["ledger"],
    // Pinned inputs, read through that reader.
    &["pins"],
];

/// The number of modules in the largest strongly connected component of the module graph
/// (modules that all reach each other through `crate::` paths in non-test code). 38 before
/// the cuts of #1056's C1, 29 after them, 19 with the storage layer out of it (C4), 16 with the prediction in the
/// decisions layer (C2).
const LARGEST_CYCLE: usize = 16;

const ALLOWLIST: &str = include_str!("module_layering_allowlist.txt");

/// (from, to) -> the items of `to` that `from` names.
type Edges = BTreeMap<(String, String), BTreeSet<String>>;

fn layer_of(module: &str) -> Option<(&'static str, u8)> {
    LAYERS
        .iter()
        .find(|(_, _, modules)| modules.contains(&module))
        .map(|(name, rank, _)| (*name, *rank))
}

fn analysis_row(module: &str) -> Option<usize> {
    ANALYSIS_ROWS.iter().position(|row| row.contains(&module))
}

/// Whether `from` may name `to`.
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

/// The modules `lib.rs` declares outside `#[cfg(test)]`.
fn declared_modules(lib: &str) -> BTreeSet<String> {
    let toks = lex(lib);
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < toks.len() {
        if is_cfg_test(&toks, i) {
            i = skip_cfg_test_item(&toks, i).0;
            continue;
        }
        if let (Tok::Ident(kw), Some(Tok::Ident(name)), Some(Tok::Punct(';'))) =
            (&toks[i], toks.get(i + 1), toks.get(i + 2))
            && kw == "mod"
        {
            out.insert(name.clone());
        }
        i += 1;
    }
    out
}

fn is_path_sep(toks: &[Tok], i: usize) -> bool {
    toks.get(i) == Some(&Tok::Punct(':')) && toks.get(i + 1) == Some(&Tok::Punct(':'))
}

/// A path as the allowlist names it: its segments up to and including the first that starts
/// with an uppercase letter (a type or constant; what follows is a variant or a method).
fn item_name(segments: &[String]) -> String {
    let mut out = Vec::new();
    for segment in segments {
        out.push(segment.as_str());
        if segment.starts_with(char::is_uppercase) {
            break;
        }
    }
    out.join("::")
}

/// The paths of the use tree (or expression path) whose first segment is at `i`, each as its
/// segments, and the index after the tree. `a::b`, `a::{b, c::d}`, `a::*`, `a as x`.
fn tree(toks: &[Tok], mut i: usize, prefix: &[String], out: &mut Vec<Vec<String>>) -> usize {
    let mut path = prefix.to_vec();
    loop {
        match toks.get(i) {
            Some(Tok::Ident(name)) => {
                path.push(name.clone());
                i += 1;
            }
            Some(Tok::Punct('*')) => {
                path.push("*".into());
                out.push(path);
                return i + 1;
            }
            Some(Tok::Punct('{')) => {
                i += 1;
                while i < toks.len() && toks[i] != Tok::Punct('}') {
                    if toks[i] == Tok::Punct(',') {
                        i += 1;
                        continue;
                    }
                    let next = tree(toks, i, &path, out);
                    // Something that is not a use tree: step over it.
                    i = if next == i { i + 1 } else { next };
                }
                return i + 1;
            }
            _ => {
                out.push(path);
                return i;
            }
        }
        if is_path_sep(toks, i) {
            i += 2;
            continue;
        }
        if let (Some(Tok::Ident(kw)), Some(Tok::Ident(alias))) = (toks.get(i), toks.get(i + 1))
            && kw == "as"
        {
            path.push(format!("(as {alias})"));
            i += 2;
        }
        out.push(path);
        return i;
    }
}

/// The modules of `modules` that `src` names through `crate::` paths, with the items it names
/// in each. `#[cfg(test)]` items are skipped.
fn references(src: &str, modules: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
    let all = lex(src);
    // Drop test items first, so every later index is into non-test code.
    let mut toks = Vec::new();
    let mut i = 0;
    while i < all.len() {
        if is_cfg_test(&all, i) {
            i = skip_cfg_test_item(&all, i).0;
            continue;
        }
        toks.push(match &all[i] {
            Tok::Str(s, l) => Tok::Str(s.clone(), *l),
            Tok::Ident(s) => Tok::Ident(s.clone()),
            Tok::Punct(c) => Tok::Punct(*c),
        });
        i += 1;
    }

    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Modules imported by name (`use crate::m;`, `use crate::m::{self, ..}`): their `m::item`
    // paths are read in the second pass.
    let mut by_name = BTreeSet::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i] == Tok::Ident("crate".into()) && is_path_sep(&toks, i + 1) {
            let mut paths = Vec::new();
            i = tree(&toks, i + 3, &[], &mut paths);
            for path in paths {
                let Some(module) = path.first().filter(|m| modules.contains(*m)) else {
                    continue;
                };
                let items = out.entry(module.clone()).or_default();
                // `m::{self}` is the module itself, and `m::sub::{self}` is `m::sub`.
                let rest = match &path[1..] {
                    [rest @ .., last] if last == "self" => rest,
                    rest => rest,
                };
                if rest.is_empty() {
                    by_name.insert(module.clone());
                } else {
                    items.insert(item_name(rest));
                }
            }
            continue;
        }
        i += 1;
    }
    for i in 0..toks.len() {
        let Tok::Ident(name) = &toks[i] else { continue };
        if !by_name.contains(name) || !is_path_sep(&toks, i + 1) {
            continue;
        }
        // `crate::m::item` was read above; `x.m::<T>()` and `other::m::item` are not this module.
        if i > 0 && matches!(toks[i - 1], Tok::Punct(':' | '.')) {
            continue;
        }
        let mut paths = Vec::new();
        tree(&toks, i + 3, &[], &mut paths);
        for path in paths.iter().filter(|p| !p.is_empty()) {
            out.entry(name.clone()).or_default().insert(item_name(path));
        }
    }
    out
}

/// The top-level module a source file (path under `src/`) belongs to.
fn module_of(rel: &str) -> &str {
    let first = rel.split('/').next().unwrap_or(rel);
    first.strip_suffix(".rs").unwrap_or(first)
}

fn scan(src_root: &Path) -> (BTreeSet<String>, Edges) {
    let lib = std::fs::read_to_string(src_root.join("lib.rs")).unwrap();
    let modules = declared_modules(&lib);
    let mut edges = Edges::new();
    for (rel, src) in non_test_sources(src_root) {
        let from = module_of(&rel);
        if !modules.contains(from) {
            continue;
        }
        for (to, items) in references(&src, &modules) {
            if to != from {
                edges
                    .entry((from.to_string(), to))
                    .or_default()
                    .extend(items);
            }
        }
    }
    (modules, edges)
}

fn line_of(from: &str, to: &str, items: &BTreeSet<String>) -> String {
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    format!("{from} -> {to}: {}", items.join(", "))
}

fn parse_allowlist(text: &str) -> Edges {
    let mut out = Edges::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = line.split_once(": ").and_then(|(edge, items)| {
            let (from, to) = edge.split_once(" -> ")?;
            Some((from.trim(), to.trim(), items))
        });
        let Some((from, to, items)) = parsed else {
            panic!("module_layering_allowlist.txt: `{line}` is not `from -> to: item, item`");
        };
        let items = items.split(", ").map(|s| s.trim().to_string()).collect();
        let previous = out.insert((from.to_string(), to.to_string()), items);
        assert!(
            previous.is_none(),
            "module_layering_allowlist.txt: {from} -> {to} is listed twice"
        );
    }
    out
}

/// The size of the largest strongly connected component (Tarjan's algorithm).
fn largest_cycle(modules: &BTreeSet<String>, edges: &Edges) -> Vec<String> {
    struct State<'a> {
        next: BTreeMap<&'a str, Vec<&'a str>>,
        index: BTreeMap<&'a str, usize>,
        low: BTreeMap<&'a str, usize>,
        stack: Vec<&'a str>,
        on_stack: BTreeSet<&'a str>,
        best: Vec<String>,
    }
    fn visit<'a>(v: &'a str, s: &mut State<'a>) {
        let n = s.index.len();
        s.index.insert(v, n);
        s.low.insert(v, n);
        s.stack.push(v);
        s.on_stack.insert(v);
        for w in s.next.get(v).cloned().unwrap_or_default() {
            if !s.index.contains_key(w) {
                visit(w, s);
                let low = s.low[v].min(s.low[w]);
                s.low.insert(v, low);
            } else if s.on_stack.contains(w) {
                let low = s.low[v].min(s.index[w]);
                s.low.insert(v, low);
            }
        }
        if s.low[v] == s.index[v] {
            let mut component = Vec::new();
            while let Some(w) = s.stack.pop() {
                s.on_stack.remove(w);
                component.push(w.to_string());
                if w == v {
                    break;
                }
            }
            if component.len() > s.best.len() {
                component.sort();
                s.best = component;
            }
        }
    }
    let mut state = State {
        next: BTreeMap::new(),
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        best: Vec::new(),
    };
    for (from, to) in edges.keys() {
        state.next.entry(from).or_default().push(to);
    }
    for module in modules {
        if !state.index.contains_key(module.as_str()) {
            visit(module, &mut state);
        }
    }
    state.best
}

#[test]
fn modules_name_only_their_own_layer_and_the_layers_below() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (modules, edges) = scan(&src_root);
    let allowlist = parse_allowlist(ALLOWLIST);
    let mut problems = Vec::new();

    for module in &modules {
        if layer_of(module).is_none() {
            problems.push(format!(
                "  `mod {module}` has no layer: add it to LAYERS (and to the table in \
                 docs/engineering/layering.md)"
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for (layer, _, listed) in LAYERS {
        for module in *listed {
            if !modules.contains(*module) {
                problems.push(format!(
                    "  LAYERS ({layer}) names `{module}`, which lib.rs does not declare"
                ));
            }
            if !seen.insert(*module) {
                problems.push(format!("  LAYERS names `{module}` twice"));
            }
        }
    }

    let analysis = LAYERS
        .iter()
        .find(|(layer, _, _)| *layer == "analysis")
        .map(|(_, _, listed)| *listed)
        .unwrap_or_default();
    for module in analysis {
        if analysis_row(module).is_none() {
            problems.push(format!(
                "  analysis module `{module}` has no row: add it to ANALYSIS_ROWS"
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for module in ANALYSIS_ROWS.iter().flat_map(|row| row.iter()) {
        if !analysis.contains(module) {
            problems.push(format!(
                "  ANALYSIS_ROWS names `{module}`, which is not an analysis module of LAYERS"
            ));
        }
        if !seen.insert(*module) {
            problems.push(format!("  ANALYSIS_ROWS names `{module}` twice"));
        }
    }

    for ((from, to), items) in &edges {
        if allowed(from, to) || layer_of(from).is_none() || layer_of(to).is_none() {
            continue;
        }
        let (from_layer, to_layer) = (layer_of(from).unwrap().0, layer_of(to).unwrap().0);
        match allowlist.get(&(from.clone(), to.clone())) {
            None => problems.push(format!(
                "  new edge, {from_layer} names {to_layer}: {}",
                line_of(from, to, items)
            )),
            Some(listed) if listed != items => {
                let new: Vec<&str> = items.difference(listed).map(String::as_str).collect();
                let gone: Vec<&str> = listed.difference(items).map(String::as_str).collect();
                let mut what = Vec::new();
                if !new.is_empty() {
                    what.push(format!("new items {}", new.join(", ")));
                }
                if !gone.is_empty() {
                    what.push(format!(
                        "items no longer used {} (shorten the line to `{}`)",
                        gone.join(", "),
                        line_of(from, to, items)
                    ));
                }
                problems.push(format!("  {from} -> {to}: {}", what.join("; ")));
            }
            Some(_) => {}
        }
    }
    for (from, to) in allowlist.keys() {
        let key = (from.clone(), to.clone());
        if allowed(from, to) {
            problems.push(format!(
                "  the allowlist has {from} -> {to}, which LAYERS allows: delete the line"
            ));
        } else if !edges.contains_key(&key) {
            problems.push(format!(
                "  the allowlist has {from} -> {to}, which the source no longer has: delete \
                 the line"
            ));
        }
    }

    let cycle = largest_cycle(&modules, &edges);
    if cycle.len() != LARGEST_CYCLE {
        problems.push(format!(
            "  the largest dependency cycle has {} modules, LARGEST_CYCLE says {LARGEST_CYCLE} \
             ({}): set the constant if it shrank, cut the new edge if it grew: {}",
            cycle.len(),
            if cycle.len() < LARGEST_CYCLE {
                "it shrank"
            } else {
                "it grew"
            },
            cycle.join(", ")
        ));
    }

    assert!(
        problems.is_empty(),
        "module layers of barca-core (#1056, docs/engineering/layering.md; the allowlist is \
         crates/barca-core/src/module_layering_allowlist.txt and may only shrink):\n{}",
        problems.join("\n")
    );
}

#[test]
fn storage_modules_name_only_the_storage_rows_above_their_own() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (_, edges) = scan(&src_root);
    let storage = LAYERS
        .iter()
        .find(|(name, ..)| *name == "storage")
        .unwrap()
        .2;
    let row_of = |module: &str| STORAGE_ROWS.iter().position(|row| row.contains(&module));
    let mut problems = Vec::new();
    for module in storage {
        let rows = STORAGE_ROWS.iter().filter(|r| r.contains(module)).count();
        if rows != 1 {
            problems.push(format!(
                "  storage module `{module}` is in {rows} rows of STORAGE_ROWS: give it one"
            ));
        }
    }
    for module in STORAGE_ROWS.iter().flat_map(|row| row.iter()) {
        if !storage.contains(module) {
            problems.push(format!(
                "  STORAGE_ROWS names `{module}`, which is not a storage module in LAYERS"
            ));
        }
    }
    for ((from, to), items) in &edges {
        if let (Some(a), Some(b)) = (row_of(from), row_of(to))
            && b >= a
        {
            problems.push(format!(
                "  row {a} names row {b}: {}",
                line_of(from, to, items)
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the order inside barca-core's storage layer (STORAGE_ROWS; \
         docs/engineering/layering.md). Move the item to a row above both of its users, or \
         pass it in:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_scan_reads_paths_groups_and_named_modules() {
    let modules: BTreeSet<String> = ["gc", "report", "serve", "status", "cache"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let src = r#"
        use crate::gc::size::human;
        use crate::report::{self, span, RenderedOutput};
        use crate::{BarcaError, serve::run_queue::{self, QueueState}, status};
        fn f(policy: &crate::cache::CachePolicy) -> String {
            let _ = crate::cache::CachePolicy::default();
            let _ = status::NodeStatus::Fresh;
            let _ = report::kind_str(1);
            other::report::ignored();
            human(1)
        }
        #[cfg(test)]
        mod tests {
            use crate::serve::ServeHost;
        }
    "#;
    let found = references(src, &modules);
    let items = |m: &str| -> Vec<&str> { found[m].iter().map(String::as_str).collect() };
    assert_eq!(items("gc"), ["size::human"]);
    assert_eq!(items("report"), ["RenderedOutput", "kind_str", "span"]);
    assert_eq!(items("serve"), ["run_queue", "run_queue::QueueState"]);
    assert_eq!(items("status"), ["NodeStatus"]);
    assert_eq!(items("cache"), ["CachePolicy"]);
}

#[test]
fn the_rule_orders_the_layers() {
    assert!(allowed("execution", "cache"));
    assert!(allowed("cache", "freshness"));
    assert!(allowed("report", "health"));
    assert!(!allowed("cache", "execution"));
    assert!(!allowed("execution", "status"));
    assert!(!allowed("status", "execution"));
    assert!(!allowed("gc", "report"));
}

#[test]
fn the_rows_order_the_analysis_layer() {
    assert!(allowed("health", "views"));
    assert!(allowed("status", "health"));
    assert!(allowed("health", "rollup"));
    assert!(allowed("run_list", "run_explanation"));
    assert!(allowed("partition_grid", "key_query"));
    // A later row, and the same row.
    assert!(!allowed("rollup", "health"));
    assert!(!allowed("views", "schedule_standing"));
    assert!(!allowed("key_query", "partition_grid"));
    assert!(!allowed("health", "run_list"));
    // The layers below are open to every row.
    assert!(allowed("views", "cache"));
    assert!(!allowed("views", "report"));
}
