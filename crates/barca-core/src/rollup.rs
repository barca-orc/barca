//! The rollup (`specs/behavior/health.md`, "Rollup"): one tree over the rows of a status
//! result, root → group → … → node, with nodes in no group under their source file. Every
//! node has exactly one parent, so every level is the exact sum of its children and no two
//! surfaces can disagree about a scope: `GET /state/rollup`, `barca status --rollup` and the
//! web UI's health map, bars, group rows, causes list and failures view all read this one
//! response.
//!
//! This is classification: it reads rows [`crate::health::classify`] has filled, the groups
//! of the graph and which nodes a run is working on, and writes nothing.
//!
//! Both units are always present: `nodes` and `health` count every node, `assets` and
//! `asset_health` count assets only (tasks and sensors are nodes, not assets).
//!
//! SEAMS for the issues that follow:
//!
//! - #961 to #963 add `causes`, `asset_causes`, `gap_seconds`, `unbounded` and `fresh_by` to
//!   [`RollupEntry`]: a leaf reads them off its row in [`leaf`], and [`Sums::add`] is the one
//!   place a level adds up its children.
//! - #963 replaces [`roots`] with the cause-based list: each row's `root`
//!   ([`crate::health::Root`]) already names what holds it, so the list stays "group the rows
//!   by `root.id`", with `causes`, `gap_seconds`, `unbounded` and `action` added to
//!   [`RollupRoot`].

use crate::BarcaError;
use crate::groups::NodeGroup;
use crate::health::{DataCoverage, Health, RootKind};
use crate::status::NodeStatus;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The id of the root entry.
pub const ROOT_ID: &str = "root";

/// What an entry of the tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Everything in scope: id `root`.
    Root,
    /// A `group(...)`: id is the group's own (`group:<file>:<name>`).
    Group,
    /// The nodes of one source file that are in no group: id `file:<path>`.
    File,
    /// A node: id is the node's.
    Node,
}

/// How many of a scope's nodes (or assets) are in each health, most severe first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HealthCounts {
    pub failing: usize,
    pub blocked: usize,
    pub suspect: usize,
    pub behind: usize,
    pub undecided: usize,
    pub never_run: usize,
    pub ok: usize,
}

impl HealthCounts {
    /// How many are in `health`.
    pub fn get(&self, health: Health) -> usize {
        match health {
            Health::Failing => self.failing,
            Health::Blocked => self.blocked,
            Health::Suspect => self.suspect,
            Health::Behind => self.behind,
            Health::Undecided => self.undecided,
            Health::NeverRun => self.never_run,
            Health::Ok => self.ok,
        }
    }

    fn slot(&mut self, health: Health) -> &mut usize {
        match health {
            Health::Failing => &mut self.failing,
            Health::Blocked => &mut self.blocked,
            Health::Suspect => &mut self.suspect,
            Health::Behind => &mut self.behind,
            Health::Undecided => &mut self.undecided,
            Health::NeverRun => &mut self.never_run,
            Health::Ok => &mut self.ok,
        }
    }

    fn add(&mut self, other: &HealthCounts) {
        for health in Health::ALL {
            *self.slot(health) += other.get(health);
        }
    }

    /// Every count added up: the nodes (or assets) counted.
    pub fn total(&self) -> usize {
        Health::ALL.iter().map(|h| self.get(*h)).sum()
    }

    /// The most severe health counted; `None` for an empty scope.
    pub fn worst(&self) -> Option<Health> {
        Health::ALL.into_iter().find(|h| self.get(*h) > 0)
    }

    /// `2 failing · 12 blocked · 380 ok`: every health counted, most severe first, in the
    /// display words; empty for an empty scope.
    pub fn line(&self) -> String {
        Health::ALL
            .into_iter()
            .filter(|h| self.get(*h) > 0)
            .map(|h| format!("{} {}", self.get(h), h.word()))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A partitioned node's keys, by how they stand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeyCounts {
    /// Keys whose last attempt failed.
    pub failed: usize,
    /// Keys without a cached result that did not fail: stale or never run.
    pub behind: usize,
    /// Keys with a cached result.
    pub ok: usize,
}

/// One entry of the tree. Every count is the exact sum of the entry's children's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RollupEntry {
    /// `root`, the group's id (`group:<file>:<name>`), `file:<path>` or the node id.
    pub id: String,
    pub kind: EntryKind,
    /// The group's or node's declared name, the file's path, `all` for the root.
    pub label: String,
    /// The entry it sits under; `null` for the root.
    pub parent: Option<String>,
    /// Its children's ids: worst health first, then most assets, then label.
    pub children: Vec<String>,
    /// Nodes underneath (1 for a node).
    pub nodes: usize,
    /// Assets underneath: tasks and sensors are nodes, not assets.
    pub assets: usize,
    /// Nodes underneath a run is working on now.
    pub running: usize,
    /// Nodes underneath by health.
    pub health: HealthCounts,
    /// Assets underneath by health.
    pub asset_health: HealthCounts,
    /// How much of the data underneath was measured: `assets` here is `assets` above.
    pub data_coverage: DataCoverage,
    /// A partitioned node's keys (a node entry only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub keys: Option<KeyCounts>,
}

impl RollupEntry {
    fn inner(id: String, kind: EntryKind, label: String) -> Self {
        RollupEntry {
            id,
            kind,
            label,
            parent: None,
            children: Vec::new(),
            nodes: 0,
            assets: 0,
            running: 0,
            health: HealthCounts::default(),
            asset_health: HealthCounts::default(),
            data_coverage: DataCoverage::default(),
            keys: None,
        }
    }
}

/// What a level adds up from its children: the one place a new summed field is added.
trait Sums {
    fn add(&mut self, child: &RollupEntry);
}

impl Sums for RollupEntry {
    fn add(&mut self, child: &RollupEntry) {
        self.nodes += child.nodes;
        self.assets += child.assets;
        self.running += child.running;
        self.health.add(&child.health);
        self.asset_health.add(&child.asset_health);
        self.data_coverage.assets += child.data_coverage.assets;
        self.data_coverage.measured += child.data_coverage.measured;
        self.data_coverage.suspect += child.data_coverage.suspect;
    }
}

/// One upstream fact and what it holds. Today only failures: a failing node with the nodes it
/// blocks. `causes`, `gap_seconds`, `unbounded` and `action` are absent until #961 to #963.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RollupRoot {
    /// `<kind>:<node id>`: the `root.id` of every row it holds.
    pub id: String,
    pub kind: RootKind,
    /// The node the fact is about: the failing node.
    pub node: String,
    /// The fact in a line: a failure's error headline as recorded.
    pub detail: String,
    /// A failure's error headline with its numbers replaced by `#`: failure roots sharing one
    /// are shown as one entry. `null` when no error was recorded.
    pub error_group: Option<String>,
    /// Nodes it holds: the failing node and every node it blocks.
    pub nodes: usize,
    /// Of them, assets.
    pub assets: usize,
}

/// `GET /state/rollup` and `barca status --rollup --json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Rollup {
    /// When the rows were classified: RFC 3339 UTC.
    pub as_of: String,
    /// Every entry, the root first, then depth first in `children` order.
    pub entries: Vec<RollupEntry>,
    /// What holds the nodes that are not fresh, the most nodes first, then by id.
    pub roots: Vec<RollupRoot>,
    /// The root entry's data coverage.
    pub data_coverage: DataCoverage,
}

impl Rollup {
    /// The root entry.
    pub fn root(&self) -> &RollupEntry {
        &self.entries[0]
    }
}

/// The source file of a node id: what is before its last `:`.
fn source_file(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(file, _)| file)
}

/// A node's own entry.
///
/// SEAM (#961 to #963): `causes`, `gap_seconds`, `unbounded` and `fresh_by` are read off the
/// row here.
fn leaf(node: &NodeStatus, running: bool) -> RollupEntry {
    let mut entry = RollupEntry::inner(node.id.clone(), EntryKind::Node, node.name.clone());
    let asset = node.kind == "asset";
    entry.nodes = 1;
    entry.assets = usize::from(asset);
    entry.running = usize::from(running);
    *entry.health.slot(node.health) = 1;
    if asset {
        *entry.asset_health.slot(node.health) = 1;
    }
    entry.data_coverage = DataCoverage::of(std::slice::from_ref(node));
    entry.keys = node
        .partitions
        .as_ref()
        .filter(|p| node.partitioned && p.total > 0)
        .map(|p| KeyCounts {
            failed: p.failed,
            behind: p.total.saturating_sub(p.cached).saturating_sub(p.failed),
            ok: p.cached,
        });
    entry
}

/// Whether `ancestor` is `id` or above it.
fn is_above(parent: &HashMap<String, String>, ancestor: &str, id: &str) -> bool {
    let mut at = Some(id);
    let mut steps = 0;
    while let Some(here) = at {
        if here == ancestor {
            return true;
        }
        steps += 1;
        if steps > parent.len() + 1 {
            return true;
        }
        at = parent.get(here).map(String::as_str);
    }
    false
}

/// The rollup of `nodes`, which [`crate::health::classify`] has filled: one status result's
/// rows (a scope with its upstream cone). `groups` are the graph's; a group with no node in
/// scope is left out. `running` holds the nodes a run is working on. A node (or group)
/// declared in several groups sits under the first by group id.
pub fn build(
    nodes: &[NodeStatus],
    groups: &[NodeGroup],
    running: &HashSet<String>,
    as_of: chrono::DateTime<chrono::Utc>,
) -> Rollup {
    let mut entries: HashMap<String, RollupEntry> = nodes
        .iter()
        .map(|n| (n.id.clone(), leaf(n, running.contains(&n.id))))
        .collect();
    let by_id: BTreeMap<&str, &NodeGroup> = groups.iter().map(|g| (g.id.as_str(), g)).collect();

    // Parents: groups in id order claim their members, so the first by id wins.
    let mut parent: HashMap<String, String> = HashMap::new();
    for (id, group) in &by_id {
        for member in &group.members {
            let known = entries.contains_key(member) || by_id.contains_key(member.as_str());
            if !known || parent.contains_key(member) || is_above(&parent, member, id) {
                continue;
            }
            parent.insert(member.clone(), (*id).to_string());
        }
    }
    for id in by_id.keys() {
        let group = by_id[id];
        entries.insert(
            (*id).to_string(),
            RollupEntry::inner((*id).to_string(), EntryKind::Group, group.name.clone()),
        );
    }
    // Nodes in no group sit under their source file.
    for node in nodes {
        if parent.contains_key(&node.id) {
            continue;
        }
        let file = source_file(&node.id);
        let id = format!("file:{file}");
        entries
            .entry(id.clone())
            .or_insert_with(|| RollupEntry::inner(id.clone(), EntryKind::File, file.to_string()));
        parent.insert(node.id.clone(), id);
    }
    entries.insert(
        ROOT_ID.to_string(),
        RollupEntry::inner(ROOT_ID.to_string(), EntryKind::Root, "all".to_string()),
    );
    let ids: Vec<String> = entries
        .keys()
        .filter(|id| *id != ROOT_ID)
        .cloned()
        .collect();
    for id in ids {
        let above = parent
            .get(&id)
            .cloned()
            .unwrap_or_else(|| ROOT_ID.to_string());
        if let Some(entry) = entries.get_mut(&id) {
            entry.parent = Some(above.clone());
        }
        if let Some(entry) = entries.get_mut(&above) {
            entry.children.push(id);
        }
    }

    // Sums bottom-up, then each level's children in display order; an empty group is dropped.
    fn sum(id: &str, entries: &mut HashMap<String, RollupEntry>) {
        let children = std::mem::take(&mut entries.get_mut(id).expect("an entry").children);
        let mut kept = Vec::with_capacity(children.len());
        for child in children {
            sum(&child, entries);
            if entries[&child].nodes == 0 {
                entries.remove(&child);
                continue;
            }
            let summed = entries[&child].clone();
            entries.get_mut(id).expect("an entry").add(&summed);
            kept.push(child);
        }
        kept.sort_by(|a, b| {
            let (a, b) = (&entries[a], &entries[b]);
            a.health
                .worst()
                .cmp(&b.health.worst())
                .then(b.assets.cmp(&a.assets))
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.id.cmp(&b.id))
        });
        entries.get_mut(id).expect("an entry").children = kept;
    }
    sum(ROOT_ID, &mut entries);

    fn walk(id: &str, entries: &mut HashMap<String, RollupEntry>, out: &mut Vec<RollupEntry>) {
        let Some(entry) = entries.remove(id) else {
            return;
        };
        let children = entry.children.clone();
        out.push(entry);
        for child in children {
            walk(&child, entries, out);
        }
    }
    let mut ordered = Vec::with_capacity(entries.len());
    walk(ROOT_ID, &mut entries, &mut ordered);
    let data_coverage = ordered[0].data_coverage;
    Rollup {
        as_of: as_of.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        entries: ordered,
        roots: roots(nodes),
        data_coverage,
    }
}

/// The roots of `nodes`: each `root` the rows name, with how much it holds, the most nodes
/// first, then by id. Today every root is a failing node holding itself and what it blocks.
///
/// SEAM (#963): the cause-based list. Rows already carry `root`; this adds `causes`,
/// `gap_seconds`, `unbounded` and `action`, and ranks by `gap_seconds` first.
pub fn roots(nodes: &[NodeStatus]) -> Vec<RollupRoot> {
    let by_id: HashMap<&str, &NodeStatus> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut out: BTreeMap<&str, RollupRoot> = BTreeMap::new();
    for node in nodes {
        let Some(root) = &node.root else { continue };
        let held = out.entry(root.id.as_str()).or_insert_with(|| {
            let (detail, error_group) = by_id
                .get(root.node.as_str())
                .map(|n| failure_detail(n))
                .unwrap_or_default();
            RollupRoot {
                id: root.id.clone(),
                kind: root.kind,
                node: root.node.clone(),
                detail,
                error_group,
                nodes: 0,
                assets: 0,
            }
        });
        held.nodes += 1;
        held.assets += usize::from(node.kind == "asset");
    }
    let mut out: Vec<RollupRoot> = out.into_values().collect();
    out.sort_by(|a, b| b.nodes.cmp(&a.nodes).then_with(|| a.id.cmp(&b.id)));
    out
}

/// A failing node's error in a line, and the group it shares with errors that differ only in
/// their numbers. A node whose keys failed with no error of its own says how many keys.
fn failure_detail(node: &NodeStatus) -> (String, Option<String>) {
    let error = node
        .last_materialization
        .as_ref()
        .filter(|m| m.status == "failed")
        .and_then(|m| m.error.as_deref())
        .unwrap_or("");
    if let Some(line) = headline(error) {
        let group = error_group(&line);
        return (line, Some(group));
    }
    match node.partitions.as_ref().filter(|p| p.failed > 0) {
        Some(p) => (format!("{} of {} keys failed", p.failed, p.total), None),
        None => ("failed, with no error recorded".to_string(), None),
    }
}

/// An error's one line: its first, or a bare traceback's last; no frames. `None` for an empty
/// error.
pub fn headline(error: &str) -> Option<String> {
    let mut lines = error.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    // A bare traceback says what went wrong on its last line, not its first.
    let bare =
        first.starts_with("Traceback (most recent call last)") || first.starts_with("File \"");
    Some(
        if bare {
            lines.next_back().unwrap_or(first)
        } else {
            first
        }
        .to_string(),
    )
}

/// `headline` with each number replaced by `#`, so `row count fell 64% day over day` and
/// `… 12% …` are one group. A number is a run of digits with `,`, `.` or `_` inside it.
pub fn error_group(headline: &str) -> String {
    let chars: Vec<char> = headline.chars().collect();
    let mut out = String::with_capacity(headline.len());
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut end = i;
        let mut j = i;
        while j < chars.len() && (chars[j].is_ascii_digit() || matches!(chars[j], ',' | '.' | '_'))
        {
            if chars[j].is_ascii_digit() {
                end = j;
            }
            j += 1;
        }
        out.push('#');
        i = end + 1;
    }
    out
}

/// `barca status --rollup` on this machine: the status of the targets' upstream cones (the
/// whole graph without targets), classified with the evidence the metadata DB holds, summed.
/// No run is tracked here, so `running` is 0 everywhere.
pub async fn rollup(
    cfg: &crate::config::ResolvedConfig,
    target_names: &[String],
    file_args: &[String],
    python: &std::path::Path,
) -> Result<Rollup, BarcaError> {
    let dag = crate::load::build_dag(file_args, python).await?;
    let mut result =
        crate::status::status_from_dag(cfg, target_names, &dag, python, 0, false).await?;
    let now = chrono::Utc::now();
    crate::health::attach_from_db(
        &mut result,
        &dag,
        &cfg.db_path,
        &crate::schedule::Zone::Local,
        now,
    )
    .await?;
    Ok(build(&result.nodes, &dag.groups, &HashSet::new(), now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_state::CacheState;
    use crate::health::{Evidence, Root, classify};
    use crate::status::{LastMaterialization, PartitionState};

    fn node(id: &str, kind: &str, inputs: &[&str], state: CacheState) -> NodeStatus {
        let name = id.rsplit(':').next().unwrap();
        let mut n: NodeStatus = serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "kind": kind, "inputs": inputs, "partitioned": false,
            "cache": {"state": "unknown", "reason": "", "detail": ""},
            "last_materialization": null, "shape": null, "env": [],
        }))
        .unwrap();
        n.cache.state = state;
        n
    }

    fn failed(mut n: NodeStatus, error: &str) -> NodeStatus {
        n.last_materialization = Some(LastMaterialization {
            content_hash: None,
            status: "failed".to_string(),
            created_at: "2026-11-16 10:00:00".to_string(),
            elapsed_seconds: None,
            run_hash: None,
            artifact: None,
            format: None,
            size_bytes: None,
            rows: None,
            partition: None,
            error: Some(error.to_string()),
        });
        n
    }

    fn keys(mut n: NodeStatus, total: usize, cached: usize, failed: usize) -> NodeStatus {
        n.partitioned = true;
        n.partitions = Some(PartitionState {
            total,
            cached,
            missing: total - cached,
            failed,
            ..serde_json::from_value(serde_json::json!({
                "total": 0, "cached": 0, "missing": 0, "missing_keys": [],
            }))
            .unwrap()
        });
        n
    }

    fn group(id: &str, members: &[&str]) -> NodeGroup {
        NodeGroup {
            id: id.to_string(),
            name: id.rsplit(':').next().unwrap().to_string(),
            description: String::new(),
            members: members.iter().map(|m| m.to_string()).collect(),
            output: String::new(),
        }
    }

    fn classified(mut nodes: Vec<NodeStatus>) -> Vec<NodeStatus> {
        classify(
            &mut nodes,
            Evidence {
                ages: &HashMap::new(),
                pinned: &HashSet::new(),
                late: &HashSet::new(),
            },
        );
        nodes
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-11-17T14:57:00Z")
            .unwrap()
            .to_utc()
    }

    /// A sensor and a failing asset in `ingest`; two assets it blocks and a task in `model`,
    /// nested in `reports`; a loose cached asset and a loose task in another file.
    fn fixture() -> (Vec<NodeStatus>, Vec<NodeGroup>) {
        let nodes = classified(vec![
            node("p.py:inbox", "sensor", &[], CacheState::AlwaysRuns),
            failed(
                node("p.py:orders", "asset", &["p.py:inbox"], CacheState::Stale),
                "ValueError: 12 rows rejected",
            ),
            node("p.py:clean", "asset", &["p.py:orders"], CacheState::Stale),
            keys(
                node("p.py:scored", "asset", &["p.py:clean"], CacheState::Partial),
                10,
                6,
                0,
            ),
            node("p.py:notify", "task", &[], CacheState::AlwaysRuns),
            node("q.py:lookup", "asset", &[], CacheState::Cached),
            node("q.py:sweep", "task", &[], CacheState::AlwaysRuns),
        ]);
        let groups = vec![
            group("group:p.py:ingest", &["p.py:inbox", "p.py:orders"]),
            group("group:p.py:model", &["p.py:clean", "p.py:scored"]),
            group("group:p.py:reports", &["group:p.py:model", "p.py:notify"]),
        ];
        (nodes, groups)
    }

    fn entry<'a>(rollup: &'a Rollup, id: &str) -> &'a RollupEntry {
        rollup
            .entries
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("no entry {id}"))
    }

    /// INV-HEALTH-03: every count of an entry is the sum over its children, and every node is
    /// under exactly one parent.
    fn assert_sums(rollup: &Rollup, nodes: &[NodeStatus]) {
        let by_id: HashMap<&str, &RollupEntry> =
            rollup.entries.iter().map(|e| (e.id.as_str(), e)).collect();
        assert_eq!(by_id.len(), rollup.entries.len(), "entry ids are unique");
        let mut parents: HashMap<&str, usize> = HashMap::new();
        for e in &rollup.entries {
            for child in &e.children {
                *parents.entry(child.as_str()).or_default() += 1;
                assert_eq!(by_id[child.as_str()].parent.as_deref(), Some(e.id.as_str()));
            }
            assert_eq!(e.health.total(), e.nodes, "{}: health counts nodes", e.id);
            assert_eq!(e.asset_health.total(), e.assets, "{}", e.id);
            assert_eq!(e.data_coverage.assets, e.assets, "{}", e.id);
            if e.kind == EntryKind::Node {
                assert!(e.children.is_empty());
                assert_eq!(e.nodes, 1);
                continue;
            }
            let mut sum = RollupEntry::inner(String::new(), e.kind, String::new());
            for child in &e.children {
                sum.add(by_id[child.as_str()]);
            }
            assert_eq!(
                (sum.nodes, sum.assets, sum.running),
                (e.nodes, e.assets, e.running),
                "{}",
                e.id
            );
            assert_eq!((sum.health, sum.asset_health), (e.health, e.asset_health));
            assert_eq!(sum.data_coverage, e.data_coverage);
        }
        for e in rollup.entries.iter().filter(|e| e.kind != EntryKind::Root) {
            assert_eq!(parents.get(e.id.as_str()), Some(&1), "{}: one parent", e.id);
        }
        assert!(rollup.root().parent.is_none());
        let leaves: HashSet<&str> = rollup
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Node)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(leaves, nodes.iter().map(|n| n.id.as_str()).collect());
        assert_eq!(rollup.root().nodes, nodes.len());
        assert_eq!(rollup.data_coverage, DataCoverage::of(nodes));
    }

    #[test]
    fn every_level_is_the_sum_of_its_children_in_both_units() {
        let (nodes, groups) = fixture();
        let running = HashSet::from(["p.py:scored".to_string()]);
        let rollup = build(&nodes, &groups, &running, now());
        assert_sums(&rollup, &nodes);
        assert_eq!(rollup.as_of, "2026-11-17T14:57:00Z");

        // Nodes and assets are both stated: 7 nodes, of which 4 are assets.
        let root = rollup.root();
        assert_eq!((root.id.as_str(), root.kind), (ROOT_ID, EntryKind::Root));
        assert_eq!((root.nodes, root.assets, root.running), (7, 4, 1));
        assert_eq!(
            root.health,
            HealthCounts {
                failing: 1,
                blocked: 2,
                never_run: 3,
                ok: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            root.asset_health,
            HealthCounts {
                failing: 1,
                blocked: 2,
                ok: 1,
                ..Default::default()
            }
        );
        // Worst first: the failing group, the blocked one, then the file of the loose nodes.
        assert_eq!(
            root.children,
            ["group:p.py:ingest", "group:p.py:reports", "file:q.py"]
        );
        let reports = entry(&rollup, "group:p.py:reports");
        assert_eq!((reports.nodes, reports.assets), (3, 2));
        assert_eq!(reports.children, ["group:p.py:model", "p.py:notify"]);
        assert_eq!(reports.label, "reports");
        let file = entry(&rollup, "file:q.py");
        assert_eq!((file.kind, file.label.as_str()), (EntryKind::File, "q.py"));
        assert_eq!((file.nodes, file.assets), (2, 1));
        // A partitioned node's keys: failed, not up to date, ok.
        let scored = entry(&rollup, "p.py:scored");
        assert_eq!(
            scored.keys,
            Some(KeyCounts {
                failed: 0,
                behind: 4,
                ok: 6
            })
        );
        assert_eq!(scored.parent.as_deref(), Some("group:p.py:model"));
        assert_eq!(entry(&rollup, "p.py:clean").keys, None);
        // The root comes first, then depth first in display order.
        let order: Vec<&str> = rollup.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            order,
            [
                "root",
                "group:p.py:ingest",
                "p.py:orders",
                "p.py:inbox",
                "group:p.py:reports",
                "group:p.py:model",
                "p.py:clean",
                "p.py:scored",
                "p.py:notify",
                "file:q.py",
                "q.py:sweep",
                "q.py:lookup",
            ]
        );
    }

    #[test]
    fn a_node_in_several_groups_is_counted_once_under_the_first_by_group_id() {
        let nodes = classified(vec![
            node("p.py:a", "asset", &[], CacheState::Cached),
            node("p.py:b", "asset", &[], CacheState::Stale),
        ]);
        // Declared out of id order, with a group in two groups and a cycle between two.
        let groups = vec![
            group("group:p.py:z", &["p.py:a", "p.py:b", "group:p.py:inner"]),
            group("group:p.py:m", &["p.py:a", "group:p.py:inner"]),
            group("group:p.py:inner", &["p.py:b", "group:p.py:m"]),
        ];
        let rollup = build(&nodes, &groups, &HashSet::new(), now());
        assert_sums(&rollup, &nodes);
        // `inner` sorts first and claims `b` and `m`; `m` claims `a` and cannot claim `inner`,
        // which is above it; `z` is left with `inner` alone.
        assert_eq!(
            entry(&rollup, "p.py:b").parent.as_deref(),
            Some("group:p.py:inner")
        );
        assert_eq!(
            entry(&rollup, "p.py:a").parent.as_deref(),
            Some("group:p.py:m")
        );
        assert_eq!(
            entry(&rollup, "group:p.py:m").parent.as_deref(),
            Some("group:p.py:inner")
        );
        assert_eq!(
            entry(&rollup, "group:p.py:z").children,
            ["group:p.py:inner"]
        );
        assert_eq!(rollup.root().children, ["group:p.py:z"]);
        assert_eq!(rollup.root().nodes, 2);
    }

    #[test]
    fn a_scope_keeps_only_the_groups_with_a_node_in_it_and_an_empty_scope_is_a_bare_root() {
        let (nodes, groups) = fixture();
        let scoped: Vec<NodeStatus> = nodes
            .iter()
            .filter(|n| ["p.py:inbox", "p.py:orders"].contains(&n.id.as_str()))
            .cloned()
            .collect();
        let rollup = build(&scoped, &groups, &HashSet::new(), now());
        assert_sums(&rollup, &scoped);
        assert_eq!(rollup.root().children, ["group:p.py:ingest"]);
        let empty = build(&[], &groups, &HashSet::new(), now());
        assert_eq!(empty.entries.len(), 1);
        assert_eq!(empty.root().nodes, 0);
        assert_eq!(empty.root().health.worst(), None);
        assert!(empty.roots.is_empty());
    }

    #[test]
    fn a_failing_node_is_a_root_holding_itself_and_what_it_blocks() {
        let (mut nodes, groups) = fixture();
        nodes.push(failed(
            node("q.py:audit", "task", &[], CacheState::AlwaysRuns),
            "ValueError: 7 rows rejected",
        ));
        nodes.push(keys(
            node("q.py:daily", "asset", &[], CacheState::Partial),
            8,
            5,
            3,
        ));
        let nodes = classified(nodes);
        let rollup = build(&nodes, &groups, &HashSet::new(), now());
        assert_sums(&rollup, &nodes);
        // The most nodes first, then by id. `orders` holds itself, `clean` and `scored`.
        assert_eq!(
            rollup.roots,
            [
                RollupRoot {
                    id: "failure:p.py:orders".into(),
                    kind: RootKind::Failure,
                    node: "p.py:orders".into(),
                    detail: "ValueError: 12 rows rejected".into(),
                    error_group: Some("ValueError: # rows rejected".into()),
                    nodes: 3,
                    assets: 3,
                },
                RollupRoot {
                    id: "failure:q.py:audit".into(),
                    kind: RootKind::Failure,
                    node: "q.py:audit".into(),
                    detail: "ValueError: 7 rows rejected".into(),
                    error_group: Some("ValueError: # rows rejected".into()),
                    nodes: 1,
                    assets: 0,
                },
                RollupRoot {
                    id: "failure:q.py:daily".into(),
                    kind: RootKind::Failure,
                    node: "q.py:daily".into(),
                    detail: "3 of 8 keys failed".into(),
                    error_group: None,
                    nodes: 1,
                    assets: 1,
                },
            ]
        );
        // Each row names the root that holds it, and the roots' nodes are exactly those rows.
        let held: usize = rollup.roots.iter().map(|r| r.nodes).sum();
        assert_eq!(held, nodes.iter().filter(|n| n.root.is_some()).count());
        assert_eq!(
            nodes.iter().find(|n| n.id == "p.py:scored").unwrap().root,
            Some(Root::failure("p.py:orders"))
        );
        let root = rollup.root();
        assert_eq!(held, root.health.failing + root.health.blocked);
    }

    #[test]
    fn an_error_is_one_line_and_its_numbers_do_not_tell_groups_apart() {
        assert_eq!(
            headline("  \nValueError: bad\n  at x"),
            Some("ValueError: bad".into())
        );
        assert_eq!(
            headline("Traceback (most recent call last):\n  File \"a.py\", line 3\nKeyError: 'id'"),
            Some("KeyError: 'id'".into())
        );
        assert_eq!(
            headline("File \"a.py\", line 3"),
            Some("File \"a.py\", line 3".into())
        );
        assert_eq!(headline(" \n"), None);
        assert_eq!(
            error_group("row count fell 64% day over day"),
            error_group("row count fell 12% day over day")
        );
        assert_eq!(
            error_group("got 1,205.5 rows, 3 files."),
            "got # rows, # files."
        );
        assert_eq!(error_group("v1_2 at 10:42"), "v# at #:#");
        assert_eq!(error_group("no numbers"), "no numbers");
    }

    #[test]
    fn the_counts_read_as_one_line_most_severe_first() {
        let (nodes, groups) = fixture();
        let rollup = build(&nodes, &groups, &HashSet::new(), now());
        assert_eq!(
            rollup.root().health.line(),
            "1 failing · 2 blocked · 3 never run · 1 ok"
        );
        assert_eq!(rollup.root().health.worst(), Some(Health::Failing));
        assert_eq!(HealthCounts::default().line(), "");
    }

    #[test]
    fn the_wire_names_are_the_specified_ones() {
        let (nodes, groups) = fixture();
        let rollup = build(&nodes, &groups, &HashSet::new(), now());
        let json = serde_json::to_value(&rollup).unwrap();
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&json), ["as_of", "data_coverage", "entries", "roots"]);
        assert_eq!(
            keys(&json["entries"][0]),
            [
                "asset_health",
                "assets",
                "children",
                "data_coverage",
                "health",
                "id",
                "kind",
                "label",
                "nodes",
                "parent",
                "running"
            ]
        );
        assert_eq!(json["entries"][0]["parent"], serde_json::Value::Null);
        assert_eq!(json["entries"][0]["kind"], "root");
        assert_eq!(
            keys(&json["entries"][0]["health"]),
            [
                "behind",
                "blocked",
                "failing",
                "never_run",
                "ok",
                "suspect",
                "undecided"
            ]
        );
        let scored = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == "p.py:scored")
            .unwrap();
        assert_eq!(
            scored["keys"],
            serde_json::json!({"failed": 0, "behind": 4, "ok": 6})
        );
        assert_eq!(scored["kind"], "node");
        assert_eq!(
            keys(&json["roots"][0]),
            [
                "assets",
                "detail",
                "error_group",
                "id",
                "kind",
                "node",
                "nodes"
            ]
        );
        assert_eq!(json["roots"][0]["kind"], "failure");
        let back: Rollup = serde_json::from_value(json).unwrap();
        assert_eq!(back, rollup);
    }
}
