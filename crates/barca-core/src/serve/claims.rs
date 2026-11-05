//! Asset steps in flight on one `barca serve`, shared between its runs (#654).
//!
//! Two runs of a server that need the same asset step (the same node id, partition key
//! included, and the same run hash) compute it once. A run claims an asset step when it leases
//! it to a worker; another run that reaches the same step while the claim is held waits for the
//! claim instead of executing it, and takes the owner's result as a cache hit when the owner
//! finishes it. Tasks, sensors and `parallel()` branches are never shared: each invocation of a
//! task is its own work, and a branch belongs to the step that called `parallel()`.
//!
//! A claim ends when the owner reports the step finished ([`SharedSteps::finish`]) or when the
//! step stops being leased to one of the owner's workers for any other reason: it failed (a
//! retry claims it again), its worker died, or the owner's run ended or was cancelled
//! ([`SharedSteps::release`], [`SharedSteps::leave`]). A run waiting on a claim that ends
//! without a result claims the step itself and executes it, so a failed or cancelled owner
//! never fails the runs that waited on it; the work done so far is lost and done again.
//!
//! Only steps leased to a worker are held, so the registry is bounded by the work in flight,
//! never by the number of partition keys a run plans. There is no deadlock: a leased step only
//! waits for its worker (and its own `parallel()` branches, which are never shared), never for
//! a claim, so every claim ends.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::watch;

pub(crate) use crate::run_host::StepKey;

/// The asset steps the runs of one server are executing now.
#[derive(Debug, Default)]
pub struct SharedSteps {
    inner: Mutex<HashMap<StepKey, Held>>,
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

    /// Claim `key` for `run_id`. A claim `run_id` already holds is its own again.
    pub(crate) fn claim(&self, run_id: &str, key: &StepKey) -> Claim {
        let mut inner = self.inner.lock().unwrap();
        match inner.get(key) {
            Some(held) if held.owner != run_id => Claim::Theirs {
                owner: held.owner.clone(),
                result: held.result.subscribe(),
            },
            Some(_) => Claim::Mine,
            None => {
                inner.insert(
                    key.clone(),
                    Held {
                        owner: run_id.to_string(),
                        result: watch::channel(None).0,
                    },
                );
                Claim::Mine
            }
        }
    }

    /// `run_id` finished `key` with `artifact`: the runs waiting on it take it as a cache hit.
    pub fn finish(&self, run_id: &str, key: &StepKey, artifact: serde_json::Value) {
        if let Some(held) = self.take(run_id, key) {
            held.result.send_replace(Some(artifact));
        }
    }

    /// `run_id` no longer executes `key` and has no result for it: a run waiting on it claims
    /// it and executes it.
    pub fn release(&self, run_id: &str, key: &StepKey) {
        drop(self.take(run_id, key));
    }

    /// Release every claim `run_id` holds (its run ended, or was cancelled).
    pub fn leave(&self, run_id: &str) {
        self.inner
            .lock()
            .unwrap()
            .retain(|_, held| held.owner != run_id);
    }

    /// How many steps are claimed now.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn take(&self, run_id: &str, key: &StepKey) -> Option<Held> {
        let mut inner = self.inner.lock().unwrap();
        if inner.get(key).is_some_and(|held| held.owner == run_id) {
            inner.remove(key)
        } else {
            None
        }
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
        assert!(matches!(steps.claim("a", &key("p.py:x")), Claim::Mine));
        assert!(matches!(steps.claim("a", &key("p.py:x")), Claim::Mine));
        let Claim::Theirs { owner, result } = steps.claim("b", &key("p.py:x")) else {
            panic!("b should wait on a");
        };
        assert_eq!(owner, "a");
        steps.finish("a", &key("p.py:x"), serde_json::json!({"path": "x.json"}));
        assert_eq!(
            settled(result).await,
            Some(serde_json::json!({"path": "x.json"}))
        );
        assert!(steps.is_empty());
        // Finished, the step is anyone's to claim (a later run finds it cached first).
        assert!(matches!(steps.claim("b", &key("p.py:x")), Claim::Mine));
    }

    #[tokio::test]
    async fn a_released_or_abandoned_claim_settles_without_a_result() {
        let steps = SharedSteps::new();
        steps.claim("a", &key("p.py:x"));
        steps.claim("a", &key("p.py:y"));
        let Claim::Theirs { result: x, .. } = steps.claim("b", &key("p.py:x")) else {
            panic!()
        };
        let Claim::Theirs { result: y, .. } = steps.claim("b", &key("p.py:y")) else {
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
        steps.claim("a", &key("p.py:x[k=1]"));
        steps.release("b", &key("p.py:x[k=1]"));
        steps.finish("b", &key("p.py:x[k=1]"), serde_json::Value::Null);
        steps.leave("b");
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps.claim("b", &key("p.py:x[k=2]")), Claim::Mine));
        let other_hash = ("p.py:x[k=1]".to_string(), "h2".to_string());
        assert!(matches!(steps.claim("b", &other_hash), Claim::Mine));
    }
}
