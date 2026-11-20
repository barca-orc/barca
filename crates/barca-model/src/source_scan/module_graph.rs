//! The module graph of one crate, read from its source: which top-level module names which
//! (#1056; `docs/engineering/crate-split-2.md`). Cargo orders crates and checks nothing between
//! the modules of one crate, so each crate that has an order keeps it with a test built on this:
//! rows ([`rows_problems`]: a module names only the rows before its own, so the modules cannot
//! form a cycle) or, where the modules are not ordered yet, the size of the largest cycle
//! ([`largest_cycle`]) as a number that may only shrink.
//!
//! What the scan reads: `crate::m::item`, `use crate::m::{a, b}`, `use crate::{m, n::item}`,
//! and, in a file that imports a module by name (`use crate::m;`), its `m::item` paths.
//! `super::` and `self::` paths stay inside one top-level module and are not read. An alias
//! (`use crate::m as x`) is recorded as an edge without the items used through it. Test module
//! files and `#[cfg(test)]` items are skipped.

use super::{Tok, is_cfg_test, lex, non_test_sources, skip_cfg_test_item};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// (from, to) -> the items of `to` that `from` names.
pub type Edges = BTreeMap<(String, String), BTreeSet<String>>;

/// The modules `lib.rs` declares outside `#[cfg(test)]`.
pub fn declared_modules(lib: &str) -> BTreeSet<String> {
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
pub fn references(src: &str, modules: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
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
pub fn module_of(rel: &str) -> &str {
    let first = rel.split('/').next().unwrap_or(rel);
    first.strip_suffix(".rs").unwrap_or(first)
}

pub fn scan(src_root: &Path) -> (BTreeSet<String>, Edges) {
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

pub fn line_of(from: &str, to: &str, items: &BTreeSet<String>) -> String {
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    format!("{from} -> {to}: {}", items.join(", "))
}

/// The size of the largest strongly connected component (Tarjan's algorithm).
pub fn largest_cycle(modules: &BTreeSet<String>, edges: &Edges) -> Vec<String> {
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

/// What breaks the order `rows` gives the modules of the crate at `src_root`, first row first:
/// a module may name the modules of the rows before its own, never its own row or a later one.
/// Every module `lib.rs` declares outside `#[cfg(test)]` must be in exactly one row, and every
/// name in a row must be such a module. Empty when the order holds.
pub fn rows_problems(src_root: &Path, rows: &[&[&str]]) -> Vec<String> {
    let (modules, edges) = scan(src_root);
    let row_of = |module: &str| rows.iter().position(|row| row.contains(&module));
    let mut problems = Vec::new();
    for module in &modules {
        let count = rows.iter().filter(|r| r.contains(&module.as_str())).count();
        if count != 1 {
            problems.push(format!("  `mod {module}` is in {count} rows: give it one"));
        }
    }
    for module in rows.iter().flat_map(|row| row.iter()) {
        if !modules.contains(*module) {
            problems.push(format!(
                "  the rows name `{module}`, which lib.rs does not declare"
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
    problems
}

/// What is wrong when the largest group of modules that all reach each other (through `crate::`
/// paths in non-test code) is not `expected` modules: `None` when it is. `expected` is a
/// ratchet: lower it when a cut shrinks the cycle, cut the new edge when a change grows it. One
/// module on its own is a cycle of 1.
pub fn cycle_problem(src_root: &Path, expected: usize) -> Option<String> {
    let (modules, edges) = scan(src_root);
    let cycle = largest_cycle(&modules, &edges);
    (cycle.len() != expected).then(|| {
        format!(
            "the largest dependency cycle has {} modules, the constant says {expected} ({}): \
             set the constant if it shrank, cut the new edge if it grew: {}",
            cycle.len(),
            if cycle.len() < expected {
                "it shrank"
            } else {
                "it grew"
            },
            cycle.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
