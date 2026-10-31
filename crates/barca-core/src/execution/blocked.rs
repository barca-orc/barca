//! What a failed partition key blocks (#764).
//!
//! A failed step blocks everything below it. When the step is one key of a partitioned node, a
//! consumer that reads the node key for key (`partitions_from(node)`, or a node with the same
//! dimension) still has everything it needs for every other key: only its own key of the
//! failure is blocked. A consumer that reads every key (a `collect()` fan-in, an unpartitioned
//! node, a node on other dimensions) is blocked whole, as before.
//!
//! The decision is made where a phase is prepared, before its cache decisions: the blocked keys
//! are dropped from the phase's steps and reported `skipped` with reason `upstream_key_failed`,
//! the rest run. The same rule as `--partition` uses to tell a narrowed consumer from a fan-in
//! (`crate::selection`): a consumer's keys line up with an input's when both have exactly the
//! same dimension names and the input is not collected.

use crate::dag::Dag;
use crate::planner::Phase;
use crate::report::kind_str;
use crate::results::StepReport;
use crate::targets::{blocking_failure, short_name};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Partition keys as `dim=value` suffixes ([`crate::PartitionKey::suffix`]), by base id.
pub(super) type Keys = HashMap<String, HashSet<String>>;

/// What this run knows about failures so far.
pub(super) struct Failures<'a> {
    pub dag: &'a Dag,
    /// Bases with a failed step (whole, or one of their keys).
    pub failed_bases: &'a HashSet<String>,
    /// The keys whose step failed, by base.
    pub failed_keys: &'a Keys,
    /// The keys that were not computed because the same key of an input failed or was blocked.
    pub blocked_keys: &'a Keys,
    /// Bases skipped whole: nothing of them ran.
    pub skipped: &'a HashSet<String>,
}

/// One key of a node that cannot run: the key, the upstream step it reads (with its key) and
/// whether that step failed (else it was blocked in turn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockedKey {
    pub key: String,
    pub upstream: String,
    pub failed: bool,
}

/// How a failure reaches a node that is below one.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Blocked {
    /// Only these keys: the rest of the node runs.
    Keys(Vec<BlockedKey>),
    /// The whole node.
    Whole,
}

/// How the failures reach `base`, a node [`blocking_failure`] says is below one. `dropped` are
/// the bases of the same phase already decided to be blocked whole.
pub(super) fn how(f: &Failures<'_>, base: &str, dropped: &HashSet<String>) -> Blocked {
    let Some(node) = f.dag.get_node(base) else {
        return Blocked::Whole;
    };
    if node.extracted.partitions.is_empty() {
        return Blocked::Whole;
    }
    let tainted = |up: &str| {
        f.failed_bases.contains(up)
            || f.blocked_keys.get(up).is_some_and(|k| !k.is_empty())
            || f.skipped.contains(up)
            || dropped.contains(up)
            || blocking_failure(f.dag, up, f.failed_bases).is_some()
    };
    if node.resolved_collected.values().any(|up| tainted(up)) {
        return Blocked::Whole;
    }
    let mine: BTreeSet<&String> = node.extracted.partitions.keys().collect();
    let aligned = |up: &str| {
        f.dag.get_node(up).is_some_and(|u| {
            let theirs: BTreeSet<&String> = u.extracted.partitions.keys().collect();
            !theirs.is_empty() && theirs == mine
        })
    };
    let mut reads: Vec<&str> = node
        .resolved_inputs
        .values()
        .map(String::as_str)
        .chain(f.dag.partition_sources(base))
        .collect();
    reads.sort_unstable();
    reads.dedup();
    let mut out: Vec<BlockedKey> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for up in reads {
        if !tainted(up) {
            continue;
        }
        if !aligned(up) {
            return Blocked::Whole;
        }
        let failed = f.failed_keys.get(up).filter(|k| !k.is_empty());
        let blocked = f.blocked_keys.get(up).filter(|k| !k.is_empty());
        if failed.is_none() && blocked.is_none() {
            // Nothing of it ran (failed before it had keys, or skipped whole).
            return Blocked::Whole;
        }
        for (keys, failed) in [(failed, true), (blocked, false)] {
            for key in keys.into_iter().flatten() {
                if seen.insert(key) {
                    out.push(BlockedKey {
                        key: key.clone(),
                        upstream: format!("{up}[{key}]"),
                        failed,
                    });
                }
            }
        }
    }
    if out.is_empty() {
        return Blocked::Whole;
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Blocked::Keys(out)
}

/// Why the coordinator skipped `key` of `base` within a phase: the same key of an aligned input
/// failed, or was blocked in turn. Falls back to the failed node below which `base` is.
pub(super) fn cause(f: &Failures<'_>, base: &str, key: &str) -> BlockedKey {
    let node = f.dag.get_node(base);
    let mine: BTreeSet<&String> = node
        .map(|n| n.extracted.partitions.keys().collect())
        .unwrap_or_default();
    let mut reads: Vec<&str> = node
        .into_iter()
        .flat_map(|n| n.resolved_inputs.values().map(String::as_str))
        .chain(f.dag.partition_sources(base))
        .filter(|up| {
            f.dag.get_node(up).is_some_and(|u| {
                let theirs: BTreeSet<&String> = u.extracted.partitions.keys().collect();
                theirs == mine
            })
        })
        .collect();
    reads.sort_unstable();
    for (keys, failed) in [(f.failed_keys, true), (f.blocked_keys, false)] {
        if let Some(up) = reads
            .iter()
            .find(|up| keys.get(**up).is_some_and(|k| k.contains(key)))
        {
            return BlockedKey {
                key: key.to_string(),
                upstream: format!("{up}[{key}]"),
                failed,
            };
        }
    }
    BlockedKey {
        key: key.to_string(),
        upstream: blocking_failure(f.dag, base, f.failed_bases)
            .unwrap_or(base)
            .to_string(),
        failed: true,
    }
}

/// The report of one blocked key.
pub(super) fn report(dag: &Dag, base: &str, b: &BlockedKey) -> StepReport {
    StepReport {
        id: format!("{base}[{}]", b.key),
        kind: kind_str(dag.get_node(base).map(|n| n.kind())),
        status: Some("skipped".to_string()),
        reason: Some("upstream_key_failed".to_string()),
        detail: Some(format!(
            "depends on '{}', which {}",
            short_key(&b.upstream),
            if b.failed { "failed" } else { "was skipped" }
        )),
        ..Default::default()
    }
}

/// `pipeline.py:a[k=1]` -> `a[k=1]`.
fn short_key(id: &str) -> String {
    match id.find('[') {
        Some(i) => format!("{}{}", short_name(id), &id[i..]),
        None => short_name(id).to_string(),
    }
}

/// Drop the blocked keys of `base` from the static keys of its steps in `phase`.
pub(super) fn drop_keys(phase: &mut Phase, base: &str, keys: &HashSet<String>) {
    for step in phase
        .streams
        .iter_mut()
        .flat_map(|s| &mut s.steps)
        .filter(|st| st.step_id.base_id() == base)
    {
        step.partition_keys
            .retain(|key| !keys.contains(&key.suffix()));
    }
}

/// `expand_pending_partitions_filtered`'s key filter: leave the blocked keys of `base` out of
/// the values a `partitions_from` source gives it (one dimension: the inherited one).
pub(super) fn filter_dims(blocked: &Keys, base: &str, dims: &mut HashMap<String, Vec<String>>) {
    let Some(keys) = blocked.get(base).filter(|k| !k.is_empty()) else {
        return;
    };
    for (dim, values) in dims.iter_mut() {
        values.retain(|v| !keys.contains(&format!("{dim}={v}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dag() -> Dag {
        let src = r#"
from barca import asset, collect, partitions, partitions_from

@asset()
def accounts() -> list:
    return ["a", "b", "c"]

@asset(partitions={"account": partitions_from(accounts)})
def forecast(account: str) -> dict:
    return {}

@asset(partitions={"account": partitions_from(forecast)})
def promo(account: str, forecast: dict) -> dict:
    return {}

@asset(partitions={"account": partitions_from(promo)})
def orders(account: str, promo: dict) -> dict:
    return {}

@asset(inputs={"orders": collect(orders)})
def summary(orders: list) -> dict:
    return {}

@asset(inputs={"forecast": collect(forecast)})
def whole(forecast: list) -> dict:
    return {}

@asset(partitions={"region": partitions(["x"])}, inputs={"forecast": forecast})
def other_dim(region: str, forecast: dict) -> dict:
    return {}
"#;
        Dag::build(&crate::parse::extract_nodes(src, "p.py").unwrap()).unwrap()
    }

    fn keys(base: &str, keys: &[&str]) -> Keys {
        HashMap::from([(
            base.to_string(),
            keys.iter().map(|k| k.to_string()).collect(),
        )])
    }

    #[test]
    fn a_key_for_key_consumer_loses_only_the_failed_keys_and_a_fan_in_is_whole() {
        let dag = dag();
        let failed_bases: HashSet<String> = ["p.py:forecast".to_string()].into();
        let failed_keys = keys("p.py:forecast", &["account=b"]);
        let none = Keys::new();
        let skipped = HashSet::new();
        let f = Failures {
            dag: &dag,
            failed_bases: &failed_bases,
            failed_keys: &failed_keys,
            blocked_keys: &none,
            skipped: &skipped,
        };
        let dropped = HashSet::new();
        assert_eq!(
            how(&f, "p.py:promo", &dropped),
            Blocked::Keys(vec![BlockedKey {
                key: "account=b".into(),
                upstream: "p.py:forecast[account=b]".into(),
                failed: true,
            }])
        );
        // An unpartitioned reader, and one on another dimension, read every key.
        assert_eq!(how(&f, "p.py:whole", &dropped), Blocked::Whole);
        assert_eq!(how(&f, "p.py:other_dim", &dropped), Blocked::Whole);

        // Below the narrowed consumer: the blocked key carries, and names the step skipped.
        let blocked = keys("p.py:promo", &["account=b"]);
        let f = Failures {
            blocked_keys: &blocked,
            ..f
        };
        assert_eq!(
            how(&f, "p.py:orders", &dropped),
            Blocked::Keys(vec![BlockedKey {
                key: "account=b".into(),
                upstream: "p.py:promo[account=b]".into(),
                failed: false,
            }])
        );
        let r = report(
            &dag,
            "p.py:orders",
            &how_keys(&f, "p.py:orders", &dropped)[0],
        );
        assert_eq!(r.id, "p.py:orders[account=b]");
        assert_eq!(r.reason.as_deref(), Some("upstream_key_failed"));
        assert_eq!(
            r.detail.as_deref(),
            Some("depends on 'promo[account=b]', which was skipped")
        );
        let blocked = keys("p.py:orders", &["account=b"]);
        let f = Failures {
            blocked_keys: &blocked,
            ..f
        };
        assert_eq!(how(&f, "p.py:summary", &dropped), Blocked::Whole);
    }

    fn how_keys(f: &Failures<'_>, base: &str, dropped: &HashSet<String>) -> Vec<BlockedKey> {
        match how(f, base, dropped) {
            Blocked::Keys(k) => k,
            Blocked::Whole => panic!("{base} blocked whole"),
        }
    }

    #[test]
    fn a_failure_without_keys_or_a_whole_skipped_input_blocks_whole() {
        let dag = dag();
        let failed_bases: HashSet<String> = ["p.py:forecast".to_string()].into();
        let none = Keys::new();
        let skipped = HashSet::new();
        let f = Failures {
            dag: &dag,
            failed_bases: &failed_bases,
            failed_keys: &none,
            blocked_keys: &none,
            skipped: &skipped,
        };
        assert_eq!(how(&f, "p.py:promo", &HashSet::new()), Blocked::Whole);
        let failed_keys = keys("p.py:forecast", &["account=b"]);
        let f = Failures {
            failed_keys: &failed_keys,
            ..f
        };
        let dropped: HashSet<String> = ["p.py:promo".to_string()].into();
        assert_eq!(how(&f, "p.py:orders", &dropped), Blocked::Whole);
    }

    #[test]
    fn the_key_filters_drop_exactly_the_blocked_keys() {
        let blocked = keys("p.py:promo", &["account=b"]);
        let mut dims = HashMap::from([(
            "account".to_string(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        )]);
        filter_dims(&blocked, "p.py:promo", &mut dims);
        assert_eq!(dims["account"], ["a", "c"]);
        filter_dims(&blocked, "p.py:other", &mut dims);
        assert_eq!(dims["account"], ["a", "c"]);
        assert_eq!(short_key("p.py:a[k=1]"), "a[k=1]");
    }
}
