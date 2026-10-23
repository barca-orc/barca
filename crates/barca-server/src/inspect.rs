//! Inspection in the CLI's shapes (RFC-0007 §3.4 "Inspection"): each body is the `--json`
//! output of the matching command, built by the same `barca_core::report` function, so a
//! client prints it as it would print a local command's. Every read goes through a private
//! snapshot of the metadata DB.

use crate::error::ApiError;
use crate::handlers::snapshot_db;
use crate::runs::{json_text, stdout_of};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use barca_core::envelope::Context;
use serde::Deserialize;
use serde_json::{Value, json};

/// The project root the CLI's `root` key names: the server's working directory, which
/// `barca serve` moved to the project root.
fn root() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|root| root.display().to_string())
}

/// `GET /list` — `barca list --json --all` (the client applies `--limit` and `--fields`).
pub async fn list(State(state): State<AppState>) -> Result<Response, ApiError> {
    let dag = state.loaded_dag().await?;
    let assets = barca_core::queries::list_assets_from_dag(&dag);
    let next_fires = crate::handlers::next_fires(&dag);
    let rendered = barca_core::report::render_list(
        &assets,
        assets.len(),
        &next_fires,
        true,
        None,
        root().as_deref(),
    );
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

#[derive(Default, Deserialize)]
pub struct NodesQuery {
    /// Comma-separated targets; absent: every node.
    pub targets: Option<String>,
    /// `--sample N`.
    pub sample: Option<usize>,
}

/// `GET /nodes?targets=a,b&sample=N` — `barca status --json --all` for the server's ledger.
pub async fn nodes(
    State(state): State<AppState>,
    Query(query): Query<NodesQuery>,
) -> Result<Response, ApiError> {
    let targets: Vec<String> = query
        .targets
        .iter()
        .flat_map(|t| t.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let dag = state.current_dag().await?;
    let snapshot = snapshot_db(&state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let result = barca_core::status::status_from_dag(
        &cfg,
        &targets,
        &dag,
        &state.config.python,
        query.sample.unwrap_or(0),
        true,
    )
    .await
    .map_err(|e| ApiError::run(e, context("status", &state)))?;
    let rendered = barca_core::report::render_status(result, true, None, None, root().as_deref());
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

fn context(command: &'static str, state: &AppState) -> Context {
    Context {
        command,
        files: state.config.files.clone(),
    }
}

#[derive(Default, Deserialize)]
pub struct HistoryQuery {
    /// `-l N`; absent: every run, as `barca history --json` prints without `-l`.
    pub limit: Option<usize>,
    /// `--all`: every run whatever `limit` says.
    pub all: Option<String>,
    /// `--status`: comma-separated statuses.
    pub status: Option<String>,
    /// `--target`: a function name or node id.
    pub target: Option<String>,
    /// `--triggered-by`: comma-separated triggers.
    pub triggered_by: Option<String>,
    /// `--since`: a duration back from now on this server's clock, or a time.
    pub since: Option<String>,
}

/// `GET /history?limit=N&all=1&status=..&target=..&triggered_by=..&since=..` — `barca history
/// --json` with the same filters. A filter value barca does not accept is a 400.
pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    let all = query
        .all
        .as_deref()
        .is_some_and(|v| !matches!(v, "" | "0" | "false"));
    let limit = if all { None } else { query.limit };
    let filter = barca_core::history_filter::HistoryFilter::parse(
        query.status.as_deref(),
        query.target.as_deref(),
        query.triggered_by.as_deref(),
        query.since.as_deref(),
        chrono::Utc::now(),
    )
    .map_err(ApiError::BadRequest)?;
    let snapshot = snapshot_db(&state).await?;
    let (runs, total) = barca_core::queries::history_at(&snapshot.path, limit, &filter).await?;
    let rendered = barca_core::report::render_history(&runs, total, true, None);
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

#[derive(Default, Deserialize)]
pub struct SqlViewsQuery {
    /// Comma-separated deployed files (root-relative); absent: every deployed file.
    pub files: Option<String>,
}

/// `GET /sql/views?files=a.py,b.py` — which result each `barca sql` view reads in the
/// server's ledger ([`barca_core::sql::Catalog`]): the client runs the query on its own machine
/// over those store paths. The server runs no SQL.
pub async fn sql_views(
    State(state): State<AppState>,
    Query(query): Query<SqlViewsQuery>,
) -> Result<Json<barca_core::sql::Catalog>, ApiError> {
    let files: Vec<String> = query
        .files
        .iter()
        .flat_map(|t| t.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let dag = state.current_dag().await?;
    let snapshot = snapshot_db(&state).await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let catalog =
        barca_core::sql::catalog_from_dag(&cfg, &dag, &state.config.python, |id: &str| {
            files.is_empty()
                || id
                    .split_once(':')
                    .is_some_and(|(file, _)| files.iter().any(|f| f == file))
        })
        .await
        .map_err(|e| ApiError::run(e, context("sql", &state)))?;
    Ok(Json(catalog))
}

/// Most rows `GET /materializations` returns at once.
pub const MATERIALIZATIONS_MAX: usize = 10_000;

#[derive(Default, Deserialize)]
pub struct MaterializationsQuery {
    /// Rows to return (default 1000, at most [`MATERIALIZATIONS_MAX`]).
    pub limit: Option<usize>,
    /// Rows to skip, newest first.
    pub offset: Option<usize>,
}

/// `GET /materializations?limit=N&offset=M` — step results, newest first, one page at a time:
/// `barca sql`'s `barca.materializations`.
pub async fn materializations(
    State(state): State<AppState>,
    Query(query): Query<MaterializationsQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(1000).min(MATERIALIZATIONS_MAX);
    let offset = query.offset.unwrap_or(0);
    let snapshot = snapshot_db(&state).await?;
    let (rows, total) = barca_core::db::read_step_rows(&snapshot.path, limit, offset).await?;
    Ok(Json(json!({
        "materializations": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// `GET /stats/{name}` — `barca stats --json`.
pub async fn stats(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let dag = state.loaded_dag().await?;
    let id = barca_core::targets::find_target_id(&dag, &name)
        .map_err(|e| ApiError::run(e, context("stats", &state)))?;
    let snapshot = snapshot_db(&state).await?;
    let stats = barca_core::db::get_asset_stats(&snapshot.path, &id).await?;
    let rendered = barca_core::report::render_stats(&stats, true, None);
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

/// `GET /project` — the deployed tree: `{api_version, root, env, deployed_at, commit,
/// files: [{path, sha256}]}`. The files are hashed on each request, so an edit (or a
/// `--watch` reload) shows at once.
pub async fn project(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let root = root();
    let mut files = Vec::new();
    for file in &state.config.files {
        let bytes = std::fs::read(file).map_err(|e| {
            ApiError::Barca(barca_core::BarcaError::Io(std::io::Error::new(
                e.kind(),
                format!("cannot read deployed file {file}: {e}"),
            )))
        })?;
        let path = match &root {
            Some(root) => std::path::Path::new(file)
                .strip_prefix(root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| file.clone()),
            None => file.clone(),
        };
        files.push(json!({"path": path, "sha256": sha256_hex(&bytes)}));
    }
    Ok(Json(json!({
        "api_version": barca_core::outcome::API_VERSION,
        "root": root,
        "env": state.config.resolved.env,
        "deployed_at": barca_core::outcome::rfc3339(state.started_at),
        "commit": root.as_deref().and_then(git_commit),
        "files": files,
    })))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The commit checked out at `root`, read from `.git` without running git: `HEAD`, the ref it
/// names (a loose ref file or `packed-refs`). `None` outside a git checkout.
fn git_commit(root: &str) -> Option<String> {
    let mut dir = std::path::Path::new(root);
    let git = loop {
        let candidate = dir.join(".git");
        if candidate.exists() {
            break candidate;
        }
        dir = dir.parent()?;
    };
    // A worktree's `.git` is a file naming the real directory.
    let git = if git.is_file() {
        let text = std::fs::read_to_string(&git).ok()?;
        let target = text.trim().strip_prefix("gitdir: ")?;
        dir.join(target)
    } else {
        git
    };
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref: ") else {
        return Some(head.to_string());
    };
    // Refs live in the common directory of a worktree.
    let common = std::fs::read_to_string(git.join("commondir"))
        .ok()
        .map(|c| git.join(c.trim()))
        .unwrap_or_else(|| git.clone());
    for base in [&git, &common] {
        if let Ok(id) = std::fs::read_to_string(base.join(reference)) {
            return Some(id.trim().to_string());
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (id, name) = line.split_once(' ')?;
        (name == reference).then(|| id.to_string())
    })
}
