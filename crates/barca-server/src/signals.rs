//! Data-health signals on `GET /state` rows (#925): what a node's recorded results say about its
//! data, from facts the metadata database already holds (row counts its workers recorded, and
//! when each result was written). Nothing is inspected or guessed: a count no worker recorded
//! raises no signal.

use std::collections::HashMap;

use barca_core::NodeKind;
use barca_core::cache_state::CacheState;
use barca_core::db::NodeAge;
use barca_core::status::NodeStatus;
use serde::Serialize;

/// One thing a node's recorded results say is worth a look. A node has any number of them, in
/// this order: `empty`, `rows_changed`, `behind_input`.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataSignal {
    /// Its latest successful result wrote no rows: for a partitioned node, `empty_keys` of the
    /// `keys` whose latest result recorded a row count (1 of 1 for a node without keys).
    Empty {
        empty_keys: u32,
        keys: u32,
        /// How many were empty as of before its newest run (each key's latest success before
        /// that run); `null` when no key had a counted result before it.
        previous_empty_keys: Option<u32>,
    },
    /// A latest successful result's row count differs from the one before it (same node, same
    /// key) by more than half of it, or rows came after an empty result. `rows` and
    /// `previous_rows` are of `key`, the key whose count moved the most relative to before
    /// (`null` for a node without keys); `changed_keys` counts every key that moved.
    RowsChanged {
        changed_keys: u32,
        key: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        rows: i64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        previous_rows: i64,
    },
    /// Its latest successful result is older than the newest successful result of `input`, an
    /// asset it reads (typically a `collect()` fan-in after its input's keys ran again), and its
    /// cache state is `unknown`, so nothing else says it is behind. The input is the one with
    /// the newest result; times are UTC `YYYY-MM-DD HH:MM:SS`, as `last_success_at`.
    BehindInput {
        input: String,
        input_success_at: String,
        last_success_at: String,
    },
}

/// The signals of `node`, from the node ages of every node (`ages`) and its inputs with their
/// kinds (`inputs`, asked only for a node whose state is `unknown`).
pub(crate) fn signals<'a>(
    node: &NodeStatus,
    ages: &HashMap<String, NodeAge>,
    inputs: impl FnOnce() -> Vec<(&'a str, NodeKind)>,
) -> Vec<DataSignal> {
    let mut out = Vec::new();
    if node.kind != "asset" {
        return out;
    }
    let Some(age) = ages.get(&node.id) else {
        return out;
    };
    if let Some(rows) = &age.rows {
        if rows.empty > 0 {
            out.push(DataSignal::Empty {
                empty_keys: rows.empty,
                keys: rows.counted,
                previous_empty_keys: rows.previous_empty,
            });
        }
        if let Some(change) = &rows.largest_change {
            out.push(DataSignal::RowsChanged {
                changed_keys: rows.changed,
                key: change.key.clone(),
                rows: change.rows,
                previous_rows: change.previous_rows,
            });
        }
    }
    if node.cache.state == CacheState::Unknown
        && let (Some(own), Some(at)) = (age.rows.as_ref(), age.last_success_at.as_ref())
    {
        let behind = inputs()
            .into_iter()
            .filter(|(_, kind)| *kind == NodeKind::Asset)
            .filter_map(|(input, _)| {
                let theirs = ages.get(input)?;
                let newest = &theirs.rows.as_ref()?.newest;
                // Newer, and from another run: a run writes a node after its inputs, and a key
                // its run recorded late (the end-of-run ledger) is not newer data.
                let other_run = newest.1.is_none() || newest.1 != own.newest.1;
                (newest.0 > own.newest.0 && other_run)
                    .then(|| Some((newest.0, input, theirs.last_success_at.clone()?)))
                    .flatten()
            })
            .max_by_key(|(id, _, _)| *id);
        if let Some((_, input, input_at)) = behind {
            out.push(DataSignal::BehindInput {
                input: input.to_string(),
                input_success_at: input_at,
                last_success_at: at.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use barca_core::db::{RowChange, RowFacts};

    fn node(id: &str, state: CacheState) -> NodeStatus {
        let mut n: NodeStatus = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "kind": "asset", "inputs": [], "partitioned": false,
            "cache": {"state": "unknown", "reason": "", "detail": ""},
            "last_materialization": null, "shape": null, "env": [],
        }))
        .unwrap();
        n.cache.state = state;
        n
    }

    fn age(newest: (i64, &str), at: &str, rows: RowFacts) -> NodeAge {
        NodeAge {
            last_success_at: Some(at.to_string()),
            rows: Some(RowFacts {
                newest: (newest.0, Some(newest.1.to_string())),
                ..rows
            }),
            ..NodeAge::default()
        }
    }

    #[test]
    fn empty_and_changed_counts_become_signals_with_both_numbers() {
        let ages = HashMap::from([(
            "p.py:a".to_string(),
            age(
                (5, "r2"),
                "2026-11-16 10:00:00",
                RowFacts {
                    partitioned: true,
                    counted: 10,
                    empty: 3,
                    previous_empty: Some(0),
                    changed: 4,
                    largest_change: Some(RowChange {
                        key: Some("k=a".into()),
                        rows: 0,
                        previous_rows: 120,
                    }),
                    ..RowFacts::default()
                },
            ),
        )]);
        assert_eq!(
            signals(&node("p.py:a", CacheState::Cached), &ages, Vec::new),
            vec![
                DataSignal::Empty {
                    empty_keys: 3,
                    keys: 10,
                    previous_empty_keys: Some(0)
                },
                DataSignal::RowsChanged {
                    changed_keys: 4,
                    key: Some("k=a".into()),
                    rows: 0,
                    previous_rows: 120
                },
            ]
        );
        // Facts without an empty key or a change say nothing; a task says nothing.
        let quiet = HashMap::from([(
            "p.py:a".to_string(),
            age((5, "r2"), "2026-11-16 10:00:00", RowFacts::default()),
        )]);
        assert!(signals(&node("p.py:a", CacheState::Cached), &quiet, Vec::new).is_empty());
        let mut task = node("p.py:a", CacheState::AlwaysRuns);
        task.kind = "task".into();
        assert!(signals(&task, &ages, Vec::new).is_empty());
    }

    #[test]
    fn an_unknown_fan_in_older_than_its_input_is_behind_it() {
        let ages = HashMap::from([
            (
                "p.py:all".to_string(),
                age((10, "r1"), "2026-11-14 10:00:00", RowFacts::default()),
            ),
            (
                "p.py:keys".to_string(),
                age((40, "r3"), "2026-11-16 09:00:00", RowFacts::default()),
            ),
            (
                "p.py:old".to_string(),
                age((4, "r0"), "2026-11-13 09:00:00", RowFacts::default()),
            ),
            (
                "p.py:sensor".to_string(),
                age((50, "r4"), "2026-11-16 09:30:00", RowFacts::default()),
            ),
        ]);
        let inputs = [
            ("p.py:keys", NodeKind::Asset),
            ("p.py:old", NodeKind::Asset),
            ("p.py:sensor", NodeKind::Sensor),
        ];
        assert_eq!(
            signals(&node("p.py:all", CacheState::Unknown), &ages, || inputs
                .to_vec()),
            vec![DataSignal::BehindInput {
                input: "p.py:keys".into(),
                input_success_at: "2026-11-16 09:00:00".into(),
                last_success_at: "2026-11-14 10:00:00".into(),
            }]
        );
        // A state that already says what to do, or that it is current, raises nothing.
        for state in [CacheState::Stale, CacheState::Cached, CacheState::Partial] {
            assert!(signals(&node("p.py:all", state), &ages, || inputs.to_vec()).is_empty());
        }
        // An input written later in the same run is not newer data.
        let same_run = HashMap::from([
            (
                "p.py:all".to_string(),
                age((10, "r1"), "2026-11-14 10:00:00", RowFacts::default()),
            ),
            (
                "p.py:keys".to_string(),
                age((12, "r1"), "2026-11-14 10:00:01", RowFacts::default()),
            ),
        ]);
        assert!(
            signals(&node("p.py:all", CacheState::Unknown), &same_run, || inputs
                .to_vec())
            .is_empty()
        );
    }
}
