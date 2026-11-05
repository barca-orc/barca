//! The metadata write boundary (architecture-cleanup step 2b, #800 item 16): every
//! `INSERT`/`UPDATE`/`DELETE`/`REPLACE` of `.barca/metadata.db` is issued by an owner in `db/`
//! or by the history recorder in `persist/recording.rs`. Everything else calls a
//! named owner function and keeps its own transaction and eligibility rules. `db_schema.rs`
//! (migrations) and `sql/` (read-only) are outside the rule, and so is test code.

use crate::source_scan::{
    Tok, is_cfg_test, lex, rust_files, skip_cfg_test_item, test_module_files,
};
use std::path::Path;

/// Owners: a file here, or under a directory here, may write.
const OWNERS: &[&str] = &["db/", "persist/recording.rs", "db_schema.rs", "sql/"];

/// Files that still write outside an owner, each with why.
const ALLOWED: &[(&str, &str)] = &[];

/// What a scan of one file found: write statements in non-test code (line, start of the SQL)
/// and the `#[cfg(test)] mod name;` files it declares.
#[derive(Debug, Default)]
struct Scan {
    writes: Vec<(usize, String)>,
    test_modules: Vec<String>,
}

fn starts_a_write(text: &str) -> bool {
    let text = text.trim_start();
    ["INSERT", "UPDATE", "DELETE", "REPLACE"].iter().any(|kw| {
        text.strip_prefix(kw)
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    })
}

fn scan(src: &str) -> Scan {
    let toks = lex(src);
    let mut out = Scan::default();
    let mut i = 0;
    while i < toks.len() {
        if is_cfg_test(&toks, i) {
            let (next, name) = skip_cfg_test_item(&toks, i);
            out.test_modules.extend(name);
            i = next;
            continue;
        }
        if let Tok::Str(text, line) = &toks[i]
            && starts_a_write(text)
        {
            let sql: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            out.writes.push((*line, sql.chars().take(60).collect()));
        }
        i += 1;
    }
    out
}

#[test]
fn metadata_writes_go_through_an_owner() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    let mut test_files = Vec::new();
    let mut found = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        let scanned = scan(&std::fs::read_to_string(file).unwrap());
        for m in &scanned.test_modules {
            test_files.extend(test_module_files(&root, file, m));
        }
        found.push((rel, scanned.writes));
    }
    let mut offenders = Vec::new();
    let mut allowed_used = Vec::new();
    for (rel, writes) in found {
        if writes.is_empty()
            || test_files.contains(&rel)
            || OWNERS
                .iter()
                .any(|o| rel == *o || (o.ends_with('/') && rel.starts_with(o)))
        {
            continue;
        }
        if ALLOWED.iter().any(|(f, _)| *f == rel) {
            allowed_used.push(rel);
            continue;
        }
        for (line, sql) in writes {
            offenders.push(format!("  {rel}:{line}: {sql}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "metadata writes outside an owner (move each into a named function in db/ or \
         persist/recording.rs, keeping the caller's transaction):\n{}",
        offenders.join("\n")
    );
    let stale: Vec<_> = ALLOWED
        .iter()
        .filter(|(f, _)| !allowed_used.iter().any(|u| u == f))
        .map(|(f, _)| *f)
        .collect();
    assert!(
        stale.is_empty(),
        "allowlisted files no longer write: {stale:?}"
    );
}

#[test]
fn the_scan_sees_writes_and_skips_test_code() {
    let src = r####"
        const A: &str = "SELECT 1";
        fn f() {
            conn.execute("
                INSERT INTO t VALUES (1)", ());
            let _c = '"'; let _l: &'static str = "x";
            conn.execute(r#"UPDATE t SET a = "b""#, ());
            // "DELETE FROM in a comment"
            /* "DELETE FROM in /* nested */ block" */
            conn.execute("\n DELETE FROM t", ());
            conn.execute(b"REPLACE INTO t VALUES (2)", ());
            let _words = "UPDATED is not a statement";
        }
        #[cfg(test)]
        fn helper() { conn.execute("DELETE FROM t", ()); }
        #[cfg(test)]
        mod tests {
            fn g() { conn.execute("INSERT INTO t VALUES (3)", ()); }
        }
        #[cfg(test)]
        mod more_tests;
        fn after() { conn.execute("DELETE FROM after", ()); }
    "####;
    let scanned = scan(src);
    let lines: Vec<usize> = scanned.writes.iter().map(|(l, _)| *l).collect();
    assert_eq!(lines, vec![4, 7, 10, 11, 22], "{:?}", scanned.writes);
    assert_eq!(scanned.writes[0].1, "INSERT INTO t VALUES (1)");
    assert_eq!(scanned.test_modules, vec!["more_tests".to_string()]);
}
