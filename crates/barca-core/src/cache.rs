//! Cache checking — run_hash computation and partition-aligned lookups.

use std::collections::HashMap;

/// Compute the run_hash for a step given its context. `env` is the step's declared env values
/// ([`crate::envdeps::hash_input`]); `None` leaves the hash exactly as a node without `env=`.
///
/// `outputs` maps an upstream step (display id) to a content hash of its output. Only sensors
/// have an entry: a consumer depends on what the sensor returned, so the output hash is folded
/// in next to the sensor's run hash (which covers only the sensor's code). Upstreams without an
/// entry contribute their run hash alone, so a pipeline without sensors hashes exactly as it did
/// before sensor outputs were hashed.
pub fn compute_run_hash(
    def_hash: &str,
    partition_key: Option<&str>,
    upstream_ids: impl Iterator<Item = impl AsRef<str>>,
    cached_run_hashes: &HashMap<String, String>,
    outputs: &HashMap<String, String>,
    env: Option<&str>,
) -> String {
    let token = |key: &str, h: &String| match outputs.get(key) {
        Some(out) => format!("{h}+output:{out}"),
        None => h.clone(),
    };
    let mut upstream_hashes: Vec<String> = Vec::new();
    for uid in upstream_ids {
        let uid = uid.as_ref();
        if let Some(h) = cached_run_hashes.get(uid) {
            upstream_hashes.push(token(uid, h));
            continue;
        }
        // Try partition-aligned lookup (same partition as current step).
        if let Some(pk) = partition_key {
            let aligned = format!("{uid}[{pk}]");
            if let Some(h) = cached_run_hashes.get(&aligned) {
                upstream_hashes.push(token(&aligned, h));
                continue;
            }
        }
        // Fan-in: collect ALL partition hashes for this base ID (sorted for determinism).
        let prefix = format!("{uid}[");
        let mut partition_hashes: Vec<(&String, &String)> = cached_run_hashes
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .collect();
        if !partition_hashes.is_empty() {
            partition_hashes.sort_by_key(|(k, _)| (*k).clone());
            for (k, h) in partition_hashes {
                upstream_hashes.push(token(k, h));
            }
        }
    }
    let hash_refs: Vec<&str> = upstream_hashes.iter().map(|s| s.as_str()).collect();
    crate::hash::run_hash(def_hash, partition_key, &hash_refs, None, env)
}

use crate::store_sync::StoreSync;
use crate::targets::{refresh_name_matches, short_name};
use crate::transfer::ArtifactLayout;
use crate::{dag::Dag, db, dispatch, dispatch::OutputRef, planner::Phase};

// ─── Cache decisions ─────────────────────────────────────────────────────────
//
// One function decides what happens to a step; a real run and `--dry-run` both call it, so the
// dry run cannot drift from what a run would do.

/// Why a step runs instead of being served from cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunReason {
    Task,
    Sensor,
    NoCache,
    Refresh,
    /// Downstream of an asset named in `--refresh` (the cascade); `root` is that asset.
    RefreshCascade {
        root: String,
    },
    RefreshAll,
    NotMaterialized,
    /// It has a cached result, but the artifact is gone and something needs to read it
    /// (see [`crate::recover`]).
    ArtifactMissing,
}

impl RunReason {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            RunReason::Task => "task",
            RunReason::Sensor => "sensor",
            RunReason::NoCache => "no_cache",
            RunReason::Refresh => "refresh",
            RunReason::RefreshCascade { .. } => "refresh_cascade",
            RunReason::RefreshAll => "refresh_all",
            RunReason::NotMaterialized => "not_materialized",
            RunReason::ArtifactMissing => "artifact_missing",
        }
    }

    pub(crate) fn detail(&self) -> String {
        match self {
            RunReason::Task => "tasks always re-run".to_string(),
            RunReason::Sensor => "sensors always re-run".to_string(),
            RunReason::NoCache => "--no-cache".to_string(),
            RunReason::Refresh => "named in --refresh".to_string(),
            RunReason::RefreshCascade { root } => {
                format!("downstream of refreshed '{root}'")
            }
            RunReason::RefreshAll => "--refresh-all".to_string(),
            RunReason::NotMaterialized => {
                "no cached result for this code and these inputs".to_string()
            }
            RunReason::ArtifactMissing => {
                "its cached result is still valid, but the artifact file is missing and \
                 something needs to read it"
                    .to_string()
            }
        }
    }
}

pub(crate) enum Decision {
    Run(RunReason),
    Cached {
        oref: OutputRef,
        /// The refreshed asset this cached step depends on, if any.
        stale_root: Option<String>,
    },
    Partitioned {
        cached: Vec<(String, OutputRef)>,
        missing: Vec<crate::model::PartitionKey>,
    },
}

/// Per-run state the decisions accumulate: run hashes (also used to persist results), assets
/// refreshed in this run, and cached assets downstream of a refreshed one.
#[derive(Default)]
pub(crate) struct DecideState {
    pub(crate) run_hashes: HashMap<String, String>,
    pub(crate) refreshed_ids: std::collections::HashSet<String>,
    /// Refreshed asset -> the `--refresh` name it was refreshed for (itself, or the named
    /// upstream it cascaded from).
    pub(crate) cascade_roots: HashMap<String, String>,
    pub(crate) stale_cached: HashMap<String, String>,
    /// Sensor step (display id) -> content hash of its output, folded into each consumer's run
    /// hash (#183). A real run fills it as sensors finish (they run in an earlier phase than
    /// their consumers); a dry run seeds it from each sensor's last recorded output.
    pub(crate) sensor_outputs: HashMap<String, String>,
}

/// The sensors `step` reads directly (base ids).
pub(crate) fn sensor_inputs<'a>(dag: &Dag, step: &'a crate::planner::StreamStep) -> Vec<&'a str> {
    let mut out: Vec<&str> = step
        .inputs
        .values()
        .map(|up| up.split('[').next().unwrap_or(up))
        .filter(|up| {
            dag.get_node(up)
                .is_some_and(|n| n.kind() == crate::NodeKind::Sensor)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// True when `sensor` (a base id) has an output hash for this run (any partition of it).
pub(crate) fn has_sensor_output(state: &DecideState, sensor: &str) -> bool {
    let prefix = format!("{sensor}[");
    state
        .sensor_outputs
        .keys()
        .any(|k| k == sensor || k.starts_with(&prefix))
}

pub(crate) async fn lookup_in(
    cache: Option<&db::CacheReader>,
    node_id: &str,
    run_hash: &str,
) -> Option<OutputRef> {
    lookup_cached(cache?, node_id, run_hash).await
}

/// The steps of `phase` with the index of their stream, each after every step of the phase it
/// depends on.
///
/// A phase's streams are how its work is split across workers, which depends on the pool size:
/// a partitioned step is cut into one chunk of keys per worker, and the chunks of an upstream
/// and of its per-key consumer land in streams independently of each other. Decisions are not
/// allowed to depend on that split. [`decide_step`] hashes a step from the run hashes of its
/// upstreams that are already in the state, so every chunk of an upstream has to be decided
/// before any chunk of its consumers (#330); walking stream by stream does not guarantee that.
/// The order here is the DAG's topological order of the nodes; the chunks of one node keep
/// their stream order.
pub(crate) fn in_dependency_order<'p>(
    dag: &Dag,
    phase: &'p Phase,
) -> Vec<(usize, &'p crate::planner::StreamStep)> {
    let rank: HashMap<&str, usize> = dag
        .topo_order()
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    let mut steps: Vec<(usize, &crate::planner::StreamStep)> = phase
        .streams
        .iter()
        .enumerate()
        .flat_map(|(i, stream)| stream.steps.iter().map(move |step| (i, step)))
        .collect();
    // Stable: steps of the same node stay in stream order.
    steps.sort_by_key(|(_, step)| {
        rank.get(step.step_id.base_id())
            .copied()
            .unwrap_or(usize::MAX)
    });
    steps
}

/// Decide what happens to `step`. Steps must be visited in dependency order
/// ([`in_dependency_order`]): in-phase upstream run
/// hashes are already in `state` when a consumer is hashed, so check-time and persist-time hashes
/// are identical. `cache` is `None` when there is no metadata DB yet (nothing is cached).
pub(crate) async fn decide_step(
    dag: &Dag,
    policy: &CachePolicy,
    no_cache: bool,
    cache: Option<&db::CacheReader>,
    state: &mut DecideState,
    step: &crate::planner::StreamStep,
) -> (crate::planner::StreamStep, Decision) {
    let base_id = step.step_id.base_id();
    let display_id = step.step_id.display();
    let base_node = dag.get_node(base_id);
    let def_hash = base_node.map(|n| n.definition_hash.as_str()).unwrap_or("");
    // Declared env (`@asset(env=[...])`) is read now, at plan time, and folded into the run hash.
    let env_input = base_node
        .map(|n| crate::envdeps::hash_input(&crate::envdeps::resolve(&n.extracted.env)))
        .unwrap_or(None);

    // Run hashes for EVERY step (sensors, tasks, refreshed and partitioned steps too): they
    // content-address artifacts and key persistence.
    let mut step = step.clone();
    if step.partition_keys.is_empty() {
        let partition_key = if step.step_id.partition.is_empty() {
            None
        } else {
            Some(step.step_id.partition.suffix())
        };
        let run_h = compute_run_hash(
            def_hash,
            partition_key.as_deref(),
            step.inputs.values(),
            &state.run_hashes,
            &state.sensor_outputs,
            env_input.as_deref(),
        );
        state.run_hashes.insert(display_id.clone(), run_h.clone());
        step.run_hashes.insert(display_id.clone(), run_h);
    } else {
        for pk in &step.partition_keys {
            let pdisplay = pk.display_id(&step.step_id.base);
            let run_h = compute_run_hash(
                def_hash,
                Some(&pk.suffix()),
                step.inputs.values(),
                &state.run_hashes,
                &state.sensor_outputs,
                env_input.as_deref(),
            );
            state.run_hashes.insert(pdisplay.clone(), run_h.clone());
            step.run_hashes.insert(pdisplay, run_h);
        }
    }

    let kind = base_node.map(|n| n.kind());
    // Sensors and tasks always re-run — never cached.
    match kind {
        Some(crate::NodeKind::Task) => return (step, Decision::Run(RunReason::Task)),
        Some(crate::NodeKind::Sensor) => return (step, Decision::Run(RunReason::Sensor)),
        _ => {}
    }
    if no_cache {
        return (step, Decision::Run(RunReason::NoCache));
    }

    // Refresh policy (`barca run`): force-rerun assets named in the refresh set and, when
    // cascading, every asset downstream of one (plan order puts upstream steps first, so a
    // refreshed upstream is already in `cascade_roots` when its consumers are decided).
    let is_asset = kind == Some(crate::NodeKind::Asset);
    let refresh = match policy {
        CachePolicy::CacheAware => None,
        CachePolicy::RefreshAll => is_asset.then_some(RunReason::RefreshAll),
        CachePolicy::RefreshSelective { names, cascade } => {
            if !is_asset {
                None
            } else if names.iter().any(|name| refresh_name_matches(base_id, name)) {
                Some(RunReason::Refresh)
            } else if *cascade {
                step.inputs.values().find_map(|up| {
                    let up_base = up.split('[').next().unwrap_or(up);
                    state
                        .cascade_roots
                        .get(up_base)
                        .map(|root| RunReason::RefreshCascade { root: root.clone() })
                })
            } else {
                None
            }
        }
    };
    if let Some(reason) = refresh {
        let root = match &reason {
            RunReason::RefreshCascade { root } => root.clone(),
            _ => short_name(base_id).to_string(),
        };
        state.refreshed_ids.insert(base_id.to_string());
        state.cascade_roots.insert(base_id.to_string(), root);
        return (step, Decision::Run(reason));
    }

    // A consumer of a sensor is only cache-checked against that sensor's output. The planner
    // runs sensors in an earlier phase, and a dry run reports such a step as unknown before it
    // gets here, so this is a safety net: without the output, never serve from cache.
    if sensor_inputs(dag, &step)
        .iter()
        .any(|s| !has_sensor_output(state, s))
    {
        return (step, Decision::Run(RunReason::NotMaterialized));
    }

    // Partitioned steps are checked per key: each partition has its own run hash, so keys
    // with a successful materialization are served from cache and only the rest execute.
    if !step.partition_keys.is_empty() {
        let mut cached = Vec::new();
        let mut missing = Vec::new();
        for pk in &step.partition_keys {
            let pdisplay = pk.display_id(&step.step_id.base);
            let run_h = step
                .run_hashes
                .get(&pdisplay)
                .cloned()
                .expect("partitioned step has a precomputed run hash per key");
            match lookup_in(cache, &pdisplay, &run_h).await {
                Some(oref) => cached.push((pdisplay, oref)),
                None => missing.push(pk.clone()),
            }
        }
        return (step, Decision::Partitioned { cached, missing });
    }

    let run_h = step
        .run_hashes
        .get(&display_id)
        .cloned()
        .expect("unpartitioned step has a precomputed run hash");
    match lookup_in(cache, &display_id, &run_h).await {
        None => (step, Decision::Run(RunReason::NotMaterialized)),
        Some(oref) => {
            // Cached, but does it depend on something refreshed in this run?
            let stale_root = step.inputs.values().find_map(|up| {
                let up_base = up.split('[').next().unwrap_or(up);
                if state.refreshed_ids.contains(up_base) {
                    Some(short_name(up_base).to_string())
                } else {
                    state.stale_cached.get(up_base).cloned()
                }
            });
            if let Some(root) = &stale_root {
                state.stale_cached.insert(base_id.to_string(), root.clone());
            }
            (step, Decision::Cached { oref, stale_root })
        }
    }
}

/// The most recent successful materialization of `node_id` with this run hash, if any. The row
/// is the cache hit; whether its artifact can still be read is settled only when something
/// needs to read it (see [`crate::recover`]).
pub(crate) async fn lookup_cached(
    cache: &db::CacheReader,
    node_id: &str,
    run_hash: &str,
) -> Option<dispatch::OutputRef> {
    const COLUMNS: &str = "artifact_path, artifact_format, artifact_size_bytes";
    let query = |columns: String| {
        cache.conn().query(
            format!(
                "SELECT {columns} FROM materializations WHERE node_id = ?1 AND run_hash = ?2 \
                 AND status = 'success' ORDER BY id DESC LIMIT 1"
            ),
            [node_id.to_string(), run_hash.to_string()],
        )
    };
    // A database from before barca recorded output hashes has no such column.
    let mut rows = match query(format!("{COLUMNS}, output_hash")).await {
        Ok(rows) => rows,
        Err(_) => query(COLUMNS.to_string()).await.unwrap(),
    };
    rows.next().await.unwrap().and_then(|row| {
        Some(dispatch::OutputRef {
            path: row.get::<String>(0).ok()?,
            format: row.get::<String>(1).ok()?,
            size_bytes: row.get::<i64>(2).ok()? as u64,
            elapsed_seconds: None,
            content_hash: row.get::<String>(3).ok().filter(|h| !h.is_empty()),
        })
    })
}

/// How a cache row's artifact is reached on this machine.
pub(crate) enum CacheHit {
    /// Use the output as recorded.
    Local(dispatch::OutputRef),
    /// The row records a location in the artifact store; `local` points at
    /// its local mirror, which is fetched before anything reads it.
    Store {
        local: dispatch::OutputRef,
        store: String,
    },
}

/// Resolve a cache row against this run's artifact store (`layout` is Some
/// when the store is separate from the local artifact dir). This only says
/// where the artifact is read from; whether it is there is checked when
/// something needs to read it (see [`crate::recover`]).
pub(crate) fn resolve_cache_hit(
    oref: dispatch::OutputRef,
    layout: Option<&ArtifactLayout>,
) -> CacheHit {
    match layout.and_then(|l| l.local_for(&oref.path)) {
        Some(local) => {
            let store = oref.path.clone();
            CacheHit::Store {
                local: dispatch::OutputRef {
                    path: local.to_string_lossy().into_owned(),
                    ..oref
                },
                store,
            }
        }
        None => CacheHit::Local(oref),
    }
}

/// Apply this run's artifact store to a cache row: the output to read, with
/// a store-backed row registered for fetching.
pub(crate) fn accept_cache_hit(
    store: &mut Option<StoreSync>,
    node_id: &str,
    oref: dispatch::OutputRef,
) -> dispatch::OutputRef {
    match resolve_cache_hit(oref, store.as_ref().map(|s| &s.layout)) {
        CacheHit::Local(o) => o,
        CacheHit::Store { local, store: at } => {
            if let Some(s) = store.as_mut() {
                s.fetchable.insert(
                    local.path.clone(),
                    (node_id.to_string(), at, local.content_hash.clone()),
                );
            }
            local
        }
    }
}

/// Apply this run's artifact store to a cache decision: cached outputs point
/// at their local mirror.
pub(crate) fn localize_decision(
    decision: Decision,
    step: &crate::planner::StreamStep,
    store: &mut Option<StoreSync>,
) -> Decision {
    match decision {
        Decision::Cached { oref, stale_root } => Decision::Cached {
            oref: accept_cache_hit(store, &step.step_id.display(), oref),
            stale_root,
        },
        Decision::Partitioned { cached, missing } => Decision::Partitioned {
            cached: cached
                .into_iter()
                .map(|(pdisplay, oref)| {
                    let oref = accept_cache_hit(store, &pdisplay, oref);
                    (pdisplay, oref)
                })
                .collect(),
            missing,
        },
        run => run,
    }
}

/// How the engine treats cached asset materializations for this invocation.
#[derive(Debug, Clone)]
pub enum CachePolicy {
    /// Normal cache-aware behavior — reuse fresh asset artifacts.
    CacheAware,
    /// Force-rerun every asset in the target's cone (`barca get|run ... --refresh-all`).
    RefreshAll,
    /// Force-rerun the named assets (`barca get|run ... --refresh a,b`). A name matches when it
    /// equals the node's base id exactly, or matches the trailing `:name` segment. With
    /// `cascade` (the default) every asset downstream of a named one in the target's cone
    /// re-runs too; without it (`--no-cascade`) all other assets stay cache-aware.
    RefreshSelective { names: Vec<String>, cascade: bool },
}

/// How an error starts when a directory at an artifact path could not be moved aside
/// (`barca._storage.ArtifactPathError`): the state of barca's own artifact directory, not a
/// fault of the step that was writing there.
pub(crate) const BLOCKED_ARTIFACT_PATH: &str = "ArtifactPathError";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PartitionKey, StepId};

    #[test]
    fn step_id_parse_unpartitioned() {
        let sid = StepId::parse("test.py:foo");
        assert_eq!(sid.base_id(), "test.py:foo");
        assert!(sid.partition.is_empty());
        assert_eq!(sid.display(), "test.py:foo");
    }

    #[test]
    fn step_id_parse_partitioned() {
        let sid = StepId::parse("test.py:foo[region=us]");
        assert_eq!(sid.base_id(), "test.py:foo");
        assert_eq!(sid.partition.0.get("region").unwrap(), "us");
        assert_eq!(sid.display(), "test.py:foo[region=us]");
    }

    #[test]
    fn step_id_round_trip() {
        let pk = PartitionKey::from(HashMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]));
        let sid = StepId::new("f:x", pk);
        let display = sid.display();
        let parsed = StepId::parse(&display);
        assert_eq!(parsed.base_id(), "f:x");
        assert_eq!(parsed.partition, sid.partition);
    }

    #[test]
    fn compute_run_hash_deterministic() {
        let mut hashes = HashMap::new();
        hashes.insert("upstream".to_string(), "h_up".to_string());

        let h1 = compute_run_hash(
            "def_abc",
            None,
            ["upstream".to_string()].iter(),
            &hashes,
            &HashMap::new(),
            None,
        );
        let h2 = compute_run_hash(
            "def_abc",
            None,
            ["upstream".to_string()].iter(),
            &hashes,
            &HashMap::new(),
            None,
        );
        assert_eq!(h1, h2);
    }

    /// Pinned run hashes computed by barca <= 0.9.0 (before declared env existed). A node that
    /// declares no env must keep exactly these hashes, or every existing cache is invalidated.
    #[test]
    fn run_hash_unchanged_for_nodes_without_env() {
        let mut hashes = HashMap::new();
        hashes.insert("upstream".to_string(), "h_up".to_string());
        assert_eq!(
            compute_run_hash(
                "def_abc",
                None,
                ["upstream".to_string()].iter(),
                &hashes,
                &HashMap::new(),
                None
            ),
            "bc7c6531d8fe3452c9a9ac36fef43665103624231e112b5d87bf20376e2e9288"
        );
        assert_eq!(
            compute_run_hash(
                "def_abc",
                Some("t=X"),
                std::iter::empty::<&String>(),
                &HashMap::new(),
                &HashMap::new(),
                None
            ),
            "7a4d6ec23915f304b5520de445fd04343119f46d664c9737a7edb7ae11babc5d"
        );
    }

    /// Run hashes of whole pipelines. Pinned from barca 0.10.0 (before #178 taught the cone
    /// analysis about bare filenames and `import module` + `module.attr`) up to 0.18, and again
    /// from 0.19.0, which changed what the definition hash covers of the decorator (#283) and
    /// so, once, every run hash. Both sets are checked below: the first shows that nothing but
    /// the decorator's part moved, the second pins what users' caches are keyed by now.
    #[test]
    fn run_hash_unchanged_for_pipelines_without_module_attribute_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("p.py"),
            r#"import json
from barca import asset
from helpers import compute
from utils.maths import double

RATE = 2


def local_helper(x):
    return x * RATE


@asset()
def plain() -> dict:
    return {"v": 1}


@asset()
def uses_local() -> int:
    return local_helper(3)


@asset()
def uses_from() -> int:
    return compute()


@asset()
def uses_subdir() -> int:
    return double(2)


@asset()
def uses_stdlib_module() -> str:
    return json.dumps({"a": 1})


@asset(inputs={"x": uses_from})
def downstream(x: int) -> int:
    return x + 1
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("helpers.py"),
            "def compute():\n    return 1\n\n\ndef unrelated():\n    return 0\n",
        )
        .unwrap();
        std::fs::create_dir(root.join("utils")).unwrap();
        std::fs::write(
            root.join("utils/maths.py"),
            "def double(x):\n    return x * 2\n",
        )
        .unwrap();

        let file = root.join("p.py").to_string_lossy().to_string();
        let dag = crate::load::build_dag_blocking(
            std::slice::from_ref(&file),
            &std::path::PathBuf::from("python3"),
        )
        .unwrap();
        let source = std::fs::read_to_string(&file).unwrap();
        // The definition hash as 0.10 to 0.18 computed it: over the function's text starting
        // at its decorator, as written, with the freshness, the input parameter names, the
        // sinks and the serializer added as JSON.
        let definition_hash_before_0_19 = |node: &crate::model::DagNode| {
            let n = &node.extracted;
            let from_def = &n.source_text[n.source_text.find("def ").unwrap()..];
            let def_at = n.byte_offset + source[n.byte_offset..].find("def ").unwrap();
            let as_written = &source[n.byte_offset..def_at + from_def.len()];
            let metadata = serde_json::json!({
                "kind": n.kind,
                "freshness": n.freshness,
                "inputs": n.inputs.iter().map(|i| &i.param_name).collect::<Vec<_>>(),
                "sinks": n.sinks,
                "serializer": n.artifact_serializer,
            })
            .to_string();
            crate::hash::definition_hash(as_written, &n.cone_hash, &metadata)
        };
        let run_hashes = |definition_hash: &dyn Fn(&crate::model::DagNode) -> String| {
            let mut hashes: HashMap<String, String> = HashMap::new();
            let mut by_name: Vec<(String, String)> = Vec::new();
            for id in dag.topo_order() {
                let node = dag.get_node(id).unwrap();
                let h = compute_run_hash(
                    &definition_hash(node),
                    None,
                    dag.upstream(id).into_iter(),
                    &hashes,
                    &HashMap::new(),
                    None,
                );
                hashes.insert(id.to_string(), h.clone());
                by_name.push((node.extracted.function_name.clone(), h));
            }
            by_name.sort();
            by_name
                .iter()
                .map(|(n, h)| format!("{n} {h}"))
                .collect::<Vec<String>>()
        };

        // 1. The values pinned from 0.10.0. They are still what comes out when the decorator
        //    is hashed as text, so the function text from `def`, the dependency cone and the
        //    run hash itself have not changed.
        assert_eq!(
            run_hashes(&definition_hash_before_0_19),
            [
                "downstream 1c56f2327b95052d761eb9f938de94d7bf6ceb4963f94c5ae7269d8bb70c56e0",
                "plain 951e243083668324e552a72ac14fdaa7a44643e4e3ef612bae42ca99a012f4cd",
                "uses_from 3cfd70e1f1c429d785cbca598dd6c658c516419b1db74fedd6da1f98f5f91b3c",
                "uses_local 20c98bdcb5158223e28ad33f1eb2374383b20e07b6241c31bb6c0d93d90191d9",
                "uses_stdlib_module 38349c33e82697699115193940df0487e7ee4a242b747e20f7b2ddbc153052f4",
                "uses_subdir 6ef456461f6549fe2b0b99a8cacd7ac5eb069530470b5574aac096cfb6b013de",
            ]
        );

        // 2. The values from 0.19.0 on. Every one differs from its 0.10.0 value above for the
        //    same single reason (#283): the definition hash no longer covers the decorator as
        //    text (`@asset()`, `@asset(inputs={"x": uses_from})`) and the freshness, but the
        //    decorator arguments that count, in canonical form (`crate::definition`).
        //    `downstream` also differs because its upstream's run hash does. If one of these
        //    moves, every cache entry is recomputed after upgrading: change them only on
        //    purpose, with a release note.
        let got = run_hashes(&|node| node.definition_hash.clone());
        assert_eq!(
            got,
            [
                "downstream 2739276cb72e94eeda531d3f2ffb89928270d6a2e0c3e39d0005196da140ef5e",
                "plain 684e90afadfdefdce36277677510e35283f8593745acddca845bb7fb195fa333",
                "uses_from b2c5f127fa312c98225a8c8ded3932aa22b1843bab930181956d294a60626baf",
                "uses_local 04cb4b3a97b9ce588545f5e148c23504df582d91af40362d885b003f5275b31c",
                "uses_stdlib_module a7487267e78586c6a8f3ffb45f56873a9e320e5d62307dbaff50933145f80012",
                "uses_subdir 3da349ac7a54d3c29b091ac8ee2b1231de4f3678cf9123157da0e8448718ad8f",
            ]
        );
    }

    #[test]
    fn declared_env_changes_the_run_hash() {
        use crate::envdeps::{hash_input, resolve_with};
        let names = vec!["SOURCE_CSV".to_string()];
        let hashes = HashMap::new();
        let h = |v: Option<&str>| {
            let vals = resolve_with(&names, |_| v.map(String::from));
            compute_run_hash(
                "def_abc",
                None,
                std::iter::empty::<&String>(),
                &hashes,
                &HashMap::new(),
                hash_input(&vals).as_deref(),
            )
        };
        let none = compute_run_hash(
            "def_abc",
            None,
            std::iter::empty::<&String>(),
            &hashes,
            &HashMap::new(),
            None,
        );
        assert_ne!(h(Some("/a.csv")), h(Some("/b.csv")));
        assert_ne!(h(None), h(Some("")));
        assert_ne!(
            h(None),
            none,
            "declaring a variable (even unset) is part of the identity"
        );
        assert_eq!(h(Some("/a.csv")), h(Some("/a.csv")));
    }

    #[test]
    fn compute_run_hash_changes_with_partition() {
        let hashes = HashMap::new();
        let h1 = compute_run_hash(
            "def_abc",
            None,
            std::iter::empty::<&String>(),
            &hashes,
            &HashMap::new(),
            None,
        );
        let h2 = compute_run_hash(
            "def_abc",
            Some("t=X"),
            std::iter::empty::<&String>(),
            &hashes,
            &HashMap::new(),
            None,
        );
        assert_ne!(h1, h2);
    }

    /// A sensor's output hash is folded into its consumer's run hash; an upstream without an
    /// output entry (every non-sensor) contributes exactly what it did before (#183).
    #[test]
    fn sensor_output_is_folded_into_the_consumer_run_hash() {
        let mut hashes = HashMap::new();
        hashes.insert("upstream".to_string(), "h_up".to_string());
        let with = |out: Option<&str>| {
            let outputs: HashMap<String, String> = out
                .map(|o| HashMap::from([("upstream".to_string(), o.to_string())]))
                .unwrap_or_default();
            compute_run_hash(
                "def_abc",
                None,
                ["upstream".to_string()].iter(),
                &hashes,
                &outputs,
                None,
            )
        };
        // No output entry: the pinned pre-#183 hash.
        assert_eq!(
            with(None),
            "bc7c6531d8fe3452c9a9ac36fef43665103624231e112b5d87bf20376e2e9288"
        );
        assert_ne!(with(Some("etag_v1")), with(None));
        assert_ne!(with(Some("etag_v1")), with(Some("etag_v2")));
        assert_eq!(with(Some("etag_v1")), with(Some("etag_v1")));
    }

    #[test]
    fn sensor_output_reaches_partitioned_consumers_and_fan_ins() {
        let mut hashes = HashMap::new();
        hashes.insert("s".to_string(), "h_s".to_string());
        hashes.insert("p[k=a]".to_string(), "h_pa".to_string());
        let out = |key: &str, v: &str| HashMap::from([(key.to_string(), v.to_string())]);
        // A partitioned consumer of an unpartitioned sensor.
        let keyed = |o: &HashMap<String, String>| {
            compute_run_hash("d", Some("k=a"), ["s"].iter(), &hashes, o, None)
        };
        assert_ne!(keyed(&out("s", "1")), keyed(&out("s", "2")));
        // Partition-aligned and fan-in lookups use the partition's own entry.
        let aligned = |o: &HashMap<String, String>| {
            compute_run_hash("d", Some("k=a"), ["p"].iter(), &hashes, o, None)
        };
        assert_ne!(aligned(&out("p[k=a]", "1")), aligned(&out("p[k=a]", "2")));
        let fan_in = |o: &HashMap<String, String>| {
            compute_run_hash("d", None, ["p"].iter(), &hashes, o, None)
        };
        assert_ne!(fan_in(&out("p[k=a]", "1")), fan_in(&HashMap::new()));
    }
}

#[cfg(test)]
mod dependency_order_tests {
    use super::*;
    use crate::planner::ResourceConfig;

    const CHAINED: &str = r#"
from barca import asset, collect, partitions_from


@asset()
def keys() -> list:
    return ["a", "b", "c"]


@asset(partitions={"region": partitions_from(keys)})
def sales(region: str) -> dict:
    return {"region": region}


@asset(partitions={"region": partitions_from(sales)})
def margin(region: str, sales: dict) -> dict:
    return sales


@asset(inputs={"m": collect(margin)})
def report(m: list) -> int:
    return len(m)
"#;

    /// The phase of `sales` and `margin`, expanded over `keys` for a pool of `pool_size`.
    fn expanded(dag: &Dag, keys: &[&str], pool_size: usize) -> Phase {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("keys.json");
        std::fs::write(&artifact, serde_json::to_string(keys).unwrap()).unwrap();
        let outputs = HashMap::from([(
            "t.py:keys".to_string(),
            OutputRef {
                path: artifact.to_string_lossy().to_string(),
                format: "json".to_string(),
                size_bytes: 0,
                elapsed_seconds: None,
                content_hash: None,
            },
        )]);
        let config = ResourceConfig {
            pool_size,
            concurrency_groups: HashMap::new(),
        };
        let plan = crate::planner::plan_from_dag(dag, &config);
        let phase = plan
            .phases
            .iter()
            .find(|p| {
                let mut steps = p.streams.iter().flat_map(|s| &s.steps);
                steps.any(|st| st.step_id.base_id() == "t.py:margin")
            })
            .expect("a phase with margin");
        dispatch::expand_pending_partitions(phase, &outputs, pool_size).expect("expanded")
    }

    /// Run hashes of every key of `sales` and `margin`, hashed in the order `order` gives.
    fn run_hashes<'p>(
        dag: &Dag,
        order: impl Iterator<Item = &'p crate::planner::StreamStep>,
    ) -> Vec<(String, String)> {
        let mut state: HashMap<String, String> =
            HashMap::from([("t.py:keys".to_string(), "h_keys".to_string())]);
        for step in order {
            let def_hash = &dag
                .get_node(step.step_id.base_id())
                .unwrap()
                .definition_hash;
            for pk in &step.partition_keys {
                let hash = compute_run_hash(
                    def_hash,
                    Some(&pk.suffix()),
                    step.inputs.values(),
                    &state,
                    &HashMap::new(),
                    None,
                );
                state.insert(pk.display_id(&step.step_id.base), hash);
            }
        }
        let mut hashes: Vec<(String, String)> = state.into_iter().collect();
        hashes.sort();
        hashes
    }

    /// #330: the chunks of `sales` and of `margin` are placed in streams independently, so
    /// stream by stream a key of `margin` can come before the same key of `sales`. In
    /// dependency order it never does, and the run hashes are the same at every pool size.
    #[test]
    fn every_key_of_an_upstream_is_decided_before_any_key_of_its_consumer() {
        let nodes = crate::parse::extract_nodes(CHAINED, "t.py").unwrap();
        let dag = Dag::build(&nodes).unwrap();
        let keys = ["a", "b", "c", "d", "e"];

        let reference = {
            let phase = expanded(&dag, &keys, 64);
            run_hashes(
                &dag,
                in_dependency_order(&dag, &phase)
                    .into_iter()
                    .map(|(_, s)| s),
            )
        };
        assert_eq!(reference.len(), 1 + 2 * keys.len());

        let mut stream_order_differs_somewhere = false;
        for pool_size in 1..=8 {
            let phase = expanded(&dag, &keys, pool_size);
            let ordered = in_dependency_order(&dag, &phase);

            // Every step of the phase, once, with the stream it came from.
            assert_eq!(
                ordered.len(),
                phase.streams.iter().map(|s| s.steps.len()).sum::<usize>()
            );
            for (stream, step) in &ordered {
                assert!(
                    phase.streams[*stream]
                        .steps
                        .iter()
                        .any(|st| std::ptr::eq(st, *step))
                );
            }
            let bases: Vec<&str> = ordered.iter().map(|(_, s)| s.step_id.base_id()).collect();
            let last_sales = bases.iter().rposition(|b| *b == "t.py:sales").unwrap();
            let first_margin = bases.iter().position(|b| *b == "t.py:margin").unwrap();
            assert!(last_sales < first_margin, "pool of {pool_size}: {bases:?}");

            assert_eq!(
                run_hashes(&dag, ordered.into_iter().map(|(_, s)| s)),
                reference,
                "pool of {pool_size}"
            );

            // What the run loop used to do: stream by stream.
            let by_stream = phase.streams.iter().flat_map(|s| &s.steps);
            if run_hashes(&dag, by_stream) != reference {
                stream_order_differs_somewhere = true;
            }
        }
        assert!(
            stream_order_differs_somewhere,
            "stream order gave the reference hashes at every pool size: this test no longer \
             covers #330"
        );
    }
}
