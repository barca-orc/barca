//! The health words the web UI's test fixtures use (#959). The UI decides no health: its unit
//! tests and its kit page still need rows "as the server would serve them", and a fixture
//! that spelled the health table by hand would be a second copy of [`crate::health`].
//!
//! This test classifies one row alone (no graph, no signals, no schedule, no tick) for every
//! combination of the facts a fixture states, and keeps the answers in
//! `ui/src/lib/testdata/health-alone.json`, which `ui/src/lib/testdata/served.ts` looks up.
//! The weights of "Distance from fresh" (#1028) are kept the same way, in
//! `ui/src/lib/testdata/weights.json`, which `ui/src/lib/testdata/rollup.ts` states a fixture
//! entry's `weight` with: the app holds no weight table, and neither does a fixture.
//!
//! A whole project is kept the same way (#1056 F4). `testdata/ui_fixture/` holds two pipeline
//! files with a node in each state the UI shows; the test copies them into a scratch project,
//! records a history for it (runs, failures, ticks, a pause, a pin, a queue) at fixed times,
//! and writes what this crate says of it at one fixed instant to
//! `ui/src/lib/testdata/served/`: the rows of `GET /state`, `GET /state/rollup`, the run list,
//! four run details with their explanations, the pauses and the pins, and the explanations of
//! runs no recorded history holds (`run-explanations.json`). `ui/src/lib/testdata/project.ts`
//! types them; the UI's unit tests and kit assert on them instead of on values someone typed.
//!
//! What the files are, exactly: the computed values (cache state, health, reason, cause, root,
//! estimates, schedule standing, every count, weight and line of the rollup, a run's
//! explanation and list line, the activity) come from this crate's public functions, as a
//! server that reads this database answers them (a read-only one: the schedules and the queue
//! are the recorded ones, and the test checks its rows against `health::attach_from_db`, what
//! `barca status` prints). The fields `barca-server` joins on in its own crate (`NodeState`
//! beyond the status row, `RunSummary`, the record half of `RunView`) are joined here from the
//! same reads, in this file: a field added there is added here by hand until those views
//! are core functions (#1056 B2).
//!
//! When the health table, a weight, a wording or the fixture project changes, regenerate the
//! files, review the diff (it is what the UI will be told), and the UI's fixtures follow:
//!
//! ```text
//! BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-service ui_fixture
//! ```

use crate::cache_state::CacheState;
use crate::health::{Checks, Evidence, classify};
use crate::status::{LastMaterialization, NodeStatus, PartitionState};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// The cache states, as their wire words.
const STATES: &[CacheState] = &[
    CacheState::Cached,
    CacheState::Unsynced,
    CacheState::Stale,
    CacheState::NeverRun,
    CacheState::Partial,
    CacheState::Blocked,
    CacheState::Unknown,
    CacheState::AlwaysRuns,
];

/// What a fixture says of the node's last attempt.
const LAST: &[&str] = &["none", "success", "failed", "cancelled"];

fn row(state: CacheState, last: &str, keys_failed: bool) -> NodeStatus {
    let mut node: NodeStatus = serde_json::from_value(serde_json::json!({
        "id": "p.py:a", "name": "a", "kind": "asset", "inputs": [], "partitioned": false,
        "cache": {"state": "unknown", "reason": "", "detail": ""},
        "last_materialization": null, "shape": null, "env": [],
    }))
    .unwrap();
    node.cache.state = state;
    if last != "none" {
        node.last_materialization = Some(LastMaterialization {
            skip_reason: None,
            content_hash: None,
            status: last.to_string(),
            created_at: "2026-11-16 10:00:00".to_string(),
            elapsed_seconds: None,
            run_hash: None,
            artifact: None,
            format: None,
            size_bytes: None,
            rows: None,
            partition: None,
            error: (last == "failed").then(|| "ValueError: no rows".to_string()),
        });
    }
    if keys_failed {
        node.partitioned = true;
        node.partitions = Some(PartitionState {
            total: 4,
            cached: 2,
            missing: 2,
            failed: 1,
            ..serde_json::from_value(serde_json::json!({
                "total": 0, "cached": 0, "missing": 0, "missing_keys": [],
            }))
            .unwrap()
        });
    }
    node
}

/// `{"<cache state>|<last attempt>|<keys failed>": {"health": …, "health_reason": …, "cause": …,
/// "last_attempt": …}}`.
fn table() -> String {
    let mut out: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for state in STATES {
        for last in LAST {
            for keys_failed in [false, true] {
                let mut nodes = vec![row(*state, last, keys_failed)];
                classify(
                    &mut nodes,
                    Evidence {
                        ages: &HashMap::new(),
                        pinned: &HashSet::new(),
                        late: &HashSet::new(),
                        checks: &Checks::default(),
                        path: crate::refresh_path::PathEvidence::none(),
                        timing: crate::estimates::Timing::none(),
                        now: 0,
                    },
                );
                let word = serde_json::to_value(state).unwrap();
                out.insert(
                    format!("{}|{last}|{keys_failed}", word.as_str().unwrap()),
                    serde_json::json!({
                        "health": nodes[0].health,
                        "health_reason": nodes[0].health_reason,
                        "cause": nodes[0].cause,
                        "last_attempt": nodes[0].last_attempt,
                    }),
                );
            }
        }
    }
    let mut text = serde_json::to_string_pretty(&out).unwrap();
    text.push('\n');
    text
}

#[test]
fn the_ui_fixture_health_table_is_what_classify_says() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/lib/testdata/health-alone.json");
    let actual = table();
    if std::env::var("BARCA_UPDATE_SNAPSHOTS").is_ok_and(|v| !v.is_empty() && v != "0") {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "{} is not what `health::classify` says of a row alone. The UI's fixtures read it \
         (ui/src/lib/testdata/served.ts): regenerate it with \
         `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-service ui_fixture`, then run the UI's \
         unit tests (`pnpm --dir ui test`).",
        path.display()
    );
}

/// `{"fresh": 1, "<cause>": <weight>, …}`: what one node of each part weighs.
fn weights() -> String {
    let mut out: BTreeMap<&str, usize> = crate::health::Cause::ALL
        .iter()
        .map(|c| (c.as_str(), c.weight()))
        .collect();
    out.insert("fresh", crate::rollup::FRESH_WEIGHT);
    let mut text = serde_json::to_string_pretty(&out).unwrap();
    text.push('\n');
    text
}

#[test]
fn the_ui_fixture_weights_are_the_rollups() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/lib/testdata/weights.json");
    let actual = weights();
    if std::env::var("BARCA_UPDATE_SNAPSHOTS").is_ok_and(|v| !v.is_empty() && v != "0") {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "{} is not what `Cause::weight` and `FRESH_WEIGHT` say. The UI's fixtures read it \
         (ui/src/lib/testdata/rollup.ts): regenerate it with \
         `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-service ui_fixture`, then run the UI's \
         unit tests (`pnpm --dir ui test`).",
        path.display()
    );
}

// ── A served project (#1056 F4) ────────────────────────────────────────────────

/// The instant every view below is answered at.
const NOW: &str = "2026-11-17T14:57:00Z";

fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(text).unwrap().to_utc()
}

/// `text` (RFC 3339) as the database records an instant: `YYYY-MM-DD HH:MM:SS`, UTC.
fn sql_time(text: &str) -> String {
    at(text).format("%Y-%m-%d %H:%M:%S").to_string()
}

async fn sql(db: &str, statement: &str, params: Vec<libsql::Value>) {
    let _g = crate::db::db_guard().await;
    let (_db, conn) = crate::db::open_conn(db).await.unwrap();
    conn.execute(statement, params)
        .await
        .unwrap_or_else(|e| panic!("{statement}: {e}"));
}

fn text(value: &str) -> libsql::Value {
    libsql::Value::Text(value.to_string())
}

fn opt(value: Option<&str>) -> libsql::Value {
    value.map_or(libsql::Value::Null, text)
}

/// One recorded step: a row of `materializations`, as a run writes it.
struct Step<'a> {
    run: &'a str,
    node: &'a str,
    hash: &'a str,
    at: &'a str,
    seconds: f64,
    /// `None` for a success; the error of a failed attempt.
    error: Option<(&'a str, &'a str)>,
}

async fn record_step(root: &Path, db: &str, step: Step<'_>) {
    let ok = step.error.is_none();
    // `orders.py:sales_by_day[day=2026-11-13]` is stored under `orders.py--sales_by_day_day_2026-11-13`.
    let dir = step
        .node
        .rsplit('/')
        .next()
        .unwrap()
        .replace(':', "--")
        .replace("[", "_")
        .replace(['=', ','], "_")
        .replace(']', "");
    let artifact = root.join(format!(".barca/artifacts/{dir}/{}.json", step.hash));
    if ok {
        // A cached result is one whose artifact is still there to read.
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, "{}").unwrap();
    }
    let artifact = artifact.display().to_string();
    sql(
        db,
        "INSERT INTO materializations (node_id, run_hash, run_id, status, created_at, \
         elapsed_seconds, artifact_path, artifact_format, artifact_size_bytes, output_hash, \
         error_type, error_message, error_traceback, error_phase, attempts) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 1)",
        vec![
            text(step.node),
            text(step.hash),
            text(step.run),
            text(if ok { "success" } else { "failed" }),
            text(&sql_time(step.at)),
            libsql::Value::Real(step.seconds),
            opt(ok.then_some(artifact.as_str())),
            opt(ok.then_some("json")),
            if ok {
                libsql::Value::Integer(82)
            } else {
                libsql::Value::Null
            },
            // What the step returned, by content: any stable word, one per (node, run hash).
            opt(ok.then_some(&format!("out-{}", &step.hash[..12]))),
            opt(step.error.map(|_| "WorkerError")),
            opt(step.error.map(|(message, _)| message)),
            opt(step.error.map(|(_, traceback)| traceback)),
            opt(step.error.map(|_| "user_code")),
        ],
    )
    .await;
}

/// The fixture project in a scratch directory, with its recorded history.
struct Project {
    dir: tempfile::TempDir,
    files: Vec<String>,
    cfg: crate::config::ResolvedConfig,
    dag: crate::dag::Dag,
}

impl Project {
    fn id(&self, name: &str) -> String {
        format!("{}/{name}", self.dir.path().display())
    }

    fn db(&self) -> &str {
        &self.cfg.db_path
    }

    async fn status(&self) -> crate::status::StatusResult {
        crate::status::status_from_dag(&self.cfg, &[], &self.dag, Path::new("python3"), 0, false)
            .await
            .unwrap()
    }

    /// The run hash a run of `node` would compute now, as `barca status` reports it.
    async fn hash(&self, node: &str) -> String {
        let id = self.id(node);
        self.status()
            .await
            .nodes
            .into_iter()
            .find(|n| n.id == id)
            .and_then(|n| n.cache.run_hash)
            .unwrap_or_else(|| panic!("no run hash for {node}"))
    }

    /// Record `node` succeeding in `run` with the result a run would reuse now.
    async fn ok(&self, run: &str, node: &str, at: &str, seconds: f64) {
        let hash = self.hash(node).await;
        record_step(
            self.dir.path(),
            self.db(),
            Step {
                run,
                node: &self.id(node),
                hash: &hash,
                at,
                seconds,
                error: None,
            },
        )
        .await;
    }

    /// The run hash of one key of a partitioned node with no inputs.
    fn key_hash(&self, node: &str, key: &str) -> String {
        let definition = &self
            .dag
            .get_node(&self.id(node))
            .unwrap()
            .fingerprint
            .definition_hash;
        barca_model::hash::run_hash(definition, Some(key), &[], None, None)
    }

    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/ui_fixture");
        let mut files = Vec::new();
        for (name, changed) in [
            ("orders.py", "2026-11-17T13:10:00Z"),
            ("billing.py", "2026-11-16T23:50:00Z"),
        ] {
            let to = dir.path().join(name);
            std::fs::copy(source.join(name), &to).unwrap();
            // When the source changed is part of an estimate: a fixed instant, not this checkout's.
            std::fs::File::options()
                .write(true)
                .open(&to)
                .unwrap()
                .set_modified(at(changed).into())
                .unwrap();
            files.push(to.display().to_string());
        }
        let dag = crate::load::build_dag(&files, Path::new("python3"))
            .await
            .unwrap();
        let mut cfg =
            crate::config::resolve_with(None, dir.path(), crate::config::Vars::NONE).unwrap();
        cfg.db_path = dir.path().join(".barca/metadata.db").display().to_string();
        std::fs::create_dir_all(dir.path().join(".barca")).unwrap();
        crate::db::init_db(&cfg.db_path).await.unwrap();
        Project {
            dir,
            files,
            cfg,
            dag,
        }
    }
}

/// A run's row, as a run records it once it ended.
struct Run<'a> {
    id: &'a str,
    command: &'a str,
    file: &'a str,
    target: Option<&'a str>,
    status: &'a str,
    /// Planned, executed, cached.
    steps: (i64, i64, i64),
    started: &'a str,
    seconds: f64,
    /// `manual`, `api` or `schedule`; then who asked and the schedule's slot.
    by: &'a str,
    principal: Option<&'a str>,
    slot: Option<(&'a str, &'a str)>,
    reasons: Option<serde_json::Value>,
}

impl Project {
    async fn run(&self, run: Run<'_>) {
        let finished =
            at(run.started) + chrono::Duration::milliseconds((run.seconds * 1000.0) as i64);
        let target = run
            .target
            .map(|name| self.id(&format!("{}:{name}", run.file)));
        sql(
            self.db(),
            "INSERT INTO runs (run_id, command, files, target, job, status, steps_total, \
             steps_executed, steps_cached, started_at, finished_at, elapsed_seconds, \
             triggered_by, principal, schedule_id, scheduled_at, plan_reasons) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            vec![
                text(run.id),
                text(run.command),
                text(&serde_json::json!([self.id(run.file)]).to_string()),
                opt(run.target),
                text(target.as_deref().unwrap_or("all")),
                text(run.status),
                libsql::Value::Integer(run.steps.0),
                libsql::Value::Integer(run.steps.1),
                libsql::Value::Integer(run.steps.2),
                text(&sql_time(run.started)),
                text(&finished.format("%Y-%m-%d %H:%M:%S").to_string()),
                libsql::Value::Real(run.seconds),
                text(run.by),
                opt(run.principal),
                opt(run.slot.map(|(schedule, _)| self.id(schedule)).as_deref()),
                opt(run.slot.map(|(_, slot)| slot)),
                opt(run
                    .reasons
                    .map(|r| r.to_string().replace("@/", &self.id("")))
                    .as_deref()),
            ],
        )
        .await;
    }

    async fn failed(&self, run: &str, node: &str, at: &str, error: &str, line: u32) {
        let hash = match node.split_once('[') {
            Some((of, key)) => self.key_hash(of, key.trim_end_matches(']')),
            None => self.hash(node).await,
        };
        let (file, name) = node.split('[').next().unwrap().split_once(':').unwrap();
        let traceback = format!("{error}\n  File \"{file}\", line {line}, in {name}");
        record_step(
            self.dir.path(),
            self.db(),
            Step {
                run,
                node: &self.id(node),
                hash: &hash,
                at,
                seconds: 0.3,
                error: Some((error, &traceback)),
            },
        )
        .await;
    }

    /// A step that succeeded with a result nothing would reuse now: its code or inputs were others then.
    async fn outdated(&self, run: &str, node: &str, at: &str, seconds: f64) {
        let hash = format!("{:0>64}", node.len());
        record_step(
            self.dir.path(),
            self.db(),
            Step {
                run,
                node: &self.id(node),
                hash: &hash,
                at,
                seconds,
                error: None,
            },
        )
        .await;
    }

    /// One tick of `schedule` at `slot` that started `run` and ended `status`.
    async fn tick(&self, schedule: &str, slot: &str, run: &str, status: &str) {
        let id = self.id(schedule);
        let slot = at(slot).timestamp();
        crate::db::upsert_schedule_state(self.db(), &id, slot)
            .await
            .unwrap();
        crate::db::record_tick(
            self.db(),
            crate::db::NewTick {
                schedule_id: &id,
                scheduled_at: slot,
                action: crate::db::TickDecision::Fired,
                skipped_for: None,
                run_id: Some(run),
            },
        )
        .await
        .unwrap();
        crate::db::record_tick_started(self.db(), &id, slot, run, slot as f64 + 1.0, 1.0)
            .await
            .unwrap();
        let outcome = match status {
            "success" => crate::db::TickTargetOutcome::Rebuilt,
            _ => crate::db::TickTargetOutcome::Failed,
        };
        crate::db::record_tick_finished(
            self.db(),
            &id,
            slot,
            run,
            slot as f64 + 3.0,
            status,
            &[(id.clone(), outcome)],
        )
        .await
        .unwrap();
    }

    /// The history: what ran, what failed, what is held, and the server that was serving.
    async fn record(&self) {
        let never_run = |nodes: u64, steps: u64, example: &str| {
            serde_json::json!({"nodes": nodes, "steps": steps, "omitted": 0, "reasons": [
                {"category": "never_run", "subject": null, "nodes": nodes, "steps": steps, "example": format!("@/{example}")},
            ]})
        };
        // The first run of everything in orders.py, a day and a half ago.
        let first = "a1f0c0000001";
        self.run(Run {
            id: first,
            command: "get",
            file: "orders.py",
            target: None,
            status: "success",
            steps: (9, 9, 0),
            started: "2026-11-16T09:00:00Z",
            seconds: 42.3,
            by: "manual",
            principal: None,
            slot: None,
            reasons: Some(never_run(7, 9, "orders.py:customers")),
        })
        .await;
        for (node, seconds) in [
            ("orders.py:orders_feed", 0.4),
            ("orders.py:orders", 3.2),
            ("orders.py:customers", 1.1),
            ("orders.py:fx_rates", 0.6),
        ] {
            self.ok(first, node, "2026-11-16T09:00:05Z", seconds).await;
        }
        self.outdated(
            first,
            "orders.py:orders_enriched",
            "2026-11-16T09:00:12Z",
            6.0,
        )
        .await;
        self.outdated(
            first,
            "orders.py:revenue_hourly",
            "2026-11-16T09:00:20Z",
            8.0,
        )
        .await;
        // Three of the five days: each key is a step of its own.
        for day in ["2026-11-13", "2026-11-14", "2026-11-15"] {
            let key = format!("day={day}");
            let hash = self.key_hash("orders.py:sales_by_day", &key);
            let node = self.id(&format!("orders.py:sales_by_day[{key}]"));
            record_step(
                self.dir.path(),
                self.db(),
                Step {
                    run: first,
                    node: &node,
                    hash: &hash,
                    at: "2026-11-16T09:00:30Z",
                    seconds: 2.5,
                    error: None,
                },
            )
            .await;
        }
        // billing.py: one clean run, then a task that failed and two failed attempts of `invoices`.
        let billing = "a1f0c0000002";
        self.run(Run {
            id: billing,
            command: "get",
            file: "billing.py",
            target: Some("invoice_totals"),
            status: "success",
            steps: (2, 2, 0),
            started: "2026-11-16T09:30:00Z",
            seconds: 5.1,
            by: "manual",
            principal: None,
            slot: None,
            reasons: Some(never_run(2, 2, "billing.py:invoices")),
        })
        .await;
        // Its code was edited that night (the file's time): these results are no longer reused.
        self.outdated(billing, "billing.py:invoices", "2026-11-16T09:30:03Z", 2.8)
            .await;
        self.outdated(
            billing,
            "billing.py:invoice_totals",
            "2026-11-16T09:30:05Z",
            1.9,
        )
        .await;
        let check = "a1f0c0000003";
        self.run(Run {
            id: check, command: "run", file: "billing.py", target: Some("check__invoices"), status: "failed", steps: (2, 1, 1),
            started: "2026-11-16T10:00:00Z", seconds: 1.2, by: "manual", principal: None, slot: None,
            reasons: Some(serde_json::json!({"nodes": 1, "steps": 1, "omitted": 0, "reasons": [
                {"category": "task", "subject": null, "nodes": 1, "steps": 1, "example": "@/billing.py:check__invoices"},
            ]})),
        })
        .await;
        self.failed(
            check,
            "billing.py:check__invoices",
            "2026-11-16T10:00:01Z",
            "AssertionError: 3 rows with a negative total",
            30,
        )
        .await;
        for (id, started, by, principal) in [
            ("a1f0c0000004", "2026-11-17T00:40:00Z", "manual", None),
            (
                "a1f0c0000005",
                "2026-11-17T13:55:00Z",
                "api",
                Some("token:ci-bot"),
            ),
        ] {
            self.run(Run {
                id, command: "get", file: "billing.py", target: Some("invoice_totals"), status: "failed", steps: (2, 1, 0),
                started, seconds: 0.9, by, principal, slot: None,
                reasons: Some(serde_json::json!({"nodes": 1, "steps": 1, "omitted": 0, "reasons": [
                    {"category": "code_changed", "subject": null, "nodes": 1, "steps": 1, "example": "@/billing.py:invoices"},
                ]})),
            })
            .await;
            let failed_at = (at(started) + chrono::Duration::seconds(1)).to_rfc3339();
            self.failed(
                id,
                "billing.py:invoices",
                &failed_at,
                "ValueError: 3 rows with a negative total",
                11,
            )
            .await;
        }
        crate::db::insert_logs(
            self.db(),
            "a1f0c0000005",
            &[
                ("reading 12,408 invoices", barca_model::LogStream::Stdout),
                (
                    "3 rows with a negative total",
                    barca_model::LogStream::Stderr,
                ),
            ]
            .map(|(line, stream)| crate::run_logs::RecordedLine {
                node_id: self.id("billing.py:invoices"),
                line: line.to_string(),
                stream,
                ts: Some("2026-11-17T13:55:01Z".to_string()),
            }),
        )
        .await
        .unwrap();
        // One day of `sales_by_day` asked for on its own, and it failed.
        let day = "a1f0c0000006";
        self.run(Run {
            id: day,
            command: "get",
            file: "orders.py",
            target: Some("sales_by_day"),
            status: "failed",
            steps: (1, 1, 0),
            started: "2026-11-17T02:00:00Z",
            seconds: 0.7,
            by: "manual",
            principal: None,
            slot: None,
            reasons: Some(never_run(1, 1, "orders.py:sales_by_day")),
        })
        .await;
        self.failed(
            day,
            "orders.py:sales_by_day[day=2026-11-16]",
            "2026-11-17T02:00:01Z",
            "KeyError: 'units'",
            48,
        )
        .await;
        // A task, run by hand.
        let publish = "a1f0c0000007";
        self.run(Run {
            id: publish, command: "run", file: "orders.py", target: Some("publish_dashboard"), status: "success", steps: (6, 1, 5),
            started: "2026-11-16T09:10:00Z", seconds: 2.0, by: "manual", principal: None, slot: None,
            reasons: Some(serde_json::json!({"nodes": 1, "steps": 1, "omitted": 0, "reasons": [
                {"category": "task", "subject": null, "nodes": 1, "steps": 1, "example": "@/orders.py:publish_dashboard"},
            ]})),
        })
        .await;
        self.ok(
            publish,
            "orders.py:publish_dashboard",
            "2026-11-16T09:10:02Z",
            1.4,
        )
        .await;

        // The schedules. `orders_feed` fires every 15 minutes and is on time; `revenue_hourly`
        // fires on the hour: its 14:00 tick still waits for the one run slot.
        for (slot, run) in [
            ("2026-11-17T14:15:00Z", "5c4ed0001415"),
            ("2026-11-17T14:30:00Z", "5c4ed0001430"),
            ("2026-11-17T14:45:00Z", "5c4ed0001445"),
        ] {
            self.run(Run {
                id: run, command: "get", file: "orders.py", target: Some("orders_feed"), status: "success", steps: (1, 1, 0),
                started: slot, seconds: 0.5, by: "schedule", principal: None, slot: Some(("orders.py:orders_feed", slot)),
                reasons: Some(serde_json::json!({"nodes": 1, "steps": 1, "omitted": 0, "reasons": [
                    {"category": "sensor", "subject": null, "nodes": 1, "steps": 1, "example": "@/orders.py:orders_feed"},
                ]})),
            })
            .await;
            self.ok(
                run,
                "orders.py:orders_feed",
                &(at(slot) + chrono::Duration::seconds(2)).to_rfc3339(),
                0.4,
            )
            .await;
            self.tick("orders.py:orders_feed", slot, run, "success")
                .await;
        }
        for (slot, run) in [
            ("2026-11-17T12:00:00Z", "5c4ed0001200"),
            ("2026-11-17T13:00:00Z", "5c4ed0001300"),
        ] {
            self.run(Run {
                id: run,
                command: "get",
                file: "orders.py",
                target: Some("revenue_hourly"),
                status: "success",
                steps: (5, 1, 4),
                started: slot,
                seconds: 9.0,
                by: "schedule",
                principal: None,
                slot: Some(("orders.py:revenue_hourly", slot)),
                reasons: None,
            })
            .await;
            self.outdated(
                run,
                "orders.py:revenue_hourly",
                &(at(slot) + chrono::Duration::seconds(9)).to_rfc3339(),
                8.0,
            )
            .await;
            self.tick("orders.py:revenue_hourly", slot, run, "success")
                .await;
        }

        // Held: one asset paused.
        crate::pauses::pause(
            &self.cfg,
            &[self.id("orders.py:customers")],
            "customers",
            at("2026-11-17T09:00:00Z").timestamp(),
        )
        .await
        .unwrap();

        // A server with one run slot has been serving since the first run, its scheduler
        // with it, and a run someone asked for waits in its queue.
        let now = at(NOW).timestamp();
        let schedules = crate::schedule::jobs_from_dag(&self.dag)
            .into_iter()
            .map(|job| job.id)
            .collect();
        crate::liveness::record_serving(self.db(), at("2026-11-15T08:55:00Z").timestamp(), 1, 4)
            .await
            .unwrap();
        crate::liveness::record_scheduler(
            self.db(),
            at("2026-11-15T08:55:00Z").timestamp(),
            schedules,
            "utc",
        )
        .await
        .unwrap();
        crate::liveness::heartbeat(self.db(), now - 20)
            .await
            .unwrap();
        // `nightly_export` fired yesterday at 06:00; no tick was decided for today's slot.
        let nightly = ("2026-11-16T06:00:00Z", "5c4ed0000600");
        self.run(Run {
            id: nightly.1,
            command: "run",
            file: "orders.py",
            target: Some("nightly_export"),
            status: "success",
            steps: (1, 1, 0),
            started: nightly.0,
            seconds: 3.0,
            by: "schedule",
            principal: None,
            slot: Some(("orders.py:nightly_export", nightly.0)),
            reasons: None,
        })
        .await;
        self.ok(
            nightly.1,
            "orders.py:nightly_export",
            "2026-11-16T06:00:03Z",
            2.6,
        )
        .await;
        self.tick("orders.py:nightly_export", nightly.0, nightly.1, "success")
            .await;
        // The slot is taken by a long backfill someone asked for at 13:58.
        let backfill = "a1f0c0000008";
        sql(
            self.db(),
            "INSERT INTO runs (run_id, command, files, target, job, status, steps_total, started_at, triggered_by) \
             VALUES (?1, 'get', ?2, 'sales_report', ?3, 'running', 7, ?4, 'manual')",
            vec![
                text(backfill),
                text(&serde_json::json!([self.id("orders.py")]).to_string()),
                text(&self.id("orders.py:sales_report")),
                text(&sql_time("2026-11-17T13:58:00Z")),
            ],
        )
        .await;
        let queued = |node: &str, trigger: serde_json::Value| {
            serde_json::json!({
                "command": "get", "policy": {"kind": "cache_aware"}, "trigger": trigger,
                "nodes": [self.id(node)],
            })
            .to_string()
        };
        let by_hand = serde_json::json!({"triggered_by": "manual"});
        crate::run_queue::push(
            self.db(),
            backfill,
            &queued("orders.py:sales_report", by_hand),
            "2026-11-17T13:58:00Z",
        )
        .await
        .unwrap();
        crate::run_queue::mark_running(self.db(), backfill)
            .await
            .unwrap();
        // Behind it: the 14:00 tick of `revenue_hourly`, then a run of `orders_enriched`.
        let hourly = self.id("orders.py:revenue_hourly");
        let slot = at("2026-11-17T14:00:00Z").timestamp();
        crate::db::upsert_schedule_state(self.db(), &hourly, slot)
            .await
            .unwrap();
        crate::db::record_tick(
            self.db(),
            crate::db::NewTick {
                schedule_id: &hourly,
                scheduled_at: slot,
                action: crate::db::TickDecision::Fired,
                skipped_for: None,
                run_id: Some("5c4ed0001400"),
            },
        )
        .await
        .unwrap();
        let tick = serde_json::json!({"triggered_by": "schedule", "schedule_id": hourly, "scheduled_at": "2026-11-17T14:00:00Z"});
        crate::run_queue::push(
            self.db(),
            "5c4ed0001400",
            &queued("orders.py:revenue_hourly", tick),
            "2026-11-17T14:00:00Z",
        )
        .await
        .unwrap();
        let api = serde_json::json!({"triggered_by": "api", "principal": "token:ci-bot"});
        crate::run_queue::push(
            self.db(),
            "a1f0c0000009",
            &queued("orders.py:orders_enriched", api),
            "2026-11-17T14:56:30Z",
        )
        .await
        .unwrap();
    }
}

impl Project {
    /// `barca pin fx_rates`: held at the version it has.
    async fn pin(&self) {
        crate::db::set_pin(
            self.db(),
            &crate::db::AssetPin {
                node_id: self.id("orders.py:fx_rates"),
                run_hash: self.hash("orders.py:fx_rates").await,
                pinned_at: "2026-11-16T12:00:00Z".to_string(),
                pinned_by: Some("ada@laptop".to_string()),
                reason: Some("month-end close".to_string()),
                run_id: None,
                upload: None,
            },
        )
        .await
        .unwrap();
    }

    /// `GET /state` and `GET /state/rollup` as a server that reads this database answers them
    /// at [`NOW`] (a read-only one: the schedules and the queue are the recorded ones).
    ///
    /// The inputs are filled as `health::from_db` fills them, and the test checks the two
    /// agree: `barca status` of the same database says the same of every node. What a state
    /// row holds beyond its status row (`barca-server`'s `NodeState`) is joined here from the
    /// same reads the server joins it from.
    async fn state(&self) -> (serde_json::Value, serde_json::Value) {
        let (db, dag, now) = (self.db(), &self.dag, at(NOW));
        let mut result = self.status().await;
        let jobs = crate::schedule::jobs_from_dag(dag);
        let ids: Vec<String> = jobs.iter().map(|job| job.id.clone()).collect();
        let seen = crate::liveness::recorded(db).await.unwrap();
        let ages = crate::db::node_ages(db).await.unwrap();
        let pinned: HashSet<String> = crate::pins::list(db)
            .await
            .unwrap()
            .into_iter()
            .map(|pin| pin.node_id)
            .collect();
        let paused: HashSet<String> = crate::pauses::list(db)
            .await
            .unwrap()
            .into_iter()
            .map(|pause| pause.id)
            .collect();
        let pauses = crate::db::get_schedule_pauses(db).await.unwrap();
        let failures = crate::db::get_schedule_failures(db).await.unwrap();
        let ticks = crate::schedule_standing::recorded(db, &ids).await.unwrap();
        let zone = seen.zone().expect("the scheduler recorded its zone");
        let observed = crate::liveness::observe(&jobs, &zone, now, &seen);
        let checks = crate::health::checks(db, dag, &ages, &zone).await.unwrap();
        let queue = crate::health::Queue::recorded(
            crate::health::recorded_queue(db, dag, &ticks, &seen, &observed, now.timestamp())
                .await
                .unwrap(),
            self.cfg.server_max_runs,
        );
        let node_ids: Vec<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
        let durations = crate::health::durations(
            crate::db::recent_durations(db, &node_ids, crate::health::DURATION_WINDOW)
                .await
                .unwrap(),
        );
        let paths = crate::refresh_path::Paths::of(dag);
        let source_times = crate::estimates::source_times_of(dag);
        let firing = HashMap::new();
        let health = crate::health::Inputs {
            ages: &ages,
            pinned: &pinned,
            paused: &paused,
            checks: &checks,
            paths: &paths,
            schedules: crate::refresh_path::ScheduleInputs {
                jobs: &jobs,
                zone: &zone,
                now,
                ticks: &ticks,
                failures: &failures,
                pauses: &pauses,
                firing: &firing,
                observed: &observed,
            },
            durations: &durations,
            queue: &queue,
            source_times: &source_times,
        }
        .assemble();
        health.attach_schedules(&mut result.nodes);
        let running = HashSet::new();
        health.attach(&mut result, &running);

        // The same database through `barca status`: one definition, two hosts.
        let mut local = self.status().await;
        crate::health::attach_from_db_with_schedules(&mut local, dag, &self.cfg, &zone, now)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&result.nodes).unwrap(),
            serde_json::to_value(&local.nodes).unwrap(),
            "the fixture's health inputs are not the ones `health::attach_from_db` fills"
        );
        let mut summed = self.status().await;
        let cli = crate::health::rollup_from_db(&mut summed, dag, &self.cfg, &zone, now)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(health.rollup(&result, &dag.groups, &running, now, false))
                .unwrap(),
            serde_json::to_value(cli).unwrap(),
            "the fixture's rollup is not the one `health::rollup_from_db` sums"
        );

        let rows: Vec<serde_json::Value> = result
            .nodes
            .iter()
            .map(|node| {
                let slot = health
                    .schedules
                    .get(&node.id)
                    .map(|schedule| &schedule.standing);
                let age = ages.get(&node.id).cloned().unwrap_or_default();
                let mut row = serde_json::to_value(node).unwrap();
                let fields = row.as_object_mut().unwrap();
                fields.insert(
                    "durations".into(),
                    serde_json::json!(durations.get(&node.id)),
                );
                fields.insert(
                    "next_run".into(),
                    serde_json::json!(slot.and_then(|s| s.next_fire)),
                );
                fields.insert(
                    "prev_run".into(),
                    serde_json::json!(slot.and_then(|s| s.prev_fire)),
                );
                fields.insert(
                    "late_since".into(),
                    serde_json::json!(slot.and_then(|s| s.late_since)),
                );
                // No run of this server's is in flight: the queued one has not started.
                fields.insert("running".into(), serde_json::Value::Null);
                fields.insert(
                    "last_success_at".into(),
                    serde_json::json!(age.last_success_at),
                );
                fields.insert("failing_since".into(), serde_json::json!(age.failing_since));
                fields.insert(
                    "consecutive_failures".into(),
                    serde_json::json!(age.consecutive_failures),
                );
                if let Some(run) = age.last_run {
                    fields.insert(
                        "last_run".into(),
                        serde_json::json!({
                            "run_id": run.run_id, "elapsed_seconds": run.elapsed_seconds,
                            "keys": run.keys, "calls": run.calls, "failed": run.failed,
                        }),
                    );
                }
                row
            })
            .collect();
        let rollup = health.rollup(&result, &dag.groups, &running, now, true);
        (
            serde_json::json!(rows),
            serde_json::to_value(rollup).unwrap(),
        )
    }

    /// `GET /runs`: every recorded run, newest first, each with its `line`, and the activity
    /// of the 24 hours before [`NOW`]. The row is `barca-server`'s `RunSummary` of a recorded
    /// run nothing is working on.
    async fn runs(&self, not_triggered_by: Option<&str>) -> serde_json::Value {
        let filter = crate::history_filter::HistoryFilter::parse(
            crate::history_filter::HistoryFlags {
                status: None,
                target: None,
                triggered_by: None,
                not_triggered_by,
                since: None,
                until: None,
                search: None,
            },
            at(NOW),
        )
        .unwrap();
        let kept = filter.run_filter();
        let records = crate::db::get_runs_filtered(self.db(), 100, &kept)
            .await
            .unwrap();
        let total = crate::db::count_runs_filtered(self.db(), &kept)
            .await
            .unwrap();
        let mut runs: Vec<serde_json::Value> = records
            .into_iter()
            .map(|record| {
                let first_failure = record.failure.as_ref().map(|f| (f.node.as_str(), f.error.as_str()));
                let line = crate::run_list::run_line(crate::run_list::RunLineFacts {
                    status: &record.status,
                    steps_total: record.steps_total,
                    steps_executed: record.steps_executed,
                    steps_cached: record.steps_cached,
                    steps_failed: record.steps_failed,
                    steps_skipped: None,
                    error: None,
                    first_failure,
                    target: record.target.as_deref(),
                });
                let mut row = serde_json::json!({
                    "id": record.run_id, "handle": null, "run_id": record.run_id,
                    "command": record.command, "files": record.files, "target": record.target,
                    "status": record.status, "triggered_by": record.triggered_by,
                    "scheduled_at": record.scheduled_at, "schedule_id": record.schedule_id,
                    "started_at": crate::time::sql_datetime_rfc3339(&record.started_at),
                    "finished_at": record.finished_at.as_deref().map(crate::time::sql_datetime_rfc3339),
                    "elapsed_seconds": record.elapsed_seconds,
                    "steps_total": record.steps_total, "steps_executed": record.steps_executed,
                    "steps_cached": record.steps_cached,
                    "error": null,
                    "first_failure": first_failure.map(|(node_id, error)| serde_json::json!({"node_id": node_id, "error": error})),
                    "principal": record.principal, "trigger_reason": record.trigger_reason,
                    "cancellation": record.cancellation,
                    "line": line,
                });
                if let Some(failed) = record.steps_failed {
                    row["steps_failed"] = serde_json::json!(failed);
                }
                row
            })
            .collect();
        // As the route sorts them: newest first, runs of one second as the database listed them.
        runs.sort_by(|a, b| b["started_at"].as_str().cmp(&a["started_at"].as_str()));
        let activity = crate::queries::activity_at(self.db(), &filter, at(NOW))
            .await
            .unwrap();
        serde_json::json!({"runs": runs, "total": total, "truncated": false, "activity": activity})
    }

    /// `GET /runs/{run_id}` of a recorded run that ended: its row, its steps, its logs, its
    /// planned nodes and its explanation, in the order `barca-server`'s `view_page` fills them.
    async fn run_view(&self, id: &str) -> serde_json::Value {
        let db = self.db();
        let record = crate::db::get_run(db, id)
            .await
            .unwrap()
            .expect("a recorded run");
        let (steps, mut steps_page) =
            crate::db::get_run_steps_window(db, id, Some(crate::db::RUN_STEPS_LIMIT), 0)
                .await
                .unwrap();
        steps_page.truncated = steps.len() < steps_page.total;
        steps_page.next_offset = (steps.len() < steps_page.total).then_some(steps.len());
        if let Some(job) = &record.job {
            let targets: Vec<&str> = if job == "all" {
                Vec::new()
            } else {
                job.split(',').collect()
            };
            let recorded = if steps_page.nodes.is_empty() {
                crate::db::run_node_summaries(db, id).await.unwrap()
            } else {
                steps_page.nodes.clone()
            };
            if let Some(nodes) = crate::queries::planned_run_nodes(
                &self.dag,
                &record.command,
                &targets,
                &record.status,
                record.steps_total,
                recorded,
            ) {
                steps_page.nodes = nodes;
            }
        }
        let mut view = crate::outcome::RunView {
            skip_reason: None,
            previous_attempts: Vec::new(),
            parent_run_id: None,
            run_attempt: None,
            stop_reason: None,
            run_id: record.run_id.clone(),
            status: record.status.clone(),
            command: record.command.clone(),
            target: record.target.clone(),
            files: record.files.clone(),
            triggered_by: record.triggered_by,
            scheduled_at: record.scheduled_at.clone(),
            schedule_id: record.schedule_id.clone(),
            started_at: crate::time::sql_datetime_rfc3339(&record.started_at),
            finished_at: record
                .finished_at
                .as_deref()
                .map(crate::time::sql_datetime_rfc3339),
            elapsed_seconds: record.elapsed_seconds,
            steps_total: record.steps_total,
            steps_executed: record.steps_executed,
            steps_cached: record.steps_cached,
            job: record.job.clone(),
            attempts: record.attempts,
            client_run_id: record.client_run_id.clone(),
            failure: record.failure.clone(),
            principal: record.principal.clone(),
            trigger_reason: record.trigger_reason.clone(),
            retries: record.retries,
            plan_reasons: record.plan_reasons.clone(),
            cancellation: record.cancellation.clone(),
            waiting_for: None,
            queue: None,
            explanation: Default::default(),
            outcome: None,
            error: steps.iter().find_map(|step| step.error.clone()),
            steps,
            steps_page,
            logs: crate::db::get_logs(db, id).await.unwrap(),
        };
        view.explanation = crate::run_explanation::explain(
            crate::run_explanation::RunFacts {
                steps_failed: record.steps_failed,
                steps_skipped: None,
                ..(&view).into()
            },
            &view.steps_page.nodes,
            &view.steps,
            at(NOW).timestamp() as f64,
        );
        view.explanation.outputs = crate::run_explanation::run_outputs(
            &view.status,
            None,
            &crate::run_explanation::output_targets(view.target.as_deref(), view.job.as_deref()),
            &view.steps,
        );
        serde_json::to_value(view).unwrap()
    }

    /// `value` as checked-in text: the scratch directory is not part of an id or a path.
    fn text(&self, value: &serde_json::Value) -> String {
        let mut text = serde_json::to_string_pretty(value).unwrap();
        text.push('\n');
        text.replace(&format!("{}/", self.dir.path().display()), "")
    }
}

/// Runs no recorded history holds (in flight, queued, cancelled) and runs of a size the fixture
/// project has not: each is the facts a server would have of it, stated here, and the
/// explanation `run_explanation::explain` gives for them. The kit shows the explanation with
/// the facts it was made from; no line of it is written by hand.
fn explanations() -> String {
    let now = at(NOW).timestamp();
    let started = |ago: i64| {
        chrono::DateTime::from_timestamp(now - ago, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let reason = |category: &str, subject: Option<&str>, nodes: u64, steps: u64, example: &str| serde_json::json!({"category": category, "subject": subject, "nodes": nodes, "steps": steps, "example": example});
    let node = |id: &str, keys: u64, seconds: f64| serde_json::json!({"node_id": id, "keys": keys, "failed": 0, "elapsed_seconds": seconds, "calls": 0});
    let get = "pipeline.py:orders_clean,pipeline.py:revenue_daily";
    // (name, the run's facts, its per-node summary)
    let scenarios = [
        (
            "queued",
            serde_json::json!({"status": "pending", "triggered_by": "client", "started_at": started(8), "steps_total": null, "steps_executed": 0, "steps_cached": 0}),
            vec![],
        ),
        (
            "running",
            serde_json::json!({"status": "running", "triggered_by": "client", "started_at": started(192), "steps_total": 4, "steps_executed": 1, "steps_cached": 1}),
            vec![],
        ),
        (
            "full rebuild in flight",
            serde_json::json!({
                "status": "running", "triggered_by": "api", "principal": "token:ci-bot", "started_at": started(360),
                "steps_total": 23_278, "steps_executed": 19_399, "steps_cached": 0, "attempts": 19_461, "retries": 62,
                "plan_reasons": {"nodes": 469, "steps": 23_278, "omitted": 3, "reasons": [
                    reason("code_changed", Some("helper pipelines/common.py:bucket_ltv"), 412, 21_104, "pipeline.py:ltv_by_account"),
                    reason("new_keys", None, 2, 1_205, "pipeline.py:sessionize"),
                    reason("inputs_changed", Some("input pipeline.py:orders_clean"), 40, 957, "pipeline.py:revenue_daily"),
                    reason("never_run", None, 12, 12, "pipeline.py:churn_model"),
                ]},
            }),
            vec![
                node("pipeline.py:sessionize", 3_120, 1_840.5),
                node("pipeline.py:ltv_by_account", 1, 212.3),
            ],
        ),
        (
            "just started",
            serde_json::json!({"status": "running", "triggered_by": "schedule", "started_at": started(4), "steps_total": 23_278, "steps_executed": 0, "steps_cached": 0}),
            vec![],
        ),
        (
            "finished, served mostly from cache",
            serde_json::json!({
                "status": "success", "triggered_by": "client", "started_at": started(192),
                "steps_total": 4, "steps_executed": 3, "steps_cached": 1, "attempts": 3, "retries": 0,
                "plan_reasons": {"nodes": 2, "steps": 3, "omitted": 0, "reasons": [
                    reason("inputs_changed", Some("sensor pipeline.py:orders_feed"), 2, 3, "pipeline.py:orders_clean"),
                ]},
            }),
            vec![],
        ),
        (
            "sensors and no retries",
            serde_json::json!({
                "status": "success", "triggered_by": "client", "started_at": started(192),
                "steps_total": 29, "steps_executed": 29, "steps_cached": 0, "attempts": 29, "retries": 0,
                "plan_reasons": {"nodes": 29, "steps": 29, "omitted": 0, "reasons": [
                    reason("sensor", None, 27, 27, "pipeline.py:orders_feed"),
                    reason("never_run", None, 2, 2, "pipeline.py:orders_clean"),
                ]},
            }),
            vec![],
        ),
        (
            "cancelled before it started",
            serde_json::json!({
                "status": "cancelled", "triggered_by": "api", "principal": "token:ops", "started_at": started(192),
                "steps_total": 0, "steps_executed": 0, "steps_cached": 0,
                "cancellation": {"by": "request", "principal": "token:ops", "started": false, "waiting_behind": ["a7e8f06a4890"], "same_as": "a7e8f06a4890"},
            }),
            vec![],
        ),
        (
            "cancelled by the server stopping",
            serde_json::json!({
                "status": "cancelled", "triggered_by": "client", "started_at": started(192),
                "steps_total": 4, "steps_executed": 3, "steps_cached": 1,
                "cancellation": {"by": "shutdown", "principal": null, "started": true, "waiting_behind": [], "same_as": null},
            }),
            vec![],
        ),
        (
            "error",
            serde_json::json!({"status": "error", "triggered_by": "client", "started_at": started(192), "steps_total": 4, "steps_executed": 0, "steps_cached": 0,
                "error": "could not start a worker: python3 not found"}),
            vec![],
        ),
        (
            "interrupted",
            serde_json::json!({"status": "interrupted", "triggered_by": "client", "started_at": started(192), "steps_total": 4, "steps_executed": 1, "steps_cached": 1,
                "error": "the server stopped while the run was in flight"}),
            vec![],
        ),
        // Two rows of a run list: steps failed and the run recorded no error of its own, and
        // the same with no failed step on record.
        (
            "failed at a key",
            serde_json::json!({"status": "failed", "triggered_by": "schedule", "started_at": started(192), "steps_total": 28_404, "steps_executed": 21_950, "steps_cached": 0, "steps_failed": 1,
                "first_failure": {"node_id": "pipeline.py:sessionize[day=2026-10-27]", "error": "KeyError: 'currency'"}}),
            vec![],
        ),
        (
            "failed with no step recorded",
            serde_json::json!({"status": "failed", "triggered_by": "client", "started_at": started(7_500), "steps_total": 4, "steps_executed": 0, "steps_cached": 0}),
            vec![],
        ),
    ];
    let mut out = serde_json::Map::new();
    for (name, run, nodes) in scenarios {
        let text = |key: &str| run[key].as_str();
        let plan_reasons: Option<crate::run_explain::PlanReasons> =
            serde_json::from_value(run["plan_reasons"].clone()).unwrap();
        let cancellation: Option<crate::run_explain::Cancellation> =
            serde_json::from_value(run["cancellation"].clone()).unwrap();
        let summaries: Vec<crate::db::RunNodeSummary> = nodes
            .iter()
            .map(|n| serde_json::from_value(n.clone()).unwrap())
            .collect();
        let explanation = crate::run_explanation::explain(
            crate::run_explanation::RunFacts {
                status: text("status").unwrap(),
                target: Some("revenue_daily"),
                job: Some(get),
                triggered_by: serde_json::from_value(run["triggered_by"].clone()).unwrap(),
                principal: text("principal"),
                attempts: run["attempts"].as_i64(),
                retries: run["retries"].as_i64(),
                steps_total: run["steps_total"].as_i64(),
                steps_executed: run["steps_executed"].as_i64().unwrap(),
                steps_cached: run["steps_cached"].as_i64().unwrap(),
                steps_failed: run["steps_failed"].as_i64(),
                steps_skipped: None,
                started_at: text("started_at").unwrap(),
                plan_reasons: plan_reasons.as_ref(),
                cancellation: cancellation.as_ref(),
            },
            &summaries,
            &[],
            now as f64,
        );
        // Its row in a run list, in the same words.
        let line = crate::run_list::run_line(crate::run_list::RunLineFacts {
            status: text("status").unwrap(),
            steps_total: run["steps_total"].as_i64(),
            steps_executed: run["steps_executed"].as_i64().unwrap(),
            steps_cached: run["steps_cached"].as_i64().unwrap(),
            steps_failed: run["steps_failed"].as_i64(),
            steps_skipped: None,
            error: text("error"),
            first_failure: run["first_failure"]["node_id"]
                .as_str()
                .zip(run["first_failure"]["error"].as_str()),
            target: Some("revenue_daily"),
        });
        let mut run = run;
        for key in [
            "principal",
            "attempts",
            "retries",
            "plan_reasons",
            "cancellation",
            "error",
            "first_failure",
        ] {
            if run.get(key).is_none() {
                run[key] = serde_json::Value::Null;
            }
        }
        run["target"] = serde_json::json!("revenue_daily");
        run["job"] = serde_json::json!(get);
        out.insert(
            name.to_string(),
            serde_json::json!({"run": run, "nodes": nodes, "explanation": explanation, "line": line}),
        );
    }
    let mut text = serde_json::to_string_pretty(&serde_json::Value::Object(out)).unwrap();
    text.push('\n');
    text
}

/// The runs whose detail the UI's tests read: a clean first run, a failed one with its log, a
/// failed key of a partitioned node, and a scheduled one.
const RUN_DETAILS: &[&str] = &[
    "a1f0c0000001",
    "a1f0c0000005",
    "a1f0c0000006",
    "5c4ed0001445",
];

#[tokio::test]
async fn the_ui_fixture_project_is_what_a_server_serves() {
    let project = Project::new().await;
    project.record().await;
    let (state, rollup) = project.state().await;
    let runs = project.runs(None).await;
    // As the Runs page asks by default: `GET /runs?not_triggered_by=schedule`.
    let by_people = project.runs(Some("schedule")).await;
    let mut details = serde_json::Map::new();
    for id in RUN_DETAILS {
        details.insert(id.to_string(), project.run_view(id).await);
    }
    let now = at(NOW).timestamp();
    let paused = crate::pauses::list(project.db()).await.unwrap();
    let pauses = serde_json::json!({
        "total": paused.len(),
        "paused": paused,
        "schedules": crate::pauses::paused_schedules(project.db(), now).await.unwrap(),
    });
    // What a pause of each node and everything below it would cover: `covers` of `GET
    // /pauses?target=<id>&downstream=true`, by node id.
    let covers: BTreeMap<String, crate::refresh_path::PauseCover> = project
        .dag
        .topo_order()
        .into_iter()
        .map(|id| {
            let cover = crate::refresh_path::pause_cover(&project.dag, id, true).unwrap();
            (id.to_string(), cover)
        })
        .collect();
    let covers = serde_json::to_value(covers).unwrap();
    // The same project once `fx_rates` is pinned.
    project.pin().await;
    let (pinned, _) = project.state().await;
    let pins = serde_json::json!({"pins": crate::pins::list(project.db()).await.unwrap()});
    let files = [
        ("state.json", project.text(&state)),
        ("rollup.json", project.text(&rollup)),
        ("runs.json", project.text(&runs)),
        ("runs-without-schedule.json", project.text(&by_people)),
        ("run-explanations.json", explanations()),
        (
            "run-details.json",
            project.text(&serde_json::Value::Object(details)),
        ),
        ("pauses.json", project.text(&pauses)),
        ("pause-covers.json", project.text(&covers)),
        ("state-pinned.json", project.text(&pinned)),
        ("pins.json", project.text(&pins)),
    ];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/lib/testdata/served");
    let update = std::env::var("BARCA_UPDATE_SNAPSHOTS").is_ok_and(|v| !v.is_empty() && v != "0");
    let mut stale = Vec::new();
    for (name, actual) in &files {
        let path = dir.join(name);
        if update {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, actual).unwrap();
        } else if std::fs::read_to_string(&path).unwrap_or_default() != *actual {
            stale.push(*name);
        }
    }
    assert!(
        stale.is_empty(),
        "{stale:?} in {} are not what barca-service says of the fixture project \
         (crates/barca-service/testdata/ui_fixture). The UI's unit tests and kit read them \
         (ui/src/lib/testdata/project.ts): regenerate them with \
         `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-service ui_fixture`, review the diff, then \
         run the UI's unit tests (`pnpm --dir ui test`).",
        dir.display()
    );
    let _ = &project.files;
}
