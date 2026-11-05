//! A run's recorded steps as `barca history RUN_ID` and `GET /runs/{run_id}` list them: a page
//! of [`RunStep`]s ([`get_run_steps_page`]), each node's [`RunNodeSummary`], the nodes the plan
//! had that recorded none ([`planned_run_nodes`]) and a failed step's last output lines.

use super::*;
use crate::step_status::StepStatus;
use serde::{Deserialize, Serialize};

/// A step recorded by this run: one it executed (`success`) or one that failed. Cached
/// steps have no new materialization row, so they are not listed (a live run on a server also
/// lists the `cached` steps it has reported); nor are steps a failure skipped.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunStep {
    pub node_id: String,
    pub status: String,
    pub elapsed_seconds: Option<f64>,
    /// A failed step's error as recorded: for a Python exception, `Type: message` and the
    /// traceback's frames.
    pub error: Option<String>,
    /// The partition key (`k=v`, pairs joined by `,`) of one key of a partitioned step.
    #[serde(default)]
    pub partition_key: Option<String>,
    /// Attempts made, retries included.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub attempts: Option<i64>,
    #[serde(default)]
    pub run_hash: Option<String>,
    /// Where the result is recorded: this machine's artifact path, or its artifact-store
    /// location when it was uploaded.
    #[serde(default)]
    pub artifact_path: Option<String>,
    #[serde(default)]
    pub artifact_format: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub artifact_size_bytes: Option<i64>,
    /// A failed step's exception type (`ZeroDivisionError`; `WorkerError` when its worker
    /// died, `UploadError` when its result could not be stored).
    #[serde(default)]
    pub error_type: Option<String>,
    /// A failed step's error message alone (without its type and traceback).
    #[serde(default)]
    pub error_message: Option<String>,
    /// A failed step's Python traceback: barca's own frames left out of a failure in the
    /// step's code, every frame kept for a failure in barca's own code (`error_phase`).
    #[serde(default)]
    pub error_traceback: Option<String>,
    /// Where a failed step failed (#664): `user_code` (the step's own code: importing its
    /// module, calling it, checking what it returned), or barca's own work around it:
    /// `load_inputs`, `split_output` (a batched call's result), `write_output` or `sink`.
    /// `null` for a step that did not fail, a worker that died, or a row from an older barca.
    #[serde(default)]
    pub error_phase: Option<String>,
    /// How the worker process ended, for a step whose worker died: `SIGKILL (possibly the OOM
    /// killer)`, another signal, `code N`, or `disconnected`. `null` for a step that raised.
    #[serde(default)]
    pub worker_exit: Option<String>,
    /// CPU seconds the step used (the worker process's user and system time).
    #[serde(default)]
    pub cpu_seconds: Option<f64>,
    /// The worker process's peak RSS while the step ran, in bytes (#627). `null` when not
    /// recorded (a failed step, a row from an older barca, or a platform that cannot tell).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_rss_bytes: Option<i64>,
    /// A failed step's last lines of output, stdout and stderr in the order they were written,
    /// without the traceback barca already shows (at most [`OUTPUT_TAIL_LINES`]; `barca logs
    /// RUN_ID --step STEP` has every line). `null` for a step that did not fail, `[]` when it
    /// printed nothing.
    #[serde(default)]
    pub output_tail: Option<Vec<String>>,
    /// The batched call (RFC-0013) that computed this key: the keys of one call share it, and
    /// its output is recorded once under `<node>[batch=<id>]`. `null` for a step that ran on
    /// its own. Experimental.
    #[serde(default)]
    pub batch: Option<String>,
}

/// How many lines of a failed step's output `barca history RUN_ID` shows ([`RunStep::output_tail`]).
pub const OUTPUT_TAIL_LINES: usize = 20;

/// How the worker ended, read from the error barca recorded when a worker died
/// (`worker for <node> exited with <how>`, or `worker disconnected`).
pub fn worker_exit(error_type: Option<&str>, message: Option<&str>) -> Option<String> {
    if error_type != Some("WorkerError") {
        return None;
    }
    let message = message?.lines().next()?;
    if message.starts_with("worker disconnected") {
        return Some("disconnected".to_string());
    }
    let (_, how) = message.split_once(" exited with ")?;
    let how = how
        .split_once(" while it waited")
        .map_or(how, |(how, _)| how);
    Some(how.to_string())
}

pub async fn get_run_steps(db_path: &str, run_id: &str) -> Result<Vec<RunStep>, BarcaError> {
    Ok(get_run_steps_page(db_path, run_id, None).await?.0)
}

/// A page of a run's recorded steps: what `barca history RUN_ID` and `GET /runs/{run_id}`
/// list (#623). A run of many partition keys records one step per key it computed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StepsPage {
    /// How many steps the run recorded.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
    /// True when `steps` lists fewer than `total`.
    pub truncated: bool,
    /// Per node over every recorded step, listed or not, when the run has a partitioned node.
    /// Failed nodes first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<RunNodeSummary>>", optional))]
    pub nodes: Vec<RunNodeSummary>,
}

/// A node's steps in one run: its keys, however many `steps` lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RunNodeSummary {
    pub node_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub keys: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub failed: usize,
    /// Summed over its keys, which may have overlapped.
    pub elapsed_seconds: f64,
    /// Batched calls that computed them; 0 when each key ran on its own.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub calls: usize,
    /// What became of the node in this run, when the server could rebuild the run's plan
    /// ([`planned_run_nodes`]): `ran` (it recorded steps), `cached` (nothing to do) or
    /// `skipped` (never run: a step it depends on failed, or the run stopped before it).
    /// Absent otherwise. Experimental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub status: Option<String>,
    /// For a `skipped` node: the failed node upstream of it that kept it from running. Absent
    /// when the run stopped before reaching it. Experimental.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub blocked_by: Option<String>,
}

/// Every node a finished run planned, in plan order, each with its [`RunNodeSummary::status`]:
/// `recorded` (the nodes with steps, from [`run_node_summaries`]) completed with the planned
/// nodes that recorded none. Such a node is `skipped` when a node upstream of it failed
/// (`blocked_by`) or when the run broke off before its phase (a single-target run stops at the
/// phase of its first failure; a run of several targets goes on), else `cached`. None when the
/// plan rebuilt from today's sources does not have `steps_total` steps: the sources changed
/// since the run, so nothing can be said about what it skipped.
pub fn planned_run_nodes(
    dag: &crate::dag::Dag,
    command: &str,
    target_ids: &[&str],
    status: &str,
    steps_total: Option<i64>,
    recorded: Vec<RunNodeSummary>,
) -> Option<Vec<RunNodeSummary>> {
    use std::collections::{HashMap, HashSet};
    let config = crate::planner::ResourceConfig {
        pool_size: std::thread::available_parallelism().map_or(4, |n| n.get()),
        concurrency_groups: HashMap::new(),
    };
    let plan = crate::targets::plan_for_targets(dag, target_ids, &config, command, None);
    if plan.phases.is_empty() || steps_total != Some(plan.total_steps as i64) {
        return None;
    }
    let mut phase_of: HashMap<&str, usize> = HashMap::new();
    let mut planned: Vec<&str> = Vec::new();
    for (i, phase) in plan.phases.iter().enumerate() {
        for step in phase.streams.iter().flat_map(|s| &s.steps) {
            if !phase_of.contains_key(step.step_id.base_id()) {
                phase_of.insert(step.step_id.base_id(), i);
                planned.push(step.step_id.base_id());
            }
        }
    }
    let mut by_id: HashMap<String, RunNodeSummary> = recorded
        .into_iter()
        .map(|n| (n.node_id.clone(), n))
        .collect();
    let failed: HashSet<String> = by_id
        .values()
        .filter(|n| n.failed > 0)
        .map(|n| n.node_id.clone())
        .collect();
    // A single-target run breaks off after the phase of its first failure; later phases were
    // never decided. A successful run, or one of several targets, reached every phase.
    let reached = if status == "success" || target_ids.len() > 1 {
        usize::MAX
    } else {
        by_id
            .keys()
            .filter_map(|id| phase_of.get(id.as_str()).copied())
            .max()
            .unwrap_or(0)
    };
    let mut nodes = Vec::with_capacity(planned.len());
    for id in planned {
        if let Some(mut node) = by_id.remove(id) {
            node.status = Some(StepStatus::Ran.to_string());
            nodes.push(node);
            continue;
        }
        let blocked_by = crate::targets::blocking_failure(dag, id, &failed);
        let skipped = blocked_by.is_some() || phase_of.get(id).copied().unwrap_or(0) > reached;
        nodes.push(RunNodeSummary {
            node_id: id.to_string(),
            status: Some(
                if skipped {
                    StepStatus::Skipped
                } else {
                    StepStatus::Cached
                }
                .to_string(),
            ),
            blocked_by: blocked_by.map(str::to_string),
            ..Default::default()
        });
    }
    nodes.extend(by_id.into_values());
    Some(nodes)
}

/// How many recorded steps `barca history RUN_ID` and `GET /runs/{run_id}` list by default.
pub const RUN_STEPS_LIMIT: usize = 1000;

/// At most `limit` of `run_id`'s recorded steps (every one with `None`), in the order they were
/// recorded, failed steps first among those kept; and how many there are.
pub async fn get_run_steps_page(
    db_path: &str,
    run_id: &str,
    limit: Option<usize>,
) -> Result<(Vec<RunStep>, StepsPage), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    // Read-only snapshots of older databases may lack later columns: those read as null.
    let mut columns = Vec::new();
    for name in [
        "attempts",
        "error_type",
        "cpu_seconds",
        "max_rss_bytes",
        "batch_id",
        "error_phase",
    ] {
        columns.push(if has_column(&conn, "materializations", name).await? {
            name
        } else {
            "NULL"
        });
    }
    let (attempts, error_type, cpu, rss, batch, phase) = (
        columns[0], columns[1], columns[2], columns[3], columns[4], columns[5],
    );
    let total = match limit {
        None => None,
        Some(_) => {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM materializations WHERE run_id = ?1",
                    [run_id],
                )
                .await
                .map_err(|e| BarcaError::Db(format!("failed to count run steps: {e}")))?;
            let row = rows
                .next()
                .await
                .map_err(|e| BarcaError::Db(format!("failed to count run steps: {e}")))?;
            Some(row.and_then(|r| r.get::<i64>(0).ok()).unwrap_or(0) as usize)
        }
    };
    // Past the limit, the failed steps are kept before the others.
    let page = match (limit, total) {
        (Some(limit), Some(total)) if total > limit => format!(
            " AND id IN (SELECT id FROM materializations WHERE run_id = ?1 ORDER BY (status = 'failed') DESC, id LIMIT {limit})"
        ),
        _ => String::new(),
    };
    let mut rows = conn.query(
        &format!("SELECT node_id, status, elapsed_seconds, error_message, {attempts}, run_hash, artifact_path, artifact_format, artifact_size_bytes, {error_type}, error_traceback, {cpu}, {rss}, {batch}, {phase} FROM materializations WHERE run_id = ?1{page} ORDER BY id"),
        [run_id],
    ).await.map_err(|e| BarcaError::Db(format!("failed to query run steps: {e}")))?;
    let text = |row: &libsql::Row, i: i32| row.get::<String>(i).ok().filter(|t| !t.is_empty());
    let mut steps = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run step: {e}")))?
    {
        let node_id = row.get::<String>(0).unwrap_or_default();
        let partition = crate::StepId::parse(&node_id).partition.suffix();
        let raw_error = row.get::<String>(3).ok();
        let raw_type = text(&row, 9);
        let worker_exit = worker_exit(raw_type.as_deref(), raw_error.as_deref());
        let (error_type, error_message, error_traceback) = match &raw_error {
            Some(message) => {
                let (kind, message, stack) = split_exception(
                    raw_type.as_deref().unwrap_or_default(),
                    message,
                    &text(&row, 10).unwrap_or_default(),
                );
                (Some(kind).filter(|k| !k.is_empty()), Some(message), stack)
            }
            None => (raw_type, None, text(&row, 10)),
        };
        steps.push(RunStep {
            partition_key: (!partition.is_empty()).then_some(partition),
            status: row.get::<String>(1).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(2).ok(),
            attempts: row.get::<i64>(4).ok(),
            run_hash: text(&row, 5),
            artifact_path: text(&row, 6),
            artifact_format: text(&row, 7),
            artifact_size_bytes: row.get::<i64>(8).ok(),
            worker_exit,
            error_type,
            error_message,
            error_traceback,
            error: raw_error,
            cpu_seconds: row.get::<f64>(11).ok(),
            max_rss_bytes: row.get::<i64>(12).ok().filter(|rss| *rss > 0),
            output_tail: None,
            batch: text(&row, 13),
            error_phase: text(&row, 14),
            node_id,
        });
    }
    drop(rows);
    // A failed step's last lines (#627). Failed steps are kept first in a page (#623), so every
    // failed step listed has its tail; only the listed failed steps' lines are read.
    // A key of a batched call: its call's output, recorded once for the call.
    let log_id = |s: &RunStep| match &s.batch {
        Some(b) => format!("{}[batch={b}]", crate::StepId::parse(&s.node_id).base_id()),
        None => s.node_id.clone(),
    };
    let failed: std::collections::HashSet<String> = steps
        .iter()
        .filter(|s| StepStatus::Failed.is(Some(&s.status)))
        .map(log_id)
        .collect();
    if !failed.is_empty() {
        let lines = output_lines(&conn, run_id, &failed).await?;
        for step in steps
            .iter_mut()
            .filter(|s| StepStatus::Failed.is(Some(&s.status)))
        {
            step.output_tail = Some(output_tail(&lines, &log_id(step)));
        }
    }
    let total = total.unwrap_or(steps.len());
    let mut nodes = node_summaries(&conn, run_id, batch).await?;
    if nodes.iter().all(|n| n.keys <= 1) {
        nodes.clear();
    }
    let page = StepsPage {
        total,
        truncated: steps.len() < total,
        nodes,
    };
    Ok((steps, page))
}

/// Every node `run_id` recorded steps for, with their counts: failed nodes first, then in the
/// order they were first recorded. `batch` is the batch id column, or `NULL` without one.
async fn node_summaries(
    conn: &libsql::Connection,
    run_id: &str,
    batch: &str,
) -> Result<Vec<RunNodeSummary>, BarcaError> {
    let mut summary = conn
        .query(
            &format!(
                "SELECT substr(node_id, 1, instr(node_id || '[', '[') - 1) AS base, COUNT(*), SUM(status = 'failed'), SUM(elapsed_seconds), COUNT(DISTINCT {batch}) \
                 FROM materializations WHERE run_id = ?1 GROUP BY base ORDER BY SUM(status = 'failed') > 0 DESC, MIN(id)"
            ),
            [run_id],
        )
        .await
        .map_err(|e| BarcaError::Db(format!("failed to summarize run steps: {e}")))?;
    let mut nodes = Vec::new();
    while let Some(row) = summary
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read run summary: {e}")))?
    {
        nodes.push(RunNodeSummary {
            node_id: row.get::<String>(0).unwrap_or_default(),
            keys: row.get::<i64>(1).unwrap_or(0) as usize,
            failed: row.get::<i64>(2).unwrap_or(0) as usize,
            elapsed_seconds: row.get::<f64>(3).unwrap_or(0.0),
            calls: row.get::<i64>(4).unwrap_or(0) as usize,
            ..Default::default()
        });
    }
    Ok(nodes)
}

/// [`StepsPage::nodes`] of `run_id` whatever its nodes' key counts: the input of
/// [`planned_run_nodes`].
pub async fn run_node_summaries(
    db_path: &str,
    run_id: &str,
) -> Result<Vec<RunNodeSummary>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let batch = if has_column(&conn, "materializations", "batch_id").await? {
        "batch_id"
    } else {
        "NULL"
    };
    node_summaries(&conn, run_id, batch).await
}

/// The recorded lines of `run_id` written by `nodes`, (node id, line), in the order they were
/// written. Empty on a database without a `logs` table.
async fn output_lines(
    conn: &libsql::Connection,
    run_id: &str,
    nodes: &std::collections::HashSet<String>,
) -> Result<Vec<(String, String)>, BarcaError> {
    let Ok(mut rows) = conn
        .query(
            "SELECT node_id, line FROM logs WHERE run_id = ?1 ORDER BY seq ASC",
            [run_id],
        )
        .await
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| BarcaError::Db(format!("failed to read log row: {e}")))?
    {
        let node = row.get::<String>(0).unwrap_or_default();
        if nodes.contains(&node) {
            out.push((node, row.get::<String>(1).unwrap_or_default()));
        }
    }
    Ok(out)
}

/// The last [`OUTPUT_TAIL_LINES`] lines `node_id` wrote, up to the traceback of its last
/// failed attempt (the run detail prints the traceback on its own).
fn output_tail(lines: &[(String, String)], node_id: &str) -> Vec<String> {
    let mut own: Vec<&str> = lines
        .iter()
        .filter(|(node, _)| node == node_id)
        .map(|(_, line)| line.as_str())
        .collect();
    if let Some(at) = own
        .iter()
        .rposition(|line| line.trim_start() == "Traceback (most recent call last):")
    {
        own.truncate(at);
    }
    let from = own.len().saturating_sub(OUTPUT_TAIL_LINES);
    own[from..].iter().map(|line| line.to_string()).collect()
}

/// One step result as `barca sql` shows it in `barca.materializations` and `GET
/// /materializations` returns it: a row of `materializations` without the columns that describe
/// one machine's disk (`evicted_at`, `last_read_at`) or that are large (`error_traceback`,
/// `output_json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepRow {
    /// Node id, with the partition suffix for a partitioned step (`p.py:daily[day=d1]`).
    pub node_id: String,
    /// The partition key (`day=d1`), or `null`.
    pub partition: Option<String>,
    pub run_id: Option<String>,
    pub run_hash: Option<String>,
    /// `success` or `failed`.
    pub status: String,
    /// When it was recorded (UTC, `YYYY-MM-DD HH:MM:SS`).
    pub created_at: String,
    pub elapsed_seconds: Option<f64>,
    pub cpu_seconds: Option<f64>,
    pub max_rss_bytes: Option<i64>,
    pub attempts: Option<i64>,
    /// `json`, `parquet`, `csv` or `pickle`.
    pub format: Option<String>,
    pub size_bytes: Option<i64>,
    /// Where the result is stored (a store path with a remote store).
    pub artifact: Option<String>,
    /// SHA-256 of the artifact's bytes.
    pub content_hash: Option<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
}

/// Step results, newest first: at most `limit` rows from `offset`, and how many there are in
/// all. Read-only; columns an older database lacks are `null`.
pub async fn read_step_rows(
    db_path: &str,
    limit: usize,
    offset: usize,
) -> Result<(Vec<StepRow>, usize), BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let mut select = Vec::new();
    for column in [
        "node_id",
        "run_id",
        "run_hash",
        "status",
        "created_at",
        "elapsed_seconds",
        "cpu_seconds",
        "max_rss_bytes",
        "attempts",
        "artifact_format",
        "artifact_size_bytes",
        "artifact_path",
        "output_hash",
        "error_type",
        "error_message",
    ] {
        if has_column(&conn, "materializations", column).await? {
            select.push(column.to_string());
        } else {
            select.push(format!("NULL AS {column}"));
        }
    }
    let failed =
        |e: libsql::Error| BarcaError::Db(format!("failed to query materializations: {e}"));
    let mut rows = conn
        .query("SELECT COUNT(*) FROM materializations", ())
        .await
        .map_err(failed)?;
    let total = rows
        .next()
        .await
        .map_err(failed)?
        .map_or(0, |r| r.get::<i64>(0).unwrap_or(0).max(0) as usize);
    drop(rows);
    let mut rows = conn
        .query(
            &format!(
                "SELECT {} FROM materializations ORDER BY id DESC LIMIT ?1 OFFSET ?2",
                select.join(", ")
            ),
            [
                limit.min(i64::MAX as usize) as i64,
                offset.min(i64::MAX as usize) as i64,
            ],
        )
        .await
        .map_err(failed)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(failed)? {
        let text = |i: i32| row.get::<String>(i).ok().filter(|s| !s.is_empty());
        let node_id = row.get::<String>(0).unwrap_or_default();
        let partition = node_id
            .split_once('[')
            .and_then(|(_, key)| key.strip_suffix(']'))
            .map(str::to_string);
        out.push(StepRow {
            partition,
            node_id,
            run_id: text(1),
            run_hash: text(2),
            status: text(3).unwrap_or_else(|| StepStatus::Success.to_string()),
            created_at: row.get::<String>(4).unwrap_or_default(),
            elapsed_seconds: row.get::<f64>(5).ok(),
            cpu_seconds: row.get::<f64>(6).ok(),
            max_rss_bytes: row.get::<i64>(7).ok(),
            attempts: row.get::<i64>(8).ok(),
            format: text(9),
            size_bytes: row.get::<i64>(10).ok(),
            artifact: text(11),
            content_hash: text(12),
            error_type: text(13),
            error_message: text(14),
        });
    }
    Ok((out, total))
}

#[cfg(test)]
mod tests {
    /// #475: the keys of one run finish in any order, so the row id alone picks an arbitrary key
    /// as a partitioned node's last materialization. The rule: the latest run (by row id), and
    /// within it the first key in sort order; rows without a run id fall back to the row id.
    #[tokio::test]
    async fn a_page_of_run_steps_keeps_failed_steps_and_counts_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for i in 0..5 {
                let node = format!("p.py:k[n={i}]");
                let status = if i == 3 { "failed" } else { "success" };
                conn.execute(
                    "INSERT INTO materializations (run_id, node_id, run_hash, status) VALUES ('many', ?1, 'h', ?2)",
                    [node.as_str(), status],
                )
                .await
                .unwrap();
            }
        }
        let (steps, page) = super::get_run_steps_page(&path, "many", Some(2))
            .await
            .unwrap();
        let ids: Vec<&str> = steps.iter().map(|s| s.node_id.as_str()).collect();
        // In recorded order, the failed step kept although it is not among the first two.
        assert_eq!(ids, ["p.py:k[n=0]", "p.py:k[n=3]"]);
        assert_eq!(
            page,
            super::StepsPage {
                total: 5,
                truncated: true,
                nodes: page.nodes.clone(),
            }
        );
        let (steps, page) = super::get_run_steps_page(&path, "many", None)
            .await
            .unwrap();
        assert_eq!(steps.len(), 5);
        assert_eq!(
            page,
            super::StepsPage {
                total: 5,
                truncated: false,
                nodes: page.nodes.clone(),
            }
        );
        let (steps, page) = super::get_run_steps_page(&path, "many", Some(10))
            .await
            .unwrap();
        assert_eq!((steps.len(), page.truncated), (5, false));
    }

    #[tokio::test]
    async fn run_inspection_selects_its_recorded_steps_and_failure_details() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::init_db(&path).await.unwrap();
        {
            let _guard = super::db_guard().await;
            let (_db, conn) = super::open_conn(&path).await.unwrap();
            for (id, node, status, error) in [
                ("first", "p.py:ok", "success", ""),
                ("first", "p.py:bad", "failed", "bad input"),
                ("other", "p.py:other", "success", ""),
            ] {
                conn.execute("INSERT INTO materializations (run_id,node_id,status,error_message,elapsed_seconds) VALUES (?1,?2,?3,NULLIF(?4,''),1.25)", [id,node,status,error]).await.unwrap();
            }
        }
        let steps = super::get_run_steps(&path, "first").await.unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].node_id, "p.py:ok");
        assert_eq!(steps[1].status, "failed");
        assert_eq!(steps[1].error.as_deref(), Some("bad input"));
        assert_eq!(steps[1].elapsed_seconds, Some(1.25));
        assert!(
            super::get_run_steps(&path, "unknown")
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod planned_nodes_tests {
    use super::{RunNodeSummary, planned_run_nodes};

    const SRC: &str = "from barca import asset\n\
@asset()\ndef accounts(): return 1\n\
@asset()\ndef prices(): return 1\n\
@asset(inputs={'accounts': accounts})\ndef forecast(accounts): return 1\n\
@asset(inputs={'forecast': forecast, 'prices': prices})\ndef plan(forecast, prices): return 1\n\
@asset(inputs={'plan': plan})\ndef publish(plan): return 1\n\
@asset()\ndef unrelated(): return 1\n";

    fn dag() -> crate::dag::Dag {
        crate::dag::Dag::build(&crate::parse::extract_nodes(SRC, "p.py").unwrap()).unwrap()
    }

    fn ran(node_id: &str, keys: usize, failed: usize) -> RunNodeSummary {
        RunNodeSummary {
            node_id: node_id.into(),
            keys,
            failed,
            ..Default::default()
        }
    }

    fn outcomes(nodes: &[RunNodeSummary]) -> Vec<(&str, &str, Option<&str>)> {
        nodes
            .iter()
            .map(|n| {
                (
                    n.node_id.as_str(),
                    n.status.as_deref().unwrap(),
                    n.blocked_by.as_deref(),
                )
            })
            .collect()
    }

    #[test]
    fn a_failure_skips_what_depends_on_it_and_the_rest_was_cached() {
        let nodes = planned_run_nodes(
            &dag(),
            "get",
            &["p.py:publish"],
            "failed",
            Some(5),
            vec![ran("p.py:forecast", 3, 1)],
        )
        .unwrap();
        let mut sorted = outcomes(&nodes);
        sorted.sort();
        assert_eq!(
            sorted,
            [
                ("p.py:accounts", "cached", None),
                ("p.py:forecast", "ran", None),
                ("p.py:plan", "skipped", Some("p.py:forecast")),
                ("p.py:prices", "cached", None),
                ("p.py:publish", "skipped", Some("p.py:forecast")),
            ]
        );
        // In plan order: a node after what it depends on.
        let at = |id: &str| nodes.iter().position(|n| n.node_id == id).unwrap();
        assert!(at("p.py:accounts") < at("p.py:forecast"));
        assert!(at("p.py:forecast") < at("p.py:plan") && at("p.py:plan") < at("p.py:publish"));
    }

    #[test]
    fn a_successful_run_cached_every_node_without_steps() {
        let nodes = planned_run_nodes(
            &dag(),
            "get",
            &["p.py:publish"],
            "success",
            Some(5),
            vec![ran("p.py:publish", 1, 0)],
        )
        .unwrap();
        let statuses: Vec<&str> = nodes.iter().map(|n| n.status.as_deref().unwrap()).collect();
        assert_eq!(statuses, ["cached", "cached", "cached", "cached", "ran"]);
    }

    #[test]
    fn a_run_whose_plan_changed_says_nothing() {
        // Today's sources plan 5 nodes for `publish`; the run recorded 4.
        let dag = dag();
        assert!(
            planned_run_nodes(&dag, "get", &["p.py:publish"], "failed", Some(4), vec![]).is_none()
        );
        assert!(
            planned_run_nodes(&dag, "get", &["p.py:publish"], "failed", None, vec![]).is_none()
        );
    }

    #[test]
    fn everything_plans_the_gettable_graph() {
        let nodes = planned_run_nodes(&dag(), "get", &[], "success", Some(6), vec![]).unwrap();
        assert_eq!(nodes.len(), 6);
        assert!(nodes.iter().all(|n| n.status.as_deref() == Some("cached")));
    }
}

#[cfg(test)]
mod output_tail_tests {
    use super::{OUTPUT_TAIL_LINES, output_tail};

    #[test]
    fn the_tail_is_the_steps_own_last_lines_before_its_traceback() {
        let mut lines: Vec<(String, String)> = (0..30)
            .map(|i| ("p.py:a".to_string(), format!("line {i}")))
            .collect();
        lines.insert(3, ("p.py:b".to_string(), "other step".to_string()));
        lines.push(("p.py:a".into(), "Traceback (most recent call last):".into()));
        lines.push(("p.py:a".into(), "  File \"p.py\", line 3, in a".into()));
        lines.push(("p.py:a".into(), "ValueError: no".into()));
        let tail = output_tail(&lines, "p.py:a");
        assert_eq!(tail.len(), OUTPUT_TAIL_LINES);
        assert_eq!(tail.first().unwrap(), "line 10");
        assert_eq!(tail.last().unwrap(), "line 29");
        assert!(output_tail(&lines, "p.py:c").is_empty());
    }
}
