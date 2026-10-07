//! When each target of a run is finished, while the run is still going.
//!
//! A run over several targets (`barca get a,b`, or the nodes `barca serve` fires at one cron
//! tick) ends when its slowest target does. A caller that cares about one target, such as the
//! scheduler deciding whether a node's previous run is still going, needs to know when that
//! target's own steps have ended. [`TargetProgress`] tracks that from what the run loop already
//! knows: which steps were served from cache, which were dispatched, and how each one ended.
//!
//! A target is finished once the last phase holding one of its steps has been decided and none
//! of its dispatched steps is still open. It finished well when none of them failed or was
//! skipped because something upstream of it failed.

use crate::planner::{ExecutionPlan, WorkerStream};
use crate::{RunEvent, StepId};

/// The last phase of `plan` holding a step of the node `base_id`.
fn last_phase_of(plan: &ExecutionPlan, base_id: &str) -> Option<usize> {
    plan.phases.iter().rposition(|phase| {
        phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .any(|st| st.step_id.base_id() == base_id)
    })
}

/// Progress of every target of one run. Each target is reported finished once.
#[derive(Debug)]
pub(crate) struct TargetProgress {
    targets: Vec<Target>,
    /// The last phase whose steps have been decided (cached or dispatched), once one has.
    decided: Option<usize>,
}

#[derive(Debug)]
struct Target {
    /// Base node id.
    id: String,
    /// The last phase of the plan with a step of this target; `None` when the plan has none.
    last_phase: Option<usize>,
    /// Dispatched steps (one per partition key) that have not ended yet.
    open: usize,
    /// A step of the target failed, or did not run because something upstream failed.
    failed: bool,
    reported: bool,
}

impl TargetProgress {
    /// Track `target_ids` (base node ids) through `plan`.
    pub(crate) fn new(target_ids: &[&str], plan: &ExecutionPlan) -> Self {
        Self::with_last_phases(target_ids.iter().map(|id| (*id, last_phase_of(plan, id))))
    }

    /// Track targets given as `(base node id, last phase with one of its steps)`.
    fn with_last_phases<'a>(targets: impl Iterator<Item = (&'a str, Option<usize>)>) -> Self {
        Self {
            targets: targets
                .map(|(id, last_phase)| Target {
                    id: id.to_string(),
                    last_phase,
                    open: 0,
                    failed: false,
                    reported: false,
                })
                .collect(),
            decided: None,
        }
    }

    fn target_mut(&mut self, base_id: &str) -> Option<&mut Target> {
        self.targets.iter_mut().find(|t| t.id == base_id)
    }

    /// The step `base_id` will not run in this run: something upstream of it failed.
    pub(crate) fn blocked(&mut self, base_id: &str) {
        if let Some(t) = self.target_mut(base_id) {
            t.failed = true;
        }
    }

    /// Phase `phase` has been decided: `dispatched` holds the steps handed to the worker pool
    /// (cached steps are not in it). Returns the events for the targets this finishes: the ones
    /// whose steps were all cached, or will not run.
    pub(crate) fn phase_decided(
        &mut self,
        phase: usize,
        dispatched: &[WorkerStream],
    ) -> Vec<RunEvent> {
        for step in dispatched.iter().flat_map(|s| &s.steps) {
            self.opened(step.step_id.base_id(), step.partition_keys.len().max(1));
        }
        self.decided = Some(phase);
        self.finished_events()
    }

    /// A dispatched step ended for good: it completed (`ok`), or it failed after its last
    /// attempt or was skipped. `node_id` is the step's display id (with its partition key).
    /// Returns the event for the target this finishes, if it does.
    pub(crate) fn step_ended(&mut self, node_id: &str, ok: bool) -> Vec<RunEvent> {
        self.closed(node_id, ok);
        self.finished_events()
    }

    /// `steps` steps of the node `base_id` (one per partition key) started.
    fn opened(&mut self, base_id: &str, steps: usize) {
        if let Some(t) = self.target_mut(base_id) {
            t.open += steps;
        }
    }

    /// One step of the node behind `node_id` ended.
    fn closed(&mut self, node_id: &str, ok: bool) {
        let step = StepId::parse(node_id);
        if let Some(t) = self.target_mut(step.base_id()) {
            t.open = t.open.saturating_sub(1);
            t.failed |= !ok;
        }
    }

    fn finished_events(&mut self) -> Vec<RunEvent> {
        let Some(decided) = self.decided else {
            return Vec::new();
        };
        self.take_finished(decided)
            .into_iter()
            .map(|(node_id, ok)| RunEvent::TargetFinished { node_id, ok })
            .collect()
    }

    /// The targets that became finished, as `(node id, finished well)`, now that every phase up
    /// to `decided_phase` has had its steps decided (cached or dispatched). Each target is
    /// returned once.
    fn take_finished(&mut self, decided_phase: usize) -> Vec<(String, bool)> {
        self.targets
            .iter_mut()
            .filter(|t| {
                !t.reported && t.open == 0 && t.last_phase.is_some_and(|p| p <= decided_phase)
            })
            .map(|t| {
                t.reported = true;
                (t.id.clone(), !t.failed)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Targets with the phase each one's step is in.
    fn progress(targets: &[(&str, usize)]) -> TargetProgress {
        TargetProgress::with_last_phases(targets.iter().map(|(id, phase)| (*id, Some(*phase))))
    }

    fn finished(id: &str, ok: bool) -> (String, bool) {
        (id.to_string(), ok)
    }

    #[test]
    fn a_cached_target_is_finished_as_soon_as_its_phase_is_decided() {
        let mut progress = progress(&[("p.py:fast", 0), ("p.py:slow", 0)]);
        // `fast` was served from cache, so only `slow` is dispatched.
        progress.opened("p.py:slow", 1);
        assert_eq!(progress.take_finished(0), vec![finished("p.py:fast", true)]);
        progress.closed("p.py:slow", true);
        assert_eq!(progress.take_finished(0), vec![finished("p.py:slow", true)]);
    }

    #[test]
    fn a_target_finishes_when_its_own_step_ends_not_when_the_run_does() {
        let mut progress = progress(&[("p.py:fast", 0), ("p.py:slow", 0)]);
        progress.opened("p.py:fast", 1);
        progress.opened("p.py:slow", 1);
        assert!(progress.take_finished(0).is_empty(), "both still running");
        progress.closed("p.py:fast", true);
        assert_eq!(
            progress.take_finished(0),
            vec![finished("p.py:fast", true)],
            "fast is finished while slow is still running"
        );
        assert!(progress.take_finished(0).is_empty(), "reported once");
    }

    #[test]
    fn a_failed_or_skipped_step_finishes_its_target_badly() {
        let mut progress = progress(&[("p.py:broken", 0), ("p.py:after", 0), ("p.py:slow", 0)]);
        for id in ["p.py:broken", "p.py:after", "p.py:slow"] {
            progress.opened(id, 1);
        }
        progress.closed("p.py:broken", false);
        progress.closed("p.py:after", false);
        assert_eq!(
            progress.take_finished(0),
            vec![
                finished("p.py:broken", false),
                finished("p.py:after", false)
            ]
        );
    }

    #[test]
    fn a_partitioned_target_finishes_after_its_last_key() {
        let mut progress = progress(&[("p.py:weekly", 0)]);
        progress.opened("p.py:weekly", 2);
        progress.closed("p.py:weekly[week=w1]", true);
        assert!(progress.take_finished(0).is_empty(), "one key still open");
        progress.closed("p.py:weekly[week=w2]", false);
        assert_eq!(
            progress.take_finished(0),
            vec![finished("p.py:weekly", false)],
            "one failed key fails the target"
        );
    }

    #[test]
    fn a_target_in_a_later_phase_waits_for_that_phase() {
        let mut progress = progress(&[("p.py:down", 1)]);
        assert!(
            progress.take_finished(0).is_empty(),
            "its own phase is not decided yet"
        );
        // Dropped before its phase is dispatched: an upstream step failed.
        progress.blocked("p.py:down");
        assert_eq!(
            progress.take_finished(1),
            vec![finished("p.py:down", false)]
        );
    }

    #[test]
    fn steps_that_are_not_targets_are_ignored() {
        let mut progress = progress(&[("p.py:t", 0)]);
        progress.opened("p.py:up", 1);
        progress.opened("p.py:t", 1);
        progress.closed("p.py:up", false);
        assert!(progress.take_finished(0).is_empty());
        progress.closed("p.py:t", true);
        assert_eq!(progress.take_finished(0), vec![finished("p.py:t", true)]);
    }

    #[test]
    fn a_target_the_plan_does_not_hold_is_never_reported() {
        let mut progress = TargetProgress::with_last_phases([("p.py:gone", None)].into_iter());
        assert!(progress.take_finished(9).is_empty());
    }

    #[test]
    fn the_run_loop_hooks_return_one_event_per_finished_target() {
        let mut progress = progress(&[("p.py:cached", 0), ("p.py:ran", 0), ("p.py:later", 1)]);
        // A step ending before any phase is decided finishes nothing.
        assert!(progress.step_ended("p.py:elsewhere", true).is_empty());

        // Phase 0 decided with nothing of `cached` dispatched: it is finished at once.
        progress.opened("p.py:ran", 1);
        let events = progress.phase_decided(0, &[]);
        assert!(
            matches!(&events[..], [RunEvent::TargetFinished { node_id, ok: true }] if node_id == "p.py:cached"),
            "{events:?}"
        );
        let events = progress.step_ended("p.py:ran", false);
        assert!(
            matches!(&events[..], [RunEvent::TargetFinished { node_id, ok: false }] if node_id == "p.py:ran"),
            "{events:?}"
        );
        // `later` waits for its own phase.
        let events = progress.phase_decided(1, &[]);
        assert!(
            matches!(&events[..], [RunEvent::TargetFinished { node_id, ok: true }] if node_id == "p.py:later"),
            "{events:?}"
        );
    }
}
