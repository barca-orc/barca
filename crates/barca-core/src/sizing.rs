//! How many workers a run starts and the capacity their steps share (#436, #628): decided
//! from the configuration and the host, the same for a run and for the commands that predict
//! one (`barca plan`, `barca status`, `barca doctor`).

use crate::planner::ExecutionPlan;
use crate::targets::short_name;

/// The cores this process may use (`available_parallelism`), 4 when unknown.
pub(crate) fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// How many workers a run starts at most and the capacity their steps' `weight=` shares, each
/// with where it came from (#436, #628).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    pub pool_size: usize,
    /// `BARCA_POOL_SIZE`, `[execution] pool_size`, `cores`, or the capacity that bounded the
    /// cores (`[execution] capacity`, `BARCA_CAPACITY`, `[server] capacity`, `--capacity`).
    pub pool_source: &'static str,
    pub capacity: usize,
    /// `BARCA_CAPACITY`, `[execution] capacity`, or `pool size`.
    pub capacity_source: &'static str,
}

/// The pool size and capacity of a run.
///
/// Pool: `BARCA_POOL_SIZE` > `[execution] pool_size` > the cores, bounded by the capacity when
/// one is set (`server`, the capacity a `barca serve` shares between its runs, bounds it too).
/// Every step takes a weight of at least 1, so no more than `capacity` steps ever run at once:
/// a worker past that would start, import the project and wait. Weights above 1 lower how
/// many run at once further, but workers start only for steps the capacity admits, so the
/// pool is not sized from them.
///
/// Capacity: `BARCA_CAPACITY` > `[execution] capacity` > the pool size.
pub fn sizing(
    capacity: crate::config::CapacitySetting,
    pool: crate::config::PoolSetting,
    server: Option<(usize, &'static str)>,
) -> Sizing {
    sizing_with(capacity, pool, server, cores())
}

fn sizing_with(
    capacity: crate::config::CapacitySetting,
    pool: crate::config::PoolSetting,
    server: Option<(usize, &'static str)>,
    cores: usize,
) -> Sizing {
    let (pool_size, pool_source) = match pool {
        Some((n, source)) => (n.max(1), source),
        None => {
            let bound = [capacity, server]
                .into_iter()
                .flatten()
                .min_by_key(|(n, _)| *n);
            match bound {
                Some((n, source)) if n < cores => (n.max(1), source),
                _ => (cores.max(1), "cores"),
            }
        }
    };
    let (capacity, capacity_source) = capacity.unwrap_or((pool_size, "pool size"));
    Sizing {
        pool_size,
        pool_source,
        capacity: capacity.max(1),
        capacity_source,
    }
}

/// [`sizing`] for a run with this configuration, under the capacity its host shares between
/// its runs: `shared` is that capacity and where it was set, `None` for a run on its own (a
/// prediction included). The caller reads it from its host, so this names no host.
pub(crate) fn sizing_for(
    cfg: &crate::config::ResolvedConfig,
    shared: Option<(usize, &'static str)>,
) -> Sizing {
    sizing(cfg.capacity, cfg.pool_size, shared)
}

/// The planned nodes whose `weight=` is above 1, by node id.
pub(crate) fn planned_weights(plan: &ExecutionPlan) -> std::collections::BTreeMap<String, u32> {
    plan.phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
        .filter(|st| st.weight > 1)
        .map(|st| (st.step_id.base_id().to_string(), st.weight))
        .collect()
}

/// Warn, once per node, about each weight above the capacity: such a step runs alone, with
/// nothing beside it, rather than never.
pub(crate) fn warn_over_capacity(
    weights: &std::collections::BTreeMap<String, u32>,
    capacity: usize,
    source: &str,
) {
    for (node, weight) in weights {
        if *weight as usize > capacity {
            crate::run_line!(
                Always,
                "[barca] warning: '{}' has weight={weight}, more than the capacity {capacity} \
                 ({source}); it runs alone",
                short_name(node)
            );
        }
    }
}
