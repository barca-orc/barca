//! A partitioned node's every key at once: its state, its latest attempt and its errors, in a
//! compact, columnar form a client can draw and filter without asking again
//! (`GET /assets/{name}/partitions/grid`), and one key in detail
//! (`GET /assets/{name}/partitions/key`).
//!
//! The state of a key is the dry run's, as `barca status` decides it, for every key rather
//! than the report's preview of a few, and it is decided for the node's whole chain: the
//! partitioned nodes it shares its keys with, upstream and downstream, so that a key waiting on
//! the same key of an input that failed reads as blocked.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::BarcaError;
use crate::dag::Dag;
use crate::db::{LatestAttempt, PartitionErrorGroup};
use crate::model::{PartitionKey, PartitionSpec, PartitionValue};

/// A key's state, one character of [`PartitionGrid::states`].
pub mod state {
    /// Served from cache: a successful result matches its code and inputs.
    pub const FRESH: char = 'c';
    /// An operator holds this key at a chosen version until unpinned.
    pub const PINNED: char = 'p';
    /// It succeeded before, and its code or inputs changed since: it runs again.
    pub const STALE: char = 's';
    /// No attempt recorded: it runs.
    pub const NEVER_RUN: char = 'n';
    /// Its latest attempt failed: it runs again.
    pub const FAILED: char = 'f';
    /// It waits on the same key of an input whose latest attempt failed (or that waits too).
    pub const BLOCKED: char = 'b';
    /// Not a key: a combination of the dimensions' values the node does not have.
    pub const NONE: char = '-';
}

/// One dimension of a node's keys: its name and its values, in order.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct GridDimension {
    pub name: String,
    pub values: Vec<String>,
}

/// How many keys are in each state.
#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct GridCounts {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub fresh: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub pinned: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub stale: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub never_run: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failed: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub blocked: usize,
}

/// Every key of a partitioned node.
///
/// Keys are the cells of the dimensions' product in row-major order, the last dimension
/// varying fastest: with `day` (2 values) and `region` (3), cell 4 is the second day's second
/// region. The per-key arrays have one entry per cell.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionGrid {
    pub id: String,
    /// The partitioned nodes that share this node's keys (the same dimensions, linked through
    /// their inputs), upstream first; this node among them.
    pub chain: Vec<String>,
    /// False while the keys are not known: a `partitions_from` source that has not run.
    pub known: bool,
    /// In key-suffix order (by name).
    pub dimensions: Vec<GridDimension>,
    /// One character per cell: `c` fresh, `s` stale, `n` never run, `f` failed, `b` blocked,
    /// `-` not a key.
    pub states: String,
    pub counts: GridCounts,
    /// The latest attempt of each cell, null where there is none: when, in seconds after
    /// `attempted_base` (unix seconds); how long it took (milliseconds); its artifact's size;
    /// and its run, an index into `runs`.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub attempted_base: i64,
    #[cfg_attr(feature = "ts", ts(type = "Array<number | null>"))]
    pub attempted_at: Vec<Option<i64>>,
    #[cfg_attr(feature = "ts", ts(type = "Array<number | null>"))]
    pub elapsed_ms: Vec<Option<u64>>,
    #[cfg_attr(feature = "ts", ts(type = "Array<number | null>"))]
    pub size_bytes: Vec<Option<i64>>,
    pub run: Vec<Option<u32>>,
    /// Each failed cell's error, an index into `errors`, by cell index.
    #[cfg_attr(feature = "ts", ts(type = "Record<string, number>"))]
    pub error: BTreeMap<u32, u32>,
    /// The failed keys' errors, most common first: the first line, each of the key's own
    /// values replaced by its dimension's name (`<region>`).
    pub errors: Vec<PartitionErrorGroup>,
    pub runs: Vec<String>,
}

/// The partitioned nodes sharing `id`'s keys: connected to it through input edges between
/// nodes with the same dimension names, in topological order.
pub fn chain(dag: &Dag, id: &str) -> Vec<String> {
    let dims = |id: &str| -> Option<BTreeSet<String>> {
        let node = dag.get_node(id)?;
        let names: BTreeSet<String> = node.extracted.partitions.keys().cloned().collect();
        (!names.is_empty()).then_some(names)
    };
    let Some(own) = dims(id) else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::from([id.to_string()]);
    let mut todo = vec![id.to_string()];
    while let Some(next) = todo.pop() {
        for other in dag.upstream(&next).into_iter().chain(dag.downstream(&next)) {
            if !seen.contains(other) && dims(other).as_ref() == Some(&own) {
                seen.insert(other.to_string());
                todo.push(other.to_string());
            }
        }
    }
    dag.topo_order()
        .into_iter()
        .filter(|n| seen.contains(*n))
        .map(str::to_string)
        .collect()
}

/// The key states of every node of `id`'s chain, decided by one dry run of the chain.
pub(crate) struct ChainStates {
    pub chain: Vec<String>,
    /// Per node: its keys (suffixes) and each key's state.
    pub states: HashMap<String, BTreeMap<String, char>>,
    pub attempts: HashMap<String, Vec<LatestAttempt>>,
    /// Nodes whose keys are not known yet.
    pub unknown: HashSet<String>,
}

pub(crate) async fn chain_states(
    cfg: &crate::config::ResolvedConfig,
    dag: &Dag,
    id: &str,
) -> Result<ChainStates, BarcaError> {
    let chain = chain(dag, id);
    let (explained, members, will_run) = crate::execution::explain_dag_with_keys(
        dag,
        cfg,
        &chain,
        crate::cache::CachePolicy::CacheAware,
        "status",
        None,
        &crate::freshness::RunRules::default(),
        &[],
    )
    .await?;
    let unknown: HashSet<String> = explained
        .steps
        .iter()
        .filter(|s| s.action.as_deref() == Some("unknown"))
        .map(|s| s.id.clone())
        .collect();
    let mut attempts = HashMap::new();
    if std::path::Path::new(&cfg.db_path).exists() {
        for node in &chain {
            attempts.insert(
                node.clone(),
                crate::db::latest_attempts(&cfg.db_path, node).await?,
            );
        }
    }
    let pins: HashSet<String> = crate::db::get_pins(&cfg.db_path)
        .await?
        .into_iter()
        .map(|p| p.node_id)
        .collect();
    let empty = BTreeSet::new();
    let mut states: HashMap<String, BTreeMap<String, char>> = HashMap::new();
    for node in &chain {
        let latest: HashMap<String, &LatestAttempt> = attempts
            .get(node)
            .map(|rows| rows.iter().map(|r| (r.key.suffix(), r)).collect())
            .unwrap_or_default();
        let runs = will_run.get(node).unwrap_or(&empty);
        let inputs: Vec<&str> = dag
            .upstream(node)
            .into_iter()
            .filter(|u| chain.iter().any(|c| c == u))
            .collect();
        let mut own = BTreeMap::new();
        for display in members.get(node).unwrap_or(&empty) {
            let step = crate::StepId::parse(display);
            let key = step.partition.suffix();
            let state = if pins.contains(display) {
                state::PINNED
            } else if !runs.contains(display) {
                state::FRESH
            } else {
                match latest.get(&key).map(|r| r.status.as_str()) {
                    Some("failed") => state::FAILED,
                    last => {
                        let waits = inputs.iter().any(|u| {
                            states
                                .get(*u)
                                .and_then(|s| s.get(&key))
                                .is_some_and(|s| matches!(*s, state::FAILED | state::BLOCKED))
                        });
                        if waits {
                            state::BLOCKED
                        } else if last.is_some() {
                            state::STALE
                        } else {
                            state::NEVER_RUN
                        }
                    }
                }
            };
            own.insert(key, state);
        }
        states.insert(node.clone(), own);
    }
    Ok(ChainStates {
        chain,
        states,
        attempts,
        unknown,
    })
}

/// `id`'s dimensions: each one's values in declared order where they are declared, and in
/// key order where they come from an upstream's output.
fn dimensions(dag: &Dag, id: &str, keys: &[PartitionKey]) -> Vec<GridDimension> {
    let specs = dag
        .get_node(id)
        .map(|n| n.extracted.partitions.clone())
        .unwrap_or_default();
    let mut names: Vec<&String> = specs.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let present: BTreeSet<&str> = keys
                .iter()
                .filter_map(|k| k.0.get(name).map(String::as_str))
                .collect();
            let mut values: Vec<String> = match specs.get(name) {
                Some(PartitionSpec::Static { values }) => values
                    .iter()
                    .map(|v| match v {
                        PartitionValue::Str(s) => s.clone(),
                        PartitionValue::Int(i) => i.to_string(),
                    })
                    .filter(|v| present.contains(v.as_str()))
                    .collect(),
                _ => Vec::new(),
            };
            let mut seen: HashSet<String> = values.iter().cloned().collect();
            values.dedup();
            for v in present {
                if seen.insert(v.to_string()) {
                    values.push(v.to_string());
                }
            }
            GridDimension {
                name: name.clone(),
                values,
            }
        })
        .collect()
}

/// `GET /assets/{name}/partitions/grid`: every key of `id`.
pub async fn partition_grid(
    cfg: &crate::config::ResolvedConfig,
    dag: &Dag,
    id: &str,
) -> Result<PartitionGrid, BarcaError> {
    let chain = chain_states(cfg, dag, id).await?;
    Ok(grid_of(dag, id, &chain))
}

pub(crate) fn grid_of(dag: &Dag, id: &str, chain: &ChainStates) -> PartitionGrid {
    let empty = BTreeMap::new();
    let own = chain.states.get(id).unwrap_or(&empty);
    let attempts: HashMap<String, &LatestAttempt> = chain
        .attempts
        .get(id)
        .map(|rows| rows.iter().map(|r| (r.key.suffix(), r)).collect())
        .unwrap_or_default();
    let parsed: Vec<PartitionKey> = own
        .keys()
        .map(|suffix| crate::StepId::parse(&format!("x[{suffix}]")).partition)
        .collect();
    let dims = dimensions(dag, id, &parsed);
    let cells: usize = if dims.is_empty() {
        0
    } else {
        dims.iter().map(|d| d.values.len()).product()
    };
    let mut grid = PartitionGrid {
        id: id.to_string(),
        chain: chain.chain.clone(),
        known: !chain.unknown.contains(id),
        dimensions: dims.clone(),
        states: String::with_capacity(cells),
        counts: GridCounts::default(),
        attempted_base: 0,
        attempted_at: Vec::with_capacity(cells),
        elapsed_ms: Vec::with_capacity(cells),
        size_bytes: Vec::with_capacity(cells),
        run: Vec::with_capacity(cells),
        error: BTreeMap::new(),
        errors: Vec::new(),
        runs: Vec::new(),
    };
    let mut error_index: HashMap<String, u32> = HashMap::new();
    let mut error_keys: Vec<usize> = Vec::new();
    let mut run_index: HashMap<String, u32> = HashMap::new();
    let mut coords = vec![0usize; dims.len()];
    for cell in 0..cells {
        let key = PartitionKey(
            dims.iter()
                .zip(&coords)
                .map(|(d, &i)| (d.name.clone(), d.values[i].clone()))
                .collect(),
        );
        let suffix = key.suffix();
        let state = own.get(&suffix).copied().unwrap_or(state::NONE);
        match state {
            state::FRESH => grid.counts.fresh += 1,
            state::PINNED => grid.counts.pinned += 1,
            state::STALE => grid.counts.stale += 1,
            state::NEVER_RUN => grid.counts.never_run += 1,
            state::FAILED => grid.counts.failed += 1,
            state::BLOCKED => grid.counts.blocked += 1,
            _ => {}
        }
        grid.states.push(state);
        let attempt = attempts.get(&suffix);
        grid.attempted_at
            .push(attempt.and_then(|a| crate::time::parse_db_utc(&a.created_at)));
        grid.elapsed_ms.push(
            attempt
                .and_then(|a| a.elapsed_seconds)
                .map(|s| (s * 1000.0).round().max(0.0) as u64),
        );
        grid.size_bytes.push(attempt.and_then(|a| a.size_bytes));
        if let Some(a) = attempt.filter(|a| state == state::FAILED && a.status == "failed") {
            let shape = crate::db::error_shape(a.error.as_deref(), &a.key);
            let next = error_index.len() as u32;
            let i = *error_index.entry(shape).or_insert(next);
            if i as usize == error_keys.len() {
                error_keys.push(0);
            }
            error_keys[i as usize] += 1;
            grid.error.insert(cell as u32, i);
        }
        grid.run
            .push(attempt.and_then(|a| a.run_id.as_ref()).map(|r| {
                let next = run_index.len() as u32;
                *run_index.entry(r.clone()).or_insert(next)
            }));
        // The next cell: the last dimension varies fastest.
        for d in (0..coords.len()).rev() {
            coords[d] += 1;
            if coords[d] < dims[d].values.len() {
                break;
            }
            coords[d] = 0;
        }
    }
    // Errors most common first: renumber the cells' indices to match.
    let mut shapes: Vec<(String, u32)> = error_index.into_iter().collect();
    shapes.sort_by(|a, b| {
        error_keys[b.1 as usize]
            .cmp(&error_keys[a.1 as usize])
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut renumber = vec![0u32; shapes.len()];
    for (new, (_, old)) in shapes.iter().enumerate() {
        renumber[*old as usize] = new as u32;
    }
    for e in grid.error.values_mut() {
        *e = renumber[*e as usize];
    }
    // Attempt times as offsets from the earliest: short numbers.
    grid.attempted_base = grid
        .attempted_at
        .iter()
        .flatten()
        .copied()
        .min()
        .unwrap_or(0);
    let base = grid.attempted_base;
    for at in grid.attempted_at.iter_mut().flatten() {
        *at -= base;
    }
    grid.errors = shapes
        .into_iter()
        .map(|(error, old)| PartitionErrorGroup {
            error,
            keys: error_keys[old as usize],
        })
        .collect();
    let mut runs: Vec<(String, u32)> = run_index.into_iter().collect();
    runs.sort_by_key(|r| r.1);
    grid.runs = runs.into_iter().map(|r| r.0).collect();
    grid
}

/// One key's state in one node of its chain.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ChainKeyState {
    pub id: String,
    /// A [`state`] character; `-` where the node has no such key or its keys are not known.
    pub state: String,
}

/// What one key read or wrote: a node's latest successful result for it, and that result's
/// shape (`barca status --sample`'s), or why there is none.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeyArtifact {
    pub id: String,
    /// The step: `node[key]` for a partitioned node, the node for one that is not.
    pub step: String,
    pub artifact_path: Option<String>,
    pub format: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<i64>,
    /// The artifact's shape: its type, columns or fields, and first rows; absent when there is
    /// no successful result to read.
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown> | null"))]
    pub shape: Option<serde_json::Value>,
}

/// `GET /assets/{name}/partitions/key`: one key of a partitioned node; `None` when the node
/// has no such key.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PartitionKeyDetail {
    pub id: String,
    /// `day=2026-01-01,region=apac`.
    pub key: String,
    /// A [`state`] character.
    pub state: String,
    /// The same key in each node of the chain, upstream first.
    pub chain: Vec<ChainKeyState>,
    /// The key's latest attempts, newest first (at most 20).
    pub attempts: Vec<crate::db::KeyAttempt>,
    /// Its latest successful result.
    pub output: Option<KeyArtifact>,
    /// What it reads: the same key of each partitioned input with its dimensions, and the
    /// latest result of each other input.
    pub inputs: Vec<KeyArtifact>,
}

/// The attempts listed for one key.
const KEY_ATTEMPTS: usize = 20;

pub async fn partition_key_detail(
    cfg: &crate::config::ResolvedConfig,
    dag: &Dag,
    python: &std::path::Path,
    id: &str,
    key: &str,
    sample: usize,
) -> Result<Option<PartitionKeyDetail>, BarcaError> {
    let parsed = crate::StepId::parse(&format!("{id}[{key}]")).partition;
    let suffix = parsed.suffix();
    let chain = chain_states(cfg, dag, id).await?;
    let state_in = |node: &str| -> char {
        chain
            .states
            .get(node)
            .and_then(|s| s.get(&suffix))
            .copied()
            .unwrap_or(state::NONE)
    };
    let own = state_in(id);
    if own == state::NONE {
        return Ok(None);
    }
    let step = format!("{id}[{suffix}]");
    let db_exists = std::path::Path::new(&cfg.db_path).exists();
    let attempts = if db_exists {
        crate::db::key_attempts(&cfg.db_path, &step, KEY_ATTEMPTS).await?
    } else {
        Vec::new()
    };
    // The output, then each input: the same key where the input shares the dimensions.
    let mut wanted: Vec<(String, String)> = vec![(id.to_string(), step.clone())];
    if let Some(node) = dag.get_node(id) {
        let mut inputs: Vec<&String> = node
            .resolved_inputs
            .values()
            .chain(node.resolved_collected.values())
            .collect();
        inputs.sort();
        inputs.dedup();
        for input in inputs {
            let same = chain.chain.iter().any(|c| c == input);
            wanted.push((
                input.clone(),
                if same {
                    format!("{input}[{suffix}]")
                } else {
                    input.clone()
                },
            ));
        }
    }
    let reader = if db_exists {
        crate::cache::CacheReader::for_config(cfg).await.ok()
    } else {
        None
    };
    let pins = match &reader {
        Some(reader) => crate::db::pins_on(reader.conn()).await?,
        None => BTreeMap::new(),
    };
    let mut found: Vec<KeyArtifact> = Vec::new();
    for (node, step) in &wanted {
        let output = match &reader {
            Some(reader) => match pins.get(step) {
                Some(pin) => crate::db::success_at(reader, step, &pin.run_hash).await,
                None => crate::db::latest_success(reader, step)
                    .await
                    .map(|l| l.output),
            },
            None => None,
        }
        .filter(|o| cfg.allows_artifact(&o.path));
        found.push(KeyArtifact {
            id: node.clone(),
            step: step.clone(),
            artifact_path: output.as_ref().map(|o| o.path.clone()),
            format: output.as_ref().map(|o| o.format.clone()),
            size_bytes: output.as_ref().map(|o| o.size_bytes as i64),
            shape: None,
        });
    }
    drop(reader);
    let readable: Vec<(usize, (String, String))> = found
        .iter()
        .enumerate()
        .filter_map(|(i, a)| Some((i, (a.artifact_path.clone()?, a.format.clone()?))))
        .collect();
    let shapes = crate::store_inspect::artifact_shapes(
        python,
        cfg,
        &readable.iter().map(|(_, a)| a.clone()).collect::<Vec<_>>(),
        sample,
        true,
    )
    .await;
    for ((i, _), shape) in readable.iter().zip(shapes) {
        found[*i].shape = Some(shape);
    }
    let mut found = found.into_iter();
    let output = found.next().filter(|o| o.artifact_path.is_some());
    Ok(Some(PartitionKeyDetail {
        id: id.to_string(),
        key: suffix.clone(),
        state: own.to_string(),
        chain: chain
            .chain
            .iter()
            .map(|node| ChainKeyState {
                id: node.clone(),
                state: if chain.unknown.contains(node) {
                    state::NONE
                } else {
                    state_in(node)
                }
                .to_string(),
            })
            .collect(),
        attempts,
        output,
        inputs: found.collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pairs: &[(&str, &str)]) -> PartitionKey {
        PartitionKey(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn error_shapes_name_the_dimension_each_value_came_from() {
        let k = key(&[("day", "2026-03-01"), ("region", "apac")]);
        assert_eq!(
            crate::db::error_shape(
                Some("s3://raw/apac/2026-03-01.csv has 0 rows\ntraceback"),
                &k
            ),
            "s3://raw/<region>/<day>.csv has 0 rows"
        );
    }

    #[test]
    fn error_shapes_leave_a_value_inside_a_longer_word() {
        let k = key(&[("k", "a")]);
        assert_eq!(
            crate::db::error_shape(Some("ValueError: no rows for a (a-b)"), &k),
            "ValueError: no rows for <k> (<k>-b)"
        );
    }
}
