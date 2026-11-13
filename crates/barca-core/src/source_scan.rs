//! A small Rust lexer for the tests that audit this crate's own source: string literals
//! (plain, byte, raw), identifiers and punctuation, with comments, char literals, lifetimes
//! and numbers dropped, and `#[cfg(test)]` items skipped. Used by the metadata write boundary
//! (`db/write_boundary_tests.rs`), the step-status vocabulary (`step_status.rs`) and the
//! layering guard (`layering_tests.rs`).

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub(crate) enum Tok {
    Str(String, usize),
    Ident(String),
    Punct(char),
}

/// A small Rust lexer: string literals (plain, byte, raw), identifiers and punctuation;
/// comments, char literals, lifetimes and numbers are dropped.
pub(crate) fn lex(src: &str) -> Vec<Tok> {
    lex_with_lines(src).0
}

/// [`lex`], with the line each token starts on (a parallel vector).
pub(crate) fn lex_with_lines(src: &str) -> (Vec<Tok>, Vec<usize>) {
    let s: Vec<char> = src.chars().collect();
    let (mut i, mut line, mut out) = (0usize, 1usize, Vec::new());
    let mut lines = Vec::new();
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
            lines.push(start_line);
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
            lines.push(start_line);
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
            lines.push(line);
        } else {
            out.push(Tok::Punct(c));
            lines.push(line);
            i += 1;
        }
    }
    (out, lines)
}

pub(crate) fn is_cfg_test(toks: &[Tok], i: usize) -> bool {
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
pub(crate) fn skip_item(toks: &[Tok], mut i: usize) -> usize {
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

pub(crate) fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
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
pub(crate) fn test_module_files(src_root: &Path, file: &Path, name: &str) -> Vec<String> {
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

/// Every string literal in non-test code: (line, text). `#[cfg(test)]` items are skipped.
pub(crate) fn string_literals(src: &str) -> Vec<(usize, String)> {
    let toks = lex(src);
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if is_cfg_test(&toks, i) {
            i = skip_cfg_test_item(&toks, i).0;
            continue;
        }
        if let Tok::Str(text, line) = &toks[i] {
            out.push((*line, text.clone()));
        }
        i += 1;
    }
    out
}

/// The index after the `#[cfg(test)]` item at `i` (further attributes included), and the
/// module name when the item is `mod name;`.
pub(crate) fn skip_cfg_test_item(toks: &[Tok], i: usize) -> (usize, Option<String>) {
    let mut j = i + 7;
    while toks.get(j) == Some(&Tok::Punct('#')) {
        j = skip_item(toks, j + 1);
    }
    let name = match (toks.get(j), toks.get(j + 1), toks.get(j + 2)) {
        (Some(Tok::Ident(m)), Some(Tok::Ident(name)), Some(Tok::Punct(';'))) if m == "mod" => {
            Some(name.clone())
        }
        _ => None,
    };
    (skip_item(toks, j), name)
}

/// Every `.rs` file under `root` that is not a test module file (`#[cfg(test)] mod name;`
/// pointing at it), as (path relative to `root`, source).
pub(crate) fn non_test_sources(root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(root, &mut files);
    files.sort();
    let mut test_files = Vec::new();
    let mut sources = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        let src = std::fs::read_to_string(file).unwrap();
        let toks = lex(&src);
        let mut i = 0;
        while i < toks.len() {
            if is_cfg_test(&toks, i) {
                let (next, name) = skip_cfg_test_item(&toks, i);
                if let Some(name) = name {
                    test_files.extend(test_module_files(root, file, &name));
                }
                i = next;
                continue;
            }
            i += 1;
        }
        sources.push((rel, src));
    }
    sources.retain(|(rel, _)| !test_files.contains(rel));
    sources
}
