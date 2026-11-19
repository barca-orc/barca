//! One entry of the rollup tree (`barca status --rollup`, `GET /state/rollup`) and the counts
//! it sums: plain data, and the sums over rows. [`crate::rollup`] builds the tree.

use super::{Cause, DataCoverage, Health, NodeStatus};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

    pub(crate) fn slot(&mut self, health: Health) -> &mut usize {
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

    pub(crate) fn add(&mut self, other: &HealthCounts) {
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

/// How many of a scope's nodes (or assets) that are not fresh have each cause. They add up to
/// the not-fresh nodes: every health but `ok` and `suspect`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CauseCounts {
    pub catching_up: usize,
    pub queued: usize,
    pub no_refresh_path: usize,
    pub overdue: usize,
    pub failing: usize,
    pub undecided: usize,
}

impl CauseCounts {
    /// How many have `cause`.
    pub fn get(&self, cause: Cause) -> usize {
        match cause {
            Cause::CatchingUp => self.catching_up,
            Cause::Queued => self.queued,
            Cause::NoRefreshPath => self.no_refresh_path,
            Cause::Overdue => self.overdue,
            Cause::Failing => self.failing,
            Cause::Undecided => self.undecided,
        }
    }

    pub(crate) fn slot(&mut self, cause: Cause) -> &mut usize {
        match cause {
            Cause::CatchingUp => &mut self.catching_up,
            Cause::Queued => &mut self.queued,
            Cause::NoRefreshPath => &mut self.no_refresh_path,
            Cause::Overdue => &mut self.overdue,
            Cause::Failing => &mut self.failing,
            Cause::Undecided => &mut self.undecided,
        }
    }

    pub(crate) fn add(&mut self, other: &CauseCounts) {
        for cause in Cause::ALL {
            *self.slot(cause) += other.get(cause);
        }
    }

    /// Every count added up: the nodes (or assets) that are not fresh.
    pub fn total(&self) -> usize {
        Cause::ALL.iter().map(|c| self.get(*c)).sum()
    }

    /// `14 overdue · 152 no refresh path · 180 catching up`: every cause counted, what
    /// alarms first, in the display words; empty when every node is fresh.
    pub fn line(&self) -> String {
        Cause::ALL
            .into_iter()
            .filter(|c| self.get(*c) > 0)
            .map(|c| format!("{} {}", self.get(c), c.word()))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// The weight of one fresh node: every node counts, so a fresh scope still has a size.
pub const FRESH_WEIGHT: usize = 1;

/// A scope's distance from fresh, in weighted nodes, by what the nodes are: each cause's
/// nodes times [`Cause::weight`], and the fresh nodes times [`FRESH_WEIGHT`]. They add up to
/// the entry's `weight`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Weights {
    pub failing: usize,
    pub overdue: usize,
    pub no_refresh_path: usize,
    pub queued: usize,
    pub undecided: usize,
    pub catching_up: usize,
    /// The nodes with no cause (`ok` and `suspect`), one each.
    pub fresh: usize,
}

impl Weights {
    /// The weights of `nodes` nodes of which `causes` are not fresh.
    pub fn of(nodes: usize, causes: &CauseCounts) -> Self {
        let w = |cause: Cause| causes.get(cause) * cause.weight();
        Weights {
            failing: w(Cause::Failing),
            overdue: w(Cause::Overdue),
            no_refresh_path: w(Cause::NoRefreshPath),
            queued: w(Cause::Queued),
            undecided: w(Cause::Undecided),
            catching_up: w(Cause::CatchingUp),
            fresh: nodes.saturating_sub(causes.total()) * FRESH_WEIGHT,
        }
    }

    /// Every part added up.
    pub fn total(&self) -> usize {
        self.failing
            + self.overdue
            + self.no_refresh_path
            + self.queued
            + self.undecided
            + self.catching_up
            + self.fresh
    }
}

impl CauseCounts {
    /// The weight of the nodes counted: each cause's nodes times its weight. What a root
    /// holds weighs this much (it holds no fresh node).
    pub fn weight(&self) -> usize {
        Cause::ALL.iter().map(|c| self.get(*c) * c.weight()).sum()
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
    /// Why the keys that are `behind` are not fresh: the node's own cause. `null` when the
    /// node is fresh. A surface colours the band of those keys by it.
    #[serde(default)]
    pub cause: Option<Cause>,
}

/// The nodes of one kind in a scope, by where they stand: what a list of one kind (the web
/// UI's Assets, Tasks and Sensors pages) counts its filter chips with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KindCounts {
    /// Nodes of the kind.
    pub nodes: usize,
    /// Of them, fresh: no cause (`ok` and `suspect`).
    pub fresh: usize,
    /// Of them, not fresh, by cause. With `fresh` they add up to `nodes`.
    pub causes: CauseCounts,
}

impl KindCounts {
    pub(crate) fn count(&mut self, node: &NodeStatus) {
        self.nodes += 1;
        match node.cause {
            Some(cause) => *self.causes.slot(cause) += 1,
            None => self.fresh += 1,
        }
    }
}

/// A scope's nodes by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KindRollup {
    pub asset: KindCounts,
    pub task: KindCounts,
    pub sensor: KindCounts,
}

impl KindRollup {
    pub(crate) fn count(&mut self, node: &NodeStatus) {
        match node.kind.as_str() {
            "asset" => self.asset.count(node),
            "task" => self.task.count(node),
            _ => self.sensor.count(node),
        }
    }

    /// The counts of every node of `nodes`.
    pub fn of<'a>(nodes: impl IntoIterator<Item = &'a NodeStatus>) -> Self {
        let mut out = KindRollup::default();
        for node in nodes {
            out.count(node);
        }
        out
    }
}

/// What one root holds underneath an entry: the root list cut to a scope, so a surface showing
/// a group lists the roots that hold its nodes with the numbers they hold there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HeldBy {
    /// The root's id: an entry of `roots`, which carries its detail and its action.
    pub root: String,
    /// Nodes underneath the entry that this root holds.
    pub nodes: usize,
    /// Of them, assets.
    pub assets: usize,
    /// Those nodes, by cause.
    pub causes: CauseCounts,
    /// The freshness gap of those nodes, node-seconds.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub gap_seconds: i64,
    /// Of those nodes, the ones with no `expected_fresh_at`.
    pub unbounded: usize,
    /// The weight of those nodes: what the list is ranked by ([`CauseCounts::weight`]).
    #[serde(default)]
    pub weight: usize,
    /// Nodes held by this root and by the roots listed before it: against the entry's
    /// `not_fresh`, how much of the scope the first roots explain.
    pub cumulative_nodes: usize,
    /// The root's action, retained when the scope is filtered.
    #[serde(default)]
    pub action_kind: ActionKind,
    /// The oldest `stale_since` among those nodes (RFC 3339, UTC): since when the first of
    /// them has been out of date. `null` when none has one recorded.
    #[serde(default)]
    pub stale_since: Option<String>,
    /// The latest `expected_fresh_at` among those nodes: when the ones with an estimate are
    /// all expected fresh. `null` when none has one (`unbounded` is all of them).
    #[serde(default)]
    pub fresh_by: Option<String>,
}

/// A scope's not-fresh nodes in two parts: those held by a root that needs someone and those
/// held by a root that is in hand ([`ActionKind::needs_someone`]). A node has one root, so the
/// node counts and the weights are the exact sums of the children's; the root counts are not
/// (one root may hold nodes under several children).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HeldSplit {
    /// Roots holding a node underneath that need someone: the head of `held_by`.
    pub need_someone: usize,
    /// Roots a tick or a run in flight will clear.
    pub catching_up: usize,
    /// Roots run by hand on request.
    pub run_by_hand: usize,
    /// Nodes underneath held by a root that needs someone.
    pub need_someone_nodes: usize,
    /// Nodes underneath held by a root that is in hand. With `need_someone_nodes` it adds up
    /// to `not_fresh`.
    pub in_hand_nodes: usize,
    /// The weight of the `need_someone_nodes` ([`Cause::weight`] of each).
    pub need_someone_weight: usize,
    /// The weight of the `in_hand_nodes`. With `need_someone_weight` it adds up to the weight
    /// of what is not fresh (`weight` less `weights.fresh`).
    pub in_hand_weight: usize,
}

impl HeldSplit {
    /// The split of `rows`, given the ids of the roots that need someone.
    pub(crate) fn of<'a>(
        rows: impl IntoIterator<Item = &'a NodeStatus>,
        actions: &HashMap<String, ActionKind>,
    ) -> Self {
        let mut out = HeldSplit::default();
        let mut seen: HashSet<&str> = HashSet::new();
        for node in rows {
            let (Some(root), Some(cause)) = (&node.root, node.cause) else {
                continue;
            };
            let first = seen.insert(root.id.as_str());
            let action = actions[&root.id];
            if action.needs_someone() {
                out.need_someone += usize::from(first);
                out.need_someone_nodes += 1;
                out.need_someone_weight += cause.weight();
            } else {
                if action == ActionKind::RunByHand {
                    out.run_by_hand += usize::from(first);
                } else {
                    out.catching_up += usize::from(first);
                }
                out.in_hand_nodes += 1;
                out.in_hand_weight += cause.weight();
            }
        }
        out
    }

    /// Number of roots that need action, will clear automatically, or run on request.
    pub fn line(&self) -> String {
        let needs = self.need_someone;
        let waiting = self.catching_up;
        let manual = self.run_by_hand;
        let mut line = format!(
            "{needs} {} you · {waiting} {} catching up",
            if needs == 1 { "needs" } else { "need" },
            if waiting == 1 { "is" } else { "are" }
        );
        if manual > 0 {
            line.push_str(&format!(" · {manual} run by hand"));
        }
        line
    }
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
    /// Nodes underneath that are not fresh, by cause.
    #[serde(default)]
    pub causes: CauseCounts,
    /// Assets underneath that are not fresh, by cause.
    #[serde(default)]
    pub asset_causes: CauseCounts,
    /// Nodes underneath that are not fresh: the nodes `causes` counts, as one number.
    #[serde(default)]
    pub not_fresh: usize,
    /// The entry's distance from fresh, in weighted nodes: every node underneath counts
    /// [`FRESH_WEIGHT`], and one that is not fresh its cause's weight instead
    /// ([`Cause::weight`]). The exact sum of the children's; what the health map sizes a
    /// segment by.
    #[serde(default)]
    pub weight: usize,
    /// `weight` by what the nodes are: each cause, and fresh.
    #[serde(default)]
    pub weights: Weights,
    /// The roots that hold those nodes, each with what it holds underneath this entry, in
    /// the order of the root list applied to what is held here (the roots that need someone
    /// first; in each part the most weight held first, a failure first among equals; then
    /// nodes, gap and id). Their `nodes` add up to `not_fresh`. Empty on a node entry: a
    /// node's root is on its row.
    #[serde(default)]
    pub held_by: Vec<HeldBy>,
    /// The not-fresh nodes underneath by whether their root needs someone or is in hand:
    /// how many roots of each, how many nodes they hold and what those weigh. What a list
    /// of scopes is ordered by: the most `need_someone_weight` first, then the most
    /// `in_hand_weight`, so a fully fresh scope (both 0) is last.
    #[serde(default)]
    pub split: HeldSplit,
    /// The oldest `stale_since` underneath (RFC 3339, UTC): since when the first node that
    /// is not fresh has been out of date. `null` when every node is fresh or none has one
    /// recorded.
    #[serde(default)]
    pub stale_since: Option<String>,
    /// The freshness gap underneath, node-seconds: the sum of the nodes' `gap_seconds`.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub gap_seconds: i64,
    /// Nodes underneath that are not fresh and have no `expected_fresh_at`: nothing known
    /// will leave them fresh (failing, blocked, undecided, missed by a tick, no refresh
    /// path), so `fresh_by` does not speak for them.
    #[serde(default)]
    pub unbounded: usize,
    /// The latest `expected_fresh_at` underneath (RFC 3339, UTC): when the nodes that have
    /// an estimate are all expected fresh. `null` when none has one.
    #[serde(default)]
    pub fresh_by: Option<String>,
    /// A partitioned node's keys (a node entry only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub keys: Option<KeyCounts>,
    /// The other groups that declare this node or group, by id: it is counted under `parent`
    /// alone, and a list of a group named here shows it as a member counted elsewhere. Empty
    /// for an entry declared in at most one group.
    #[serde(default)]
    pub also_in: Vec<String>,
}

impl RollupEntry {
    pub(crate) fn inner(id: String, kind: EntryKind, label: String) -> Self {
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
            causes: CauseCounts::default(),
            asset_causes: CauseCounts::default(),
            not_fresh: 0,
            weight: 0,
            weights: Weights::default(),
            held_by: Vec::new(),
            split: HeldSplit::default(),
            stale_since: None,
            gap_seconds: 0,
            unbounded: 0,
            fresh_by: None,
            keys: None,
            also_in: Vec::new(),
        }
    }
}

impl RollupEntry {
    /// The scope's distance from fresh in a line, as of `now` (unix seconds): `14 overdue ·
    /// 152 no refresh path · 180 catching up (nodes);
    /// about 180 fresh within 5m, 166 with no estimate; 38h of gap across nodes (node-time)`. The times
    /// lead and the sum comes last with its unit: it is node-time added up over the nodes,
    /// never how long anything has been out of date. `every node is fresh` when none is
    /// behind.
    pub fn gap_line(&self, now: i64) -> String {
        let behind = self.causes.total();
        if behind == 0 {
            return "every node is fresh".to_string();
        }
        let mut line = format!("{} (nodes)", self.causes.line());
        let estimated = behind.saturating_sub(self.unbounded);
        let within = self
            .fresh_by
            .as_deref()
            .and_then(barca_model::time::parse_rfc3339)
            .map(|at| {
                // Rounded up to the unit shown: an estimate is never said to be sooner.
                let left = (at - now).max(0);
                let unit = match left {
                    s if s < 60 => 1,
                    s if s < 3600 => 60,
                    s if s < 86_400 => 3600,
                    _ => 86_400,
                };
                barca_store::text::span((left + unit - 1) / unit * unit)
            });
        let mut said = Vec::new();
        if let (Some(within), true) = (within, estimated > 0) {
            said.push(format!("about {estimated} fresh within {within}"));
        }
        if self.unbounded > 0 {
            said.push(format!("{} with no estimate", self.unbounded));
        }
        if !said.is_empty() {
            line.push_str(&format!("; {}", said.join(", ")));
        }
        if self.gap_seconds > 0 {
            line.push_str(&format!(
                "; {} of gap across nodes (node-time)",
                barca_store::text::span(self.gap_seconds)
            ));
        }
        line
    }
}

impl RollupEntry {
    /// Sets `weight` and `weights` from the entry's own `nodes` and `causes`: linear in the
    /// counts, so a level's weight is the sum of its children's.
    pub(crate) fn weigh(&mut self) {
        self.weights = Weights::of(self.nodes, &self.causes);
        self.weight = self.weights.total();
    }
}

/// What a level adds up from its children: the one place a new summed field is added.
pub(crate) trait Sums {
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
        self.causes.add(&child.causes);
        self.asset_causes.add(&child.asset_causes);
        self.not_fresh += child.not_fresh;
        self.weigh();
        self.gap_seconds += child.gap_seconds;
        self.unbounded += child.unbounded;
        // The latest estimate underneath. Every one is RFC 3339 UTC to the second, so the
        // later time is the greater string.
        if child.fresh_by > self.fresh_by {
            self.fresh_by.clone_from(&child.fresh_by);
        }
        oldest(&mut self.stale_since, child.stale_since.as_deref());
    }
}

/// Keeps in `oldest` the earlier of it and `at`: both RFC 3339 UTC to the second, so the
/// earlier time is the lesser string. A missing time is never the oldest.
pub(crate) fn oldest(oldest: &mut Option<String>, at: Option<&str>) {
    if let Some(at) = at
        && oldest.as_deref().is_none_or(|known| at < known)
    {
        *oldest = Some(at.to_string());
    }
}

/// Keeps in `latest` the later of it and `at`.
pub(crate) fn latest(latest: &mut Option<String>, at: Option<&str>) {
    if let Some(at) = at
        && latest.as_deref().is_none_or(|known| at > known)
    {
        *latest = Some(at.to_string());
    }
}

/// What clears a root.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Nothing to do: a run in flight or the next tick computes what it holds.
    #[default]
    Wait,
    /// Run `command`: ticks missed what it holds, or a pin holds it (`barca unpin`).
    Run,
    /// Run `command` when its result is wanted: the node is run by hand by declaration (a
    /// `Manual` node, or a task no schedule reaches), so nothing is overdue and nobody is
    /// asked to act.
    RunByHand,
    /// Give the node (or one above it) a `Schedule`: nothing scheduled reaches it.
    AddSchedule,
    /// Resume the paused schedule (`barca resume --schedule`) or node (`barca resume`).
    ResumeSchedule,
    /// Start a server that fires schedules (`barca serve`).
    StartServer,
    /// Fix the error, then run `command` to try again.
    FixFailure,
    /// Give the server more run slots (`--max-runs`, `[server] max_runs`), or space the
    /// schedules: its ticks wait for a slot longer than they are apart.
    RaiseMaxRuns,
}

impl ActionKind {
    /// The value as JSON spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Wait => "wait",
            ActionKind::Run => "run",
            ActionKind::RunByHand => "run_by_hand",
            ActionKind::AddSchedule => "add_schedule",
            ActionKind::ResumeSchedule => "resume_schedule",
            ActionKind::StartServer => "start_server",
            ActionKind::FixFailure => "fix_failure",
            ActionKind::RaiseMaxRuns => "raise_max_runs",
        }
    }

    /// The display word: what the CLI prints and the UI shows.
    pub fn word(self) -> &'static str {
        match self {
            ActionKind::Wait => "wait",
            ActionKind::Run => "run",
            ActionKind::RunByHand => "run by hand",
            ActionKind::AddSchedule => "add a schedule",
            ActionKind::ResumeSchedule => "resume",
            ActionKind::StartServer => "start a server",
            ActionKind::FixFailure => "fix the failure",
            ActionKind::RaiseMaxRuns => "raise max_runs",
        }
    }
}

impl ActionKind {
    /// Whether the action asks someone to act. `wait` does not (a run in flight or a tick
    /// clears the root) and neither does `run_by_hand` (the node waits for its author, by
    /// declaration); every other action does. A root list puts the roots that need someone
    /// first.
    pub fn needs_someone(self) -> bool {
        !matches!(self, ActionKind::Wait | ActionKind::RunByHand)
    }

    /// What to do where the action has no command, after the display word.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            ActionKind::AddSchedule => Some("freshness=Schedule(...) on it or on a node above it"),
            ActionKind::RaiseMaxRuns => Some(
                "barca serve --max-runs N or [server] max_runs in barca.toml, or space the schedules",
            ),
            _ => None,
        }
    }
}
