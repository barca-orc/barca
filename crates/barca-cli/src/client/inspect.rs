//! The inspection commands against a server (`barca list|status|history|stats|queue|sql
//! --server`): the CLI's `--json` bodies, rendered here with this command's own `--limit`,
//! `--fields` and output mode by the same `barca_core::report` functions a local command uses.

use super::{Server, encode, in_files};
use crate::error::{CliError, Context, ErrorKind};
use barca_core::BarcaError;
use barca_core::cache_state::CacheState;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// `barca list --server`: `GET /list` (or `/groups`), with this command's files, limit, fields
/// and output mode.
pub(crate) async fn list(
    server: &Server,
    files: &[PathBuf],
    groups: bool,
    json: bool,
    limit: Option<usize>,
    fields: Option<&[String]>,
) -> Result<(), CliError> {
    server.health(None).await?;
    let files = server.deployed_files(files).await?;
    if groups {
        let body = server.get_json("/groups").await?;
        let groups: Vec<barca_core::groups::NodeGroup> = serde_json::from_value(body)
            .map_err(|e| server.unexpected("GET /groups", &e.to_string()))?;
        crate::commands::print_groups(&groups, json);
        return Ok(());
    }
    let body = server.get_json("/list").await?;
    let bad = |e: String| server.unexpected("GET /list", &e);
    let nodes: Vec<barca_core::report::ListNode> =
        serde_json::from_value(body["nodes"].clone()).map_err(|e| bad(e.to_string()))?;
    let mut assets = Vec::new();
    let mut next_fires = std::collections::HashMap::new();
    for node in nodes.into_iter().filter(|n| in_files(&n.id, &files)) {
        let freshness: barca_core::Freshness = match &node.schedule {
            Some(cron) => serde_json::from_value(json!({"type": "Schedule", "value": cron})),
            None => serde_json::from_value(json!({"type": match node.freshness {
                barca_core::report::ListFreshness::Manual => "Manual",
                _ => "Always",
            }})),
        }
        .map_err(|e| bad(e.to_string()))?;
        if let Some(next) = node.next_fire {
            next_fires.insert(node.id.clone(), next);
        }
        assets.push(barca_core::results::AssetSummary {
            id: node.id,
            kind: node.kind,
            freshness,
            inputs: node.inputs,
            env: node.env,
            file: node.file,
            line: node.line,
        });
    }
    let total = assets.len();
    assets.truncate(limit.unwrap_or(total));
    crate::commands::emit(barca_core::report::render_list(
        &assets,
        total,
        &next_fires,
        json,
        fields,
        body["root"].as_str(),
    ));
    Ok(())
}

/// `barca status --server`: `GET /nodes`, with this command's files, limit, fields and mode.
pub(crate) async fn status(
    server: &Server,
    targets: &[String],
    files: &[PathBuf],
    opts: crate::commands::StatusOpts<'_>,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let files = server.deployed_files(files).await?;
    let mut path = format!("/nodes?sample={}", opts.sample);
    if !targets.is_empty() {
        path.push_str(&format!("&targets={}", encode(&targets.join(","))));
    }
    let body = server.get_json(&path).await?;
    let mut result: barca_core::status::StatusResult = serde_json::from_value(body.clone())
        .map_err(|e| server.unexpected("GET /nodes", &e.to_string()))?;
    if !files.is_empty() {
        result.nodes.retain(|n| in_files(&n.id, &files));
        result.summary = barca_core::status::StatusSummary::of(&result.nodes);
    }
    if server.implicit {
        server.annotate_here(&mut result, env).await?;
    }
    // Filtered here, after this machine's own states are known (`not_in_ledger`); the summary
    // keeps counting every node.
    opts.filter.apply(&mut result);
    crate::commands::emit(barca_core::report::render_status(
        result,
        opts.json,
        opts.limit,
        opts.fields,
        body["root"].as_str(),
    ));
    Ok(())
}

impl Server {
    /// `barca status` with a configured server: mark each node with whether this machine has a
    /// copy of its result (`local`), and show a result whose ledger post is pending as
    /// `unsynced` (`not_in_ledger`).
    async fn annotate_here(
        &self,
        result: &mut barca_core::status::StatusResult,
        env: Option<&str>,
    ) -> Result<(), CliError> {
        let here = |e: BarcaError| CliError::from_barca(e, &Context::default());
        let cfg = barca_core::config::resolve(env).map_err(here)?;
        let layout = barca_core::transfer::ArtifactLayout::new(
            cfg.local_artifact_dir.clone(),
            &cfg.artifact_root,
        );
        let pending = pending_posts(&cfg, &self.url).await.map_err(here)?;
        for node in &mut result.nodes {
            // The server's `store_only` (RFC-0009) is about the server's own disk: its result is
            // in the ledger and its object in the store, which is `cached` for this machine,
            // whether or not a copy is here (`local` says).
            if node.cache.state == CacheState::Unsynced && node.cache.reason == "store_only" {
                node.cache.state = CacheState::Cached;
                node.cache.reason = "materialized".to_string();
                node.cache.detail =
                    "a successful materialization matches this code and these inputs".to_string();
            }
            node.local = Some(has_copy(&layout, node.cache.artifact.as_deref()));
            barca_core::status::not_in_ledger(node, &pending);
        }
        result.summary = barca_core::status::StatusSummary::of(&result.nodes);
        Ok(())
    }
}

/// Whether this machine holds the artifact the server recorded at `artifact`: its mirror of
/// that store path exists under this machine's artifact directory. A path outside this
/// project's store (a server without one) is never here.
fn has_copy(layout: &barca_core::transfer::ArtifactLayout, artifact: Option<&str>) -> bool {
    artifact
        .and_then(|a| layout.local_for(a))
        .is_some_and(|p| p.is_file())
}

/// `barca history --server`: `GET /history`, with the filters as query parameters.
pub(crate) async fn history(
    server: &Server,
    limit: Option<usize>,
    filter: &barca_core::history_filter::HistoryFilter,
    json: bool,
    fields: Option<&[String]>,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let mut query = vec![match limit {
        Some(n) => format!("limit={n}"),
        None => "all=1".to_string(),
    }];
    for (key, value) in filter.query_pairs() {
        query.push(format!("{key}={}", encode(&value)));
    }
    let path = format!("/history?{}", query.join("&"));
    let body = server.get_json(&path).await?;
    let bad = |e: serde_json::Error| server.unexpected("GET /history", &e.to_string());
    let runs: Vec<barca_core::db::RunRecord> =
        serde_json::from_value(body["runs"].clone()).map_err(bad)?;
    let total = body["total"].as_u64().map_or(runs.len(), |t| t as usize);
    crate::commands::emit(barca_core::report::render_history(
        &runs, total, json, fields,
    ));
    Ok(())
}

/// `barca history RUN_ID` against a server: `GET /runs/{run_id}`, the run and the steps it
/// recorded. A run the server does not know but this machine does gets the command that
/// reads it here.
pub(crate) async fn run_detail(
    server: &Server,
    run_id: &str,
    limit: Option<usize>,
    json: bool,
    select: &str,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let page = match limit {
        Some(n) => format!("?limit={n}"),
        None => "?all=true".to_string(),
    };
    let path = format!("/runs/{}{page}", encode(run_id));
    let body = match server.get_json(&path).await {
        Ok(body) => body,
        Err(e) => {
            let here = format!("barca history {run_id} --local");
            return Err(here_instead(e, run_id, env, &here).await);
        }
    };
    let bad = |e: serde_json::Error| server.unexpected(&format!("GET {path}"), &e.to_string());
    let run: barca_core::db::RunRecord = serde_json::from_value(body.clone()).map_err(bad)?;
    let mut steps: Vec<barca_core::db::RunStep> =
        serde_json::from_value(body["steps"].clone()).map_err(bad)?;
    // The steps the run recorded, as on this machine: a finished run the server started also
    // lists the cached steps it streamed, until the server restarts.
    let listed = steps.len();
    if !matches!(run.status.as_str(), "pending" | "running") {
        steps.retain(|step| {
            !barca_core::step_status::StepStatus::Cached.is(Some(step.status.as_str()))
        });
    }
    // A server older than 0.30.2 lists every step and sends no page. The cached steps left
    // out above are not counted either.
    let steps_page = match body.get("steps_page") {
        Some(page) => {
            let mut page: barca_core::db::StepsPage =
                serde_json::from_value(page.clone()).map_err(bad)?;
            page.total -= listed - steps.len();
            page
        }
        None => barca_core::db::StepsPage {
            total: steps.len(),
            truncated: false,
            nodes: Vec::new(),
        },
    };
    let detail = barca_core::queries::RunDetail {
        run,
        steps,
        steps_page,
    };
    crate::commands::emit(barca_core::report::render_run_detail(&detail, json, select));
    Ok(())
}

/// This machine's record of `run_id` (with `None`, its newest run), read from a private copy
/// of its metadata DB; `None` when there is none or it cannot be read. Used only for hints.
pub(super) async fn local_run(
    env: Option<&str>,
    run_id: Option<&str>,
) -> Option<barca_core::db::RunRecord> {
    let cfg = barca_core::config::resolve(env).ok()?;
    let snapshot = barca_core::db::DbSnapshot::take(&cfg.db_path)
        .await
        .ok()??;
    match run_id {
        Some(id) => barca_core::db::get_run(snapshot.path(), id).await.ok()?,
        None => barca_core::db::get_recent_runs(snapshot.path(), 1)
            .await
            .ok()?
            .into_iter()
            .next(),
    }
}

/// A stored start time as a sortable key (`datetime('now')` or RFC 3339, both UTC).
pub(super) fn started_key(ts: &str) -> String {
    let ts = ts.trim();
    ts.strip_suffix('Z').unwrap_or(ts).replacen('T', " ", 1)
}

/// A server that does not know `run_id`, when this machine does: the same error, with the
/// command that reads it here (`here`) as its remediation.
pub(super) async fn here_instead(
    e: CliError,
    run_id: &str,
    env: Option<&str>,
    here: &str,
) -> CliError {
    if e.kind != ErrorKind::Usage
        || !e.error.contains("not found")
        || local_run(env, Some(run_id)).await.is_none()
    {
        return e;
    }
    CliError::from_prose(
        ErrorKind::Usage,
        format!("error: {}\nthis run is on this machine: {here}", e.error),
    )
}

/// `barca queue [pause|resume]`: `GET /queue`, `POST /queue/pause` or `POST /queue/resume`, and
/// the queue as JSON or a table.
pub(crate) async fn queue(
    server: &Server,
    action: Option<crate::args::QueueAction>,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    use crate::args::QueueAction;
    server.health(env).await?;
    let (path, text) = match action {
        None => ("/queue", server.get_text("/queue").await?),
        Some(QueueAction::Pause) => (
            "/queue/pause",
            server.post_json("/queue/pause", &json!({})).await?,
        ),
        Some(QueueAction::Resume) => (
            "/queue/resume",
            server.post_json("/queue/resume", &json!({})).await?,
        ),
    };
    let body: Value = serde_json::from_str(&text)
        .map_err(|e| server.unexpected(&format!("GET {path}"), &format!("not JSON: {e}")))?;
    match action {
        Some(QueueAction::Pause) => barca_core::errln!(
            "[barca] run queue paused on {}: queued runs wait until `barca queue resume`",
            server.shown_url
        ),
        Some(QueueAction::Resume) => {
            barca_core::errln!("[barca] run queue resumed on {}", server.shown_url)
        }
        None => {}
    }
    if json {
        barca_core::outln!(
            "{}",
            serde_json::to_string_pretty(&body).expect("a JSON value prints")
        );
    } else {
        barca_core::term::stdout_str(&render_queue(&body));
    }
    Ok(())
}

/// The queue as a table: a summary line, then the running and the queued runs.
fn render_queue(body: &Value) -> String {
    let list = |key: &str| body[key].as_array().cloned().unwrap_or_default();
    let (running, queued) = (list("running"), list("queued"));
    let mut state = Vec::new();
    if body["draining"].as_bool() == Some(true) {
        state.push("draining (the server is stopping; the queue resumes at its next start)".into());
    }
    if body["paused"].as_bool() == Some(true) {
        state.push(format!(
            "paused since {}",
            body["paused_at"].as_str().unwrap_or("?")
        ));
    }
    if state.is_empty() {
        state.push("open".to_string());
    }
    let mut out = format!(
        "queue: {} · {} of {} running · {} queued\n",
        state.join(", "),
        running.len(),
        body["max_runs"].as_u64().unwrap_or(0),
        queued.len()
    );
    if running.is_empty() && queued.is_empty() {
        return out;
    }
    out.push_str(&format!(
        "\n{:<4} {:<14} {:<8} {:<7} {:<28} {:<9} {}\n",
        "POS", "RUN_ID", "STATUS", "COMMAND", "TARGET", "TRIGGER", "QUEUED_AT"
    ));
    for run in running.iter().chain(queued.iter()) {
        let target = run["target"].as_str().unwrap_or("(all)");
        out.push_str(&format!(
            "{:<4} {:<14} {:<8} {:<7} {:<28} {:<9} {}\n",
            run["position"]
                .as_u64()
                .map_or("-".to_string(), |p| p.to_string()),
            run["run_id"].as_str().unwrap_or(""),
            run["status"].as_str().unwrap_or(""),
            run["command"].as_str().unwrap_or(""),
            target,
            run["triggered_by"].as_str().unwrap_or(""),
            run["queued_at"].as_str().unwrap_or(""),
        ));
    }
    out
}

/// `barca stats --server`: `GET /stats/{name}`.
pub(crate) async fn stats(
    server: &Server,
    target: &str,
    files: &[PathBuf],
    json: bool,
    fields: Option<&[String]>,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    server.deployed_files(files).await?;
    let mut body = server
        .get_json(&format!("/stats/{}", encode(target)))
        .await?;
    // The body is `barca stats --json`, which names the node `id`.
    if let Some(obj) = body.as_object_mut()
        && let Some(id) = obj.remove("id")
    {
        obj.insert("node_id".into(), id);
    }
    let stats: barca_core::db::AssetStats = serde_json::from_value(body)
        .map_err(|e| server.unexpected("GET /stats", &e.to_string()))?;
    crate::commands::emit(barca_core::report::render_stats(&stats, json, fields));
    Ok(())
}

/// `barca sql` with a server: the views resolved from the server's ledger (`GET /sql/views`),
/// barca's tables from its records (`GET /history`, `GET /materializations`, `GET /nodes`), and
/// the query run on this machine (the server runs no SQL). The artifacts are read from the
/// store this project shares with the server, as a local `barca sql` reads them.
pub(crate) async fn sql(
    server: &Server,
    query: &str,
    files: &[PathBuf],
    limit: Option<usize>,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    use barca_core::sql::tables;
    let here = |e: BarcaError| CliError::from_barca(e, &Context::default());
    let health = server.health(env).await?;
    let cfg = barca_core::config::resolve(env).map_err(here)?;
    let instead = if server.implicit {
        "pass --local to query this machine's own results"
    } else {
        "leave out --server to query this machine's own results"
    };
    barca_core::ledger::same_store(
        &cfg,
        &server.shown_url,
        health["store_uri"].as_str(),
        instead,
    )
    .map_err(here)?;
    let files = server.deployed_files(files).await?;
    let mut path = "/sql/views".to_string();
    if !files.is_empty() {
        path.push_str(&format!("?files={}", encode(&files.join(","))));
    }
    let body = server.get_json(&path).await?;
    let catalog: barca_core::sql::Catalog = serde_json::from_value(body)
        .map_err(|e| server.unexpected("GET /sql/views", &e.to_string()))?;

    let pending = if server.implicit {
        pending_posts(&cfg, &server.url).await.map_err(here)?
    } else {
        Vec::new()
    };
    let cap = tables::SERVER_ROWS;
    let mut runs = tables::runs(None);
    if tables::named(query, tables::RUNS) {
        let route = format!("/history?limit={cap}");
        let body = server.get_json(&route).await?;
        let rows: Vec<barca_core::db::RunRecord> = serde_json::from_value(body["runs"].clone())
            .map_err(|e| server.unexpected("GET /history", &e.to_string()))?;
        let total = body["total"].as_u64().map_or(rows.len(), |t| t as usize);
        runs = tables::runs(Some(&rows));
        runs.note = tables::truncated_note(tables::RUNS, rows.len(), total, &server.shown_url);
    }
    let mut steps = tables::materializations(None);
    if tables::named(query, tables::MATERIALIZATIONS) {
        let route = format!("/materializations?limit={cap}");
        let body = server.get_json(&route).await?;
        let rows: Vec<barca_core::db::StepRow> =
            serde_json::from_value(body["materializations"].clone())
                .map_err(|e| server.unexpected("GET /materializations", &e.to_string()))?;
        let total = body["total"].as_u64().map_or(rows.len(), |t| t as usize);
        steps = tables::materializations(Some(&rows));
        steps.note = tables::truncated_note(
            tables::MATERIALIZATIONS,
            rows.len(),
            total,
            &server.shown_url,
        );
    }
    let mut nodes = tables::nodes(None);
    if tables::named(query, tables::NODES) {
        let body = server.get_json("/nodes?sample=0").await?;
        let mut result: barca_core::status::StatusResult = serde_json::from_value(body)
            .map_err(|e| server.unexpected("GET /nodes", &e.to_string()))?;
        if !files.is_empty() {
            result.nodes.retain(|n| in_files(&n.id, &files));
        }
        if server.implicit {
            server.annotate_here(&mut result, env).await?;
        }
        nodes = tables::nodes(Some(&result.nodes));
    }

    let python = barca_core::commands::find_python();
    let origin = barca_core::sql::Origin::Server {
        url: server.shown_url.clone(),
        pending,
    };
    let result = barca_core::sql::run(
        &cfg,
        query,
        limit,
        catalog,
        vec![runs, steps, nodes],
        Vec::new(),
        &python,
        &origin,
    )
    .await
    .map_err(here)?;
    crate::commands::emit(barca_core::report::render_sql(result, json));
    Ok(())
}

/// This machine's results whose post to the server at `url` is pending (`unsynced`).
async fn pending_posts(
    cfg: &barca_core::config::ResolvedConfig,
    url: &str,
) -> Result<Vec<barca_core::status::PendingPost>, BarcaError> {
    if !Path::new(&cfg.db_path).exists() {
        return Ok(Vec::new());
    }
    Ok(barca_core::ledger::posts::unsynced(&cfg.db_path, url)
        .await?
        .into_iter()
        .flat_map(|post| post.steps)
        .map(|step| barca_core::status::PendingPost {
            node_id: step.node_id,
            run_hash: step.run_hash,
            error: None,
        })
        .collect())
}
