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
//! Admission is first come, first served across runs, with the head reserved:
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
//! Nothing waits on itself, so there is no deadlock: what is admitted is running (or about to
//! be leased to a worker that is starting), and running steps finish (or fail, or the run is
//! cancelled), giving their weight back. A run that ends gives back everything it held and
//! leaves the line ([`SharedCapacity::leave`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::Serialize;

/// The capacity every run of one server shares.
#[derive(Debug)]
pub struct SharedCapacity {
    total: u32,
    source: &'static str,
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
}

#[derive(Debug, Clone)]
struct Ticket {
    run_id: String,
    node: String,
    weight: u32,
    since: std::time::Instant,
}

/// What `GET /health` and `GET /queue` show of the shared capacity.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CapacitySnapshot {
    /// The capacity (`[server] capacity`, `--capacity`).
    pub total: u32,
    /// Where it came from: `--capacity` or `[server] capacity`.
    pub source: String,
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
}

impl SharedCapacity {
    /// A capacity of `total` (at least 1), set by `source`.
    pub fn new(total: usize, source: &'static str) -> Self {
        let (changed, _) = tokio::sync::watch::channel(0);
        Self {
            total: u32::try_from(total.max(1)).unwrap_or(u32::MAX),
            source,
            inner: Mutex::new(Inner::default()),
            changed,
        }
    }

    pub fn total(&self) -> u32 {
        self.total
    }

    pub fn source(&self) -> &'static str {
        self.source
    }

    /// What a step of `weight` counts for: at least 1, at most the whole capacity.
    pub fn weight_of(&self, weight: u32) -> u32 {
        weight.clamp(1, self.total)
    }

    /// A receiver that sees every change after which a waiting run may be admitted.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn notify(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Ask for `weight` (already [`Self::weight_of`]) for `run_id`'s head step `node`. Admitted
    /// (true) when the run is first in line, or the line is empty, and the weight fits what is
    /// left; the weight is then held by the run until [`Self::release`]. Otherwise the run
    /// keeps (or takes) its place in line, for this step, and false is returned: it asks again
    /// when [`Self::subscribe`] says something changed.
    pub fn try_acquire(&self, run_id: &str, node: &str, weight: u32) -> bool {
        let weight = self.weight_of(weight);
        let mut inner = self.inner.lock().unwrap();
        let place = inner.line.iter().position(|t| t.run_id == run_id);
        let first = match place {
            Some(at) => at == 0,
            None => inner.line.is_empty(),
        };
        if first && inner.in_use + weight <= self.total {
            inner.in_use += weight;
            *inner.held.entry(run_id.to_string()).or_default() += weight;
            if place.is_some() {
                inner.line.pop_front();
                drop(inner);
                // The next in line may fit what is left.
                self.notify();
            }
            return true;
        }
        match place {
            Some(at) => {
                let ticket = &mut inner.line[at];
                ticket.node = node.to_string();
                ticket.weight = weight;
            }
            None => inner.line.push_back(Ticket {
                run_id: run_id.to_string(),
                node: node.to_string(),
                weight,
                since: std::time::Instant::now(),
            }),
        }
        false
    }

    /// Take `run_id` out of the line: its head step no longer waits for this capacity (it does
    /// not fit the run's own, or the run has nothing ready).
    pub fn withdraw(&self, run_id: &str) {
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
        let held = inner.held.get(run_id).copied().unwrap_or(0);
        let weight = weight.min(held);
        inner.in_use = inner.in_use.saturating_sub(weight);
        if held == weight {
            inner.held.remove(run_id);
        } else {
            inner.held.insert(run_id.to_string(), held - weight);
        }
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
        drop(inner);
        self.notify();
    }

    /// The weight in use, by whom, and the steps waiting.
    pub fn snapshot(&self) -> CapacitySnapshot {
        let inner = self.inner.lock().unwrap();
        CapacitySnapshot {
            total: self.total,
            source: self.source.to_string(),
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
            waiting: inner
                .line
                .iter()
                .map(|t| WaitingStep {
                    run_id: t.run_id.clone(),
                    node: t.node.clone(),
                    weight: t.weight,
                    waiting_seconds: (t.since.elapsed().as_secs_f64() * 10.0).round() / 10.0,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_that_fits_with_nobody_waiting_is_admitted_at_once() {
        let cap = SharedCapacity::new(4, "[server] capacity");
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
        let cap = SharedCapacity::new(4, "[server] capacity");
        assert!(cap.try_acquire("rebuild", "a", 3));
        for _ in 0..10 {
            assert!(cap.try_acquire("sync", "pull", 1));
            cap.release("sync", 1);
        }
        assert_eq!(cap.snapshot().in_use, 3);
    }

    #[test]
    fn a_heavy_step_is_not_overtaken_by_a_stream_of_light_ones() {
        let cap = SharedCapacity::new(4, "[server] capacity");
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
        let cap = SharedCapacity::new(2, "--capacity");
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
        let cap = SharedCapacity::new(3, "[server] capacity");
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
        let cap = SharedCapacity::new(2, "[server] capacity");
        assert!(cap.try_acquire("a", "x", 2));
        assert!(!cap.try_acquire("b", "y", 2));
        assert!(!cap.try_acquire("c", "z", 1));
        cap.withdraw("b");
        cap.release("a", 1);
        assert!(cap.try_acquire("c", "z", 1));
    }

    #[test]
    fn waiting_runs_are_told_when_weight_comes_back() {
        let cap = SharedCapacity::new(1, "[server] capacity");
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
        let cap = SharedCapacity::new(2, "[server] capacity");
        assert!(cap.try_acquire("a", "x", 2));
        cap.force("b", 2);
        assert_eq!(cap.snapshot().in_use, 4);
        assert!(!cap.try_acquire("c", "y", 1));
        cap.release("b", 2);
        cap.release("a", 2);
        assert!(cap.try_acquire("c", "y", 1));
    }
}
