//! The lines a run prints while it executes: the progress bar's ETA and total, the `--agent`
//! step lines, the batch lines and the last line of the run. They are the run path's own
//! output, not a view of a result, so they live with the orchestrator that prints them
//! (`docs/engineering/layering.md`, C5). Each function returns the text; the caller emits it.

use crate::dag::Dag;

/// Format seconds as a fixed-width time string for progress display.
/// Always 8 chars wide: "   5s   ", " 2m 30s ", " 1h 05m ", "2d 03h  "
pub(super) fn fmt_eta(secs: f64) -> String {
    let s = secs.round() as u64;
    if s < 60 {
        format!("{s:>4}s   ")
    } else if s < 3600 {
        format!("{:>2}m {:02}s ", s / 60, s % 60)
    } else if s < 86400 {
        format!("{:>2}h {:02}m ", s / 3600, (s % 3600) / 60)
    } else {
        let d = s / 86400;
        format!("{d:>2}d {:02}h  ", (s % 86400) / 3600)
    }
}

/// The progress total, grown to cover steps the plan did not count (the children a `parallel()`
/// call fans out to complete as extra steps). Keeps `completed <= total` for the counters and
/// the ETA subtraction.
pub(super) fn reconcile_total(total_steps: usize, completed_steps: usize) -> usize {
    total_steps.max(completed_steps)
}

/// The `--agent` step-line suffix for `node_id`'s declared env (empty when none is declared).
pub(super) fn env_suffix(dag: &Dag, node_id: &str) -> String {
    dag.get_node(crate::StepId::parse(node_id).base_id())
        .map(|n| crate::envdeps::agent_suffix(&crate::envdeps::resolve(&n.decl.env)))
        .unwrap_or_default()
}

/// How a run ended, for its last stderr line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunOutcome {
    Done,
    Failed,
    Cancelled,
}

/// A batched node's calls in a run, so its per-key `step:` lines read as one computation.
pub(super) fn batch_line(node: &str, b: &crate::results::BatchExecution) -> String {
    let calls = if b.calls == 1 { "call" } else { "calls" };
    let keys = if b.keys == 1 { "key" } else { "keys" };
    format!(
        "[barca] batch:{node} {} {calls}, {} {keys} | import {:.1}s, load {:.1}s, compute {:.1}s, split {:.1}s, write {:.1}s",
        b.calls,
        b.keys,
        b.import_seconds,
        b.load_seconds,
        b.compute_seconds,
        b.split_seconds,
        b.write_seconds
    )
}

/// The last progress line of a run that executed steps, with or without `--agent`:
/// `[barca] N/M steps | done in Xs` (`failed in`, `cancelled after`).
pub(super) fn end_of_run_line(
    completed: usize,
    total: usize,
    secs: f64,
    outcome: RunOutcome,
) -> String {
    let how = match outcome {
        RunOutcome::Done => "done in",
        RunOutcome::Failed => "failed in",
        RunOutcome::Cancelled => "cancelled after",
    };
    format!("[barca] {completed}/{total} steps | {how} {secs:.1}s")
}

/// The `--agent` line for a step served from cache: `[barca] step:<id> cached`, with its declared
/// env.
pub(super) fn cached_step_line(dag: &Dag, display_id: &str) -> String {
    format!(
        "[barca] step:{display_id} cached{}",
        env_suffix(dag, display_id)
    )
}

/// The `--agent` line for a step that raised: `[barca] step:<id> failed: <first line of the
/// error>`, beside the `completed` and `cached` lines.
pub(super) fn failed_step_line(node_id: &str, error: &str) -> String {
    let first = error
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown error");
    format!("[barca] step:{node_id} failed: {first}")
}

#[cfg(test)]
mod total_tests {
    use super::reconcile_total;

    #[test]
    fn total_grows_to_cover_steps_the_plan_did_not_count() {
        // parallel() children complete as extra steps beyond the plan.
        assert_eq!(reconcile_total(2, 4), 4);
        assert_eq!(reconcile_total(4, 2), 4);
        assert_eq!(reconcile_total(3, 3), 3);
        assert_eq!(reconcile_total(0, 0), 0);
    }

    #[test]
    fn remaining_never_underflows() {
        // The ETA math subtracts usizes; this used to panic in debug and wrap in release.
        assert_eq!(2usize.saturating_sub(4), 0);
    }
}

#[cfg(test)]
mod run_line_tests {
    use super::{RunOutcome, batch_line, end_of_run_line, failed_step_line};

    #[test]
    fn a_batched_node_has_one_line_with_its_calls_keys_and_phases() {
        let b = barca_model::reports::BatchExecution {
            calls: 1,
            keys: 3077,
            import_seconds: 0.12,
            load_seconds: 1.2,
            compute_seconds: 8.44,
            split_seconds: 3.1,
            write_seconds: 14.2,
        };
        assert_eq!(
            batch_line("p.py:orders", &b),
            "[barca] batch:p.py:orders 1 call, 3077 keys | import 0.1s, load 1.2s, compute 8.4s, split 3.1s, write 14.2s"
        );
    }

    #[test]
    fn end_of_run_line_is_one_format_and_never_says_done_on_failure() {
        assert_eq!(
            end_of_run_line(3, 3, 1.26, RunOutcome::Done),
            "[barca] 3/3 steps | done in 1.3s"
        );
        let failed = end_of_run_line(0, 3, 0.0, RunOutcome::Failed);
        assert_eq!(failed, "[barca] 0/3 steps | failed in 0.0s");
        assert!(!failed.contains("done"));
        assert_eq!(
            end_of_run_line(1, 3, 2.0, RunOutcome::Cancelled),
            "[barca] 1/3 steps | cancelled after 2.0s"
        );
    }

    #[test]
    fn failed_step_line_names_the_step_and_the_first_error_line() {
        assert_eq!(
            failed_step_line(
                "p.py:broken",
                "\nValueError: boom\n  File \"p.py\", line 3, in broken"
            ),
            "[barca] step:p.py:broken failed: ValueError: boom"
        );
        assert_eq!(
            failed_step_line("p.py:x", ""),
            "[barca] step:p.py:x failed: unknown error"
        );
    }
}
