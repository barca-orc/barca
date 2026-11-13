//! The crate graph (#905). Cargo stops a crate from naming one it does not depend on, so the
//! layering is the dependency lists in the manifests; this test keeps those lists from growing
//! an edge the design rules out:
//!
//! ```text
//! barca-model  <  barca-compile  <  barca-engine  <  barca-core  <  barca-server, barca-cli
//! barca-model  <  barca-db                        <  barca-core
//! ```
//!
//! `barca-compile` and `barca-db` are siblings; `barca-engine` (the per-step scheduler and worker
//! loop) never reaches the metadata database; `barca-core` wires everything, including
//! `execution`, the run orchestrator; `barca-cli` and `barca-server` see only `barca-core`.
//! libsql is linked by `barca-db` and `barca-core` alone. A new crate under `crates/` gets a line
//! in [`ALLOWED`]. Moving an item down a crate, or passing it in as a value, is the fix for a
//! failure here; widening the table is not.

use std::path::Path;

/// Each crate (its directory under `crates/`) and the workspace crates it may depend on, in any
/// dependency table (dev-dependencies too: a dev cycle splits a crate's tests from its code).
const ALLOWED: &[(&str, &[&str])] = &[
    ("barca-model", &[]),
    ("barca-compile", &["barca-model"]),
    ("barca-db", &["barca-model"]),
    ("barca-engine", &["barca-model", "barca-compile"]),
    (
        "barca-core",
        &["barca-model", "barca-compile", "barca-db", "barca-engine"],
    ),
    ("barca-server", &["barca-core"]),
    ("barca-cli", &["barca-core", "barca-server"]),
];

/// The crates that may link libsql (the metadata database).
const LIBSQL: &[&str] = &["barca-db", "barca-core"];

/// Every dependency name in a manifest's dependency tables, target-specific ones included.
fn dependencies(manifest: &toml::Table) -> Vec<String> {
    let tables = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut out = Vec::new();
    let mut collect = |t: &toml::Table| {
        for name in tables {
            if let Some(deps) = t.get(name).and_then(toml::Value::as_table) {
                out.extend(deps.keys().cloned());
            }
        }
    };
    collect(manifest);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            collect(target);
        }
    }
    out
}

#[test]
fn crates_depend_only_on_crates_below_them() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut problems = Vec::new();
    let mut dirs: Vec<String> = std::fs::read_dir(&crates_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    for dir in &dirs {
        let Some((_, allowed)) = ALLOWED.iter().find(|(d, _)| d == dir) else {
            problems.push(format!(
                "  crates/{dir} is not in ALLOWED (crates/barca-core/src/layering_tests.rs)"
            ));
            continue;
        };
        let text = std::fs::read_to_string(crates_dir.join(dir).join("Cargo.toml")).unwrap();
        let manifest: toml::Table = text.parse().unwrap();
        for dep in dependencies(&manifest) {
            let workspace_crate = ALLOWED.iter().any(|(d, _)| *d == dep);
            if workspace_crate && !allowed.contains(&dep.as_str()) {
                problems.push(format!("  {dir} depends on {dep}"));
            }
            if dep == "libsql" && !LIBSQL.contains(&dir.as_str()) {
                problems.push(format!("  {dir} depends on libsql"));
            }
        }
    }
    for (dir, _) in ALLOWED {
        if !dirs.iter().any(|d| d == dir) {
            problems.push(format!(
                "  ALLOWED names crates/{dir}, which does not exist"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "crate graph (#905): move the item into a crate the user may depend on, or pass it in \
         as a value.\n{}",
        problems.join("\n")
    );
}
