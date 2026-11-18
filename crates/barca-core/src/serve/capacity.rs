//! A capacity shared by every run of one `barca serve` (`[server] capacity`,
//! `barca serve --capacity`, #628).
//!
//! Each run admits its steps against its own capacity (`[execution] capacity`, #436): the
//! `weight=` of the steps it runs at once add up to at most that. With a shared capacity a
//! step is also admitted against one budget that every run of the server draws from, so two
//! runs that start together cannot overload the machine between them, and a light run still
//! proceeds beside a heavy one while the heavy steps take what the light ones leave.
//!
//! The runs keep their own worker pools (a pool shared between runs is a standing non-goal):
//! this is a ledger of weights in the server's process that the run coordinators ask before
//! they lease a step to a worker, not a pool.
//!
//! Admission goes by a line, with the head reserved:
//!
//! - A run asks for the step at the head of its own ready queue, and only once that step fits
//!   its own capacity.
//! - When nobody is waiting and the step fits what is left, it is admitted at once.
//! - Otherwise the run waits in line (one place per run, for its head step). Only the run at
//!   the front of the line can be admitted, when its step fits; nobody behind it is admitted
//!   first, even a lighter step that would fit. So a heavy step is never overtaken by a stream
//!   of light ones: the capacity that frees up is kept for it until it fits.
//! - A step heavier than the whole capacity counts as all of it (it runs alone on the server,
//!   with a warning at planning, as it runs alone within a run).
//!
//! Which run is at the front depends on the [`Admission`] order (`[server] admission`, #1003):
//!
//! - [`Admission::RequestedFirst`] (the default): a requested run's step (`POST /runs`, the web
//!   UI, a `--server` client, an agent) goes ahead of a scheduled run's ([`Origin`]). Runs of
//!   one origin go in the order they first asked, so an earlier request is finished before a
//!   later one starts taking turns with it, and the same among scheduled runs. A scheduled step
//!   that has waited `max_wait` (`[server] scheduled_max_wait_seconds`) goes first, so a stream
//!   of requests cannot starve the schedules. Nothing running is preempted: a requested step
//!   still waits for the steps that hold the capacity when it asks.
//! - [`Admission::FirstCome`]: the oldest ticket is at the front, whatever its run.
//!
//! A run gives back the weight of a step that ended and asks for its next step in one call
//! ([`SharedCapacity::release_and_try_acquire`]), so no other run can take the weight in between:
//! a requested run keeps its place from one step to the next.
//!
//! Nothing waits on itself, so there is no deadlock: what is admitted is running (or about to
//! be leased to a worker that is starting), and running steps finish (or fail, or the run is
//! cancelled), giving their weight back. A run that ends gives back everything it held and
//! leaves the line ([`SharedCapacity::leave`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Where a run came from, as far as the line is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Someone asked for it: `POST /runs`, the web UI, a `--server` client, an agent.
    Requested,
    /// A cron tick of the server.
    Scheduled,
}

impl Origin {
    pub fn of(triggered_by: crate::outcome::TriggeredBy) -> Self {
        match triggered_by {
            crate::outcome::TriggeredBy::Schedule => Origin::Scheduled,
            _ => Origin::Requested,
        }
    }
}

/// How the line orders the runs that wait (`[server] admission`, #1003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// First come, first served: the oldest ticket is at the front, whatever its run.
    FirstCome,
    /// Requested runs first, each origin in the order its runs first asked; a scheduled ticket
    /// that has waited `max_wait` goes ahead of them.
    RequestedFirst { max_wait: Duration },
}

impl Admission {
    /// `[server] admission`'s value.
    pub fn as_str(self) -> &'static str {
        match self {
            Admission::FirstCome => "first_come",
            Admission::RequestedFirst { .. } => "requested_first",
        }
    }

    /// The bound on a scheduled step's wait, in seconds; `None` under first come, first served.
    pub fn max_wait_seconds(self) -> Option<u64> {
        match self {
            Admission::FirstCome => None,
            Admission::RequestedFirst { max_wait } => Some(max_wait.as_secs()),
        }
    }
}

impl Default for Admission {
    fn default() -> Self {
        crate::config::ServerAdmission::default().into()
    }
}

impl From<crate::config::ServerAdmission> for Admission {
    fn from(setting: crate::config::ServerAdmission) -> Self {
        match setting {
            crate::config::ServerAdmission::FirstCome => Admission::FirstCome,
            crate::config::ServerAdmission::RequestedFirst { max_wait_secs } => {
                Admission::RequestedFirst {
                    max_wait: Duration::from_secs(max_wait_secs),
                }
            }
        }
    }
}

/// The capacity every run of one server shares.
#[derive(Debug)]
pub struct SharedCapacity {
    total: u32,
    source: &'static str,
    admission: Admission,
    inner: Mutex<Inner>,
    /// Bumped whenever weight is given back or the line moves: the runs waiting try again.
    changed: tokio::sync::watch::Sender<u64>,
}

#[derive(Debug, Default)]
struct Inner {
    in_use: u32,
    /// Weight held by run id.
    held: BTreeMap<String, u32>,
    /// The runs waiting for their head step, in the order they asked.
    line: VecDeque<Ticket>,
    /// The order in which runs first asked, by run id: runs of one origin go in this order.
    order: BTreeMap<String, u64>,
    next_order: u64,
}

#[derive(Debug, Clone)]
struct Ticket {
    run_id: String,
    node: String,
    weight: u32,
    origin: Origin,
    since: Instant,
}

/// What `GET /health` and `GET /queue` show of the shared capacity.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CapacitySnapshot {
    /// The capacity (`[server] capacity`, `--capacity`).
    pub total: u32,
    /// Where it came from: `--capacity` or `[server] capacity`.
    pub source: String,
    /// The order of the line (`[server] admission`): `requested_first` or `first_come`.
    pub admission: String,
    /// Under `requested_first`, how long a scheduled step waits at most before it goes ahead of
    /// requested ones (`[server] scheduled_max_wait_seconds`); `null` under `first_come`.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub scheduled_max_wait_seconds: Option<u64>,
    /// The weight the steps running now (or being leased) take, all runs together.
    pub in_use: u32,
    /// The weight each run holds, by run id (runs holding nothing are left out).
    pub runs: Vec<HeldWeight>,
    /// The steps waiting for weight to come free, in the order they are admitted.
    pub waiting: Vec<WaitingStep>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HeldWeight {
    pub run_id: String,
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct WaitingStep {
    pub run_id: String,
    /// The step's node id (with its partition key, when it has one).
    pub node: String,
    /// The weight it waits for: its `weight=`, at most the capacity.
    pub weight: u32,
    pub waiting_seconds: f64,
    /// Whether its run was requested or scheduled: under `requested_first`, a requested run's
    /// step goes ahead of a scheduled run's that has not waited `scheduled_max_wait_seconds`.
    pub origin: Origin,
}

impl SharedCapacity {
    /// A capacity of `total` (at least 1), set by `source`, ordered by the default
    /// [`Admission`].
    pub fn new(total: usize, source: &'static str) -> Self {
        Self::with_admission(total, source, Admission::default())
    }

    /// A capacity of `total` whose line is ordered by `admission`.
    pub fn with_admission(total: usize, source: &'static str, admission: Admission) -> Self {
        let (changed, _) = tokio::sync::watch::channel(0);
        Self {
            total: u32::try_from(total.max(1)).unwrap_or(u32::MAX),
            source,
            admission,
            inner: Mutex::new(Inner::default()),
            changed,
        }
    }

    pub fn admission(&self) -> Admission {
        self.admission
    }

    /// Where `line[at]` stands: the smallest key is at the front.
    fn rank(&self, inner: &Inner, at: usize, now: Instant) -> (u8, u64, usize) {
        let t = &inner.line[at];
        let order = inner.order.get(&t.run_id).copied().unwrap_or(u64::MAX);
        match (self.admission, t.origin) {
            (Admission::FirstCome, _) => (0, 0, at),
            (Admission::RequestedFirst { max_wait }, Origin::Scheduled)
                if now.saturating_duration_since(t.since) >= max_wait =>
            {
                (0, order, at)
            }
            (_, Origin::Requested) => (1, order, at),
            (_, Origin::Scheduled) => (2, order, at),
        }
    }

    /// The ticket at the front of the line: the only one that can be admitted.
    fn front(&self, inner: &Inner, now: Instant) -> Option<usize> {
        (0..inner.line.len()).min_by_key(|&at| self.rank(inner, at, now))
    }

    pub fn total(&self) -> u32 {
        self.total
    }

    pub fn source(&self) -> &'static str {
        self.source
    }

    /// What a step of `weight` counts for: at least 1, at most the whole capacity.
    pub(crate) fn weight_of(&self, weight: u32) -> u32 {
        weight.clamp(1, self.total)
    }

    /// A receiver that sees every change after which a waiting run may be admitted.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn notify(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Ask for `weight` (already [`Self::weight_of`]) for `run_id`'s head step `node`, as a
    /// requested run: see [`Self::try_acquire_as`].
    pub fn try_acquire(&self, run_id: &str, node: &str, weight: u32) -> bool {
        self.try_acquire_as(run_id, node, weight, Origin::Requested)
    }

    /// Ask for `weight` for `run_id`'s head step `node`. The run takes (or keeps) its ticket in
    /// line; it is admitted (true) when its ticket is at the front ([`Admission`]) and the
    /// weight fits what is left, and then holds the weight until [`Self::release`]. Otherwise
    /// false: it asks again when [`Self::subscribe`] says something changed.
    pub fn try_acquire_as(&self, run_id: &str, node: &str, weight: u32, origin: Origin) -> bool {
        self.release_and_try_acquire_at(run_id, 0, node, weight, origin, Instant::now())
    }

    /// Give back `release` of what `run_id` holds and ask for its next head step, in one step:
    /// no other run is admitted to the weight in between, so the ordering of the line decides
    /// who gets it ([`Self::try_acquire_as`]).
    pub fn release_and_try_acquire(
        &self,
        run_id: &str,
        release: u32,
        node: &str,
        weight: u32,
        origin: Origin,
    ) -> bool {
        self.release_and_try_acquire_at(run_id, release, node, weight, origin, Instant::now())
    }

    fn release_and_try_acquire_at(
        &self,
        run_id: &str,
        release: u32,
        node: &str,
        weight: u32,
        origin: Origin,
        now: Instant,
    ) -> bool {
        let weight = self.weight_of(weight);
        let mut inner = self.inner.lock().unwrap();
        let inner = &mut *inner;
        let released = Self::give_back(inner, run_id, release);
        if !inner.order.contains_key(run_id) {
            inner.order.insert(run_id.to_string(), inner.next_order);
            inner.next_order += 1;
        }
        let was_in_line = match inner.line.iter().position(|t| t.run_id == run_id) {
            Some(at) => {
                let ticket = &mut inner.line[at];
                ticket.node = node.to_string();
                ticket.weight = weight;
                ticket.origin = origin;
                true
            }
            None => {
                inner.line.push_back(Ticket {
                    run_id: run_id.to_string(),
                    node: node.to_string(),
                    weight,
                    origin,
                    since: now,
                });
                false
            }
        };
        let front = self
            .front(inner, now)
            .expect("the ticket was just put in line");
        let admitted = inner.line[front].run_id == run_id && inner.in_use + weight <= self.total;
        if admitted {
            inner.line.remove(front);
            inner.in_use += weight;
            *inner.held.entry(run_id.to_string()).or_default() += weight;
        }
        // Weight came back that this run did not take, or the line moved: the next in line may
        // fit what is left.
        if released || (admitted && was_in_line) {
            self.notify();
        }
        admitted
    }

    /// Take `weight` off what `run_id` holds. True when anything was given back.
    fn give_back(inner: &mut Inner, run_id: &str, weight: u32) -> bool {
        if weight == 0 {
            return false;
        }
        let held = inner.held.get(run_id).copied().unwrap_or(0);
        let weight = weight.min(held);
        inner.in_use = inner.in_use.saturating_sub(weight);
        if held == weight {
            inner.held.remove(run_id);
        } else {
            inner.held.insert(run_id.to_string(), held - weight);
        }
        weight > 0
    }

    /// Take `run_id` out of the line: its head step no longer waits for this capacity (it does
    /// not fit the run's own, or the run has nothing ready).
    pub(crate) fn withdraw(&self, run_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.line.len();
        inner.line.retain(|t| t.run_id != run_id);
        let moved = inner.line.len() != before;
        drop(inner);
        if moved {
            self.notify();
        }
    }

    /// Give back `weight` of what `run_id` holds.
    pub fn release(&self, run_id: &str, weight: u32) {
        if weight == 0 {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        Self::give_back(&mut inner, run_id, weight);
        drop(inner);
        self.notify();
    }

    /// Take `weight` for `run_id` whether or not it fits: a step resumed after its
    /// `parallel()` branches, which had its weight while it waited. They gave back at least as
    /// much a moment before, so this goes over the capacity only when another run was admitted
    /// in between, and only until that step or this one ends.
    pub fn force(&self, run_id: &str, weight: u32) {
        if weight == 0 {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.in_use += weight;
        *inner.held.entry(run_id.to_string()).or_default() += weight;
    }

    /// `run_id` ended: give back all it holds and leave the line.
    pub fn leave(&self, run_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        let held = inner.held.remove(run_id).unwrap_or(0);
        inner.in_use = inner.in_use.saturating_sub(held);
        inner.line.retain(|t| t.run_id != run_id);
        inner.order.remove(run_id);
        drop(inner);
        self.notify();
    }

    /// The weight in use, by whom, and the steps waiting.
    pub fn snapshot(&self) -> CapacitySnapshot {
        let inner = self.inner.lock().unwrap();
        let now = Instant::now();
        let mut at: Vec<usize> = (0..inner.line.len()).collect();
        at.sort_by_key(|&i| self.rank(&inner, i, now));
        CapacitySnapshot {
            total: self.total,
            source: self.source.to_string(),
            admission: self.admission.as_str().to_string(),
            scheduled_max_wait_seconds: self.admission.max_wait_seconds(),
            in_use: inner.in_use,
            runs: inner
                .held
                .iter()
                .filter(|(_, w)| **w > 0)
                .map(|(run_id, weight)| HeldWeight {
                    run_id: run_id.clone(),
                    weight: *weight,
                })
                .collect(),
            waiting: at
                .into_iter()
                .map(|i| &inner.line[i])
                .map(|t| WaitingStep {
                    run_id: t.run_id.clone(),
                    node: t.node.clone(),
                    weight: t.weight,
                    waiting_seconds: (now.saturating_duration_since(t.since).as_secs_f64() * 10.0)
                        .round()
                        / 10.0,
                    origin: t.origin,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capacity whose line is first come, first served: what the line itself does, whatever
    /// the origin of the runs.
    fn first_come(total: usize, source: &'static str) -> SharedCapacity {
        SharedCapacity::with_admission(total, source, Admission::FirstCome)
    }

    #[test]
    fn a_step_that_fits_with_nobody_waiting_is_admitted_at_once() {
        let cap = first_come(4, "[server] capacity");
        assert!(cap.try_acquire("heavy", "rebuild", 3));
        assert!(cap.try_acquire("light", "sync", 1));
        let s = cap.snapshot();
        assert_eq!(s.in_use, 4);
        assert!(s.waiting.is_empty());
    }

    #[test]
    fn a_light_run_proceeds_while_a_heavy_run_holds_the_rest() {
        // Capacity 4: the rebuild's steps weigh 3, the sync's 1. The rebuild's second heavy
        // step does not fit its own run beside the first (its capacity is 4 too), so it never
        // asks; the sync's steps go through one after another beside the heavy one.
        let cap = first_come(4, "[server] capacity");
        assert!(cap.try_acquire("rebuild", "a", 3));
        for _ in 0..10 {
            assert!(cap.try_acquire("sync", "pull", 1));
            cap.release("sync", 1);
        }
        assert_eq!(cap.snapshot().in_use, 3);
    }

    #[test]
    fn a_heavy_step_is_not_overtaken_by_a_stream_of_light_ones() {
        let cap = first_come(4, "[server] capacity");
        // Two light runs fill half the capacity, then a heavy step asks for 3.
        assert!(cap.try_acquire("l1", "x", 1));
        assert!(cap.try_acquire("l2", "y", 1));
        assert!(!cap.try_acquire("heavy", "big", 3));
        // More light work arrives: it fits what is left, and still waits behind the heavy step.
        assert!(!cap.try_acquire("l3", "z", 1));
        assert!(!cap.try_acquire("l1", "x2", 1), "l1 asks again: behind too");
        let waiting: Vec<String> = cap
            .snapshot()
            .waiting
            .into_iter()
            .map(|w| w.run_id)
            .collect();
        assert_eq!(waiting, ["heavy", "l3", "l1"]);
        // One light step ends: 1 in use, the heavy step fits.
        cap.release("l2", 1);
        assert!(!cap.try_acquire("l3", "z", 1), "not first in line");
        assert!(cap.try_acquire("heavy", "big", 3));
        assert_eq!(cap.snapshot().in_use, 4);
        // Then the line goes on in order.
        cap.release("l1", 1);
        assert!(!cap.try_acquire("l1", "x2", 1), "l3 is ahead of it");
        assert!(cap.try_acquire("l3", "z", 1));
        cap.release("heavy", 3);
        assert!(cap.try_acquire("l1", "x2", 1));
        assert!(cap.snapshot().waiting.is_empty());
    }

    #[test]
    fn a_step_heavier_than_the_capacity_runs_alone_and_does_not_deadlock() {
        let cap = first_come(2, "--capacity");
        assert_eq!(cap.weight_of(9), 2);
        assert!(cap.try_acquire("a", "small", 1));
        assert!(!cap.try_acquire("b", "huge", 9));
        cap.release("a", 1);
        assert!(cap.try_acquire("b", "huge", 9));
        assert_eq!(cap.snapshot().in_use, 2);
        assert!(!cap.try_acquire("a", "small", 1));
        cap.release("b", 2);
        assert!(cap.try_acquire("a", "small", 1));
    }

    #[test]
    fn a_run_that_leaves_gives_back_its_weight_and_its_place() {
        let cap = first_come(3, "[server] capacity");
        assert!(cap.try_acquire("a", "x", 2));
        assert!(!cap.try_acquire("b", "y", 2));
        assert!(!cap.try_acquire("c", "z", 1));
        // b is cancelled while it waits: c is first now, and fits.
        cap.leave("b");
        assert!(cap.try_acquire("c", "z", 1));
        cap.leave("a");
        let s = cap.snapshot();
        assert_eq!(s.in_use, 1);
        assert_eq!(
            s.runs,
            [HeldWeight {
                run_id: "c".into(),
                weight: 1
            }]
        );
    }

    #[test]
    fn a_withdrawn_run_lets_the_next_one_in() {
        let cap = first_come(2, "[server] capacity");
        assert!(cap.try_acquire("a", "x", 2));
        assert!(!cap.try_acquire("b", "y", 2));
        assert!(!cap.try_acquire("c", "z", 1));
        cap.withdraw("b");
        cap.release("a", 1);
        assert!(cap.try_acquire("c", "z", 1));
    }

    #[test]
    fn waiting_runs_are_told_when_weight_comes_back() {
        let cap = first_come(1, "[server] capacity");
        let mut rx = cap.subscribe();
        rx.borrow_and_update();
        assert!(cap.try_acquire("a", "x", 1));
        assert!(!cap.try_acquire("b", "y", 1));
        assert!(!rx.has_changed().unwrap());
        cap.release("a", 1);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn force_goes_over_and_release_comes_back_under() {
        let cap = first_come(2, "[server] capacity");
        assert!(cap.try_acquire("a", "x", 2));
        cap.force("b", 2);
        assert_eq!(cap.snapshot().in_use, 4);
        assert!(!cap.try_acquire("c", "y", 1));
        cap.release("b", 2);
        cap.release("a", 2);
        assert!(cap.try_acquire("c", "y", 1));
    }

    // Requested runs first (#1003).

    const AGING: Admission = Admission::RequestedFirst {
        max_wait: Duration::from_secs(120),
    };
    use Origin::{Requested, Scheduled};

    impl SharedCapacity {
        fn acquire_at(
            &self,
            run_id: &str,
            node: &str,
            weight: u32,
            origin: Origin,
            now: Instant,
        ) -> bool {
            self.release_and_try_acquire_at(run_id, 0, node, weight, origin, now)
        }
    }

    fn at(base: Instant, secs: f64) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    #[test]
    fn fifo_keeps_the_alternation_of_today() {
        // Capacity 1: the scheduled run holds it, the requested run waits; the scheduled run's
        // next step asks after the requested one, so the two take turns.
        let cap = SharedCapacity::with_admission(1, "[server] capacity", Admission::FirstCome);
        assert!(cap.try_acquire_as("sched", "s1", 1, Scheduled));
        assert!(!cap.try_acquire_as("req", "r1", 1, Requested));
        cap.release("sched", 1);
        assert!(
            !cap.try_acquire_as("sched", "s2", 1, Scheduled),
            "behind req"
        );
        assert!(cap.try_acquire_as("req", "r1", 1, Requested));
        cap.release("req", 1);
        assert!(
            !cap.try_acquire_as("req", "r2", 1, Requested),
            "behind sched"
        );
        assert!(cap.try_acquire_as("sched", "s2", 1, Scheduled));
    }

    #[test]
    fn a_requested_step_goes_ahead_of_a_waiting_scheduled_one() {
        let cap = SharedCapacity::with_admission(1, "[server] capacity", AGING);
        // The scheduled step running is not preempted: the requested one waits for it.
        assert!(cap.try_acquire_as("sched", "s1", 1, Scheduled));
        assert!(!cap.try_acquire_as("req", "r1", 1, Requested));
        cap.release("sched", 1);
        // The scheduled run asks first (it just released), and still goes behind.
        assert!(!cap.try_acquire_as("sched", "s2", 1, Scheduled));
        assert!(cap.try_acquire_as("req", "r1", 1, Requested));
        // From then on the requested run keeps the capacity, step after step.
        for n in 2..10 {
            cap.release("req", 1);
            assert!(cap.try_acquire_as("req", &format!("r{n}"), 1, Requested));
            assert!(!cap.try_acquire_as("sched", "s2", 1, Scheduled));
        }
        let waiting: Vec<String> = cap
            .snapshot()
            .waiting
            .into_iter()
            .map(|w| w.run_id)
            .collect();
        assert_eq!(waiting, ["sched"]);
        // Done: the scheduled run goes on.
        cap.leave("req");
        assert!(cap.try_acquire_as("sched", "s2", 1, Scheduled));
    }

    #[test]
    fn requested_runs_go_in_the_order_they_first_asked() {
        // The earlier request is finished first, not interleaved with the later one.
        let cap = SharedCapacity::with_admission(1, "[server] capacity", AGING);
        assert!(cap.try_acquire_as("r1", "a1", 1, Requested));
        assert!(!cap.try_acquire_as("r2", "b1", 1, Requested));
        assert!(!cap.try_acquire_as("sched", "s1", 1, Scheduled));
        for n in 2..5 {
            // r1 asks again as it releases (the engine does both in one go); r2 and the
            // scheduled run, woken by the release, ask after it.
            cap.release("r1", 1);
            assert!(cap.try_acquire_as("r1", &format!("a{n}"), 1, Requested));
            assert!(
                !cap.try_acquire_as("r2", "b1", 1, Requested),
                "r1 first asked first"
            );
            assert!(!cap.try_acquire_as("sched", "s1", 1, Scheduled));
        }
        let waiting: Vec<String> = cap
            .snapshot()
            .waiting
            .into_iter()
            .map(|w| w.run_id)
            .collect();
        assert_eq!(waiting, ["r2", "sched"]);
        cap.leave("r1");
        assert!(!cap.try_acquire_as("sched", "s1", 1, Scheduled));
        assert!(cap.try_acquire_as("r2", "b1", 1, Requested));
    }

    #[test]
    fn a_heavy_requested_step_keeps_the_front_against_light_scheduled_ones() {
        // Capacity 4, 3 in use by a scheduled step: a requested step of 2 waits, and a scheduled
        // step of 1, which would fit, does not go first.
        let cap = SharedCapacity::with_admission(4, "[server] capacity", AGING);
        assert!(cap.try_acquire_as("big", "x", 3, Scheduled));
        assert!(!cap.try_acquire_as("req", "r", 2, Requested));
        assert!(!cap.try_acquire_as("small", "y", 1, Scheduled));
        cap.release("big", 3);
        assert!(!cap.try_acquire_as("small", "y", 1, Scheduled));
        assert!(cap.try_acquire_as("req", "r", 2, Requested));
        // What is left fits the scheduled step now that nothing ranks ahead of it.
        assert!(cap.try_acquire_as("small", "y", 1, Scheduled));
        assert_eq!(cap.snapshot().in_use, 3);
    }

    #[test]
    fn aging_lets_a_scheduled_step_through_once_it_has_waited_max_wait() {
        let base = Instant::now();
        let cap = SharedCapacity::with_admission(1, "[server] capacity", AGING);
        assert!(cap.acquire_at("req", "r0", 1, Requested, at(base, 0.0)));
        assert!(!cap.acquire_at("sched", "s", 1, Scheduled, at(base, 0.5)));
        // A steady stream of requested steps of 10 s each.
        let mut t = 10.0;
        while t < 120.5 {
            cap.release("req", 1);
            assert!(
                cap.acquire_at("req", "r", 1, Requested, at(base, t)),
                "at {t}"
            );
            t += 10.0;
        }
        // At 130 s the scheduled step has waited 129.5 s >= 120 s: it goes first.
        cap.release("req", 1);
        assert!(!cap.acquire_at("req", "r", 1, Requested, at(base, t)));
        assert!(cap.acquire_at("sched", "s", 1, Scheduled, at(base, t)));
    }

    #[test]
    fn a_run_that_leaves_forgets_its_order() {
        let cap = SharedCapacity::with_admission(1, "[server] capacity", AGING);
        assert!(cap.try_acquire_as("r1", "a", 1, Requested));
        cap.leave("r1");
        assert!(cap.inner.lock().unwrap().order.is_empty());
    }

    // A simulation of runs at one capacity: virtual time, every step known in advance.

    #[derive(Clone)]
    struct SimRun {
        origin: Origin,
        arrive: f64,
        weight: u32,
        steps: Vec<f64>,
    }

    #[derive(Clone, Debug, Default)]
    struct SimOut {
        finish: f64,
        /// How long each step waited for the capacity.
        waits: Vec<f64>,
    }

    fn run(origin: Origin, arrive: f64, steps: usize, secs: f64) -> SimRun {
        SimRun {
            origin,
            arrive,
            weight: 1,
            steps: vec![secs; steps],
        }
    }

    /// How a run whose step ended gives the weight back, and who asks first then.
    #[derive(Clone, Copy, PartialEq)]
    enum Handoff {
        /// As the engine does: the weight goes back with the run's next ask
        /// ([`SharedCapacity::release_and_try_acquire`]), and that run asks first.
        WithAsk,
        /// The same, but the other runs, woken by something else, ask before it.
        WithAskOthersFirst,
        /// The weight goes back on its own ([`SharedCapacity::release`]) and the other runs ask
        /// before the run whose step ended: the gap the prototype of #1003 had.
        BareOthersFirst,
    }
    use Handoff::*;

    /// Steps end and runs arrive at event times. Runs ask in the order [`Handoff`] says, then
    /// arrivals, then every run waiting, until nobody more is admitted.
    fn simulate(
        total: usize,
        admission: Admission,
        runs: &[SimRun],
        handoff: Handoff,
    ) -> Vec<SimOut> {
        let cap = SharedCapacity::with_admission(total, "sim", admission);
        let base = Instant::now();
        let n = runs.len();
        let id = |r: usize| format!("run{r}");
        let (mut next, mut busy, mut arrived) = (vec![0; n], vec![false; n], vec![false; n]);
        let mut owed = vec![0u32; n];
        let mut asked: Vec<Option<f64>> = vec![None; n];
        let mut out = vec![SimOut::default(); n];
        let mut running: Vec<(f64, usize)> = Vec::new();
        let mut t = 0.0;
        loop {
            let mut askers = Vec::new();
            let mut released = Vec::new();
            running.retain(|&(end, r)| {
                if end <= t + 1e-9 {
                    released.push(r);
                    false
                } else {
                    true
                }
            });
            for &r in &released {
                busy[r] = false;
                next[r] += 1;
                if next[r] == runs[r].steps.len() {
                    out[r].finish = t;
                    cap.leave(&id(r));
                } else if handoff == BareOthersFirst {
                    cap.release(&id(r), runs[r].weight.min(total as u32));
                } else {
                    owed[r] += runs[r].weight.min(total as u32);
                }
            }
            if handoff == WithAsk {
                askers.extend(released.iter().copied());
            }
            for r in 0..n {
                if !arrived[r] && runs[r].arrive <= t + 1e-9 {
                    arrived[r] = true;
                    askers.push(r);
                }
            }
            let line: Vec<usize> = (0..n).filter(|r| !askers.contains(r)).collect();
            askers.extend(line);
            loop {
                let mut admitted = false;
                for &r in &askers {
                    if !arrived[r] || busy[r] || next[r] == runs[r].steps.len() {
                        continue;
                    }
                    let since = *asked[r].get_or_insert(t);
                    let node = format!("{r}:{}", next[r]);
                    let now = base + Duration::from_secs_f64(t);
                    let give_back = std::mem::take(&mut owed[r]);
                    if cap.release_and_try_acquire_at(
                        &id(r),
                        give_back,
                        &node,
                        runs[r].weight,
                        runs[r].origin,
                        now,
                    ) {
                        busy[r] = true;
                        out[r].waits.push(t - since);
                        asked[r] = None;
                        running.push((t + runs[r].steps[next[r]], r));
                        admitted = true;
                    }
                }
                if !admitted {
                    break;
                }
            }
            let ends = running.iter().map(|&(e, _)| e);
            let arrivals = (0..n).filter(|&r| !arrived[r]).map(|r| runs[r].arrive);
            match ends.chain(arrivals).reduce(f64::min) {
                Some(next_t) => t = next_t,
                None => return out,
            }
        }
    }

    #[test]
    fn a_requested_run_overlapping_a_scheduled_one_is_delayed_by_one_step_at_most() {
        // #951: capacity 1, a scheduled run of 20 steps of 2 s, a request of 10 steps of 1 s
        // 3.5 s in. Alone the request takes 10 s.
        let runs = [run(Scheduled, 0.0, 20, 2.0), run(Requested, 3.5, 10, 1.0)];
        let fifo = simulate(1, Admission::FirstCome, &runs, WithAsk);
        let first = simulate(1, AGING, &runs, WithAsk);
        // Today: one scheduled step between each two requested steps, 9 x 2 s more, plus the
        // 0.5 s left of the scheduled step running when it came.
        assert_eq!(fifo[1].finish - 3.5, 10.0 + 9.0 * 2.0 + 0.5);
        // Requested first: only the step running when it came.
        assert_eq!(first[1].finish - 3.5, 10.0 + 0.5);
        // The scheduled run ends when it would have anyway: the same work, in another order.
        assert_eq!(fifo[0].finish, 50.0);
        assert_eq!(first[0].finish, 50.0);
    }

    #[test]
    fn two_requests_together_finish_one_after_the_other() {
        let runs = [run(Requested, 0.0, 10, 1.0), run(Requested, 2.0, 10, 1.0)];
        let fifo = simulate(1, Admission::FirstCome, &runs, WithAsk);
        let first = simulate(1, AGING, &runs, WithAsk);
        // Today the first runs alone until the second asks (3 steps), then they take turns.
        assert_eq!((fifo[0].finish, fifo[1].finish), (17.0, 20.0));
        assert_eq!((first[0].finish, first[1].finish), (10.0, 20.0));
    }

    /// A steady stream of requested runs (each 10 steps of 3 s, the next one asking as the last
    /// one ends) beside a scheduled run of 20 steps of 1 s from the start.
    fn stream() -> Vec<SimRun> {
        let mut runs = vec![run(Scheduled, 0.5, 20, 1.0)];
        for k in 0..60 {
            runs.push(run(Requested, k as f64 * 30.0, 10, 3.0));
        }
        runs
    }

    #[test]
    fn aging_bounds_a_scheduled_steps_wait_under_a_stream_of_requests() {
        let out = simulate(1, AGING, &stream(), WithAsk);
        // Each scheduled step waits at most max_wait plus the requested step running then.
        let worst = out[0].waits.iter().copied().fold(0.0, f64::max);
        assert!(worst <= 120.0 + 3.0, "worst wait {worst}");
        assert!(out[0].finish > 0.0, "the scheduled run finished");
        // Strict priority without aging would never have let it finish while the stream ran.
        let strict = Admission::RequestedFirst {
            max_wait: Duration::from_secs(100_000),
        };
        let out = simulate(1, strict, &stream(), WithAsk);
        assert!(out[0].finish >= 1800.0, "starved until the stream ended");
    }

    #[test]
    fn a_wake_up_cannot_take_the_weight_a_requested_run_gives_back() {
        // The weight of a requested run's step that ended goes back with its next ask, so a
        // scheduled run that asks first finds it still held and waits behind; the requested
        // run keeps its place. With a bare release before the ask (the prototype), the
        // scheduled run got in and the two took turns as under first come, first served.
        let runs = [run(Scheduled, 0.0, 20, 2.0), run(Requested, 3.5, 10, 1.0)];
        let first = simulate(1, AGING, &runs, WithAsk);
        let raced = simulate(1, AGING, &runs, WithAskOthersFirst);
        assert_eq!(raced[1].finish, first[1].finish);
        assert_eq!(raced[1].finish - 3.5, 10.0 + 0.5);
        let bare = simulate(1, AGING, &runs, BareOthersFirst);
        let fifo = simulate(1, Admission::FirstCome, &runs, WithAsk);
        assert_eq!(bare[1].finish, fifo[1].finish);
    }

    #[test]
    fn a_scheduled_run_asking_between_two_requested_steps_waits() {
        // The engine's handoff, step by step: the requested run holds the capacity, the
        // scheduled run asks (woken by anything), then the requested run gives back and asks.
        let cap = SharedCapacity::with_admission(1, "[server] capacity", AGING);
        assert!(cap.try_acquire_as("sched", "s1", 1, Scheduled));
        assert!(!cap.try_acquire_as("req", "r1", 1, Requested));
        assert!(!cap.release_and_try_acquire("sched", 1, "s2", 1, Scheduled));
        assert!(cap.try_acquire_as("req", "r1", 1, Requested));
        for n in 2..6 {
            assert!(
                !cap.try_acquire_as("sched", "s2", 1, Scheduled),
                "still held"
            );
            assert!(cap.release_and_try_acquire("req", 1, &format!("r{n}"), 1, Requested));
        }
        assert_eq!(cap.snapshot().in_use, 1);
        // The requested run's last step ends with nothing more to ask: the scheduled one goes.
        cap.leave("req");
        assert!(cap.try_acquire_as("sched", "s2", 1, Scheduled));
    }

    #[test]
    fn requested_first_with_weights_and_a_capacity_above_one() {
        // Capacity 4. A scheduled run of weight-2 steps and a requested run of weight-3 steps:
        // the requested steps run one after another; a scheduled step runs beside one only
        // when it fits what is left, never ahead of the requested step that waits.
        let cap = SharedCapacity::with_admission(4, "[server] capacity", AGING);
        assert!(cap.try_acquire_as("sched", "s1", 2, Scheduled));
        assert!(!cap.try_acquire_as("req", "r1", 3, Requested), "2 + 3 > 4");
        // The scheduled step ends; its next asks with the release and goes behind.
        assert!(!cap.release_and_try_acquire("sched", 2, "s2", 2, Scheduled));
        assert!(cap.try_acquire_as("req", "r1", 3, Requested));
        assert!(
            !cap.try_acquire_as("sched", "s2", 2, Scheduled),
            "3 + 2 > 4"
        );
        for n in 2..5 {
            assert!(cap.release_and_try_acquire("req", 3, &format!("r{n}"), 3, Requested));
            assert!(!cap.try_acquire_as("sched", "s2", 2, Scheduled));
        }
        let s = cap.snapshot();
        assert_eq!(s.admission, "requested_first");
        assert_eq!(s.scheduled_max_wait_seconds, Some(120));
        assert_eq!(
            s.waiting
                .iter()
                .map(|w| (w.run_id.as_str(), w.weight, w.origin))
                .collect::<Vec<_>>(),
            [("sched", 2, Scheduled)]
        );
        // A light requested step goes first, and the scheduled step that waits fits beside it.
        assert!(cap.release_and_try_acquire("req", 3, "r5", 1, Requested));
        assert!(cap.try_acquire_as("sched", "s2", 2, Scheduled));
        assert_eq!(cap.snapshot().in_use, 3);
    }

    #[test]
    fn scheduled_runs_go_in_the_order_they_first_asked() {
        let cap = SharedCapacity::with_admission(1, "[server] capacity", AGING);
        assert!(cap.try_acquire_as("s1", "a1", 1, Scheduled));
        assert!(!cap.try_acquire_as("s2", "b1", 1, Scheduled));
        for n in 2..5 {
            assert!(cap.release_and_try_acquire("s1", 1, &format!("a{n}"), 1, Scheduled));
            assert!(!cap.try_acquire_as("s2", "b1", 1, Scheduled));
        }
        cap.leave("s1");
        assert!(cap.try_acquire_as("s2", "b1", 1, Scheduled));
    }

    #[test]
    fn first_come_keeps_scheduled_and_requested_alike() {
        let cap = first_come(1, "[server] capacity");
        let s = cap.snapshot();
        assert_eq!(
            (s.admission.as_str(), s.scheduled_max_wait_seconds),
            ("first_come", None)
        );
        assert!(cap.try_acquire_as("s1", "a1", 1, Scheduled));
        assert!(!cap.try_acquire_as("r", "b1", 1, Requested));
        // The scheduled run's next step asks with its release, after the requested one: it
        // waits behind it.
        assert!(!cap.release_and_try_acquire("s1", 1, "a2", 1, Scheduled));
        assert!(cap.try_acquire_as("r", "b1", 1, Requested));
    }

    /// The numbers in the #951 report: `cargo test -p barca-core --lib sim_report -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "prints a table"]
    fn sim_report() {
        let rules: [(&str, Admission); 5] = [
            ("fifo", Admission::FirstCome),
            (
                "aging 60s",
                Admission::RequestedFirst {
                    max_wait: Duration::from_secs(60),
                },
            ),
            ("aging 120s", AGING),
            (
                "aging 300s",
                Admission::RequestedFirst {
                    max_wait: Duration::from_secs(300),
                },
            ),
            (
                "strict",
                Admission::RequestedFirst {
                    max_wait: Duration::from_secs(1_000_000),
                },
            ),
        ];
        // The issue's workload: a request of 85 steps averaging ~1 s (86.5 s of steps), a large
        // scheduled run of 110 steps averaging ~3.6 s (400 s), started 0-300 s before it.
        let issue = |lead: f64| vec![run(Scheduled, 0.0, 110, 3.6), run(Requested, lead, 85, 1.0)];
        println!("issue workload: request 85 x 1 s (86.5 s alone), scheduled 110 x 3.6 s");
        println!(
            "{:<11} {:>5} {:>9} {:>9} {:>11}",
            "rule", "lead", "req wall", "req +", "sched wall"
        );
        for (name, rule) in rules {
            for lead in [10.0, 100.0, 250.0, 330.0] {
                let out = simulate(1, rule, &issue(lead), WithAsk);
                println!(
                    "{name:<11} {lead:>5} {:>9.1} {:>9.1} {:>11.1}",
                    out[1].finish - lead,
                    out[1].finish - lead - 85.0,
                    out[0].finish
                );
            }
        }
        println!("\nstream: requests of 10 x 3 s back to back for 30 min, scheduled 20 x 1 s");
        println!(
            "{:<11} {:>10} {:>15} {:>12}",
            "rule", "sched wall", "sched max wait", "req p50 wall"
        );
        for (name, rule) in rules {
            let out = simulate(1, rule, &stream(), WithAsk);
            let worst = out[0].waits.iter().copied().fold(0.0, f64::max);
            let mut walls: Vec<f64> = stream()
                .iter()
                .zip(&out)
                .skip(1)
                .map(|(r, o)| o.finish - r.arrive)
                .collect();
            walls.sort_by(f64::total_cmp);
            println!(
                "{name:<11} {:>10.1} {:>15.1} {:>12.1}",
                out[0].finish,
                worst,
                walls[walls.len() / 2]
            );
        }
    }
}
