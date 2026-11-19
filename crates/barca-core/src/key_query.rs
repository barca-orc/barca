//! A partitioned node's keys, asked for: which keys (state, a dimension's values, a range of a
//! dimension, an error, text), in which order, one page at a time, and the node's keys in one
//! picture (the grid's rows, and what is wrong and where).
//!
//! One implementation for `barca status NODE --keys` and `--overview`, on this machine and from
//! a server (`GET /assets/{name}/partitions/keys`, `.../partitions/overview`), and for the web
//! UI's key explorer, which renders these answers and derives none of them.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::db::PartitionErrorGroup;
use crate::partition_grid::{GridCounts, GridDimension, PartitionGrid, state};

/// The most keys or values a served run selection spells out. A larger set that is not a
/// rectangle of the dimensions has no selection: it is named by narrowing the filter.
pub const SELECTION_MAX: usize = 5_000;

/// The most cells (keys and gaps) the overview's rows hold: rows past it are left out and
/// counted.
pub const OVERVIEW_CELLS_MAX: usize = 200_000;

/// The states of a key a run computes: every state but fresh and pinned.
pub const NEEDS_RUN: [char; 4] = [
    state::FAILED,
    state::BLOCKED,
    state::STALE,
    state::NEVER_RUN,
];

/// The order keys are listed in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeySort {
    /// Key order: the dimensions' values in their own order, the last varying fastest.
    #[default]
    Key,
    /// Most urgent state first (failed, blocked, stale, never run, pinned, fresh).
    State,
    /// Latest attempt first; keys never attempted last.
    Attempted,
    /// Slowest latest attempt first.
    Elapsed,
    /// Largest result first.
    Size,
    /// Most rows first.
    Rows,
}

impl KeySort {
    pub const NAMES: [(&'static str, KeySort); 6] = [
        ("key", KeySort::Key),
        ("state", KeySort::State),
        ("attempted", KeySort::Attempted),
        ("elapsed", KeySort::Elapsed),
        ("size", KeySort::Size),
        ("rows", KeySort::Rows),
    ];

    pub fn parse(name: &str) -> Result<KeySort, String> {
        Self::NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| *s)
            .ok_or_else(|| {
                format!(
                    "unknown key order '{name}': use {}",
                    Self::NAMES.map(|(n, _)| n).join(", ")
                )
            })
    }
}

/// A stretch of one dimension, in that dimension's order, both ends included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRange {
    pub dimension: String,
    pub from: String,
    pub to: String,
}

/// Which keys are listed, in which order, and which page of them. Every filter that is set
/// must hold.
#[derive(Debug, Clone, Default)]
pub struct KeyQuery {
    /// [`state`] characters; empty: every state.
    pub states: Vec<char>,
    /// Only keys a run computes ([`NEEDS_RUN`]).
    pub needs_run: bool,
    /// Per dimension, the values kept.
    pub values: Vec<(String, Vec<String>)>,
    pub range: Option<KeyRange>,
    /// An index into the grid's `errors`: the failed keys with that error.
    pub error: Option<usize>,
    /// A substring of the key, in any case.
    pub text: String,
    pub sort: KeySort,
    pub offset: usize,
    /// `None`: every key from `offset` on.
    pub limit: Option<usize>,
}

/// A [`KeyQuery`] as the CLI's flags and the route's parameters spell it.
#[derive(Debug, Clone, Default)]
pub struct KeyQueryArgs<'a> {
    /// `--state failed,blocked` / `state=failed,blocked`.
    pub states: &'a [String],
    /// `--needs-run` / `needs_run=1`.
    pub needs_run: bool,
    /// `--value region=apac,emea` / `value=region=apac,emea`, one per dimension.
    pub values: &'a [String],
    /// `--range day=2026-01-01..2026-01-31` / `range=...`.
    pub range: Option<&'a str>,
    /// `--error 0` / `error=0`.
    pub error: Option<usize>,
    /// `--match apac` / `q=apac`.
    pub text: Option<&'a str>,
    /// `--sort state` / `sort=state`.
    pub sort: Option<&'a str>,
    pub offset: usize,
    pub limit: Option<usize>,
}

impl KeyQuery {
    /// The query the arguments spell. A state or an order barca does not have, or a value or
    /// range that is not `DIMENSION=...`, is an error that names what is accepted.
    pub fn parse(args: &KeyQueryArgs) -> Result<KeyQuery, String> {
        let mut states = Vec::new();
        for name in args
            .states
            .iter()
            .flat_map(|n| n.split(','))
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            match state::NAMED.iter().find(|(_, word)| *word == name) {
                Some((c, _)) => {
                    if !states.contains(c) {
                        states.push(*c);
                    }
                }
                None => {
                    return Err(format!(
                        "unknown key state '{name}': use {}",
                        state::NAMED.map(|(_, word)| word).join(", ")
                    ));
                }
            }
        }
        let mut values: Vec<(String, Vec<String>)> = Vec::new();
        for spec in args.values.iter().filter(|s| !s.trim().is_empty()) {
            let (dimension, list) = spec
                .split_once('=')
                .filter(|(d, v)| !d.trim().is_empty() && !v.is_empty())
                .ok_or_else(|| {
                    format!(
                        "value '{spec}': use DIMENSION=VALUE[,VALUE...], as in region=apac,emea"
                    )
                })?;
            let list: Vec<String> = list
                .split(',')
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
            match values.iter_mut().find(|(d, _)| d == dimension.trim()) {
                Some((_, kept)) => kept.extend(list),
                None => values.push((dimension.trim().to_string(), list)),
            }
        }
        let range = match args.range.filter(|r| !r.trim().is_empty()) {
            None => None,
            Some(spec) => {
                let parsed = spec.split_once('=').and_then(|(dimension, stretch)| {
                    let (from, to) = stretch.split_once("..")?;
                    (!dimension.trim().is_empty() && !from.is_empty() && !to.is_empty()).then(
                        || KeyRange {
                            dimension: dimension.trim().to_string(),
                            from: from.to_string(),
                            to: to.to_string(),
                        },
                    )
                });
                Some(parsed.ok_or_else(|| {
                    format!(
                        "range '{spec}': use DIMENSION=FROM..TO, as in day=2026-01-01..2026-01-31"
                    )
                })?)
            }
        };
        Ok(KeyQuery {
            states,
            needs_run: args.needs_run,
            values,
            range,
            error: args.error,
            text: args.text.unwrap_or_default().trim().to_string(),
            sort: match args.sort.map(str::trim).filter(|s| !s.is_empty()) {
                Some(name) => KeySort::parse(name)?,
                None => KeySort::Key,
            },
            offset: args.offset,
            limit: args.limit,
        })
    }

    /// Whether any filter narrows the keys (the order and the page do not).
    pub fn filters(&self) -> bool {
        !self.states.is_empty()
            || self.needs_run
            || !self.values.is_empty()
            || self.range.is_some()
            || self.error.is_some()
            || !self.text.is_empty()
    }
}

/// One key, decoded: a line of `barca status NODE --keys`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeyRow {
    /// `day=2026-01-01,region=apac`.
    pub key: String,
    /// The key's value of each dimension, in the order of `dimensions`.
    pub values: Vec<String>,
    /// `failed`, `blocked`, `stale`, `never_run`, `pinned` or `fresh`.
    pub state: String,
    /// The key's latest attempt (RFC 3339); null where it has none.
    pub attempted_at: Option<String>,
    pub elapsed_seconds: Option<f64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub size_bytes: Option<i64>,
    /// Rows the latest attempt wrote, where its worker counted them.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub rows: Option<i64>,
    pub run_id: Option<String>,
    /// A failed key's error as the grid groups it (an entry of `errors`): the first line,
    /// with the key's own values replaced by their dimension's name.
    pub error: Option<String>,
}

/// `barca status NODE --keys --json`, `GET /assets/{name}/partitions/keys`: one page of the
/// keys a query matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeysPage {
    pub id: String,
    /// False while the keys are not known: a `partitions_from` source that has not run.
    pub known: bool,
    /// The dimensions' names, in key order.
    pub dimensions: Vec<String>,
    /// Every key of the node by state, whatever the query.
    pub counts: GridCounts,
    /// The failed keys' errors, most common first; `--error N` is an index into it.
    pub errors: Vec<PartitionErrorGroup>,
    pub keys: Vec<KeyRow>,
    /// Keys the query matches; `keys` is a page of them.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    /// True when keys the query matches come after this page.
    pub truncated: bool,
    /// Keys skipped before this page.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub offset: usize,
    /// Of the keys the query matches, those a run computes (failed, blocked, stale, never
    /// run).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub needs_run: usize,
    /// The keys the query matches as `--partition` takes them: a JSON object of each
    /// dimension's values when they form a rectangle, a list of keys otherwise. Past
    /// [`SELECTION_MAX`] scattered keys, a query for the keys a run computes that filters by
    /// dimension alone (`--needs-run`, `--value`, `--range`) gets its filters' rectangle,
    /// whose run computes the same keys; any other gets null. Null when nothing matches.
    pub selection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub hint: Option<String>,
}

/// Where the keys of an issue sit in one dimension they do not span whole.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct IssuePlace {
    pub dimension: String,
    /// `apac`, `2026-03-01 → 2026-03-20`, `7 values, a … k`.
    pub words: String,
}

/// A problem some keys share: one error of the failed keys, or a state a run clears.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeyIssue {
    /// The state of its keys: `failed`, `blocked`, `stale` or `never_run`.
    pub state: String,
    /// For failed keys with a grouped error: its index into `errors`.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub error: Option<usize>,
    /// The error's first line, or what the state means.
    pub title: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub keys: usize,
    /// Per dimension the keys do not span whole, where they are.
    pub places: Vec<IssuePlace>,
    /// Its keys as `--partition` takes them; null past [`SELECTION_MAX`] scattered keys.
    pub selection: Option<String>,
}

/// One row of the overview: a combination of every dimension but the one the rows run along.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct OverviewRow {
    /// `region=apac`; empty for a one-dimension node.
    pub label: String,
    /// The row's value of each other dimension, in order.
    pub values: Vec<String>,
    /// One character per value of the dimension the rows run along, in its order: `c` fresh,
    /// `p` pinned, `s` stale, `n` never run, `f` failed, `b` blocked, `-` not a key.
    pub states: String,
}

/// `barca status NODE --overview --json`, `GET /assets/{name}/partitions/overview`: a
/// partitioned node's keys in one picture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct KeysOverview {
    pub id: String,
    /// The partitioned nodes that share this node's keys, upstream first; this node among them.
    pub chain: Vec<String>,
    pub known: bool,
    pub dimensions: Vec<GridDimension>,
    pub counts: GridCounts,
    /// Keys a run computes: failed, blocked, stale and never run.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub needs_run: usize,
    pub errors: Vec<PartitionErrorGroup>,
    /// What is wrong and where, the most keys first: each error of the failed keys, then the
    /// blocked, stale and never-run keys.
    pub issues: Vec<KeyIssue>,
    /// The dimension the rows run along: the one with the most values. Null with no keys.
    pub along: Option<String>,
    pub rows: Vec<OverviewRow>,
    /// Rows the node has; `rows` holds the first of them.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub rows_total: usize,
    pub truncated: bool,
    /// The newest attempt of any key (RFC 3339); null with none.
    pub last_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub hint: Option<String>,
}

/// What a state means for the keys in it, as an issue's title.
fn state_title(code: char) -> &'static str {
    match code {
        state::BLOCKED => "Waiting on the same keys of an input that failed",
        state::STALE => "Code or inputs changed since they succeeded",
        state::NEVER_RUN => "Never run",
        _ => "Failed",
    }
}

/// `12345` as `12,345`.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

impl PartitionGrid {
    /// A query's filters against this grid: per dimension which value indexes are kept
    /// (`None`: all), or why the query does not fit the node.
    fn kept_values(&self, query: &KeyQuery) -> Result<Vec<Option<Vec<bool>>>, String> {
        let names = || {
            self.dimensions
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let index = |name: &str| -> Result<usize, String> {
            self.dimensions
                .iter()
                .position(|d| d.name == name)
                .ok_or_else(|| {
                    format!(
                        "'{}' has no dimension '{name}': its dimensions are {}",
                        self.id,
                        names()
                    )
                })
        };
        let mut kept: Vec<Option<Vec<bool>>> = vec![None; self.dimensions.len()];
        for (name, values) in &query.values {
            let d = index(name)?;
            let wanted: BTreeSet<&str> = values.iter().map(String::as_str).collect();
            kept[d] = Some(
                self.dimensions[d]
                    .values
                    .iter()
                    .map(|v| wanted.contains(v.as_str()))
                    .collect(),
            );
        }
        if let Some(range) = &query.range {
            let d = index(&range.dimension)?;
            let at = |value: &str| -> Result<usize, String> {
                self.dimensions[d]
                    .values
                    .iter()
                    .position(|v| v == value)
                    .ok_or_else(|| {
                        format!(
                            "'{}' has no {} '{value}': a range runs between two of its values",
                            self.id, range.dimension
                        )
                    })
            };
            let (a, b) = (at(&range.from)?, at(&range.to)?);
            let (lo, hi) = (a.min(b), a.max(b));
            let within: Vec<bool> = (0..self.dimensions[d].values.len())
                .map(|i| i >= lo && i <= hi)
                .collect();
            kept[d] = Some(match kept[d].take() {
                Some(already) => already.iter().zip(&within).map(|(a, b)| *a && *b).collect(),
                None => within,
            });
        }
        if let Some(error) = query.error
            && error >= self.errors.len()
        {
            return Err(match self.errors.len() {
                0 => format!("'{}' has no failed key with a grouped error", self.id),
                n => format!(
                    "'{}' has {n} error group{}: --error takes 0 to {}",
                    self.id,
                    if n == 1 { "" } else { "s" },
                    n - 1
                ),
            });
        }
        Ok(kept)
    }

    /// The cells a query matches, in the query's order.
    fn matching(&self, query: &KeyQuery) -> Result<Vec<usize>, String> {
        if !self.known || self.dimensions.is_empty() {
            return Ok(Vec::new());
        }
        let kept = self.kept_values(query)?;
        let strides = self.strides();
        let sizes: Vec<usize> = self.dimensions.iter().map(|d| d.values.len()).collect();
        let text = query.text.to_lowercase();
        let mut cells: Vec<usize> = Vec::new();
        for (cell, code) in self.states.bytes().enumerate() {
            let code = code as char;
            if code == state::NONE || state::name(code).is_none() {
                continue;
            }
            if !query.states.is_empty() && !query.states.contains(&code) {
                continue;
            }
            if query.needs_run && !NEEDS_RUN.contains(&code) {
                continue;
            }
            if query.error.is_some()
                && self.error.get(&(cell as u32)).map(|e| *e as usize) != query.error
            {
                continue;
            }
            // Dimension filters narrow by coordinates before any key is spelled.
            if kept.iter().enumerate().any(|(d, keep)| {
                keep.as_ref()
                    .is_some_and(|keep| !keep[cell / strides[d] % sizes[d]])
            }) {
                continue;
            }
            if !text.is_empty() && !self.key_of(cell, &strides).to_lowercase().contains(&text) {
                continue;
            }
            cells.push(cell);
        }
        // Stable: keys that tie stay in key order. Keys with nothing to compare come last.
        fn most_first<T: Ord + Copy>(cells: &mut [usize], of: impl Fn(usize) -> Option<T>) {
            cells.sort_by(|a, b| match (of(*a), of(*b)) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        }
        match query.sort {
            KeySort::Key => {}
            KeySort::State => {
                let states = self.states.as_bytes();
                let rank = |cell: usize| {
                    state::NAMED
                        .iter()
                        .position(|(c, _)| *c as u8 == states[cell])
                        .unwrap_or(usize::MAX)
                };
                cells.sort_by_key(|cell| rank(*cell));
            }
            KeySort::Attempted => {
                most_first(&mut cells, |c| self.attempted_at.get(c).copied().flatten())
            }
            KeySort::Elapsed => {
                most_first(&mut cells, |c| self.elapsed_ms.get(c).copied().flatten())
            }
            KeySort::Size => most_first(&mut cells, |c| self.size_bytes.get(c).copied().flatten()),
            KeySort::Rows => most_first(&mut cells, |c| self.rows.get(c).copied().flatten()),
        }
        Ok(cells)
    }

    fn row_of(&self, cell: usize, strides: &[usize]) -> Option<KeyRow> {
        let code = self.states.as_bytes().get(cell).copied()? as char;
        let name = state::name(code)?;
        let at = |v: &Vec<Option<i64>>| v.get(cell).copied().flatten();
        Some(KeyRow {
            key: self.key_of(cell, strides),
            values: self
                .dimensions
                .iter()
                .zip(strides)
                .map(|(d, stride)| d.values[cell / stride % d.values.len()].clone())
                .collect(),
            state: name.to_string(),
            attempted_at: at(&self.attempted_at)
                .map(|offset| crate::time::rfc3339(self.attempted_base + offset)),
            elapsed_seconds: self
                .elapsed_ms
                .get(cell)
                .copied()
                .flatten()
                .map(|ms| ms as f64 / 1000.0),
            size_bytes: at(&self.size_bytes),
            rows: at(&self.rows),
            run_id: self
                .run
                .get(cell)
                .copied()
                .flatten()
                .and_then(|r| self.runs.get(r as usize).cloned()),
            error: self
                .error
                .get(&(cell as u32))
                .and_then(|e| self.errors.get(*e as usize))
                .map(|group| group.error.clone()),
        })
    }

    /// `cells` as `--partition` takes them: each dimension's values when the cells are every
    /// combination of them, each key otherwise (never their cross-product, which would name
    /// keys that were not asked for). `None` for no cells, or too many to spell.
    fn selection_of(&self, cells: &[usize]) -> Option<String> {
        if cells.is_empty() {
            return None;
        }
        let strides = self.strides();
        let present: Vec<BTreeSet<usize>> = self
            .dimensions
            .iter()
            .zip(&strides)
            .map(|(d, stride)| cells.iter().map(|c| c / stride % d.values.len()).collect())
            .collect();
        let product = present
            .iter()
            .try_fold(1usize, |n, v| n.checked_mul(v.len()));
        if product == Some(cells.len()) {
            if present.iter().map(BTreeSet::len).sum::<usize>() > SELECTION_MAX {
                return None;
            }
            let parts: Vec<String> = self
                .dimensions
                .iter()
                .zip(&present)
                .map(|(d, values)| {
                    let list: Vec<String> =
                        values.iter().map(|i| json_string(&d.values[*i])).collect();
                    format!("{}:[{}]", json_string(&d.name), list.join(","))
                })
                .collect();
            return Some(format!("{{{}}}", parts.join(",")));
        }
        if cells.len() > SELECTION_MAX {
            return None;
        }
        let mut ordered = cells.to_vec();
        ordered.sort_unstable();
        let keys: Vec<String> = ordered
            .iter()
            .map(|cell| {
                let parts: Vec<String> = self
                    .dimensions
                    .iter()
                    .zip(&strides)
                    .map(|(d, stride)| {
                        format!(
                            "{}:{}",
                            json_string(&d.name),
                            json_string(&d.values[cell / stride % d.values.len()])
                        )
                    })
                    .collect();
                format!("{{{}}}", parts.join(","))
            })
            .collect();
        Some(format!("[{}]", keys.join(",")))
    }

    /// The keys `query` matches, one page of them, with what the whole match adds up to. An
    /// error says why the query does not fit this node (a dimension or a range value it does
    /// not have, an error index past its errors).
    pub fn keys_page(&self, query: &KeyQuery) -> Result<KeysPage, String> {
        let cells = self.matching(query)?;
        let total = cells.len();
        let states = self.states.as_bytes();
        let needs_run = cells
            .iter()
            .filter(|c| NEEDS_RUN.contains(&(states[**c] as char)))
            .count();
        let strides = self.strides();
        let keys: Vec<KeyRow> = cells
            .iter()
            .skip(query.offset)
            .take(query.limit.unwrap_or(usize::MAX))
            .filter_map(|cell| self.row_of(*cell, &strides))
            .collect();
        let truncated = query.offset.min(total) + keys.len() < total;
        Ok(KeysPage {
            id: self.id.clone(),
            known: self.known,
            dimensions: self.dimensions.iter().map(|d| d.name.clone()).collect(),
            counts: self.counts.clone(),
            errors: self.errors.clone(),
            hint: truncated.then(|| {
                let next = query.offset + keys.len();
                if query.offset == 0 {
                    format!("pass --limit N for more, or --all for all {total} keys")
                } else {
                    format!("pass --offset {next} for the next keys, or --all for all {total} keys")
                }
            }),
            keys,
            total,
            truncated,
            offset: query.offset,
            needs_run,
            selection: self
                .selection_of(&cells)
                .or_else(|| self.run_selection(query, &cells)),
        })
    }

    /// The selection of a query that asks for the keys a run computes and narrows them by
    /// dimension alone, when its keys are too many and too scattered to spell: the filters'
    /// own rectangle (each filtered dimension's values, every value of the rest). A run of it
    /// computes the keys in it that need one, which are exactly the keys matched, so nothing
    /// more is computed than was asked for. With no dimension filter it names every key, by
    /// the dimension with the fewest values.
    fn run_selection(&self, query: &KeyQuery, cells: &[usize]) -> Option<String> {
        if cells.is_empty()
            || !query.needs_run
            || !query.states.is_empty()
            || query.error.is_some()
            || !query.text.is_empty()
        {
            return None;
        }
        let kept = self.kept_values(query).ok()?;
        let mut parts: Vec<(usize, Vec<&str>)> = self
            .dimensions
            .iter()
            .zip(&kept)
            .enumerate()
            .filter_map(|(d, (dimension, keep))| {
                let keep = keep.as_ref()?;
                Some((
                    d,
                    dimension
                        .values
                        .iter()
                        .zip(keep)
                        .filter(|(_, kept)| **kept)
                        .map(|(v, _)| v.as_str())
                        .collect(),
                ))
            })
            .collect();
        if parts.is_empty() {
            let (d, smallest) = self
                .dimensions
                .iter()
                .enumerate()
                .min_by_key(|(_, dimension)| dimension.values.len())?;
            parts.push((d, smallest.values.iter().map(String::as_str).collect()));
        }
        if parts.iter().map(|(_, values)| values.len()).sum::<usize>() > SELECTION_MAX {
            return None;
        }
        let parts: Vec<String> = parts
            .iter()
            .map(|(d, values)| {
                let list: Vec<String> = values.iter().map(|v| json_string(v)).collect();
                format!(
                    "{}:[{}]",
                    json_string(&self.dimensions[*d].name),
                    list.join(",")
                )
            })
            .collect();
        Some(format!("{{{}}}", parts.join(",")))
    }

    /// What is wrong and where: each error of the failed keys, the most keys first, then the
    /// blocked, stale and never-run keys.
    pub fn issues(&self) -> Vec<KeyIssue> {
        let strides = self.strides();
        let mut by_error: BTreeMap<Option<usize>, Vec<usize>> = BTreeMap::new();
        let mut by_state: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        const STATES: [char; 3] = [state::BLOCKED, state::STALE, state::NEVER_RUN];
        for (cell, code) in self.states.bytes().enumerate() {
            let code = code as char;
            if code == state::FAILED {
                let error = self.error.get(&(cell as u32)).map(|e| *e as usize);
                by_error.entry(error).or_default().push(cell);
            } else if let Some(i) = STATES.iter().position(|s| *s == code) {
                by_state.entry(i).or_default().push(cell);
            }
        }
        let places = |cells: &[usize]| -> Vec<IssuePlace> {
            self.dimensions
                .iter()
                .zip(&strides)
                .filter_map(|(d, stride)| {
                    let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
                    for cell in cells {
                        *counts.entry(cell / stride % d.values.len()).or_default() += 1;
                    }
                    let (first, last) = (*counts.keys().next()?, *counts.keys().next_back()?);
                    let distinct = counts.len();
                    if distinct == d.values.len() && distinct > 1 {
                        return None;
                    }
                    let words = if distinct <= 3 {
                        // The values with the most keys first.
                        let mut top: Vec<(usize, usize)> = counts.into_iter().collect();
                        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                        top.iter()
                            .map(|(i, _)| d.values[*i].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else if last - first == distinct - 1 {
                        format!("{} → {}", d.values[first], d.values[last])
                    } else {
                        format!(
                            "{} values, {} … {}",
                            grouped(distinct),
                            d.values[first],
                            d.values[last]
                        )
                    };
                    Some(IssuePlace {
                        dimension: d.name.clone(),
                        words,
                    })
                })
                .collect()
        };
        let issue = |code: char, error: Option<usize>, cells: &[usize]| KeyIssue {
            state: state::name(code).unwrap_or_default().to_string(),
            error,
            title: error
                .and_then(|e| self.errors.get(e))
                .map(|group| group.error.clone())
                .unwrap_or_else(|| state_title(code).to_string()),
            keys: cells.len(),
            places: places(cells),
            selection: self.selection_of(cells),
        };
        let mut errors: Vec<(Option<usize>, Vec<usize>)> = by_error.into_iter().collect();
        errors.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        let mut out: Vec<KeyIssue> = errors
            .iter()
            .map(|(error, cells)| issue(state::FAILED, *error, cells))
            .collect();
        for (i, cells) in &by_state {
            out.push(issue(STATES[*i], None, cells));
        }
        out
    }

    /// The node's keys in one picture: counts, issues and the grid's rows along its longest
    /// dimension, at most `limit` rows and [`OVERVIEW_CELLS_MAX`] cells.
    pub fn overview(&self, limit: Option<usize>) -> KeysOverview {
        let blocks = self.blocks();
        let rows_total = blocks.len();
        let along = self.dimensions.get(self.along());
        let width = along.map_or(1, |d| d.values.len().max(1));
        let shown = (OVERVIEW_CELLS_MAX / width)
            .max(1)
            .min(limit.unwrap_or(usize::MAX))
            .min(rows_total);
        let states = self.states.as_bytes();
        let along_index = self.along();
        let strides = self.strides();
        let rows: Vec<OverviewRow> = blocks
            .into_iter()
            .take(shown)
            .map(|block| OverviewRow {
                values: block.cells.first().map_or_else(Vec::new, |cell| {
                    self.dimensions
                        .iter()
                        .zip(&strides)
                        .enumerate()
                        .filter(|(d, _)| *d != along_index)
                        .map(|(_, (d, stride))| d.values[cell / stride % d.values.len()].clone())
                        .collect()
                }),
                states: block
                    .cells
                    .iter()
                    .map(|c| states.get(*c).map_or(state::NONE, |s| *s as char))
                    .collect(),
                label: block.label,
            })
            .collect();
        let truncated = rows.len() < rows_total;
        KeysOverview {
            id: self.id.clone(),
            chain: self.chain.clone(),
            known: self.known,
            dimensions: self.dimensions.clone(),
            counts: self.counts.clone(),
            needs_run: NEEDS_RUN.iter().map(|c| self.counts.of(*c)).sum(),
            errors: self.errors.clone(),
            issues: self.issues(),
            along: along.filter(|_| rows_total > 0).map(|d| d.name.clone()),
            hint: truncated
                .then(|| format!("pass --limit N for more, or --all for all {rows_total} rows")),
            rows,
            rows_total,
            truncated,
            last_attempt_at: self
                .attempted_at
                .iter()
                .flatten()
                .max()
                .map(|offset| crate::time::rfc3339(self.attempted_base + offset)),
        }
    }
}

impl KeysPage {
    /// This page with the next one after it: what a client that asked a server for every key
    /// prints, having read it a page at a time.
    pub fn extend(&mut self, next: KeysPage) {
        self.keys.extend(next.keys);
        self.truncated = next.truncated;
        self.hint = next.hint;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `day` (4 values) × `region` (2): cell = day * 2 + region.
    ///
    /// ```text
    ///        emea apac
    ///   d1    c    f(0)
    ///   d2    f(1) f(0)
    ///   d3    b    n
    ///   d4    s    -
    /// ```
    fn grid() -> PartitionGrid {
        PartitionGrid {
            id: "p.py:events".into(),
            chain: vec!["p.py:raw".into(), "p.py:events".into()],
            known: true,
            dimensions: vec![
                GridDimension {
                    name: "day".into(),
                    values: vec!["d1".into(), "d2".into(), "d3".into(), "d4".into()],
                },
                GridDimension {
                    name: "region".into(),
                    values: vec!["emea".into(), "apac".into()],
                },
            ],
            states: "cfffbns-".into(),
            counts: GridCounts {
                fresh: 1,
                failed: 3,
                blocked: 1,
                never_run: 1,
                stale: 1,
                ..Default::default()
            },
            attempted_base: 1_700_000_000,
            attempted_at: vec![
                Some(0),
                Some(50),
                Some(40),
                Some(60),
                None,
                None,
                Some(10),
                None,
            ],
            elapsed_ms: vec![
                Some(1500),
                Some(20),
                Some(900),
                Some(30),
                None,
                None,
                Some(10),
                None,
            ],
            size_bytes: vec![Some(2048), None, None, None, None, None, Some(10), None],
            rows: vec![Some(3), None, None, None, None, None, Some(7), None],
            run: vec![
                Some(0),
                Some(1),
                Some(1),
                Some(1),
                None,
                None,
                Some(0),
                None,
            ],
            error: [(1, 0), (2, 1), (3, 0)].into_iter().collect(),
            errors: vec![
                PartitionErrorGroup {
                    error: "ValueError: no rows for <region>".into(),
                    keys: 2,
                },
                PartitionErrorGroup {
                    error: "KeyError: 'price'".into(),
                    keys: 1,
                },
            ],
            runs: vec!["run-a".into(), "run-b".into()],
        }
    }

    fn query<'a>(change: impl FnOnce(&mut KeyQueryArgs<'a>)) -> KeyQuery {
        let mut args = KeyQueryArgs::default();
        change(&mut args);
        KeyQuery::parse(&args).unwrap()
    }

    fn keys(page: &KeysPage) -> Vec<&str> {
        page.keys.iter().map(|r| r.key.as_str()).collect()
    }

    #[test]
    fn every_key_is_listed_in_key_order_and_a_gap_is_not_a_key() {
        let page = grid().keys_page(&KeyQuery::default()).unwrap();
        assert_eq!(page.total, 7);
        assert_eq!(page.needs_run, 6);
        assert!(!page.truncated && page.hint.is_none());
        assert_eq!(keys(&page)[0], "day=d1,region=emea");
        assert_eq!(keys(&page)[6], "day=d4,region=emea");
        assert_eq!(page.keys[0].values, ["d1", "emea"]);
        assert_eq!(page.keys[0].state, "fresh");
        assert_eq!(
            page.keys[0].attempted_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(page.keys[1].run_id.as_deref(), Some("run-b"));
        assert_eq!(page.dimensions, ["day", "region"]);
    }

    #[test]
    fn a_state_a_value_a_range_an_error_and_text_each_narrow_and_all_must_hold() {
        let g = grid();
        let states = ["failed".to_string(), "never_run".to_string()];
        let by_state = g.keys_page(&query(|a| a.states = &states)).unwrap();
        assert_eq!(by_state.total, 4);
        let apac = ["region=apac".to_string()];
        let by_value = g.keys_page(&query(|a| a.values = &apac)).unwrap();
        assert_eq!(
            keys(&by_value),
            [
                "day=d1,region=apac",
                "day=d2,region=apac",
                "day=d3,region=apac"
            ]
        );
        // A range reads either way round, both ends included.
        let by_range = g
            .keys_page(&query(|a| a.range = Some("day=d3..d2")))
            .unwrap();
        assert_eq!(by_range.total, 4);
        let by_error = g.keys_page(&query(|a| a.error = Some(0))).unwrap();
        assert_eq!(
            keys(&by_error),
            ["day=d1,region=apac", "day=d2,region=apac"]
        );
        let by_text = g.keys_page(&query(|a| a.text = Some("D2"))).unwrap();
        assert_eq!(keys(&by_text), ["day=d2,region=emea", "day=d2,region=apac"]);
        let needing = g.keys_page(&query(|a| a.needs_run = true)).unwrap();
        assert_eq!((needing.total, needing.needs_run), (6, 6));
        let all = g
            .keys_page(&query(|a| {
                a.states = &states;
                a.values = &apac;
                a.range = Some("day=d2..d4");
                a.text = Some("apac");
            }))
            .unwrap();
        assert_eq!(keys(&all), ["day=d2,region=apac", "day=d3,region=apac"]);
        // `counts` is every key, whatever the query.
        assert_eq!(all.counts.failed, 3);
    }

    #[test]
    fn a_page_says_where_it_is_and_whether_more_follow() {
        let g = grid();
        let first = g.keys_page(&query(|a| a.limit = Some(3))).unwrap();
        assert_eq!(
            (first.keys.len(), first.total, first.truncated),
            (3, 7, true)
        );
        assert!(
            first
                .hint
                .as_deref()
                .unwrap()
                .contains("--all for all 7 keys")
        );
        let mut second = g
            .keys_page(&query(|a| {
                a.limit = Some(3);
                a.offset = 3;
            }))
            .unwrap();
        assert_eq!(second.offset, 3);
        assert!(second.truncated);
        assert!(second.hint.as_deref().unwrap().contains("--offset 6"));
        let last = g
            .keys_page(&query(|a| {
                a.limit = Some(3);
                a.offset = 6;
            }))
            .unwrap();
        assert_eq!((last.keys.len(), last.truncated), (1, false));
        // A client that reads every page prints what one unbounded read prints.
        let mut whole = first;
        second.keys.truncate(3);
        whole.extend(second);
        whole.extend(last);
        assert_eq!(
            keys(&whole),
            keys(&g.keys_page(&KeyQuery::default()).unwrap())
        );
        assert!(!whole.truncated && whole.hint.is_none());
        let past = g.keys_page(&query(|a| a.offset = 50)).unwrap();
        assert!(past.keys.is_empty() && !past.truncated && past.total == 7);
    }

    #[test]
    fn each_order_puts_the_most_first_and_keys_without_a_value_last() {
        let g = grid();
        let order = |name: &'static str| -> Vec<String> {
            g.keys_page(&query(|a| a.sort = Some(name)))
                .unwrap()
                .keys
                .into_iter()
                .map(|r| r.key)
                .collect()
        };
        let by_state = order("state");
        assert_eq!(by_state[0], "day=d1,region=apac");
        assert_eq!(by_state[3], "day=d3,region=emea");
        assert_eq!(by_state[6], "day=d1,region=emea");
        assert_eq!(order("attempted")[0], "day=d2,region=apac");
        assert_eq!(order("attempted")[6], "day=d3,region=apac");
        assert_eq!(order("elapsed")[0], "day=d1,region=emea");
        assert_eq!(order("size")[1], "day=d4,region=emea");
        assert_eq!(order("rows")[0], "day=d4,region=emea");
        let err = KeySort::parse("newest").unwrap_err();
        assert!(err.contains("unknown key order 'newest'") && err.contains("attempted"));
    }

    #[test]
    fn a_query_that_does_not_fit_the_node_says_what_the_node_has() {
        let g = grid();
        let zone = ["zone=a".to_string()];
        let err = g.keys_page(&query(|a| a.values = &zone)).unwrap_err();
        assert!(
            err.contains("no dimension 'zone'") && err.contains("day, region"),
            "{err}"
        );
        let err = g
            .keys_page(&query(|a| a.range = Some("day=d1..d9")))
            .unwrap_err();
        assert!(err.contains("no day 'd9'"), "{err}");
        let err = g.keys_page(&query(|a| a.error = Some(2))).unwrap_err();
        assert!(err.contains("--error takes 0 to 1"), "{err}");
        // A value the dimension does not have matches nothing: values come and go.
        let gone = ["region=latam".to_string()];
        assert_eq!(g.keys_page(&query(|a| a.values = &gone)).unwrap().total, 0);
        for bad in ["region", "=apac", "region="] {
            let spec = [bad.to_string()];
            let args = KeyQueryArgs {
                values: &spec,
                ..Default::default()
            };
            assert!(
                KeyQuery::parse(&args)
                    .unwrap_err()
                    .contains("DIMENSION=VALUE")
            );
        }
        let args = KeyQueryArgs {
            range: Some("day=d1"),
            ..Default::default()
        };
        assert!(
            KeyQuery::parse(&args)
                .unwrap_err()
                .contains("DIMENSION=FROM..TO")
        );
        let cached = ["cached".to_string()];
        let args = KeyQueryArgs {
            states: &cached,
            ..Default::default()
        };
        let err = KeyQuery::parse(&args).unwrap_err();
        assert!(err.contains("unknown key state 'cached'") && err.contains("never_run"));
    }

    #[test]
    fn a_selection_is_a_rectangle_when_the_keys_are_one_and_each_key_otherwise() {
        let g = grid();
        // d1 and d2 of apac: every combination of their values.
        let page = g.keys_page(&query(|a| a.error = Some(0))).unwrap();
        assert_eq!(
            page.selection.as_deref(),
            Some(r#"{"day":["d1","d2"],"region":["apac"]}"#)
        );
        // Scattered keys are named one by one, never widened to their cross-product.
        let failed = ["failed".to_string()];
        let page = g.keys_page(&query(|a| a.states = &failed)).unwrap();
        assert_eq!(
            page.selection.as_deref(),
            Some(
                r#"[{"day":"d1","region":"apac"},{"day":"d2","region":"emea"},{"day":"d2","region":"apac"}]"#
            )
        );
        let none = ["pinned".to_string()];
        assert!(
            g.keys_page(&query(|a| a.states = &none))
                .unwrap()
                .selection
                .is_none()
        );
    }

    #[test]
    fn too_many_scattered_keys_to_run_are_named_by_the_filters_rectangle_or_not_at_all() {
        // 120 × 100 keys, every other one never ran: 6,000 scattered keys, past what a
        // selection spells key by key.
        let (a, b) = (120, 100);
        let g = PartitionGrid {
            dimensions: vec![
                GridDimension {
                    name: "a".into(),
                    values: (0..a).map(|i| format!("a{i:03}")).collect(),
                },
                GridDimension {
                    name: "b".into(),
                    values: (0..b).map(|i| format!("b{i:03}")).collect(),
                },
            ],
            states: (0..a * b)
                .map(|cell| {
                    if (cell / b + cell % b) % 2 == 0 {
                        'n'
                    } else {
                        'c'
                    }
                })
                .collect(),
            error: Default::default(),
            errors: vec![],
            ..grid()
        };
        // The keys a run computes, unfiltered: every key, named by the shorter dimension.
        let all = g.keys_page(&query(|q| q.needs_run = true)).unwrap();
        assert_eq!(all.total, 6_000);
        let selection: serde_json::Value =
            serde_json::from_str(all.selection.as_deref().unwrap()).unwrap();
        assert_eq!(selection["b"].as_array().unwrap().len(), 100);
        assert!(selection.get("a").is_none());
        // Narrowed by a dimension: that dimension's values, every value of the other.
        let range = g
            .keys_page(&query(|q| {
                q.needs_run = true;
                q.range = Some("a=a010..a119");
            }))
            .unwrap();
        assert_eq!(range.total, 5_500);
        let selection: serde_json::Value =
            serde_json::from_str(range.selection.as_deref().unwrap()).unwrap();
        assert_eq!(selection["a"].as_array().unwrap().len(), 110);
        assert_eq!(selection["a"][0], "a010");
        // A state filter keeps some of what a run of the rectangle would compute: no selection
        // rather than one that computes more than was asked for.
        let never = ["never_run".to_string()];
        assert!(
            g.keys_page(&query(|q| q.states = &never))
                .unwrap()
                .selection
                .is_none()
        );
        assert!(
            g.keys_page(&query(|q| {
                q.needs_run = true;
                q.text = Some("=b0");
            }))
            .unwrap()
            .selection
            .is_none()
        );
        // Few enough keys are still spelled one by one, whatever the query.
        let few = g
            .keys_page(&query(|q| {
                q.needs_run = true;
                q.range = Some("a=a000..a001");
            }))
            .unwrap();
        assert!(few.selection.as_deref().unwrap().starts_with("[{"));
    }

    #[test]
    fn issues_group_failed_keys_by_error_then_the_states_and_say_where_each_sits() {
        let issues = grid().issues();
        let seen: Vec<(&str, Option<usize>, usize)> = issues
            .iter()
            .map(|i| (i.state.as_str(), i.error, i.keys))
            .collect();
        assert_eq!(
            seen,
            [
                ("failed", Some(0), 2),
                ("failed", Some(1), 1),
                ("blocked", None, 1),
                ("stale", None, 1),
                ("never_run", None, 1),
            ]
        );
        assert_eq!(issues[0].title, "ValueError: no rows for <region>");
        assert_eq!(
            issues[0].places,
            [
                IssuePlace {
                    dimension: "day".into(),
                    words: "d1, d2".into()
                },
                IssuePlace {
                    dimension: "region".into(),
                    words: "apac".into()
                },
            ]
        );
        assert_eq!(
            issues[2].title,
            "Waiting on the same keys of an input that failed"
        );
        assert_eq!(
            issues[2].selection.as_deref(),
            Some(r#"{"day":["d3"],"region":["emea"]}"#)
        );
    }

    #[test]
    fn a_long_stretch_reads_as_its_ends_and_scattered_values_as_a_count() {
        let n = 12;
        let mut g = PartitionGrid {
            dimensions: vec![GridDimension {
                name: "day".into(),
                values: (0..n).map(|i| format!("v{i:02}")).collect(),
            }],
            states: "cffffcccfcfc".into(),
            error: Default::default(),
            errors: vec![],
            ..grid()
        };
        assert_eq!(g.issues()[0].places[0].words, "6 values, v01 … v10");
        g.states = "cffffccccccc".into();
        assert_eq!(g.issues()[0].places[0].words, "v01 → v04");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }

    #[test]
    fn the_overview_has_the_rows_the_grid_prints_and_bounds_them() {
        let g = grid();
        let overview = g.overview(None);
        assert_eq!(overview.along.as_deref(), Some("day"));
        assert_eq!(overview.needs_run, 6);
        let rows: Vec<(&str, &str)> = overview
            .rows
            .iter()
            .map(|r| (r.label.as_str(), r.states.as_str()))
            .collect();
        assert_eq!(rows, [("region=emea", "cfbs"), ("region=apac", "ffn-")]);
        assert_eq!(overview.rows[1].values, ["apac"]);
        assert_eq!(
            overview.last_attempt_at.as_deref(),
            Some("2023-11-14T22:14:20Z")
        );
        assert!(!overview.truncated && overview.hint.is_none());
        let one = g.overview(Some(1));
        assert_eq!(
            (one.rows.len(), one.rows_total, one.truncated),
            (1, 2, true)
        );
        assert!(one.hint.as_deref().unwrap().contains("all 2 rows"));
    }

    #[test]
    fn keys_that_are_not_known_yet_match_nothing_whatever_is_asked() {
        let g = PartitionGrid {
            known: false,
            dimensions: vec![],
            states: String::new(),
            counts: GridCounts::default(),
            error: Default::default(),
            errors: vec![],
            ..grid()
        };
        let zone = ["zone=a".to_string()];
        let page = g.keys_page(&query(|a| a.values = &zone)).unwrap();
        assert!(!page.known && page.total == 0 && page.selection.is_none());
        assert!(g.overview(None).along.is_none());
    }
}
