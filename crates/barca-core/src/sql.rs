//! `barca sql` — query cached artifacts with DuckDB.
//!
//! Every node with a result on disk becomes a view: assets and sensors at their cached artifact
//! (or, when stale, their last successful one, with a note), tasks at their last result, and a
//! partitioned asset as one view over its keys with a `partition` column. The view is named after
//! the function, or after the full node id when two nodes share a function name. The query runs
//! in `python -m barca._sql`, an in-memory DuckDB that opens artifact files only: user code is
//! never imported and nothing is recorded. An artifact in remote storage is fetched, when the
//! query names its view, into [`CACHE_DIR`] and queried from there.

use crate::BarcaError;
use crate::config::ResolvedConfig;
use crate::status;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Local copies of remote artifacts, laid out as `<scheme>/<bucket>/<path>`. Every environment
/// shares it: the object's full URI is the key.
pub const CACHE_DIR: &str = ".barca/sql-cache";

#[derive(Debug, Clone, Serialize)]
struct ViewFile {
    path: String,
    partition: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct View {
    name: String,
    node: String,
    format: String,
    files: Vec<ViewFile>,
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

fn human_size(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if kb < 1024.0 {
        format!("{kb:.1} KB")
    } else {
        format!("{:.1} MB", kb / 1024.0)
    }
}

fn format_of(path: &str) -> String {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("parquet") => "parquet",
        Some("json") => "json",
        Some("pkl") | Some("pickle") => "pickle",
        _ => "unknown",
    }
    .to_string()
}

/// Run `query` over the views of every node in the project (or in `file_args`).
pub async fn sql(
    cfg: &ResolvedConfig,
    query: &str,
    file_args: &[String],
    python: &std::path::Path,
    limit: Option<usize>,
) -> Result<SqlResult, BarcaError> {
    let st = status::status(cfg, &[], file_args, python, 0, false).await?;

    let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &st.nodes {
        by_name
            .entry(n.name.as_str())
            .or_default()
            .push(n.id.as_str());
    }

    let mut views: Vec<View> = Vec::new();
    // View name -> (command that would produce it, node name) for nodes with no result yet.
    let mut missing: HashMap<String, String> = HashMap::new();
    let mut notes: Vec<String> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    for n in &st.nodes {
        let view_name = if by_name[n.name.as_str()].len() > 1 {
            n.id.clone()
        } else {
            n.name.clone()
        };
        let get = if n.kind == "task" { "run" } else { "get" };
        if n.partitioned {
            let files = if Path::new(&cfg.db_path).exists() {
                crate::db::partition_artifacts_for_config(cfg, &n.id).await?
            } else {
                Vec::new()
            };
            if files.is_empty() {
                missing.insert(view_name, format!("barca {get} {}", n.name));
                continue;
            }
            if n.cache.state != "cached" {
                stale.push(view_name.clone());
            }
            views.push(View {
                name: view_name,
                node: n.id.clone(),
                format: files[0].2.clone(),
                files: files
                    .into_iter()
                    .map(|(node_id, path, _)| ViewFile {
                        path,
                        partition: node_id
                            .find('[')
                            .map(|i| node_id[i + 1..].trim_end_matches(']').to_string()),
                    })
                    .collect(),
            });
            continue;
        }
        let (path, format) = match (&n.cache.artifact, &n.last_materialization) {
            (Some(path), _) if n.cache.state == "cached" => (path.clone(), format_of(path)),
            (_, Some(m))
                if m.status == "success"
                    && m.artifact
                        .as_deref()
                        .is_some_and(|path| cfg.allows_artifact(path)) =>
            {
                let path = m.artifact.clone().unwrap_or_default();
                if n.cache.state == "stale" {
                    stale.push(view_name.clone());
                }
                let format = m.format.clone().unwrap_or_else(|| format_of(&path));
                (path, format)
            }
            _ => {
                missing.insert(view_name, format!("barca {get} {}", n.name));
                continue;
            }
        };
        views.push(View {
            name: view_name,
            node: n.id.clone(),
            format,
            files: vec![ViewFile {
                path,
                partition: None,
            }],
        });
    }

    let mut dup: Vec<&str> = by_name
        .iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(name, _)| *name)
        .collect();
    dup.sort();
    for name in dup {
        let ids: Vec<String> = by_name[name].iter().map(|id| format!("\"{id}\"")).collect();
        notes.push(format!(
            "several nodes are named '{name}': their views are named by id: {}",
            ids.join(", ")
        ));
    }
    let lower = query.to_lowercase();
    for name in &stale {
        if lower.contains(&name.to_lowercase()) {
            notes.push(format!(
                "'{name}' is stale (its code or inputs changed since it ran): the view shows its \
                 last result. Run `barca get {name}` to refresh it."
            ));
        }
    }

    let request = serde_json::json!({
        "query": query,
        "limit": limit,
        "cache_dir": CACHE_DIR,
        "views": views,
    });
    let out = run_helper(python, cfg, &request).await?;
    if out.fetched.files > 0 {
        let n = out.fetched.files;
        notes.push(format!(
            "fetched {n} remote artifact{} ({}) into {CACHE_DIR}/",
            if n == 1 { "" } else { "s" },
            human_size(out.fetched.bytes)
        ));
    }
    let Some(err) = out.error else {
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
        if names.is_empty() {
            "No views are available: nothing has a cached result yet. Run `barca get` first."
                .to_string()
        } else {
            format!("Views: {}", names.join(", "))
        }
    };
    Err(match err.kind.as_str() {
        "missing_table" => {
            let table = err.table.unwrap_or_default();
            let find = |m: &HashMap<String, String>| {
                m.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(&table))
                    .map(|(k, v)| (k.clone(), v.clone()))
            };
            let unavailable: HashMap<String, String> =
                out.unavailable.clone().into_iter().collect();
            if let Some((name, cmd)) = find(&missing) {
                BarcaError::Usage(format!(
                    "'{name}' has no result yet, so there is no view for it\n\
                     Run `{cmd}` first, then re-run this query."
                ))
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
            } else if let Some((name, why)) = find(&unavailable) {
                BarcaError::Usage(format!(
                    "'{name}' cannot be queried: {why}\n\
                     Return a DataFrame, Arrow table, DuckDB relation or list of dicts to store \
                     it as parquet or json."
                ))
            } else {
                BarcaError::Usage(format!("no view named '{table}'\n{}", available()))
            }
        }
        "no_duckdb" => BarcaError::Usage(format!(
            "{}\nInstall it: `pip install duckdb` (or `uv add duckdb`).",
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
    use tokio::io::AsyncWriteExt;
    let mut cmd = crate::helper_proc::python_module(
        python,
        "barca._sql",
        cfg.storage_options_json.as_deref(),
    );
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = crate::helper_proc::spawn(&mut cmd)
        .map_err(|e| BarcaError::Other(format!("cannot run {}: {e}", python.display())))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| BarcaError::Other("no stdin for the sql helper".into()))?;
    stdin
        .write_all(request.to_string().as_bytes())
        .await
        .map_err(|e| BarcaError::Other(format!("sql helper: {e}")))?;
    drop(stdin);
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| BarcaError::Other(format!("sql helper: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(BarcaError::Other(format!(
            "the sql helper failed: {}",
            err.lines().last().unwrap_or("exited with an error")
        )));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| BarcaError::Other(format!("the sql helper returned invalid JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_come_from_the_extension() {
        assert_eq!(format_of(".barca/artifacts/a/h.parquet"), "parquet");
        assert_eq!(format_of("x.json"), "json");
        assert_eq!(format_of("x.pkl"), "pickle");
        assert_eq!(format_of("x"), "unknown");
    }

    #[test]
    fn fetched_sizes_are_readable() {
        assert_eq!(human_size(40), "40 bytes");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }
}
