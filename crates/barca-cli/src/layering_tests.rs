//! The module graph inside this crate (#1056, track E). Cargo checks nothing between the modules
//! of one crate, so this test does: every module has a row in [`LAYERS`], and non-test code may
//! name another module (`crate::<module>`) only when that module is in a row above its own.
//! Because a row never uses itself or a row below, the modules cannot form a dependency cycle.
//!
//! ```text
//! leaf  <  output  <  args  <  input  <  route  <  request  <  print  <  client | commands  <  dispatch  <  main
//! ```
//!
//! What the order holds in place:
//!
//! - `client` (a command sent to a server) and `commands` (a command run on this machine) are
//!   the two backends. They share a row, so neither can use the other: what both need (the
//!   request types, the printers) is in a row above them, and what needs both (`doctor`, the
//!   choice between them) is in `dispatch`, below them.
//! - `error` is a leaf: it knows neither the argument schema nor the output rule, so `args`,
//!   `output` and `error` cannot form the cycle they once did.
//! - Only `dispatch` uses both backends, so there is one place where a command's destination
//!   is decided.
//!
//! A failure is fixed by moving the item down (a helper or a type to a module above both of its
//! users in the table) or by passing it in as a value. There is no allowlist: the table has no
//! exception today, and one is not to be added. `docs/engineering/cli-layering.md` has the table
//! in prose.
//!
//! What counts as an edge: a `crate::<module>` path (or `super::<module>`, which is the same
//! thing in a top-level module file) outside comments, string literals and `#[cfg(test)]` items,
//! in `src/<module>.rs` or `src/<module>/`. Files named `tests.rs` or `*_tests.rs` are test code.
//! The scanner is the one in `crates/barca-compile/src/layering_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Every module, top row first. A module may use the modules of the rows above its own.
const LAYERS: &[&[&str]] = &[
    // Vocabulary and leaves: bounded output, the manual, the error envelope.
    &["bounded", "docs", "error"],
    // The output rule (it ends the process with a usage error), the lazy runtime.
    &["output", "runtime"],
    // The argument schema.
    &["args"],
    // Checks of single arguments, the project root, projections of the parsed command.
    &["input"],
    // Where each command can run: the table.
    &["route"],
    // A command as a request, checked once.
    &["request"],
    // What a command prints, wherever it ran.
    &["print"],
    // The two backends: a server, this machine.
    &["client", "commands"],
    // The one place that picks between them.
    &["dispatch"],
    &["main"],
    // Test-only modules (`#[cfg(test)] mod x;` in main.rs): they may read anything.
    &["contract", "snapshot"],
];

/// `source` with comments and the contents of string and char literals removed, so that a path
/// in a doc comment or a message is not read as code. Line structure is kept.
fn code_only(source: &str) -> String {
    let s: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let starts = |i: usize, pat: &str| {
        pat.chars()
            .enumerate()
            .all(|(k, c)| s.get(i + k) == Some(&c))
    };
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = 0;
    while i < s.len() {
        if starts(i, "//") {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if starts(i, "/*") {
            let mut depth = 0;
            loop {
                if starts(i, "/*") {
                    depth += 1;
                    i += 2;
                } else if starts(i, "*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    if s.get(i) == Some(&'\n') {
                        out.push('\n');
                    }
                    i += 1;
                }
                if depth == 0 || i >= s.len() {
                    break;
                }
            }
        } else if (s[i] == 'r' || starts(i, "br")) && (i == 0 || !ident(s[i - 1])) && {
            let mut j = i + if s[i] == 'b' { 2 } else { 1 };
            while s.get(j) == Some(&'#') {
                j += 1;
            }
            s.get(j) == Some(&'"')
        } {
            // A raw string: r"...", r#"..."#, br##"..."##.
            let mut j = i + if s[i] == 'b' { 2 } else { 1 };
            let mut hashes = 0;
            while s[j] == '#' {
                hashes += 1;
                j += 1;
            }
            j += 1;
            loop {
                if j >= s.len() {
                    break;
                }
                if s[j] == '"' && (1..=hashes).all(|k| s.get(j + k) == Some(&'#')) {
                    j += 1 + hashes;
                    break;
                }
                if s[j] == '\n' {
                    out.push('\n');
                }
                j += 1;
            }
            out.push_str("\"\"");
            i = j;
        } else if s[i] == '"' {
            i += 1;
            while i < s.len() && s[i] != '"' {
                if s[i] == '\\' {
                    i += 1;
                }
                if s.get(i) == Some(&'\n') {
                    out.push('\n');
                }
                i += 1;
            }
            i += 1;
            out.push_str("\"\"");
        } else if s[i] == '\'' {
            // A char literal ('a', '\n', '\'', '{'); anything else is a lifetime.
            let end = match (s.get(i + 1), s.get(i + 2)) {
                (Some('\\'), _) => (i + 3..s.len().min(i + 12)).find(|&j| s[j] == '\''),
                (Some(_), Some('\'')) => Some(i + 2),
                _ => None,
            };
            match end {
                Some(end) => {
                    out.push_str("' '");
                    i = end + 1;
                }
                None => {
                    out.push('\'');
                    i += 1;
                }
            }
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// `code` (already through [`code_only`]) without its `#[cfg(test)]` items: the attribute, any
/// attributes after it, and the item up to its closing brace or its `;`.
fn without_test_items(code: &str) -> String {
    const ATTR: &str = "#[cfg(test)]";
    let mut out = String::new();
    let mut rest = code;
    while let Some(at) = rest.find(ATTR) {
        out.push_str(&rest[..at]);
        let s: Vec<char> = rest[at + ATTR.len()..].chars().collect();
        let mut i = 0;
        // Further attributes on the same item.
        loop {
            while i < s.len() && s[i].is_whitespace() {
                i += 1;
            }
            if s.get(i) == Some(&'#') && s.get(i + 1) == Some(&'[') {
                let mut depth = 0;
                while i < s.len() {
                    match s[i] {
                        '[' => depth += 1,
                        ']' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        let mut depth = 0;
        while i < s.len() {
            let c = s[i];
            i += 1;
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                // A field, a statement or a `mod tests;`.
                ';' | ',' if depth == 0 => break,
                _ => {}
            }
        }
        let consumed: usize = s[..i].iter().map(|c| c.len_utf8()).sum();
        rest = &rest[at + ATTR.len() + consumed..];
    }
    out.push_str(rest);
    out
}

/// The paths of a use tree that starts at `s[*i]`: `a::b` is `["a::b"]`, `{a::B, c::{D, E}}` is
/// `["a::B", "c::D", "c::E"]`. Stops at the first character that is not part of one.
fn use_tree(s: &[char], i: &mut usize, prefix: &str, out: &mut Vec<String>) {
    let skip = |i: &mut usize| {
        while s.get(*i).is_some_and(|c| c.is_whitespace()) {
            *i += 1;
        }
    };
    let mut path = prefix.to_string();
    loop {
        skip(i);
        if s.get(*i) == Some(&'{') {
            *i += 1;
            loop {
                skip(i);
                match s.get(*i) {
                    Some('}') => {
                        *i += 1;
                        return;
                    }
                    Some(',') => *i += 1,
                    Some(_) => {
                        let before = *i;
                        use_tree(s, i, &path, out);
                        if *i == before {
                            return; // not a use tree after all
                        }
                        // `B as Bee`: the alias is a local name, not a path.
                        skip(i);
                        if s.get(*i) == Some(&'a')
                            && s.get(*i + 1) == Some(&'s')
                            && s.get(*i + 2).is_some_and(|c| c.is_whitespace())
                        {
                            *i += 2;
                            skip(i);
                            while s.get(*i).is_some_and(|c| c.is_alphanumeric() || *c == '_') {
                                *i += 1;
                            }
                        }
                    }
                    None => return,
                }
            }
        }
        let start = *i;
        while s
            .get(*i)
            .is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '*')
        {
            *i += 1;
        }
        if *i == start {
            break;
        }
        let segment: String = s[start..*i].iter().collect();
        if !path.is_empty() {
            path.push_str("::");
        }
        path.push_str(&segment);
        skip(i);
        if s.get(*i) == Some(&':') && s.get(*i + 1) == Some(&':') {
            *i += 2;
        } else {
            break;
        }
    }
    if path != prefix {
        out.push(path);
    }
}

/// Every `crate::...` path in `code` (and `super::...` when `top_level`), without the root.
fn crate_paths(code: &str, top_level: bool) -> Vec<String> {
    let s: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let roots: &[&str] = if top_level {
        &["crate::", "super::"]
    } else {
        &["crate::"]
    };
    let mut i = 0;
    while i < s.len() {
        let boundary =
            i == 0 || !(s[i - 1].is_alphanumeric() || s[i - 1] == '_' || s[i - 1] == '$');
        let root = roots.iter().find(|root| {
            boundary
                && root
                    .chars()
                    .enumerate()
                    .all(|(k, c)| s.get(i + k) == Some(&c))
        });
        match root {
            Some(root) => {
                i += root.len();
                use_tree(&s, &mut i, "", &mut out);
            }
            None => i += 1,
        }
    }
    out
}

/// The non-test source files of each module: `src/<module>.rs` and everything under
/// `src/<module>/`. The bool is whether the file is the module's own top-level file.
fn module_files(src: &Path) -> BTreeMap<String, Vec<(std::path::PathBuf, bool)>> {
    fn is_test(path: &Path) -> bool {
        let stem = path.file_stem().unwrap().to_string_lossy();
        stem == "tests" || stem.ends_with("_tests")
    }
    fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, bool)>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") && !is_test(&path) {
                out.push((path, false));
            }
        }
    }
    let mut modules: BTreeMap<String, Vec<(std::path::PathBuf, bool)>> = BTreeMap::new();
    for entry in std::fs::read_dir(src).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            let mut files = Vec::new();
            walk(&path, &mut files);
            // `src/<module>/mod.rs` is the module's own file.
            for (file, top) in &mut files {
                *top = file.parent() == Some(path.as_path()) && file.ends_with("mod.rs");
            }
            if !files.is_empty() {
                modules.entry(name).or_default().extend(files);
            }
        } else if path.extension().is_some_and(|e| e == "rs") && !is_test(&path) {
            modules.entry(name).or_default().push((path, true));
        }
    }
    modules
}

/// `(from, to)` to the symbols `from` names in `to`.
type Edges = BTreeMap<(String, String), BTreeSet<String>>;

/// The crate's modules, and the edges between them in non-test code.
fn edges(src: &Path) -> (BTreeSet<String>, Edges) {
    let files = module_files(src);
    let modules: BTreeSet<String> = files.keys().cloned().collect();
    let mut edges = Edges::new();
    for (module, paths) in &files {
        for (path, top_level) in paths {
            let text = std::fs::read_to_string(path).unwrap();
            let code = without_test_items(&code_only(&text));
            for used in crate_paths(&code, *top_level) {
                let (to, symbol) = used.split_once("::").unwrap_or((used.as_str(), "self"));
                if to != module && modules.contains(to) {
                    edges
                        .entry((module.clone(), to.to_string()))
                        .or_default()
                        .insert(symbol.to_string());
                }
            }
        }
    }
    (modules, edges)
}

fn row_of(module: &str) -> Option<usize> {
    LAYERS.iter().position(|modules| modules.contains(&module))
}

/// Why `from` may not use `to`, when it may not.
fn broken_rule(from: &str, to: &str) -> Option<String> {
    let (from_row, to_row) = (row_of(from)?, row_of(to)?);
    // The test-only row may read anything, itself included.
    if from_row == LAYERS.len() - 1 {
        return None;
    }
    if to_row == from_row {
        return Some(format!("both are in row {from_row} of LAYERS"));
    }
    if to_row > from_row {
        return Some(format!(
            "{to} (row {to_row}) is below {from} (row {from_row})"
        ));
    }
    None
}

#[test]
fn modules_use_only_the_rows_above_them() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (modules, edges) = edges(&src);
    let mut problems = Vec::new();

    for module in &modules {
        if row_of(module).is_none() {
            problems.push(format!(
                "  module `{module}` has no row in LAYERS: add it to the row below everything it uses"
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for module in LAYERS.iter().flat_map(|modules| modules.iter()) {
        if !modules.contains(*module) {
            problems.push(format!(
                "  LAYERS names `{module}`, which is not a module of this crate"
            ));
        }
        if !seen.insert(*module) {
            problems.push(format!("  LAYERS names `{module}` twice"));
        }
    }
    for ((from, to), symbols) in &edges {
        if let Some(why) = broken_rule(from, to) {
            let symbols: Vec<&str> = symbols.iter().map(String::as_str).collect();
            problems.push(format!("  {from} -> {to} ({}): {why}", symbols.join(", ")));
        }
    }

    assert!(
        problems.is_empty(),
        "barca-cli's module layering (crates/barca-cli/src/layering_tests.rs, \
         docs/engineering/cli-layering.md):\n{}\nMove the item to a module above both of its \
         users in LAYERS, or pass it in as a value.",
        problems.join("\n")
    );
}

/// The last row is what it says: every module in it is declared `#[cfg(test)]` in main.rs, and
/// no other module is.
#[test]
fn the_test_only_row_is_test_only() {
    let main =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap();
    let mut test_only = BTreeSet::new();
    let mut previous = "";
    for line in main.lines() {
        if previous.trim() == "#[cfg(test)]"
            && let Some(name) = line.trim().strip_prefix("mod ")
        {
            let name = name.trim_end_matches(';');
            let stem_is_test = name == "tests" || name.ends_with("_tests");
            if !stem_is_test {
                test_only.insert(name.to_string());
            }
        }
        if !line.trim().starts_with("#[path") {
            previous = line;
        }
    }
    let row: BTreeSet<String> = LAYERS
        .last()
        .unwrap()
        .iter()
        .map(|m| m.to_string())
        .collect();
    assert_eq!(test_only, row);
}

#[test]
fn the_scan_reads_code_and_nothing_else() {
    let source = r##"
//! Docs name [`crate::docs_only::Thing`].
use crate::{alpha::A, beta::{self, B as Bee}, gamma::*};
use super::delta::D;
fn f<'a>(x: &'a str) -> char {
    let _ = "a string naming crate::in_string::X and a } brace";
    let _ = r#"raw crate::in_raw::X"#;
    /* a block comment naming crate::in_block::X */
    crate::epsilon::call(x);
    $crate::not_ours::x!();
    '{'
}
#[cfg(test)]
#[allow(dead_code)]
fn only_in_tests() { crate::test_only::helper(); }
struct S {
    #[cfg(test)]
    probe: crate::test_field::Probe,
    kept: crate::zeta::Kept,
}
#[cfg(test)]
mod tests;
fn after() { crate::eta::last() }
"##;
    let code = without_test_items(&code_only(source));
    let mut paths = crate_paths(&code, true);
    paths.sort();
    assert_eq!(
        paths,
        [
            "alpha::A",
            "beta::B",
            "beta::self",
            "delta::D",
            "epsilon::call",
            "eta::last",
            "gamma::*",
            "zeta::Kept",
        ]
    );
    // `super::` inside a submodule file is that module's own parent, not the crate root.
    assert_eq!(
        crate_paths("use super::delta::D;", false),
        Vec::<String>::new()
    );
}

#[test]
fn a_wrong_way_edge_is_named() {
    // Up the table and within a row: the two backends, and the old cycle.
    assert!(broken_rule("client", "commands").is_some());
    assert!(broken_rule("commands", "client").is_some());
    assert!(broken_rule("error", "args").is_some());
    assert!(broken_rule("error", "output").is_some());
    assert!(broken_rule("request", "client").is_some());
    assert!(broken_rule("print", "commands").is_some());
    assert!(broken_rule("route", "dispatch").is_some());
    // Down the table.
    assert!(broken_rule("output", "error").is_none());
    assert!(broken_rule("args", "output").is_none());
    assert!(broken_rule("client", "request").is_none());
    assert!(broken_rule("commands", "print").is_none());
    assert!(broken_rule("dispatch", "client").is_none());
    assert!(broken_rule("dispatch", "commands").is_none());
}

/// A command that kept a connection to a metadata DB closes it as it ends, which folds the
/// log into the main file (`barca_core::db::end_of_command`). `crate::exit` does that, so it
/// is the one place that ends the process: the `logs -f` paths once called `term::exit`
/// themselves and left the log unfolded.
#[test]
fn every_exit_goes_through_the_one_that_closes_the_database() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut exits = Vec::new();
    for (path, _) in module_files(&src).into_values().flatten() {
        let code = without_test_items(&code_only(&std::fs::read_to_string(&path).unwrap()));
        for call in ["term::exit(", "process::exit("] {
            for _ in code.matches(call) {
                exits.push(format!(
                    "{} {call}",
                    path.strip_prefix(&src).unwrap().display()
                ));
            }
        }
    }
    assert_eq!(
        exits,
        ["main.rs term::exit("],
        "call `crate::exit(code)` instead: it closes the metadata DB first"
    );
}
