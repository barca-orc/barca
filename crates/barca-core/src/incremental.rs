//! Persisted evidence of freshness. A proof is valid only at its metadata generation.
//! The executor still decides and executes affected work through the normal cache path.
use crate::{
    BarcaError,
    cache::CachePolicy,
    dag::Dag,
    db,
    dispatch::OutputRef,
    model::{Freshness, NodeKind, PartitionKey, PartitionSpec},
    planner::{ExecutionPlan, Phase, StreamStep},
    results::{KeySummary, LISTED_KEYS, PartitionSummary, StepReport},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) const CREATE: &str = "CREATE TABLE IF NOT EXISTS freshness_proofs (id TEXT PRIMARY KEY, generation INTEGER NOT NULL, snapshot TEXT NOT NULL)";

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Source {
    keys: Vec<String>,
    hashes: Option<Arc<Hashes>>,
}
/// Intern repeated version hashes; metadata size tracks keys, not repeated 64-byte digests.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Hashes {
    values: Vec<String>,
    keys: HashMap<String, usize>,
}
impl Hashes {
    fn new(hashes: HashMap<String, String>) -> Self {
        let mut values = Vec::new();
        let mut interned = HashMap::new();
        let keys = hashes
            .into_iter()
            .map(|(key, hash)| {
                let index = *interned.entry(hash.clone()).or_insert_with(|| {
                    values.push(hash);
                    values.len() - 1
                });
                (key, index)
            })
            .collect();
        Self { values, keys }
    }
    fn get(&self, key: &str) -> Option<&String> {
        self.values.get(*self.keys.get(key)?)
    }
    fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.keys
            .iter()
            .filter_map(|(key, index)| self.values.get(*index).map(|hash| (key, hash)))
    }
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Proof {
    signature: String,
    count: usize,
    output_hash: Option<String>,
    output: Option<OutputRef>,
    run_hash: Option<String>,
    source: Option<Arc<Source>>,
}
#[derive(Default, PartialEq, Serialize, Deserialize)]
struct Snapshot {
    nodes: HashMap<String, Proof>,
}
#[derive(Clone, Debug)]
enum Change {
    All,
    Fresh,
    Keys(String, Arc<HashSet<String>>),
}

pub(crate) struct Incremental {
    key: String,
    generation: i64,
    previous: Snapshot,
    next: Snapshot,
    signatures: HashMap<String, String>,
    changes: HashMap<String, Change>,
    templates: HashMap<String, StreamStep>,
    /// Fresh logical steps represented by counts, rather than per-key objects.
    pub(crate) cached: HashMap<String, usize>,
    pub(crate) filters: HashMap<String, (String, Arc<HashSet<String>>)>,
    seeded: HashSet<String>,
}

impl Incremental {
    pub(crate) async fn open(
        dag: &Dag,
        plan: &ExecutionPlan,
        cfg: &crate::config::ResolvedConfig,
        policy: &CachePolicy,
        selected: bool,
    ) -> Result<Option<Self>, BarcaError> {
        if selected || !matches!(policy, CachePolicy::CacheAware) || cfg.ledger.is_some() {
            return Ok(None);
        }
        let mut templates = HashMap::new();
        for step in plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
        {
            let id = step.step_id.base_id();
            templates
                .entry(id.to_owned())
                .or_insert_with(|| step.with_keys(Vec::new()));
        }
        let mut ids: Vec<_> = templates.keys().cloned().collect();
        ids.sort();
        let key = format!(
            "freshness-v1:{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&cfg.artifact_root, &cfg.local_artifact_dir)).unwrap()
            )
        );
        let cache = db::CacheReader::for_config(cfg).await?;
        let generation = generation(cache.conn()).await?;
        let mut previous = Snapshot::default();
        let mut lookup = db::prepare(
            cache.conn(),
            "SELECT snapshot FROM freshness_proofs WHERE id = ?1 AND generation = ?2",
        )
        .await
        .map_err(db_error)?;
        for id in ids {
            let mut rows = lookup
                .query((format!("{key}:{id}"), generation))
                .await
                .map_err(db_error)?;
            if let Some(row) = rows.next().await.map_err(db_error)?
                && let Ok(text) = row.get::<String>(0)
                && let Ok(proof) = serde_json::from_str(&text)
            {
                previous.nodes.insert(id, proof);
            }
        }
        let mut signatures = HashMap::new();
        for id in dag.order_for(templates.keys().map(String::as_str)) {
            if !templates.contains_key(id) {
                continue;
            }
            let n = dag.get_node(id).unwrap();
            let mut upstream: Vec<_> = dag
                .upstream(id)
                .into_iter()
                .map(|up| {
                    (
                        up,
                        if crate::cache::is_cutoff(dag, up)
                            || dag.get_node(up).is_some_and(|n| {
                                matches!(n.extracted.freshness, Freshness::Manual)
                                    && n.kind() == NodeKind::Asset
                            })
                        {
                            None
                        } else {
                            signatures.get(up)
                        },
                    )
                })
                .collect();
            upstream.sort_by_key(|(id, _)| *id);
            let env = crate::envdeps::hash_input(&crate::envdeps::resolve(&n.extracted.env));
            let bytes = serde_json::to_vec(
                &serde_json::to_value(&(
                    n.definition_hash.as_str(),
                    &n.resolved_inputs,
                    &n.resolved_collected,
                    &n.extracted.freshness,
                    &n.extracted.sinks,
                    &n.extracted.partitions,
                    env,
                    upstream,
                ))
                .unwrap(),
            )
            .unwrap();
            signatures.insert(id.to_owned(), format!("{:x}", Sha256::digest(bytes)));
        }
        Ok(Some(Self {
            key,
            generation,
            previous,
            next: Snapshot::default(),
            signatures,
            changes: HashMap::new(),
            templates,
            cached: HashMap::new(),
            filters: HashMap::new(),
            seeded: HashSet::new(),
        }))
    }

    fn source_for<'a>(&'a self, dag: &Dag, base: &str) -> Option<(&'a str, &'a Source)> {
        let mut current = base.to_owned();
        loop {
            let proof = self.next.nodes.get(&current)?;
            if let Some(source) = &proof.source {
                return Some((self.next.nodes.get_key_value(&current)?.0, source));
            }
            let sources = dag.partition_sources(&current);
            let [source] = sources.as_slice() else {
                return None;
            };
            current = (*source).to_owned();
        }
    }

    fn count(&self, dag: &Dag, id: &str, original: &Phase) -> usize {
        let node = dag.get_node(id).unwrap();
        if node.extracted.partitions.is_empty() {
            return 1;
        }
        if let Some((_, source)) = self.source_for(dag, id) {
            return source.keys.len();
        }
        original
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .filter(|s| s.step_id.base_id() == id)
            .map(|s| s.partition_keys.len())
            .sum()
    }

    fn change(&self, dag: &Dag, id: &str) -> Change {
        let n = dag.get_node(id).unwrap();
        let Some(old) = self.previous.nodes.get(id) else {
            return Change::All;
        };
        if old.signature != self.signatures[id]
            || n.kind() != NodeKind::Asset
            || matches!(n.extracted.freshness, Freshness::Manual)
            || n.extracted.partitions.len() > 1
        {
            return Change::All;
        }
        let dim = n.extracted.partitions.keys().next();
        let mut changed = HashSet::new();
        for up in dag.upstream(id) {
            // Inherited partitions carry membership, not the whole source value. The
            // aligned data edge already carries the key's version through its chain.
            if !n.extracted.inherited_partitions.is_empty()
                && dag.partition_sources(id).contains(&up)
                && !n.resolved_inputs.values().any(|input| input == up)
            {
                continue;
            }
            let keyed_source = dim
                .filter(|dim| {
                    !n.extracted.inherited_partitions.contains(*dim)
                        && matches!(
                            n.extracted.partitions.get(*dim),
                            Some(PartitionSpec::DerivedFrom { .. })
                        )
                        && dag.partition_sources(id).contains(&up)
                })
                .and_then(|dim| {
                    let source = self.next.nodes.get(up)?.source.as_ref()?;
                    let previous = self.previous.nodes.get(up)?.source.as_ref()?;
                    let new = source.hashes.as_ref()?;
                    let old = previous.hashes.as_ref()?;
                    Some((dim, new, old))
                });
            if let Some((_, new, old)) = keyed_source {
                changed.extend(
                    new.iter()
                        .filter(|(k, v)| old.get(k) != Some(*v))
                        .map(|(k, _)| k.clone()),
                );
                // Removed keys affect collected consumers even though there is no
                // surviving partition to recompute.
                changed.extend(
                    old.iter()
                        .filter(|(k, _)| new.get(k).is_none())
                        .map(|(k, _)| k.clone()),
                );
                continue;
            }
            match self.changes.get(up) {
                Some(Change::Fresh) => {}
                Some(Change::Keys(up_dim, values)) if Some(up_dim) == dim => {
                    changed.extend(values.iter().cloned())
                }
                _ => return Change::All,
            }
        }
        match dim {
            Some(dim) if !changed.is_empty() => Change::Keys(dim.clone(), Arc::new(changed)),
            _ => Change::Fresh,
        }
    }

    /// Remove proven fresh steps before partition expansion and cache lookup.
    pub(crate) fn prune(&mut self, dag: &Dag, original: &Phase) -> (Phase, Vec<StepReport>, usize) {
        let planned: HashSet<_> = original
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .map(|s| s.step_id.base_id())
            .collect();
        let mut reports = Vec::new();
        let mut omitted = 0;
        for id in dag.order_for(planned.iter().copied()) {
            self.cached.remove(id);
            self.filters.remove(id);
            let change = self.change(dag, id);

            self.changes.insert(id.to_owned(), change.clone());
            let old = self.previous.nodes.get(id).cloned().unwrap_or_default();
            self.next.nodes.insert(
                id.to_owned(),
                Proof {
                    signature: self.signatures[id].clone(),
                    ..old
                },
            );
            let count = self.count(dag, id, original);
            self.next.nodes.get_mut(id).unwrap().count = count;
            let kept = match &change {
                Change::Fresh => 0,
                Change::Keys(dim, values) => {
                    let active = self.source_for(dag, id).map_or_else(
                        || values.clone(),
                        |(_, source)| {
                            Arc::new(
                                source
                                    .keys
                                    .iter()
                                    .filter(|k| values.contains(*k))
                                    .cloned()
                                    .collect(),
                            )
                        },
                    );
                    let kept = active.len().min(count);
                    self.filters.insert(id.to_owned(), (dim.clone(), active));
                    kept
                }
                Change::All => count,
            };
            let cached = count.saturating_sub(kept);
            if cached > 0 || (count == 0 && matches!(change, Change::Fresh)) {
                self.cached.insert(id.to_owned(), cached);
                omitted += cached;
                reports.push(StepReport {
                    id: id.to_owned(),
                    kind: crate::report::kind_str(dag.get_node(id).map(|n| n.kind())),
                    status: Some("cached".into()),
                    run_hash: if dag.get_node(id).unwrap().extracted.partitions.is_empty() {
                        self.next.nodes[id].run_hash.clone()
                    } else {
                        None
                    },
                    artifact: if dag.get_node(id).unwrap().extracted.partitions.is_empty() {
                        self.next.nodes[id].output.as_ref().map(|o| o.path.clone())
                    } else {
                        None
                    },
                    env: crate::report::env_report(dag, id),
                    undeclared_env: dag
                        .get_node(id)
                        .map(|n| n.extracted.undeclared_env())
                        .filter(|v| !v.is_empty()),
                    partitions: (!dag.get_node(id).unwrap().extracted.partitions.is_empty())
                        .then_some(PartitionSummary {
                            total: cached,
                            cached,
                            ..Default::default()
                        }),
                    ..Default::default()
                });
            }
        }
        let mut phase = original.clone();
        for stream in &mut phase.streams {
            stream.steps.retain_mut(|step| {
                let id = step.step_id.base_id();
                if matches!(self.changes.get(id), Some(Change::Fresh)) {
                    return false;
                }
                if let Some((dim, values)) = self.filters.get(id) {
                    step.partition_keys
                        .retain(|k| k.0.get(dim).is_some_and(|v| values.contains(v)));
                }
                true
            });
        }
        phase.streams.retain(|s| !s.steps.is_empty());
        (phase, reports, omitted)
    }

    pub(crate) fn observe(
        &mut self,
        dag: &Dag,
        phase: &Phase,
        outputs: &HashMap<String, OutputRef>,
        hashes: &HashMap<String, String>,
    ) {
        let bases: HashSet<_> = phase
            .streams
            .iter()
            .flat_map(|s| &s.steps)
            .map(|s| s.step_id.base_id())
            .collect();
        for id in bases {
            let Some(proof) = self.next.nodes.get_mut(id) else {
                continue;
            };
            if let Some(output) = outputs.get(id) {
                proof.output_hash = output.content_hash.clone();
                proof.output = Some(output.clone());
                proof.run_hash = hashes.get(id).cloned();
                if dag.feeds_partitions(id)
                    && let Ok(text) = std::fs::read_to_string(&output.path)
                    && let Ok(source) = crate::keyed::read_source(&text)
                {
                    proof.source = Some(Arc::new(match source {
                        crate::keyed::SourceKeys::List(keys) => Source { keys, hashes: None },
                        crate::keyed::SourceKeys::Keyed(keys, hashes) => Source {
                            keys,
                            hashes: Some(Arc::new(Hashes::new(hashes))),
                        },
                    }));
                }
                // A sensor runs every time, but consumers depend on what it observed.
                if dag
                    .get_node(id)
                    .is_some_and(|n| crate::cache::folds_output(dag, &n.id))
                    && proof.output_hash.is_some()
                    && self.previous.nodes.get(id).is_some_and(|old| {
                        (crate::cache::is_cutoff(dag, id)
                            || (old.signature == proof.signature && old.run_hash == proof.run_hash))
                            && old.output_hash == proof.output_hash
                    })
                {
                    self.changes.insert(id.to_owned(), Change::Fresh);
                }
            }
        }
    }

    pub(crate) fn filter(&self, base: &str, dims: &mut HashMap<String, Vec<String>>) {
        if let Some((dim, keys)) = self.filters.get(base)
            && let Some(values) = dims.get_mut(dim)
        {
            values.retain(|v| keys.contains(v));
        }
    }

    pub(crate) fn can_seed(&self, base: &str, key: Option<&PartitionKey>) -> bool {
        match self.changes.get(base) {
            Some(Change::Fresh) => true,
            Some(Change::Keys(dim, values)) => key
                .and_then(|k| k.0.get(dim))
                .is_some_and(|v| !values.contains(v)),
            _ => false,
        }
    }
    pub(crate) fn unseeded_fresh(&self, id: &str) -> bool {
        self.cached.contains_key(id) && !self.seeded.contains(id)
    }
    pub(crate) fn count_for(&self, id: &str) -> usize {
        self.next.nodes.get(id).map_or(0, |p| p.count)
    }
    pub(crate) fn unseeded_cached(&self) -> usize {
        self.cached
            .values()
            .sum::<usize>()
            .saturating_sub(self.seeded.len())
    }

    pub(crate) fn target_keys(&self, dag: &Dag, id: &str) -> Option<Vec<PartitionKey>> {
        let n = dag.get_node(id)?;
        let dim = n.extracted.partitions.keys().next()?;
        if let Some((_, source)) = self.source_for(dag, id) {
            return Some(
                source
                    .keys
                    .iter()
                    .map(|v| {
                        PartitionKey(std::collections::BTreeMap::from([(dim.clone(), v.clone())]))
                    })
                    .collect(),
            );
        }
        match n.extracted.partitions.get(dim)? {
            PartitionSpec::Static { values } => Some(
                values
                    .iter()
                    .map(|v| {
                        PartitionKey(std::collections::BTreeMap::from([(
                            dim.clone(),
                            match v {
                                crate::model::PartitionValue::Str(v) => v.clone(),
                                crate::model::PartitionValue::Int(v) => v.to_string(),
                            },
                        )]))
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Only dependencies that will be read enter the executor's cache/recovery state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn seed(
        &mut self,
        dag: &Dag,
        cfg: &crate::config::ResolvedConfig,
        ids: Vec<(String, Option<PartitionKey>)>,
        state: &mut crate::cache::DecideState,
        outputs: &mut HashMap<String, OutputRef>,
        cached_ids: &mut HashSet<String>,
        cached_steps: &mut crate::recover::CachedSteps,
        store: &mut Option<crate::store_sync::StoreSync>,
    ) -> Result<(), BarcaError> {
        if self.previous.nodes.is_empty() {
            return Ok(());
        }
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|(base, key)| {
                let display = key
                    .as_ref()
                    .map_or_else(|| base.clone(), |k| k.display_id(base));
                !outputs.contains_key(&display)
                    && !self.seeded.contains(&display)
                    && self.can_seed(base, key.as_ref())
            })
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let needs_rows = cfg.remote_artifacts() || ids.iter().any(|(_, key)| key.is_some());
        let cache = if needs_rows {
            Some(db::CacheReader::for_config(cfg).await?)
        } else {
            None
        };
        for (base, key) in ids {
            let display = key
                .as_ref()
                .map_or_else(|| base.clone(), |k| k.display_id(&base));
            if outputs.contains_key(&display)
                || self.seeded.contains(&display)
                || !self.can_seed(&base, key.as_ref())
            {
                continue;
            }
            if !self.previous.nodes.contains_key(&base) {
                continue;
            }
            let known = (key.is_none() && !cfg.remote_artifacts())
                .then(|| self.previous.nodes.get(&base))
                .flatten()
                .and_then(|p| {
                    Some((p.run_hash.clone()?, {
                        let mut output = p.output.clone()?;
                        output.content_hash = p.output_hash.clone();
                        output
                    }))
                });
            let Some((run_hash, output)) = (match known {
                Some(v) => Some(v),
                None => match &cache {
                    Some(cache) => latest(cache, &display, self.generation).await?,
                    None => None,
                },
            }) else {
                continue;
            };
            let output = crate::cache::accept_cache_hit(store, &display, output);
            self.seeded.insert(display.clone());
            let template = &self.templates[&base];
            let mut step = template.with_keys(key.into_iter().collect());
            step.run_hashes.insert(display.clone(), run_hash.clone());
            if !step.pending_partitions.is_empty() && !step.partition_keys.is_empty() {
                let node = dag.get_node(&base).unwrap();
                let mut sources = Vec::new();
                for dim in step.pending_partitions.keys() {
                    if let Some((source_id, source)) = self.source_for(dag, &base)
                        && let Some(hashes) = &source.hashes
                    {
                        let inherited = node.extracted.inherited_partitions.contains(dim);
                        let named = step
                            .inputs
                            .iter()
                            .find(|(_, up)| *up == source_id)
                            .map(|(p, _)| p.clone());
                        let param = if inherited {
                            None
                        } else {
                            Some(named.unwrap_or_else(|| {
                                crate::targets::short_name(source_id).to_owned()
                            }))
                        };
                        if let Some(param) = &param {
                            step.inputs.remove(param);
                        }
                        sources.push(crate::keyed::KeyedSource {
                            dim: dim.clone(),
                            source_id: source_id.to_owned(),
                            path: self.next.nodes[source_id]
                                .output
                                .as_ref()
                                .unwrap()
                                .path
                                .clone(),
                            param,
                            hashes: Arc::new(
                                step.partition_keys
                                    .iter()
                                    .filter_map(|key| {
                                        let value = key.0.get(dim)?;
                                        Some((value.clone(), hashes.get(value)?.clone()))
                                    })
                                    .collect(),
                            ),
                        });
                    }
                }
                step.keyed.sources = (!sources.is_empty()).then(|| Arc::new(sources));
                step.pending_partitions.clear();
            }
            state.seed_fresh(dag, &step, &display, run_hash, output.content_hash.as_ref());
            outputs.insert(display.clone(), output);
            cached_ids.insert(display);
            cached_steps.remember(step);
        }
        Ok(())
    }

    pub(crate) fn membership_changed(&self, dag: &Dag, id: &str) -> bool {
        let Some((source_id, source)) = self.source_for(dag, id) else {
            return false;
        };
        self.previous
            .nodes
            .get(source_id)
            .and_then(|p| p.source.as_ref())
            .is_none_or(|old| old.keys != source.keys)
    }

    pub(crate) fn summary(
        &self,
        id: &str,
        keys: Option<&[(String, OutputRef)]>,
        cached: &HashSet<String>,
    ) -> Option<KeySummary> {
        let total = self.next.nodes.get(id)?.count;
        if total <= LISTED_KEYS
            || self.templates.get(id)?.keyed.is_source
            || !self.cached.contains_key(id)
        {
            return None;
        }
        let mut computed: Vec<_> = keys
            .unwrap_or_default()
            .iter()
            .filter(|(key, _)| !cached.contains(&format!("{id}[{key}]")))
            .cloned()
            .collect();
        computed.sort_by(|a, b| a.0.cmp(&b.0));
        let count = computed.len();
        computed.truncate(LISTED_KEYS);
        Some(KeySummary {
            keys: total,
            computed: count,
            listed: computed,
        })
    }

    /// Preserve logical counts and node spans without manufacturing one report per fresh key.
    pub(crate) fn telemetry(&self, dag: &Dag, report: &mut crate::telemetry::RunReport) {
        use crate::telemetry::{StepOutcome, StepReport};
        let contexts = crate::telemetry::node_contexts(dag);
        for (base, count) in &self.cached {
            let present = report
                .steps
                .iter()
                .filter(|s| s.node.base_id == *base && self.seeded.contains(&s.node_id))
                .count();
            let mut omitted = count.saturating_sub(present);
            if omitted == 0 {
                continue;
            }
            let partitioned = dag
                .get_node(base)
                .is_some_and(|n| !n.extracted.partitions.is_empty());
            if !report.steps.iter().any(|s| s.node.base_id == *base) {
                report.steps.push(StepReport {
                    node_id: base.clone(),
                    node: Arc::new(contexts[base].clone()),
                    partition: partitioned.then(String::new),
                    outcome: StepOutcome::Cached,
                    start_unix_ns: report.start_unix_ns,
                    duration_ns: 0,
                    attempts: None,
                    run_hash: None,
                    upstream: dag.upstream(base).into_iter().map(str::to_owned).collect(),
                    size_bytes: None,
                    digest: None,
                    cpu_seconds: None,
                    max_rss_bytes: None,
                    error_type: None,
                    error_message: None,
                    error_traceback: None,
                });
                omitted -= 1;
            }
            if partitioned {
                report.cached_partitions.insert(base.clone(), omitted);
            }
        }
    }

    pub(crate) async fn save(
        &self,
        cfg: &crate::config::ResolvedConfig,
        run_id: &str,
        materialized: bool,
    ) -> Result<(), BarcaError> {
        if (!materialized && self.next == self.previous)
            || self.next.nodes.len() != self.templates.len()
        {
            return Ok(());
        }
        let cache = db::CacheReader::for_config(cfg).await?;
        // Another writer invalidates the proof rather than contributing unvalidated work.
        let mut rows = cache
            .conn()
            .query(
                "SELECT 1 FROM materializations WHERE id > ?1 AND (run_id IS NULL OR run_id != ?2) LIMIT 1",
                (self.generation, run_id.to_owned()),
            )
            .await
            .map_err(db_error)?;
        if rows.next().await.map_err(db_error)?.is_some() {
            return Ok(());
        }
        let generation = generation(cache.conn()).await?;
        db::with_tx(cache.conn(), "BEGIN IMMEDIATE", "freshness proofs", async {
            cache.conn().execute("DELETE FROM freshness_proofs WHERE generation != ?1", [generation]).await.map_err(db_error)?;
            let mut insert = db::prepare(cache.conn(), "INSERT OR REPLACE INTO freshness_proofs (id, generation, snapshot) VALUES (?1, ?2, ?3)").await.map_err(db_error)?;
            for (id, proof) in &self.next.nodes {
                let snapshot = serde_json::to_string(proof).map_err(|e| BarcaError::Other(e.to_string()))?;
                insert.execute((format!("{}:{id}", self.key), generation, snapshot)).await.map_err(db_error)?;
            }
            Ok(())
        }).await?;
        Ok(())
    }
}
fn db_error(e: impl std::fmt::Display) -> BarcaError {
    BarcaError::Other(format!("freshness proof: {e}"))
}
async fn generation(conn: &turso::Connection) -> Result<i64, BarcaError> {
    let mut rows = conn
        .query("SELECT COALESCE(MAX(id),0) FROM materializations", ())
        .await
        .map_err(db_error)?;
    rows.next()
        .await
        .map_err(db_error)?
        .unwrap()
        .get::<i64>(0)
        .map_err(db_error)
}
async fn latest(
    cache: &db::CacheReader,
    id: &str,
    through: i64,
) -> Result<Option<(String, OutputRef)>, BarcaError> {
    let mut rows = cache.conn().query("SELECT run_hash, artifact_path, artifact_format, artifact_size_bytes, output_hash FROM materializations WHERE node_id = ?1 AND id <= ?2 AND status = 'success' AND artifact_path IS NOT NULL ORDER BY id DESC LIMIT 1", (id.to_owned(), through)).await.map_err(db_error)?;
    let Some(row) = rows.next().await.map_err(db_error)? else {
        return Ok(None);
    };
    let path = row.get::<String>(1).map_err(db_error)?;
    if !cache.allows_artifact(&path) {
        return Ok(None);
    }
    Ok(Some((
        row.get::<String>(0).map_err(db_error)?,
        OutputRef {
            path,
            format: row.get::<String>(2).map_err(db_error)?,
            size_bytes: row.get::<i64>(3).map_err(db_error)? as u64,
            elapsed_seconds: None,
            content_hash: row.get::<String>(4).ok(),
        },
    )))
}
