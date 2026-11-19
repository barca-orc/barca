//! The order inside `barca-store` (#1056; `docs/engineering/crate-split-2.md`). Cargo keeps this
//! crate below the decisions and the engine; it checks nothing between the modules of one crate,
//! so this test does. A module names only the modules of the rows before its own, never its own
//! row or a later one, so the storage layer has no cycle and no exception: an edge this order
//! forbids fails. Moving the item to a row before both of its users, or passing it in as a
//! value, is the fix.

use barca_model::source_scan::module_graph::rows_problems;
use std::path::Path;

/// First row first. A new module gets a row here.
const ROWS: &[&[&str]] = &[
    // Plain data and helpers with no module of this crate below them: text every layer formats
    // the same way, the HTTPS client.
    &["http", "text"],
    // The error envelope and the API version a client and a server share; the result types a
    // run and a plan return.
    &["envelope", "results"],
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

#[test]
fn modules_name_only_the_rows_before_their_own() {
    let problems = rows_problems(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), ROWS);
    assert!(
        problems.is_empty(),
        "the order inside barca-store (ROWS in crates/barca-store/src/layering_tests.rs). Move \
         the item to a row before both of its users, or pass it in:\n{}",
        problems.join("\n")
    );
}
