//! Why a node is not fresh and what explains it (`specs/behavior/health.md`, "Cause" and
//! "Root"; #963, #973): the `cause` and `root` of every row of one status result, and its
//! `gap_seconds`.
//!
//! [`attribute`] is called by [`crate::health::classify`], once per status result, over rows
//! that already carry their health, refresh path and times. It reads no database: one pass
//! over the rows in dependency order, so a node finds what is above it already answered.
//!
//! The cause is the first matching row of the spec's table, in this order: `failing`,
//! `undecided`, `catching_up` (a running run computes it), `overdue`, `queued`,
//! `no_refresh_path`, `catching_up` (the next tick will).
//!
//! The root is the upstream fact that explains the node. Attribution walks up from the node
//! through inputs that are not fresh and stops at the first node whose own evidence explains
//! it: a failure, changed code, a changed environment variable, an input or a sensor whose
//! result is newer than what the node read. A node with a cause the walk does not explain
//! (nothing refreshes it, its schedule's ticks wait for a run slot) is rooted at the schedule
//! or at the point where one would have to be added; so is an undecided node no tick will
//! reach, which only a run decides.

use crate::estimates::PlannedRun;
use crate::health::{Cause, DataSignal, Evidence, Health, HealthReason, Root, RootKind};
use crate::refresh_path::{PathEvidence, PathSchedule};
use crate::status::NodeStatus;
use std::collections::HashMap;

/// The instant `gap_seconds` is read at: the start of `now`'s minute (unix seconds), so an
/// answer does not change with every second of the clock and a poll that finds nothing
/// changed within a minute answers the same body.
pub fn gap_instant(now: i64) -> i64 {
    now - now.rem_euclid(60)
}

/// `gap_seconds` of a classified row as of `at` ([`gap_instant`]): how long it has been out
/// of date plus how long until it is expected fresh; 0 for a fresh or undecided node.
pub fn gap_seconds(node: &NodeStatus, at: i64) -> i64 {
    if node.health.is_fresh() || node.health == Health::Undecided {
        return 0;
    }
    crate::estimates::behind_seconds(node, at) + crate::estimates::ahead_seconds(node, at)
}

/// Whether a scheduler is firing schedules, as far as this process can tell: one it runs
/// itself published a schedule (`live`), or the tick record proves one somewhere does (a
/// schedule that is not paused fired its newest slot: it has a tick and is not late). A
/// `--read-only` server and `barca status` on the serving machine run no scheduler; the
/// ticks the scheduling server wrote say it is up.
pub fn scheduler_seen(evidence: PathEvidence<'_>) -> bool {
    evidence.schedules.values().any(|s| {
        s.live
            || (!s.paused
                && !s.failing
                && s.standing
                    .as_ref()
                    .is_some_and(|st| st.last_fired.is_some() && !st.late))
    })
}

/// Whether a tick of `schedule` will come: it has a next tick (a scheduler here fires it), or
/// a scheduler is seen firing ([`scheduler_seen`]) and this schedule is not paused, held as
/// failing, or late (one a running scheduler does not fire falls behind its slots).
pub fn fires(schedule: &PathSchedule, seen: bool) -> bool {
    if schedule.next_tick.is_some() {
        return true;
    }
    if schedule.paused || schedule.failing {
        return false;
    }
    seen && schedule.standing.as_ref().is_none_or(|st| !st.late)
}

/// Whether a tick of the schedule is waiting for a run slot now, or its ticks typically wait
/// longer than they are apart (queue-bound): the run queue, not the schedule, is behind.
fn waits_for_slot(schedule: &PathSchedule) -> bool {
    schedule
        .standing
        .as_ref()
        .is_some_and(|st| st.queue_bound || st.last_status.as_deref() == Some("pending"))
}

/// Whether `run` computes `node` when it is out of date: a tick's run computes its
/// schedule's targets (`has_path`: not a paused or pinned node, which a tick leaves as it
/// is), another run its targets and what is above them.
fn computes(run: &PlannedRun, node: &str, has_path: bool, evidence: PathEvidence<'_>) -> bool {
    match (&run.schedule, &run.nodes) {
        (Some(schedule), _) => {
            has_path && evidence.paths.schedules(node).iter().any(|s| s == schedule)
        }
        (None, Some(planned)) => planned.contains(node),
        (None, None) => true,
    }
}

/// What the rows before a node answered, by row.
struct Above<'a> {
    at: HashMap<&'a str, usize>,
    /// The change (or the top of the chain) the walk found for each not-fresh row.
    origin: Vec<Option<(RootKind, &'a str)>>,
    cause: Vec<Option<Cause>>,
    root: Vec<Option<Root>>,
    has_path: Vec<bool>,
    /// For a row that is not fresh: the paused node (by row) that holds it, itself or the
    /// nearest above it through inputs that are not fresh. A tick reads a paused node as it
    /// is, so no tick leaves the row fresh until that node is resumed.
    held: Vec<Option<usize>>,
}

/// The node a changed component of a run hash names (`input ID`, `sensor ID`): its row.
/// A partition key (`ID[k=v]`) names its node.
fn named<'a>(component: &str, above: &Above<'a>) -> Option<usize> {
    let id = component.split_once(' ')?.1;
    let id = id.split('[').next().unwrap_or(id);
    above.at.get(id).copied()
}

/// What an input (by row) says of a node that is out of date against it: the input's own
/// origin when it is not fresh itself (the change is further up), else the input: a sensor
/// whose observation changed, or a result newer than what the node read.
fn through<'a>(
    input: usize,
    nodes: &'a [NodeStatus],
    above: &Above<'a>,
) -> Option<(RootKind, &'a str)> {
    let up = &nodes[input];
    if !up.health.is_fresh() {
        return above.origin[input].filter(|(kind, _)| *kind != RootKind::Failure);
    }
    Some((
        if up.kind == "sensor" {
            RootKind::SensorValue
        } else {
            RootKind::Input
        },
        up.id.as_str(),
    ))
}

/// The upstream fact the walk finds for `node` (a row that is not fresh): its own evidence
/// first, then what its inputs that are not fresh found, then the node itself as the top of
/// its chain.
fn origin<'a>(
    node: &'a NodeStatus,
    nodes: &'a [NodeStatus],
    above: &Above<'a>,
) -> (RootKind, &'a str) {
    let id = node.id.as_str();
    // A failure: the node's own, or the failing node that blocks it.
    if let Some(root) = node.root.as_ref().filter(|r| r.kind == RootKind::Failure) {
        return (RootKind::Failure, root.node.as_str());
    }
    // What changed in its run hash since its last successful result: its own definition
    // first, then a declared variable, then what it reads.
    let changed = node.cache.changed.as_deref().unwrap_or(&[]);
    let part = |c: &'a String| c.split(' ').next().unwrap_or(c);
    // What it reads: an input, a sensor, a source value, and an upstream node's function
    // named as a helper (`inputs={"orders": orders}` references it).
    let is_input = |c: &'a String| match part(c) {
        "input" | "sensor" | "value" => true,
        "helper" => named(c, above).is_some(),
        _ => false,
    };
    if changed.iter().any(|c| !is_input(c) && part(c) != "env") {
        return (RootKind::Code, id);
    }
    if changed.iter().any(|c| part(c) == "env") {
        return (RootKind::Env, id);
    }
    for component in changed.iter().filter(|c| is_input(c)) {
        match named(component, above) {
            Some(input) => {
                if let Some(found) = through(input, nodes, above) {
                    return found;
                }
            }
            // A source value, or an input outside the scope: the change is on the node.
            None => return (RootKind::Input, id),
        }
    }
    // A result recorded before its components were (no `changed`): the cause alone.
    let causes = node.cache.change_causes.as_deref().unwrap_or(&[]);
    let has = |cause: &str| causes.iter().any(|c| c == cause);
    if changed.is_empty() {
        if has("code_changed") {
            return (RootKind::Code, id);
        }
        if has("env_changed") {
            return (RootKind::Env, id);
        }
        if has("inputs_changed") {
            return (RootKind::Input, id);
        }
    }
    // Older than an input's newest result (`behind_input`).
    for signal in &node.signals {
        if let DataSignal::BehindInput { input, .. } = signal
            && let Some(found) = above
                .at
                .get(input.as_str())
                .and_then(|input| through(*input, nodes, above))
        {
            return found;
        }
    }
    // Out of date because of what is above it: the first input, in dependency order, that
    // is not fresh and explained.
    let inherited = node
        .inputs
        .iter()
        .filter_map(|input| above.at.get(input.as_str()).copied())
        .filter(|input| !nodes[*input].health.is_fresh())
        .filter_map(|input| {
            above.origin[input]
                .filter(|(kind, _)| *kind != RootKind::Failure)
                .map(|found| (input, found))
        })
        .min_by_key(|(input, _)| *input);
    if let Some((_, found)) = inherited {
        return found;
    }
    // The top of its chain: nothing on it or above it says more.
    let kind = match (node.health, node.health_reason) {
        (Health::Undecided, _) => RootKind::Undecided,
        (Health::NeverRun, _) | (_, HealthReason::Partial) => RootKind::NeverRun,
        (_, HealthReason::ScheduleLate) => RootKind::SchedulerOff,
        _ => RootKind::Input,
    };
    (kind, id)
}

/// The `cause` and `root` of every row of `nodes`, which [`crate::health::classify`] has
/// given their health, failure root, refresh path and times: one status result, in
/// dependency order. `(None, None)` exactly for a fresh row.
pub fn attribute(
    nodes: &[NodeStatus],
    evidence: Evidence<'_>,
) -> Vec<(Option<Cause>, Option<Root>)> {
    if nodes.iter().all(|n| n.health.is_fresh()) {
        return vec![(None, None); nodes.len()];
    }
    let seen = scheduler_seen(evidence.path);
    let mut above = Above {
        at: nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect(),
        origin: vec![None; nodes.len()],
        cause: vec![None; nodes.len()],
        root: vec![None; nodes.len()],
        has_path: vec![false; nodes.len()],
        held: vec![None; nodes.len()],
    };
    for (i, node) in nodes.iter().enumerate() {
        let path = crate::refresh_path::path_schedule(&node.id, evidence.path);
        above.has_path[i] = path.is_some();
        if node.health.is_fresh() {
            continue;
        }
        let found = origin(node, nodes, &above);
        above.origin[i] = Some(found);
        // What holds it: a paused node that is not fresh, itself or above it.
        let holder = if evidence.path.paused.contains(&node.id) {
            Some(i)
        } else {
            node.inputs
                .iter()
                .filter_map(|input| above.at.get(input.as_str()).copied())
                .filter(|input| !nodes[*input].health.is_fresh())
                .filter_map(|input| above.held[input])
                .min()
        };
        above.held[i] = holder;
        // A run that leaves it fresh: one that computes it and, where a paused node holds
        // it, computes that node too, which only a run someone asked for does.
        let plans = |running: bool| {
            evidence.timing.runs.iter().find(|r| {
                r.running == running
                    && computes(r, &node.id, path.is_some(), evidence.path)
                    && holder.is_none_or(|holder| {
                        r.schedule.is_none() && computes(r, &nodes[holder].id, false, evidence.path)
                    })
            })
        };
        let failure = node.root.as_ref().filter(|r| r.kind == RootKind::Failure);
        let waiting = plans(false);
        // A tick of its refresh path will come.
        let reached = path.is_some_and(|(_, s)| fires(s, seen));
        // That tick waits, or typically waits, for a run slot.
        let slot_wait = path.is_some_and(|(_, s)| fires(s, seen) && waits_for_slot(s));
        // The schedule whose queue explains the node: its path's, or a waiting tick's, when
        // its ticks wait for a run slot longer than they are apart (queue-bound). A tick
        // that only waits now is a moment, not an explanation: the node keeps the change
        // that made it out of date as its root.
        let bound = |id: &str| {
            evidence
                .path
                .schedules
                .get(id)
                .and_then(|s| s.standing.as_ref())
                .is_some_and(|st| st.queue_bound)
        };
        let queue = path
            .filter(|(id, s)| fires(s, seen) && bound(id))
            .map(|(id, _)| id)
            .or(waiting
                .and_then(|r| r.schedule.as_deref())
                .filter(|id| bound(id)));
        let cause = if failure.is_some() || path.is_some_and(|(_, s)| s.failing) {
            Cause::Failing
        } else if node.health == Health::Undecided {
            Cause::Undecided
        } else if plans(true).is_some() {
            Cause::CatchingUp
        } else if holder.is_some() && waiting.is_none() {
            // Held by a paused node: ticks come and find it as it is. Waiting clears
            // nothing; resuming the node does.
            Cause::NoRefreshPath
        } else if node
            .refresh_path
            .as_ref()
            .is_some_and(|p| p.missed_ticks >= 1)
        {
            Cause::Overdue
        } else if waiting.is_some() || slot_wait {
            Cause::Queued
        } else if !reached {
            Cause::NoRefreshPath
        } else {
            Cause::CatchingUp
        };
        // What a node no tick will reach hangs on: its path's schedule when that is paused
        // or not fired; a paused node itself; else the topmost node of the chain that
        // nothing refreshes.
        let unreached = || match path {
            Some((schedule, state)) => Root::of(
                if state.paused {
                    RootKind::SchedulePaused
                } else {
                    RootKind::SchedulerOff
                },
                schedule,
            ),
            None if evidence.path.paused.contains(&node.id) => {
                Root::of(RootKind::SchedulePaused, &node.id)
            }
            None => node
                .inputs
                .iter()
                .filter_map(|input| above.at.get(input.as_str()).copied())
                .filter(|input| {
                    !above.has_path[*input]
                        && above.root[*input].as_ref().is_some_and(|root| {
                            matches!(root.kind, RootKind::NoSchedule | RootKind::SchedulePaused)
                        })
                })
                .min()
                .and_then(|input| above.root[input].clone())
                .unwrap_or_else(|| Root::of(RootKind::NoSchedule, &node.id)),
        };
        let root = match cause {
            Cause::Failing => match (failure, path) {
                (Some(root), _) => root.clone(),
                (None, Some((schedule, _))) => Root::failure(schedule),
                (None, None) => Root::failure(&node.id),
            },
            Cause::Queued if queue.is_some() => {
                Root::of(RootKind::Queue, queue.unwrap_or(&node.id))
            }
            Cause::NoRefreshPath => match holder {
                Some(holder) => Root::of(RootKind::SchedulePaused, &nodes[holder].id),
                None => unreached(),
            },
            // Only a run decides it. Where no tick will come, what holds it is that, not
            // the change above it: waiting clears nothing.
            Cause::Undecided if !reached => unreached(),
            _ => Root::of(found.0, found.1),
        };
        above.cause[i] = Some(cause);
        above.root[i] = Some(root);
    }
    above.cause.into_iter().zip(above.root).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::Dag;
    use crate::db::NodeAge;
    use crate::health::{Checks, classify};
    use crate::refresh_path::{PathSchedules, Paths};
    use crate::rollup::{ActionKind, Rollup};
    use crate::schedule::Zone;
    use crate::schedule_standing::{TickRun, Ticks};
    use crate::status::ScheduleStatus;
    use std::collections::HashSet;

    // 2026-11-17 14:57:00 UTC.
    const NOW: i64 = 1_794_927_420;

    /// A project as classification reads it: the graph, one status row per node, and the
    /// evidence. Every node starts fresh, every schedule fired by a scheduler here.
    struct World {
        dag: Dag,
        nodes: Vec<NodeStatus>,
        paths: Paths,
        schedules: PathSchedules,
        ages: HashMap<String, NodeAge>,
        checks: Checks,
        paused: HashSet<String>,
        pinned: HashSet<String>,
        durations: HashMap<String, f64>,
        runs: Vec<PlannedRun>,
        max_runs: Option<usize>,
    }

    impl World {
        fn new(source: &str) -> Self {
            let dag = Dag::build(&crate::parse::extract_nodes(source, "p.py").unwrap())
                .expect("valid dag");
            let nodes = crate::queries::list_assets_from_dag(&dag)
                .into_iter()
                .map(|n| {
                    let kind = match n.kind {
                        crate::NodeKind::Asset => "asset",
                        crate::NodeKind::Sensor => "sensor",
                        crate::NodeKind::Task => "task",
                    };
                    serde_json::from_value(serde_json::json!({
                        "id": n.id, "name": crate::targets::short_name(&n.id), "kind": kind,
                        "inputs": n.inputs, "partitioned": false,
                        "cache": {
                            "state": if kind == "asset" { "cached" } else { "always_runs" },
                            "reason": "", "detail": "",
                        },
                        "last_materialization":
                            {"status": "success", "created_at": "2026-11-17 10:00:00"},
                        "shape": null, "env": [],
                    }))
                    .unwrap()
                })
                .collect();
            let paths = Paths::of(&dag);
            let mut world = World {
                dag,
                nodes,
                paths,
                schedules: PathSchedules::new(),
                ages: HashMap::new(),
                checks: Checks::default(),
                paused: HashSet::new(),
                pinned: HashSet::new(),
                durations: HashMap::new(),
                runs: Vec::new(),
                max_runs: Some(1),
            };
            for job in crate::schedule::jobs_from_dag(&world.dag) {
                world.schedule(crate::targets::short_name(&job.id), true, false, None);
            }
            world
        }

        fn id(&self, name: &str) -> String {
            self.nodes
                .iter()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("no node {name}"))
                .id
                .clone()
        }

        /// The schedule of `name` as of [`NOW`]: fired by a scheduler here (`live`) or only
        /// recorded, paused until resumed or not, over the recorded `ticks`.
        fn schedule(&mut self, name: &str, live: bool, paused: bool, ticks: Option<Ticks>) {
            let now = chrono::DateTime::from_timestamp(NOW, 0).unwrap();
            let job = crate::schedule::jobs_from_dag(&self.dag)
                .into_iter()
                .find(|job| crate::targets::short_name(&job.id) == name)
                .unwrap_or_else(|| panic!("no schedule {name}"));
            let pause = paused.then_some(crate::db::SchedulePause {
                paused_at: 0,
                until: None,
            });
            let standing = crate::schedule_standing::standing(
                &job.cron,
                &Zone::Utc,
                now,
                paused,
                ticks.as_ref(),
            );
            let mut block = ScheduleStatus::of(&job, standing.clone(), (0, 5), pause.as_ref());
            block.live = live;
            self.schedules.insert(
                job.id.clone(),
                PathSchedule::of(&job, &Zone::Utc, &standing, &block),
            );
        }

        /// Give `name` a cache state and what changed in its run hash.
        fn set(&mut self, name: &str, state: &str, changed: &[&str]) {
            let node = self
                .nodes
                .iter_mut()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("no node {name}"));
            node.cache.state = serde_json::from_value(serde_json::json!(state)).unwrap();
            if matches!(state, "never_run") || (state == "always_runs" && changed == ["never"]) {
                node.last_materialization = None;
                return;
            }
            if !changed.is_empty() {
                let changed: Vec<String> = changed.iter().map(|c| c.to_string()).collect();
                node.cache.change_causes = Some(crate::changes::causes(&changed));
                node.cache.changed = Some(changed);
            }
        }

        fn fail(&mut self, name: &str) {
            let node = self.nodes.iter_mut().find(|n| n.name == name).unwrap();
            node.last_materialization.as_mut().unwrap().status = "failed".into();
        }

        fn evidence(&self) -> Evidence<'_> {
            static NONE: std::sync::LazyLock<HashSet<String>> =
                std::sync::LazyLock::new(HashSet::new);
            Evidence {
                ages: &self.ages,
                pinned: &self.pinned,
                late: &NONE,
                checks: &self.checks,
                path: PathEvidence {
                    paths: &self.paths,
                    schedules: &self.schedules,
                    paused: &self.paused,
                    pinned: &self.pinned,
                },
                timing: crate::estimates::Timing {
                    durations: &self.durations,
                    runs: &self.runs,
                    max_runs: self.max_runs,
                    ..crate::estimates::Timing::none()
                },
                now: NOW,
            }
        }

        fn classify(&mut self) {
            let mut nodes = std::mem::take(&mut self.nodes);
            classify(&mut nodes, self.evidence());
            self.nodes = nodes;
        }

        fn node(&self, name: &str) -> &NodeStatus {
            self.nodes.iter().find(|n| n.name == name).unwrap()
        }

        /// `(cause, root id)` of `name`, with the file prefix of the root's node dropped.
        fn why(&self, name: &str) -> (Option<&'static str>, Option<String>) {
            let node = self.node(name);
            (
                node.cause.map(Cause::as_str),
                node.root.as_ref().map(|r| r.id.replace("p.py:", "")),
            )
        }

        fn rollup(&self, server: bool) -> Rollup {
            crate::rollup::build(
                &self.nodes,
                &self.dag.groups,
                &HashSet::new(),
                chrono::DateTime::from_timestamp(NOW, 0).unwrap(),
                self.evidence(),
                server,
            )
        }
    }

    fn is(cause: &'static str, root: &str) -> (Option<&'static str>, Option<String>) {
        (Some(cause), Some(root.to_string()))
    }

    /// INV-HEALTH-01 and INV-HEALTH-04: a cause and a root exactly on the rows that are not
    /// fresh, every such row under one root of the list, and the roots' counts adding up.
    fn assert_held(world: &World) -> Rollup {
        let rollup = world.rollup(false);
        let mut behind = 0;
        for node in &world.nodes {
            let fresh = node.health.is_fresh();
            assert_eq!(node.cause.is_none(), fresh, "{}: cause", node.id);
            assert_eq!(node.root.is_none(), fresh, "{}: root", node.id);
            if fresh {
                assert_eq!(node.gap_seconds, 0, "{}", node.id);
                continue;
            }
            behind += 1;
            let root = node.root.as_ref().unwrap();
            assert_eq!(root.id, format!("{}:{}", root.kind.as_str(), root.node));
            assert_eq!(
                rollup.roots.iter().filter(|r| r.id == root.id).count(),
                1,
                "{}: one root",
                node.id
            );
        }
        let total = rollup.root();
        assert_eq!(total.causes.total(), behind);
        assert_eq!(rollup.roots.iter().map(|r| r.nodes).sum::<usize>(), behind);
        assert_eq!(
            rollup.roots.iter().map(|r| r.gap_seconds).sum::<i64>(),
            total.gap_seconds
        );
        assert_eq!(
            rollup.roots.iter().map(|r| r.unbounded).sum::<usize>(),
            total.unbounded
        );
        assert_eq!(
            total.gap_seconds,
            world.nodes.iter().map(|n| n.gap_seconds).sum::<i64>()
        );
        for root in &rollup.roots {
            assert_eq!(root.causes.total(), root.nodes, "{}", root.id);
        }
        rollup
    }

    const SHAPES: &str = r#"
from barca import asset, sensor, task, Always, Manual, Schedule

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox(): ...

@asset(inputs={"inbox": inbox}, freshness=Always)
def orders(inbox): ...

@asset(inputs={"orders": orders}, freshness=Always)
def clean(orders): ...

@asset(inputs={"clean": clean}, freshness=Always)
def report(clean): ...

@asset(inputs={"orders": orders}, freshness=Manual)
def reviewed(orders): ...

@asset(inputs={"reviewed": reviewed}, freshness=Manual)
def signed(reviewed): ...

@asset(freshness=Always)
def loose(): ...

@asset(inputs={"loose": loose}, freshness=Always)
def below_loose(loose): ...

@asset(inputs={"below_loose": below_loose}, freshness=Schedule("0 * * * *"))
def hourly(below_loose): ...
"#;

    #[test]
    fn a_fresh_node_has_no_cause_no_root_and_no_gap() {
        let mut world = World::new(SHAPES);
        world.classify();
        for node in &world.nodes {
            assert_eq!((node.cause, &node.root, node.gap_seconds), (None, &None, 0));
        }
        let rollup = assert_held(&world);
        assert!(rollup.roots.is_empty());
        assert_eq!(rollup.root().gap_line(NOW), "every node is fresh");
    }

    #[test]
    fn the_first_matching_row_of_the_cause_table_decides() {
        let stale = |world: &mut World| {
            world.set("orders", "stale", &["sensor p.py:inbox"]);
            world.set("clean", "stale", &[]);
            world.set("report", "unknown", &[]);
        };
        // Row 7: a live path whose next tick will compute it.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.classify();
        assert_eq!(world.why("orders"), is("catching_up", "sensor_value:inbox"));
        assert_eq!(world.why("clean"), is("catching_up", "sensor_value:inbox"));
        // Row 2: the read path cannot decide it; the change above still explains it.
        assert_eq!(world.why("report"), is("undecided", "sensor_value:inbox"));
        assert_held(&world);

        // Row 4: a finished tick had it as a target and did not leave it fresh.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        let inbox = world.id("inbox");
        world.checks.misses = HashMap::from([(world.id("clean"), vec![(inbox.clone(), 400.0)])]);
        world.classify();
        assert_eq!(world.why("clean"), is("overdue", "sensor_value:inbox"));
        assert_eq!(world.why("orders"), is("catching_up", "sensor_value:inbox"));

        // Row 3 comes before row 4: a running run computes it.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.checks.misses = HashMap::from([(world.id("clean"), vec![(inbox.clone(), 400.0)])]);
        world.runs = vec![PlannedRun {
            run_id: "r1".into(),
            schedule: Some(inbox.clone()),
            nodes: None,
            running: true,
            start: Some(NOW - 5),
        }];
        world.classify();
        assert_eq!(world.why("clean"), is("catching_up", "sensor_value:inbox"));

        // Row 5: the run that computes it waits for a run slot. The root stays the change
        // that made it out of date, for a tick's run and a run someone asked for alike: a
        // wait for a slot is a moment (#1040). Only a queue-bound schedule names the queue.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.runs = vec![PlannedRun {
            run_id: "r1".into(),
            schedule: Some(inbox.clone()),
            nodes: None,
            running: false,
            start: Some(NOW + 30),
        }];
        world.classify();
        assert_eq!(world.why("clean"), is("queued", "sensor_value:inbox"));
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.runs = vec![PlannedRun {
            run_id: "r1".into(),
            schedule: None,
            nodes: Some(HashSet::from([world.id("orders"), world.id("clean")])),
            running: false,
            start: Some(NOW + 30),
        }];
        world.classify();
        assert_eq!(world.why("clean"), is("queued", "sensor_value:inbox"));
        // Row 2 comes before rows 3 and 5: an undecided node stays undecided.
        assert_eq!(world.why("report"), is("undecided", "sensor_value:inbox"));

        // Row 1 comes first: its own failure, a failing node above it, a schedule held
        // after `max_failures` failed ticks.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.fail("orders");
        world.runs = vec![PlannedRun {
            run_id: "r1".into(),
            schedule: Some(inbox.clone()),
            nodes: None,
            running: true,
            start: Some(NOW - 5),
        }];
        world.classify();
        assert_eq!(world.why("orders"), is("failing", "failure:orders"));
        assert_eq!(world.why("clean"), is("failing", "failure:orders"));
        assert_eq!(world.why("report"), is("failing", "failure:orders"));
        assert_held(&world);
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.schedules.get_mut(&inbox).unwrap().failing = true;
        world.schedules.get_mut(&inbox).unwrap().next_tick = None;
        world.classify();
        assert_eq!(world.why("clean"), is("failing", "failure:inbox"));
        let rollup = assert_held(&world);
        assert_eq!(
            rollup.roots[0].detail,
            "inbox's schedule is held after repeated failed ticks"
        );
    }

    #[test]
    fn a_root_is_the_change_the_walk_up_stops_at() {
        // Code: the node whose definition changed, for it and what is stale below it.
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["code", "sensor p.py:inbox"]);
        // What reads a node names its function as a helper: that is the node's change, not
        // the reader's own.
        world.set(
            "clean",
            "stale",
            &["helper p.py:orders", "input p.py:orders"],
        );
        world.set("report", "unknown", &[]);
        world.classify();
        for name in ["orders", "clean"] {
            assert_eq!(world.why(name), is("catching_up", "code:orders"));
        }
        assert_eq!(world.why("report"), is("undecided", "code:orders"));
        // A declared environment variable.
        let mut world = World::new(SHAPES);
        world.set("clean", "stale", &["env TOKEN"]);
        world.set("report", "stale", &[]);
        world.classify();
        assert_eq!(world.why("report"), is("catching_up", "env:clean"));
        // An input with a newer result than what the node read: the input, which is fresh.
        let mut world = World::new(SHAPES);
        world.set("clean", "stale", &["input p.py:orders"]);
        world.set("report", "stale", &["input p.py:clean"]);
        world.classify();
        assert_eq!(world.why("clean"), is("catching_up", "input:orders"));
        // `clean` is not fresh itself, so `report` reads through it to the same change.
        assert_eq!(world.why("report"), is("catching_up", "input:orders"));
        // Never run: the topmost never-run node of the chain.
        let mut world = World::new(SHAPES);
        for name in ["orders", "clean", "report"] {
            world.set(name, "never_run", &[]);
        }
        world.classify();
        assert_eq!(world.why("report"), is("catching_up", "never_run:orders"));
        // Undecided with nothing above that explains it: the topmost undecided node.
        let mut world = World::new(SHAPES);
        world.set("clean", "unknown", &[]);
        world.set("report", "unknown", &[]);
        world.classify();
        assert_eq!(world.why("report"), is("undecided", "undecided:clean"));
        let rollup = assert_held(&world);
        assert_eq!(rollup.roots[0].action.kind, ActionKind::Wait);
        assert_eq!(rollup.roots[0].unbounded, 2);
        // A node that changed itself is its own root, whatever is stale above it.
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["sensor p.py:inbox"]);
        world.set("clean", "stale", &["code"]);
        world.classify();
        assert_eq!(world.why("clean"), is("catching_up", "code:clean"));
    }

    #[test]
    fn a_node_nothing_refreshes_roots_where_a_schedule_is_missing_paused_or_not_fired() {
        // No schedule above: the topmost not-fresh node no tick reaches. `hourly` below it
        // is scheduled, so its own tick computes it: the chain's top explains it.
        let mut world = World::new(SHAPES);
        for name in ["loose", "below_loose", "hourly"] {
            world.set(name, "never_run", &[]);
        }
        world.classify();
        assert_eq!(
            world.why("loose"),
            is("no_refresh_path", "no_schedule:loose")
        );
        assert_eq!(
            world.why("below_loose"),
            is("no_refresh_path", "no_schedule:loose")
        );
        assert_eq!(world.why("hourly"), is("catching_up", "never_run:loose"));
        let rollup = assert_held(&world);
        let root = rollup
            .roots
            .iter()
            .find(|r| r.id == "no_schedule:p.py:loose")
            .unwrap();
        assert_eq!(root.detail, "no tick reaches loose");
        assert_eq!((root.nodes, root.unbounded), (2, 2));
        assert_eq!(
            (root.action.kind, root.action.command.as_deref()),
            (ActionKind::AddSchedule, None)
        );

        // A `Manual` node below a live chain: no tick computes it, someone runs it. What
        // is `Manual` below it hangs on the same root, and one command computes both.
        let mut world = World::new(SHAPES);
        world.set("reviewed", "stale", &["input p.py:orders"]);
        world.set("signed", "stale", &[]);
        world.durations = HashMap::from([(world.id("reviewed"), 40.0), (world.id("signed"), 20.2)]);
        world.classify();
        assert_eq!(
            world.why("reviewed"),
            is("no_refresh_path", "no_schedule:reviewed")
        );
        assert_eq!(
            world.why("signed"),
            is("no_refresh_path", "no_schedule:reviewed")
        );
        let rollup = assert_held(&world);
        let root = &rollup.roots[0];
        assert_eq!(root.detail, "reviewed is Manual: no tick computes it");
        assert_eq!(
            (
                root.action.kind,
                root.action.command.as_deref(),
                root.action.estimated_seconds
            ),
            (
                ActionKind::Run,
                Some("barca get signed --refresh reviewed"),
                Some(61)
            )
        );
        // Answered by a server, the command goes to it.
        assert_eq!(
            world.rollup(true).roots[0].action.command.as_deref(),
            Some("barca get signed --refresh reviewed --server")
        );

        // A paused node (`barca pause`) is skipped by every tick: it is its own root, and
        // resuming it is the action. What is below it keeps its path, and no tick leaves it
        // fresh while the node above is read as it is: it roots at the paused node (#1040).
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["sensor p.py:inbox"]);
        world.set("clean", "stale", &[]);
        world.paused = HashSet::from([world.id("orders")]);
        world.classify();
        assert_eq!(
            world.why("orders"),
            is("no_refresh_path", "schedule_paused:orders")
        );
        assert_eq!(
            world.why("clean"),
            is("no_refresh_path", "schedule_paused:orders")
        );
        let rollup = assert_held(&world);
        let root = rollup
            .roots
            .iter()
            .find(|r| r.kind == RootKind::SchedulePaused)
            .unwrap();
        assert_eq!(root.nodes, 2);
        assert_eq!(root.detail, "orders is paused: every tick skips it");
        assert_eq!(
            (root.action.kind, root.action.command.as_deref()),
            (
                ActionKind::ResumeSchedule,
                Some("barca resume orders --server")
            )
        );

        // A pinned asset is read as its pinned result: a root of its own, cleared by
        // unpinning it.
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["sensor p.py:inbox"]);
        world.pinned = HashSet::from([world.id("orders")]);
        world.classify();
        assert_eq!(
            world.why("orders"),
            is("no_refresh_path", "no_schedule:orders")
        );
        let rollup = assert_held(&world);
        assert_eq!(
            (
                rollup.roots[0].detail.as_str(),
                rollup.roots[0].action.command.as_deref()
            ),
            (
                "orders is pinned: every run reads its pinned result",
                Some("barca unpin orders")
            )
        );

        // A paused schedule: the scheduled node, for everything its ticks would reach.
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["sensor p.py:inbox"]);
        world.set("clean", "stale", &[]);
        world.schedule("inbox", true, true, None);
        world.classify();
        assert_eq!(
            world.why("clean"),
            is("no_refresh_path", "schedule_paused:inbox")
        );
        let rollup = assert_held(&world);
        assert_eq!(rollup.roots[0].detail, "inbox's schedule is paused");
        assert_eq!(
            (
                rollup.roots[0].action.kind,
                rollup.roots[0].action.command.as_deref()
            ),
            (
                ActionKind::ResumeSchedule,
                Some("barca resume --schedule inbox --server")
            )
        );
    }

    /// A tick of `*/5` that fired at `at` and ended well.
    fn fired(at: i64) -> Ticks {
        Ticks {
            newest: Some(TickRun {
                run_id: "r1".into(),
                scheduled_at: at,
                status: "success".into(),
                queued_at: None,
                started_at: Some(at),
                finished_at: Some(at + 20),
            }),
            queue_waits: vec![0.0],
            run_seconds: vec![20.0],
            decisions: Vec::new(),
        }
    }

    #[test]
    fn the_tick_record_says_whether_a_scheduler_is_firing_where_none_runs_here() {
        // 14:55, the slot before now.
        let last_slot = NOW - 120;
        let stale = |world: &mut World| {
            world.set("orders", "stale", &["sensor p.py:inbox"]);
            world.set("hourly", "stale", &["code"]);
        };
        // No scheduler here and no tick on record: nothing fires the schedules.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.set("clean", "unknown", &[]);
        world.set("reviewed", "unknown", &[]);
        world.schedule("inbox", false, false, None);
        world.schedule("hourly", false, false, None);
        world.classify();
        assert!(!world.node("orders").refresh_path.as_ref().unwrap().live);
        assert_eq!(
            world.why("orders"),
            is("no_refresh_path", "scheduler_off:inbox")
        );
        // An undecided node stays undecided, but what holds it is the tick that will not
        // come, not the change above it: waiting clears nothing.
        assert_eq!(world.why("clean"), is("undecided", "scheduler_off:inbox"));
        assert_eq!(
            world.why("reviewed"),
            is("undecided", "no_schedule:reviewed")
        );
        assert_eq!(
            world.why("hourly"),
            is("no_refresh_path", "scheduler_off:hourly")
        );
        let rollup = assert_held(&world);
        assert_eq!(
            (
                rollup.roots[0].detail.as_str(),
                rollup.roots[0].action.kind,
                rollup.roots[0].action.command.as_deref()
            ),
            (
                "no scheduler is firing inbox's schedule",
                ActionKind::StartServer,
                Some("barca serve")
            )
        );

        // A `--read-only` server: no scheduler here, but `inbox` fired its newest slot, so
        // one is running somewhere. `refresh_path.live` still says what this process fires;
        // the cause reads the record. `hourly` never fired and is not late: it will too.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.schedule("inbox", false, false, Some(fired(last_slot)));
        world.schedule("hourly", false, false, None);
        world.classify();
        assert!(!world.node("orders").refresh_path.as_ref().unwrap().live);
        assert_eq!(world.why("orders"), is("catching_up", "sensor_value:inbox"));
        assert_eq!(world.why("hourly"), is("catching_up", "code:hourly"));
        // No estimate without a scheduler here: the gap counts what is behind only.
        assert_eq!(world.node("orders").expected_fresh_at, None);

        // The scheduler stopped an hour ago: its newest tick is far behind its slots.
        let mut world = World::new(SHAPES);
        stale(&mut world);
        world.schedule("inbox", false, false, Some(fired(last_slot - 3600)));
        world.schedule("hourly", false, false, None);
        world.classify();
        assert_eq!(
            world.why("orders"),
            is("no_refresh_path", "scheduler_off:inbox")
        );
        let rollup = assert_held(&world);
        let root = rollup
            .roots
            .iter()
            .find(|r| r.id == "scheduler_off:p.py:inbox")
            .unwrap();
        assert_eq!(
            root.detail,
            "no scheduler has fired inbox's schedule for 57m"
        );
    }

    const QUEUE: &str = r#"
from barca import asset, sensor, Always, Schedule

@sensor(freshness=Schedule("* * * * *"))
def pulse(): ...

@asset(inputs={"pulse": pulse}, freshness=Always)
def beat(pulse): ...

@sensor(freshness=Schedule("* * * * *"))
def feed(): ...

@asset(inputs={"feed": feed}, freshness=Always)
def rows(feed): ...
"#;

    /// #973: one run slot and two schedules ticking every minute, whose runs take longer
    /// than a minute together. The queue is the cause, and the root says the wait.
    #[test]
    fn ticks_that_wait_longer_than_they_are_apart_name_the_queue() {
        let mut world = World::new(QUEUE);
        world.set("beat", "stale", &["sensor p.py:pulse"]);
        world.set("rows", "stale", &["sensor p.py:feed"]);
        // `pulse` typically waits 2m10s for the slot: queue-bound. `feed` does not, but its
        // newest tick is waiting for the slot now.
        let slot = NOW - NOW.rem_euclid(60);
        world.schedule(
            "pulse",
            true,
            false,
            Some(Ticks {
                queue_waits: vec![120.0, 130.0, 140.0],
                ..fired(slot)
            }),
        );
        world.schedule(
            "feed",
            true,
            false,
            Some(Ticks {
                newest: Some(TickRun {
                    run_id: "r2".into(),
                    scheduled_at: slot,
                    status: "pending".into(),
                    queued_at: Some(slot),
                    started_at: None,
                    finished_at: None,
                }),
                queue_waits: vec![5.0],
                run_seconds: vec![20.0],
                decisions: Vec::new(),
            }),
        );
        world.durations = HashMap::from([(world.id("beat"), 45.0)]);
        world.classify();
        assert_eq!(world.why("beat"), is("queued", "queue:pulse"));
        // A tick that only waits now is queued, and keeps what made it out of date as its
        // root: the queue root is for a schedule whose ticks wait longer than they are apart.
        assert_eq!(world.why("rows"), is("queued", "sensor_value:feed"));
        let rollup = assert_held(&world);
        assert_eq!(rollup.root().causes.queued, 2);
        let root = |id: &str| rollup.roots.iter().find(|r| r.id == id).unwrap();
        let pulse = root("queue:p.py:pulse");
        assert_eq!(
            pulse.detail,
            "pulse's ticks wait about 2m for a run slot, every 1m: max_runs is 1"
        );
        assert_eq!(
            (
                pulse.action.kind,
                pulse.action.command.as_deref(),
                pulse.action.estimated_seconds
            ),
            (ActionKind::RaiseMaxRuns, None, Some(45))
        );
        assert!(rollup.roots.iter().all(|r| r.id != "queue:p.py:feed"));
        assert_eq!(root("sensor_value:p.py:feed").action.kind, ActionKind::Wait);
        // Where the answering process does not know the setting, it is named without a number.
        world.max_runs = None;
        assert!(
            world.rollup(false).roots.iter().any(|r| r.detail
                == "pulse's ticks wait about 2m for a run slot, every 1m: max_runs limits the runs at once")
        );
        // A paused schedule is not waiting for a slot: nothing will fire it.
        world.schedule("pulse", true, true, None);
        world.classify();
        assert_eq!(
            world.why("beat"),
            is("no_refresh_path", "schedule_paused:pulse")
        );
    }

    #[test]
    fn ticks_that_missed_what_a_root_holds_make_its_action_a_run() {
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["code"]);
        world.set("clean", "stale", &[]);
        world.set("report", "stale", &[]);
        let inbox = world.id("inbox");
        world.checks.misses = HashMap::from([
            (world.id("clean"), vec![(inbox.clone(), 400.0)]),
            (world.id("report"), vec![(inbox, 400.0)]),
        ]);
        world.classify();
        let rollup = assert_held(&world);
        let root = &rollup.roots[0];
        assert_eq!((root.id.as_str(), root.nodes), ("code:p.py:orders", 3));
        assert_eq!((root.causes.overdue, root.causes.catching_up), (2, 1));
        // One command computes everything it holds: the held node nothing held reads.
        assert_eq!(
            (root.action.kind, root.action.command.as_deref()),
            (ActionKind::Run, Some("barca get report"))
        );
        // An overdue node's estimate already failed: it has none, so the root is unbounded.
        assert_eq!(root.unbounded, 2);
    }

    #[test]
    fn the_gap_is_what_is_behind_plus_what_is_ahead_read_at_the_minute() {
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["sensor p.py:inbox"]);
        world.set("clean", "unknown", &[]);
        // `orders` last succeeded at 10:00; `inbox` observed a new value at 14:00.
        let at = |time: &str| NodeAge {
            last_success_at: Some(format!("2026-11-17 {time}")),
            ..NodeAge::default()
        };
        world.ages = HashMap::from([
            (world.id("orders"), at("10:00:00")),
            (world.id("inbox"), at("14:00:00")),
        ]);
        world.durations = HashMap::from([(world.id("inbox"), 1.0), (world.id("orders"), 30.0)]);
        world.classify();
        let orders = world.node("orders");
        assert_eq!(orders.stale_since.as_deref(), Some("2026-11-17T14:00:00Z"));
        // The next tick is 15:00; the poll takes 1s and `orders` 30s: 15:00:35, rounded up.
        assert_eq!(
            orders.expected_fresh_at.as_deref(),
            Some("2026-11-17T15:00:35Z")
        );
        // 57 minutes behind at 14:57:00, 3m35s ahead.
        assert_eq!(orders.gap_seconds, 57 * 60 + 215);
        // Undecided: no gap, whatever the times.
        assert_eq!(world.node("clean").gap_seconds, 0);
        // Within the minute the answer does not move with the clock.
        let mut later = World::new(SHAPES);
        later.nodes = world.nodes.clone();
        let mut nodes = std::mem::take(&mut later.nodes);
        classify(
            &mut nodes,
            Evidence {
                now: NOW + 40,
                ..world.evidence()
            },
        );
        assert_eq!(
            nodes
                .iter()
                .find(|n| n.name == "orders")
                .unwrap()
                .gap_seconds,
            57 * 60 + 215
        );
        let rollup = assert_held(&world);
        assert_eq!(
            (rollup.root().gap_seconds, rollup.root().unbounded),
            (57 * 60 + 215, 1)
        );
        assert_eq!(
            rollup.root().gap_line(NOW),
            "1 undecided · 1 catching up (nodes); about 1 fresh within 4m, 1 with no estimate"
        );
    }

    /// The audit's shape (#963): three upstream changes fanning out to a few stale nodes
    /// and a few hundred the read path cannot decide, a chain no schedule reaches, and a
    /// queue-bound schedule, among nodes that are fresh. 698 nodes.
    fn audit() -> World {
        audit_with(0)
    }

    /// [`audit`], and with `blocked > 0` a failing node `load` that blocks so many nodes.
    fn audit_with(blocked: usize) -> World {
        use std::fmt::Write;
        let mut source = String::from(
            r#"
from barca import asset, sensor, Always, Schedule

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox(): ...

@sensor(freshness=Schedule("0 * * * *"))
def feed(): ...

@sensor(freshness=Schedule("* * * * *"))
def pulse(): ...
"#,
        );
        let mut asset = |name: &str, input: Option<&str>| {
            match input {
            Some(input) => writeln!(
                source,
                "@asset(inputs={{\"{input}\": {input}}}, freshness=Always)\ndef {name}({input}): ...\n"
            )
            .unwrap(),
            None => writeln!(source, "@asset(freshness=Always)\ndef {name}(): ...\n").unwrap(),
        }
        };
        // (the node that changed, what it reads, stale nodes below it, undecided below those)
        let changes = [
            ("orders", "inbox", 9, 170),
            ("prices", "feed", 3, 56),
            ("rates", "feed", 2, 21),
        ];
        for (top, input, stale, undecided) in changes {
            asset(top, Some(input));
            for i in 0..stale {
                asset(&format!("{top}_stage_{i}"), Some(top));
            }
            for i in 0..undecided {
                asset(
                    &format!("{top}_out_{i}"),
                    Some(&format!("{top}_stage_{}", i % stale)),
                );
            }
        }
        // A chain nothing schedules: 152 nodes that never ran.
        asset("ledger", None);
        for i in 0..151 {
            let above = if i < 8 {
                "ledger".to_string()
            } else {
                format!("ledger_{}", i % 8)
            };
            asset(&format!("ledger_{i}"), Some(&above));
        }
        // A schedule whose ticks wait for the one run slot.
        asset("beat", Some("pulse"));
        for i in 0..5 {
            asset(&format!("beat_{i}"), Some("beat"));
        }
        // A failing node and what it blocks.
        if blocked > 0 {
            asset("load", Some("inbox"));
            for i in 0..blocked {
                asset(&format!("load_{i}"), Some("load"));
            }
        }
        // And what is fresh.
        for i in 0..273 {
            asset(&format!("ok_{i}"), Some(["inbox", "feed"][i % 2]));
        }
        let mut world = World::new(&source);
        world.set("orders", "stale", &["sensor p.py:inbox"]);
        world.set("prices", "stale", &["code"]);
        world.set("rates", "stale", &["env RATES_URL"]);
        world.set("beat", "stale", &["sensor p.py:pulse"]);
        if blocked > 0 {
            world.set("load", "stale", &["sensor p.py:inbox"]);
            world.fail("load");
        }
        let names: Vec<String> = world.nodes.iter().map(|n| n.name.clone()).collect();
        for name in &names {
            if name.contains("_stage_") || name.starts_with("beat_") || name.starts_with("load_") {
                world.set(name, "stale", &[]);
            } else if name.contains("_out_") {
                world.set(name, "unknown", &[]);
            } else if name.starts_with("ledger") {
                world.set(name, "never_run", &[]);
            }
        }
        let slot = NOW - NOW.rem_euclid(60);
        world.schedule(
            "pulse",
            true,
            false,
            Some(Ticks {
                queue_waits: vec![120.0, 130.0, 140.0],
                ..fired(slot)
            }),
        );
        world.durations = names.iter().map(|name| (world.id(name), 3.0)).collect();
        world.ages = HashMap::from([
            (
                world.id("inbox"),
                NodeAge {
                    last_success_at: Some("2026-11-17 14:40:00".into()),
                    ..NodeAge::default()
                },
            ),
            (
                world.id("orders"),
                NodeAge {
                    last_success_at: Some("2026-11-17 10:00:00".into()),
                    ..NodeAge::default()
                },
            ),
        ]);
        world.classify();
        world
    }

    #[test]
    fn on_the_audit_shape_three_roots_hold_nine_in_ten_of_what_is_not_fresh() {
        let world = audit();
        assert_eq!(world.nodes.len(), 698);
        let rollup = assert_held(&world);
        let behind = rollup.root().causes.total();
        assert_eq!(behind, 422);
        let seen: Vec<(&str, usize, ActionKind)> = rollup
            .roots
            .iter()
            .map(|r| (r.id.as_str(), r.nodes, r.action.kind))
            .collect();
        assert_eq!(
            seen,
            [
                ("no_schedule:p.py:ledger", 152, ActionKind::AddSchedule),
                ("sensor_value:p.py:inbox", 180, ActionKind::Wait),
                ("code:p.py:prices", 60, ActionKind::Wait),
                ("env:p.py:rates", 24, ActionKind::Wait),
                ("queue:p.py:pulse", 6, ActionKind::RaiseMaxRuns),
            ]
        );
        // The scope's own list says the same, and how much its head explains (#967).
        let root = rollup.root();
        assert_eq!(root.not_fresh, 422);
        assert_eq!(
            root.held_by
                .iter()
                .map(|h| (h.root.as_str(), h.nodes, h.cumulative_nodes))
                .collect::<Vec<_>>(),
            [
                ("no_schedule:p.py:ledger", 152, 152),
                ("sensor_value:p.py:inbox", 180, 332),
                ("code:p.py:prices", 60, 392),
                ("env:p.py:rates", 24, 416),
                ("queue:p.py:pulse", 6, 422),
            ]
        );
        let top: usize = rollup.roots.iter().take(3).map(|r| r.nodes).sum();
        assert!(top * 10 >= behind * 9, "{top} of {behind}");
        assert_eq!(top, 392);
        let causes = rollup.root().causes;
        assert_eq!(
            (
                causes.catching_up,
                causes.undecided,
                causes.no_refresh_path,
                causes.queued
            ),
            (17, 247, 152, 6)
        );
        // What clearing a root costs: the typical compute time of what it holds.
        assert_eq!(rollup.roots[0].action.estimated_seconds, Some(456));
        // The order is the weight held (#1028): 152 nodes nothing refreshes weigh 6 each,
        // 180 that a tick will decide or compute 2 each.
        assert_eq!(
            rollup.roots.iter().map(|r| r.weight).collect::<Vec<_>>(),
            [912, 360, 120, 48, 12]
        );
        assert_eq!(
            rollup.roots.iter().map(|r| r.weight).sum::<usize>() + 276,
            rollup.root().weight,
            "the roots' weights and one per fresh node are the scope's"
        );
        // The whole to-do list, as `barca status --rollup` prints it.
        let text: String = crate::report::render_rollup(&rollup, false)
            .chunks
            .iter()
            .map(|chunk| match chunk {
                crate::report::OutputChunk::Stdout(text)
                | crate::report::OutputChunk::Stderr(text) => text.as_str(),
            })
            .collect();
        println!("{text}");
        assert!(text.contains("What holds the 422 nodes that are not fresh, and what clears it:"));
        assert!(text.contains("The first 3 hold 392 of them."));
        assert!(text.contains("1. no schedule: ledger"));
        assert!(
            text.contains(
                "Distance from fresh: 1728 (weighted nodes); 698 when every node is fresh"
            )
        );
        assert!(text.contains(
            "do:   add a schedule: freshness=Schedule(...) on it or on a node above it (about 7m of compute)"
        ));
        assert!(text.contains(
            "Gap: 152 no refresh path · 6 queued · 247 undecided · 17 catching up (nodes); \
             about 23 fresh within 4m, 399 with no estimate"
        ));
    }

    /// #1028: roots rank by the weight they hold, so a failure that blocks 20 nodes is among
    /// the first three, ahead of a change that holds 60 a tick will bring up to date, and the
    /// first three hold three quarters of what is not fresh.
    #[test]
    fn roots_rank_by_weight_and_a_failure_with_blocked_nodes_is_among_the_first_three() {
        let world = audit_with(20);
        let rollup = assert_held(&world);
        let behind = rollup.root().not_fresh;
        assert_eq!(behind, 443);
        let seen: Vec<(&str, usize, usize)> = rollup
            .roots
            .iter()
            .map(|r| (r.id.as_str(), r.nodes, r.weight))
            .collect();
        assert_eq!(
            seen,
            [
                ("no_schedule:p.py:ledger", 152, 912),
                ("sensor_value:p.py:inbox", 180, 360),
                ("failure:p.py:load", 21, 126),
                ("code:p.py:prices", 60, 120),
                ("env:p.py:rates", 24, 48),
                ("queue:p.py:pulse", 6, 12),
            ]
        );
        let third = &rollup.root().held_by[2];
        assert_eq!(third.root, "failure:p.py:load");
        assert!(
            third.cumulative_nodes * 4 >= behind * 3,
            "{} of {behind}",
            third.cumulative_nodes
        );
    }

    /// #968: a node that is not fresh and has no `expected_fresh_at` says why in one word;
    /// a fresh node and one with an estimate say nothing.
    #[test]
    fn a_node_with_no_estimate_says_why() {
        use crate::health::NoEstimate;
        let world = audit_with(20);
        let why = |name: &str| {
            let node = world.node(name);
            (node.expected_fresh_at.is_some(), node.no_estimate)
        };
        // Nothing scheduled reaches the ledger chain; a failure holds what it blocks.
        assert_eq!(why("ledger_7"), (false, Some(NoEstimate::NoPath)));
        assert_eq!(why("load"), (false, Some(NoEstimate::Failing)));
        assert_eq!(why("load_3"), (false, Some(NoEstimate::Failing)));
        // Only a run decides a node the read path cannot.
        assert_eq!(why("orders_out_4"), (false, Some(NoEstimate::Undecided)));
        // A fresh node, and a node the next tick computes, have no reason to give.
        assert_eq!(why("ok_1"), (false, None));
        assert_eq!(why("orders_stage_1"), (true, None));
        // Every not-fresh node has an estimate or a reason, never neither or both.
        for node in &world.nodes {
            assert_eq!(
                node.no_estimate.is_some(),
                node.cause.is_some() && node.expected_fresh_at.is_none(),
                "{}",
                node.id
            );
        }
        // The words a surface prints after "no estimate: ".
        assert_eq!(NoEstimate::NoPath.word(), "no refresh path");
        assert_eq!(
            serde_json::to_value(NoEstimate::NoSchedulerHere).unwrap(),
            "no_scheduler_here"
        );
    }

    /// `at` (unix seconds) as a materialization records it.
    fn stamp(at: i64) -> String {
        chrono::DateTime::from_timestamp(at, 0)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    /// #1040: a paused node that is out of date, the nodes below it, and the tick of its
    /// path's schedule in every state (to come, waiting for a run slot, running). No tick
    /// computes the paused node, so none leaves it or what is below it fresh: no estimate is
    /// given and then withdrawn, and all of it roots at the paused node, cleared by resuming it.
    #[test]
    fn nothing_is_promised_through_a_paused_node_and_what_is_below_it_roots_at_it() {
        let tick_run = |running: bool| PlannedRun {
            run_id: "r1".into(),
            schedule: Some("p.py:inbox".into()),
            nodes: None,
            running,
            start: Some(if running { NOW - 5 } else { NOW + 10 }),
        };
        for runs in [vec![], vec![tick_run(false)], vec![tick_run(true)]] {
            let mut world = World::new(SHAPES);
            world.set("orders", "stale", &["sensor p.py:inbox"]);
            world.set("clean", "stale", &[]);
            world.set("report", "stale", &[]);
            world.paused = HashSet::from([world.id("orders")]);
            world.durations = HashMap::from([(world.id("orders"), 10.0)]);
            let waits = runs.iter().any(|r| !r.running);
            if waits {
                // The tick's run waits for a run slot: the schedule's newest tick is pending.
                let slot = NOW - NOW.rem_euclid(300);
                world.schedule(
                    "inbox",
                    true,
                    false,
                    Some(Ticks {
                        newest: Some(TickRun {
                            run_id: "r1".into(),
                            scheduled_at: slot,
                            status: "pending".into(),
                            queued_at: Some(slot),
                            started_at: None,
                            finished_at: None,
                        }),
                        ..Default::default()
                    }),
                );
            }
            world.runs = runs;
            world.classify();
            for name in ["orders", "clean", "report"] {
                let node = world.node(name);
                assert_eq!(node.expected_fresh_at, None, "{name}: {:?}", world.runs);
                assert_eq!(
                    world.why(name),
                    is("no_refresh_path", "schedule_paused:orders"),
                    "{name}: {:?}",
                    world.runs
                );
                assert_eq!(node.no_estimate, Some(crate::health::NoEstimate::Paused));
            }
            let rollup = assert_held(&world);
            assert_eq!(rollup.roots.len(), 1);
            assert_eq!(
                (
                    rollup.roots[0].nodes,
                    rollup.roots[0].action.kind,
                    rollup.roots[0].action.command.as_deref()
                ),
                (
                    3,
                    ActionKind::ResumeSchedule,
                    Some("barca resume orders --server")
                )
            );
        }
        // A run someone asked for computes a paused node: it clears all three.
        let mut world = World::new(SHAPES);
        world.set("orders", "stale", &["sensor p.py:inbox"]);
        world.set("clean", "stale", &[]);
        world.paused = HashSet::from([world.id("orders")]);
        world.runs = vec![PlannedRun {
            run_id: "r2".into(),
            schedule: None,
            nodes: None,
            running: true,
            start: Some(NOW - 5),
        }];
        world.classify();
        assert_eq!(world.why("clean"), is("catching_up", "sensor_value:inbox"));
        assert!(world.node("clean").expected_fresh_at.is_some());
    }

    /// #1040: `stale_since` is when the input first changed after the node's own result, and
    /// stays there however often the sensor is polled with the same observation afterwards.
    #[test]
    fn out_of_date_since_is_the_inputs_first_change_however_often_it_is_polled() {
        let since = |polled: i64, changes: &[i64]| {
            let mut world = World::new(SHAPES);
            world.set("orders", "stale", &["sensor p.py:inbox"]);
            world.set("clean", "stale", &[]);
            world.ages = HashMap::from([
                (
                    world.id("inbox"),
                    NodeAge {
                        last_success_at: Some(stamp(polled)),
                        changed_at: std::sync::Arc::new(
                            changes.iter().map(|at| stamp(*at)).collect(),
                        ),
                        ..Default::default()
                    },
                ),
                (
                    world.id("orders"),
                    NodeAge {
                        last_success_at: Some(stamp(NOW - 3600)),
                        ..Default::default()
                    },
                ),
            ]);
            world.classify();
            assert_eq!(
                world.node("clean").stale_since,
                world.node("orders").stale_since
            );
            world
                .node("orders")
                .stale_since
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .map(|at| at - NOW)
        };
        // The observation changed 20 minutes ago (and once, long before the node's own
        // result); the sensor has been polled every minute since, without a change.
        let changes = [NOW - 7200, NOW - 1200];
        assert_eq!(since(NOW - 1200, &changes), Some(-1200));
        assert_eq!(since(NOW - 60, &changes), Some(-1200));
        assert_eq!(since(NOW, &changes), Some(-1200));
        // It changed again: still the first change after the node's result.
        assert_eq!(since(NOW, &[NOW - 7200, NOW - 1200, NOW - 30]), Some(-1200));
        // No change on record (results that recorded no digest): the newest result.
        assert_eq!(since(NOW - 60, &[]), Some(-60));
    }

    /// #1040: ticks could not compute what is below a failing node. Once that node is fixed
    /// and run by hand, those ticks are not misses of the nodes below it: they could not
    /// have made them fresh, and no tick has yet had the chance to since.
    #[test]
    fn a_tick_that_could_not_reach_a_node_is_not_a_miss_once_what_is_above_it_succeeds_again() {
        let after = |fixed: Option<i64>| {
            let mut world = World::new(SHAPES);
            world.set("clean", "stale", &["input p.py:orders"]);
            world.set("report", "stale", &[]);
            let inbox = world.id("inbox");
            let misses = vec![
                (inbox.clone(), (NOW - 300) as f64),
                (inbox, (NOW - 600) as f64),
            ];
            world.checks.misses = HashMap::from([
                (world.id("clean"), misses.clone()),
                (world.id("report"), misses),
            ]);
            if let Some(at) = fixed {
                world.ages.insert(
                    world.id("orders"),
                    NodeAge {
                        last_success_at: Some(stamp(NOW - 1)),
                        recovered_at: Some(stamp(at)),
                        ..Default::default()
                    },
                );
            }
            world.classify();
            let node = world.node("report");
            (
                world.why("report").0,
                node.refresh_path.as_ref().unwrap().missed_ticks,
                node.expected_fresh_at.is_some(),
            )
        };
        // Nothing above it succeeded since: two ticks reached it and left it out of date.
        assert_eq!(after(None), (Some("overdue"), 2, false));
        assert_eq!(after(Some(NOW - 900)), (Some("overdue"), 2, false));
        // `orders` succeeded between the two: the older tick is from before that.
        assert_eq!(after(Some(NOW - 400)), (Some("overdue"), 1, false));
        // `orders` was fixed and run a minute ago: no tick has missed it since, and the
        // next one is expected to leave it fresh.
        assert_eq!(after(Some(NOW - 60)), (Some("catching_up"), 0, true));
    }

    /// #1040: a tick waiting for a run slot does not replace a code change as the root; a
    /// queue-bound schedule does.
    #[test]
    fn a_tick_waiting_for_a_run_slot_keeps_the_change_as_the_root() {
        let slot = NOW - NOW.rem_euclid(300);
        let why = |waits: Vec<f64>| {
            let mut world = World::new(SHAPES);
            world.set("orders", "stale", &["code"]);
            world.set("clean", "stale", &[]);
            world.schedule(
                "inbox",
                true,
                false,
                Some(Ticks {
                    newest: Some(TickRun {
                        run_id: "r1".into(),
                        scheduled_at: slot,
                        status: "pending".into(),
                        queued_at: Some(slot),
                        started_at: None,
                        finished_at: None,
                    }),
                    queue_waits: waits,
                    ..Default::default()
                }),
            );
            world.runs = vec![PlannedRun {
                run_id: "r1".into(),
                schedule: Some(world.id("inbox")),
                nodes: None,
                running: false,
                start: Some(NOW + 20),
            }];
            world.classify();
            (world.why("orders"), world.why("clean"))
        };
        assert_eq!(
            why(vec![2.0, 3.0]),
            (is("queued", "code:orders"), is("queued", "code:orders"))
        );
        assert_eq!(
            why(vec![400.0, 500.0]),
            (is("queued", "queue:inbox"), is("queued", "queue:inbox"))
        );
    }
}
