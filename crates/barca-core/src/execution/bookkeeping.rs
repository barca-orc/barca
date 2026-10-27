//! Collected result evidence and its persistence/report projections.
//!
//! Execution owns decisions and resource lifetimes. This module records facts from finished
//! work and builds borrowed views of them; it does not dispatch, publish or write to storage.

use crate::cache::DecideState;
use crate::dag::Dag;
use crate::dispatch::{OutputRef, StepFailure};
use crate::persist::RunLedger;
use crate::results::StepReport;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct RunResults {
    pub(super) outputs: HashMap<String, OutputRef>,
    pub(super) reports: Vec<StepReport>,
    pub(super) sinks: HashMap<String, String>,
    pub(super) timings: HashMap<String, (Option<f64>, Option<u64>)>,
    pub(super) observations: HashMap<String, serde_json::Value>,
    pub(super) clocks: HashMap<String, (f64, f64)>,
    pub(super) failures: Vec<StepFailure>,
    /// Attempts belong to the exact step identity, including its partition key.
    pub(super) attempts: HashMap<String, u32>,
    /// Worker-reported active duration and slow import duration, including failed attempts.
    pub(super) durations: HashMap<String, (f64, f64)>,
    /// Each executed key's run-local batched call identity (RFC-0013).
    pub(super) batches: HashMap<String, String>,
}

/// The terminal projection's owned scratch data. Constructed after recorder shutdown, so it
/// sees the same final results and measured costs as the terminal write.
pub(super) struct RunEvidence {
    components: HashMap<String, String>,
    cost_snapshot: Vec<(String, crate::cost::NodeEstimate)>,
}

impl RunEvidence {
    pub(super) fn new(
        results: &RunResults,
        dag: &Dag,
        decisions: &DecideState,
        cached: &HashSet<String>,
        costs: &crate::cost::CostModel,
    ) -> Self {
        let cost_snapshot = costs
            .persistence_snapshot()
            .map(|(node_id, estimate)| (node_id.clone(), *estimate))
            .collect();
        let components = results
            .outputs
            .keys()
            .filter(|id| !cached.contains(*id))
            .filter_map(|id| {
                crate::changes::step_components_json(dag, decisions, id)
                    .map(|components| (id.clone(), components))
            })
            .collect();
        Self {
            components,
            cost_snapshot,
        }
    }
}

/// Run identity and execution decisions supplied by the driver at the terminal boundary.
/// No result map is duplicated here: those belong to `RunResults`.
pub(super) struct LedgerContext<'a> {
    pub(super) run_id: &'a str,
    pub(super) status: &'a str,
    pub(super) command: &'a str,
    pub(super) files: &'a [String],
    pub(super) target: Option<&'a str>,
    pub(super) job: &'a str,
    pub(super) steps_total: usize,
    pub(super) steps_executed: usize,
    pub(super) steps_cached: usize,
    pub(super) elapsed: f64,
    pub(super) cached_node_ids: &'a HashSet<String>,
    pub(super) decisions: &'a DecideState,
    pub(super) store_paths: &'a HashMap<String, String>,
    pub(super) touched: &'a [String],
}

impl RunResults {
    pub(super) fn record_output(
        &mut self,
        node_id: &str,
        artifact_val: &serde_json::Value,
        attempts: u32,
    ) -> OutputRef {
        // Attempts belong to this exact lease, including its partition key.
        self.attempts.insert(node_id.to_string(), attempts);
        if let Some(observation) = artifact_val.get("source_observation") {
            self.observations
                .insert(node_id.to_string(), observation.clone());
        }
        let oref = OutputRef {
            path: artifact_val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            format: artifact_val
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("json")
                .to_string(),
            size_bytes: artifact_val
                .get("size_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            elapsed_seconds: artifact_val.get("elapsed_seconds").and_then(|v| v.as_f64()),
            content_hash: artifact_val
                .get("content_hash")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        };
        if let Some(sinks) = artifact_val.get("sinks").and_then(|v| v.as_array())
            && !sinks.is_empty()
        {
            self.sinks.insert(
                node_id.to_string(),
                serde_json::Value::from(sinks.clone()).to_string(),
            );
        }
        let cpu = artifact_val.get("cpu_seconds").and_then(|v| v.as_f64());
        let rss = artifact_val.get("max_rss_bytes").and_then(|v| v.as_u64());
        if cpu.is_some() || rss.is_some() {
            self.timings.insert(node_id.to_string(), (cpu, rss));
        }
        if let (Some(finished), Some(wall)) = (
            artifact_val.get("finished_at").and_then(|v| v.as_f64()),
            artifact_val.get("wall_seconds").and_then(|v| v.as_f64()),
        ) {
            self.clocks.insert(node_id.to_string(), (finished, wall));
        }
        oref
    }

    pub(super) fn record_failure(
        &mut self,
        node_id: String,
        error_type: &str,
        message: String,
        attempts: u32,
        clock: Option<(std::time::SystemTime, std::time::Duration)>,
        phase: Option<crate::dispatch::ErrorPhase>,
    ) {
        self.failures.push(StepFailure {
            node_id,
            error: crate::dispatch::StepError {
                error_type: error_type.to_string(),
                message,
                traceback: String::new(),
                phase,
                attempts,
                started_unix_ns: clock.map(|(started, _)| {
                    started
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos() as u64)
                }),
                elapsed_seconds: clock.map(|(_, wall)| wall.as_secs_f64()),
            },
        });
    }

    /// Run hashes identify plan steps. Other coordinator outputs are parallel children,
    /// whose facts may be observed but whose artifacts are never terminal materializations.
    pub(super) fn merge_plan_outputs(
        &mut self,
        phase_outputs: &HashMap<String, OutputRef>,
        run_hashes: &HashMap<String, String>,
    ) {
        for (node_id, output) in phase_outputs {
            if run_hashes.contains_key(node_id) {
                self.outputs.insert(node_id.clone(), output.clone());
            }
        }
    }

    /// Apply physical attempt and worker timing evidence to the final logical reports.
    pub(super) fn annotate_reports(&self, step_reports: &mut [StepReport]) {
        let mut attempts_by_base: HashMap<String, u32> = HashMap::new();
        for (node, attempts) in &self.attempts {
            let id = crate::StepId::parse(node);
            if id.partition.0.contains_key("_branch") {
                continue;
            }
            let base = id.base_id().to_string();
            let count = attempts_by_base.entry(base).or_default();
            *count = (*count).max(*attempts);
        }
        for report in step_reports {
            if let Some(attempts) = attempts_by_base.get(&report.id) {
                report.attempts = Some(*attempts);
            }
            if let Some((elapsed, imports)) = self.durations.get(&report.id) {
                report.elapsed_seconds = Some(*elapsed);
                report.import_seconds = (*imports > 1.0).then_some(*imports);
            }
        }
    }

    pub(super) fn ledger<'a>(
        &'a self,
        run: LedgerContext<'a>,
        evidence: &'a RunEvidence,
    ) -> RunLedger<'a> {
        RunLedger {
            run_id: run.run_id,
            status: run.status,
            command: run.command,
            files: crate::db::encode_files(run.files),
            target: run.target,
            job: run.job,
            steps_total: run.steps_total,
            steps_executed: run.steps_executed,
            steps_cached: run.steps_cached,
            elapsed: run.elapsed,
            all_outputs: &self.outputs,
            all_failures: &self.failures,
            all_sinks: &self.sinks,
            all_attempts: &self.attempts,
            all_timings: &self.timings,
            batches: &self.batches,
            cached_node_ids: run.cached_node_ids,
            run_hashes: run.decisions.run_hashes(),
            components: &evidence.components,
            output_hashes: &run.decisions.sensor_outputs,
            store_paths: run.store_paths,
            cost_snapshot: &evidence.cost_snapshot,
            touched: run.touched,
        }
    }
}
