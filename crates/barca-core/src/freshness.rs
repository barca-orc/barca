//! Freshness at run time (RFC-0008): what `Always` and `Manual` do.
//!
//! - A scheduled tick brings its node up to date and then, in the same run, every `Always` node
//!   reachable from it going downstream through `Always` nodes only ([`followers`]).
//! - `Manual` pins itself: a `Manual` asset or task that a run reads but was not asked for is
//!   served from its latest successful result, not recomputed ([`walk`], [`load_pins`]). One
//!   that has never been computed blocks what reads it.
//! - Under a tick, a task other than the scheduled one fires only when it is out of date: when
//!   its last successful run used other versions of its inputs, or a task it reads has run
//!   successfully since ([`crate::cache::decide_step`]).

use crate::cache::{CachePolicy, compute_run_hash_with};
use crate::dag::Dag;
use crate::db;
use crate::dispatch::OutputRef;
use crate::targets::{refresh_name_matches, short_name};
use crate::{Freshness, NodeKind};
use std::collections::{HashMap, HashSet, VecDeque};

/// `Schedule(cron)` without `max_failures=`: consecutive failed ticks before the schedule is
/// skipped and reported `failing`.
pub const DEFAULT_MAX_FAILURES: u32 = 5;

/// The freshness rules a run follows, besides its cache policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunRules {
    /// Read a `Manual` asset or task that is not a target and not named in `--refresh` as it
    /// is, instead of recomputing it (`barca get`, `barca run` and ticks).
    pub pin_manual: bool,
    /// A tick of this scheduled node: every other task fires only when it is out of date.
    pub tick: Option<String>,
}

impl RunRules {
    /// `barca get` / `barca run`: a stale `Manual` upstream is read as it is.
    pub fn request() -> Self {
        RunRules {
            pin_manual: true,
            tick: None,
        }
    }

    /// A tick of `scheduled` under `barca serve`.
    pub fn tick(scheduled: &str) -> Self {
        RunRules {
            pin_manual: true,
            tick: Some(scheduled.to_string()),
        }
    }
}

/// The nodes a tick of `scheduled` reconciles besides itself: every `Always` node below it, in
/// dependency order. The walk goes on past a `Manual` node, which is not reconciled (it is
/// refreshed by hand) but does not wall off what is below it: that is reconciled against the
/// `Manual` node's current result, so it is recomputed (or fires) once the `Manual` node is
/// refreshed. A `Schedule` node stops the walk: it is updated on its own ticks. It depends on
/// the DAG alone, so it is computed when the DAG is loaded.
pub fn followers(dag: &Dag, scheduled: &str) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut reached: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([scheduled]);
    while let Some(id) = queue.pop_front() {
        for down in dag.downstream(id) {
            let Some(node) = dag.get_node(down) else {
                continue;
            };
            if down == scheduled || !seen.insert(down) {
                continue;
            }
            match node.extracted.freshness {
                Freshness::Always => {
                    reached.insert(down);
                    queue.push_back(down);
                }
                Freshness::Manual if node.kind() != NodeKind::Sensor => queue.push_back(down),
                Freshness::Manual | Freshness::Schedule(_) => {}
            }
        }
    }
    dag.order_for(reached.iter().copied())
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Whether `id` pins itself: a `Manual` asset or task. Sensors are polled whenever a run needs
/// them, whatever their freshness. A partitioned node is not pinned (it is computed as before).
fn pins_itself(dag: &Dag, id: &str) -> bool {
    dag.get_node(id).is_some_and(|n| {
        n.extracted.freshness == Freshness::Manual
            && matches!(n.kind(), NodeKind::Asset | NodeKind::Task)
            && n.extracted.partitions.is_empty()
    })
}

/// Whether `policy` recomputes `id` whatever its cache (`--refresh-all`, or named in
/// `--refresh`).
fn refreshed(policy: &CachePolicy, id: &str) -> bool {
    match policy {
        CachePolicy::CacheAware => false,
        CachePolicy::RefreshAll => true,
        CachePolicy::RefreshSelective { names, .. } => {
            names.iter().any(|name| refresh_name_matches(id, name))
        }
    }
}

/// What a request for `targets` reads: its cone, walking upstream from the targets but not past
/// a `Manual` node that is not named in `--refresh` (`stops`, which are in the cone). A `Manual`
/// asset is read as it is even when it is the target: only `--refresh` (or `--refresh-all`)
/// recomputes it. A `Manual` task named as the target of `barca run` runs: that is how a task is
/// refreshed by hand (`--refresh` names assets only).
pub(crate) struct Walk<'a> {
    /// Every node the run plans, in dependency order.
    pub(crate) cone: Vec<&'a str>,
    /// The `Manual` nodes read as they are.
    pub(crate) stops: Vec<String>,
}

pub(crate) fn walk<'a>(dag: &'a Dag, targets: &[&str], policy: &CachePolicy) -> Walk<'a> {
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stops: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for id in targets {
        if let Some(node) = dag.get_node(id)
            && visited.insert(node.id.as_str())
        {
            queue.push_back(node.id.as_str());
        }
    }
    while let Some(id) = queue.pop_front() {
        let task_target =
            targets.contains(&id) && dag.get_node(id).is_some_and(|n| n.kind() == NodeKind::Task);
        if !task_target && pins_itself(dag, id) && !refreshed(policy, id) {
            stops.insert(id);
            continue;
        }
        for up in dag.upstream(id) {
            if visited.insert(up) {
                queue.push_back(up);
            }
        }
    }
    let order = dag.order_for(visited.iter().copied());
    Walk {
        cone: order
            .iter()
            .copied()
            .filter(|id| visited.contains(id))
            .collect(),
        stops: order
            .iter()
            .filter(|id| stops.contains(*id))
            .map(|id| id.to_string())
            .collect(),
    }
}

/// A `Manual` node read as it is: its latest successful result.
#[derive(Debug, Clone)]
pub(crate) struct Pin {
    /// The run hash of that result. Nodes below hash it and the result's content hash
    /// (`output`), so a refresh of the `Manual` node that gives new bytes changes their run
    /// hashes and they are recomputed (#548).
    pub(crate) run_hash: String,
    pub(crate) output: OutputRef,
    /// Its inputs' latest results or its code differ from the ones it was computed with.
    pub(crate) stale: bool,
}

/// The `Manual` nodes of a [`Walk`], with what each is read as.
#[derive(Debug, Default)]
pub(crate) struct Pins {
    pub(crate) pinned: HashMap<String, Pin>,
    /// `Manual` nodes that have never been computed: nothing below them can run.
    pub(crate) never: Vec<String>,
}

/// Look up each stop's latest successful result. `cache` is `None` when there is no metadata
/// database yet (nothing was ever computed).
pub(crate) async fn load_pins(
    cache: Option<&db::CacheReader>,
    dag: &Dag,
    stops: &[String],
) -> Pins {
    let mut pins = Pins::default();
    for id in stops {
        let latest = match cache {
            Some(cache) => db::latest_success(cache, id).await,
            None => None,
        };
        match (cache, latest) {
            (Some(cache), Some(row)) => {
                let none = HashMap::new();
                let stale = is_stale(cache, dag, id, &row.run_hash, &none, &none).await;
                pins.pinned.insert(
                    id.clone(),
                    Pin {
                        run_hash: row.run_hash,
                        output: row.output,
                        stale,
                    },
                );
            }
            _ => pins.never.push(id.clone()),
        }
    }
    pins
}

/// Whether the `Manual` node `id`, last computed with `run_hash`, would hash differently against
/// its inputs' current versions and its current code. An input's version is its run hash in
/// `run_hashes` (and a sensor's output in `outputs`) when this run computed or polled it, else
/// its latest recorded result. `false` when that cannot be told (an input is partitioned or has
/// no result).
pub(crate) async fn is_stale(
    cache: &db::CacheReader,
    dag: &Dag,
    id: &str,
    run_hash: &str,
    run_hashes: &HashMap<String, String>,
    outputs: &HashMap<String, String>,
) -> bool {
    let Some(node) = dag.get_node(id) else {
        return false;
    };
    let inputs: Vec<&String> = node
        .resolved_inputs
        .values()
        .chain(node.resolved_collected.values())
        .collect();
    let mut hashes: HashMap<String, String> = HashMap::new();
    let mut sensors: Vec<&str> = Vec::new();
    for up in &inputs {
        let Some(up_node) = dag.get_node(up) else {
            return false;
        };
        if !up_node.extracted.partitions.is_empty() {
            return false;
        }
        match run_hashes.get(up.as_str()) {
            Some(h) => {
                hashes.insert(up.to_string(), h.clone());
            }
            None => {
                let Some(latest) = db::latest_success(cache, up).await else {
                    return false;
                };
                hashes.insert(up.to_string(), latest.run_hash);
            }
        }
        if crate::cache::folds_output(dag, up) && !outputs.contains_key(up.as_str()) {
            sensors.push(up.as_str());
        }
    }
    let mut outputs = outputs.clone();
    outputs.extend(
        db::last_output_hashes(cache, &sensors)
            .await
            .unwrap_or_default(),
    );
    let env = crate::envdeps::hash_input(&crate::envdeps::resolve(&node.extracted.env));
    let current = compute_run_hash_with(
        &node.definition_hash,
        None,
        inputs.iter(),
        &hashes,
        &outputs,
        env.as_deref(),
        Vec::new(),
        Some(dag),
    );
    current != run_hash
}

/// The `Manual` node that blocks `target`: one in its walk that has never been computed.
pub(crate) fn blocked_on(
    dag: &Dag,
    target: &str,
    policy: &CachePolicy,
    never: &[String],
) -> Option<String> {
    if never.is_empty() {
        return None;
    }
    walk(dag, &[target], policy)
        .stops
        .into_iter()
        .find(|stop| never.contains(stop))
}

/// The command that recomputes the `Manual` node `id`.
pub(crate) fn refresh_command(dag: &Dag, id: &str, file_args: &[String]) -> String {
    let name = short_name(id);
    let files = file_args.join(" ");
    let files = if files.is_empty() {
        String::new()
    } else {
        format!(" {files}")
    };
    match dag.get_node(id).map(|n| n.kind()) {
        Some(NodeKind::Task) => format!("barca run {name}{files}"),
        _ => format!("barca get {name}{files} --refresh {name}"),
    }
}

/// The note printed when a request reads a stale `Manual` node as it is.
pub(crate) fn stale_note(dag: &Dag, id: &str, file_args: &[String]) -> String {
    format!(
        "[barca] note: {} is Manual and stale; {} to recompute",
        short_name(id),
        refresh_command(dag, id, file_args)
    )
}

/// The usage error for a request that reads a `Manual` node that has never been computed.
pub(crate) fn never_computed(dag: &Dag, id: &str, file_args: &[String]) -> crate::BarcaError {
    crate::BarcaError::Usage(format!(
        "'{}' is Manual and has never been computed; a Manual node is computed only when \
         named in --refresh\nCompute it first: {}",
        short_name(id),
        refresh_command(dag, id, file_args)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node: `kind` is `a`sset, `s`ensor or `t`ask; `fresh` is `always`, `manual` or a cron.
    fn node(name: &str, kind: char, fresh: &str, inputs: &[&str]) -> String {
        let decorator = match kind {
            's' => "sensor",
            't' => "task",
            _ => "asset",
        };
        let freshness = match fresh {
            "always" => "Always".to_string(),
            "manual" => "Manual".to_string(),
            cron => format!("Schedule({cron:?})"),
        };
        let inputs: Vec<String> = inputs.iter().map(|up| format!("{up:?}: {up}")).collect();
        let params = inputs
            .iter()
            .map(|i| i.split(':').next().unwrap().trim_matches('"').to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "@{decorator}(inputs={{{}}}, freshness={freshness})\ndef {name}({params}):\n    return 1\n",
            inputs.join(", ")
        )
    }

    fn dag(nodes: &[String]) -> Dag {
        let src = format!(
            "from barca import asset, sensor, task, Always, Manual, Schedule\n{}",
            nodes.join("")
        );
        Dag::build(&crate::parse::extract_nodes(&src, "p.py").unwrap()).expect("valid dag")
    }

    fn names(ids: &[String]) -> Vec<&str> {
        ids.iter().map(|id| short_name(id)).collect()
    }

    fn id(dag: &Dag, name: &str) -> String {
        dag.topo_order()
            .into_iter()
            .find(|id| short_name(id) == name)
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_chain_of_always_nodes_follows_the_scheduled_node() {
        let d = dag(&[
            node("inbox", 's', "*/5 * * * *", &[]),
            node("orders", 'a', "always", &["inbox"]),
            node("summary", 'a', "always", &["orders"]),
            node("report", 't', "always", &["orders"]),
        ]);
        let f = followers(&d, &id(&d, "inbox"));
        assert_eq!(names(&f)[0], "orders", "upstream first: {f:?}");
        let mut all = names(&f);
        all.sort();
        assert_eq!(all, ["orders", "report", "summary"]);
    }

    #[test]
    fn a_diamond_is_reached_once() {
        let d = dag(&[
            node("src", 's', "* * * * *", &[]),
            node("left", 'a', "always", &["src"]),
            node("right", 'a', "always", &["src"]),
            node("join", 'a', "always", &["left", "right"]),
        ]);
        let f = followers(&d, &id(&d, "src"));
        assert_eq!(f.len(), 3);
        assert_eq!(short_name(f.last().unwrap()), "join");
    }

    #[test]
    fn a_schedule_in_the_middle_stops_the_walk_and_a_manual_node_is_not_reconciled() {
        let d = dag(&[
            node("src", 's', "* * * * *", &[]),
            node("model", 'a', "manual", &["src"]),
            node("scored", 'a', "always", &["model"]),
            node("hourly", 'a', "0 * * * *", &["src"]),
            node("after_hourly", 'a', "always", &["hourly"]),
        ]);
        // `model` is not reconciled; `scored` below it is, against its current result.
        assert_eq!(names(&followers(&d, &id(&d, "src"))), ["scored"]);
        assert_eq!(
            names(&followers(&d, &id(&d, "hourly"))),
            ["after_hourly"],
            "a scheduled node in the middle has its own followers"
        );
    }

    #[test]
    fn what_is_below_a_manual_node_is_reconciled_by_a_tick_above_it() {
        // RFC-0008 section 3.3: `report` and `publish` are checked at each tick of `prices`;
        // whether they run is the up-to-date rule, not reachability.
        let d = dag(&[
            node("prices", 's', "*/5 * * * *", &[]),
            node("baseline", 'a', "manual", &["prices"]),
            node("report", 'a', "always", &["baseline", "prices"]),
            node("publish", 't', "always", &["baseline"]),
        ]);
        let found = followers(&d, &id(&d, "prices"));
        let mut f = names(&found);
        f.sort();
        assert_eq!(f, ["publish", "report"]);
    }

    #[test]
    fn a_node_under_two_scheduled_nodes_follows_each() {
        let d = dag(&[
            node("a", 's', "* * * * *", &[]),
            node("b", 's', "0 * * * *", &[]),
            node("both", 'a', "always", &["a", "b"]),
        ]);
        assert_eq!(names(&followers(&d, &id(&d, "a"))), ["both"]);
        assert_eq!(names(&followers(&d, &id(&d, "b"))), ["both"]);
    }

    #[test]
    fn a_node_under_no_scheduled_node_follows_nothing() {
        let d = dag(&[
            node("raw", 'a', "always", &[]),
            node("clean", 'a', "always", &["raw"]),
            node("tick", 's', "* * * * *", &[]),
        ]);
        let all: Vec<String> = ["raw", "clean", "tick"]
            .iter()
            .flat_map(|n| followers(&d, &id(&d, n)))
            .collect();
        // Only an unscheduled node's own walk reaches `clean`, and the scheduler never walks
        // from an unscheduled node.
        assert_eq!(names(&followers(&d, &id(&d, "tick"))), Vec::<&str>::new());
        assert_eq!(names(&all), ["clean"]);
    }

    #[test]
    fn the_walk_stops_at_a_manual_node_target_or_not_unless_refreshed() {
        let d = dag(&[
            node("raw", 'a', "always", &[]),
            node("model", 'a', "manual", &["raw"]),
            node("scored", 'a', "always", &["model"]),
        ]);
        let scored = id(&d, "scored");
        let w = walk(&d, &[&scored], &CachePolicy::CacheAware);
        assert_eq!(names(&w.stops), ["model"]);
        assert_eq!(w.cone.len(), 2, "raw is not planned: {:?}", w.cone);

        // `barca get model` without `--refresh model` reads it as it is too.
        let model = id(&d, "model");
        let w = walk(&d, &[&model], &CachePolicy::CacheAware);
        assert_eq!(names(&w.stops), ["model"]);
        assert_eq!(w.cone.len(), 1);
        let named = CachePolicy::RefreshSelective {
            names: vec!["model".into()],
            cascade: true,
        };
        let w = walk(&d, &[&model], &named);
        assert!(w.stops.is_empty());
        assert_eq!(w.cone.len(), 2);
    }

    #[test]
    fn a_manual_task_named_as_the_target_runs() {
        let d = dag(&[
            node("raw", 'a', "always", &[]),
            node("ship", 't', "manual", &["raw"]),
        ]);
        let ship = id(&d, "ship");
        let w = walk(&d, &[&ship], &CachePolicy::CacheAware);
        assert!(w.stops.is_empty());
        assert_eq!(w.cone.len(), 2);
    }

    #[test]
    fn refresh_of_the_manual_node_recomputes_it_and_its_cone() {
        let d = dag(&[
            node("raw", 'a', "always", &[]),
            node("model", 'a', "manual", &["raw"]),
            node("scored", 'a', "always", &["model"]),
        ]);
        let scored = id(&d, "scored");
        for policy in [
            CachePolicy::RefreshAll,
            CachePolicy::RefreshSelective {
                names: vec!["model".into()],
                cascade: true,
            },
        ] {
            let w = walk(&d, &[&scored], &policy);
            assert!(w.stops.is_empty());
            assert_eq!(w.cone.len(), 3);
        }
    }

    #[test]
    fn sensors_are_never_pinned() {
        // Sensors default to Manual; a run that needs one polls it.
        let d = dag(&[
            node("poll", 's', "manual", &[]),
            node("use", 'a', "always", &["poll"]),
        ]);
        let target = id(&d, "use");
        let w = walk(&d, &[&target], &CachePolicy::CacheAware);
        assert!(w.stops.is_empty());
        assert_eq!(w.cone.len(), 2);
    }

    #[test]
    fn a_never_computed_manual_node_blocks_only_what_reads_it() {
        let d = dag(&[
            node("src", 's', "* * * * *", &[]),
            node("model", 'a', "manual", &[]),
            node("scored", 'a', "always", &["model", "src"]),
            node("count", 'a', "always", &["src"]),
        ]);
        let never = vec![id(&d, "model")];
        let policy = CachePolicy::CacheAware;
        assert_eq!(
            blocked_on(&d, &id(&d, "scored"), &policy, &never).as_deref(),
            Some(never[0].as_str())
        );
        assert_eq!(blocked_on(&d, &id(&d, "count"), &policy, &never), None);
    }
}
