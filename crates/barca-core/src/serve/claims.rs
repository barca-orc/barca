//! Asset steps in flight on one `barca serve`, shared between its runs (#654).
//!
//! Two runs of a server that need the same asset step (the same node id, partition key
//! included, and the same run hash) compute it once. A run claims an asset step when it leases
//! it to a worker; another run that reaches the same step while the claim is held waits for the
//! claim instead of executing it, and takes the owner's result as a cache hit when the owner
//! finishes it. Tasks, sensors and `parallel()` branches are never shared: each invocation of a
//! task is its own work, and a branch belongs to the step that called `parallel()`.
//!
//! A run that was already going when the owner finished the step, and reaches it afterwards,
//! takes the owner's result too ([`SharedSteps::now`]): it looked for a cached result before
//! there was one, and would otherwise execute the step again. Two ticks that fire in the same
//! second and have a target in common are such runs. A run that starts after the step finished
//! is not given it from here: it finds it cached, or computes it again when it was asked to
//! (`--refresh`). The last [`FINISHED_KEPT`] finished steps are kept for this.
//!
//! A claim ends when the owner reports the step finished ([`SharedSteps::finish`]) or when the
//! step stops being leased to one of the owner's workers for any other reason: it failed (a
//! retry claims it again), its worker died, or the owner's run ended or was cancelled
//! ([`SharedSteps::release`], [`SharedSteps::leave`]). A run waiting on a claim that ends
//! without a result claims the step itself and executes it, so a failed or cancelled owner
//! never fails the runs that waited on it; the work done so far is lost and done again.
//!
//! Only steps leased to a worker are held, so the registry is bounded by the work in flight
//! and [`FINISHED_KEPT`], never by the number of partition keys a run plans. There is no deadlock: a leased step only
//! waits for its worker (and its own `parallel()` branches, which are never shared), never for
//! a claim, so every claim ends.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use tokio::sync::watch;

pub(crate) use crate::run_host::StepKey;

/// How many finished steps are remembered for the runs that were going when they finished.
pub(crate) const FINISHED_KEPT: usize = 4096;

/// The asset steps the runs of one server are executing now, and the ones they finished last.
#[derive(Debug, Default)]
pub struct SharedSteps {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    held: HashMap<StepKey, Held>,
    /// The steps finished last, by key, each with when it finished ([`Inner::clock`]).
    finished: HashMap<StepKey, Finished>,
    /// The keys of `finished`, oldest first. A key finished again is listed again; only its
    /// newest listing removes it.
    order: VecDeque<(StepKey, u64)>,
    /// Counts finished steps.
    clock: u64,
}

#[derive(Debug)]
struct Finished {
    owner: String,
    at: u64,
    artifact: serde_json::Value,
}

#[derive(Debug)]
struct Held {
    owner: String,
    /// Sends the owner's artifact when it finishes the step. Dropped without a value when the
    /// claim ends otherwise, which the waiters see as the claim abandoned.
    result: watch::Sender<Option<serde_json::Value>>,
}

/// The outcome of [`SharedSteps::claim`].
#[derive(Debug)]
pub(crate) enum Claim {
    /// The step is this run's to execute.
    Mine,
    /// Another run is executing it: wait with [`settled`].
    Theirs {
        owner: String,
        result: watch::Receiver<Option<serde_json::Value>>,
    },
}

impl SharedSteps {
    pub fn new() -> Self {
        Self::default()
    }

    /// A point in time for [`Self::claim`]: a run takes it when it starts.
    pub fn now(&self) -> u64 {
        self.inner.lock().unwrap().clock
    }

    /// Claim `key` for `run_id`, a run that started at `since` ([`Self::now`]). A claim
    /// `run_id` already holds is its own again. A step another run finished after `since` is
    /// that run's, already settled with its result.
    pub(crate) fn claim(&self, run_id: &str, key: &StepKey, since: u64) -> Claim {
        let mut inner = self.inner.lock().unwrap();
        match inner.held.get(key) {
            Some(held) if held.owner != run_id => {
                return Claim::Theirs {
                    owner: held.owner.clone(),
                    result: held.result.subscribe(),
                };
            }
            Some(_) => return Claim::Mine,
            None => {}
        }
        if let Some(done) = inner.finished.get(key)
            && done.owner != run_id
            && done.at > since
        {
            return Claim::Theirs {
                owner: done.owner.clone(),
                result: watch::channel(Some(done.artifact.clone())).1,
            };
        }
        inner.held.insert(
            key.clone(),
            Held {
                owner: run_id.to_string(),
                result: watch::channel(None).0,
            },
        );
        Claim::Mine
    }

    /// `run_id` finished `key` with `artifact`: the runs waiting on it take it as a cache hit,
    /// and so does a run going now that reaches it later.
    pub fn finish(&self, run_id: &str, key: &StepKey, artifact: serde_json::Value) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.held.get(key).is_some_and(|held| held.owner == run_id) {
            return;
        }
        let held = inner.held.remove(key).expect("checked");
        inner.clock += 1;
        let at = inner.clock;
        inner.finished.insert(
            key.clone(),
            Finished {
                owner: run_id.to_string(),
                at,
                artifact: artifact.clone(),
            },
        );
        inner.order.push_back((key.clone(), at));
        while inner.order.len() > FINISHED_KEPT {
            let (old, at) = inner.order.pop_front().expect("not empty");
            if inner.finished.get(&old).is_some_and(|done| done.at == at) {
                inner.finished.remove(&old);
            }
        }
        drop(inner);
        held.result.send_replace(Some(artifact));
    }

    /// `run_id` no longer executes `key` and has no result for it: a run waiting on it claims
    /// it and executes it.
    pub fn release(&self, run_id: &str, key: &StepKey) {
        let mut inner = self.inner.lock().unwrap();
        if inner.held.get(key).is_some_and(|held| held.owner == run_id) {
            inner.held.remove(key);
        }
    }

    /// Release every claim `run_id` holds (its run ended, or was cancelled).
    pub fn leave(&self, run_id: &str) {
        self.inner
            .lock()
            .unwrap()
            .held
            .retain(|_, held| held.owner != run_id);
    }

    /// How many steps are claimed now.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Wait for a claim another run holds to end: its artifact when the owner finished the step,
/// `None` when the claim ended without one (claim the step again then).
pub async fn settled(
    mut result: watch::Receiver<Option<serde_json::Value>>,
) -> Option<serde_json::Value> {
    loop {
        if let Some(artifact) = result.borrow_and_update().clone() {
            return Some(artifact);
        }
        if result.changed().await.is_err() {
            return result.borrow().clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(node: &str) -> StepKey {
        (node.to_string(), "h".to_string())
    }

    #[tokio::test]
    async fn a_waiter_gets_the_owners_result() {
        let steps = SharedSteps::new();
        assert!(matches!(steps.claim("a", &key("p.py:x"), 0), Claim::Mine));
        assert!(matches!(steps.claim("a", &key("p.py:x"), 0), Claim::Mine));
        let Claim::Theirs { owner, result } = steps.claim("b", &key("p.py:x"), 0) else {
            panic!("b should wait on a");
        };
        assert_eq!(owner, "a");
        steps.finish("a", &key("p.py:x"), serde_json::json!({"path": "x.json"}));
        assert_eq!(
            settled(result).await,
            Some(serde_json::json!({"path": "x.json"}))
        );
        assert!(steps.is_empty());
        // Finished, the step is the claim of a run that starts now (it finds it cached first).
        assert!(matches!(
            steps.claim("c", &key("p.py:x"), steps.now()),
            Claim::Mine
        ));
    }

    #[tokio::test]
    async fn a_run_going_when_the_step_finished_takes_the_result_and_a_later_run_does_not() {
        let steps = SharedSteps::new();
        // `a` and `b` start together; `a` computes the step and ends before `b` reaches it.
        let started = steps.now();
        assert!(matches!(
            steps.claim("a", &key("p.py:x"), started),
            Claim::Mine
        ));
        steps.finish("a", &key("p.py:x"), serde_json::json!({"path": "x.json"}));
        steps.leave("a");
        let Claim::Theirs { owner, result } = steps.claim("b", &key("p.py:x"), started) else {
            panic!("b was going when a finished the step");
        };
        assert_eq!(owner, "a");
        assert_eq!(
            settled(result).await,
            Some(serde_json::json!({"path": "x.json"}))
        );
        assert!(steps.is_empty(), "taking a finished result claims nothing");
        // The owner computes it again when it reaches it again; a run that starts now does too.
        assert!(matches!(
            steps.claim("a", &key("p.py:x"), started),
            Claim::Mine
        ));
        steps.release("a", &key("p.py:x"));
        assert!(matches!(
            steps.claim("c", &key("p.py:x"), steps.now()),
            Claim::Mine
        ));
    }

    #[test]
    fn only_the_last_finished_steps_are_kept() {
        let steps = SharedSteps::new();
        for n in 0..FINISHED_KEPT + 10 {
            let k = key(&format!("p.py:x[k={n}]"));
            steps.claim("a", &k, 0);
            steps.finish("a", &k, serde_json::Value::Null);
        }
        // Finished again: its older listing does not remove it.
        let last = key(&format!("p.py:x[k={}]", FINISHED_KEPT + 9));
        steps.claim("a", &last, 0);
        steps.finish("a", &last, serde_json::Value::Null);
        let inner = steps.inner.lock().unwrap();
        assert!(inner.finished.len() <= FINISHED_KEPT);
        assert!(!inner.finished.contains_key(&key("p.py:x[k=0]")));
        assert!(inner.finished.contains_key(&last));
    }

    #[tokio::test]
    async fn a_released_or_abandoned_claim_settles_without_a_result() {
        let steps = SharedSteps::new();
        steps.claim("a", &key("p.py:x"), 0);
        steps.claim("a", &key("p.py:y"), 0);
        let Claim::Theirs { result: x, .. } = steps.claim("b", &key("p.py:x"), 0) else {
            panic!()
        };
        let Claim::Theirs { result: y, .. } = steps.claim("b", &key("p.py:y"), 0) else {
            panic!()
        };
        steps.release("a", &key("p.py:x"));
        assert_eq!(settled(x).await, None);
        steps.leave("a");
        assert_eq!(settled(y).await, None);
        assert!(steps.is_empty());
    }

    #[test]
    fn only_the_owner_ends_a_claim_and_keys_and_hashes_are_separate() {
        let steps = SharedSteps::new();
        steps.claim("a", &key("p.py:x[k=1]"), 0);
        steps.release("b", &key("p.py:x[k=1]"));
        steps.finish("b", &key("p.py:x[k=1]"), serde_json::Value::Null);
        steps.leave("b");
        assert_eq!(steps.len(), 1);
        assert!(matches!(
            steps.claim("b", &key("p.py:x[k=2]"), 0),
            Claim::Mine
        ));
        let other_hash = ("p.py:x[k=1]".to_string(), "h2".to_string());
        assert!(matches!(steps.claim("b", &other_hash, 0), Claim::Mine));
    }
}
