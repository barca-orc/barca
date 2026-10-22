//! `barca sql` — query cached artifacts with DuckDB.
//!
//! Every node with a result on disk becomes a view: assets and sensors at their cached artifact
//! (or, when stale, their last successful one, with a note), tasks at their last result, and a
//! partitioned asset as one view over its current known keys with a `partition` column.
//! The view uses the declared name, or the function name when unset. Duplicate names use
//! full node IDs. The query runs
//! in `python -m barca._sql`, an in-memory DuckDB that opens artifact files only: user code is
//! never imported and nothing is recorded. An artifact in remote storage is fetched, when the
//! query names its view, into [`CACHE_DIR`] and queried from there.
//!
//! Two steps: [`Catalog`] resolves which result each view reads (on this machine from its
//! metadata DB, or on a server from its ledger: `GET /sql/views` serves [`catalog_from_dag`]),
//! and [`run`] queries it here. barca's own records are tables in the `barca` schema
//! ([`tables`]): `barca.runs`, `barca.materializations` and `barca.nodes`.

use crate::BarcaError;
use crate::config::ResolvedConfig;
use crate::status;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub mod tables;

/// Local copies of remote artifacts, laid out as `<scheme>/<bucket>/<path>`. Every environment
/// shares it: the object's full URI is the key.
pub const CACHE_DIR: &str = ".barca/sql-cache";

/// One file of a view: a stored result, and for a partitioned asset its key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViewFile {
    /// Where the result is stored (a store path with a remote store).
    pub path: String,
    /// The partition key (`week=w1`); `null` for an unpartitioned node.
    pub partition: Option<String>,
    /// SHA-256 of the result's bytes as recorded, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

/// A queryable view: a node and the results it reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct View {
    /// The view's name in SQL: the node's name, or its id when two nodes share the name.
    pub name: String,
    /// Node id.
    pub node: String,
    /// `asset`, `task` or `sensor`.
    pub kind: String,
    pub partitioned: bool,
    /// `parquet`, `csv`, `json` or `pickle`.
    pub format: String,
    pub files: Vec<ViewFile>,
    /// The code or inputs changed since the result was computed: the view shows the last one.
    pub stale: bool,
    /// The run hash the node's code and inputs give now, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_hash: Option<String>,
}

/// A node with no queryable result: no view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Missing {
    pub name: String,
    pub node: String,
    pub kind: String,
    pub partitioned: bool,
    /// The command that produces it (`barca get revenue`).
    pub command: String,
    /// A partitioned node whose current keys are not known until a source materializes: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys_unknown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_hash: Option<String>,
}

/// Several nodes with one name: their views are named by id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Duplicate {
    pub name: String,
    pub ids: Vec<String>,
}

/// Which result every view reads: `GET /sql/views`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Catalog {
    pub views: Vec<View>,
    pub missing: Vec<Missing>,
    pub duplicates: Vec<Duplicate>,
}

/// Where the catalog came from, for the notes and errors.
#[derive(Debug, Clone)]
pub enum Origin {
    /// This machine's metadata DB.
    Local,
    /// A server's ledger. `pending` holds this machine's results whose post to it is pending
    /// (with a configured server; empty for `--server`).
    Server {
        url: String,
        pending: Vec<status::PendingPost>,
    },
}

/// The query's result: column names in order, one object per row, and how many rows the query
/// returns in all (`truncated` when only the first `limit` are in `rows`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlResult {
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub total: u64,
    pub truncated: bool,
    /// Notes for stderr (stale views, renamed views); not part of the JSON result.
    #[serde(skip)]
    pub notes: Vec<String>,
}

#[derive(Deserialize)]
struct HelperError {
    kind: String,
    message: String,
    table: Option<String>,
}

/// Why a remote view could not be fetched: a missing fsspec driver (`driver`, the caller's
/// environment) or anything else (`fetch`: credentials, the network, a missing object).
#[derive(Deserialize, Clone)]
struct Unreachable {
    kind: String,
    reason: String,
}

#[derive(Deserialize, Default)]
struct Fetched {
    #[serde(default)]
    files: u64,
    #[serde(default)]
    bytes: u64,
}

#[derive(Deserialize)]
struct HelperOutput {
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    rows: Vec<serde_json::Value>,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    unavailable: BTreeMap<String, String>,
    #[serde(default)]
    unreachable: BTreeMap<String, Unreachable>,
    #[serde(default)]
    fetched: Fetched,
    error: Option<HelperError>,
}

fn format_of(path: &str) -> String {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("parquet") => "parquet",
        Some("csv") => "csv",
        Some("json") => "json",
        Some("pkl") | Some("pickle") => "pickle",
        _ => "unknown",
    }
    .to_string()
}

/// Whether `query` names the view `name`: the name as a whole word, in any case (the same rule
/// `barca._sql` uses to decide which remote views to fetch).
pub fn names(query: &str, name: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let (query, name) = (query.to_lowercase(), name.to_lowercase());
    query.match_indices(&name).any(|(at, _)| {
        let before = query[..at].chars().next_back();
        let after = query[at + name.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// Run `query` on this machine over the views of every node in the project (or in
/// `file_args`) and barca's tables, from this machine's metadata DB.
pub async fn sql(
    cfg: &ResolvedConfig,
    query: &str,
    file_args: &[String],
    python: &std::path::Path,
    limit: Option<usize>,
) -> Result<SqlResult, BarcaError> {
    let (st, membership) =
        status::status_with_membership(cfg, &[], file_args, python, 0, false).await?;
    let catalog = resolve(cfg, &st, &membership, |_| true).await?;
    let tables = tables::local(cfg, query, &st.nodes).await?;
    run(
        cfg,
        query,
        limit,
        catalog,
        tables,
        Vec::new(),
        python,
        &Origin::Local,
    )
    .await
}

/// The views of a loaded DAG (`barca serve`'s), for the nodes `keep` accepts (by node id).
pub async fn catalog_from_dag(
    cfg: &ResolvedConfig,
    dag: &crate::dag::Dag,
    python: &std::path::Path,
    keep: impl Fn(&str) -> bool,
) -> Result<Catalog, BarcaError> {
    let (st, membership) =
        status::status_from_dag_with_membership(cfg, &[], dag, python, 0, false).await?;
    resolve(cfg, &st, &membership, keep).await
}

/// Which result each node's view reads.
async fn resolve(
    cfg: &ResolvedConfig,
    st: &status::StatusResult,
    membership: &crate::cache::ExpandedMembership,
    keep: impl Fn(&str) -> bool,
) -> Result<Catalog, BarcaError> {
    let nodes: Vec<&status::NodeStatus> = st.nodes.iter().filter(|n| keep(&n.id)).collect();
    let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &nodes {
        by_name
            .entry(n.name.as_str())
            .or_default()
            .push(n.id.as_str());
    }

    let mut catalog = Catalog::default();
    for n in nodes {
        let view_name = if by_name[n.name.as_str()].len() > 1 {
            n.id.clone()
        } else {
            n.name.clone()
        };
        let get = if n.kind == "task" { "run" } else { "get" };
        let missing = |keys_unknown: Option<String>| Missing {
            name: view_name.clone(),
            node: n.id.clone(),
            kind: n.kind.clone(),
            partitioned: n.partitioned,
            command: format!("barca {get} {}", n.name),
            keys_unknown,
            run_hash: n.cache.run_hash.clone(),
        };
        let view = |format: String, files: Vec<ViewFile>, stale: bool| View {
            name: view_name.clone(),
            node: n.id.clone(),
            kind: n.kind.clone(),
            partitioned: n.partitioned,
            format,
            files,
            stale,
            run_hash: n.cache.run_hash.clone(),
        };
        if n.partitioned {
            let Some(current) = membership.get(&n.id) else {
                catalog.missing.push(missing(Some(n.cache.detail.clone())));
                continue;
            };
            let mut files = if Path::new(&cfg.db_path).exists() {
                crate::db::partition_artifacts_for_config(cfg, &n.id).await?
            } else {
                Vec::new()
            };
            files.retain(|a| current.contains(&a.node_id));
            if files.is_empty() {
                catalog.missing.push(missing(None));
                continue;
            }
            let format = files[0].format.clone();
            let files = files
                .into_iter()
                .map(|a| ViewFile {
                    partition: a
                        .node_id
                        .strip_prefix(&n.id)
                        .and_then(|suffix| suffix.strip_prefix('['))
                        .and_then(|suffix| suffix.strip_suffix(']'))
                        .map(str::to_string),
                    path: a.path,
                    content_hash: a.content_hash,
                })
                .collect();
            catalog
                .views
                .push(view(format, files, n.cache.state != "cached"));
            continue;
        }
        let last = n.last_materialization.as_ref();
        let (path, format, stale) = match (&n.cache.artifact, last) {
            (Some(path), _) if n.cache.state == "cached" => {
                let format = last
                    .filter(|m| m.artifact.as_deref() == Some(path.as_str()))
                    .and_then(|m| m.format.clone())
                    .unwrap_or_else(|| format_of(path));
                (path.clone(), format, false)
            }
            (_, Some(m))
                if m.status == "success"
                    && m.artifact
                        .as_deref()
                        .is_some_and(|path| cfg.allows_artifact(path)) =>
            {
                let path = m.artifact.clone().unwrap_or_default();
                let format = m.format.clone().unwrap_or_else(|| format_of(&path));
                (path, format, n.cache.state == "stale")
            }
            _ => {
                catalog.missing.push(missing(None));
                continue;
            }
        };
        let content_hash = last
            .filter(|m| m.artifact.as_deref() == Some(path.as_str()))
            .and_then(|m| m.content_hash.clone());
        catalog.views.push(view(
            format,
            vec![ViewFile {
                path,
                partition: None,
                content_hash,
            }],
            stale,
        ));
    }

    let mut dup: Vec<(&str, &Vec<&str>)> = by_name
        .iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(name, ids)| (*name, ids))
        .collect();
    dup.sort();
    catalog.duplicates = dup
        .into_iter()
        .map(|(name, ids)| Duplicate {
            name: name.to_string(),
            ids: ids.iter().map(|id| id.to_string()).collect(),
        })
        .collect();
    Ok(catalog)
}

/// Run `query` here over `catalog`'s views and barca's `tables`. `notes` are printed before
/// the query's own.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    cfg: &ResolvedConfig,
    query: &str,
    limit: Option<usize>,
    catalog: Catalog,
    tables: Vec<tables::Table>,
    mut notes: Vec<String>,
    python: &std::path::Path,
    origin: &Origin,
) -> Result<SqlResult, BarcaError> {
    let Catalog {
        views,
        missing,
        duplicates,
    } = catalog;
    let pending = |node: &str, partitioned: bool, run_hash: Option<&str>| match origin {
        Origin::Server { pending, .. } => {
            status::pending_for(node, partitioned, run_hash, pending).is_some()
        }
        Origin::Local => false,
    };
    for d in &duplicates {
        let ids: Vec<String> = d.ids.iter().map(|id| format!("\"{id}\"")).collect();
        notes.push(format!(
            "several nodes are named '{}': their views are named by id: {}",
            d.name,
            ids.join(", ")
        ));
    }
    for v in &views {
        if !names(query, &v.name) {
            continue;
        }
        let name = &v.name;
        if v.stale {
            notes.push(format!(
                "'{name}' is stale (its code or inputs changed since it ran): the view shows its \
                 last result. Run `barca get {name}` to refresh it."
            ));
        }
        if v.kind == "asset" && pending(&v.node, v.partitioned, v.run_hash.as_deref()) {
            notes.push(format!(
                "'{name}' has a result computed here that the server has not recorded yet \
                 (unsynced): the view shows the server's. `barca sql --local` queries this \
                 machine's; the next get or run against the server posts it."
            ));
        }
    }

    let request = serde_json::json!({
        "query": query,
        "limit": limit,
        "cache_dir": CACHE_DIR,
        "views": views,
        "tables": tables,
    });
    let out = run_helper(python, cfg, &request).await?;
    if out.fetched.files > 0 {
        let n = out.fetched.files;
        notes.push(format!(
            "fetched {n} remote artifact{} ({}) into {CACHE_DIR}/",
            if n == 1 { "" } else { "s" },
            crate::gc::size::human(out.fetched.bytes)
        ));
    }
    for t in &tables {
        if let Some(note) = &t.note
            && names(query, t.name)
        {
            notes.push(note.clone());
        }
    }
    let Some(err) = out.error else {
        // The results the query read: `last_read_at` moves, so the cache size limit evicts them
        // after results nobody reads (RFC-0012).
        let read: Vec<String> = views
            .iter()
            .filter(|v| names(query, &v.name))
            .flat_map(|v| v.files.iter().map(|f| f.path.clone()))
            .collect();
        crate::db::touch_read(&cfg.db_path, &read).await;
        return Ok(SqlResult {
            columns: out.columns,
            rows: out.rows,
            total: out.total,
            truncated: out.truncated,
            notes,
        });
    };
    let available = || {
        let mut names: Vec<&str> = views
            .iter()
            .map(|v| v.name.as_str())
            .filter(|n| !out.unavailable.contains_key(*n))
            .collect();
        names.sort();
        let tables: Vec<String> = tables
            .iter()
            .map(|t| format!("{}.{}", t.schema, t.name))
            .collect();
        let tables = if tables.is_empty() {
            String::new()
        } else {
            format!("\nbarca's tables: {}", tables.join(", "))
        };
        if names.is_empty() {
            let none = match origin {
                Origin::Local => {
                    "No views are available: nothing has a cached result yet. Run `barca get` first."
                        .to_string()
                }
                Origin::Server { url, .. } => format!(
                    "No views are available: the server at {url} has no result yet. Run \
                     `barca get` first."
                ),
            };
            format!("{none}{tables}")
        } else {
            format!("Views: {}{tables}", names.join(", "))
        }
    };
    Err(match err.kind.as_str() {
        "missing_table" => {
            let table = err.table.unwrap_or_default();
            let unavailable: HashMap<String, String> =
                out.unavailable.clone().into_iter().collect();
            if let Some(m) = missing.iter().find(|m| m.name.eq_ignore_ascii_case(&table)) {
                let (name, cmd) = (&m.name, &m.command);
                if let Some(why) = &m.keys_unknown {
                    return Err(BarcaError::Usage(format!(
                        "'{name}' has no current partition view: {why}\n\
                         Partition keys require source materialization. Run `{cmd}` first, then re-run this query."
                    )));
                }
                match origin {
                    Origin::Local => BarcaError::Usage(format!(
                        "'{name}' has no result yet, so there is no view for it\n\
                         Run `{cmd}` first, then re-run this query."
                    )),
                    Origin::Server { url, .. }
                        if pending(&m.node, m.partitioned, m.run_hash.as_deref()) =>
                    {
                        BarcaError::Usage(format!(
                            "'{name}' has no result on the server at {url}: this machine \
                             computed one the server has not recorded yet (unsynced)\n\
                             Query this machine's result with `barca sql --local`, or run \
                             `{cmd}` to post it, then re-run this query."
                        ))
                    }
                    Origin::Server { url, .. } => BarcaError::Usage(format!(
                        "'{name}' has no result on the server at {url} yet, so there is no view \
                         for it\n\
                         Run `{cmd}` first, then re-run this query."
                    )),
                }
            } else if let Some((name, why)) = out
                .unreachable
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&table))
            {
                let msg = format!(
                    "'{name}' is in remote storage and could not be fetched: {}",
                    why.reason
                );
                if why.kind == "driver" {
                    BarcaError::Usage(msg)
                } else {
                    BarcaError::Other(format!(
                        "{msg}\nCheck the credentials and network this machine uses for the \
                         remote store (`barca docs remote`), then re-run this query."
                    ))
                }
            } else if let Some((name, why)) = unavailable
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&table))
            {
                BarcaError::Usage(format!(
                    "'{name}' cannot be queried: {why}\n\
                     Return a DataFrame, Arrow table, DuckDB relation or list of dicts to store \
                     it as parquet, CSV or json."
                ))
            } else {
                let meant = crate::suggest::suggestion(
                    &table,
                    views
                        .iter()
                        .map(|v| v.name.as_str())
                        .filter(|n| !out.unavailable.contains_key(*n)),
                )
                .map(|m| format!("{m}\n"))
                .unwrap_or_default();
                BarcaError::Usage(format!("no view named '{table}'\n{meant}{}", available()))
            }
        }
        "no_duckdb" => BarcaError::Usage(format!(
            "{}\nInstall it: `uv add 'barca[sql]'`.",
            err.message
        )),
        _ => BarcaError::Usage(format!(
            "{}\n{}",
            err.message.lines().next().unwrap_or("SQL error"),
            available()
        )),
    })
}

async fn run_helper(
    python: &Path,
    cfg: &ResolvedConfig,
    request: &serde_json::Value,
) -> Result<HelperOutput, BarcaError> {
    use crate::helper_proc::JsonFailed;
    let cmd = crate::helper_proc::python_module(
        python,
        "barca._sql",
        cfg.storage_options_json.as_deref(),
    );
    crate::helper_proc::run_json(cmd, request, None)
        .await
        .map_err(|failed| {
            BarcaError::Other(match failed {
                JsonFailed::Spawn(e) => format!("cannot run {}: {e}", python.display()),
                JsonFailed::Io(e) => format!("sql helper: {e}"),
                JsonFailed::TimedOut(limit) => {
                    format!("sql helper: no answer within {}s", limit.as_secs())
                }
                JsonFailed::Exited(stderr) => format!(
                    "the sql helper failed: {}",
                    crate::helper_proc::last_line(&stderr)
                ),
                JsonFailed::Invalid(e) => format!("the sql helper returned invalid JSON: {e}"),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_come_from_the_extension() {
        assert_eq!(format_of(".barca/artifacts/a/h.parquet"), "parquet");
        assert_eq!(format_of("x.json"), "json");
        assert_eq!(format_of("x.pkl"), "pickle");
        assert_eq!(format_of("x.csv"), "csv");
        assert_eq!(format_of("x"), "unknown");
    }
}

#[cfg(test)]
mod names_tests {
    #[test]
    fn a_view_is_named_as_a_whole_word() {
        assert!(super::names("select * from total", "total"));
        assert!(super::names("SELECT * FROM Total t", "total"));
        assert!(!super::names("select * from totals", "total"));
        assert!(!super::names("select subtotal from x", "total"));
        assert!(super::names("select * from \"p.py:total\"", "p.py:total"));
    }
}
