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
//! Finding them does not read the history, only its end and its indexes, because history is
//! append-only: a run's row keeps the row id it was given, a pull puts the rows it carries
//! after the last row of the download, and a push uploads the whole file. So every blob is
//! its predecessor plus rows appended, and a local database is a blob plus rows appended.
//!
//! - **Runs the pulled database lacks.** Local runs are read from the newest backwards, each
//!   looked up in the pulled database by `run_id` (unique index). The first one found there
//!   *in the same row* marks where the two histories are one: everything older is in both.
//!   With nothing unpushed that is the first row read. A pulled database that shares no
//!   history with the local one (the shared state was reset) is never matched, and every
//!   local run is carried.
//! - **Runs both have, in different states.** A run the pulled database holds as unfinished
//!   (see [`SETTLED`]; found through the index on `runs.status`) can be further along
//!   locally. Its steps are compared when its `(status, steps_executed)` differs between the
//!   two (a run's `steps_executed` moves with every step it records). Runs settled in the
//!   pulled database are normally not looked at again. The exception is local cancellation:
//!   an upload may publish a completed outcome before the interrupted coordinator hears its
//!   acknowledgement. A matching local cancelled outcome must survive the next pull.
//!
//! Copying is idempotent: carrying the same local database onto the same pulled one twice
//! adds nothing the second time. That is what makes an interrupted pull safe to repeat.
//!
//! Not carried: a step row with no `run_id` (written before 0.17, which did not record it), a
//! successful step of a run made on this host whose artifact is no longer reachable from here
//! (it would be a cache hit on nothing; steps of other hosts' runs came from the shared state
//! and go back as they are), and the cost estimates and scheduler state, which are not history
//! (timings are rebuilt by running; `barca serve` does not share state).

use crate::BarcaError;
use std::collections::{HashMap, HashSet};
use turso::{Connection, Value};

/// The outcomes a run records for itself when it ends. A run the pulled database holds with one
/// of these normally has all its writes. Cancellation during upload acknowledgement can
/// still correct a completed outcome; see `carry_unpushed`. `running`
/// (pushed mid-run by another process on the same machine) and `interrupted` (marked later, by
/// whoever noticed the process was gone) can be behind what this machine recorded.
const SETTLED: [&str; 3] = ["success", "failed", "cancelled"];

/// The other two: what a run is before it records its outcome, and what it is called once
/// someone notices its process is gone.
const UNFINISHED: [&str; 2] = ["running", "interrupted"];

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
    "owner",
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
    /// Why the local file held nothing that could be carried (not a database, empty, or not
    /// a barca database), when that is so: it was replaced, and the user is told.
    pub unreadable: Option<String>,
    /// True when the local database was opened and compared with the pulled one (false when
    /// there was none, or the base record showed nothing had been written to it).
    pub compared: bool,
    /// The runs something was carried for, in the order found.
    pub kept_runs: Vec<String>,
    /// True when an earlier pull already said this (the same rows, still not pushed).
    pub announced: bool,
}

impl Carried {
    /// True when the pulled database was changed.
    pub fn wrote(&self) -> bool {
        self.runs + self.steps + self.log_lines > 0 || !self.kept_runs.is_empty()
    }

    /// Identifies what was kept, so that keeping the same rows again is not announced again.
    pub fn digest(&self) -> String {
        if !self.wrote() && self.steps_without_artifact == 0 {
            return String::new();
        }
        format!(
            "{}:{}:{}:{}:{}",
            self.runs,
            self.steps,
            self.log_lines,
            self.steps_without_artifact,
            self.kept_runs.join(",")
        )
    }

    /// One line for stderr saying what the pull kept or could not keep; None when there is
    /// nothing to say, or nothing new. Informational, not part of the CLI contract.
    pub fn note(&self) -> Option<String> {
        if let Some(why) = &self.unreadable {
            return Some(format!(
                "[barca] warning: the local history file held no barca history ({why}); it \
                 was replaced by the shared history"
            ));
        }
        if self.announced {
            return None;
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

fn int(v: &Value) -> i64 {
    match v {
        Value::Integer(n) => *n,
        _ => 0,
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
pub(crate) async fn steps_of_run(
    conn: &Connection,
    run_id: &str,
) -> Result<HashSet<String>, BarcaError> {
    let mut out = HashSet::new();
    let mut rows = conn
        .query(
            "SELECT node_id FROM materializations WHERE run_id = ?1",
            [run_id.to_string()],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read recorded steps: {e}")))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read recorded steps: {e}")))?
    {
        out.insert(
            row.get::<String>(0).map_err(|e| {
                BarcaError::Db(format!("failed to read recorded step identity: {e}"))
            })?,
        );
    }
    Ok(out)
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
    let this_host = crate::db::local_host();
    let ours = |host: &str| !this_host.is_empty() && host == this_host;

    // Local runs the pulled database lacks: newest first, until the two histories meet.
    let mut open: Vec<OpenRun> = Vec::new();
    let mut below = i64::MAX;
    'tail: loop {
        let batch = rows_of(
            local,
            "SELECT id, run_id, status, COALESCE(steps_executed, 0), COALESCE(host, '') \
             FROM runs WHERE id < ?1 ORDER BY id DESC LIMIT 16",
            vec![Value::Integer(below)],
            "reading the local runs",
        )
        .await?;
        if batch.is_empty() {
            break;
        }
        for r in &batch {
            below = int(&r[0]);
            let Some(run_id) = text(&r[1]) else { continue };
            let there = rows_of(
                pulled,
                "SELECT id FROM runs WHERE run_id = ?1",
                vec![Value::Text(run_id.to_string())],
                "looking a run up in the pulled database",
            )
            .await?;
            match there.first().map(|row| int(&row[0])) {
                None => open.push(OpenRun {
                    run_id: run_id.to_string(),
                    status: text(&r[2]).unwrap_or("").to_string(),
                    ours: ours(text(&r[4]).unwrap_or("")),
                    in_pulled: false,
                }),
                // The same run in the same row: from here back the histories are one.
                Some(id) if id == below => break 'tail,
                // There under another row: both have it; see the unfinished runs below.
                Some(_) => {}
            }
        }
    }
    open.reverse();

    // Runs the pulled database holds as unfinished and the local one holds in another state.
    let unfinished_sql = "SELECT run_id, status, COALESCE(steps_executed, 0), \
                          COALESCE(host, '') FROM runs WHERE status = ?1";
    let mut local_unfinished: HashMap<String, (String, i64, String)> = HashMap::new();
    let mut pulled_unfinished: Vec<(String, String, i64)> = Vec::new();
    for status in UNFINISHED {
        let of = |conn, what| rows_of(conn, unfinished_sql, vec![Value::Text(status.into())], what);
        for r in of(local, "reading the local unfinished runs").await? {
            if let (Some(id), Some(st), Some(host)) = (text(&r[0]), text(&r[1]), text(&r[3])) {
                local_unfinished.insert(id.into(), (st.into(), int(&r[2]), host.into()));
            }
        }
        for r in of(pulled, "reading the pulled unfinished runs").await? {
            if let (Some(id), Some(st)) = (text(&r[0]), text(&r[1])) {
                pulled_unfinished.push((id.into(), st.into(), int(&r[2])));
            }
        }
    }
    for (run_id, status, steps) in pulled_unfinished {
        let local_state = match local_unfinished.get(&run_id) {
            // The same state on both sides.
            Some((st, n, _)) if (st.as_str(), *n) == (status.as_str(), steps) => continue,
            Some((st, _, host)) => Some((st.clone(), host.clone())),
            // Not unfinished here: finished here, or not here at all.
            None => rows_of(
                local,
                "SELECT status, COALESCE(host, '') FROM runs WHERE run_id = ?1",
                vec![Value::Text(run_id.clone())],
                "looking a run up in the local database",
            )
            .await?
            .first()
            .and_then(|r| Some((text(&r[0])?.to_string(), text(&r[1])?.to_string()))),
        };
        if let Some((local_status, host)) = local_state {
            open.push(OpenRun {
                run_id,
                status: local_status,
                ours: ours(&host),
                in_pulled: true,
            });
        }
    }
    // An upload can land before its helper acknowledges it. A subsequent interrupt
    // corrects the local outcome to cancelled, even when the bounded corrective push
    // cannot finish. Compare only indexed cancellations, never all settled history.
    // Require the recorded owner to match as well as the globally unique run ID.
    // A stale success/failed outcome never replaces a remote cancellation.
    let cancelled = rows_of(
        local,
        "SELECT run_id, COALESCE(host, ''), COALESCE(pid, 0), COALESCE(owner, '') \
         FROM runs WHERE status = 'cancelled'",
        vec![],
        "reading local cancellations",
    )
    .await?;
    for row in cancelled {
        let Some(run_id) = text(&row[0]) else {
            continue;
        };
        let remote = rows_of(
            pulled,
            "SELECT status, COALESCE(host, ''), COALESCE(pid, 0), COALESCE(owner, '') \
             FROM runs WHERE run_id = ?1",
            vec![Value::Text(run_id.into())],
            "comparing a cancelled run's published outcome",
        )
        .await?;
        if remote.first().is_some_and(|r| {
            matches!(text(&r[0]), Some("success" | "failed")) && row[1..] == r[1..]
        }) {
            open.push(OpenRun {
                run_id: run_id.into(),
                status: "cancelled".into(),
                ours: ours(text(&row[1]).unwrap_or("")),
                in_pulled: true,
            });
        }
    }
    if open.is_empty() {
        return Ok(carried);
    }

    pulled.execute("BEGIN", ()).await.map_err(db_err("begin"))?;
    let copied = copy_runs(local, pulled, &open, &mut carried).await;
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

/// A local run that is compared with the pulled database.
struct OpenRun {
    run_id: String,
    /// Its status in the local database.
    status: String,
    /// Made on this host: its artifacts are expected here.
    ours: bool,
    /// The pulled database has a row for it (as unfinished).
    in_pulled: bool,
}

async fn copy_runs(
    local: &Connection,
    pulled: &Connection,
    open: &[OpenRun],
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

    for run in open {
        let (run_id, local_status) = (&run.run_id, &run.status);
        let id = || vec![Value::Text(run_id.clone())];
        let before = (carried.runs, carried.steps, carried.log_lines);

        // The run row. An unfinished published outcome, or a completed outcome whose
        // cancellation acknowledgement was interrupted, yields to the local correction.
        let mut corrected_outcome = false;
        match run.in_pulled {
            false => {
                for row in rows_of(local, &run_select, id(), "reading a local run").await? {
                    pulled
                        .execute(run_insert.as_str(), row)
                        .await
                        .map_err(db_err("writing a run"))?;
                    carried.runs += 1;
                }
            }
            true if SETTLED.contains(&local_status.as_str()) => {
                let outcome = rows_of(
                    local,
                    "SELECT status, steps_executed, steps_cached, finished_at, elapsed_seconds \
                     FROM runs WHERE run_id = ?1",
                    id(),
                    "reading a local run",
                )
                .await?;
                for mut row in outcome {
                    corrected_outcome = true;
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
            true => {}
        }

        // Its steps: the ones the pulled database has no row for.
        let mut there = steps_of_run(pulled, run_id).await?;
        for row in rows_of(local, &step_select, id(), "reading local steps").await? {
            let Some(node_id) = text(&row[step_node]).map(str::to_string) else {
                continue;
            };
            if there.contains(&node_id) {
                continue;
            }
            if run.ours
                && text(&row[step_status]) == Some("success")
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
        if corrected_outcome || before != (carried.runs, carried.steps, carried.log_lines) {
            carried.kept_runs.push(run_id.clone());
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

    /// A run made on this host, with `steps_executed` at the number of steps it has recorded
    /// so far (as the recorder keeps it).
    pub(crate) async fn add_run(db_path: &str, run_id: &str, status: &str) {
        add_run_from(db_path, run_id, status, &db::local_host()).await;
    }

    pub(crate) async fn add_run_from(db_path: &str, run_id: &str, status: &str, host: &str) {
        exec(
            db_path,
            "INSERT INTO runs (run_id, command, files, status, steps_executed, host) \
             VALUES (?1, 'get', '[\"f.py\"]', ?2, \
             (SELECT COUNT(*) FROM materializations WHERE run_id = ?1), ?3)",
            vec![
                Value::Text(run_id.into()),
                Value::Text(status.into()),
                Value::Text(host.into()),
            ],
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
            add_step(&pulled, run, "f.py:a", &file).await;
            add_run(&pulled, run, "running").await;
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
                "died\trunning\t1\t0\tNULL\tNULL",
                // The outcome the run recorded for itself replaces `running`.
                "ended\tfailed\t2\t3\t1.5\t2026-01-01 00:00:00",
                // A local `running` never overwrites what the shared copy says.
                "noticed\tinterrupted\t1\t0\tNULL\tNULL",
            ]
        );
        let mut want = Vec::new();
        for run in ["died", "ended", "noticed"] {
            for node in ["f.py:a", "f.py:b"] {
                want.push(format!("{run}\t{node}\tsuccess"));
            }
        }
        assert_eq!(steps(&pulled).await, want);

        // The pulled database is the local one now. At the next pull `died` and `noticed` are
        // in the same state on both sides, so they are not compared again (a step added to
        // one behind the recorder's back shows that they are skipped).
        add_step(&pulled, "died", "f.py:unseen", &file).await;
        add_step(&pulled, "noticed", "f.py:unseen", &file).await;
        let fresh = fresh_db(&dir, "fresh.db").await;
        for run in ["died", "noticed"] {
            add_step(&fresh, run, "f.py:a", &file).await;
        }
        add_run(&fresh, "died", "running").await;
        add_run(&fresh, "noticed", "interrupted").await;
        // (`ended` is not in the fresh copy at all, so it is carried whole.)
        assert_eq!(carry(&pulled, &fresh).await.kept_runs, ["ended"]);
    }

    #[tokio::test]
    async fn cancellation_corrects_a_settled_upload_without_new_steps_or_logs() {
        let dir = tempfile::tempdir().unwrap();
        let local = fresh_db(&dir, "local.db").await;
        let pulled = fresh_db(&dir, "pulled.db").await;
        let file = artifact(&dir, "completed.json");
        for status in ["success", "failed"] {
            add_run(&local, status, "cancelled").await;
            add_run(&pulled, status, status).await;
            for db_path in [&local, &pulled] {
                add_step(db_path, status, "f.py:a", &file).await;
                db::insert_logs(db_path, status, &[("f.py:a".into(), "completed".into())])
                    .await
                    .unwrap();
            }
        }
        add_run(&pulled, "unrelated", "success").await;
        let carried = carry(&local, &pulled).await;
        assert!(carried.wrote(), "outcome-only corrections must be tracked");
        assert_eq!((carried.runs, carried.steps, carried.log_lines), (0, 0, 0));
        assert_eq!(carried.kept_runs, ["success", "failed"]);
        assert!(!carried.digest().is_empty());
        assert_eq!(
            runs(&pulled).await,
            [
                "failed\tcancelled",
                "success\tcancelled",
                "unrelated\tsuccess"
            ]
        );
        assert_eq!(carry(&local, &pulled).await, Carried::default());
        assert_eq!(
            steps(&pulled).await,
            ["failed\tf.py:a\tsuccess", "success\tf.py:a\tsuccess"]
        );
        for run in ["success", "failed"] {
            assert_eq!(db::get_logs(&pulled, run).await.unwrap().len(), 1);
        }
        // A stale completed outcome from a machine that pulled before cancellation
        // must never reverse the correction.
        let stale = fresh_db(&dir, "stale.db").await;
        add_run(&stale, "success", "success").await;
        assert_eq!(carry(&stale, &pulled).await, Carried::default());
        assert_eq!(runs(&pulled).await[1], "success\tcancelled");
    }

    #[tokio::test]
    async fn cancellation_never_overwrites_a_different_recorded_owner() {
        let dir = tempfile::tempdir().unwrap();
        let local = fresh_db(&dir, "local.db").await;
        let pulled = fresh_db(&dir, "pulled.db").await;
        for field in ["host", "pid", "owner"] {
            add_run(&local, field, "cancelled").await;
            add_run(&pulled, field, "success").await;
            exec(
                &pulled,
                &format!("UPDATE runs SET {field} = ?1 WHERE run_id = ?2"),
                vec![Value::Text("different".into()), Value::Text(field.into())],
            )
            .await;
        }
        assert_eq!(carry(&local, &pulled).await, Carried::default());
        assert_eq!(
            runs(&pulled).await,
            ["host\tsuccess", "owner\tsuccess", "pid\tsuccess"]
        );
    }

    #[tokio::test]
    async fn another_hosts_steps_go_back_as_they_came() {
        // Rows of a run made elsewhere can only be here because they were pulled. If the
        // shared state no longer has them (it was rolled back), they are carried whole: their
        // artifacts were never expected on this machine.
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone.json").to_string_lossy().to_string();
        let pulled = fresh_db(&dir, "pulled.db").await;
        let local = fresh_db(&dir, "local.db").await;
        add_step(&local, "theirs", "f.py:a", &gone).await;
        add_run_from(&local, "theirs", "success", "another-machine").await;

        let carried = carry(&local, &pulled).await;
        assert_eq!(
            (carried.runs, carried.steps, carried.steps_without_artifact),
            (1, 1, 0)
        );
    }

    /// A database holding `history` (run ids, in row order), each with one step.
    async fn history_db(dir: &tempfile::TempDir, name: &str, history: &[&str]) -> String {
        let file = artifact(dir, "h.json");
        let db_path = fresh_db(dir, name).await;
        for run in history {
            add_step(&db_path, run, "f.py:a", &file).await;
            add_run_from(&db_path, run, "success", "another-machine").await;
        }
        db_path
    }

    #[tokio::test]
    async fn only_the_end_of_the_local_history_is_read_when_the_two_share_their_past() {
        let dir = tempfile::tempdir().unwrap();
        // Both grew from the blob [a, b]: here two runs were recorded, there three were pushed.
        let local = history_db(&dir, "local.db", &["a", "b", "mine-1", "mine-2"]).await;
        let pulled = history_db(&dir, "pulled.db", &["a", "b", "x", "y", "z"]).await;
        let carried = carry(&local, &pulled).await;
        assert_eq!(carried.kept_runs, ["mine-1", "mine-2"]);
        assert_eq!(
            runs(&pulled).await.len(),
            7,
            "the shared past is there once"
        );

        // The reading stops where the histories meet (`b`, the same run in the same row).
        // Not a state barca produces: a run below that point that the other side lacks. It
        // is here to show that the past is not read.
        let local = history_db(&dir, "local-2.db", &["only-here", "b", "mine"]).await;
        let pulled = history_db(&dir, "pulled-2.db", &["a", "b", "x"]).await;
        assert_eq!(carry(&local, &pulled).await.kept_runs, ["mine"]);
    }

    #[tokio::test]
    async fn a_run_both_have_in_different_rows_is_not_copied_again() {
        // `mine` was carried by an earlier pull on this machine (after `x`), pushed, and is
        // in the pulled database in the row it got then. The local database is what that
        // pull left, so the histories meet at the last row.
        let dir = tempfile::tempdir().unwrap();
        let local = history_db(&dir, "local.db", &["a", "x", "mine"]).await;
        let pulled = history_db(&dir, "pulled.db", &["a", "x", "mine", "y"]).await;
        assert_eq!(carry(&local, &pulled).await, Carried::default());
        // And when the rows do not line up at all (`mine` recorded before `x` was pulled).
        let local = history_db(&dir, "local-2.db", &["a", "mine", "later"]).await;
        let pulled = history_db(&dir, "pulled-2.db", &["a", "x", "mine"]).await;
        assert_eq!(carry(&local, &pulled).await.kept_runs, ["later"]);
        assert_eq!(runs(&pulled).await.len(), 4);
    }

    #[tokio::test]
    async fn a_pulled_database_with_another_past_gets_every_local_run() {
        // The shared state was deleted and created again by another machine: nothing lines
        // up, so nothing is assumed to be there.
        let dir = tempfile::tempdir().unwrap();
        let mut local_history: Vec<String> = (0..40).map(|i| format!("ours-{i:02}")).collect();
        local_history.sort();
        let names: Vec<&str> = local_history.iter().map(String::as_str).collect();
        let local = history_db(&dir, "local.db", &names).await;
        let pulled = history_db(&dir, "pulled.db", &["theirs-1", "theirs-2"]).await;
        let carried = carry(&local, &pulled).await;
        assert_eq!((carried.runs, carried.steps), (40, 40));
        assert_eq!(carried.kept_runs, names, "in the order they were recorded");
        assert_eq!(runs(&pulled).await.len(), 42);
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
        assert!(
            unreadable
                .note()
                .unwrap()
                .contains("held no barca history (short read)")
        );
    }
}
