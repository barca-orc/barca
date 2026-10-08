//! Tree discovery: which `.py` files make up the project.
//!
//! With no scope, barca walks the project root (the cwd by the time this runs, see
//! `config::find_root`). A scope is a list of files and directories: files are taken as given,
//! directories are walked. A walked file is a candidate only if it mentions barca (`from barca`
//! or `import barca`), so helper modules, notebooks and scratch scripts are never parsed as
//! pipelines; they are still read for cache hashing (`load::build_dag_blocking`).
//!
//! What a walk skips is controlled by `[discovery]` in barca.toml: `exclude` adds glob patterns
//! to the built-in excludes; `include` replaces the walk with the files its patterns match
//! (built-in excludes then do not apply). Patterns are root-relative, `/`-separated, and support
//! `*` (within a path segment), `**` (any number of segments) and `?`.

use crate::BarcaError;
use crate::config::DiscoveryToml;
use std::path::{Path, PathBuf};

/// Directory names never walked (unless `include` names files in them).
pub const EXCLUDED_DIRS: &[&str] = &[
    "__pycache__",
    "venv",
    "node_modules",
    "site-packages",
    "build",
    "dist",
    "tests",
    "test",
];

/// File names never discovered by a walk.
fn excluded_file(name: &str) -> bool {
    name == "conftest.py"
        || name == "setup.py"
        || name.starts_with("test_")
        || name.ends_with("_test.py")
}

fn excluded_dir(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIRS.contains(&name)
}

/// Whether a source mentions barca at the start of an import statement.
pub fn mentions_barca(source: &str) -> bool {
    source.lines().any(|line| {
        let l = line.trim_start();
        l.starts_with("from barca") || l.starts_with("import barca")
    })
}

/// The project's files, root-relative with `/` separators, sorted and without repeats.
/// `scope` entries are root-relative paths (`.py` files or directories); empty means the root.
/// Explicit `.py` files are always kept (they need not mention barca and are not filtered by
/// excludes); a missing explicit file is left for the parser to report.
pub fn discover(
    root: &Path,
    scope: &[PathBuf],
    cfg: &DiscoveryToml,
) -> Result<Vec<String>, BarcaError> {
    let include = cfg.include.as_deref().unwrap_or(&[]);
    let exclude = cfg.exclude.as_deref().unwrap_or(&[]);
    let walk_all = scope.is_empty();
    let dirs: Vec<PathBuf> = if walk_all {
        vec![PathBuf::from(".")]
    } else {
        scope
            .iter()
            .filter(|p| root.join(p).is_dir())
            .cloned()
            .collect()
    };

    let mut out: Vec<String> = scope
        .iter()
        .filter(|p| !root.join(p).is_dir())
        .map(|p| slash(p))
        .collect();

    let mut walked: Vec<String> = Vec::new();
    for dir in &dirs {
        if !include.is_empty() {
            walk(root, dir, &mut walked, false);
            walked.retain(|f| include.iter().any(|pat| glob_match(pat, f)));
        } else {
            walk(root, dir, &mut walked, true);
        }
    }
    walked.retain(|f| !exclude.iter().any(|pat| glob_match(pat, f)));
    walked.retain(|f| {
        std::fs::read_to_string(root.join(f))
            .map(|src| mentions_barca(&src))
            .unwrap_or(false)
    });
    if walked.is_empty() && out.is_empty() {
        let where_ = if walk_all {
            "the project root".to_string()
        } else {
            dirs.iter().map(|d| slash(d)).collect::<Vec<_>>().join(", ")
        };
        return Err(BarcaError::Usage(format!(
            "no @asset/@task/@sensor files found under {where_} ({}): no .py file there \
             imports barca. Pass the files explicitly (barca list pipeline.py), or check \
             [discovery] in barca.toml and the built-in excludes (barca docs discovery)",
            root.display()
        )));
    }
    out.extend(walked);
    out.sort();
    out.dedup();
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>, apply_excludes: bool) {
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = dir.join(entry.file_name());
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if apply_excludes && excluded_dir(&name) {
                continue;
            }
            walk(root, &path, out, apply_excludes);
        } else if name.ends_with(".py") && !(apply_excludes && excluded_file(&name)) {
            out.push(slash(&path));
        }
    }
}

/// `./a/b.py` -> `a/b.py`, with `/` separators.
fn slash(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_start_matches("./");
    if s.is_empty() {
        ".".to_string()
    } else {
        s.to_string()
    }
}

/// Glob match on `/`-separated paths: `*` and `?` stay inside a segment, `**` spans segments
/// (a trailing `dir/**` matches everything under `dir`).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.trim_start_matches("./").split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| match_segments(rest, &segs[i..])),
        Some((p, rest)) => match segs.split_first() {
            Some((s, srest)) => {
                match_segment(p.as_bytes(), s.as_bytes()) && match_segments(rest, srest)
            }
            None => false,
        },
    }
}

fn match_segment(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| match_segment(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && match_segment(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && match_segment(rest, &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("scratch/**", "scratch/a.py"));
        assert!(glob_match("scratch/**", "scratch/x/y/a.py"));
        assert!(!glob_match("scratch/**", "other/scratch/a.py"));
        assert!(glob_match("**/scratch/**", "other/scratch/a.py"));
        assert!(glob_match("pipelines/*.py", "pipelines/a.py"));
        assert!(!glob_match("pipelines/*.py", "pipelines/sub/a.py"));
        assert!(glob_match("pipelines/**/*.py", "pipelines/a.py"));
        assert!(glob_match("pipelines/**/*.py", "pipelines/sub/a.py"));
        assert!(glob_match("p?.py", "p1.py"));
        assert!(glob_match("./a.py", "a.py"));
        assert!(!glob_match("a.py", "b/a.py"));
        assert!(glob_match("**/a.py", "b/a.py"));
        assert!(glob_match("**/a.py", "a.py"));
    }

    #[test]
    fn barca_mentions() {
        assert!(mentions_barca("import os\nfrom barca import asset\n"));
        assert!(mentions_barca("    import barca\n"));
        assert!(!mentions_barca(
            "# from barca import asset? no\nx = 'import barca'\n"
        ));
    }

    #[test]
    fn default_excludes() {
        for d in [
            ".venv",
            ".git",
            "__pycache__",
            "venv",
            "node_modules",
            "tests",
            "build",
        ] {
            assert!(excluded_dir(d), "{d}");
        }
        assert!(!excluded_dir("pipelines"));
        for f in ["test_x.py", "x_test.py", "conftest.py", "setup.py"] {
            assert!(excluded_file(f), "{f}");
        }
        assert!(!excluded_file("testing_utils.py"));
    }

    fn in_tree<T>(files: &[(&str, &str)], f: impl FnOnce(&Path) -> T) -> T {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, src) in files {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, src).unwrap();
        }
        f(tmp.path())
    }

    const B: &str = "from barca import asset\n";

    #[test]
    fn walks_the_root_skipping_excludes_and_non_barca_files() {
        let got = in_tree(
            &[
                ("p/a.py", B),
                ("p/helpers.py", "def f(): pass\n"),
                ("tests/t.py", B),
                ("test_x.py", B),
                (".venv/v.py", B),
                ("top.py", B),
            ],
            |r| discover(r, &[], &DiscoveryToml::default()).unwrap(),
        );
        assert_eq!(got, vec!["p/a.py", "top.py"]);
    }

    #[test]
    fn scope_files_are_kept_and_directories_walked() {
        let got = in_tree(
            &[("p/a.py", B), ("q/b.py", B), ("q/plain.py", "x = 1\n")],
            |r| {
                discover(
                    r,
                    &[PathBuf::from("q/plain.py"), PathBuf::from("p")],
                    &DiscoveryToml::default(),
                )
                .unwrap()
            },
        );
        assert_eq!(got, vec!["p/a.py", "q/plain.py"]);
    }

    #[test]
    fn include_replaces_the_walk_and_exclude_extends_it() {
        let files = [("p/a.py", B), ("p/b.py", B), ("tests/t.py", B)];
        let cfg = DiscoveryToml {
            include: Some(vec!["tests/**".into(), "p/a.py".into()]),
            exclude: None,
        };
        let got = in_tree(&files, |r| discover(r, &[], &cfg).unwrap());
        assert_eq!(got, vec!["p/a.py", "tests/t.py"]);
        let cfg = DiscoveryToml {
            include: None,
            exclude: Some(vec!["p/b.py".into()]),
        };
        let got = in_tree(&files, |r| discover(r, &[], &cfg).unwrap());
        assert_eq!(got, vec!["p/a.py"]);
    }

    #[test]
    fn an_empty_walk_is_a_usage_error() {
        let err = in_tree(&[("x.py", "y = 1\n")], |r| {
            discover(r, &[], &DiscoveryToml::default()).unwrap_err()
        });
        assert!(matches!(err, BarcaError::Usage(m) if m.contains("no @asset")));
    }
}
