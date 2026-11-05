//! `barca doctor` with a server: one check per thing that has to be right for this machine to
//! use it.

use super::Server;
use barca_core::config::ServerSource;
use barca_core::outcome::API_VERSION;
use serde_json::{Value, json};

/// `barca doctor` with a server (`--server`, or a configured one without `--local`): one check
/// per thing that has to be right for this machine to use it. A failed check's detail ends with
/// a line saying what to do. Nothing is changed on the server or here.
pub(crate) async fn doctor_checks(
    server: &Server,
    report: &mut barca_core::diagnostics::DoctorReport,
    env: Option<&str>,
) {
    let source = server.source.map(ServerSource::describe);
    let named = match source {
        Some(source) => format!("{} ({source})", server.shown_url),
        None => server.shown_url.clone(),
    };
    let cfg = barca_core::config::resolve(env).ok();
    let health = match server.get_json("/health").await {
        Ok(health) => health,
        Err(e) => {
            let fix = e.remediation.clone().unwrap_or_default();
            let fix = if server.implicit {
                format!("{fix} To check this machine only, pass --local.")
            } else {
                fix
            };
            report.add(
                "server",
                false,
                format!("{}\n{}", e.error.trim_start_matches("error: "), fix.trim()),
                json!({"url": server.shown_url, "source": source, "reachable": false}),
            );
            unsynced_posts(server, report, cfg.as_ref()).await;
            return;
        }
    };
    report.add(
        "server",
        true,
        format!("{named} answers"),
        json!({"url": server.shown_url, "source": source, "reachable": true}),
    );

    // Versions: the API version must match; another barca version with the same API works.
    let version = health["version"].as_str().unwrap_or("unknown");
    let api = health["api_version"].as_u64();
    let mine = env!("CARGO_PKG_VERSION");
    let api_ok = api == Some(u64::from(API_VERSION));
    let detail = if !api_ok {
        format!(
            "the server runs barca {version} (API version {}), this client barca {mine} (API \
             version {API_VERSION}): this client cannot use it\nInstall the same barca version \
             here and on the server.",
            api.map_or("none".to_string(), |v| v.to_string())
        )
    } else if version != mine {
        format!(
            "barca {version} on the server, {mine} here, both API version {API_VERSION}; the same \
             version on both prints the same output"
        )
    } else {
        format!("barca {version}, API version {API_VERSION}, the same as this client")
    };
    report.add(
        "server_version",
        api_ok,
        detail,
        json!({"version": version, "api_version": api, "client_version": mine,
               "client_api_version": API_VERSION}),
    );

    // The store: a result one side records must be readable by the other.
    let theirs = health["store_uri"].as_str();
    let ours = cfg
        .as_ref()
        .filter(|c| c.remote_artifacts())
        .map(|c| c.artifact_root.clone());
    let shown = |uri: Option<&str>| {
        uri.map_or("no store".to_string(), barca_core::transfer::diagnostic_uri)
    };
    let store_ok = match (theirs, ours.as_deref()) {
        (Some(t), Some(o)) => {
            barca_core::ledger::normalize_uri(t) == barca_core::ledger::normalize_uri(o)
        }
        _ => false,
    };
    let detail = match (theirs, ours.as_deref()) {
        _ if store_ok => format!("both write to {}", shown(theirs)),
        (None, _) => format!(
            "the server has no artifact store, so it cannot share results with this machine\n\
             Give the server's barca.toml a [remote] section (the same uri as this project's: \
             {}) and restart barca serve.",
            shown(ours.as_deref())
        ),
        (Some(t), None) => format!(
            "the server writes artifacts to {}, this project has no [remote] store\nAdd a \
             [remote] section to barca.toml with the uri the server's barca.toml has (barca docs \
             remote).",
            shown(Some(t))
        ),
        (Some(t), Some(o)) => format!(
            "the server writes artifacts to {}, this project to {}: a result one of them \
             records could not be read by the other\nSet [remote] uri in barca.toml to the uri \
             the server's barca.toml has, and use the server's environment.",
            shown(Some(t)),
            shown(Some(o))
        ),
    };
    report.add(
        "server_store",
        store_ok,
        detail,
        json!({"server": theirs.map(barca_core::transfer::diagnostic_uri),
               "project": ours.as_deref().map(barca_core::transfer::diagnostic_uri),
               "server_env": health["env"], "project_env": cfg.as_ref().map(|c| c.env.clone())}),
    );

    // Draining, and a metadata DB the server cannot use.
    let draining = health["status"].as_str() == Some("draining");
    let metadata_error = health["metadata_error"].as_str();
    let (ok, detail) = match (draining, metadata_error) {
        (true, _) => (
            false,
            "the server is shutting down (draining): it finishes its runs, starts none and \
             refuses posts\nStart barca serve again (or wait for the deploy that stopped it)."
                .to_string(),
        ),
        (false, Some(error)) => (
            false,
            format!(
                "the server cannot use its metadata DB: {}\nOn the server, put a backup back \
                 with `barca state restore` or start over with `barca state reset`.",
                error.lines().next().unwrap_or(error)
            ),
        ),
        (false, None) => (
            true,
            format!(
                "serving{}",
                if health["read_only"].as_bool() == Some(true) {
                    " read-only (no runs, no scheduler)"
                } else {
                    ""
                }
            ),
        ),
    };
    report.add(
        "server_health",
        ok,
        detail,
        json!({"status": health["status"], "read_only": health["read_only"],
               "metadata_error": health["metadata_error"]}),
    );

    // Files the server did not load: their nodes are missing from everything it answers.
    let errors: Vec<&Value> = health["load_errors"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let detail = if errors.is_empty() {
        "every deployed file loaded".to_string()
    } else {
        let files: Vec<String> = errors
            .iter()
            .map(|e| {
                format!(
                    "{}: {}",
                    e["file"].as_str().unwrap_or("?"),
                    e["error"]
                        .as_str()
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
                )
            })
            .collect();
        format!(
            "the server did not load {}, so its nodes are missing there: {}\nFix the file \
             and deploy it again: a `barca serve --watch` reloads it, otherwise restart barca \
             serve.",
            barca_core::report::plural(errors.len() as u64, "file"),
            files.join("; "),
        )
    };
    report.add(
        "server_sources",
        errors.is_empty(),
        detail,
        json!({"load_errors": health["load_errors"]}),
    );

    // The scheduler, and the schedules that need attention.
    scheduler(server, report, &health).await;
    unsynced_posts(server, report, cfg.as_ref()).await;
}

/// The `server_scheduler` check: running or not, and how many schedules are paused or failing
/// (`GET /schedule`, the numbers the web UI shows).
async fn scheduler(
    server: &Server,
    report: &mut barca_core::diagnostics::DoctorReport,
    health: &Value,
) {
    let wanted = health["scheduler"].as_bool().unwrap_or(false);
    let running = health["scheduler_enabled"].as_bool().unwrap_or(false);
    let jobs = match server.get_json("/schedule").await {
        Ok(Value::Array(jobs)) => jobs,
        Ok(_) | Err(_) if !running => Vec::new(),
        Ok(other) => {
            report.add(
                "server_scheduler",
                false,
                format!(
                    "GET /schedule answered {other}\nCheck that the URL points at barca serve."
                ),
                Value::Null,
            );
            return;
        }
        Err(e) => {
            report.add(
                "server_scheduler",
                false,
                format!("{}\n{}", e.error, e.remediation.unwrap_or_default()),
                Value::Null,
            );
            return;
        }
    };
    let ids = |pred: &dyn Fn(&Value) -> bool| -> Vec<String> {
        jobs.iter()
            .filter(|j| pred(j))
            .filter_map(|j| j["id"].as_str().map(str::to_string))
            .collect()
    };
    let paused = ids(&|j| j["paused"].as_bool() == Some(true));
    let failing = ids(&|j| j["failing"].as_bool() == Some(true));
    let data = json!({"running": running, "schedules": jobs.len(), "paused": paused,
                      "failing": failing});
    if wanted && !running {
        report.add(
            "server_scheduler",
            false,
            "the scheduler stopped on an error: no schedule fires\nRead the server's stderr \
             (and metadata_error in GET /health), fix the cause and restart barca serve.",
            data,
        );
        return;
    }
    if !wanted {
        report.add(
            "server_scheduler",
            true,
            "not running on this server (--no-schedule or --read-only): schedules do not fire \
             there",
            data,
        );
        return;
    }
    let mut detail = format!(
        "running; {}",
        barca_core::report::plural(jobs.len() as u64, "schedule")
    );
    if !paused.is_empty() {
        detail.push_str(&format!(
            ", {} paused ({})",
            paused.len(),
            paused.join(", ")
        ));
    }
    if failing.is_empty() {
        report.add("server_scheduler", true, detail, data);
        return;
    }
    let first = failing[0].as_str();
    let job = jobs.iter().find(|j| j["id"].as_str() == Some(first));
    let count = |key: &str| job.and_then(|j| j[key].as_u64()).unwrap_or(0);
    let command = match job.and_then(|j| j["kind"].as_str()) {
        Some("task") => "run",
        _ => "get",
    };
    detail.push_str(&format!(
        ", {} failing ({}): {first} failed {} ticks in a row (max_failures={}) and its ticks are \
         skipped\nRun `barca status --problems` for the error and fix it, then `barca {command} \
         {first} --server`: a success resets the count and the ticks resume.",
        failing.len(),
        failing.join(", "),
        count("failures"),
        count("max_failures"),
    ));
    report.add("server_scheduler", false, detail, data);
}

/// The `server_posts` check: runs this machine computed against the server's ledger whose post
/// did not arrive (`ledger_posts`, state `unsynced`), read from a snapshot of the metadata DB.
async fn unsynced_posts(
    server: &Server,
    report: &mut barca_core::diagnostics::DoctorReport,
    cfg: Option<&barca_core::config::ResolvedConfig>,
) {
    let Some(cfg) = cfg else {
        return;
    };
    let posts = match barca_core::db::DbSnapshot::take(&cfg.db_path).await {
        Ok(Some(snapshot)) => barca_core::ledger::posts::all(snapshot.path())
            .await
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let pending: Vec<_> = posts
        .into_iter()
        .filter(|p| p.state == barca_core::ledger::posts::UNSYNCED && p.server == server.url)
        .collect();
    let runs: Vec<&str> = pending.iter().map(|p| p.client_run_id.as_str()).collect();
    let data = json!({"unsynced": runs.len(), "runs": runs,
                      "last_error": pending.last().and_then(|p| p.error.as_ref())
                          .map(|e| e.replace(&server.url, &server.shown_url))});
    if pending.is_empty() {
        report.add(
            "server_posts",
            true,
            "every run this machine computed against the server is recorded there",
            data,
        );
        return;
    }
    let why = pending
        .last()
        .and_then(|p| p.error.as_deref())
        .map(|e| {
            format!(
                " (last: {})",
                e.lines()
                    .next()
                    .unwrap_or(e)
                    .replace(&server.url, &server.shown_url)
            )
        })
        .unwrap_or_default();
    report.add(
        "server_posts",
        false,
        format!(
            "{} computed here {} not recorded on the server yet: {}{why}\nThe next `barca get` or \
             `barca run` against the server sends {} first; `barca status --state unsynced` lists \
             the results.",
            barca_core::report::plural(runs.len() as u64, "run"),
            if runs.len() == 1 { "is" } else { "are" },
            runs.join(", "),
            if runs.len() == 1 { "it" } else { "them" },
        ),
        data,
    );
}
