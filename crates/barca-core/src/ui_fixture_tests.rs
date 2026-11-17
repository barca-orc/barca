//! The health words the web UI's test fixtures use (#959). The UI decides no health: its unit
//! tests and its kit page still need rows "as the server would serve them", and a fixture
//! that spelled the health table by hand would be a second copy of [`crate::health`].
//!
//! This test classifies one row alone (no graph, no signals, no schedule, no tick) for every
//! combination of the facts a fixture states, and keeps the answers in
//! `ui/src/lib/testdata/health-alone.json`, which `ui/src/lib/testdata/served.ts` looks up.
//! The weights of "Distance from fresh" (#1028) are kept the same way, in
//! `ui/src/lib/testdata/weights.json`, which `ui/src/lib/testdata/rollup.ts` states a fixture
//! entry's `weight` with: the app holds no weight table, and neither does a fixture.
//! When the health table or a weight changes, regenerate the files and the UI's fixtures follow:
//!
//! ```text
//! BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-core ui_fixture
//! ```

use crate::cache_state::CacheState;
use crate::health::{Checks, Evidence, classify};
use crate::status::{LastMaterialization, NodeStatus, PartitionState};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// The cache states, as their wire words.
const STATES: &[CacheState] = &[
    CacheState::Cached,
    CacheState::Unsynced,
    CacheState::Stale,
    CacheState::NeverRun,
    CacheState::Partial,
    CacheState::Blocked,
    CacheState::Unknown,
    CacheState::AlwaysRuns,
];

/// What a fixture says of the node's last attempt.
const LAST: &[&str] = &["none", "success", "failed", "cancelled"];

fn row(state: CacheState, last: &str, keys_failed: bool) -> NodeStatus {
    let mut node: NodeStatus = serde_json::from_value(serde_json::json!({
        "id": "p.py:a", "name": "a", "kind": "asset", "inputs": [], "partitioned": false,
        "cache": {"state": "unknown", "reason": "", "detail": ""},
        "last_materialization": null, "shape": null, "env": [],
    }))
    .unwrap();
    node.cache.state = state;
    if last != "none" {
        node.last_materialization = Some(LastMaterialization {
            content_hash: None,
            status: last.to_string(),
            created_at: "2026-11-16 10:00:00".to_string(),
            elapsed_seconds: None,
            run_hash: None,
            artifact: None,
            format: None,
            size_bytes: None,
            rows: None,
            partition: None,
            error: (last == "failed").then(|| "ValueError: no rows".to_string()),
        });
    }
    if keys_failed {
        node.partitioned = true;
        node.partitions = Some(PartitionState {
            total: 4,
            cached: 2,
            missing: 2,
            failed: 1,
            ..serde_json::from_value(serde_json::json!({
                "total": 0, "cached": 0, "missing": 0, "missing_keys": [],
            }))
            .unwrap()
        });
    }
    node
}

/// `{"<cache state>|<last attempt>|<keys failed>": {"health": …, "health_reason": …}}`.
fn table() -> String {
    let mut out: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for state in STATES {
        for last in LAST {
            for keys_failed in [false, true] {
                let mut nodes = vec![row(*state, last, keys_failed)];
                classify(
                    &mut nodes,
                    Evidence {
                        ages: &HashMap::new(),
                        pinned: &HashSet::new(),
                        late: &HashSet::new(),
                        checks: &Checks::default(),
                        path: crate::refresh_path::PathEvidence::none(),
                        timing: crate::estimates::Timing::none(),
                        now: 0,
                    },
                );
                let word = serde_json::to_value(state).unwrap();
                out.insert(
                    format!("{}|{last}|{keys_failed}", word.as_str().unwrap()),
                    serde_json::json!({
                        "health": nodes[0].health,
                        "health_reason": nodes[0].health_reason,
                    }),
                );
            }
        }
    }
    let mut text = serde_json::to_string_pretty(&out).unwrap();
    text.push('\n');
    text
}

#[test]
fn the_ui_fixture_health_table_is_what_classify_says() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/lib/testdata/health-alone.json");
    let actual = table();
    if std::env::var("BARCA_UPDATE_SNAPSHOTS").is_ok_and(|v| !v.is_empty() && v != "0") {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "{} is not what `health::classify` says of a row alone. The UI's fixtures read it \
         (ui/src/lib/testdata/served.ts): regenerate it with \
         `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-core ui_fixture`, then run the UI's \
         unit tests (`pnpm --dir ui test`).",
        path.display()
    );
}

/// `{"fresh": 1, "<cause>": <weight>, …}`: what one node of each part weighs.
fn weights() -> String {
    let mut out: BTreeMap<&str, usize> = crate::health::Cause::ALL
        .iter()
        .map(|c| (c.as_str(), c.weight()))
        .collect();
    out.insert("fresh", crate::rollup::FRESH_WEIGHT);
    let mut text = serde_json::to_string_pretty(&out).unwrap();
    text.push('\n');
    text
}

#[test]
fn the_ui_fixture_weights_are_the_rollups() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/lib/testdata/weights.json");
    let actual = weights();
    if std::env::var("BARCA_UPDATE_SNAPSHOTS").is_ok_and(|v| !v.is_empty() && v != "0") {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "{} is not what `Cause::weight` and `FRESH_WEIGHT` say. The UI's fixtures read it \
         (ui/src/lib/testdata/rollup.ts): regenerate it with \
         `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-core ui_fixture`, then run the UI's \
         unit tests (`pnpm --dir ui test`).",
        path.display()
    );
}
