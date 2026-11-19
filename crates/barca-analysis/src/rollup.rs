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
//! The same counts are kept by cause (`causes`, `asset_causes`), with the freshness gap
//! (`gap_seconds`) and how many nodes have no estimate (`unbounded`); [`Sums::add`] is the one
//! place a level adds up its children.
//!
//! [`roots`] is the to-do list: each row's `root` ([`crate::health::Root`]) names what holds
//! it, so the list is the rows grouped by `root.id`, each with what it holds and the action
//! that clears it.

use crate::estimates::Evidence;
pub use crate::views::{
    ActionKind, CauseCounts, EntryKind, FRESH_WEIGHT, HealthCounts, HeldBy, HeldSplit, KeyCounts,
    KindCounts, KindRollup, ROOT_ID, RollupEntry, Weights,
};
use crate::views::{
    DataCoverage, Health, NodeStatus, RootKind, Sums, last_attempt_failed, latest, oldest,
};
use barca_compile::groups::NodeGroup;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// What clears a root, and what that costs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RootAction {
    pub kind: ActionKind,
    /// The `barca` command that clears the root, when one does; `null` when the action is
    /// not a command (waiting, a schedule to declare, a server setting).
    pub command: Option<String>,
    /// The typical compute time of the nodes the root holds, whole seconds: the sum of
    /// their typical step durations (a partitioned node once per key without a result; a
    /// node without samples takes the project's median). `null` when none has a sample.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub estimated_seconds: Option<u64>,
    /// The `barca` command that brings what the root holds up to date once, now, where
    /// `command` is `null` because what clears the root is a declaration (`add_schedule`):
    /// the chain is fresh after it and falls behind again at the next change. `null` for
    /// every other action.
    #[serde(default)]
    pub run_once: Option<String>,
}

/// One upstream fact and what it holds: every node whose `root.id` is this root's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RollupRoot {
    /// `<kind>:<node id>`: the `root.id` of every row it holds.
    pub id: String,
    pub kind: RootKind,
    /// The node the fact is about: the failing node, the sensor, the node whose code
    /// changed, the scheduled node.
    pub node: String,
    /// The fact in a line: a failure's error headline as recorded, what changed, why no
    /// tick comes, how long its ticks wait for a run slot.
    pub detail: String,
    /// A failure's error headline with its numbers replaced by `#`: failure roots sharing one
    /// are shown as one entry. `null` for every other kind and when no error was recorded.
    pub error_group: Option<String>,
    /// Nodes it holds.
    pub nodes: usize,
    /// Of them, assets.
    pub assets: usize,
    /// The nodes it holds, by cause.
    #[serde(default)]
    pub causes: CauseCounts,
    /// The freshness gap it holds, node-seconds: the sum of its nodes' `gap_seconds`.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub gap_seconds: i64,
    /// Of its nodes, those with no `expected_fresh_at`: their gap grows until someone acts.
    #[serde(default)]
    pub unbounded: usize,
    /// The weight of the nodes it holds: what each part of `roots` is ranked by
    /// ([`CauseCounts::weight`]).
    #[serde(default)]
    pub weight: usize,
    /// The oldest `stale_since` among its nodes (RFC 3339, UTC): since when the first of
    /// them has been out of date. `null` when none has one recorded.
    #[serde(default)]
    pub stale_since: Option<String>,
    /// The latest `expected_fresh_at` among its nodes: when the ones with an estimate are
    /// all expected fresh. `null` when none has one (`unbounded` is all of them).
    #[serde(default)]
    pub fresh_by: Option<String>,
    /// Whether it needs someone: its action is neither `wait` nor `run_by_hand`
    /// ([`ActionKind::needs_someone`]). `roots` lists the ones that do first.
    #[serde(default)]
    pub needs_someone: bool,
    /// What clears it.
    pub action: RootAction,
}

/// One source file's nodes, summed: every node of the file in scope, in whichever group it
/// sits. The tree has a `file` entry only for the nodes of a file that are in no group, so a
/// surface that lists files (the web UI's sidebar) reads these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct FileRollup {
    /// The source file, as node ids name it.
    pub file: String,
    /// Its nodes in scope.
    pub nodes: usize,
    /// Of them, assets.
    pub assets: usize,
    /// Of them, nodes a run is working on now.
    pub running: usize,
    /// Its nodes by health.
    pub health: HealthCounts,
    /// Its assets by health.
    pub asset_health: HealthCounts,
    /// The most severe health among its nodes.
    pub worst: Health,
    /// Its nodes that are not fresh, by cause: the first with a node, in the served order of
    /// the causes, is the one word and colour a line about the file shows.
    #[serde(default)]
    pub causes: CauseCounts,
    /// Its nodes that are fresh: no cause. With `causes` they add up to `nodes`.
    #[serde(default)]
    pub fresh: usize,
    /// Its nodes by kind: how many of each are fresh and how many have each cause.
    #[serde(default)]
    pub kinds: KindRollup,
}

/// `GET /state/rollup` and `barca status --rollup --json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Rollup {
    /// When the rows were classified: RFC 3339 UTC.
    pub as_of: String,
    /// Every entry, the root first, then depth first in `children` order.
    pub entries: Vec<RollupEntry>,
    /// What holds the nodes that are not fresh: the roots that need someone first, then the
    /// ones in hand; in each part the most weight held first, a failure first among equals,
    /// then nodes held, gap held and id.
    pub roots: Vec<RollupRoot>,
    /// The root entry's data coverage.
    pub data_coverage: DataCoverage,
    /// Each source file in scope with its nodes summed, by file path.
    #[serde(default)]
    pub files: Vec<FileRollup>,
    /// Every node in scope by kind (asset, task, sensor): how many of each are fresh and how
    /// many have each cause. What a list of one kind counts its filters with.
    #[serde(default)]
    pub kinds: KindRollup,
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
/// `unbounded` and `fresh_by` are read off the row's times (#962), `causes` and
/// `gap_seconds` off the row's own (#963).
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
    if let Some(cause) = node.cause {
        *entry.causes.slot(cause) = 1;
        if asset {
            *entry.asset_causes.slot(cause) = 1;
        }
    }
    entry.not_fresh = usize::from(node.cause.is_some());
    entry.weigh();
    entry.stale_since.clone_from(&node.stale_since);
    entry.gap_seconds = node.gap_seconds;
    entry.unbounded = usize::from(!node.health.is_fresh() && node.expected_fresh_at.is_none());
    entry.fresh_by.clone_from(&node.expected_fresh_at);
    entry.keys = node
        .partitions
        .as_ref()
        .filter(|p| node.partitioned && p.total > 0)
        .map(|p| KeyCounts {
            failed: p.failed,
            behind: p.total.saturating_sub(p.cached).saturating_sub(p.failed),
            ok: p.cached,
            cause: node.cause,
        });
    entry
}

/// The group each node sits under in the rollup: the first group, by group id, that declares
/// it. The same rule [`build`] places a node by, for a caller that has no status rows (a run
/// in progress counting its nodes by group, [`crate::run_list::run_now`]).
pub fn node_groups(groups: &[NodeGroup]) -> HashMap<&str, &NodeGroup> {
    let ids: HashSet<&str> = groups.iter().map(|g| g.id.as_str()).collect();
    let by_id: BTreeMap<&str, &NodeGroup> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut out = HashMap::new();
    for group in by_id.values() {
        for member in &group.members {
            if !ids.contains(member.as_str()) {
                out.entry(member.as_str()).or_insert(*group);
            }
        }
    }
    out
}

/// Each source file of `nodes`, summed, by file path.
fn files(nodes: &[NodeStatus], running: &HashSet<String>) -> Vec<FileRollup> {
    let mut out: BTreeMap<&str, FileRollup> = BTreeMap::new();
    for node in nodes {
        let file = source_file(&node.id);
        let sums = out.entry(file).or_insert_with(|| FileRollup {
            file: file.to_string(),
            nodes: 0,
            assets: 0,
            running: 0,
            health: HealthCounts::default(),
            asset_health: HealthCounts::default(),
            worst: Health::Ok,
            causes: CauseCounts::default(),
            fresh: 0,
            kinds: KindRollup::default(),
        });
        sums.kinds.count(node);
        match node.cause {
            Some(cause) => *sums.causes.slot(cause) += 1,
            None => sums.fresh += 1,
        }
        sums.nodes += 1;
        sums.running += usize::from(running.contains(&node.id));
        *sums.health.slot(node.health) += 1;
        if node.kind == "asset" {
            sums.assets += 1;
            *sums.asset_health.slot(node.health) += 1;
        }
    }
    out.into_values()
        .map(|mut f| {
            f.worst = f.health.worst().unwrap_or(Health::Ok);
            f
        })
        .collect()
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
///
/// `evidence` is what the rows were classified with: the root list reads the schedules, the
/// typical durations and `max_runs` from it. `server`: a server answers, so a root's command
/// names `--server`.
pub fn build(
    nodes: &[NodeStatus],
    groups: &[NodeGroup],
    running: &HashSet<String>,
    as_of: chrono::DateTime<chrono::Utc>,
    evidence: Evidence<'_>,
    server: bool,
) -> Rollup {
    let mut entries: HashMap<String, RollupEntry> = nodes
        .iter()
        .map(|n| (n.id.clone(), leaf(n, running.contains(&n.id))))
        .collect();
    let by_id: BTreeMap<&str, &NodeGroup> = groups.iter().map(|g| (g.id.as_str(), g)).collect();

    // Parents: groups in id order claim their members, so the first by id wins.
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut also_in: HashMap<String, Vec<String>> = HashMap::new();
    for (id, group) in &by_id {
        for member in &group.members {
            let known = entries.contains_key(member) || by_id.contains_key(member.as_str());
            if !known || is_above(&parent, member, id) {
                continue;
            }
            match parent.get(member) {
                // Declared again: it stays under the first group and names this one.
                Some(first) if first != id => {
                    let also = also_in.entry(member.clone()).or_default();
                    if !also.iter().any(|g| g == id) {
                        also.push((*id).to_string());
                    }
                }
                Some(_) => {}
                None => {
                    parent.insert(member.clone(), (*id).to_string());
                }
            }
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
            entry.also_in = also_in.remove(&id).unwrap_or_default();
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
    // A group left out as empty is not named as another place a node is declared.
    let kept: HashSet<String> = ordered.iter().map(|e| e.id.clone()).collect();
    for entry in &mut ordered {
        entry.also_in.retain(|g| kept.contains(g));
    }
    // What each root holds under every scope: a node that is not fresh is counted under
    // each entry above it, so a level's roots are the exact sum of its children's.
    let above: HashMap<String, String> = ordered
        .iter()
        .filter_map(|e| Some((e.id.clone(), e.parent.clone()?)))
        .collect();
    let mut under: HashMap<&str, Vec<&NodeStatus>> = HashMap::new();
    for node in nodes.iter().filter(|n| n.root.is_some()) {
        // Its own entry first (for its `split`), then every entry above it.
        let mut at = Some(&node.id);
        while let Some(id) = at {
            under.entry(id.as_str()).or_default().push(node);
            at = above.get(id);
        }
    }
    let roots = roots(nodes, evidence, server);
    let actions = root_actions(&roots);
    for entry in &mut ordered {
        if let Some(rows) = under.remove(entry.id.as_str()) {
            entry.split = HeldSplit::of(rows.iter().copied(), &actions);
            // A node's root is on its row: its entry lists none.
            if entry.kind != EntryKind::Node {
                entry.held_by = held_by(rows, &actions);
            }
        }
    }
    Rollup {
        as_of: as_of.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        entries: ordered,
        roots,
        data_coverage,
        files: files(nodes, running),
        kinds: KindRollup::of(nodes),
    }
}

/// The root entry of `nodes` alone: every count of the scope, without the tree. What
/// `barca status --json` carries as `rollup`; `running` and `evidence` as in [`build`].
pub fn total(
    nodes: &[NodeStatus],
    running: &HashSet<String>,
    actions: &HashMap<String, ActionKind>,
) -> RollupEntry {
    let mut root = RollupEntry::inner(ROOT_ID.to_string(), EntryKind::Root, "all".to_string());
    for node in nodes {
        root.add(&leaf(node, running.contains(&node.id)));
    }
    root.split = HeldSplit::of(nodes, actions);
    root.held_by = held_by(nodes, actions);
    root
}

/// The action for each root id in the classified scope.
pub(crate) fn root_actions(roots: &[RollupRoot]) -> HashMap<String, ActionKind> {
    roots
        .iter()
        .map(|r| (r.id.clone(), r.action.kind))
        .collect()
}

/// What a root list is ranked by: whether the root needs someone, what it holds, and whether
/// it is a failure.
struct Rank<'a> {
    /// The scheduler root: nothing it holds clears until a scheduler runs, whatever else
    /// is wrong, so it comes before every other root.
    scheduler: bool,
    needs: bool,
    weight: usize,
    failure: bool,
    nodes: usize,
    gap: i64,
    id: &'a str,
}

/// The order of a root list: the roots that need someone before the ones in hand (a root
/// that only waits is nothing to do, however much it holds); in each part the most weight
/// held first (how far what the root holds is from fresh), a failure first among equals,
/// then the most nodes, the largest gap, and id.
fn rank(a: Rank<'_>, b: Rank<'_>) -> std::cmp::Ordering {
    b.scheduler
        .cmp(&a.scheduler)
        .then(b.needs.cmp(&a.needs))
        .then(b.weight.cmp(&a.weight))
        .then(b.failure.cmp(&a.failure))
        .then(b.nodes.cmp(&a.nodes))
        .then(b.gap.cmp(&a.gap))
        .then_with(|| a.id.cmp(b.id))
}

/// Whether a root id (`<kind>:<node id>`) is a failure's.
fn is_failure(root: &str) -> bool {
    root.starts_with("failure:")
}

/// What each root holds among `rows` (one scope's nodes), in the root list's order. `needs`:
/// the roots that need someone ([`root_actions`]).
fn held_by<'a>(
    rows: impl IntoIterator<Item = &'a NodeStatus>,
    actions: &HashMap<String, ActionKind>,
) -> Vec<HeldBy> {
    let mut held: BTreeMap<&str, HeldBy> = BTreeMap::new();
    for node in rows {
        let Some(root) = &node.root else { continue };
        let entry = held.entry(root.id.as_str()).or_insert_with(|| HeldBy {
            root: root.id.clone(),
            nodes: 0,
            assets: 0,
            causes: CauseCounts::default(),
            gap_seconds: 0,
            unbounded: 0,
            weight: 0,
            cumulative_nodes: 0,
            action_kind: actions[&root.id],
            stale_since: None,
            fresh_by: None,
        });
        entry.nodes += 1;
        entry.assets += usize::from(node.kind == "asset");
        if let Some(cause) = node.cause {
            *entry.causes.slot(cause) += 1;
        }
        entry.gap_seconds += node.gap_seconds;
        entry.unbounded += usize::from(node.expected_fresh_at.is_none());
        oldest(&mut entry.stale_since, node.stale_since.as_deref());
        latest(&mut entry.fresh_by, node.expected_fresh_at.as_deref());
    }
    let mut out: Vec<HeldBy> = held.into_values().collect();
    for entry in &mut out {
        entry.weight = entry.causes.weight();
    }
    fn key(h: &HeldBy) -> Rank<'_> {
        Rank {
            scheduler: h.root == RootKind::SchedulerOff.as_str(),
            needs: h.action_kind.needs_someone(),
            weight: h.weight,
            failure: is_failure(&h.root),
            nodes: h.nodes,
            gap: h.gap_seconds,
            id: &h.root,
        }
    }
    out.sort_by(|a, b| rank(key(a), key(b)));
    let mut so_far = 0;
    for entry in &mut out {
        so_far += entry.nodes;
        entry.cumulative_nodes = so_far;
    }
    out
}

/// A node as a `barca` command names it: its declared name, or its id when another node in
/// scope has the same name.
fn target<'a>(node: &'a NodeStatus, names: &HashMap<&str, usize>) -> &'a str {
    if names.get(node.name.as_str()).copied().unwrap_or(0) > 1 {
        &node.id
    } else {
        &node.name
    }
}

/// The roots of `nodes`: each `root` the rows name, with what it holds and what clears it.
/// The roots that need someone come first, then the ones in hand; in each part the most
/// weight held first, a failure first among equals; then nodes held, gap held and id.
/// `evidence` and `server` as in [`build`].
pub fn roots(nodes: &[NodeStatus], evidence: Evidence<'_>, server: bool) -> Vec<RollupRoot> {
    struct Held<'a> {
        root: RollupRoot,
        rows: Vec<&'a NodeStatus>,
    }
    let by_id: HashMap<&str, &NodeStatus> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut held: BTreeMap<&str, Held<'_>> = BTreeMap::new();
    for node in nodes {
        let Some(root) = &node.root else { continue };
        let entry = held.entry(root.id.as_str()).or_insert_with(|| Held {
            root: RollupRoot {
                id: root.id.clone(),
                kind: root.kind,
                node: root.node.clone(),
                detail: String::new(),
                error_group: None,
                nodes: 0,
                assets: 0,
                causes: CauseCounts::default(),
                gap_seconds: 0,
                unbounded: 0,
                weight: 0,
                stale_since: None,
                fresh_by: None,
                needs_someone: false,
                action: RootAction {
                    kind: ActionKind::Wait,
                    command: None,
                    estimated_seconds: None,
                    run_once: None,
                },
            },
            rows: Vec::new(),
        });
        entry.root.nodes += 1;
        entry.root.assets += usize::from(node.kind == "asset");
        if let Some(cause) = node.cause {
            *entry.root.causes.slot(cause) += 1;
        }
        entry.root.gap_seconds += node.gap_seconds;
        entry.root.unbounded += usize::from(node.expected_fresh_at.is_none());
        oldest(&mut entry.root.stale_since, node.stale_since.as_deref());
        latest(&mut entry.root.fresh_by, node.expected_fresh_at.as_deref());
        entry.rows.push(node);
    }
    if held.is_empty() {
        return Vec::new();
    }
    let mut names: HashMap<&str, usize> = HashMap::new();
    for node in nodes {
        *names.entry(node.name.as_str()).or_default() += 1;
    }
    let durations = evidence.timing.durations;
    let default = crate::estimates::project_median(durations);
    let via = if server { " --server" } else { "" };
    let mut out: Vec<RollupRoot> = held
        .into_values()
        .map(|Held { mut root, rows }| {
            let about = by_id.get(root.node.as_str()).copied();
            let name = about.map_or_else(
                || barca_compile::targets::short_name(&root.node).to_string(),
                |n| target(n, &names).to_string(),
            );
            let schedule = evidence.path.schedules.get(&root.node);
            let (detail, error_group) = detail(&root, &name, about, schedule, evidence);
            let name = barca_store::text::shell_quote(&name);
            root.detail = detail;
            root.error_group = error_group;
            root.weight = root.causes.weight();
            // The command that computes everything the root holds: the held nodes nothing
            // else held reads (a run computes what is above its targets). Tasks are run,
            // never got.
            let run = |refresh: &str| {
                let ids: HashSet<&str> = rows.iter().map(|n| n.id.as_str()).collect();
                let read: HashSet<&str> = rows
                    .iter()
                    .flat_map(|n| n.inputs.iter().map(String::as_str))
                    .filter(|input| ids.contains(input))
                    .collect();
                let mut assets = Vec::new();
                let mut tasks = Vec::new();
                for node in rows.iter().filter(|n| !read.contains(n.id.as_str())) {
                    let targets = if node.kind == "task" {
                        &mut tasks
                    } else {
                        &mut assets
                    };
                    targets.push(target(node, &names));
                }
                let mut commands = Vec::new();
                for (verb, mut targets) in [("get", assets), ("run", tasks)] {
                    if !targets.is_empty() {
                        targets.sort_unstable();
                        commands.push(format!(
                            "barca {verb} {}{refresh}{via}",
                            barca_store::text::shell_quote(&targets.join(","))
                        ));
                    }
                }
                commands.join(" && ")
            };
            let overdue = root.causes.overdue > 0;
            let (kind, command) = match root.kind {
                RootKind::Failure => (
                    ActionKind::FixFailure,
                    Some(match about.map(|n| n.kind.as_str()) {
                        Some("task") => format!("barca run {name}{via}"),
                        _ => format!("barca get {name}{via}"),
                    }),
                ),
                RootKind::Queue => (
                    if schedule
                        .and_then(|s| s.standing.as_ref())
                        .is_some_and(|st| st.queue_bound)
                    {
                        ActionKind::RaiseMaxRuns
                    } else {
                        ActionKind::Wait
                    },
                    None,
                ),
                // One action whatever it holds: nothing under it moves until a scheduler runs.
                RootKind::SchedulerOff => {
                    (ActionKind::StartServer, Some("barca serve".to_string()))
                }
                // A paused node is resumed by name, a paused schedule by its scheduled node;
                // both pauses live on the server.
                RootKind::SchedulePaused => (
                    ActionKind::ResumeSchedule,
                    Some(if evidence.path.paused.contains(&root.node) {
                        format!("barca resume {name} --server")
                    } else {
                        format!("barca resume --schedule {name} --server")
                    }),
                ),
                RootKind::NoSchedule if evidence.path.pinned.contains(&root.node) => {
                    (ActionKind::Run, Some(format!("barca unpin {name}{via}")))
                }
                // Run by hand by declaration: nobody is asked to act. A run reads a `Manual`
                // node as it is unless it is named in `--refresh`, which also recomputes
                // what is below it; a task no schedule reaches is run with `barca run`.
                RootKind::NoSchedule if evidence.path.paths.is_manual(&root.node) => (
                    ActionKind::RunByHand,
                    Some(run(&format!(" --refresh {name}"))),
                ),
                RootKind::NoSchedule if about.is_some_and(|n| n.kind == "task") => {
                    (ActionKind::RunByHand, Some(run("")))
                }
                RootKind::NoSchedule => (ActionKind::AddSchedule, None),
                _ if overdue => (ActionKind::Run, Some(run(""))),
                _ => (ActionKind::Wait, None),
            };
            let sampled = rows.iter().any(|n| durations.contains_key(&n.id));
            root.needs_someone = kind.needs_someone();
            root.action = RootAction {
                kind,
                // What a missing schedule holds can still be computed once, by hand.
                run_once: (kind == ActionKind::AddSchedule).then(|| run("")),
                command,
                estimated_seconds: sampled.then(|| {
                    rows.iter()
                        .map(|n| crate::estimates::work(n, durations, default))
                        .sum::<f64>()
                        .ceil() as u64
                }),
            };
            root
        })
        .collect();
    fn key(r: &RollupRoot) -> Rank<'_> {
        Rank {
            scheduler: r.id == RootKind::SchedulerOff.as_str(),
            needs: r.needs_someone,
            weight: r.weight,
            failure: r.kind == RootKind::Failure,
            nodes: r.nodes,
            gap: r.gap_seconds,
            id: &r.id,
        }
    }
    out.sort_by(|a, b| rank(key(a), key(b)));
    out
}

/// A root's fact in a line, and a failure's error group. `name` is the root's node as a
/// command names it, `about` its row, `schedule` its schedule when it is a scheduled node.
fn detail(
    root: &RollupRoot,
    name: &str,
    about: Option<&NodeStatus>,
    schedule: Option<&crate::refresh_path::PathSchedule>,
    evidence: Evidence<'_>,
) -> (String, Option<String>) {
    use barca_store::text::span;
    let standing = schedule.and_then(|s| s.standing.as_ref());
    let line = match root.kind {
        RootKind::Failure => {
            return match about.filter(|n| last_attempt_failed(n)) {
                Some(node) => failure_detail(node),
                // A schedule held after `max_failures` failed ticks.
                None => (
                    format!("{name}'s schedule is held after repeated failed ticks"),
                    None,
                ),
            };
        }
        RootKind::SensorValue => format!("{name} observed a new value"),
        RootKind::Code => format!("the code of {name} changed"),
        RootKind::Env => format!("an environment variable that {name} declares changed"),
        RootKind::Input => match about.is_some_and(|n| n.health.is_fresh()) {
            true => format!("{name} has a newer result than what reads it"),
            false => format!("{name} is out of date against what it reads"),
        },
        RootKind::NeverRun => match about.is_some_and(|n| n.health == Health::NeverRun) {
            true => format!("{name} has never run"),
            false => format!("{name} has keys that never ran"),
        },
        RootKind::NoSchedule if evidence.path.pinned.contains(&root.node) => {
            format!("{name} is pinned: every run reads its pinned result")
        }
        RootKind::NoSchedule if evidence.path.paths.is_manual(&root.node) => {
            match about.is_some_and(|n| n.health == Health::NeverRun) {
                true => format!("{name} is Manual and has not run yet: no tick computes it"),
                false => format!("{name} is Manual: no tick computes it"),
            }
        }
        RootKind::NoSchedule if about.is_some_and(|n| n.kind == "task") => {
            match about.is_some_and(|n| n.health == Health::NeverRun) {
                true => format!(
                    "{name} is a task with no schedule and has not run yet: it runs when someone runs it"
                ),
                false => format!("{name} is a task with no schedule: it runs when someone runs it"),
            }
        }
        RootKind::NoSchedule => format!("no tick reaches {name}"),
        RootKind::SchedulePaused if evidence.path.paused.contains(&root.node) => {
            format!("{name} is paused: every tick skips it")
        }
        RootKind::SchedulePaused => format!("{name}'s schedule is paused"),
        RootKind::SchedulerOff => scheduler_detail(evidence),
        RootKind::Queue => {
            let slots = match evidence.timing.max_runs {
                Some(n) => format!("max_runs is {n}"),
                None => "max_runs limits the runs at once".to_string(),
            };
            match standing {
                Some(st) if st.queue_bound => format!(
                    "{name}'s ticks wait about {} for a run slot, every {}: {slots}",
                    span(st.typical_queue_wait_seconds.ceil() as i64),
                    span(st.interval_seconds.unwrap_or(0)),
                ),
                _ => format!("a tick of {name} is waiting for a run slot: {slots}"),
            }
        }
        RootKind::Undecided => format!("barca cannot decide {name} until a run reports"),
    };
    (line, None)
}

/// The scheduler root's fact: that no scheduler is running and since when, how many
/// schedules that leaves unfired and how many of them have already missed a slot. Where a
/// scheduler runs and does not serve some schedules, those.
fn scheduler_detail(evidence: Evidence<'_>) -> String {
    use barca_store::text::span;
    let schedules = evidence.path.schedules;
    let off: Vec<(&String, &crate::refresh_path::PathSchedule)> =
        schedules.iter().filter(|(_, s)| s.off()).collect();
    let count = |n: usize| format!("{n} schedule{}", if n == 1 { "" } else { "s" });
    let missed = off.iter().filter(|(_, s)| s.unserved()).count();
    let unfired = match missed {
        0 => format!("{} will not fire", count(off.len())),
        n if n == off.len() => format!("{} missed a slot", count(n)),
        n => format!("{} will not fire, {n} missed a slot", count(off.len())),
    };
    // A scheduler fires other schedules: these are ones it does not serve.
    if schedules.values().any(|s| s.live) {
        let mut names: Vec<&str> = off
            .iter()
            .map(|(id, _)| barca_compile::targets::short_name(id))
            .collect();
        names.sort_unstable();
        let more = names.len().saturating_sub(3);
        let mut list = names.into_iter().take(3).collect::<Vec<_>>().join(", ");
        if more > 0 {
            list.push_str(&format!(" and {more} more"));
        }
        return format!("the running scheduler does not fire {list}: {unfired}");
    }
    match off.iter().filter_map(|(_, s)| s.off_since).max() {
        Some(since) => format!(
            "the scheduler is not running: no sign of it for {}, {unfired}",
            span(evidence.now - since)
        ),
        None => format!("no scheduler has run for this project: {unfired}"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{Checks, Evidence, Root, classify};
    use crate::status::{LastMaterialization, PartitionState};
    use crate::views::Cause;
    use barca_model::cache_state::CacheState;

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
                checks: &Checks::default(),
                path: crate::refresh_path::PathEvidence::none(),
                timing: crate::estimates::Timing::none(),
                now: 0,
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
            // A cause exactly for the nodes that are not fresh (INV-HEALTH-01).
            assert_eq!(
                e.causes.total(),
                e.nodes - e.health.ok - e.health.suspect,
                "{}: causes count the nodes that are not fresh",
                e.id
            );
            assert_eq!(
                e.asset_causes.total(),
                e.assets - e.asset_health.ok - e.asset_health.suspect,
                "{}",
                e.id
            );
            assert_eq!(e.asset_health.total(), e.assets, "{}", e.id);
            assert_eq!(e.data_coverage.assets, e.assets, "{}", e.id);
            assert_eq!(e.not_fresh, e.causes.total(), "{}", e.id);
            // The weight is one per fresh node and each cause's weight per node that is
            // not fresh (#1028), and what the roots hold here weighs the rest.
            assert_eq!(e.weight, e.weights.total(), "{}", e.id);
            assert_eq!(
                e.weight,
                (e.nodes - e.not_fresh) * FRESH_WEIGHT + e.causes.weight(),
                "{}",
                e.id
            );
            if e.kind != EntryKind::Node {
                assert_eq!(
                    e.held_by.iter().map(|h| h.weight).sum::<usize>(),
                    e.causes.weight(),
                    "{}",
                    e.id
                );
                assert!(
                    e.held_by.windows(2).all(|w| w[0].weight >= w[1].weight),
                    "{}: held_by is ranked by weight",
                    e.id
                );
            }
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
            assert_eq!((sum.causes, sum.asset_causes), (e.causes, e.asset_causes));
            assert_eq!(sum.not_fresh, e.not_fresh, "{}", e.id);
            assert_eq!((sum.weight, sum.weights), (e.weight, e.weights), "{}", e.id);
            // The roots of a scope: what each holds adds up to the scope's not-fresh nodes,
            // gap and unbounded nodes, and to what its children's roots hold.
            assert_eq!(
                e.held_by.iter().map(|h| h.nodes).sum::<usize>(),
                e.not_fresh,
                "{}",
                e.id
            );
            assert_eq!(
                e.held_by.iter().map(|h| h.gap_seconds).sum::<i64>(),
                e.gap_seconds,
                "{}",
                e.id
            );
            assert_eq!(
                e.held_by.iter().map(|h| h.unbounded).sum::<usize>(),
                e.unbounded,
                "{}",
                e.id
            );
            assert_eq!(
                e.held_by.last().map_or(0, |h| h.cumulative_nodes),
                e.not_fresh,
                "{}",
                e.id
            );
            let mut below: BTreeMap<&str, usize> = BTreeMap::new();
            for child in &e.children {
                let child = by_id[child.as_str()];
                for h in &child.held_by {
                    *below.entry(h.root.as_str()).or_default() += h.nodes;
                }
                if child.kind == EntryKind::Node {
                    assert!(
                        child.held_by.is_empty(),
                        "{}: a node lists no roots",
                        child.id
                    );
                    let row = nodes.iter().find(|n| n.id == child.id).expect("a row");
                    if let Some(root) = &row.root {
                        *below.entry(root.id.as_str()).or_default() += 1;
                    }
                }
            }
            let here: BTreeMap<&str, usize> = e
                .held_by
                .iter()
                .map(|h| (h.root.as_str(), h.nodes))
                .collect();
            assert_eq!(here, below, "{}: its roots are its children's", e.id);
            assert_eq!(
                (sum.gap_seconds, sum.unbounded),
                (e.gap_seconds, e.unbounded)
            );
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
        // The files are the same nodes cut another way: they add up to the root.
        let root = rollup.root();
        let mut of_files = RollupEntry::inner("files".into(), EntryKind::Root, String::new());
        for f in &rollup.files {
            of_files.nodes += f.nodes;
            of_files.assets += f.assets;
            of_files.running += f.running;
            of_files.health.add(&f.health);
            of_files.asset_health.add(&f.asset_health);
            assert_eq!(Some(f.worst), f.health.worst(), "{}", f.file);
            assert_eq!(f.causes.total() + f.fresh, f.nodes, "{}", f.file);
        }
        assert_eq!(
            (of_files.nodes, of_files.assets, of_files.running),
            (root.nodes, root.assets, root.running)
        );
        assert_eq!(
            (of_files.health, of_files.asset_health),
            (root.health, root.asset_health)
        );
    }

    #[test]
    fn a_node_declared_in_two_groups_is_counted_once_and_names_the_other() {
        let (nodes, mut groups) = fixture();
        let shared = nodes[0].id.clone();
        let first = rollup_parent(
            &build(
                &nodes,
                &groups,
                &HashSet::new(),
                now(),
                Evidence::none(0),
                false,
            ),
            &shared,
        );
        groups.push(NodeGroup {
            id: "group:zz.py:extra".into(),
            name: "extra".into(),
            description: String::new(),
            members: vec![shared.clone(), "q.py:lookup".to_string()],
            output: shared.clone(),
        });
        let rollup = build(
            &nodes,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
        assert_sums(&rollup, &nodes);
        let entry = rollup.entries.iter().find(|e| e.id == shared).unwrap();
        assert_eq!(first, "group:p.py:ingest");
        assert_eq!(entry.parent.as_deref(), Some(first.as_str()));
        assert_eq!(entry.also_in, vec!["group:zz.py:extra".to_string()]);
        // Every node in a group sits where `node_groups` says, the rule a run in progress
        // counts its nodes by.
        let by_node = node_groups(&groups);
        for e in rollup.entries.iter().filter(|e| e.kind == EntryKind::Node) {
            let parent = e.parent.as_deref().unwrap();
            match by_node.get(e.id.as_str()) {
                Some(group) => assert_eq!(parent, group.id, "{}", e.id),
                None => assert!(parent.starts_with("file:"), "{}", e.id),
            }
        }
    }

    fn rollup_parent(rollup: &Rollup, id: &str) -> String {
        rollup
            .entries
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.parent.clone())
            .unwrap()
    }

    #[test]
    fn every_level_is_the_sum_of_its_children_in_both_units() {
        let (nodes, groups) = fixture();
        let running = HashSet::from(["p.py:scored".to_string()]);
        let rollup = build(&nodes, &groups, &running, now(), Evidence::none(0), false);
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
                ok: 6,
                // Why its keys are behind: the failure upstream that blocks the node.
                cause: Some(Cause::Failing),
            })
        );
        // The same nodes by kind (#992): what a list of one kind counts its filters with.
        let kinds = rollup.kinds;
        assert_eq!(
            (kinds.asset.nodes, kinds.task.nodes, kinds.sensor.nodes),
            (4, 2, 1)
        );
        assert_eq!((kinds.asset.fresh, kinds.asset.causes.failing), (1, 3));
        // A sensor that never ran is not fresh: it has a cause like any node.
        assert_eq!(kinds.sensor.fresh + kinds.sensor.causes.total(), 1);
        for kind in [kinds.asset, kinds.task, kinds.sensor] {
            assert_eq!(kind.fresh + kind.causes.total(), kind.nodes);
        }
        assert_eq!(
            kinds.asset.nodes + kinds.task.nodes + kinds.sensor.nodes,
            rollup.root().nodes
        );
        // A file's are its own nodes', whichever group they sit in.
        let by_file: HashMap<&str, &FileRollup> =
            rollup.files.iter().map(|f| (f.file.as_str(), f)).collect();
        assert_eq!(
            (
                by_file["p.py"].kinds.asset.nodes,
                by_file["q.py"].kinds.asset.nodes
            ),
            (3, 1)
        );
        assert_eq!(by_file["q.py"].kinds.task.nodes, 1);
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
        let rollup = build(
            &nodes,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
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
        let rollup = build(
            &scoped,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
        assert_sums(&rollup, &scoped);
        assert_eq!(rollup.root().children, ["group:p.py:ingest"]);
        let empty = build(
            &[],
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
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
        let rollup = build(
            &nodes,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
        assert_sums(&rollup, &nodes);
        // A scope lists the roots that hold its nodes, wherever the root's own node is
        // (#967): `orders` fails in `ingest` and holds the two assets of `model`.
        let held = |id: &str| -> Vec<(String, usize, usize)> {
            entry(&rollup, id)
                .held_by
                .iter()
                .map(|h| (h.root.clone(), h.nodes, h.cumulative_nodes))
                .collect()
        };
        assert_eq!(
            held("group:p.py:model"),
            [("failure:p.py:orders".to_string(), 2, 2)]
        );
        assert_eq!(
            held("group:p.py:ingest"),
            [
                ("failure:p.py:orders".to_string(), 1, 1),
                ("no_schedule:p.py:inbox".to_string(), 1, 2)
            ]
        );
        assert_eq!(entry(&rollup, "group:p.py:model").not_fresh, 2);
        assert_eq!(
            entry(&rollup, ROOT_ID).held_by.first().map(|h| h.nodes),
            rollup.roots.first().map(|r| r.nodes)
        );
        // The root entry's list is the root list, in its order.
        assert_eq!(
            entry(&rollup, ROOT_ID)
                .held_by
                .iter()
                .map(|h| (h.root.as_str(), h.nodes, h.gap_seconds, h.unbounded))
                .collect::<Vec<_>>(),
            rollup
                .roots
                .iter()
                .map(|r| (r.id.as_str(), r.nodes, r.gap_seconds, r.unbounded))
                .collect::<Vec<_>>()
        );
        // The most nodes first, then by id. `orders` holds itself, `clean` and `scored`.
        let seen: Vec<_> = rollup
            .roots
            .iter()
            .filter(|r| r.kind == RootKind::Failure)
            .map(|r| {
                (
                    r.id.as_str(),
                    r.detail.as_str(),
                    r.error_group.as_deref(),
                    (r.nodes, r.assets),
                    r.action.kind,
                    r.action.command.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "failure:p.py:orders",
                    "ValueError: 12 rows rejected",
                    Some("ValueError: # rows rejected"),
                    (3, 3),
                    ActionKind::FixFailure,
                    Some("barca get orders"),
                ),
                (
                    "failure:q.py:audit",
                    "ValueError: 7 rows rejected",
                    Some("ValueError: # rows rejected"),
                    (1, 0),
                    ActionKind::FixFailure,
                    Some("barca run audit"),
                ),
                (
                    "failure:q.py:daily",
                    "3 of 8 keys failed",
                    None,
                    (1, 1),
                    ActionKind::FixFailure,
                    Some("barca get daily"),
                ),
            ]
        );
        // A failure has no estimate: every node it holds is unbounded, and its cause failing.
        assert!(
            rollup
                .roots
                .iter()
                .filter(|r| r.kind == RootKind::Failure)
                .all(|r| r.unbounded == r.nodes && r.causes.failing == r.nodes)
        );
        // Each row names the root that holds it, and the roots' nodes are exactly those rows.
        let held: usize = rollup.roots.iter().map(|r| r.nodes).sum();
        assert_eq!(held, nodes.iter().filter(|n| n.root.is_some()).count());
        assert_eq!(
            nodes.iter().find(|n| n.id == "p.py:scored").unwrap().root,
            Some(Root::failure("p.py:orders"))
        );
        let root = rollup.root();
        assert_eq!(held, root.causes.total());
        let failing: usize = rollup
            .roots
            .iter()
            .filter(|r| r.kind == RootKind::Failure)
            .map(|r| r.nodes)
            .sum();
        assert_eq!(failing, root.health.failing + root.health.blocked);
        assert_eq!(failing, root.causes.failing);
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
        let rollup = build(
            &nodes,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
        assert_eq!(
            rollup.root().health.line(),
            "1 failing · 2 blocked · 3 never run · 1 ok"
        );
        assert_eq!(rollup.root().health.worst(), Some(Health::Failing));
        assert_eq!(HealthCounts::default().line(), "");
    }

    #[test]
    fn fresh_by_is_the_latest_estimate_underneath_and_unbounded_counts_the_rest() {
        let (mut nodes, groups) = fixture();
        // Without estimates every node that is not fresh is unbounded, at every level.
        let rollup = build(
            &nodes,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
        let not_fresh = nodes.iter().filter(|n| !n.health.is_fresh()).count();
        assert!(not_fresh >= 2, "the fixture has nodes that are not fresh");
        assert_eq!(rollup.root().unbounded, not_fresh);
        assert_eq!(rollup.root().fresh_by, None);
        // Two of them get an estimate: the root carries the later one, and counts them out.
        let mut times = ["2026-11-17T15:00:20Z", "2026-11-17T15:04:05Z"].into_iter();
        for node in nodes.iter_mut().filter(|n| !n.health.is_fresh()).take(2) {
            node.expected_fresh_at = times.next().map(str::to_string);
        }
        let rollup = build(
            &nodes,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
        assert_eq!(rollup.root().unbounded, not_fresh - 2);
        assert_eq!(
            rollup.root().fresh_by.as_deref(),
            Some("2026-11-17T15:04:05Z")
        );
        // Every level is the sum, and the latest, of its children.
        let by_id: HashMap<&str, &RollupEntry> =
            rollup.entries.iter().map(|e| (e.id.as_str(), e)).collect();
        for entry in rollup.entries.iter().filter(|e| !e.children.is_empty()) {
            let children = entry.children.iter().map(|id| by_id[id.as_str()]);
            assert_eq!(
                entry.unbounded,
                children.clone().map(|c| c.unbounded).sum::<usize>(),
                "{}",
                entry.id
            );
            assert_eq!(
                entry.fresh_by,
                children.filter_map(|c| c.fresh_by.clone()).max(),
                "{}",
                entry.id
            );
        }
    }

    #[test]
    fn the_wire_names_are_the_specified_ones() {
        let (nodes, groups) = fixture();
        let rollup = build(
            &nodes,
            &groups,
            &HashSet::new(),
            now(),
            Evidence::none(0),
            false,
        );
        let json = serde_json::to_value(&rollup).unwrap();
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(
            keys(&json),
            [
                "as_of",
                "data_coverage",
                "entries",
                "files",
                "kinds",
                "roots"
            ]
        );
        assert_eq!(
            keys(&json["files"][0]),
            [
                "asset_health",
                "assets",
                "causes",
                "file",
                "fresh",
                "health",
                "kinds",
                "nodes",
                "running",
                "worst"
            ]
        );
        assert_eq!(
            keys(&json["entries"][0]),
            [
                "also_in",
                "asset_causes",
                "asset_health",
                "assets",
                "causes",
                "children",
                "data_coverage",
                "fresh_by",
                "gap_seconds",
                "health",
                "held_by",
                "id",
                "kind",
                "label",
                "nodes",
                "not_fresh",
                "parent",
                "running",
                "split",
                "stale_since",
                "unbounded",
                "weight",
                "weights"
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
            serde_json::json!({"failed": 0, "behind": 4, "ok": 6, "cause": "failing"})
        );
        assert_eq!(scored["kind"], "node");
        assert_eq!(
            keys(&json["roots"][0]),
            [
                "action",
                "assets",
                "causes",
                "detail",
                "error_group",
                "fresh_by",
                "gap_seconds",
                "id",
                "kind",
                "needs_someone",
                "node",
                "nodes",
                "stale_since",
                "unbounded",
                "weight"
            ]
        );
        assert_eq!(
            keys(&json["entries"][0]["weights"]),
            [
                "catching_up",
                "failing",
                "fresh",
                "no_refresh_path",
                "overdue",
                "queued",
                "undecided"
            ]
        );
        assert_eq!(json["roots"][0]["kind"], "failure");
        assert_eq!(
            json["roots"][0]["action"],
            serde_json::json!(
                {"kind": "fix_failure", "command": "barca get orders", "run_once": null, "estimated_seconds": null}
            )
        );
        assert_eq!(
            keys(&json["entries"][0]["causes"]),
            [
                "catching_up",
                "failing",
                "no_refresh_path",
                "overdue",
                "queued",
                "undecided"
            ]
        );
        assert_eq!(
            keys(&json["entries"][0]["asset_causes"]),
            keys(&json["entries"][0]["causes"])
        );
        // Every kind and action is spelled as the specification names it.
        for (kind, name) in [
            (RootKind::SensorValue, "sensor_value"),
            (RootKind::Code, "code"),
            (RootKind::Env, "env"),
            (RootKind::Input, "input"),
            (RootKind::NeverRun, "never_run"),
            (RootKind::NoSchedule, "no_schedule"),
            (RootKind::SchedulePaused, "schedule_paused"),
            (RootKind::SchedulerOff, "scheduler_off"),
            (RootKind::Failure, "failure"),
            (RootKind::Queue, "queue"),
            (RootKind::Undecided, "undecided"),
        ] {
            assert_eq!(serde_json::json!(kind), name);
            assert_eq!(kind.as_str(), name);
        }
        for (action, name) in [
            (ActionKind::Wait, "wait"),
            (ActionKind::Run, "run"),
            (ActionKind::AddSchedule, "add_schedule"),
            (ActionKind::ResumeSchedule, "resume_schedule"),
            (ActionKind::StartServer, "start_server"),
            (ActionKind::FixFailure, "fix_failure"),
            (ActionKind::RaiseMaxRuns, "raise_max_runs"),
        ] {
            assert_eq!(serde_json::json!(action), name);
            assert_eq!(action.as_str(), name);
        }
        for cause in Cause::ALL {
            assert_eq!(serde_json::json!(cause), cause.as_str());
        }
        let back: Rollup = serde_json::from_value(json).unwrap();
        assert_eq!(back, rollup);
    }

    /// #1028: the order of a root list is the weight held; among roots that hold the same
    /// weight a failure comes first, then the most nodes, the largest gap and the id.
    #[test]
    fn roots_rank_by_weight_then_failures_first() {
        let r = |weight, failure, nodes, gap, id| Rank {
            scheduler: false,
            needs: true,
            weight,
            failure,
            nodes,
            gap,
            id,
        };
        let mut list = [
            r(12, false, 6, 0, "queue:a"),
            r(12, false, 2, 900, "no_schedule:b"),
            r(12, true, 2, 0, "failure:c"),
            r(90, false, 15, 0, "no_schedule:d"),
            r(12, false, 2, 900, "code:e"),
            r(12, false, 2, 60, "input:f"),
        ];
        list.sort_by(|a, b| {
            rank(
                r(a.weight, a.failure, a.nodes, a.gap, a.id),
                r(b.weight, b.failure, b.nodes, b.gap, b.id),
            )
        });
        assert_eq!(
            list.iter().map(|r| r.id).collect::<Vec<_>>(),
            [
                "no_schedule:d",
                "failure:c",
                "queue:a",
                "code:e",
                "no_schedule:b",
                "input:f"
            ]
        );
        // A node's weight is its cause's, a fresh node's is 1.
        assert_eq!(
            Cause::ALL.map(Cause::weight),
            [6, 6, 6, 2, 2, 2],
            "failing, overdue, no refresh path; queued, undecided, catching up"
        );
        assert_eq!(FRESH_WEIGHT, 1);
    }
}
