//! The Datadog trace and span ids of a run and its steps: deterministic in the run id and the
//! step, so every process that sends or shows one (the coordinator, a worker's spans, a log
//! line, `GET /runs/{run_id}`) derives the same id without a message.
//! [`crate::telemetry::datadog`] sends them.

use sha2::{Digest, Sha256};

/// A non-zero 63-bit id derived from `parts`, so a run's ids are the same however often
/// it is exported.
pub(crate) fn span_id(parts: &[&str]) -> u64 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) >> 1).max(1)
}

/// A non-zero 64-bit id derived from `parts`: the upper half of a 128-bit trace id.
fn wide_id(parts: &[&str]) -> u64 {
    (span_id(parts) << 1 | 1).max(1)
}

/// The lower 64 bits of a run's trace id: the `trace_id` of its `barca.run` span, and the
/// `dd.trace_id` of a `BARCA_LOG=json` line with no node.
pub(crate) fn trace_id(run_id: &str) -> u64 {
    span_id(&["trace", run_id])
}

/// The upper 64 bits of a run's 128-bit trace id, sent as `_dd.p.tid` (16 hex digits).
pub(crate) fn trace_id_high(run_id: &str) -> u64 {
    wide_id(&["trace-high", run_id])
}

/// The id of a run's `barca.run` span.
pub(crate) fn run_span_id(run_id: &str) -> u64 {
    span_id(&["run", run_id])
}

/// The lower 64 bits of the trace id of the step `node` (a node id, with its partition key
/// when it has one) in a run: deterministic in the run and the step, so a worker's Python
/// spans, the step's log lines and the coordinator's root span agree without a message.
pub(crate) fn step_trace_id(run_id: &str, node: &str) -> u64 {
    span_id(&["step-trace", run_id, node])
}

/// The upper 64 bits of a step's 128-bit trace id.
pub(crate) fn step_trace_id_high(run_id: &str, node: &str) -> u64 {
    wide_id(&["step-trace-high", run_id, node])
}

/// The id of the `barca.step` span of `node` in a run: the root of the step's trace.
pub(crate) fn step_span_id(run_id: &str, node: &str) -> u64 {
    span_id(&["step", run_id, node])
}

/// The ids of the trace the step `node` (its display id) of a run was sent as, as Datadog's
/// UI writes them: the 128-bit trace id in 32 hex digits (what `https://app.<site>/apm/trace/`
/// takes) and the root `barca.step` span's id in decimal. Derived, never stored: a run's row
/// records only that it was traced (`runs.trace`), so these must stay the ids the run sent.
pub fn step_trace_ref(run_id: &str, node: &str) -> (String, String) {
    (
        format!(
            "{:016x}{:016x}",
            step_trace_id_high(run_id, node),
            step_trace_id(run_id, node)
        ),
        step_span_id(run_id, node).to_string(),
    )
}
