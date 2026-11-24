//! Compact status predictions for fully validated aligned chains. Uncertain evidence takes
//! the ordinary predictor, including remote storage, held nodes and explicit key listings.
use crate::{
    cache::CacheReader,
    evidence::{self, Proof},
};
use barca_db::db;
use barca_model::{
    BarcaError,
    config::ResolvedConfig,
    dag::Dag,
    model::{Freshness, NodeKind},
};
use barca_store::results::{ExplainResult, ExplainSummary, PartitionSummary, StepReport};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

// Fixed expiry, never extended on a hit. Only a digest per validated node is retained.
// This avoids re-statting every selected file after an unchanged observation. The server's
// status lifetime adds at most 25 seconds to this five-second local check lifetime.
static AVAILABLE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
const AVAILABILITY_AGE: Duration = Duration::from_secs(5);

fn error(e: impl std::fmt::Display) -> BarcaError {
    BarcaError::Other(format!("inspection evidence: {e}"))
}

fn source_matches(proof: &Proof) -> bool {
    let (Some(source), Some(output), Some(hash)) =
        (&proof.source, &proof.output, &proof.output_hash)
    else {
        return false;
    };
    if !source.valid() || !source.matches(&output.path, Some(hash)) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(&output.path) else {
        return false;
    };
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => digest.update(&buffer[..n]),
            Err(_) => return false,
        }
    }
    format!("{:x}", digest.finalize()) == *hash
}

pub async fn explain(
    dag: &Dag,
    cfg: &ResolvedConfig,
    targets: &[String],
) -> Result<Option<ExplainResult>, BarcaError> {
    if cfg.remote_artifacts()
        || cfg.ledger.is_some()
        || !std::path::Path::new(&cfg.db_path).exists()
    {
        return Ok(None);
    }
    let resolved = barca_compile::targets::resolve_targets(dag, targets, "status")?;
    let ids: HashSet<String> = if targets.is_empty() {
        dag.topo_order().into_iter().map(str::to_owned).collect()
    } else {
        resolved
            .iter()
            .flat_map(|(_, id)| dag.subgraph(id))
            .map(str::to_owned)
            .collect()
    };
    let signatures = evidence::signatures(dag, &ids);
    let cache = CacheReader::for_config(cfg).await?;
    let revision = db::freshness_proofs::revision(cache.conn()).await.map_err(error)?;
    let namespace = evidence::key(cfg, revision);
    let mut proofs: HashMap<String, Proof> = HashMap::new();
    let mut generation = None;
    let mut steps = Vec::new();
    for id in dag.order_for(ids.iter().map(String::as_str)) {
        let node = dag.get_node(id).unwrap();
        let partitioned = !node.decl.partitions.is_empty();
        if !matches!(node.kind(), NodeKind::Asset | NodeKind::Sensor)
            || (node.kind() == NodeKind::Asset && matches!(node.decl.freshness, Freshness::Manual))
            || crate::cache::is_cutoff(dag, id)
            || (node.kind() == NodeKind::Asset && (!partitioned || node.decl.partitions.len() != 1))
            || (node.kind() == NodeKind::Sensor && (partitioned || !dag.upstream(id).is_empty()))
        {
            return Ok(None);
        }
        let Some((current, text)) =
            db::freshness_proofs::read(cache.conn(), &format!("{namespace}:{id}"))
                .await
                .map_err(error)?
        else {
            return Ok(None);
        };
        if generation.is_some_and(|old| old != current) {
            return Ok(None);
        }
        generation = Some(current);
        let Ok(proof) = serde_json::from_str::<Proof>(&text) else {
            return Ok(None);
        };
        if proof.signature != signatures[id] {
            return Ok(None);
        }
        if node.kind() == NodeKind::Sensor {
            if !source_matches(&proof) {
                return Ok(None);
            }
            steps.push(StepReport {
                id: id.into(),
                kind: "sensor".into(),
                action: Some("run".into()),
                reason: Some("sensor".into()),
                run_hash: proof.run_hash.clone(),
                ..Default::default()
            });
        } else {
            // Proof publication validates the chain, but an unsupported upstream must not
            // silently supply membership. All upstream proofs were checked in topo order.
            if proof.count == 0 || dag.upstream(id).iter().any(|up| !proofs.contains_key(*up)) {
                return Ok(None);
            }
            let mut sources: Vec<_> = proofs
                .iter()
                .filter_map(|(id, proof)| proof.source.as_ref().map(|source| (id, source)))
                .collect();
            sources.sort_by_key(|(id, _)| *id);
            let digest = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&(
                        std::fs::canonicalize(&cfg.db_path).map_err(error)?,
                        &cfg.local_artifact_dir,
                        cfg.remote_off,
                        id,
                        &text,
                        &sources
                    ))
                    .map_err(error)?
                )
            );
            let available = AVAILABLE.get_or_init(Default::default);
            let hit = available
                .lock()
                .unwrap()
                .get(&digest)
                .is_some_and(|at| at.elapsed() < AVAILABILITY_AGE);
            if !hit {
                let started = Instant::now();
                if !db::freshness_proofs::inspect_paths(
                    cache.conn(),
                    id,
                    current,
                    proof.count,
                    &proof.selected,
                    |path| cache.allows_artifact(path) && std::path::Path::new(path).is_file(),
                )
                .await
                .map_err(error)?
                {
                    return Ok(None);
                }
                barca_store::text::trace_since("inspection: selected artifacts", started);
                let mut kept = available.lock().unwrap();
                kept.retain(|_, at| at.elapsed() < AVAILABILITY_AGE);
                if kept.len() >= 128 {
                    kept.clear();
                }
                kept.insert(digest, started);
            }
            let mut sensors: Vec<_> = node
                .resolved_inputs
                .values()
                .map(String::as_str)
                .chain(
                    dag.partition_sources(id)
                        .into_iter()
                        .filter(|_| node.decl.inherited_partitions.is_empty()),
                )
                .filter(|up| {
                    dag.get_node(up)
                        .is_some_and(|n| n.kind() == NodeKind::Sensor)
                })
                .collect();
            sensors.sort();
            sensors.dedup();
            let note = sensors
                .iter()
                .map(|id| {
                    format!(
                        "assumes sensor '{}' returns the same value as its last run",
                        barca_compile::targets::short_name(id)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            steps.push(StepReport {
                id: id.into(),
                kind: "asset".into(),
                detail: (!note.is_empty()).then_some(note),
                action: Some(barca_model::step_status::StepStatus::Cached.to_string()),
                reason: Some("materialized".into()),
                partitions: Some(PartitionSummary {
                    total: proof.count,
                    cached: proof.count,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        proofs.insert(id.into(), proof);
    }
    // Refresh can invalidate eligibility without appending a materialization.
    if db::freshness_proofs::revision(cache.conn()).await.map_err(error)? != revision {
        return Ok(None);
    }
    // Recheck every proof, including cache-only selection changes at the same generation.
    for (id, proof) in &proofs {
        let Some((current, text)) =
            db::freshness_proofs::read(cache.conn(), &format!("{namespace}:{id}"))
                .await
                .map_err(error)?
        else {
            return Ok(None);
        };
        if Some(current) != generation
            || serde_json::from_str::<Proof>(&text).ok().as_ref() != Some(proof)
        {
            return Ok(None);
        }
    }
    let mut summary = ExplainSummary::default();
    for step in &steps {
        summary.add(step);
    }
    Ok(Some(ExplainResult {
        dry_run: true,
        command: "status".into(),
        target: match resolved.as_slice() {
            [(_, id)] => Some(barca_compile::targets::short_name(id).into()),
            _ => None,
        },
        targets: if resolved.len() > 1 {
            resolved
                .iter()
                .map(|(name, id)| {
                    let cone: HashSet<_> = dag.subgraph(id).into_iter().collect();
                    let mut summary = ExplainSummary::default();
                    for step in steps.iter().filter(|s| cone.contains(s.id.as_str())) {
                        summary.add(step);
                    }
                    (
                        name.clone(),
                        barca_store::results::TargetPrediction { summary },
                    )
                })
                .collect()
        } else {
            Vec::new()
        },
        steps,
        summary,
        warnings: Vec::new(),
        uploads: None,
        capacity: crate::sizing::sizing_for(cfg, None).capacity,
        partition_selection: None,
    }))
}
