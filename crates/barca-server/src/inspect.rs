//! Inspection in the CLI's shapes (RFC-0007 §3.4 "Inspection"): each body is the `--json`
//! output of the matching command, built by the same `barca_core::report` function, so a
//! client prints it as it would print a local command's. Every read goes through a private
//! snapshot of the metadata DB.

use crate::error::ApiError;
use crate::respond::{json_text, stdout_of};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use barca_core::envelope::Context;
use barca_core::served::{Project, ProjectFile, ProjectTelemetry};
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
    let dag = state.reads.loaded_dag().await?;
    let assets = barca_core::queries::list_assets_from_dag(&dag);
    let next_fires = crate::schedule_view::next_fires(&dag);
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
    pub files: Option<String>,
    /// `--state a,b`: only nodes in these cache states.
    pub state: Option<String>,
    /// `--problems`: only nodes that need attention (`1`, `true`).
    pub problems: Option<String>,
}

/// `GET /nodes?targets=a,b&sample=N&state=a,b&problems=1` — `barca status --json --all` for the
/// server's ledger, each scheduled node with its scheduler's view in `schedule`.
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
    let filter = barca_core::status::StatusFilter {
        states: barca_core::status::StatusFilter::parse_states(
            &query.state.iter().cloned().collect::<Vec<_>>(),
        )
        .map_err(|e| ApiError::BadRequest(format!("state: {e}")))?,
        problems: query
            .problems
            .as_deref()
            .is_some_and(|v| !matches!(v, "" | "0" | "false")),
        ..Default::default()
    };
    let dag = state.reads.inspect_dag().await?;
    let db_state = state.reads.db_state();
    let snapshot = state.reads.snapshot_db().await?;
    let sample = query.sample.unwrap_or(0);
    let mut result = node_status(
        &state,
        &dag,
        &db_state,
        &snapshot,
        targets,
        sample,
        query.files.as_deref(),
    )
    .await?;
    let disk_cfg = state.config.resolved.clone();
    result.disk_usage = tokio::task::spawn_blocking(move || barca_core::gc::disk_usage(&disk_cfg))
        .await
        .unwrap_or_default();
    filter.apply(&mut result);
    let rendered = barca_core::report::render_status(result, true, None, None, root().as_deref());
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

/// `barca status` for `targets` (none: every node) over `dag` and the database in state
/// `db_state`, each scheduled node with its scheduler's view and every node with its health:
/// the nodes of `GET /nodes` and the rows of `barca.nodes` (`POST /sql/query`).
pub(crate) async fn node_status(
    state: &AppState,
    dag: &std::sync::Arc<barca_core::dag::Dag>,
    db_state: &crate::read_cache::Stamp,
    snapshot: &std::sync::Arc<crate::snapshot::SnapshotDb>,
    targets: Vec<String>,
    sample: usize,
    files: Option<&str>,
) -> Result<barca_core::status::StatusResult, ApiError> {
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let python = state.config.python.clone();
    let (graph, names, kept_snapshot) = (dag.clone(), targets.clone(), snapshot.clone());
    // Kept until the sources or the database change ([`crate::read_cache`]); the live schedule
    // state and the filter are applied to each answer.
    let kept = state
        .reads.cached_status(dag, db_state, (targets, sample, true), move || async move {
            let _snapshot = kept_snapshot;
            barca_core::status::status_from_dag(&cfg, &names, &graph, &python, sample, true).await
        })
        .await
        .map_err(|e| ApiError::run(e, context("status", state)))?;
    let mut result = (*kept).clone();
    let now = chrono::Utc::now();
    let running =
        crate::health_view::attach_health_at(state, &mut result, dag, db_state, &snapshot.path, now)
            .await?;
    // `as_of` is in the body, so the clock to the second would change the `ETag` every second
    // and an unchanged poll would never be a `304`. It is the start of the minute, the instant
    // `gap_seconds` is read at; nothing else in the body moves with the second.
    result.as_of = barca_core::time::rfc3339_secs(barca_core::causes::gap_instant(now.timestamp()));
    if let Some(files) = files {
        let files: std::collections::HashSet<_> = files.split(',').collect();
        result.nodes.retain(|n| {
            n.id.rsplit_once(':')
                .is_some_and(|(f, _)| files.contains(f))
        });
        result.data_coverage = barca_core::health::DataCoverage::of(&result.nodes);
        let actions = result
            .rollup
            .iter()
            .flat_map(|entry| &entry.held_by)
            .map(|held| (held.root.clone(), held.action_kind))
            .collect();
        result.rollup = Some(barca_core::rollup::total(&result.nodes, &running, &actions));
        result.summary = barca_core::status::StatusSummary::of(&result.nodes);
    }
    Ok(result)
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
    /// `--not-triggered-by`: comma-separated triggers to leave out.
    pub not_triggered_by: Option<String>,
    /// `--until`: a duration back from now on this server's clock, or a time.
    pub until: Option<String>,
    /// `--search`: text in a run's id, target or job.
    pub search: Option<String>,
}

/// `GET /history?limit=N&all=1&status=..&target=..&triggered_by=..&not_triggered_by=..
/// &since=..&until=..&search=..` — `barca history --json` with the same filters. A filter
/// value barca does not accept is a 400.
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
        barca_core::history_filter::HistoryFlags {
            status: query.status.as_deref(),
            target: query.target.as_deref(),
            triggered_by: query.triggered_by.as_deref(),
            not_triggered_by: query.not_triggered_by.as_deref(),
            since: query.since.as_deref(),
            until: query.until.as_deref(),
            search: query.search.as_deref(),
        },
        chrono::Utc::now(),
    )
    .map_err(ApiError::BadRequest)?;
    let snapshot = state.reads.snapshot_db().await?;
    let (runs, total) = barca_core::queries::history_at(&snapshot.path, limit, &filter).await?;
    // Asked for the runs in progress: each with what it is working on now (#991).
    let now = if filter.statuses.iter().any(|s| s == "running")
        && runs.iter().any(|r| r.status == "running")
    {
        crate::runs::in_flight_now(&state, &snapshot).await?
    } else {
        Default::default()
    };
    let extras = barca_core::report::HistoryExtras::now(now);
    let rendered = barca_core::report::render_history(&runs, total, true, None, &extras);
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

#[derive(Default, Deserialize)]
pub struct SqlViewsQuery {
    /// Comma-separated deployed files (root-relative); absent: every deployed file.
    pub files: Option<String>,
}

/// `GET /sql/views?files=a.py,b.py` — which result each `barca sql` view reads in the
/// server's ledger ([`barca_core::sql::Catalog`]): the client runs the query on its own machine
/// over those store paths. The server runs no SQL over results or files
/// ([`crate::sql_query`] answers queries over barca's own tables).
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
    let dag = state.reads.inspect_dag().await?;
    let snapshot = state.reads.snapshot_db().await?;
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
    let snapshot = state.reads.snapshot_db().await?;
    let (rows, total) = barca_core::db::read_step_rows(&snapshot.path, limit, offset).await?;
    Ok(Json(json!({
        "materializations": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// The most keys one `GET /assets/{name}/partitions` lists.
const PARTITION_KEYS_MAX: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct PartitionsQuery {
    /// `success` or `failed`: the key's latest attempt.
    pub status: Option<String>,
    /// A substring of the key.
    pub q: Option<String>,
    /// Keys to return (default 100, at most [`PARTITION_KEYS_MAX`]).
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// `GET /assets/{name}/partitions` — a node's keys with a recorded attempt, each with its latest.
pub async fn partitions(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<PartitionsQuery>,
) -> Result<Json<barca_core::db::PartitionKeys>, ApiError> {
    let dag = state.reads.loaded_dag().await?;
    let id = barca_core::targets::find_target_id(&dag, &name)
        .map_err(|e| ApiError::run(e, context("partitions", &state)))?;
    let snapshot = state.reads.snapshot_db().await?;
    let filter = barca_core::db::PartitionKeysFilter {
        status: query.status,
        query: query.q.filter(|q| !q.is_empty()),
        limit: query.limit.unwrap_or(100).min(PARTITION_KEYS_MAX),
        offset: query.offset.unwrap_or(0),
    };
    Ok(Json(
        barca_core::db::partition_keys(&snapshot.path, &id, &filter).await?,
    ))
}

/// The key grid of `name`, kept until the sources or the database change: what the grid, the
/// keys and the overview routes all read, so a filter costs a pass over the grid and no
/// database read.
async fn kept_partition_grid(
    state: &AppState,
    name: &str,
) -> Result<std::sync::Arc<barca_core::partition_grid::PartitionGrid>, ApiError> {
    let dag = state.reads.loaded_dag().await?;
    let id = barca_core::partition_grid::partitioned_target(&dag, name)
        .map_err(|e| ApiError::run(e, context("status", state)))?;
    let db = state.reads.db_state();
    if let Some(grid) = state.reads.kept_grid(&dag, &db, &id) {
        return Ok(grid);
    }
    let snapshot = state.reads.snapshot_db().await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let grid = std::sync::Arc::new(
        barca_core::partition_grid::partition_grid(&cfg, &dag, &id)
            .await
            .map_err(|e| ApiError::run(e, context("partitions grid", state)))?,
    );
    state.reads.keep_grid(&dag, &db, grid.clone());
    Ok(grid)
}

/// `GET /assets/{name}/partitions/grid` — every key of a partitioned node: its state as the
/// dry run decides it, across the node's chain, and its latest attempt, in columns.
pub async fn partition_grid(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<std::sync::Arc<barca_core::partition_grid::PartitionGrid>>, ApiError> {
    Ok(Json(kept_partition_grid(&state, &name).await?))
}

/// The most keys one `GET /assets/{name}/partitions/keys` lists.
pub const KEYS_PAGE_MAX: usize = 1000;

/// `GET /assets/{name}/partitions/keys?state=failed,blocked&needs_run=1&value=region=apac,emea
/// &range=day=a..b&error=0&q=text&sort=state&limit=N&offset=M` — `barca status NODE --keys
/// --json` with the same filters: one page of the keys that match (default 100, at most
/// [`KEYS_PAGE_MAX`]). `value` repeats, one per dimension. A filter the node does not have (a
/// dimension, a range value, an error group) is a 400 saying what it has.
pub async fn partition_keys_page(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> Result<Json<barca_core::key_query::KeysPage>, ApiError> {
    let last = |key: &str| -> Option<&str> {
        params
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let number = |key: &str| -> Result<Option<usize>, ApiError> {
        last(key)
            .filter(|v| !v.is_empty())
            .map(|v| {
                v.parse::<usize>().map_err(|_| {
                    ApiError::BadRequest(format!("{key}: '{v}' is not a whole number"))
                })
            })
            .transpose()
    };
    let states: Vec<String> = last("state").map(str::to_string).into_iter().collect();
    let values: Vec<String> = params
        .iter()
        .filter(|(k, _)| k == "value")
        .map(|(_, v)| v.clone())
        .collect();
    let query = barca_core::key_query::KeyQuery::parse(&barca_core::key_query::KeyQueryArgs {
        states: &states,
        needs_run: last("needs_run").is_some_and(|v| !matches!(v, "" | "0" | "false")),
        values: &values,
        range: last("range"),
        error: number("error")?,
        text: last("q"),
        sort: last("sort"),
        offset: number("offset")?.unwrap_or(0),
        limit: Some(number("limit")?.unwrap_or(100).min(KEYS_PAGE_MAX)),
    })
    .map_err(ApiError::BadRequest)?;
    let grid = kept_partition_grid(&state, &name).await?;
    // A pass over every key of the node, and a sort when an order is asked for: off the
    // async threads.
    let page = tokio::task::spawn_blocking(move || grid.keys_page(&query))
        .await
        .map_err(|e| ApiError::BadRequest(format!("the keys query did not finish: {e}")))?
        .map_err(ApiError::BadRequest)?;
    Ok(Json(page))
}

#[derive(Debug, Default, Deserialize)]
pub struct PartitionOverviewQuery {
    /// Rows to return (default: as many as fit [`barca_core::key_query::OVERVIEW_CELLS_MAX`]).
    pub limit: Option<usize>,
}

/// `GET /assets/{name}/partitions/overview?limit=N` — `barca status NODE --overview --json`:
/// the node's keys in one picture: counts, what is wrong and where, and the grid's rows.
pub async fn partition_overview(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<PartitionOverviewQuery>,
) -> Result<Json<barca_core::key_query::KeysOverview>, ApiError> {
    let grid = kept_partition_grid(&state, &name).await?;
    let overview = tokio::task::spawn_blocking(move || grid.overview(query.limit))
        .await
        .map_err(|e| ApiError::BadRequest(format!("the overview did not finish: {e}")))?;
    Ok(Json(overview))
}

#[derive(Debug, Deserialize)]
pub struct PartitionKeyQuery {
    /// The key: `day=2026-01-01,region=apac`.
    pub key: String,
    /// First rows of each artifact read (default 5, at most 50).
    pub sample: Option<usize>,
}

/// `GET /assets/{name}/partitions/key?key=...` — one key: its state in each node of the chain,
/// its latest attempts, and the shape of its result and of what it read.
pub async fn partition_key(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<PartitionKeyQuery>,
) -> Result<Json<barca_core::partition_grid::PartitionKeyDetail>, ApiError> {
    let dag = state.reads.loaded_dag().await?;
    let id = barca_core::targets::find_target_id(&dag, &name)
        .map_err(|e| ApiError::run(e, context("partition key", &state)))?;
    let snapshot = state.reads.snapshot_db().await?;
    let mut cfg = state.config.resolved.clone();
    cfg.db_path = snapshot.path.clone();
    let detail = barca_core::partition_grid::partition_key_detail(
        &cfg,
        &dag,
        &state.config.python,
        &id,
        &query.key,
        query
            .sample
            .unwrap_or(5)
            .min(barca_core::partition_grid::KEY_SAMPLE_MAX),
    )
    .await
    .map_err(|e| ApiError::run(e, context("partition key", &state)))?;
    detail.map(Json).ok_or_else(|| {
        ApiError::NotFound(barca_core::partition_grid::no_such_key(&name, &query.key))
    })
}

/// `GET /stats/{name}` — `barca stats --json`.
pub async fn stats(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let dag = state.reads.loaded_dag().await?;
    let id = barca_core::targets::find_target_id(&dag, &name)
        .map_err(|e| ApiError::run(e, context("stats", &state)))?;
    let snapshot = state.reads.snapshot_db().await?;
    let stats = barca_core::db::get_asset_stats(&snapshot.path, &id).await?;
    let rendered = barca_core::report::render_stats(&stats, true, None);
    Ok(json_text(StatusCode::OK, stdout_of(rendered)))
}

/// `GET /project` — the deployed tree: `{api_version, root, env, deployed_at, commit,
/// files: [{path, sha256}]}`. The files are hashed on each request, so an edit (or a
/// `--watch` reload) shows at once.
pub async fn project(State(state): State<AppState>) -> Result<Json<Project>, ApiError> {
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
        files.push(ProjectFile {
            path,
            sha256: barca_core::hash::sha256_hex(&bytes),
        });
    }
    Ok(Json(Project {
        api_version: barca_core::outcome::API_VERSION,
        env: state.config.resolved.env.clone(),
        deployed_at: barca_core::outcome::rfc3339(state.lifecycle.started_at),
        commit: root.as_deref().and_then(git_commit),
        root,
        files,
        // Where a step's `trace_id` opens (`{datadog_url}/apm/trace/{trace_id}`): `null`
        // when no Datadog site is configured and the integration is off.
        telemetry: ProjectTelemetry {
            datadog_url: barca_core::config::datadog_app_url(
                &state.config.resolved.project_root,
                barca_core::telemetry::is_on("datadog"),
            )
            .ok()
            .flatten(),
        },
    }))
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
/// `GET /logs/{run_id}` — persisted stdout lines for a run (durable history).
pub async fn logs(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let snap = state.reads.snapshot_db().await?;
    let entries = barca_core::db::get_logs(&snap.path, &run_id).await?;
    Ok(Json(json!({ "logs": entries })))
}
