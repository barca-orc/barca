//! What a pull does with local rows that were never pushed (RFC-0006 §4.1, #221).
//!
//! A machine's local metadata DB can hold rows the shared copy has never seen: a run that was
//! killed before its end-of-run push (its run row and the steps it recorded as they finished),
//! a run whose push failed, a run made with `BARCA_STATE=off`. A pull replaces the local
//! database with the shared one, so those rows are first copied onto the pulled database, and
//! only then is it swapped in ([`crate::db::replace_db`]). They reach the shared copy with the
//! next push.
//!
//! "Never pushed" is decided from the two databases alone, with no marker to keep in step:
//!
//! - a **run** is unpushed when the pulled database has no row with its `run_id`;
//! - a **step** (a `materializations` row) is unpushed when the pulled database has no row
//!   with its `(run_id, node_id)`: a step has one outcome per run, the same identity the
//!   end-of-run ledger uses to add only what is missing;
//! - a run's **log lines** are unpushed when the pulled database has none for that run.
//!
//! Only the runs the pulled database does not hold as finished are compared step by step (see
//! [`SETTLED`]), so a pull with nothing to carry costs one scan of `runs` on each side.
//!
//! Copying is idempotent: carrying the same local database onto the same pulled one twice
//! adds nothing the second time. That is what makes an interrupted pull safe to repeat.
//!
//! Not carried: a step row with no `run_id` (written before 0.17, which did not record it), a
//! successful step whose artifact is no longer reachable from this machine (it would be a
//! cache hit on nothing), and the cost estimates and scheduler state, which are not history
//! (timings are rebuilt by running; `barca serve` does not share state).

use crate::BarcaError;
use std::collections::{HashMap, HashSet};
use turso::{Connection, Value};

/// The outcomes a run records for itself when it ends. A run the pulled database holds with one
/// of these was pushed after its last write, so the pulled database has all of it. `running`
/// (pushed mid-run by another process on the same machine) and `interrupted` (marked later, by
/// whoever noticed the process was gone) can be behind what this machine recorded.
const SETTLED: [&str; 3] = ["success", "failed", "cancelled"];

/// Every column of `runs` except its row id, which the receiving database assigns.
pub(crate) const RUN_COLUMNS: &[&str] = &[
    "run_id",
    "command",
    "files",
    "target",
    "status",
    "steps_total",
    "steps_executed",
    "steps_cached",
    "started_at",
    "finished_at",
    "elapsed_seconds",
    "pid",
    "host",
];

/// Every column of `materializations` except its row id.
pub(crate) const STEP_COLUMNS: &[&str] = &[
    "node_id",
    "run_hash",
    "output_json",
    "artifact_path",
    "artifact_format",
    "artifact_size_bytes",
    "elapsed_seconds",
    "status",
    "error_message",
    "error_traceback",
    "attempts",
    "sinks_json",
    "created_at",
    "cpu_seconds",
    "max_rss_bytes",
    "output_hash",
    "run_id",
    "error_type",
];

/// Every column of `logs` except its row id.
pub(crate) const LOG_COLUMNS: &[&str] = &["run_id", "node_id", "seq", "line", "created_at"];

/// What one pull carried over from the local database.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Carried {
    /// Runs the pulled database did not have.
    pub runs: usize,
    /// Step rows (successes and failures) the pulled database did not have.
    pub steps: usize,
    /// Log lines of runs the pulled database had no log for.
    pub log_lines: usize,
    /// Successful steps left behind because their artifact is not reachable from here.
    pub steps_without_artifact: usize,
    /// Why the local database could not be read, when it could not: nothing was carried.
    pub unreadable: Option<String>,
}

impl Carried {
    /// True when the pulled database was changed.
    pub fn wrote(&self) -> bool {
        self.runs + self.steps + self.log_lines > 0
    }

    /// One line for stderr saying what the pull kept or could not keep; None when there is
    /// nothing to say. Informational, not part of the CLI contract.
    pub fn note(&self) -> Option<String> {
        if let Some(why) = &self.unreadable {
            return Some(format!(
                "[barca] warning: the local history could not be read ({why}); it was replaced \
                 by the shared history, and anything recorded only on this machine is gone"
            ));
        }
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let mut kept = Vec::new();
        if self.runs > 0 {
            kept.push(plural(self.runs, "run"));
        }
        if self.steps > 0 {
            kept.push(plural(self.steps, "finished step"));
        }
        let mut note = match kept.is_empty() {
            true if self.steps_without_artifact == 0 => return None,
            true => "[barca] local history".to_string(),
            false => format!(
                "[barca] kept {} recorded only on this machine (not yet in the shared history)",
                kept.join(" and ")
            ),
        };
        if self.steps_without_artifact > 0 {
            note.push_str(&format!(
                ": left out {} whose result file is no longer here",
                plural(self.steps_without_artifact, "step")
            ));
        }
        Some(note)
    }
}

fn db_err(what: &str) -> impl Fn(turso::Error) -> BarcaError + '_ {
    move |e| BarcaError::Db(format!("carrying local history over a pull: {what}: {e}"))
}

/// All rows of `sql` as raw values.
async fn rows_of(
    conn: &Connection,
    sql: &str,
    params: Vec<Value>,
    what: &str,
) -> Result<Vec<Vec<Value>>, BarcaError> {
    let mut rows = conn.query(sql, params).await.map_err(db_err(what))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_err(what))? {
        let mut values = Vec::with_capacity(row.column_count());
        for i in 0..row.column_count() {
            values.push(row.get_value(i).map_err(db_err(what))?);
        }
        out.push(values);
    }
    Ok(out)
}

fn text(v: &Value) -> Option<&str> {
    match v {
        Value::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

fn insert_sql(table: &str, columns: &[&str]) -> String {
    let marks: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        columns.join(", "),
        marks.join(", ")
    )
}

fn column_index(columns: &[&str], name: &str) -> usize {
    columns
        .iter()
        .position(|c| *c == name)
        .expect("a column named in this module")
}

/// The steps of `run_id` that have a row in this database. A step has one outcome per run, so
/// this is what tells a writer (the end-of-run ledger, its replay after a push conflict, and
/// a pull carrying local rows over) which of a run's steps are already there.
pub(crate) async fn steps_of_run(conn: &Connection, run_id: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(mut rows) = conn
        .query(
            "SELECT node_id FROM materializations WHERE run_id = ?1",
            [run_id.to_string()],
        )
        .await
    else {
        return out;
    };
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(node_id) = row.get::<String>(0) {
            out.insert(node_id);
        }
    }
    out
}

/// True when a recorded artifact location can still be read from this machine: an object-store
/// URI (fetched and checked when something reads it), or a file that exists.
fn artifact_reachable(path: &str) -> bool {
    !path.is_empty() && (path.contains("://") || std::path::Path::new(path).exists())
}

/// Copy onto `pulled` every row of `local` that it does not have (see the module docs for what
/// that means). Both databases must have the current schema (`db::init_schema`). `local` is
/// only read. The copy is one transaction on `pulled`.
pub(crate) async fn carry_unpushed(
    local: &Connection,
    pulled: &Connection,
) -> Result<Carried, BarcaError> {
    let mut carried = Carried::default();

    let pulled_status: HashMap<String, String> = rows_of(
        pulled,
        "SELECT run_id, status FROM runs",
        vec![],
        "reading the pulled runs",
    )
    .await?
    .iter()
    .filter_map(|r| Some((text(&r[0])?.to_string(), text(&r[1])?.to_string())))
    .collect();

    // Local runs the pulled database does not hold as finished, oldest first.
    let open: Vec<(String, String)> = rows_of(
        local,
        "SELECT run_id, status FROM runs ORDER BY id",
        vec![],
        "reading the local runs",
    )
    .await?
    .iter()
    .filter_map(|r| Some((text(&r[0])?.to_string(), text(&r[1])?.to_string())))
    .filter(|(run_id, _)| {
        pulled_status
            .get(run_id)
            .is_none_or(|status| !SETTLED.contains(&status.as_str()))
    })
    .collect();
    if open.is_empty() {
        return Ok(carried);
    }

    pulled.execute("BEGIN", ()).await.map_err(db_err("begin"))?;
    let copied = copy_runs(local, pulled, &open, &pulled_status, &mut carried).await;
    if let Err(e) = copied {
        pulled.execute("ROLLBACK", ()).await.ok();
        return Err(e);
    }
    pulled
        .execute("COMMIT", ())
        .await
        .map_err(db_err("commit"))?;
    Ok(carried)
}

async fn copy_runs(
    local: &Connection,
    pulled: &Connection,
    open: &[(String, String)],
    pulled_status: &HashMap<String, String>,
    carried: &mut Carried,
) -> Result<(), BarcaError> {
    let run_select = format!(
        "SELECT {} FROM runs WHERE run_id = ?1",
        RUN_COLUMNS.join(", ")
    );
    let step_select = format!(
        "SELECT {} FROM materializations WHERE run_id = ?1 ORDER BY id",
        STEP_COLUMNS.join(", ")
    );
    let log_select = format!(
        "SELECT {} FROM logs WHERE run_id = ?1 ORDER BY id",
        LOG_COLUMNS.join(", ")
    );
    let (run_insert, step_insert, log_insert) = (
        insert_sql("runs", RUN_COLUMNS),
        insert_sql("materializations", STEP_COLUMNS),
        insert_sql("logs", LOG_COLUMNS),
    );
    let (step_node, step_status, step_path) = (
        column_index(STEP_COLUMNS, "node_id"),
        column_index(STEP_COLUMNS, "status"),
        column_index(STEP_COLUMNS, "artifact_path"),
    );

    for (run_id, local_status) in open {
        let id = || vec![Value::Text(run_id.clone())];

        // The run row. The pulled database may hold it as `running` or `interrupted` while
        // this machine saw it end: then the outcome it recorded for itself stands.
        match pulled_status.get(run_id) {
            None => {
                for row in rows_of(local, &run_select, id(), "reading a local run").await? {
                    pulled
                        .execute(run_insert.as_str(), row)
                        .await
                        .map_err(db_err("writing a run"))?;
                    carried.runs += 1;
                }
            }
            Some(_) if SETTLED.contains(&local_status.as_str()) => {
                let outcome = rows_of(
                    local,
                    "SELECT status, steps_executed, steps_cached, finished_at, elapsed_seconds \
                     FROM runs WHERE run_id = ?1",
                    id(),
                    "reading a local run",
                )
                .await?;
                for mut row in outcome {
                    row.push(Value::Text(run_id.clone()));
                    pulled
                        .execute(
                            "UPDATE runs SET status = ?1, steps_executed = ?2, steps_cached = ?3, \
                             finished_at = ?4, elapsed_seconds = ?5 WHERE run_id = ?6",
                            row,
                        )
                        .await
                        .map_err(db_err("writing a run's outcome"))?;
                }
            }
            Some(_) => {}
        }

        // Its steps: the ones the pulled database has no row for.
        let mut there = steps_of_run(pulled, run_id).await;
        for row in rows_of(local, &step_select, id(), "reading local steps").await? {
            let Some(node_id) = text(&row[step_node]).map(str::to_string) else {
                continue;
            };
            if there.contains(&node_id) {
                continue;
            }
            if text(&row[step_status]) == Some("success")
                && !artifact_reachable(text(&row[step_path]).unwrap_or(""))
            {
                carried.steps_without_artifact += 1;
                continue;
            }
            pulled
                .execute(step_insert.as_str(), row)
                .await
                .map_err(db_err("writing a step"))?;
            there.insert(node_id);
            carried.steps += 1;
        }

        // Its captured output, all or nothing: lines are written once, when a run ends.
        let has_log = !rows_of(
            pulled,
            "SELECT 1 FROM logs WHERE run_id = ?1 LIMIT 1",
            id(),
            "reading the pulled log",
        )
        .await?
        .is_empty();
        if !has_log {
            for row in rows_of(local, &log_select, id(), "reading the local log").await? {
                pulled
                    .execute(log_insert.as_str(), row)
                    .await
                    .map_err(db_err("writing a log line"))?;
                carried.log_lines += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    //! Small databases for tests of the carry and of the swap around it.
    use crate::db;
    use turso::Value;

    /// A database with the current schema at `dir/name`.
    pub(crate) async fn fresh_db(dir: &tempfile::TempDir, name: &str) -> String {
        let path = dir.path().join(name).to_string_lossy().to_string();
        db::init_db(&path).await.unwrap();
        path
    }

    /// A file that exists, to stand for a step's artifact.
    pub(crate) fn artifact(dir: &tempfile::TempDir, name: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, b"1").unwrap();
        path.to_string_lossy().to_string()
    }

    pub(crate) async fn exec(db_path: &str, sql: &str, params: Vec<Value>) {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await.unwrap();
        conn.execute(sql, params).await.unwrap();
    }

    pub(crate) async fn add_run(db_path: &str, run_id: &str, status: &str) {
        exec(
            db_path,
            "INSERT INTO runs (run_id, command, files, status, steps_executed, host) \
             VALUES (?1, 'get', '[\"f.py\"]', ?2, 0, 'some-other-host')",
            vec![Value::Text(run_id.into()), Value::Text(status.into())],
        )
        .await;
    }

    pub(crate) async fn add_step(db_path: &str, run_id: &str, node_id: &str, artifact: &str) {
        exec(
            db_path,
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, \
             artifact_size_bytes, status, run_id) VALUES (?1, 'h-' || ?1, ?2, 'json', 1, 'success', ?3)",
            vec![
                Value::Text(node_id.into()),
                Value::Text(artifact.into()),
                Value::Text(run_id.into()),
            ],
        )
        .await;
    }

    /// Every row of `sql` as tab-joined text, in the order given.
    pub(crate) async fn query(db_path: &str, sql: &str) -> Vec<String> {
        let _g = db::db_guard().await;
        let (_db, conn) = db::open_conn(db_path).await.unwrap();
        let mut rows = conn.query(sql, ()).await.unwrap();
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let cells: Vec<String> = (0..row.column_count())
                .map(|i| match row.get_value(i).unwrap() {
                    Value::Null => "NULL".to_string(),
                    Value::Integer(n) => n.to_string(),
                    Value::Real(x) => x.to_string(),
                    Value::Text(t) => t,
                    Value::Blob(_) => "<blob>".to_string(),
                })
                .collect();
            out.push(cells.join("\t"));
        }
        out
    }

    pub(crate) async fn runs(db_path: &str) -> Vec<String> {
        query(db_path, "SELECT run_id, status FROM runs ORDER BY run_id").await
    }

    pub(crate) async fn steps(db_path: &str) -> Vec<String> {
        query(
            db_path,
            "SELECT run_id, node_id, status FROM materializations ORDER BY run_id, node_id, id",
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::db;

    /// Carry `local` onto `pulled` the way a pull does, without the swap.
    async fn carry(local: &str, pulled: &str) -> Carried {
        let _g = db::db_guard().await;
        let (_l, local) = db::open_conn(local).await.unwrap();
        let (_p, pulled) = db::open_conn(pulled).await.unwrap();
        carry_unpushed(&local, &pulled).await.unwrap()
    }

    #[tokio::test]
    async fn the_column_lists_name_every_column_but_the_row_id() {
        // A column added by a later migration must be carried too: this fails until it is
        // added to the list above.
        let dir = tempfile::tempdir().unwrap();
        let db_path = fresh_db(&dir, "m.db").await;
        for (table, columns) in [
            ("runs", RUN_COLUMNS),
            ("materializations", STEP_COLUMNS),
            ("logs", LOG_COLUMNS),
        ] {
            let mut actual: Vec<String> = query(&db_path, &format!("PRAGMA table_info({table})"))
                .await
                .iter()
                .map(|row| row.split('\t').nth(1).unwrap().to_string())
                .filter(|name| name != "id")
                .collect();
            actual.sort();
            let mut listed: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
            listed.sort();
            assert_eq!(listed, actual, "{table}");
        }
    }

    #[tokio::test]
    async fn rows_the_pulled_database_lacks_are_copied_once_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = artifact(&dir, "a.json");

        // Both sides know `shared` (it was pushed). Only this machine knows `killed`.
        let pulled = fresh_db(&dir, "pulled.db").await;
        let local = fresh_db(&dir, "local.db").await;
        for db_path in [&pulled, &local] {
            add_run(db_path, "shared", "success").await;
            add_step(db_path, "shared", "f.py:a", &file).await;
        }
        add_run(&pulled, "theirs", "success").await;
        add_step(&pulled, "theirs", "f.py:a", &file).await;
        add_run(&local, "killed", "running").await;
        add_step(&local, "killed", "f.py:a", &file).await;
        add_step(&local, "killed", "f.py:b", &file).await;
        db::insert_logs(&local, "killed", &[("f.py:a".into(), "hello".into())])
            .await
            .unwrap();

        let carried = carry(&local, &pulled).await;
        assert_eq!(
            (carried.runs, carried.steps, carried.log_lines),
            (1, 2, 1),
            "{carried:?}"
        );
        assert!(carried.wrote());
        let want_runs = ["killed\trunning", "shared\tsuccess", "theirs\tsuccess"];
        let want_steps = [
            "killed\tf.py:a\tsuccess",
            "killed\tf.py:b\tsuccess",
            "shared\tf.py:a\tsuccess",
            "theirs\tf.py:a\tsuccess",
        ];
        assert_eq!(runs(&pulled).await, want_runs);
        assert_eq!(steps(&pulled).await, want_steps);
        assert_eq!(db::get_logs(&pulled, "killed").await.unwrap().len(), 1);
        // Every column made the trip, not only the ones the assertions above read.
        let whole = |db_path: String| async move {
            query(
                &db_path,
                &format!(
                    "SELECT {} FROM materializations WHERE run_id = 'killed' ORDER BY node_id",
                    STEP_COLUMNS.join(", ")
                ),
            )
            .await
        };
        assert_eq!(whole(pulled.clone()).await, whole(local.clone()).await);

        // Idempotent: the same local database carried again adds nothing.
        let again = carry(&local, &pulled).await;
        assert_eq!(again, Carried::default());
        assert_eq!(runs(&pulled).await, want_runs);
        assert_eq!(steps(&pulled).await, want_steps);
        assert_eq!(db::get_logs(&pulled, "killed").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_run_pushed_while_it_was_going_gets_the_rest_of_its_steps_and_its_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let file = artifact(&dir, "a.json");
        let pulled = fresh_db(&dir, "pulled.db").await;
        let local = fresh_db(&dir, "local.db").await;

        // Another process on this machine pushed the local database mid-run: the shared copy
        // has `ended` and `died` as `running`, with the one step each had recorded by then.
        for run in ["ended", "died", "noticed"] {
            add_run(&pulled, run, "running").await;
            add_step(&pulled, run, "f.py:a", &file).await;
            add_step(&local, run, "f.py:a", &file).await;
            add_step(&local, run, "f.py:b", &file).await;
        }
        // Someone else already noticed that `noticed` was dead.
        exec(
            &pulled,
            "UPDATE runs SET status = 'interrupted' WHERE run_id = 'noticed'",
            vec![],
        )
        .await;
        add_run(&local, "ended", "running").await;
        exec(
            &local,
            "UPDATE runs SET status = 'failed', steps_executed = 2, steps_cached = 3, \
             elapsed_seconds = 1.5, finished_at = '2026-01-01 00:00:00' WHERE run_id = 'ended'",
            vec![],
        )
        .await;
        add_run(&local, "died", "running").await;
        add_run(&local, "noticed", "running").await;

        let carried = carry(&local, &pulled).await;
        assert_eq!((carried.runs, carried.steps), (0, 3), "{carried:?}");
        assert_eq!(
            query(
                &pulled,
                "SELECT run_id, status, steps_executed, steps_cached, elapsed_seconds, \
                 finished_at FROM runs ORDER BY run_id"
            )
            .await,
            [
                "died\trunning\t0\t0\tNULL\tNULL",
                // The outcome the run recorded for itself replaces `running`.
                "ended\tfailed\t2\t3\t1.5\t2026-01-01 00:00:00",
                // A local `running` never overwrites what the shared copy says.
                "noticed\tinterrupted\t0\t0\tNULL\tNULL",
            ]
        );
        let mut want = Vec::new();
        for run in ["died", "ended", "noticed"] {
            for node in ["f.py:a", "f.py:b"] {
                want.push(format!("{run}\t{node}\tsuccess"));
            }
        }
        assert_eq!(steps(&pulled).await, want);
    }

    #[tokio::test]
    async fn a_step_whose_artifact_is_gone_is_not_carried() {
        let dir = tempfile::tempdir().unwrap();
        let here = artifact(&dir, "here.json");
        let gone = dir.path().join("gone.json").to_string_lossy().to_string();
        let pulled = fresh_db(&dir, "pulled.db").await;
        let local = fresh_db(&dir, "local.db").await;
        add_run(&local, "killed", "running").await;
        add_step(&local, "killed", "f.py:here", &here).await;
        add_step(&local, "killed", "f.py:gone", &gone).await;
        // A store location is not checked here: it is fetched, and verified, when read.
        add_step(
            &local,
            "killed",
            "f.py:stored",
            "s3://bucket/f.py--stored/h.json",
        )
        .await;
        // A failed step has no artifact to miss.
        exec(
            &local,
            "INSERT INTO materializations (node_id, run_hash, status, error_message, run_id) \
             VALUES ('f.py:bad', 'h', 'failed', 'boom', 'killed')",
            vec![],
        )
        .await;

        let carried = carry(&local, &pulled).await;
        assert_eq!(
            (carried.runs, carried.steps, carried.steps_without_artifact),
            (1, 3, 1)
        );
        assert_eq!(
            steps(&pulled).await,
            [
                "killed\tf.py:bad\tfailed",
                "killed\tf.py:here\tsuccess",
                "killed\tf.py:stored\tsuccess",
            ]
        );
        assert!(carried.note().unwrap().contains("left out 1 step"));
    }

    #[tokio::test]
    async fn a_finished_run_the_pulled_database_holds_is_not_compared_again() {
        // The rule that keeps a pull cheap: once the shared copy holds a run with the outcome
        // it recorded for itself, that run was pushed whole.
        let dir = tempfile::tempdir().unwrap();
        let file = artifact(&dir, "a.json");
        let pulled = fresh_db(&dir, "pulled.db").await;
        let local = fresh_db(&dir, "local.db").await;
        for db_path in [&pulled, &local] {
            add_run(db_path, "done", "success").await;
            add_step(db_path, "done", "f.py:a", &file).await;
        }
        // Not a state barca produces (a run writes nothing after its outcome is pushed); it is
        // here to show that the run's steps are not looked at.
        add_step(&local, "done", "f.py:late", &file).await;
        let carried = carry(&local, &pulled).await;
        assert_eq!(carried, Carried::default());
        assert_eq!(carried.note(), None);
        assert_eq!(steps(&pulled).await, ["done\tf.py:a\tsuccess"]);
    }

    #[test]
    fn the_note_says_what_was_kept() {
        let carried = Carried {
            runs: 1,
            steps: 2,
            ..Default::default()
        };
        assert_eq!(
            carried.note().unwrap(),
            "[barca] kept 1 run and 2 finished steps recorded only on this machine (not yet in \
             the shared history)"
        );
        let unreadable = Carried {
            unreadable: Some("short read".into()),
            ..Default::default()
        };
        assert!(unreadable.note().unwrap().contains("could not be read"));
    }
}
