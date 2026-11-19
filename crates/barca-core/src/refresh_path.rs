//! A node's refresh path (`specs/behavior/health.md`, "Refresh path"; #961): the schedule
//! whose tick brings it up to date, when that schedule next fires, and how many of its ticks
//! already had the node as a target and did not leave it fresh. `None` when no tick ever
//! will: nothing scheduled reaches the node as a target, or the node is paused or pinned.
//!
//! Two layers:
//!
//! - [`Paths`] is static: which schedules reach each node as a target. It is the scheduler's
//!   own walk ([`crate::freshness::tick_reach`]), computed once per loaded graph and kept
//!   (seconds on a graph of thousands of nodes and dozens of schedules), never per poll.
//! - [`PathSchedule`] is each schedule as of one instant: its next tick, whether it is paused
//!   and whether a scheduler fires it. It moves with the clock, so it is built per answer
//!   from where the schedule stands ([`crate::schedule_standing::standing`]).
//!
//! [`path_schedule`] joins them for one node and is what the cause and the estimated times
//! (#962, #963) read; [`refresh_path`] adds the missed ticks and is the field every surface
//! carries.

use crate::dag::Dag;
use crate::freshness::{Reach, tick_reach};
use crate::schedule::{Zone, next_fire};
use crate::schedule_standing::Standing;
pub use crate::views::RefreshPath;
use crate::views::ScheduleStatus;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// What a table says of a node with no refresh path.
pub const NO_REFRESH_PATH: &str = "no refresh path";

/// For each node, the schedules whose ticks have it as a target: the static half of a
/// refresh path. It depends on the graph alone; compute it once per loaded graph.
#[derive(Clone, Debug, Default)]
pub struct Paths {
    targets: HashMap<String, Vec<String>>,
    /// Nodes declared `freshness=Manual`: refreshed by request only.
    manual: HashSet<String>,
}

impl Paths {
    /// The scheduler's own walk over `dag` ([`tick_reach`], from every node with a
    /// `Schedule`): a node keeps the schedules that reach it
    /// as [`Reach::Target`]. One reached only as an input, or not at all, has none.
    pub fn of(dag: &Dag) -> Self {
        let scheduled: Vec<&str> = dag
            .topo_order()
            .into_iter()
            .filter(|id| {
                dag.get_node(id)
                    .is_some_and(|n| matches!(n.extracted.freshness, crate::Freshness::Schedule(_)))
            })
            .collect();
        let manual: HashSet<String> = dag
            .topo_order()
            .into_iter()
            .filter(|id| {
                dag.get_node(id)
                    .is_some_and(|n| matches!(n.extracted.freshness, crate::Freshness::Manual))
            })
            .map(str::to_string)
            .collect();
        if scheduled.is_empty() {
            return Self {
                targets: HashMap::new(),
                manual,
            };
        }
        let targets = tick_reach(dag, scheduled)
            .into_iter()
            .filter_map(|(node, reached)| {
                let by: Vec<String> = reached
                    .into_iter()
                    .filter(|(_, how)| *how == Reach::Target)
                    .map(|(schedule, _)| schedule)
                    .collect();
                (!by.is_empty()).then_some((node, by))
            })
            .collect();
        Self { targets, manual }
    }

    /// Whether `node` is declared `freshness=Manual`: no tick computes it, someone runs it.
    pub fn is_manual(&self, node: &str) -> bool {
        self.manual.contains(node)
    }

    /// The schedules whose ticks have `node` as a target, sorted by id; empty when none does.
    pub fn schedules(&self, node: &str) -> &[String] {
        self.targets.get(node).map_or(&[], Vec::as_slice)
    }
}

/// One schedule as of one instant: the runtime half of a refresh path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathSchedule {
    /// When it next fires, unix seconds: its next slot, or the first slot after a timed pause
    /// ends. `None` for a pause until resumed and when it is not `live`.
    pub next_tick: Option<i64>,
    pub paused: bool,
    /// A running scheduler fires it: the scheduler published it and does not hold it as
    /// `failing`.
    pub live: bool,
    /// Held after `max_failures` failed ticks: its ticks are skipped.
    pub failing: bool,
    /// Where it stands ([`crate::schedule_standing::standing`]): last tick, typical queue
    /// wait, lateness. `None` for a schedule nothing reported.
    pub standing: Option<Standing>,
    /// The first slot no scheduler has decided, when one has passed
    /// ([`crate::liveness::Seen::unserved_since`]). Read only where the schedule is not live.
    pub unserved_since: Option<i64>,
    /// For a schedule no scheduler fires: the last sign of the one that did
    /// ([`crate::liveness::Seen::off_since`]); `None` when none ever did.
    pub off_since: Option<i64>,
    /// Its one word ([`crate::schedule_standing::ScheduleState`]).
    pub state: crate::schedule_standing::ScheduleState,
}

/// A schedule nothing reported (absent from [`PathEvidence::schedules`]): not firing.
static NOT_FIRING: PathSchedule = PathSchedule {
    next_tick: None,
    paused: false,
    live: false,
    failing: false,
    standing: None,
    unserved_since: None,
    off_since: None,
    state: crate::schedule_standing::ScheduleState::OnTime,
};

impl PathSchedule {
    /// From where `job`'s schedule stands and its `schedule` block, as the surface answering
    /// reports them (`block.live` is the scheduler's own word), with cron evaluated in `zone`.
    pub fn of(
        job: &crate::schedule::ScheduledJob,
        zone: &Zone,
        standing: &Standing,
        block: &ScheduleStatus,
    ) -> Self {
        let live = block.live && !block.failing;
        let next_tick = if !live {
            None
        } else if block.paused {
            block
                .paused_until
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .and_then(|until| chrono::DateTime::from_timestamp(until, 0))
                .and_then(|until| next_fire(&job.cron, zone, until))
                .map(|slot| slot.timestamp())
        } else {
            standing.next_fire
        };
        Self {
            next_tick,
            paused: block.paused,
            live,
            failing: block.failing,
            standing: Some(standing.clone()),
            unserved_since: None,
            off_since: None,
            state: block.state,
        }
    }

    /// With what the records say of the scheduler that fires it ([`crate::liveness::observe`]),
    /// for a process that runs none: the slot nothing served and since when nothing fires it.
    pub fn seen(mut self, seen: &crate::liveness::Seen) -> Self {
        if !seen.live {
            self.unserved_since = seen.unserved_since;
            self.off_since = seen.off_since;
        }
        self
    }

    /// No scheduler fires it, and that (not a pause, not a hold after `max_failures`) is why
    /// no tick comes.
    pub fn off(&self) -> bool {
        !self.live && !self.paused && !self.failing
    }

    /// A slot of it has passed that no scheduler served, and none fires it: what it refreshes
    /// is no longer kept fresh ([`crate::health::decide`], row 6a).
    pub fn unserved(&self) -> bool {
        self.off() && self.unserved_since.is_some()
    }
}

/// Every schedule as of one instant, by scheduled node id.
pub type PathSchedules = HashMap<String, PathSchedule>;

/// What a refresh path is read from, beside the tick record.
#[derive(Clone, Copy)]
pub struct PathEvidence<'a> {
    /// The static reach of the graph ([`Paths::of`]).
    pub paths: &'a Paths,
    /// Each schedule now ([`PathSchedule::of`]). A schedule absent here is not firing.
    pub schedules: &'a PathSchedules,
    /// Paused nodes (`barca pause`): a tick skips one, so it has no path.
    pub paused: &'a HashSet<String>,
    /// Pinned assets (`barca pin`): a tick reads one as its pinned result, so it has no path.
    pub pinned: &'a HashSet<String>,
}

impl PathEvidence<'static> {
    /// No schedule reaches anything: every node's path is `None`.
    pub fn none() -> Self {
        static PATHS: LazyLock<Paths> = LazyLock::new(Paths::default);
        static SCHEDULES: LazyLock<PathSchedules> = LazyLock::new(PathSchedules::default);
        static NODES: LazyLock<HashSet<String>> = LazyLock::new(HashSet::new);
        Self {
            paths: &PATHS,
            schedules: &SCHEDULES,
            paused: &NODES,
            pinned: &NODES,
        }
    }
}

/// The schedule of `node`'s refresh path, with its state now: of the schedules whose ticks
/// have the node as a target, the one that fires earliest next (one with no next tick last,
/// ties by id). `None` when no tick has it as a target, and for a paused or pinned node,
/// which a tick leaves as it is.
///
/// This is the entry point for what is derived from a path: `next_tick` is when a tick next
/// evaluates the node, and `standing` carries the schedule's last tick and typical queue wait.
pub fn path_schedule<'a>(
    node: &str,
    evidence: PathEvidence<'a>,
) -> Option<(&'a str, &'a PathSchedule)> {
    if evidence.paused.contains(node) || evidence.pinned.contains(node) {
        return None;
    }
    evidence
        .paths
        .schedules(node)
        .iter()
        .map(|id| {
            (
                id.as_str(),
                evidence.schedules.get(id).unwrap_or(&NOT_FIRING),
            )
        })
        .min_by_key(|(id, schedule)| (schedule.next_tick.is_none(), schedule.next_tick, *id))
}

/// How many of `misses` (a node's finished ticks that did not leave it fresh, as
/// `(schedule, when the run ended)`, back to the last tick that did:
/// [`crate::db::LatestTicks::misses`]) count for `node`: those of a schedule of its path that
/// ended after `last_success` (unix seconds), its newest successful result, and not before
/// the latest recovery above it. Once an input recovers, earlier ticks that could not
/// compute the node cease to count. Later healthy polls do not clear genuine misses.
pub fn missed_ticks(
    node: &str,
    paths: &Paths,
    misses: &[(String, f64)],
    last_success: Option<i64>,
    recovered_above: Option<i64>,
) -> u32 {
    let schedules = paths.schedules(node);
    misses
        .iter()
        .filter(|(schedule, ended)| {
            schedules.contains(schedule)
                && last_success.is_none_or(|at| *ended > at as f64)
                // Results are recorded to the second: one in the second the run ended is
                // the run's own.
                && recovered_above.is_none_or(|at| ended.floor() >= at as f64)
        })
        .count() as u32
}

/// `node`'s [`RefreshPath`]: [`path_schedule`] with the ticks that missed it (`missed`,
/// [`missed_ticks`]; pass 0 for a fresh node).
pub fn refresh_path(node: &str, missed: u32, evidence: PathEvidence<'_>) -> Option<RefreshPath> {
    let (schedule, state) = path_schedule(node, evidence)?;
    Some(RefreshPath {
        schedule: schedule.to_string(),
        next_tick: state.next_tick.and_then(crate::time::rfc3339_secs),
        paused: state.paused,
        live: state.live,
        state: state.state,
        missed_ticks: missed,
    })
}

/// What a scheduler running in the answering process says of one schedule it fires: its own
/// count of failed ticks and why it skipped the last due one. A host without a scheduler
/// (`barca status`, a read-only server) has none, and the records are read instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Firing {
    pub failures: u32,
    pub max_failures: u32,
    /// Why the last due tick was skipped (`in_flight`, `paused`).
    pub skip_reason: Option<String>,
}

/// What every schedule of a graph is read from, as of `now`: plain data, filled by the host
/// answering. `barca status` reads all of it from the metadata database; a server fills it
/// from its read cache, its pauses and its scheduler. Where the hosts differ, they differ
/// here and nowhere else.
#[derive(Clone, Copy)]
pub struct ScheduleInputs<'a> {
    /// The scheduled nodes ([`crate::schedule::jobs_from_dag`]).
    pub jobs: &'a [crate::schedule::ScheduledJob],
    /// The zone cron is evaluated in: the one the firing scheduler uses.
    pub zone: &'a Zone,
    pub now: chrono::DateTime<chrono::Utc>,
    /// Each schedule's recorded ticks ([`crate::schedule_standing::recorded`]).
    pub ticks: &'a HashMap<String, crate::schedule_standing::Ticks>,
    /// Each schedule's recorded failed ticks: read for a schedule absent from `firing`.
    pub failures: &'a HashMap<String, crate::db::ScheduleFailures>,
    /// The paused schedules; one whose `until` has passed is over.
    pub pauses: &'a HashMap<String, crate::db::SchedulePause>,
    /// The schedules a scheduler in this process fires; empty where none runs.
    pub firing: &'a HashMap<String, Firing>,
    /// What the records say of a scheduler elsewhere ([`crate::liveness::observe`]); the
    /// default where this process runs the scheduler.
    pub observed: &'a crate::liveness::Observed,
}

/// One schedule as every surface reports it: where it stands, its `schedule` block and what
/// a refresh path through it reads.
#[derive(Clone, Debug)]
pub struct ScheduleView {
    pub standing: Standing,
    pub block: ScheduleStatus,
    pub path: PathSchedule,
}

/// Every schedule of `inputs`, by scheduled node id: the one walk from a schedule's pause,
/// failed ticks and recorded ticks to its standing ([`crate::schedule_standing::standing`]),
/// its `schedule` block and its refresh-path state. A schedule is `live` when a scheduler
/// here fires it or the records show one firing it; paused, or held after `max_failures`
/// failed ticks, no tick is expected of it.
pub fn schedule_views(inputs: ScheduleInputs<'_>) -> HashMap<String, ScheduleView> {
    let ScheduleInputs { zone, now, .. } = inputs;
    inputs
        .jobs
        .iter()
        .map(|job| {
            let pause = inputs
                .pauses
                .get(&job.id)
                .filter(|p| p.until.is_none_or(|until| until > now.timestamp()));
            let firing = inputs.firing.get(&job.id);
            let failed = match firing {
                Some(firing) => (firing.failures, firing.max_failures),
                None => (
                    inputs.failures.get(&job.id).map_or(0, |f| f.failures),
                    job.max_failures,
                ),
            };
            let standing = crate::schedule_standing::standing(
                &job.cron,
                zone,
                now,
                pause.is_some() || failed.0 >= failed.1,
                inputs.ticks.get(&job.id),
            );
            let mut block = ScheduleStatus::of(job, standing.clone(), failed, pause);
            block.skip_reason = firing.and_then(|f| f.skip_reason.clone());
            let seen = inputs
                .observed
                .schedules
                .get(&job.id)
                .copied()
                .unwrap_or_default();
            block.live = firing.is_some() || seen.live;
            let path = PathSchedule::of(job, zone, &standing, &block).seen(&seen);
            (
                job.id.clone(),
                ScheduleView {
                    standing,
                    block,
                    path,
                },
            )
        })
        .collect()
}

/// [`schedule_views`] as a refresh path reads them ([`PathEvidence::schedules`]).
pub fn path_schedules(views: &HashMap<String, ScheduleView>) -> PathSchedules {
    views
        .iter()
        .map(|(id, view)| (id.clone(), view.path.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::jobs_from_dag;

    fn dag(source: &str) -> Dag {
        Dag::build(&crate::parse::extract_nodes(source, "p.py").unwrap()).expect("valid dag")
    }

    fn id(dag: &Dag, name: &str) -> String {
        dag.topo_order()
            .into_iter()
            .find(|id| crate::targets::short_name(id) == name)
            .unwrap_or_else(|| panic!("no node {name}"))
            .to_string()
    }

    /// Every schedule of `dag` firing, as of `now`, with the given ones paused until a time
    /// (`Some`) or until resumed (`None`).
    fn firing(dag: &Dag, now: i64, paused: &[(&str, Option<i64>)]) -> PathSchedules {
        let now = chrono::DateTime::from_timestamp(now, 0).unwrap();
        jobs_from_dag(dag)
            .iter()
            .map(|job| {
                let pause = paused
                    .iter()
                    .find(|(name, _)| crate::targets::short_name(&job.id) == *name)
                    .map(|(_, until)| crate::db::SchedulePause {
                        paused_at: 0,
                        until: *until,
                    });
                let standing = crate::schedule_standing::standing(
                    &job.cron,
                    &Zone::Utc,
                    now,
                    pause.is_some(),
                    None,
                );
                let mut block = ScheduleStatus::of(job, standing.clone(), (0, 5), pause.as_ref());
                block.live = true;
                (
                    job.id.clone(),
                    PathSchedule::of(job, &Zone::Utc, &standing, &block),
                )
            })
            .collect()
    }

    /// A path as the tests read it: `(schedule name, next tick, paused, live)`.
    type Seen = (String, Option<i64>, bool, bool);

    /// The path of each named node.
    fn paths_of(
        dag: &Dag,
        schedules: &PathSchedules,
        held: (&[&str], &[&str]),
        names: &[&str],
    ) -> Vec<Option<Seen>> {
        let paths = Paths::of(dag);
        let ids = |names: &[&str]| -> HashSet<String> {
            names.iter().map(|name| id(dag, name)).collect()
        };
        let (paused, pinned) = (ids(held.0), ids(held.1));
        let evidence = PathEvidence {
            paths: &paths,
            schedules,
            paused: &paused,
            pinned: &pinned,
        };
        names
            .iter()
            .map(|name| {
                refresh_path(&id(dag, name), 0, evidence).map(|path| {
                    (
                        crate::targets::short_name(&path.schedule).to_string(),
                        path.next_tick
                            .as_deref()
                            .and_then(crate::time::parse_rfc3339),
                        path.paused,
                        path.live,
                    )
                })
            })
            .collect()
    }

    // 2026-11-17 14:57:00 UTC.
    const NOW: i64 = 1_794_927_420;
    const AT_15_00: i64 = NOW + 180;

    const SHAPES: &str = r#"
from barca import asset, sensor, task, Always, Manual, Schedule

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox(): ...

@asset(inputs={"inbox": inbox}, freshness=Always)
def orders(inbox): ...

@asset(inputs={"orders": orders}, freshness=Manual)
def reviewed(orders): ...

@asset(inputs={"reviewed": reviewed}, freshness=Always)
def report(reviewed): ...

@sensor
def feed(): ...

@asset(inputs={"feed": feed}, freshness=Schedule("0 * * * *"))
def hourly(feed): ...

@asset(inputs={"feed": feed}, freshness=Always)
def feed_rows(feed): ...

@sensor(freshness=Schedule("0 6 * * *"))
def daily(): ...

@asset(inputs={"daily": daily}, freshness=Always)
def daily_rows(daily): ...

@asset(inputs={"daily": daily}, freshness=Schedule("*/10 * * * *"))
def reads_daily(daily): ...

@asset(freshness=Always)
def loose(): ...

@asset(inputs={"loose": loose}, freshness=Always)
def below_loose(loose): ...
"#;

    #[test]
    fn a_path_is_a_schedule_whose_tick_has_the_node_as_a_target() {
        let d = dag(SHAPES);
        let schedules = firing(&d, NOW, &[]);
        let path = |name: &str| paths_of(&d, &schedules, (&[], &[]), &[name]).remove(0);
        let on = |schedule: &str, at: i64| Some((schedule.to_string(), Some(at), false, true));
        // A scheduled sensor: its own schedule, and the `Always` asset below it.
        assert_eq!(path("inbox"), on("inbox", AT_15_00));
        assert_eq!(path("orders"), on("inbox", AT_15_00));
        // A `Manual` asset in between: a tick reads it as it is, so it has no path; the
        // `Always` asset below it is still a target of the tick.
        assert_eq!(path("reviewed"), None);
        assert_eq!(path("report"), on("inbox", AT_15_00));
        // An unscheduled sensor polled as an input of a scheduled node: what is below it is a
        // target of the schedule that polls it (#978). The sensor itself is only an input.
        assert_eq!(path("feed"), None);
        assert_eq!(path("hourly"), on("hourly", AT_15_00));
        assert_eq!(path("feed_rows"), on("hourly", AT_15_00));
        // A sensor with its own, slower schedule: what is below it follows that schedule, not
        // the faster one that polls the sensor.
        let at_six = NOW + 15 * 3600 + 180;
        assert_eq!(path("daily_rows"), on("daily", at_six));
        assert_eq!(path("reads_daily"), on("reads_daily", AT_15_00));
        // No scheduled node above, and no polled sensor: nothing reaches it.
        assert_eq!(path("loose"), None);
        assert_eq!(path("below_loose"), None);
    }

    #[test]
    fn membership_is_the_schedulers_own_target_list() {
        let d = dag(SHAPES);
        let paths = Paths::of(&d);
        let jobs = jobs_from_dag(&d);
        for node in d.topo_order() {
            let mut by: Vec<String> = jobs
                .iter()
                .filter(|job| job.targets().iter().any(|target| target == node))
                .map(|job| job.id.clone())
                .collect();
            by.sort();
            assert_eq!(paths.schedules(node), by, "{node}");
        }
    }

    #[test]
    fn a_graph_with_no_schedule_has_no_path() {
        let d = dag(r#"
from barca import asset, sensor, Always

@sensor
def feed(): ...

@asset(inputs={"feed": feed}, freshness=Always)
def rows(feed): ...

@asset(inputs={"rows": rows})
def plain(rows): ...
"#);
        let schedules = firing(&d, NOW, &[]);
        assert_eq!(
            paths_of(&d, &schedules, (&[], &[]), &["feed", "rows", "plain"]),
            [None, None, None]
        );
    }

    #[test]
    fn a_paused_schedule_is_a_path_that_says_so() {
        let d = dag(SHAPES);
        // Until resumed: no next tick.
        let schedules = firing(&d, NOW, &[("inbox", None)]);
        assert_eq!(
            paths_of(&d, &schedules, (&[], &[]), &["orders"]),
            [Some(("inbox".to_string(), None, true, true))]
        );
        // Until 15:02: the first slot after it.
        let schedules = firing(&d, NOW, &[("inbox", Some(NOW + 300))]);
        assert_eq!(
            paths_of(&d, &schedules, (&[], &[]), &["orders"]),
            [Some(("inbox".to_string(), Some(NOW + 480), true, true))]
        );
    }

    #[test]
    fn a_schedule_no_scheduler_fires_is_a_path_with_no_next_tick() {
        let d = dag(SHAPES);
        // No scheduler published anything (this machine, a read-only server).
        let recorded = path_schedules(&schedule_views(ScheduleInputs {
            jobs: &jobs_from_dag(&d),
            zone: &Zone::Utc,
            now: chrono::DateTime::from_timestamp(NOW, 0).unwrap(),
            ticks: &HashMap::new(),
            failures: &HashMap::new(),
            pauses: &HashMap::new(),
            firing: &HashMap::new(),
            observed: &crate::liveness::Observed::default(),
        }));
        assert_eq!(
            paths_of(&d, &recorded, (&[], &[]), &["orders", "loose"]),
            [Some(("inbox".to_string(), None, false, false)), None]
        );
        // Held after `max_failures` failed ticks: published, and not firing.
        let mut schedules = firing(&d, NOW, &[]);
        let inbox = schedules.get_mut(&id(&d, "inbox")).unwrap();
        *inbox = PathSchedule {
            next_tick: None,
            live: false,
            failing: true,
            ..inbox.clone()
        };
        assert_eq!(
            paths_of(&d, &schedules, (&[], &[]), &["orders"]),
            [Some(("inbox".to_string(), None, false, false))]
        );
    }

    #[test]
    fn a_paused_or_pinned_node_has_no_path_and_what_is_below_it_keeps_its_own() {
        let d = dag(SHAPES);
        let schedules = firing(&d, NOW, &[]);
        let on = Some(("inbox".to_string(), Some(AT_15_00), false, true));
        assert_eq!(
            paths_of(&d, &schedules, (&["orders"], &[]), &["orders", "report"]),
            [None, on.clone()]
        );
        assert_eq!(
            paths_of(&d, &schedules, (&[], &["orders"]), &["orders", "report"]),
            [None, on]
        );
    }

    #[test]
    fn of_several_schedules_the_earliest_next_tick_is_the_path() {
        let d = dag(r#"
from barca import asset, sensor, Always, Schedule

@sensor(freshness=Schedule("0 * * * *"))
def slow(): ...

@sensor(freshness=Schedule("*/5 * * * *"))
def fast(): ...

@asset(inputs={"slow": slow, "fast": fast}, freshness=Always)
def both(slow, fast): ...
"#);
        // 14:57: fast fires at 15:00, and so does slow; the tie goes to the id.
        assert_eq!(
            paths_of(&d, &firing(&d, NOW, &[]), (&[], &[]), &["both"]),
            [Some(("fast".to_string(), Some(AT_15_00), false, true))]
        );
        // 15:01: fast at 15:05, slow at 16:00.
        assert_eq!(
            paths_of(&d, &firing(&d, NOW + 240, &[]), (&[], &[]), &["both"]),
            [Some(("fast".to_string(), Some(NOW + 480), false, true))]
        );
        // A paused schedule has no next tick: the one that fires is the path.
        assert_eq!(
            paths_of(
                &d,
                &firing(&d, NOW + 240, &[("fast", None)]),
                (&[], &[]),
                &["both"]
            ),
            [Some(("slow".to_string(), Some(NOW + 3780), false, true))]
        );
    }

    #[test]
    fn missed_ticks_are_the_paths_misses_since_the_newest_success() {
        let d = dag(SHAPES);
        let paths = Paths::of(&d);
        let (orders, inbox, hourly) = (id(&d, "orders"), id(&d, "inbox"), id(&d, "hourly"));
        let misses = [
            (inbox.clone(), 400.0),
            // A schedule that no longer has the node as a target.
            (hourly, 350.0),
            (inbox.clone(), 300.0),
            (inbox, 200.0),
        ];
        assert_eq!(missed_ticks(&orders, &paths, &misses, None, None), 3);
        // A run nobody scheduled succeeded at 250: the misses before it were made good.
        assert_eq!(missed_ticks(&orders, &paths, &misses, Some(250), None), 2);
        assert_eq!(missed_ticks(&orders, &paths, &misses, Some(400), None), 0);
        assert_eq!(missed_ticks(&orders, &paths, &[], None, None), 0);
    }

    #[test]
    fn classification_counts_the_ticks_that_missed_a_node_that_is_not_fresh() {
        use crate::health::{Checks, Evidence, classify};
        let d = dag(SHAPES);
        let paths = Paths::of(&d);
        let schedules = firing(&d, NOW, &[]);
        let (orders, inbox) = (id(&d, "orders"), id(&d, "inbox"));
        let checks = Checks {
            misses: HashMap::from([(
                orders.clone(),
                vec![(inbox.clone(), 400.0), (inbox.clone(), 300.0)],
            )]),
            ..Checks::default()
        };
        let none = HashSet::new();
        let path = |state: &str, last_success: Option<&str>| {
            let mut nodes: Vec<crate::status::NodeStatus> = vec![
                serde_json::from_value(serde_json::json!({
                    "id": orders, "name": "orders", "kind": "asset", "inputs": [],
                    "partitioned": false,
                    "cache": {"state": state, "reason": "", "detail": ""},
                    "last_materialization": null, "shape": null, "env": [],
                }))
                .unwrap(),
            ];
            let ages = HashMap::from([(
                orders.clone(),
                crate::db::NodeAge {
                    last_success_at: last_success.map(str::to_string),
                    ..Default::default()
                },
            )]);
            classify(
                &mut nodes,
                Evidence {
                    ages: &ages,
                    pinned: &none,
                    late: &none,
                    checks: &checks,
                    path: PathEvidence {
                        paths: &paths,
                        schedules: &schedules,
                        paused: &none,
                        pinned: &none,
                    },
                    timing: crate::estimates::Timing::none(),
                    now: NOW,
                },
            );
            let path = nodes.remove(0).refresh_path.expect("a path");
            assert_eq!(path.schedule, inbox);
            path.missed_ticks
        };
        // Out of date, and two finished ticks did not leave it fresh.
        assert_eq!(path("stale", None), 2);
        // It succeeded at 00:05:50 (350), by a run no tick started: one miss since.
        assert_eq!(path("stale", Some("1970-01-01 00:05:50")), 1);
        // Fresh: no tick missed it, whatever the record holds.
        assert_eq!(path("cached", None), 0);
    }

    #[test]
    fn the_line_says_what_will_refresh_it() {
        let path = |next: Option<&str>, paused, live, missed| RefreshPath {
            schedule: "pipeline.py:inbox".to_string(),
            next_tick: next.map(str::to_string),
            paused,
            live,
            state: Default::default(),
            missed_ticks: missed,
        };
        let line = |p: RefreshPath| p.line(NOW);
        assert_eq!(
            line(path(Some("2026-11-17T15:00:00Z"), false, true, 0)),
            "next tick 15:00 UTC by inbox"
        );
        assert_eq!(
            line(path(Some("2026-11-18T06:00:00Z"), false, true, 2)),
            "next tick 2026-11-18 06:00 UTC by inbox, missed 2 ticks"
        );
        assert_eq!(line(path(None, true, true, 0)), "inbox is paused");
        assert_eq!(
            line(path(Some("2026-11-17T15:05:00Z"), true, true, 0)),
            "inbox is paused, next tick 15:05 UTC"
        );
        assert_eq!(
            line(path(None, false, false, 1)),
            "inbox is not firing, missed 1 tick"
        );
    }
}
