//! Measured-cost estimation and adaptive batch sizing.
//!
//! Barca never introspects task cost at plan time (static analysis only), so
//! the dispatcher *measures* it instead: workers self-time every task, the
//! coordinator folds observations into an EWMA per node, and the estimate
//! drives how many tasks a worker leases per pull (`K`).
//!
//! Every batch-sizing decision trades between exactly two goals:
//! 1. **Keep all workers fed** — enough batches that nobody idles → small `K`.
//! 2. **Amortize per-pull coordination cost** — comm negligible vs. work → large `K`.
//!
//! The one irreducible heuristic is the cold-start default (a never-run node's
//! cost genuinely isn't known yet). It is deliberately high (30s): over-estimating
//! cost yields `K = 1` and mild extra comm overhead (cheap error), while
//! under-estimating over-batches secretly-heavy tasks onto one worker and
//! tail-blocks the run (catastrophic error). The first completed tasks decay
//! the estimate, so the cold price is paid once per node — and persisting the
//! EWMA across runs (see `db::cost_estimates_for`) means once *ever*.

use std::collections::HashMap;

/// Cold-start estimate for a never-run node, in seconds. Calibrated to
/// ~30s-partition data processing; deliberately safe (see module docs).
pub const COLD_START_SECONDS: f64 = 30.0;

/// Fixed per-pull coordination cost, in seconds. Covers the UDS round-trip +
/// JSON framing for one batch assignment. The queue carries artifact
/// references (not payloads), so this is genuinely fixed — which is what makes
/// batching able to amortize it. Overridable via `BARCA_COMM_COST_SECONDS`.
pub const DEFAULT_COMM_COST_SECONDS: f64 = 0.005;

/// Target ratio of comm cost to work per batch: 1%, not 0.1%. Going from
/// 50%→5% comm is a 10× win; 5%→0.1% recovers ~5% more throughput but demands
/// 10× bigger batches, which costs 10× worse load balance and tail latency.
pub const TARGET_COMM_RATIO: f64 = 0.01;

/// Batches each worker should see over a run — enough for balance and
/// natural work-stealing (a fast worker just pulls again sooner).
pub const CHUNKS_PER_WORKER: usize = 3;

/// A workload whose *total* estimated cost is below this is trivial: comm
/// amortization wins outright and parallelism is moot.
const TRIVIAL_TOTAL_SECONDS: f64 = 1.0;

/// EWMA weight for observations that raise the estimate. "It got slower" is
/// urgent — shrink K now to stop over-batching.
const ALPHA_RISE: f64 = 0.5;

/// EWMA weight for observations that lower the estimate. "It got faster" is
/// good news — confirm it over a few observations before trusting it.
const ALPHA_FALL: f64 = 0.25;

/// An estimate may drop at most this factor per observation (fall slow).
const FLOOR_FACTOR: f64 = 0.7;

/// One node's running estimate.
#[derive(Debug, Clone, Copy)]
pub struct NodeEstimate {
    /// Current EWMA of per-task wall cost, seconds.
    pub estimate_seconds: f64,
    /// Last observed CPU seconds (informational; persisted for `barca stats`).
    pub cpu_seconds: f64,
    /// Last observed peak RSS in bytes (informational).
    pub max_rss_bytes: u64,
    /// Observations folded into this estimate (including seeded history).
    pub samples: u64,
}

/// Measured-cost model: per-node estimates with a three-tier prior, updated
/// on every task completion, driving the batch-pull size `K`.
///
/// Wall time (not CPU time) drives `K`: batch sizing exists to keep workers
/// *occupied*, and a worker blocked on I/O is exactly as occupied as one
/// burning CPU. CPU time and RSS ride along as history for later policies.
pub struct CostModel {
    /// Exact node id (including partition suffix) → estimate.
    per_node: HashMap<String, NodeEstimate>,
    /// Base node id → estimate over all its partitions. Lets an unseen
    /// partition key inherit the typical cost of its siblings.
    per_base: HashMap<String, NodeEstimate>,
    comm_cost_seconds: f64,
    /// Nodes observed this run: the only estimates that can differ from what was seeded.
    observed: std::collections::HashSet<String>,
    observed_bases: std::collections::HashSet<String>,
}

impl CostModel {
    pub fn new() -> Self {
        let comm_cost_seconds = std::env::var("BARCA_COMM_COST_SECONDS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(DEFAULT_COMM_COST_SECONDS);
        Self {
            per_node: HashMap::new(),
            per_base: HashMap::new(),
            comm_cost_seconds,
            observed: Default::default(),
            observed_bases: Default::default(),
        }
    }

    /// Seed the model from persisted estimates (run start). Base-tier
    /// estimates are rebuilt as the median of the seeded node estimates so a
    /// new partition inherits sibling history immediately.
    pub fn seed(&mut self, persisted: impl IntoIterator<Item = (String, NodeEstimate)>) {
        let mut by_base: HashMap<String, Vec<f64>> = HashMap::new();
        let mut explicit_bases = std::collections::HashSet::new();
        for (node_id, est) in persisted {
            let base = crate::StepId::parse(&node_id).base_id().to_string();
            if node_id == base {
                explicit_bases.insert(base.clone());
                self.per_base.insert(base.clone(), est);
            }
            by_base.entry(base).or_default().push(est.estimate_seconds);
            self.per_node.insert(node_id, est);
        }
        for (base, mut vals) in by_base {
            if explicit_bases.contains(&base) {
                continue;
            }
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = vals[vals.len() / 2];
            self.per_base.insert(
                base,
                NodeEstimate {
                    estimate_seconds: median,
                    cpu_seconds: 0.0,
                    max_rss_bytes: 0,
                    samples: vals.len() as u64,
                },
            );
        }
    }

    /// Exact history is loaded only for keys that are about to execute.
    pub(crate) fn seed_exact(
        &mut self,
        persisted: impl IntoIterator<Item = (String, NodeEstimate)>,
    ) {
        self.per_node.extend(persisted);
    }

    /// Three-tier prior: exact-node history → node-level (sibling-partition)
    /// history → cold-start default.
    pub fn estimate(&self, node_id: &str) -> f64 {
        if let Some(e) = self.per_node.get(node_id) {
            return e.estimate_seconds;
        }
        let base = crate::StepId::parse(node_id).base_id().to_string();
        if let Some(e) = self.per_base.get(&base) {
            return e.estimate_seconds;
        }
        COLD_START_SECONDS
    }

    /// Fold one completed task's measurements into the running estimates.
    /// Called on every completion, so `K` for the *next* pull already reflects
    /// this task — the first wave of a cold run is the probe.
    pub fn observe(&mut self, node_id: &str, wall_seconds: f64, cpu_seconds: f64, rss_bytes: u64) {
        let wall = wall_seconds.max(0.0);
        let base = crate::StepId::parse(node_id).base_id().to_string();
        self.observed.insert(node_id.to_string());
        self.observed_bases.insert(base.clone());
        Self::fold(
            self.per_node.entry(node_id.to_string()),
            wall,
            cpu_seconds,
            rss_bytes,
        );
        Self::fold(self.per_base.entry(base), wall, cpu_seconds, rss_bytes);
    }

    fn fold(
        entry: std::collections::hash_map::Entry<'_, String, NodeEstimate>,
        wall: f64,
        cpu: f64,
        rss: u64,
    ) {
        entry
            .and_modify(|e| {
                let prev = e.estimate_seconds;
                let new = if wall > prev {
                    // Rise fast: over-batching is the catastrophic error.
                    ALPHA_RISE * wall + (1.0 - ALPHA_RISE) * prev
                } else {
                    // Fall slow: confirm speedups before shrinking, and never
                    // drop more than (1 - FLOOR_FACTOR) per observation.
                    (ALPHA_FALL * wall + (1.0 - ALPHA_FALL) * prev).max(prev * FLOOR_FACTOR)
                };
                e.estimate_seconds = new;
                e.cpu_seconds = cpu;
                e.max_rss_bytes = e.max_rss_bytes.max(rss);
                e.samples += 1;
            })
            .or_insert(NodeEstimate {
                estimate_seconds: wall,
                cpu_seconds: cpu,
                max_rss_bytes: rss,
                samples: 1,
            });
    }

    /// How many tasks a worker should lease in one pull.
    ///
    /// ```text
    /// floor   = ceil(comm_cost / (per_task_cost × target_ratio))   # goal 2: amortize comm
    /// ceiling = max(1, remaining / (workers × chunks_per_worker))  # goal 1: keep workers fed
    /// K       = clamp(floor, ceiling)
    /// ```
    ///
    /// `remaining_tasks` is the *pull-eligible* pool (ready items) — work
    /// blocked on upstreams can't be pulled this wave and must not let one
    /// worker drain the whole ready queue.
    ///
    /// When the goals conflict (`floor > ceiling`), total workload breaks the
    /// tie: a trivial workload collapses toward sequential (comm-amortized);
    /// a large one favors the ceiling (balance beats shaving comm).
    ///
    /// A heavy node (≥ ~0.5s/task at the default comm cost) computes
    /// `floor = 1`, so heavy work is pulled one-at-a-time, fully parallel —
    /// the batching machinery stays dormant and only wakes for light nodes.
    pub fn batch_size(&self, node_id: &str, remaining_tasks: usize, workers: usize) -> usize {
        self.batch_size_with_backlog(node_id, remaining_tasks, 0, workers)
    }

    /// [`Self::batch_size`], with `backlog` more tasks of the phase waiting behind the ready
    /// ones (on upstreams, or later in their stream). They count toward whether the workload
    /// is trivial, never toward the ceiling: 16 ready heads of streams 31 steps long are not a
    /// trivial 16 steps to pack into two pulls, which left most of the pool idle while each
    /// pull ran its steps in turn (#560).
    pub fn batch_size_with_backlog(
        &self,
        node_id: &str,
        remaining_tasks: usize,
        backlog: usize,
        workers: usize,
    ) -> usize {
        if remaining_tasks == 0 {
            return 0;
        }
        let per_task = self.estimate(node_id).max(1e-6);
        let workers = workers.max(1);

        let floor = (self.comm_cost_seconds / (per_task * TARGET_COMM_RATIO)).ceil() as usize;
        let floor = floor.max(1);
        let ceiling = (remaining_tasks / (workers * CHUNKS_PER_WORKER)).max(1);

        let k = if floor <= ceiling {
            floor
        } else {
            let total_cost = per_task * (remaining_tasks + backlog) as f64;
            if total_cost < TRIVIAL_TOTAL_SECONDS {
                // Trivial workload: parallelism is moot — collapse to ~2 pulls.
                remaining_tasks.div_ceil(2)
            } else {
                ceiling
            }
        };
        k.min(remaining_tasks)
    }

    /// Keep exact histories on disk, with one aggregate prior per changed logical node.
    pub(crate) fn persistence_snapshot(&self) -> impl Iterator<Item = (&String, &NodeEstimate)> {
        self.snapshot()
            .filter(|(id, _)| !self.observed_bases.contains(*id))
            .chain(self.base_snapshot())
    }

    /// Persist one scheduling prior per logical node, independent of partition count.
    pub(crate) fn base_snapshot(&self) -> impl Iterator<Item = (&String, &NodeEstimate)> {
        self.per_base
            .iter()
            .filter(|(base, _)| self.observed_bases.contains(*base))
    }

    /// Snapshot the exact-node estimates this run changed, for run-end persistence. A seeded
    /// estimate no step updated is already in the database as it is: writing it again was one
    /// upsert per node on every run, cached ones included (#560).
    pub fn snapshot(&self) -> impl Iterator<Item = (&String, &NodeEstimate)> {
        self.per_node
            .iter()
            .filter(|(node_id, _)| self.observed.contains(*node_id))
    }
}

impl Default for CostModel {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> CostModel {
        CostModel {
            per_node: HashMap::new(),
            per_base: HashMap::new(),
            comm_cost_seconds: DEFAULT_COMM_COST_SECONDS,
            observed: Default::default(),
            observed_bases: Default::default(),
        }
    }

    #[test]
    fn cold_node_uses_default() {
        let m = model();
        assert_eq!(m.estimate("f.py:never_run"), COLD_START_SECONDS);
    }

    #[test]
    fn cold_default_forces_k_of_one() {
        // The probe wave: heavy default → K = 1, fully parallel, no over-batch risk.
        let m = model();
        assert_eq!(m.batch_size("f.py:never_run", 500, 8), 1);
    }

    #[test]
    fn observation_creates_exact_estimate() {
        let mut m = model();
        m.observe("f.py:fetch[t=A]", 0.2, 0.15, 1024);
        assert!((m.estimate("f.py:fetch[t=A]") - 0.2).abs() < 1e-9);
    }

    #[test]
    fn unseen_partition_inherits_sibling_cost() {
        // The node-tier prior: a new ticker inherits the typical cost of its
        // siblings instead of paying the 30s cold default.
        let mut m = model();
        m.observe("f.py:fetch[t=A]", 0.2, 0.1, 0);
        m.observe("f.py:fetch[t=B]", 0.3, 0.1, 0);
        let est = m.estimate("f.py:fetch[t=C]");
        assert!(est < 1.0, "sibling prior should apply, got {est}");
    }

    #[test]
    fn estimate_rises_fast() {
        let mut m = model();
        m.observe("f.py:a", 1.0, 1.0, 0);
        m.observe("f.py:a", 10.0, 10.0, 0);
        // ALPHA_RISE = 0.5 → 0.5*10 + 0.5*1 = 5.5
        assert!((m.estimate("f.py:a") - 5.5).abs() < 1e-9);
    }

    #[test]
    fn estimate_falls_slow() {
        let mut m = model();
        m.observe("f.py:a", 10.0, 10.0, 0);
        m.observe("f.py:a", 0.01, 0.01, 0);
        // EWMA would give 0.25*0.01 + 0.75*10 = 7.5, floor allows ≥ 7.0.
        let est = m.estimate("f.py:a");
        assert!(est >= 10.0 * FLOOR_FACTOR - 1e-9, "dropped too fast: {est}");
        // Repeated fast observations keep decaying it.
        for _ in 0..30 {
            m.observe("f.py:a", 0.01, 0.01, 0);
        }
        assert!(m.estimate("f.py:a") < 0.1);
    }

    #[test]
    fn heavy_tasks_pull_one_at_a_time() {
        let mut m = model();
        m.observe("f.py:heavy", 30.0, 29.0, 0);
        // comm is ~0.002% of one task's work — batching stays dormant.
        assert_eq!(m.batch_size("f.py:heavy", 100, 8), 1);
    }

    #[test]
    fn tiny_tasks_batch_up_to_balance_ceiling() {
        let mut m = model();
        // 1ms tasks: floor = ceil(0.005 / (0.001 * 0.01)) = 500.
        for _ in 0..40 {
            m.observe("f.py:tiny", 0.001, 0.001, 0);
        }
        // Large workload (5000 × 1ms = 5s total, not trivial): favor the
        // ceiling = 5000 / (8 × 3) = 208.
        let k = m.batch_size("f.py:tiny", 5000, 8);
        assert_eq!(k, 5000 / (8 * CHUNKS_PER_WORKER));
    }

    #[test]
    fn trivial_workload_collapses_to_sequential() {
        let mut m = model();
        for _ in 0..40 {
            m.observe("f.py:tiny", 0.001, 0.001, 0);
        }
        // 100 × 1ms = 0.1s total → trivial → ~2 pulls drain it.
        let k = m.batch_size("f.py:tiny", 100, 8);
        assert_eq!(k, 50);
    }

    #[test]
    fn moderate_tasks_amortize_comm_at_floor() {
        let mut m = model();
        // 100ms tasks: floor = ceil(0.005 / (0.1 * 0.01)) = 5.
        for _ in 0..40 {
            m.observe("f.py:mid", 0.1, 0.1, 0);
        }
        let k = m.batch_size("f.py:mid", 1000, 8);
        assert_eq!(k, 5);
    }

    #[test]
    fn k_never_exceeds_remaining() {
        let mut m = model();
        for _ in 0..40 {
            m.observe("f.py:tiny", 0.001, 0.001, 0);
        }
        assert_eq!(m.batch_size("f.py:tiny", 3, 8), 2); // trivial → div_ceil(3,2)
        assert_eq!(m.batch_size("f.py:tiny", 0, 8), 0);
    }

    #[test]
    fn seed_rebuilds_base_tier_from_median() {
        let mut m = model();
        let est = |s: f64| NodeEstimate {
            estimate_seconds: s,
            cpu_seconds: 0.0,
            max_rss_bytes: 0,
            samples: 5,
        };
        m.seed(vec![
            ("f.py:fetch[t=A]".to_string(), est(0.1)),
            ("f.py:fetch[t=B]".to_string(), est(0.2)),
            ("f.py:fetch[t=C]".to_string(), est(9.0)),
        ]);
        // Seen partition: exact tier.
        assert!((m.estimate("f.py:fetch[t=A]") - 0.1).abs() < 1e-9);
        // Unseen partition: median of siblings (0.2), robust to the outlier.
        assert!((m.estimate("f.py:fetch[t=D]") - 0.2).abs() < 1e-9);
    }

    #[test]
    fn probe_then_size_loop_converges_within_run() {
        // Simulates the first run of a light node: cold default → K=1 probe →
        // adaptively batched from pull two.
        let mut m = model();
        assert_eq!(m.batch_size("f.py:light", 1000, 8), 1);
        m.observe("f.py:light", 0.002, 0.002, 0);
        let k_after_probe = m.batch_size("f.py:light", 992, 8);
        assert!(
            k_after_probe > 1,
            "estimate should decay after probe, got K={k_after_probe}"
        );
    }

    /// #560: ready heads of long streams are not a trivial workload.
    #[test]
    fn work_waiting_behind_the_ready_steps_is_not_trivial() {
        let mut m = model();
        m.observe("f.py:a", 0.05, 0.05, 0);
        // 16 ready steps of 50 ms: 0.8 s in all, trivial, packed into two pulls.
        assert_eq!(m.batch_size("f.py:a", 16, 16), 8);
        // The same 16 at the heads of streams holding 484 more: one at a time.
        assert_eq!(m.batch_size_with_backlog("f.py:a", 16, 484, 16), 1);
    }

    #[test]
    fn rss_high_water_mark_is_kept() {
        let mut m = model();
        m.observe("f.py:a", 1.0, 1.0, 500);
        m.observe("f.py:a", 1.0, 1.0, 100);
        let (_, e) = m.snapshot().next().unwrap();
        assert_eq!(e.max_rss_bytes, 500);
        assert_eq!(e.samples, 2);
    }

    /// #560: a seeded estimate that no step of the run updated is not written back.
    #[test]
    fn the_snapshot_holds_only_what_this_run_observed() {
        let mut m = model();
        let seeded = NodeEstimate {
            estimate_seconds: 1.0,
            cpu_seconds: 1.0,
            max_rss_bytes: 0,
            samples: 3,
        };
        m.seed([
            ("f.py:a".to_string(), seeded),
            ("f.py:b".to_string(), seeded),
        ]);
        assert_eq!(m.snapshot().count(), 0);
        m.observe("f.py:b", 2.0, 2.0, 0);
        let ids: Vec<&String> = m.snapshot().map(|(id, _)| id).collect();
        assert_eq!(ids, ["f.py:b"]);
    }
    #[test]
    fn base_priors_are_bounded_by_logical_nodes_and_keep_their_history() {
        let mut m = model();
        for i in 0..1000 {
            m.observe(&format!("p.py:a[k={i}]"), 0.1, 0.05, 100);
        }
        let persisted: Vec<_> = m.base_snapshot().map(|(k, v)| (k.clone(), *v)).collect();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].0, "p.py:a");
        assert_eq!(persisted[0].1.samples, 1000);
        let mut warm = model();
        warm.seed(persisted);
        assert!((warm.estimate("p.py:a[k=new]") - 0.1).abs() < 1e-9);
        warm.observe("p.py:a[k=new]", 0.2, 0.1, 100);
        assert_eq!(warm.base_snapshot().next().unwrap().1.samples, 1001);
    }
}
