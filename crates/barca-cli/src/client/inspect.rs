//! The inspection commands against a server (`barca list|status|history|stats|queue|sql
//! --server`): the CLI's `--json` bodies, rendered here with this command's own `--limit`,
//! `--fields` and output mode by the same `barca_core::report` functions a local command uses.

use super::{Server, encode, in_files};
use crate::error::{CliError, Context, ErrorKind};
use barca_core::BarcaError;
use barca_core::cache_state::CacheState;
use barca_core::served::{Pauses, QueueView, SchedulePauseAnswer};
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
        crate::print::print_groups(&groups, json);
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
    crate::print::emit(barca_core::report::render_list(
        &assets,
        total,
        &next_fires,
        json,
        fields,
        body["root"].as_str(),
    ));
    Ok(())
}

/// `barca status --rollup --server`: `GET /state/rollup`, scoped to this command's targets and
/// files.
pub(crate) async fn rollup(
    server: &Server,
    targets: &[String],
    files: &[PathBuf],
    json: bool,
    now: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let files = server.deployed_files(files).await?;
    let mut query = Vec::new();
    if now {
        query.push("now=true".into());
    }
    if !targets.is_empty() {
        query.push(format!("targets={}", encode(&targets.join(","))));
    }
    // Files alone scope the rollup to their nodes. With targets they only say where the targets
    // are, as on this machine: the route would add every node of the files to the scope.
    if targets.is_empty() && !files.is_empty() {
        query.push(format!("files={}", encode(&files.join(","))));
    }
    let path = match query.is_empty() {
        true => "/state/rollup".to_string(),
        false => format!("/state/rollup?{}", query.join("&")),
    };
    let body = server.get_json(&path).await?;
    let rollup: barca_core::rollup::Rollup = serde_json::from_value(body)
        .map_err(|e| server.unexpected("GET /state/rollup", &e.to_string()))?;
    crate::print::emit(barca_core::report::render_rollup(&rollup, json));
    Ok(())
}

/// `barca status --server`: `GET /nodes`, with this command's files, limit, fields and mode.
pub(crate) async fn status(
    server: &Server,
    targets: &[String],
    files: &[PathBuf],
    opts: crate::request::StatusOpts<'_>,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let files = server.deployed_files(files).await?;
    let mut path = format!("/nodes?sample={}", opts.sample);
    if !targets.is_empty() {
        path.push_str(&format!("&targets={}", encode(&targets.join(","))));
    }
    if !files.is_empty() {
        path.push_str(&format!("&files={}", encode(&files.join(","))));
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
    crate::print::emit(barca_core::report::render_status(
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
    // The server's own words for each run: its `line`, and with `--status running` what a
    // run in progress is working on (`now`).
    let extras = barca_core::report::HistoryExtras::of_server_runs(&body["runs"]);
    crate::print::emit(barca_core::report::render_history(
        &runs, total, json, fields, &extras,
    ));
    Ok(())
}

/// `barca history --activity --server`: `activity` of `GET /runs`, counted by the server over
/// the runs of the last 24 hours that the filters keep.
pub(crate) async fn activity(
    server: &Server,
    filter: &barca_core::history_filter::HistoryFilter,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let mut path = "/runs?limit=1".to_string();
    for (key, value) in filter.query_pairs() {
        path.push_str(&format!("&{key}={}", encode(&value)));
    }
    let body = server.get_json(&path).await?;
    let Some(activity) = body.get("activity").filter(|a| !a.is_null()) else {
        return Err(server.unexpected(
            "GET /runs",
            "the server sent no `activity`: it runs a barca older than 0.44",
        ));
    };
    let activity: barca_core::run_list::RunActivity = serde_json::from_value(activity.clone())
        .map_err(|e| server.unexpected("GET /runs", &e.to_string()))?;
    crate::print::emit(barca_core::report::render_activity(&activity, json));
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
            ..Default::default()
        },
    };
    // The server's own rows of what the targets returned (it has the run's outcome).
    let outputs =
        serde_json::from_value(body["explanation"]["outputs"].clone()).unwrap_or_default();
    let detail = barca_core::queries::RunDetail {
        run,
        steps,
        steps_page,
        outputs,
    };
    crate::print::emit(barca_core::report::render_run_detail(&detail, json, select));
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
        // The server's answer as it sent it: a key a newer server adds is printed too.
        barca_core::outln!(
            "{}",
            serde_json::to_string_pretty(&body).expect("a JSON value prints")
        );
    } else {
        let view: QueueView = serde_json::from_value(body)
            .map_err(|e| server.unexpected(&format!("GET {path}"), &e.to_string()))?;
        barca_core::term::stdout_str(&render_queue(&view));
    }
    Ok(())
}

/// The queue as a table: a summary line, then the running and the queued runs.
fn render_queue(view: &QueueView) -> String {
    let (running, queued) = (&view.running, &view.queued);
    let mut state = Vec::new();
    if view.draining {
        state.push("draining (the server is stopping; the queue resumes at its next start)".into());
    }
    if view.paused {
        state.push(format!(
            "paused since {}",
            view.paused_at.as_deref().unwrap_or("?")
        ));
    }
    if state.is_empty() {
        state.push("open".to_string());
    }
    // The slots in the server's words (`slots`): it says when a requested run is in the
    // extra slot. Older servers omit this field.
    let slots = match view.slots.as_str() {
        "" => view.max_runs.map_or_else(
            || format!("{} running · run slots unknown", running.len()),
            |max| barca_core::run_list::queue_slots_line(running.len(), max),
        ),
        slots => slots.to_string(),
    };
    let mut out = format!(
        "queue: {} · {slots} · {} queued\n",
        state.join(", "),
        queued.len()
    );
    if running.is_empty() && queued.is_empty() {
        return out;
    }
    out.push_str(&format!(
        "\n{:<4} {:<14} {:<8} {:<7} {:<28} {:<9} {:<25} {}\n",
        "POS", "RUN_ID", "STATUS", "COMMAND", "TARGET", "TRIGGER", "QUEUED_AT", "START"
    ));
    for run in running.iter().chain(queued.iter()) {
        // The targets in a few words (`target_line`); `--json` has the full list. Older
        // servers omit it.
        let target = match run.target_line.as_str() {
            "" => barca_core::run_list::target_line(run.target.as_deref()),
            line => line.to_string(),
        };
        // When it started, or (`~`) when the server estimates it will.
        let start = match (&run.started_at, &run.estimated_start_at) {
            (Some(at), _) => at.to_string(),
            (None, Some(at)) => format!("~{at}"),
            (None, None) => "-".to_string(),
        };
        out.push_str(&format!(
            "{:<4} {:<14} {:<8} {:<7} {:<28} {:<9} {:<25} {}\n",
            run.position.map_or("-".to_string(), |p| p.to_string()),
            run.run_id,
            run.status,
            run.command,
            target,
            run.triggered_by.as_str(),
            run.queued_at,
            start,
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
    crate::print::emit(barca_core::report::render_stats(&stats, json, fields));
    Ok(())
}

/// The most rows `POST /sql/query` answers (`barca_server`'s `MAX_LIMIT`): a larger `--limit`,
/// or `--all`, runs on this machine.
const SERVER_QUERY_ROWS: usize = 10_000;

/// Whether the server answers `query` itself (`POST /sql/query`): it names one of barca's
/// tables and no result view, no file was typed (the server's `barca.nodes` is every deployed
/// file's), and the rows fit one answer. With a configured server (no `--server`), a query
/// over `barca.nodes` runs here: its `local` and `unsynced` describe this machine.
fn server_runs(
    server: &Server,
    query: &str,
    catalog: &barca_core::sql::Catalog,
    files: &[String],
    limit: Option<usize>,
) -> bool {
    use barca_core::sql::{names, tables};
    // Node ids in SQL string literals are values, including in a tick-outcomes predicate.
    // Doubled single quotes escape a quote within a literal.
    let identifiers = query.split('\'').step_by(2).collect::<Vec<_>>().join(" ");
    let query = identifiers.as_str();
    let barca = tables::NAMES;
    let views = catalog
        .views
        .iter()
        .map(|v| &v.name)
        .chain(catalog.missing.iter().map(|m| &m.name));
    barca.iter().any(|t| tables::named(query, t))
        && !views.into_iter().any(|name| names(query, name))
        && files.is_empty()
        && limit.is_some_and(|n| n <= SERVER_QUERY_ROWS)
        && !(server.implicit && tables::named(query, tables::NODES))
}

/// `barca sql` with a server. A query that names only barca's tables (`barca.runs`,
/// `barca.materializations`, `barca.nodes`, `barca.schedules`, `barca.ticks`, `barca.queue`)
/// is answered by the server (`POST /sql/query`,
/// [`server_runs`]). Any other runs on this machine: the views resolved from the server's
/// ledger (`GET /sql/views`), barca's tables from its records (`GET /history`,
/// `GET /materializations`, `GET /nodes`; the schedules, ticks and queue through
/// `POST /sql/query`), the artifacts read from the store this project
/// shares with the server, as a local `barca sql` reads them. The server never runs SQL over
/// results or files.
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
    let files = server.deployed_files(files).await?;
    let mut path = "/sql/views".to_string();
    if !files.is_empty() {
        path.push_str(&format!("?files={}", encode(&files.join(","))));
    }
    let body = server.get_json(&path).await?;
    let catalog: barca_core::sql::Catalog = serde_json::from_value(body)
        .map_err(|e| server.unexpected("GET /sql/views", &e.to_string()))?;

    if server_runs(server, query, &catalog, &files, limit)
        && let Some(answer) = server.sql_query(query, limit.unwrap_or_default()).await?
    {
        let bad = |e: serde_json::Error| server.unexpected("POST /sql/query", &e.to_string());
        let result = barca_core::sql::SqlResult {
            columns: serde_json::from_value(answer["columns"].clone()).map_err(bad)?,
            rows: serde_json::from_value(answer["rows"].clone()).map_err(bad)?,
            total: serde_json::from_value(answer["total"].clone()).map_err(bad)?,
            truncated: answer["truncated"].as_bool().unwrap_or_default(),
            notes: serde_json::from_value(answer["notes"].clone()).unwrap_or_default(),
        };
        crate::print::emit(barca_core::report::render_sql(result, json));
        return Ok(());
    }

    let instead = if server.implicit {
        "pass --local to query this machine's own results"
    } else {
        "leave out --server to query this machine's own results"
    };
    // This project's own server keeps its artifacts where this command reads them: there is
    // no second machine whose store must match.
    if !server.project {
        barca_core::ledger::same_store(
            &cfg,
            &server.shown_url,
            health["store_uri"].as_str(),
            instead,
        )
        .map_err(here)?;
    }

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
    // The schedules, the ticks and the run queue have no route that lists them as rows: the
    // server's own query route answers `select *` over each, the rows it would query itself.
    let mut more = Vec::new();
    for (name, columns) in [
        (tables::SCHEDULES, tables::SCHEDULE_COLUMNS),
        (tables::TICKS, tables::TICK_COLUMNS),
        (tables::QUEUE, tables::QUEUE_COLUMNS),
    ] {
        if !tables::named(query, name) {
            more.push(tables::table(name, columns, None));
            continue;
        }
        let select = format!("select * from barca.{name}");
        let mut table = tables::table(name, columns, Some(Vec::new()));
        match server.sql_query(&select, cap).await? {
            Some(answer) => {
                let rows = match &answer["rows"] {
                    serde_json::Value::Array(rows) => rows.clone(),
                    _ => Vec::new(),
                };
                let total = answer["total"].as_u64().map_or(rows.len(), |t| t as usize);
                table.note = tables::truncated_note(name, rows.len(), total, &server.shown_url);
                table.rows = Some(rows);
            }
            None => {
                table.note = Some(format!(
                    "barca.{name} is empty: the server at {} does not answer queries over \
                     barca's tables (it is from before the query route, or its Python \
                     environment has no DuckDB: `uv add 'barca[sql]'` there)",
                    server.shown_url
                ));
            }
        }
        more.push(table);
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
        [runs, steps, nodes].into_iter().chain(more).collect(),
        Vec::new(),
        &python,
        &origin,
    )
    .await
    .map_err(here)?;
    crate::print::emit(barca_core::report::render_sql(result, json));
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

/// `barca pause [TARGET] [--downstream]`: `POST /pauses`, or without a target `GET /pauses`,
/// and the paused nodes as JSON or a table.
pub(crate) async fn pause(
    server: &Server,
    target: Option<&str>,
    downstream: bool,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let (path, text) = match target {
        None => ("GET /pauses", server.get_text("/pauses").await?),
        Some(target) => (
            "POST /pauses",
            server
                .post_json(
                    "/pauses",
                    &json!({"target": target, "downstream": downstream}),
                )
                .await?,
        ),
    };
    let (body, pauses) = pauses_of(server, path, &text)?;
    if let Some(target) = target {
        let n = pauses.added.as_ref().map_or(0, Vec::len);
        barca_core::errln!(
            "[barca] paused {n} node(s) on {} via {target}: scheduled runs skip them until `barca resume {target}{}`",
            server.shown_url,
            if downstream { " --downstream" } else { "" }
        );
    }
    print_pauses(&body, &pauses, json);
    Ok(())
}

/// The answer of a pauses route: as the server sent it (what `--json` prints) and as
/// [`Pauses`] (what the table and the count read).
fn pauses_of(server: &Server, path: &str, text: &str) -> Result<(Value, Pauses), CliError> {
    let body: Value = serde_json::from_str(text)
        .map_err(|e| server.unexpected(path, &format!("not JSON: {e}")))?;
    let pauses = serde_json::from_value(body.clone())
        .map_err(|e| server.unexpected(path, &e.to_string()))?;
    Ok((body, pauses))
}

/// `barca pause --schedule NODE [--until WHEN]` (`pause`: `Some(until)`) or `barca resume
/// --schedule NODE` (`None`): `POST /schedule/{id}/pause` or `.../resume`, and the schedule's
/// entry as the route answers it, as JSON or one line.
pub(crate) async fn pause_schedule(
    server: &Server,
    schedule: &str,
    pause: Option<Option<&str>>,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let path = match pause {
        Some(Some(until)) => format!(
            "/schedule/{}/pause?until={}",
            encode(schedule),
            encode(until)
        ),
        Some(None) => format!("/schedule/{}/pause", encode(schedule)),
        None => format!("/schedule/{}/resume", encode(schedule)),
    };
    let text = server.post_json(&path, &json!({})).await?;
    let body: Value = serde_json::from_str(&text)
        .map_err(|e| server.unexpected(&format!("POST {path}"), &format!("not JSON: {e}")))?;
    let answer: SchedulePauseAnswer = serde_json::from_value(body.clone())
        .map_err(|e| server.unexpected(&format!("POST {path}"), &e.to_string()))?;
    let (id, paused, paused_until, cron, next_fire) = match &answer {
        SchedulePauseAnswer::Job(job) => (
            job.id.as_str(),
            job.paused,
            job.paused_until,
            Some(job.cron.as_str()),
            job.next_fire,
        ),
        SchedulePauseAnswer::Pause {
            id,
            paused,
            paused_until,
        } => (id.as_str(), *paused, *paused_until, None, None),
    };
    match pause {
        Some(_) => barca_core::errln!(
            "[barca] schedule {id} paused on {}: its ticks do not fire until {}",
            server.shown_url,
            match paused_until {
                Some(until) => format!(
                    "{} (or `barca resume --schedule {schedule}`)",
                    barca_core::outcome::rfc3339(until)
                ),
                None => format!("`barca resume --schedule {schedule}`"),
            }
        ),
        None => barca_core::errln!(
            "[barca] schedule {id} resumed on {}: it fires again from its next tick",
            server.shown_url
        ),
    }
    if json {
        barca_core::outln!(
            "{}",
            serde_json::to_string_pretty(&body).expect("a JSON value prints")
        );
    } else {
        barca_core::term::stdout_str(&schedule_line(id, paused, paused_until, cron, next_fire));
    }
    Ok(())
}

/// One schedule's entry in a line: `p.py:nightly: paused until resumed · cron 0 2 * * * · next
/// fire 2026-10-21T02:00:00Z`. Times are unix seconds, as `GET /schedule` gives them.
fn schedule_line(
    id: &str,
    paused: bool,
    paused_until: Option<i64>,
    cron: Option<&str>,
    next_fire: Option<i64>,
) -> String {
    let mut parts = vec![match (paused, paused_until) {
        (true, Some(until)) => format!("paused until {}", barca_core::outcome::rfc3339(until)),
        (true, None) => "paused until resumed".to_string(),
        _ => "not paused".to_string(),
    }];
    if let Some(cron) = cron {
        parts.push(format!("cron {cron}"));
    }
    if let Some(at) = next_fire {
        parts.push(format!("next fire {}", barca_core::outcome::rfc3339(at)));
    }
    format!("{id}: {}\n", parts.join(" · "))
}

/// The most keys asked of a server at once: `--all`, or a longer `--limit`, reads page after
/// page (the server lists at most this many, `KEYS_PAGE_MAX`).
const KEYS_PER_REQUEST: usize = 1000;

/// `GET /assets/{name}/partitions/keys` for `query`, `limit` keys from `offset`.
fn keys_path(
    target: &str,
    query: &barca_core::key_query::KeyQuery,
    offset: usize,
    limit: usize,
) -> String {
    use barca_core::key_query::KeySort;
    use barca_core::partition_grid::state;
    let mut path = format!(
        "/assets/{}/partitions/keys?limit={limit}&offset={offset}",
        encode(target)
    );
    if !query.states.is_empty() {
        let names: Vec<&str> = query
            .states
            .iter()
            .filter_map(|c| state::name(*c))
            .collect();
        path.push_str(&format!("&state={}", encode(&names.join(","))));
    }
    if query.needs_run {
        path.push_str("&needs_run=1");
    }
    for (dimension, values) in &query.values {
        path.push_str(&format!(
            "&value={}",
            encode(&format!("{dimension}={}", values.join(",")))
        ));
    }
    if let Some(range) = &query.range {
        path.push_str(&format!(
            "&range={}",
            encode(&format!("{}={}..{}", range.dimension, range.from, range.to))
        ));
    }
    if let Some(error) = query.error {
        path.push_str(&format!("&error={error}"));
    }
    if !query.text.is_empty() {
        path.push_str(&format!("&q={}", encode(&query.text)));
    }
    if let Some((name, _)) = KeySort::NAMES.iter().find(|(_, s)| *s == query.sort)
        && query.sort != KeySort::Key
    {
        path.push_str(&format!("&sort={name}"));
    }
    path
}

/// `barca status NODE --keys | --overview | --grid | --key KEY` with a server: the page of
/// keys (`GET /assets/{name}/partitions/keys`), the overview (`.../partitions/overview`), the
/// grid (`.../partitions/grid`) or one key (`.../partitions/key`), printed by the renderers a
/// local command uses.
pub(crate) async fn partition_keys(
    server: &Server,
    target: &str,
    files: &[PathBuf],
    view: crate::request::KeysView,
    json: bool,
    limit: Option<usize>,
    env: Option<&str>,
) -> Result<(), CliError> {
    use crate::request::KeysView;
    server.health(env).await?;
    server.deployed_files(files).await?;
    let rendered = match view {
        KeysView::Key { key, sample } => {
            let path = format!(
                "/assets/{}/partitions/key?key={}&sample={sample}",
                encode(target),
                encode(&key)
            );
            let detail: barca_core::partition_grid::PartitionKeyDetail =
                serde_json::from_value(server.get_json(&path).await?).map_err(|e| {
                    server.unexpected("GET /assets/{name}/partitions/key", &e.to_string())
                })?;
            barca_core::report::render_partition_key(&detail, json)
        }
        KeysView::Keys(query) => {
            // The server lists a bounded page: every key, or more than a page of them, is
            // read a page at a time and printed as one.
            let mut page: Option<barca_core::key_query::KeysPage> = None;
            loop {
                let have = page.as_ref().map_or(0, |p| p.keys.len());
                let want = limit.map_or(KEYS_PER_REQUEST, |l| (l - have).min(KEYS_PER_REQUEST));
                let path = keys_path(target, &query, query.offset + have, want);
                let next: barca_core::key_query::KeysPage =
                    serde_json::from_value(server.get_json(&path).await?).map_err(|e| {
                        server.unexpected("GET /assets/{name}/partitions/keys", &e.to_string())
                    })?;
                let more = next.truncated && !next.keys.is_empty();
                match &mut page {
                    Some(page) => page.extend(next),
                    None => page = Some(next),
                }
                let have = page.as_ref().map_or(0, |p| p.keys.len());
                if !more || limit.is_some_and(|l| have >= l) {
                    break;
                }
            }
            let page = page.expect("the loop reads at least one page");
            barca_core::report::render_partition_keys(&page, query.filters(), json)
        }
        KeysView::Overview => {
            let mut path = format!("/assets/{}/partitions/overview", encode(target));
            if let Some(limit) = limit {
                path.push_str(&format!("?limit={limit}"));
            }
            let overview: barca_core::key_query::KeysOverview =
                serde_json::from_value(server.get_json(&path).await?).map_err(|e| {
                    server.unexpected("GET /assets/{name}/partitions/overview", &e.to_string())
                })?;
            barca_core::report::render_partition_overview(&overview, json)
        }
        KeysView::Grid => {
            let path = format!("/assets/{}/partitions/grid", encode(target));
            let grid: barca_core::partition_grid::PartitionGrid =
                serde_json::from_value(server.get_json(&path).await?).map_err(|e| {
                    server.unexpected("GET /assets/{name}/partitions/grid", &e.to_string())
                })?;
            barca_core::report::render_partition_grid(&grid, limit, json)
        }
    };
    crate::print::emit(rendered);
    Ok(())
}

/// `barca resume TARGET [--downstream]` or `barca resume --all`: `POST /pauses/resume`.
pub(crate) async fn resume(
    server: &Server,
    target: Option<&str>,
    downstream: bool,
    all: bool,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    let text = server
        .post_json(
            "/pauses/resume",
            &json!({"target": target, "downstream": downstream, "all": all}),
        )
        .await?;
    let (body, pauses) = pauses_of(server, "POST /pauses/resume", &text)?;
    barca_core::errln!(
        "[barca] resumed {} node(s) on {}",
        pauses.removed.as_ref().map_or(0, Vec::len),
        server.shown_url
    );
    print_pauses(&body, &pauses, json);
    Ok(())
}

/// `body`: the server's answer as it sent it, printed with `--json`; `pauses`: the same,
/// read, for the table.
fn print_pauses(body: &Value, pauses: &Pauses, json: bool) {
    if json {
        barca_core::outln!(
            "{}",
            serde_json::to_string_pretty(body).expect("a JSON value prints")
        );
        return;
    }
    let paused = &pauses.paused;
    let mut out = format!("{} node(s) paused\n", paused.len());
    if !paused.is_empty() {
        out.push_str(&format!("\n{:<40} {:<22} {}\n", "NODE", "PAUSED_AT", "VIA"));
        for p in paused {
            out.push_str(&format!(
                "{:<40} {:<22} {}\n",
                p.id,
                barca_core::outcome::rfc3339(p.paused_at),
                p.via
            ));
        }
    }
    let schedules = &pauses.schedules;
    if !schedules.is_empty() {
        out.push_str(&format!(
            "\n{} schedule(s) paused\n\n{:<40} {:<22} {}\n",
            schedules.len(),
            "SCHEDULE",
            "PAUSED_AT",
            "UNTIL"
        ));
        for s in schedules {
            out.push_str(&format!(
                "{:<40} {:<22} {}\n",
                s.id,
                barca_core::outcome::rfc3339(s.paused_at),
                s.until
                    .map_or("resumed".to_string(), barca_core::outcome::rfc3339)
            ));
        }
    }
    barca_core::term::stdout_str(&out);
}
