//! Which files to evict, and in what order: the one pure function behind the size limit.
//!
//! [`plan`] takes what is on disk (materialization rows with their files, leftovers), what
//! must not be touched ([`Protections`]) and the budget ([`Goal`]), and returns the ordered
//! eviction list. `barca gc --dry-run` prints the list, `barca gc` and the check after every
//! command delete it, so a prediction cannot drift from what is deleted.
//!
//! The order (RFC-0012):
//!
//! 1. **Leftovers**: staging directories of dead processes, `*.moved-aside*` directories,
//!    `sql-cache/` copies (least recently used first). Nothing reads them.
//! 2. **Superseded results**: a file none of whose rows is the newest successful result of
//!    its node (node id including the partition key). Oldest first.
//! 3. **Local copies the store holds**: a current result whose upload to the store was
//!    confirmed. It is fetched again when read. Least recently read first.
//! 4. **Current results**: recomputed when read. Least recently read first.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// The eviction tiers, in the order they are evicted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Leftovers = 1,
    Superseded = 2,
    StoreHeld = 3,
    Current = 4,
}

impl Tier {
    pub const ALL: [Tier; 4] = [
        Tier::Leftovers,
        Tier::Superseded,
        Tier::StoreHeld,
        Tier::Current,
    ];

    /// The tier's name in JSON (`leftovers`, `superseded`, `store_held`, `current`).
    pub fn code(self) -> &'static str {
        match self {
            Tier::Leftovers => "leftovers",
            Tier::Superseded => "superseded",
            Tier::StoreHeld => "store_held",
            Tier::Current => "current",
        }
    }

    /// The tier in words, plural, for the stderr line and `barca gc`.
    pub fn words(self) -> &'static str {
        match self {
            Tier::Leftovers => "leftovers",
            Tier::Superseded => "superseded results",
            Tier::StoreHeld => "local copies held by the store",
            Tier::Current => "least recently read results",
        }
    }
}

/// What kind of leftover a tier-1 entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverKind {
    /// `.barca/staging/<pid>/` of a process that is not running.
    Staging,
    /// A `*.moved-aside*` directory beside an artifact path.
    MovedAside,
    /// A downloaded copy in `.barca/sql-cache/`, with its `.version` stamp.
    SqlCache,
}

/// One successful materialization row whose artifact has a file on this machine.
#[derive(Debug, Clone)]
pub struct Row {
    /// Which environment's metadata DB the row is in.
    pub env: String,
    /// The row id: a later row has a larger id.
    pub id: i64,
    /// Exact node id, with the partition key of a partitioned step.
    pub node_id: String,
    pub run_hash: Option<String>,
    /// The artifact path as recorded (a local path or a store URI).
    pub recorded: String,
    /// The file on this disk that is deleted to evict it: the path itself, or the local
    /// mirror of a store URI. `None` when the row has no file barca may delete here (the
    /// artifact is outside `.barca/`, or the row has no artifact).
    pub file: Option<PathBuf>,
    /// The file's size on disk; 0 when it is not there.
    pub bytes: u64,
    /// Whether the file is on disk now.
    pub present: bool,
    /// `created_at` (UTC, `YYYY-MM-DD HH:MM:SS`).
    pub created_at: String,
    /// `last_read_at`, when it was ever read.
    pub last_read_at: Option<String>,
    /// The artifact is in the artifact store, its upload confirmed with a digest: deleting the
    /// local copy costs a fetch, not a recompute.
    pub store_held: bool,
}

impl Row {
    /// When this result was last used: read, or written.
    fn used_at(&self) -> &str {
        match &self.last_read_at {
            Some(read) if read.as_str() > self.created_at.as_str() => read,
            _ => &self.created_at,
        }
    }
}

/// A tier-1 entry: a file or directory nothing reads.
#[derive(Debug, Clone)]
pub struct Leftover {
    pub kind: LeftoverKind,
    /// What is deleted (a directory is deleted with its contents).
    pub path: PathBuf,
    /// Sidecar files deleted with it (a `sql-cache` copy's `.version` stamp).
    pub with: Vec<PathBuf>,
    pub bytes: u64,
    /// Seconds since the epoch it was last modified: older goes first.
    pub modified: u64,
}

/// What must not be evicted.
#[derive(Debug, Clone, Default)]
pub struct Protections {
    /// Files never evicted: what this command produced, read or returned.
    pub paths: HashSet<PathBuf>,
    /// Artifact paths as recorded, never evicted (the same, before mapping to a file).
    pub recorded: HashSet<String>,
    /// Run hashes never evicted: those of this command, and of runs in flight in this process.
    pub run_hashes: HashSet<String>,
    /// Base node ids whose current results are not evicted (tiers 3 and 4): nodes of a run in
    /// flight whose run hashes are not all known yet (dynamic partitions).
    pub nodes: HashSet<String>,
    /// Base node ids with `freshness=Manual`: their current result is not evicted unless the
    /// store holds it (nothing but a person recomputes a `Manual` node).
    pub manual: HashSet<String>,
    /// False when the project's `Manual` nodes could not be read: then no current result
    /// without a store copy is evicted at all.
    pub manual_known: bool,
    /// The last tier that may be evicted: [`Tier::Superseded`] while another process has a run
    /// in flight on this `.barca/`, or when `barca gc` runs with no limit.
    pub max_tier: Option<Tier>,
    /// Keep every staging directory: another process has a run in flight, possibly in another
    /// pid namespace (a second container on the same volume), where a pid that looks dead here
    /// can be alive.
    pub keep_staging: bool,
}

impl Protections {
    fn allows(&self, tier: Tier) -> bool {
        self.max_tier.is_none_or(|max| tier <= max)
    }
}

/// How far to evict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    /// Evict in order until the total is at or below `target` bytes.
    DownTo { target: u64 },
    /// Evict everything the protections allow (`barca gc` with no limit: tiers 1 and 2).
    Everything,
}

/// One entry of the eviction list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Eviction {
    pub tier: Tier,
    /// What is deleted.
    pub path: PathBuf,
    /// Sidecar files deleted with it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<PathBuf>,
    pub bytes: u64,
    /// For a leftover, what kind it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leftover: Option<LeftoverKind>,
    /// For a result, the node it belongs to (with its partition key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// For a result: (environment, recorded artifact path) of the rows marked `evicted_at`.
    #[serde(skip)]
    pub rows: Vec<(String, String)>,
    /// For a result, the run hashes of its rows (a run that starts later may pin them).
    #[serde(skip)]
    pub run_hashes: Vec<String>,
}

/// The ordered eviction list, and what it leaves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub evictions: Vec<Eviction>,
    /// The total after every eviction in the list.
    pub after: u64,
    /// The goal was not reached: what may be evicted was not enough.
    pub short: bool,
}

impl Plan {
    pub fn bytes(&self) -> u64 {
        self.evictions.iter().map(|e| e.bytes).sum()
    }
}

/// A file's candidate: its tier, ordering key and rows.
struct Candidate<'r> {
    tier: Tier,
    file: &'r PathBuf,
    rows: Vec<&'r Row>,
}

/// The eviction list for `total` bytes on disk.
///
/// Rows are grouped by file: a file is evicted with all its rows, and is protected if any of
/// them is. Rows whose file is not there, or that have no file barca may delete, are only used
/// to tell which result of a node is the newest.
pub fn plan(
    total: u64,
    rows: &[Row],
    leftovers: &[Leftover],
    protect: &Protections,
    goal: Goal,
) -> Plan {
    // The newest successful row of each node, per environment.
    let mut newest: HashMap<(&str, &str), i64> = HashMap::new();
    for row in rows {
        let slot = newest
            .entry((row.env.as_str(), row.node_id.as_str()))
            .or_insert(row.id);
        *slot = (*slot).max(row.id);
    }
    let is_newest =
        |row: &Row| newest.get(&(row.env.as_str(), row.node_id.as_str())) == Some(&row.id);

    let mut by_file: HashMap<&PathBuf, Vec<&Row>> = HashMap::new();
    for row in rows {
        if let (Some(file), true) = (&row.file, row.present) {
            by_file.entry(file).or_default().push(row);
        }
    }
    let mut candidates: Vec<Candidate<'_>> = by_file
        .into_iter()
        .filter_map(|(file, rows)| {
            let current = rows.iter().any(|r| is_newest(r));
            let tier = match current {
                false => Tier::Superseded,
                true if rows.iter().any(|r| is_newest(r) && r.store_held) => Tier::StoreHeld,
                true => Tier::Current,
            };
            (!protected(protect, tier, file, &rows)).then_some(Candidate { tier, file, rows })
        })
        .filter(|c| protect.allows(c.tier))
        .collect();
    // Superseded: oldest first. Current results: least recently used first. Ties by path, so
    // the order never depends on hashing.
    candidates.sort_by(|a, b| {
        let key = |c: &Candidate<'_>| match c.tier {
            Tier::Superseded => c
                .rows
                .iter()
                .map(|r| r.created_at.as_str())
                .max()
                .unwrap_or("")
                .to_string(),
            _ => c
                .rows
                .iter()
                .map(|r| r.used_at())
                .max()
                .unwrap_or("")
                .to_string(),
        };
        (a.tier, key(a), a.file).cmp(&(b.tier, key(b), b.file))
    });

    let mut left: Vec<&Leftover> = leftovers
        .iter()
        .filter(|l| protect.allows(Tier::Leftovers) && !protect.paths.contains(&l.path))
        .filter(|l| !(protect.keep_staging && l.kind == LeftoverKind::Staging))
        .collect();
    left.sort_by(|a, b| (a.kind, a.modified, &a.path).cmp(&(b.kind, b.modified, &b.path)));

    let ordered = left
        .into_iter()
        .map(|l| Eviction {
            tier: Tier::Leftovers,
            path: l.path.clone(),
            with: l.with.clone(),
            bytes: l.bytes,
            leftover: Some(l.kind),
            node: None,
            rows: Vec::new(),
            run_hashes: Vec::new(),
        })
        .chain(candidates.into_iter().map(|c| {
            let mut rows: Vec<(String, String)> = c
                .rows
                .iter()
                .map(|r| (r.env.clone(), r.recorded.clone()))
                .collect();
            rows.sort();
            rows.dedup();
            let node = c
                .rows
                .iter()
                .map(|r| r.node_id.as_str())
                .min()
                .map(String::from);
            let mut run_hashes: Vec<String> =
                c.rows.iter().filter_map(|r| r.run_hash.clone()).collect();
            run_hashes.sort();
            run_hashes.dedup();
            Eviction {
                tier: c.tier,
                path: c.file.clone(),
                with: Vec::new(),
                bytes: c.rows.iter().map(|r| r.bytes).max().unwrap_or(0),
                leftover: None,
                node,
                rows,
                run_hashes,
            }
        }));

    let mut out = Plan {
        evictions: Vec::new(),
        after: total,
        short: false,
    };
    for eviction in ordered {
        if let Goal::DownTo { target } = goal
            && out.after <= target
        {
            break;
        }
        out.after = out.after.saturating_sub(eviction.bytes);
        out.evictions.push(eviction);
    }
    if let Goal::DownTo { target } = goal {
        out.short = out.after > target;
    }
    out
}

fn base_of(id: &str) -> &str {
    id.split('[').next().unwrap_or(id)
}

/// Whether a file of `tier` with these rows must stay.
fn protected(protect: &Protections, tier: Tier, file: &PathBuf, rows: &[&Row]) -> bool {
    if protect.paths.contains(file) {
        return true;
    }
    let pinned = rows.iter().any(|r| {
        protect.recorded.contains(&r.recorded)
            || r.run_hash
                .as_ref()
                .is_some_and(|h| protect.run_hashes.contains(h))
    });
    if pinned {
        return true;
    }
    if tier < Tier::StoreHeld {
        return false;
    }
    if rows
        .iter()
        .any(|r| protect.nodes.contains(base_of(&r.node_id)))
    {
        return true;
    }
    // A current result with no store copy is recomputed when needed, which nothing does for a
    // `Manual` node except a person.
    tier == Tier::Current
        && (!protect.manual_known
            || rows
                .iter()
                .any(|r| protect.manual.contains(base_of(&r.node_id))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, node: &str, hash: &str, bytes: u64, created: &str) -> Row {
        Row {
            env: "default".into(),
            id,
            node_id: node.into(),
            run_hash: Some(hash.into()),
            recorded: format!(".barca/artifacts/{node}/{hash}.json"),
            file: Some(PathBuf::from(format!(
                ".barca/artifacts/{node}/{hash}.json"
            ))),
            bytes,
            present: true,
            created_at: format!("2026-10-20 {created}"),
            last_read_at: None,
            store_held: false,
        }
    }

    fn leftover(kind: LeftoverKind, path: &str, bytes: u64, modified: u64) -> Leftover {
        Leftover {
            kind,
            path: PathBuf::from(path),
            with: Vec::new(),
            bytes,
            modified,
        }
    }

    fn open() -> Protections {
        Protections {
            manual_known: true,
            ..Default::default()
        }
    }

    fn names(plan: &Plan) -> Vec<String> {
        plan.evictions
            .iter()
            .map(|e| e.path.display().to_string())
            .collect()
    }

    /// One of everything: a leftover, a superseded result, a store-held current result, a
    /// current result. The rows of `a` were written in order h1, h2.
    fn project() -> (Vec<Row>, Vec<Leftover>) {
        let mut held = row(4, "p.py:b", "hb", 100, "10:00:00");
        held.store_held = true;
        let rows = vec![
            row(1, "p.py:a", "h1", 100, "09:00:00"),
            row(2, "p.py:a", "h2", 100, "09:30:00"),
            row(3, "p.py:c", "hc", 100, "08:00:00"),
            held,
        ];
        let left = vec![leftover(LeftoverKind::MovedAside, "x.moved-aside", 100, 5)];
        (rows, left)
    }

    #[test]
    fn tiers_are_evicted_in_order_until_the_target() {
        let (rows, left) = project();
        // 500 bytes on disk; evict down to 200: the leftover, the superseded h1, then the
        // store-held b. c (current, oldest) and h2 stay.
        let plan = plan(500, &rows, &left, &open(), Goal::DownTo { target: 200 });
        assert_eq!(
            names(&plan),
            [
                "x.moved-aside",
                ".barca/artifacts/p.py:a/h1.json",
                ".barca/artifacts/p.py:b/hb.json"
            ]
        );
        assert_eq!(
            plan.evictions.iter().map(|e| e.tier).collect::<Vec<_>>(),
            [Tier::Leftovers, Tier::Superseded, Tier::StoreHeld]
        );
        assert_eq!(plan.after, 200);
        assert!(!plan.short);
    }

    #[test]
    fn current_results_go_least_recently_read_first() {
        let (mut rows, left) = project();
        // c is older than h2 but was read after it was written.
        rows[2].last_read_at = Some("2026-10-20 11:00:00".into());
        let plan = plan(500, &rows, &left, &open(), Goal::DownTo { target: 0 });
        assert_eq!(
            names(&plan)[3..],
            [
                ".barca/artifacts/p.py:a/h2.json".to_string(),
                ".barca/artifacts/p.py:c/hc.json".to_string()
            ]
        );
        assert_eq!(plan.after, 0);
    }

    #[test]
    fn nothing_is_evicted_under_the_target() {
        let (rows, left) = project();
        let plan = plan(500, &rows, &left, &open(), Goal::DownTo { target: 500 });
        assert!(plan.evictions.is_empty());
        assert_eq!(plan.after, 500);
    }

    #[test]
    fn everything_stops_at_the_last_allowed_tier() {
        let (rows, left) = project();
        let protect = Protections {
            max_tier: Some(Tier::Superseded),
            ..open()
        };
        let plan = plan(500, &rows, &left, &protect, Goal::Everything);
        assert_eq!(
            names(&plan),
            ["x.moved-aside", ".barca/artifacts/p.py:a/h1.json"]
        );
        assert!(!plan.short);
        let plan = super::plan(500, &rows, &left, &protect, Goal::DownTo { target: 0 });
        assert!(plan.short, "tiers 3 and 4 are off limits");
    }

    #[test]
    fn protected_files_stay() {
        let (rows, left) = project();
        let mut protect = open();
        // This command's own output, by path; a pinned run hash; a pinned node.
        protect
            .paths
            .insert(PathBuf::from(".barca/artifacts/p.py:a/h1.json"));
        protect.run_hashes.insert("hb".into());
        protect.nodes.insert("p.py:c".into());
        protect.paths.insert(PathBuf::from("x.moved-aside"));
        let plan = plan(500, &rows, &left, &protect, Goal::DownTo { target: 0 });
        assert_eq!(names(&plan), [".barca/artifacts/p.py:a/h2.json"]);
        assert!(plan.short);
    }

    #[test]
    fn a_pinned_node_still_loses_its_superseded_results() {
        let (rows, _) = project();
        let mut protect = open();
        protect.nodes.insert("p.py:a".into());
        let plan = plan(400, &rows, &[], &protect, Goal::DownTo { target: 0 });
        let evicted = names(&plan);
        assert!(evicted.contains(&".barca/artifacts/p.py:a/h1.json".to_string()));
        assert!(!evicted.contains(&".barca/artifacts/p.py:a/h2.json".to_string()));
    }

    #[test]
    fn a_manual_node_keeps_its_current_result_unless_the_store_holds_it() {
        let (mut rows, _) = project();
        let mut protect = open();
        protect.manual.insert("p.py:c".into());
        protect.manual.insert("p.py:b".into());
        let plan_ = plan(400, &rows, &[], &protect, Goal::DownTo { target: 0 });
        let evicted = names(&plan_);
        assert!(evicted.contains(&".barca/artifacts/p.py:b/hb.json".to_string()));
        assert!(!evicted.contains(&".barca/artifacts/p.py:c/hc.json".to_string()));
        // A superseded result of a Manual node is not its current result.
        rows.push(row(5, "p.py:c", "hc2", 100, "12:00:00"));
        let plan_ = plan(500, &rows, &[], &protect, Goal::DownTo { target: 0 });
        assert!(names(&plan_).contains(&".barca/artifacts/p.py:c/hc.json".to_string()));
    }

    #[test]
    fn unknown_manual_nodes_protect_every_current_result_without_a_store_copy() {
        let (rows, _) = project();
        let protect = Protections::default();
        let plan = plan(400, &rows, &[], &protect, Goal::DownTo { target: 0 });
        assert_eq!(
            plan.evictions.iter().map(|e| e.tier).collect::<Vec<_>>(),
            [Tier::Superseded, Tier::StoreHeld]
        );
    }

    #[test]
    fn partition_keys_are_separate_results() {
        let rows = vec![
            row(1, "p.py:a[k=1]", "h1", 10, "09:00:00"),
            row(2, "p.py:a[k=2]", "h2", 10, "09:00:00"),
        ];
        let plan = plan(20, &rows, &[], &open(), Goal::DownTo { target: 0 });
        assert!(plan.evictions.iter().all(|e| e.tier == Tier::Current));
        assert_eq!(plan.evictions.len(), 2);
    }

    #[test]
    fn a_file_shared_by_rows_is_current_if_any_row_is_newest() {
        // The same run hash computed twice (a recompute after an eviction): one file, two rows,
        // and a newer result in between for another node does not matter.
        let mut rows = vec![
            row(1, "p.py:a", "h1", 10, "09:00:00"),
            row(3, "p.py:a", "h1", 10, "10:00:00"),
        ];
        rows.push(row(2, "p.py:a", "h0", 10, "09:30:00"));
        let plan = plan(20, &rows, &[], &open(), Goal::Everything);
        let one = plan
            .evictions
            .iter()
            .find(|e| e.path.ends_with("h1.json"))
            .unwrap();
        assert_eq!(one.tier, Tier::Current);
        assert_eq!(one.rows.len(), 1, "both rows name one recorded path");
        assert_eq!(one.bytes, 10, "the file is counted once");
    }

    #[test]
    fn missing_files_and_files_outside_barca_are_never_listed_but_still_decide_newest() {
        let mut rows = vec![
            row(1, "p.py:a", "h1", 10, "09:00:00"),
            row(2, "p.py:a", "h2", 10, "10:00:00"),
        ];
        rows[1].present = false;
        rows.push(Row {
            file: None,
            ..row(3, "p.py:b", "hb", 10, "09:00:00")
        });
        let plan = plan(20, &rows, &[], &open(), Goal::Everything);
        assert_eq!(names(&plan), [".barca/artifacts/p.py:a/h1.json"]);
        assert_eq!(plan.evictions[0].tier, Tier::Superseded);
    }

    #[test]
    fn environments_have_separate_newest_results() {
        let mut other = row(2, "p.py:a", "h2", 10, "10:00:00");
        other.env = "prod".into();
        let rows = vec![row(1, "p.py:a", "h1", 10, "09:00:00"), other];
        let plan = plan(20, &rows, &[], &open(), Goal::Everything);
        assert!(plan.evictions.iter().all(|e| e.tier == Tier::Current));
    }

    #[test]
    fn staging_stays_while_another_process_has_a_run_in_flight() {
        let left = vec![
            leftover(LeftoverKind::Staging, "st", 1, 1),
            leftover(LeftoverKind::MovedAside, "m", 1, 1),
        ];
        let protect = Protections {
            keep_staging: true,
            ..open()
        };
        let plan = plan(2, &[], &left, &protect, Goal::Everything);
        assert_eq!(names(&plan), ["m"]);
    }

    #[test]
    fn leftovers_go_by_kind_then_age() {
        let left = vec![
            leftover(LeftoverKind::SqlCache, "s-new", 1, 9),
            leftover(LeftoverKind::SqlCache, "s-old", 1, 1),
            leftover(LeftoverKind::MovedAside, "m", 1, 50),
            leftover(LeftoverKind::Staging, "st", 1, 99),
        ];
        let plan = plan(4, &[], &left, &open(), Goal::Everything);
        assert_eq!(names(&plan), ["st", "m", "s-old", "s-new"]);
    }

    #[test]
    fn the_same_inputs_give_the_same_list() {
        let (rows, left) = project();
        let a = plan(500, &rows, &left, &open(), Goal::DownTo { target: 0 });
        let mut reversed = rows.clone();
        reversed.reverse();
        let b = plan(500, &reversed, &left, &open(), Goal::DownTo { target: 0 });
        assert_eq!(a, b);
    }
}
