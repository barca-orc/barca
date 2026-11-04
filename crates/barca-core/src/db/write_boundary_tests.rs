//! The metadata write boundary (architecture-cleanup step 2b, #800 item 16): every
//! `INSERT`/`UPDATE`/`DELETE`/`REPLACE` of `.barca/metadata.db` is issued by an owner in `db/`
//! (or `db.rs`) or by the history recorder in `persist/recording.rs`. Everything else calls a
//! named owner function and keeps its own transaction and eligibility rules. `db_schema.rs`
//! (migrations) and `sql/` (read-only) are outside the rule, and so is test code.

use std::path::{Path, PathBuf};

/// Owners: a file here, or under a directory here, may write.
const OWNERS: &[&str] = &[
    "db.rs",
    "db/",
    "persist/recording.rs",
    "db_schema.rs",
    "sql/",
];

/// Files that still write outside an owner, each with why.
const ALLOWED: &[(&str, &str)] = &[("persist.rs", "moving to an owner")];

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String, usize),
    Ident(String),
    Punct(char),
}

/// A small Rust lexer: string literals (plain, byte, raw), identifiers and punctuation;
/// comments, char literals, lifetimes and numbers are dropped.
fn lex(src: &str) -> Vec<Tok> {
    let s: Vec<char> = src.chars().collect();
    let (mut i, mut line, mut out) = (0usize, 1usize, Vec::new());
    let at = |i: usize| s.get(i).copied().unwrap_or('\0');
    while i < s.len() {
        let c = s[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == '*' {
            let mut depth = 0;
            loop {
                if i >= s.len() {
                    break;
                }
                if s[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if s[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if (c == 'r' || (c == 'b' && at(i + 1) == 'r')) && {
            let mut j = i + if c == 'b' { 2 } else { 1 };
            while at(j) == '#' {
                j += 1;
            }
            at(j) == '"'
        } {
            i += if c == 'b' { 2 } else { 1 };
            let mut hashes = 0;
            while s[i] == '#' {
                hashes += 1;
                i += 1;
            }
            i += 1;
            let start_line = line;
            let mut text = String::new();
            while i < s.len() {
                if s[i] == '"' && (1..=hashes).all(|k| at(i + k) == '#') {
                    i += 1 + hashes;
                    break;
                }
                if s[i] == '\n' {
                    line += 1;
                }
                text.push(s[i]);
                i += 1;
            }
            out.push(Tok::Str(text, start_line));
        } else if c == '"' || (c == 'b' && at(i + 1) == '"') {
            i += if c == 'b' { 2 } else { 1 };
            let start_line = line;
            let mut text = String::new();
            while i < s.len() && s[i] != '"' {
                if s[i] == '\\' {
                    let next = at(i + 1);
                    if next == '\n' {
                        line += 1;
                    }
                    // An escape is whitespace or one character: neither starts a keyword.
                    text.push(if matches!(next, 'n' | 't' | 'r' | '\n') {
                        ' '
                    } else {
                        next
                    });
                    i += 2;
                    continue;
                }
                if s[i] == '\n' {
                    line += 1;
                }
                text.push(s[i]);
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(text, start_line));
        } else if c == '\'' {
            // A char literal ('x', '\n', '\u{..}') or a lifetime ('a).
            if at(i + 1) == '\\' {
                i += 2;
                while i < s.len() && s[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if at(i + 2) == '\'' {
                i += 3;
            } else {
                i += 1;
                while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                    i += 1;
                }
            }
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(s[start..i].iter().collect()));
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

fn is_cfg_test(toks: &[Tok], i: usize) -> bool {
    let want = [
        Tok::Punct('#'),
        Tok::Punct('['),
        Tok::Ident("cfg".into()),
        Tok::Punct('('),
        Tok::Ident("test".into()),
        Tok::Punct(')'),
        Tok::Punct(']'),
    ];
    toks.len() >= i + want.len() && toks[i..i + want.len()] == want
}

/// The index after the item that starts at `i`: its `;`, its `,` (a field or argument) or the
/// `}` closing its first `{`; or at a `}` closing the enclosing block.
fn skip_item(toks: &[Tok], mut i: usize) -> usize {
    let mut depth = 0i32;
    while i < toks.len() {
        match &toks[i] {
            Tok::Punct('{' | '(' | '[') => depth += 1,
            Tok::Punct('}' | ')' | ']') => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
                if depth == 0 && toks[i] == Tok::Punct('}') {
                    return i + 1;
                }
            }
            Tok::Punct(';' | ',') if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    i
}

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
            let mut j = i + 7;
            // Further attributes on the same item.
            while toks.get(j) == Some(&Tok::Punct('#')) {
                j = skip_item(&toks, j + 1);
            }
            if let (Some(Tok::Ident(m)), Some(Tok::Ident(name)), Some(Tok::Punct(';'))) =
                (toks.get(j), toks.get(j + 1), toks.get(j + 2))
                && m == "mod"
            {
                out.test_modules.push(name.clone());
            }
            i = skip_item(&toks, j);
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

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The files `#[cfg(test)] mod name;` in `file` may name.
fn test_module_files(src_root: &Path, file: &Path, name: &str) -> Vec<String> {
    let rel = file.strip_prefix(src_root).unwrap();
    let stem = rel.file_stem().unwrap().to_str().unwrap();
    let parent = rel.parent().unwrap_or(Path::new(""));
    let dir = if matches!(stem, "mod" | "lib" | "main") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    [
        dir.join(format!("{name}.rs")),
        dir.join(name).join("mod.rs"),
    ]
    .iter()
    .map(|p| p.to_str().unwrap().replace('\\', "/"))
    .collect()
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
