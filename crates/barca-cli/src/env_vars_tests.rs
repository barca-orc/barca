//! Every `BARCA_*` environment variable any crate or the Python package names is documented in
//! the contract's "Environment variables" section, or listed in [`INTERNAL_ENV_VARS`] with why
//! not. Skipped outside the repository (an sdist of this crate alone).

use crate::snapshot::crate_dir;
use std::path::{Path, PathBuf};

/// `BARCA_*` variables barca sets or reads that are not documented in contract.md, each with
/// why. A name ending in `*` stands for every name with that prefix.
const INTERNAL_ENV_VARS: &[(&str, &str)] = &[
    (
        "BARCA_WORKER*",
        "set by the coordinator on its own Python worker processes",
    ),
    (
        "BARCA_SOCKET",
        "the coordinator's Unix socket, set for its own workers and helpers",
    ),
    (
        "BARCA_LIFELINE",
        "set for Python helpers so they exit when the coordinator does",
    ),
    (
        "BARCA_JOB_PGID",
        "the coordinator's process group, set for Python helpers",
    ),
    (
        "BARCA_DOCTOR_REQUEST",
        "the request `barca doctor` passes to its Python helper",
    ),
    (
        "BARCA_RUN_ID",
        "sample user source in env_reads' tests, not a variable barca sets",
    ),
    (
        "BARCA_UPDATE_SNAPSHOTS",
        "regenerates barca's own test snapshots",
    ),
    (
        "BARCA_UPLOAD_SNAPSHOT_PREFIX",
        "set by the coordinator for its workers and transfer helper",
    ),
];

/// The only file that may name a `BARCA_TEST_*` variable (hooks for barca's own tests).
const TEST_HOOKS: &str = "crates/barca-model/src/test_hooks.rs";

/// Every `"BARCA_..."` or `'BARCA_...'` string literal in `text`. A bare prefix (`"BARCA_"`, a
/// name ending in `_`) is not a variable.
fn barca_env_literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for quote in ['"', '\''] {
        let needle = format!("{quote}BARCA_");
        let mut rest = text;
        while let Some(at) = rest.find(&needle) {
            let tail = &rest[at + 1..];
            let len = tail
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(tail.len());
            let name = &tail[..len];
            if !name.ends_with('_') {
                out.push(name.to_string());
            }
            rest = &tail[len..];
        }
    }
    out
}

fn source_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

#[test]
fn every_barca_env_var_is_in_the_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = root.join("python/barca");
    if !python.is_dir() {
        return; // an sdist of this crate alone
    }
    let mut files = Vec::new();
    for krate in [
        "barca-model",
        "barca-compile",
        "barca-db",
        "barca-engine",
        "barca-core",
        "barca-cli",
        "barca-server",
    ] {
        source_files(
            &root.join("crates").join(krate).join("src"),
            "rs",
            &mut files,
        );
    }
    source_files(&python, "py", &mut files);
    files.sort();
    assert!(files.len() > 50, "found only {} source files", files.len());

    let contract = std::fs::read_to_string(crate_dir().join("docs/contract.md")).unwrap();
    let start = contract
        .find("\n## Environment variables\n")
        .expect("contract.md has an Environment variables section");
    let section = &contract[start + 1..];
    let section = &section[..section[3..].find("\n## ").map_or(section.len(), |e| e + 3)];

    let test_prefix = concat!("BARCA", "_TEST_");
    let internal = |name: &str| {
        INTERNAL_ENV_VARS
            .iter()
            .any(|(pattern, _)| match pattern.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => name == *pattern,
            })
    };
    let mut missing = std::collections::BTreeSet::new();
    let mut stray_test_hooks = std::collections::BTreeSet::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        for name in barca_env_literals(&text) {
            if name.starts_with(test_prefix) {
                if !file.ends_with(TEST_HOOKS) {
                    stray_test_hooks.insert(format!("{name} in {shown}"));
                }
            } else if !internal(&name) && !section.contains(&format!("`{name}`")) {
                missing.insert(format!("{name} in {shown}"));
            }
        }
    }
    assert!(
        stray_test_hooks.is_empty(),
        "read {test_prefix}* variables only in {TEST_HOOKS}: {stray_test_hooks:#?}"
    );
    assert!(
        missing.is_empty(),
        "these variables are not in contract.md's \"Environment variables\" section; document \
         them there and in site/src/content/docs/reference/config.mdx, or add an internal one \
         to INTERNAL_ENV_VARS with why: {missing:#?}"
    );
}

#[test]
fn env_literals_are_found_in_both_quotes() {
    // Spelled with `X_` so this file names no variable of its own.
    let text = r#"env("X_A") os.environ.get('X_B_2', "x") "X_" 'X_P_' X_C"#.replace("X_", "BARCA_");
    let want = ["X_A", "X_B_2"].map(|n| n.replace("X_", "BARCA_"));
    assert_eq!(barca_env_literals(&text), want);
}
