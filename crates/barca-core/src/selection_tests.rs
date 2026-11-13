//! Unit tests of `barca_compile::selection` (#905 step 4: moved up from barca-compile, because
//! they narrow the plan to a target's cone with `crate::queries::filter_plan_to_subgraph`, which
//! is core's).

use crate::BarcaError;
use crate::dag::Dag;
use crate::model::PartitionKey;
use crate::planner::ExecutionPlan;
use crate::selection::*;
use std::collections::BTreeMap;

fn key(pairs: &[(&str, &str)]) -> PartitionKey {
    PartitionKey(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

use crate::planner::{ResourceConfig, plan_from_dag_narrowed};

const SRC: &str = r#"
from barca import asset, collect, partitions, partitions_from, task

@asset()
def rate():
return 2

@asset(inputs={"rate": rate}, partitions={"retailer": partitions(["r1", "r2", "r3", "r4"])})
def orders(retailer, rate):
return 1

@asset(partitions={"retailer": partitions_from(orders)})
def revenue(retailer, orders):
return 1

@asset(partitions={"retailer": partitions(["r1", "r2", "r9"])})
def returns(retailer):
return 1

@asset(partitions={"day": partitions(["d1", "d2"])})
def calendar(day):
return 1

@asset(inputs={"calendar": calendar}, partitions={"retailer": partitions_from(revenue)})
def daily(retailer, revenue, calendar):
return 1

@asset(inputs={"all": collect(revenue)})
def total(all):
return 1

@asset(inputs={"total": total})
def report(total):
return 1

@asset()
def names():
return ["r1", "r2"]

@asset(partitions={"retailer": partitions_from(names)})
def late(retailer):
return 1

@task(partitions={"retailer": partitions_from(orders)})
def notify(retailer, orders):
return None
"#;

fn dag() -> Dag {
    Dag::build(&crate::parse::extract_nodes(SRC, "p.py").unwrap()).unwrap()
}

fn plan_of(dag: &Dag, sel: Option<&PartitionSelection>) -> ExecutionPlan {
    let config = ResourceConfig {
        pool_size: 2,
        concurrency_groups: Default::default(),
    };
    plan_from_dag_narrowed(dag, &config, sel)
}

/// Every planned step's display id, one per key, sorted.
fn planned(plan: &ExecutionPlan) -> Vec<String> {
    let mut ids: Vec<String> = plan
        .phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
        .flat_map(|st| {
            if st.partition_keys.is_empty() {
                vec![st.step_id.display()]
            } else {
                st.partition_keys
                    .iter()
                    .map(|k| k.display_id(st.step_id.base_id()))
                    .collect()
            }
        })
        .collect();
    ids.sort();
    ids
}

fn apply(raw: &str, targets: &[&str]) -> Result<(ExecutionPlan, Narrowing), BarcaError> {
    let dag = dag();
    let sel = PartitionSelection::parse(raw).unwrap();
    let mut plan = plan_of(&dag, Some(&sel));
    if !targets.is_empty() {
        plan = crate::queries::filter_plan_to_subgraph(plan, &dag.subgraph_many(targets));
    }
    let n = sel.apply(&dag, &mut plan, targets)?;
    Ok((plan, n))
}

#[test]
fn a_selection_narrows_every_node_with_its_dimension() {
    let (plan, n) = apply("retailer=r1,r2", &[]).unwrap();
    let ids = planned(&plan);
    for id in [
        "p.py:orders[retailer=r1]",
        "p.py:orders[retailer=r2]",
        "p.py:revenue[retailer=r2]",
        "p.py:returns[retailer=r1]",
        "p.py:daily[retailer=r1]",
        "p.py:late",
        "p.py:notify[retailer=r2]",
    ] {
        assert!(ids.iter().any(|i| i == id), "{id} in {ids:?}");
    }
    assert!(
        !ids.iter().any(|i| i.contains("r3") || i.contains("r9")),
        "{ids:?}"
    );
    // The upstream on another dimension keeps every key; an unpartitioned input runs.
    assert!(ids.contains(&"p.py:calendar[day=d1]".to_string()));
    assert!(ids.contains(&"p.py:calendar[day=d2]".to_string()));
    assert!(ids.contains(&"p.py:rate".to_string()));
    assert_eq!(
        n.nodes.get("p.py:orders").copied().flatten(),
        Some(KeyCounts {
            selected: 2,
            skipped: 2
        })
    );
    assert_eq!(
        n.nodes.get("p.py:returns").copied().flatten(),
        Some(KeyCounts {
            selected: 2,
            skipped: 1
        })
    );
    // Keys of a list known only after it ran: counted later.
    assert_eq!(n.nodes.get("p.py:late"), Some(&None));
    assert!(!n.nodes.contains_key("p.py:calendar"));
    assert_eq!(plan.total_steps, ids.len());
}

#[test]
fn a_fan_in_over_selected_keys_and_what_is_below_it_are_skipped() {
    let (plan, n) = apply("retailer=r1", &[]).unwrap();
    let ids = planned(&plan);
    assert!(!ids.iter().any(|i| i == "p.py:total" || i == "p.py:report"));
    let skipped: Vec<(&str, bool)> = n
        .skipped
        .iter()
        .map(|s| (s.id.as_str(), s.fan_in))
        .collect();
    assert_eq!(skipped, [("p.py:total", true), ("p.py:report", false)]);
    assert!(n.skipped[0].detail.contains("reads every key of 'revenue'"));
    assert!(n.skipped[1].detail.contains("depends on 'total'"));
    let note = n.note().unwrap();
    assert_eq!(
        note,
        "[barca] note: --partition skips a fan-in that reads every key: total, and 1 node \
         below it. Run without --partition to update them."
    );
    let reports = n.skipped_reports(&dag(), false);
    assert_eq!(reports[0].reason.as_deref(), Some("partition_selection"));
    assert_eq!(reports[0].status.as_deref(), Some("skipped"));
}

#[test]
fn a_node_reading_selected_keys_across_dimensions_is_a_fan_in() {
    // `daily` (retailer) reads `calendar` (day): selecting days makes it read some days only.
    let (plan, n) = apply("day=d1", &[]).unwrap();
    let ids = planned(&plan);
    assert!(ids.contains(&"p.py:calendar[day=d1]".to_string()));
    assert!(n.skipped.iter().any(|s| s.id == "p.py:daily" && s.fan_in));
    // Nothing on the other dimension is narrowed.
    assert_eq!(
        ids.iter().filter(|i| i.starts_with("p.py:orders[")).count(),
        4
    );
}

#[test]
fn a_target_the_selection_skips_is_a_usage_error() {
    let err = apply("retailer=r1", &["p.py:total"])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("'total' reads every key of 'revenue'"),
        "{err}"
    );
    let err = apply("retailer=r1", &["p.py:report"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("depends on 'total'"), "{err}");
}

#[test]
fn unknown_values_and_dimensions_are_usage_errors() {
    // `late`'s keys are not known yet, so the value may be one of them: filtered later.
    assert!(apply("retailer=r7", &[]).is_ok());
    let err = apply("region=emea", &[]).unwrap_err().to_string();
    assert!(
        err.contains("dimension 'region'") && err.contains("day, retailer"),
        "{err}"
    );
    // Within the cone of `revenue` the keys are all known.
    let err = apply("retailer=r7", &["p.py:revenue"])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("retailer=r7") && err.contains("r1, r2, r3, r4"),
        "{err}"
    );
    // A value of another node with the dimension is a key of this run.
    assert!(apply("retailer=r9", &["p.py:revenue"]).is_err());
    let (plan, _) = apply("retailer=r9", &[]).unwrap();
    assert_eq!(
        planned(&plan)
            .iter()
            .filter(|i| i.contains("retailer="))
            .collect::<Vec<_>>(),
        ["p.py:returns[retailer=r9]"]
    );
}

#[test]
fn the_planner_builds_only_the_selected_keys() {
    let dag = dag();
    let sel = PartitionSelection::parse("retailer=r2").unwrap();
    let plan = plan_of(&dag, Some(&sel));
    let orders: Vec<String> = planned(&plan)
        .into_iter()
        .filter(|i| i.starts_with("p.py:orders["))
        .collect();
    assert_eq!(orders, ["p.py:orders[retailer=r2]"]);
    // A node with none of the selected values is built whole, and `apply` removes it.
    let sel = PartitionSelection::parse("retailer=r9").unwrap();
    let mut plan = plan_of(&dag, Some(&sel));
    assert_eq!(
        planned(&plan)
            .iter()
            .filter(|i| i.starts_with("p.py:orders["))
            .count(),
        4
    );
    let n = sel.apply(&dag, &mut plan, &[]).unwrap();
    assert!(!planned(&plan).iter().any(|i| i.starts_with("p.py:orders[")));
    assert_eq!(
        n.nodes.get("p.py:orders").copied().flatten(),
        Some(KeyCounts {
            selected: 0,
            skipped: 4
        })
    );
}

#[test]
fn keys_known_late_are_narrowed_once_expanded() {
    let mut n = Narrowing {
        selection: PartitionSelection::parse("retailer=r2").unwrap(),
        nodes: BTreeMap::from([("p.py:late".to_string(), None)]),
        skipped: Vec::new(),
    };
    let dag = dag();
    let plan = plan_of(&dag, None);
    let mut phase = plan
        .phases
        .iter()
        .find(|p| {
            p.streams
                .iter()
                .flat_map(|s| &s.steps)
                .any(|s| s.step_id.base_id() == "p.py:late")
        })
        .unwrap()
        .clone();
    for st in phase.streams.iter_mut().flat_map(|s| &mut s.steps) {
        if st.step_id.base_id() == "p.py:late" {
            st.pending_partitions.clear();
            st.partition_keys = vec![key(&[("retailer", "r1")]), key(&[("retailer", "r2")])];
        }
    }
    n.narrow_expanded(&mut phase);
    let late: Vec<&PartitionKey> = phase
        .streams
        .iter()
        .flat_map(|s| &s.steps)
        .filter(|s| s.step_id.base_id() == "p.py:late")
        .flat_map(|s| &s.partition_keys)
        .collect();
    assert_eq!(late, [&key(&[("retailer", "r2")])]);
    assert_eq!(
        n.nodes["p.py:late"],
        Some(KeyCounts {
            selected: 1,
            skipped: 1
        })
    );
}

#[test]
fn refresh_of_a_skipped_node_is_a_usage_error() {
    let (_, n) = apply("retailer=r1", &[]).unwrap();
    assert!(n.check_refresh(&["orders".to_string()]).is_ok());
    let err = n.check_refresh(&["total".to_string()]).unwrap_err();
    assert!(
        err.to_string().contains("--partition skips 'total'"),
        "{err}"
    );
}

#[test]
fn parses_one_dimension_with_several_values() {
    let s = PartitionSelection::parse("region=emea,amer").unwrap();
    assert!(s.matches(&key(&[("region", "emea")])));
    assert!(s.matches(&key(&[("region", "amer")])));
    assert!(!s.matches(&key(&[("region", "apac")])));
}

#[test]
fn a_json_object_selects_compound_keys() {
    let s = PartitionSelection::parse(r#"{"region": "emea", "day": ["d1", "d2"]}"#).unwrap();
    assert!(s.matches(&key(&[("region", "emea"), ("day", "d2")])));
    assert!(!s.matches(&key(&[("region", "emea"), ("day", "d3")])));
    assert!(!s.matches(&key(&[("region", "amer"), ("day", "d1")])));
    // A dimension the key does not have is not checked: the node's own dimensions are.
    assert!(s.matches(&key(&[("region", "emea")])));
    assert!(!s.matches(&key(&[("region", "amer")])));
    let n = PartitionSelection::parse(r#"{"n": 3}"#).unwrap();
    assert!(n.matches(&key(&[("n", "3")])));
}

#[test]
fn exact_keys_preserve_pairs_and_project_onto_upstream_dimensions() {
    let s =
        PartitionSelection::parse(r#"[{"day":"d1","region":"east"},{"day":"d2","region":"west"}]"#)
            .unwrap();
    assert!(s.matches(&key(&[("day", "d1"), ("region", "east")])));
    assert!(!s.matches(&key(&[("day", "d1"), ("region", "west")])));
    assert!(s.matches(&key(&[("day", "d2")])));
    assert!(s.matches(&key(&[("unrelated", "x")])));
    assert_eq!(
        PartitionSelection::parse(&s.to_json().to_string()).unwrap(),
        s
    );
    for bad in [
        "[]",
        "[{}]",
        r#"[{"day":["d1"]}]"#,
        r#"[{"day":"d1"},{"region":"east"}]"#,
    ] {
        assert!(PartitionSelection::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn malformed_selections_name_the_accepted_forms() {
    for bad in [
        "region",
        "=emea",
        "region=",
        "{\"region\": null}",
        "[1]",
        "{}",
    ] {
        let err = PartitionSelection::parse(bad).unwrap_err();
        assert!(err.contains("--partition"), "{bad}: {err}");
        assert!(err.contains("region=emea,amer"), "{bad}: {err}");
    }
}
