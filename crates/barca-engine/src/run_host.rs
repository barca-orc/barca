//! What a run asks of whatever hosts it: a capacity shared with other runs, and asset steps
//! other runs may be computing.
//!
//! A `barca get` or `barca run` on its own is hosted by [`LocalHost`], which shares nothing:
//! it has no shared capacity, so the worker pool never asks for one, and it shares no steps,
//! so every step is the run's own. Under `barca serve` every run is hosted by
//! `barca_core::serve::ServeHost`, which wraps the server's `barca_core::serve::capacity` (#628) and
//! `barca_core::serve::claims` (#654). The pool asks [`RunHost::shared_capacity`] and
//! [`RunHost::shares_steps`] once, when it starts, and keeps no per-run state for what the
//! host does not share, so a local run pays nothing for either.

use std::future::Future;
use std::pin::Pin;

/// What identifies shared work: the step's display id (with its partition key) and its run hash.
pub type StepKey = (String, String);

/// A wait for a step another run is computing: its artifact when that run finished it, `None`
/// when its claim ended without one.
pub(crate) type Settled = Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>;

/// The outcome of [`RunHost::claim_step`].
pub enum StepClaim {
    /// The step is this run's to execute.
    Mine,
    /// Another run is executing it: wait for `settled`.
    Theirs { owner: String, settled: Settled },
}

/// The capacity a host shares between its runs: its total weight and where it was set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedBudget {
    pub total: u32,
    pub source: &'static str,
}

/// What the local loop calls on the host of a run. The capacity operations are only called
/// when [`Self::shared_capacity`] is `Some`, the step operations only when
/// [`Self::shares_steps`]; their arguments are as `barca_core::serve::capacity::SharedCapacity`
/// and `barca_core::serve::claims::SharedSteps` take them.
pub trait RunHost: Send + Sync + std::fmt::Debug {
    /// Whether the run belongs to a server, which owns the final shutdown backup: the run's
    /// own end takes a routine backup instead.
    fn server_owned(&self) -> bool;

    /// The capacity the host's runs share, if any.
    fn shared_capacity(&self) -> Option<SharedBudget>;
    /// A receiver that sees every change after which a run waiting for capacity may be
    /// admitted.
    fn capacity_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>>;
    /// What a step of `weight` counts for against the shared capacity.
    fn weight_of(&self, weight: u32) -> u32;
    /// The weight in use now, all runs together.
    fn capacity_in_use(&self) -> u32;
    /// Ask for `weight` for `run_id`'s head step `node`: admitted, or `run_id` waits in line.
    fn try_acquire(&self, run_id: &str, node: &str, weight: u32) -> bool;
    /// `run_id`'s head step no longer waits for the shared capacity.
    fn withdraw(&self, run_id: &str);
    /// Give back `weight` of what `run_id` holds.
    fn release_capacity(&self, run_id: &str, weight: u32);
    /// Take `weight` for `run_id` whether or not it fits (a step resumed after `parallel()`).
    fn force_capacity(&self, run_id: &str, weight: u32);
    /// `run_id` ended: give back all it holds of the shared capacity and leave the line.
    fn leave_capacity(&self, run_id: &str);

    /// Whether the host's runs share in-flight asset steps.
    fn shares_steps(&self) -> bool;
    /// Claim `key` for `run_id` as it is leased to one of its workers.
    fn claim_step(&self, run_id: &str, key: &StepKey) -> StepClaim;
    /// `run_id` finished `key` with `artifact`: the runs waiting on it take it as a cache hit.
    fn finish_step(&self, run_id: &str, key: &StepKey, artifact: serde_json::Value);
    /// `run_id` no longer executes `key` and has no result for it.
    fn release_step(&self, run_id: &str, key: &StepKey);
    /// Release every step claim `run_id` holds.
    fn leave_steps(&self, run_id: &str);
}

/// A run on its own: no shared capacity (every step is admitted), no shared steps (every
/// step is the run's own), and the run owns its final backup.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalHost;

impl LocalHost {
    /// The host of a run on its own.
    pub fn shared() -> std::sync::Arc<dyn RunHost> {
        std::sync::Arc::new(Self)
    }
}

impl RunHost for LocalHost {
    fn server_owned(&self) -> bool {
        false
    }
    fn shared_capacity(&self) -> Option<SharedBudget> {
        None
    }
    fn capacity_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
    fn weight_of(&self, weight: u32) -> u32 {
        weight
    }
    fn capacity_in_use(&self) -> u32 {
        0
    }
    fn try_acquire(&self, _run_id: &str, _node: &str, _weight: u32) -> bool {
        true
    }
    fn withdraw(&self, _run_id: &str) {}
    fn release_capacity(&self, _run_id: &str, _weight: u32) {}
    fn force_capacity(&self, _run_id: &str, _weight: u32) {}
    fn leave_capacity(&self, _run_id: &str) {}
    fn shares_steps(&self) -> bool {
        false
    }
    fn claim_step(&self, _run_id: &str, _key: &StepKey) -> StepClaim {
        StepClaim::Mine
    }
    fn finish_step(&self, _run_id: &str, _key: &StepKey, _artifact: serde_json::Value) {}
    fn release_step(&self, _run_id: &str, _key: &StepKey) {}
    fn leave_steps(&self, _run_id: &str) {}
}
