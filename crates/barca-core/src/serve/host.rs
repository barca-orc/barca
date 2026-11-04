//! The host of every run `barca serve` starts: the server's shared capacity (when it has one)
//! and its in-flight asset steps.

use std::sync::Arc;

use super::capacity::SharedCapacity;
use super::claims::{self, Claim, SharedSteps};
use crate::run_host::{RunHost, SharedBudget, StepClaim, StepKey};

/// A run of `barca serve`: admitted against the server's capacity as well as its own, and
/// waiting for an asset step another of the server's runs is computing instead of computing it
/// again.
#[derive(Debug)]
pub struct ServeHost {
    capacity: Option<Arc<SharedCapacity>>,
    steps: Arc<SharedSteps>,
}

impl ServeHost {
    pub fn new(capacity: Option<Arc<SharedCapacity>>, steps: Arc<SharedSteps>) -> Self {
        Self { capacity, steps }
    }
}

impl RunHost for ServeHost {
    fn server_owned(&self) -> bool {
        true
    }
    fn shared_capacity(&self) -> Option<SharedBudget> {
        self.capacity.as_ref().map(|c| SharedBudget {
            total: c.total(),
            source: c.source(),
        })
    }
    fn capacity_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.capacity.as_ref().map(|c| c.subscribe())
    }
    fn weight_of(&self, weight: u32) -> u32 {
        match &self.capacity {
            Some(c) => c.weight_of(weight),
            None => weight,
        }
    }
    fn capacity_in_use(&self) -> u32 {
        self.capacity.as_ref().map_or(0, |c| c.snapshot().in_use)
    }
    fn try_acquire(&self, run_id: &str, node: &str, weight: u32) -> bool {
        self.capacity
            .as_ref()
            .is_none_or(|c| c.try_acquire(run_id, node, weight))
    }
    fn withdraw(&self, run_id: &str) {
        if let Some(c) = &self.capacity {
            c.withdraw(run_id);
        }
    }
    fn release_capacity(&self, run_id: &str, weight: u32) {
        if let Some(c) = &self.capacity {
            c.release(run_id, weight);
        }
    }
    fn force_capacity(&self, run_id: &str, weight: u32) {
        if let Some(c) = &self.capacity {
            c.force(run_id, weight);
        }
    }
    fn leave_capacity(&self, run_id: &str) {
        if let Some(c) = &self.capacity {
            c.leave(run_id);
        }
    }
    fn shares_steps(&self) -> bool {
        true
    }
    fn claim_step(&self, run_id: &str, key: &StepKey) -> StepClaim {
        match self.steps.claim(run_id, key) {
            Claim::Mine => StepClaim::Mine,
            Claim::Theirs { owner, result } => StepClaim::Theirs {
                owner,
                settled: Box::pin(claims::settled(result)),
            },
        }
    }
    fn finish_step(&self, run_id: &str, key: &StepKey, artifact: serde_json::Value) {
        self.steps.finish(run_id, key, artifact);
    }
    fn release_step(&self, run_id: &str, key: &StepKey) {
        self.steps.release(run_id, key);
    }
    fn leave_steps(&self, run_id: &str) {
        self.steps.leave(run_id);
    }
}
