//! The crate graph (#905, #1056). Cargo stops a crate from naming one it does not depend on, so
//! the layering is the dependency lists in the manifests; this test keeps those lists from
//! growing an edge the design rules out. Each crate may use the crates on the lines above its
//! own that [`ALLOWED`] names:
//!
//! ```text
//! barca-model                      the shared vocabulary
//! barca-compile   barca-db   barca-proc      source to plan; the database; child processes
//! barca-engine                     the worker loop: compile and proc, never the database
//! barca-store                      what is kept: db, proc and compile, never the engine
//! barca-decide                     cache and freshness verdicts: store and below
//! barca-run       barca-analysis   a run records (engine, decide); a read model reads (decide)
//! barca-service                    one operation per command; the hosts' one import
//! barca-server    barca-cli        the hosts
//! ```
//!
//! `barca-engine` (the per-step scheduler and worker loop) never reaches the metadata database;
//! `barca-run` is the one crate that drives it and records what happened; `barca-analysis` and
//! `barca-run` sit side by side and never name each other; `barca-cli` and `barca-server` see
//! only `barca-service`. libsql is linked by the crates in [`LIBSQL`] alone. A new crate under
//! `crates/` gets a line in [`ALLOWED`]. Moving an item down a crate, or passing it in as a
//! value, is the fix for a failure here; widening the table is not.

use std::path::Path;

/// Each crate (its directory under `crates/`) and the workspace crates it may depend on, in any
/// dependency table (dev-dependencies too: a dev cycle splits a crate's tests from its code).
const ALLOWED: &[(&str, &[&str])] = &[
    ("barca-model", &[]),
    ("barca-compile", &["barca-model"]),
    ("barca-db", &["barca-model"]),
    ("barca-proc", &["barca-model"]),
    (
        "barca-engine",
        &["barca-model", "barca-compile", "barca-proc"],
    ),
    (
        "barca-store",
        &["barca-model", "barca-compile", "barca-db", "barca-proc"],
    ),
    (
        "barca-decide",
        &["barca-model", "barca-compile", "barca-db", "barca-store"],
    ),
    (
        "barca-analysis",
        &[
            "barca-model",
            "barca-compile",
            "barca-db",
            "barca-store",
            "barca-decide",
        ],
    ),
    (
        "barca-run",
        &[
            "barca-model",
            "barca-compile",
            "barca-db",
            "barca-proc",
            "barca-engine",
            "barca-store",
            "barca-decide",
        ],
    ),
    (
        "barca-service",
        &[
            "barca-model",
            "barca-compile",
            "barca-db",
            "barca-proc",
            "barca-engine",
            "barca-store",
            "barca-decide",
            "barca-analysis",
            "barca-run",
        ],
    ),
    ("barca-server", &["barca-service"]),
    ("barca-cli", &["barca-service", "barca-server"]),
];

/// The crates that may link libsql (the metadata database): `barca-db`, which owns every
/// write; the three that read or migrate through a connection `barca-db` hands them (gc and
/// the ledger in `barca-store`, proofs and step rows in `barca-run`, queries in
/// `barca-service`); and `barca-decide`, in one test only (a dev-dependency). Not
/// `barca-engine`, `barca-proc`, `barca-compile`, `barca-analysis` or the hosts.
const LIBSQL: &[&str] = &[
    "barca-db",
    "barca-store",
    "barca-decide",
    "barca-run",
    "barca-service",
];

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
                "  crates/{dir} is not in ALLOWED (crates/barca-service/src/layering_tests.rs)"
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
        "crate graph (#905, #1056): move the item into a crate the user may depend on, or pass it in \
         as a value.\n{}",
        problems.join("\n")
    );
}
