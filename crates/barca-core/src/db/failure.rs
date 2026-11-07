//! What the asset panel says about a node whose last execution failed (`GET /assets/{name}`'s
//! `failure`): where in the project the error was raised, the run hash of the failed attempt,
//! and the node's last success to compare it with. Read from `materializations`; never a cache
//! decision.

use super::*;
use serde::{Deserialize, Serialize};
use std::path::{Component, PathBuf};

/// A source location in the project: a path relative to the project root and a 1-based line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SourceLine {
    pub file: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub line: u32,
    /// The function the frame is in, when the traceback names one.
    pub function: Option<String>,
}

/// A node's last successful materialization, as the failure is compared with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LastSuccess {
    pub run_hash: Option<String>,
    pub created_at: String,
    pub run_id: Option<String>,
}

/// The facts about a node's latest execution when it failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct FailureFacts {
    /// The last traceback frame inside the project (not in a virtualenv or site-packages):
    /// the line of the project's own code that raised, or called what raised. `null` when the
    /// traceback has no such frame (a worker that died, an error in barca's own code).
    pub raised_at: Option<SourceLine>,
    /// The run hash the failed attempt executed with (code, inputs' run hashes, env).
    pub run_hash: Option<String>,
    pub run_id: Option<String>,
    pub created_at: String,
    /// The node's last success, or `null` when it never succeeded.
    pub last_success: Option<LastSuccess>,
}

/// [`FailureFacts`] for `node_id` when its latest execution failed; `None` when it succeeded
/// or never ran. `root` is the project root that `raised_at` is relative to.
pub async fn failure_facts(
    db_path: &str,
    node_id: &str,
    root: &Path,
) -> Result<Option<FailureFacts>, BarcaError> {
    let _g = db_guard_for(db_path).await;
    let (_db, conn) = open_conn(db_path).await?;
    let fail = |e: libsql::Error| BarcaError::Db(format!("failed to query the last failure: {e}"));
    let mut rows = conn
        .query(
            "SELECT status, run_hash, run_id, created_at, error_traceback, error_message \
             FROM materializations WHERE node_id = ?1 ORDER BY id DESC LIMIT 1",
            [node_id],
        )
        .await
        .map_err(fail)?;
    let Some(row) = rows.next().await.map_err(fail)? else {
        return Ok(None);
    };
    if row.get::<String>(0).ok().as_deref() != Some("failed") {
        return Ok(None);
    }
    let text = |i: i32| row.get::<String>(i).ok().filter(|s| !s.is_empty());
    let traceback = [text(4), text(5)]
        .into_iter()
        .flatten()
        .find_map(|tb| raised_at(&tb, root));
    let mut facts = FailureFacts {
        raised_at: traceback,
        run_hash: text(1),
        run_id: text(2),
        created_at: text(3).unwrap_or_default(),
        last_success: None,
    };
    drop(rows);
    let mut rows = conn
        .query(
            "SELECT run_hash, created_at, run_id FROM materializations \
             WHERE node_id = ?1 AND status = 'success' ORDER BY id DESC LIMIT 1",
            [node_id],
        )
        .await
        .map_err(fail)?;
    if let Some(row) = rows.next().await.map_err(fail)? {
        let text = |i: i32| row.get::<String>(i).ok().filter(|s| !s.is_empty());
        facts.last_success = Some(LastSuccess {
            run_hash: text(0),
            created_at: text(1).unwrap_or_default(),
            run_id: text(2),
        });
    }
    Ok(Some(facts))
}

/// The last frame of a Python `traceback` whose file is inside `root` and not installed code,
/// with its path relative to `root`.
pub fn raised_at(traceback: &str, root: &Path) -> Option<SourceLine> {
    traceback.lines().rev().find_map(|line| {
        let (file, line_no, function) = parse_frame(line)?;
        let path = PathBuf::from(file);
        let path = if path.is_absolute() { path } else { root.join(path) };
        let relative = normalize(&path).strip_prefix(normalize(root)).ok()?.to_path_buf();
        let installed = relative.components().any(|c| {
            matches!(c, Component::Normal(s) if s == "site-packages" || s == "dist-packages" || s.to_str().is_some_and(|s| s.starts_with(".venv") || s == "venv"))
        });
        (!installed).then(|| SourceLine {
            file: relative.display().to_string(),
            line: line_no,
            function,
        })
    })
}

/// `  File "PATH", line N, in FN` → (PATH, N, FN).
fn parse_frame(line: &str) -> Option<(&str, u32, Option<String>)> {
    let rest = line.trim_start().strip_prefix("File \"")?;
    let (file, rest) = rest.split_once("\", line ")?;
    let (number, rest) = rest.split_once(", in ").unwrap_or((rest.trim_end(), ""));
    let function = Some(rest.trim())
        .filter(|f| !f.is_empty())
        .map(str::to_string);
    Some((file, number.trim().parse().ok()?, function))
}

/// `a/./b/../c` → `a/c`, without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TB: &str = "  File \"/srv/app/.venv/lib/python3.12/site-packages/barca/_worker.py\", line 10, in run\n    x()\n  File \"/srv/app/pipeline/assets.py\", line 136, in broken_report\n    helper()\n  File \"/srv/app/pipeline/util.py\", line 4, in helper\n    pd.read()\n  File \"/srv/app/.venv/lib/python3.12/site-packages/pandas/io.py\", line 99, in read\n    raise KeyError('revenue')";

    #[test]
    fn raised_at_is_the_last_frame_inside_the_project() {
        let at = raised_at(TB, Path::new("/srv/app")).unwrap();
        assert_eq!(
            at,
            SourceLine {
                file: "pipeline/util.py".into(),
                line: 4,
                function: Some("helper".into())
            }
        );
    }

    #[test]
    fn raised_at_skips_frames_outside_the_root() {
        assert_eq!(raised_at(TB, Path::new("/elsewhere")), None);
        assert_eq!(raised_at("ValueError: boom", Path::new("/srv/app")), None);
    }

    #[tokio::test]
    async fn failure_facts_name_the_raising_line_and_the_last_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db").display().to_string();
        super::super::init_db(&path).await.unwrap();
        let root = Path::new("/srv/app");
        assert!(
            failure_facts(&path, "a.py:x", root)
                .await
                .unwrap()
                .is_none()
        );
        {
            let _guard = super::super::db_guard().await;
            let (_db, conn) = super::super::open_conn(&path).await.unwrap();
            conn.execute(
                "INSERT INTO materializations(node_id, status, run_hash, run_id) VALUES ('a.py:x', 'success', 'h1', 'r1')",
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO materializations(node_id, status, run_hash, run_id, error_message, error_traceback) VALUES ('a.py:x', 'failed', 'h2', 'r2', 'boom', ?1)",
                [TB],
            )
            .await
            .unwrap();
        }
        let facts = failure_facts(&path, "a.py:x", root).await.unwrap().unwrap();
        assert_eq!(facts.run_hash.as_deref(), Some("h2"));
        assert_eq!(facts.raised_at.unwrap().line, 4);
        let last = facts.last_success.unwrap();
        assert_eq!(
            (last.run_hash.as_deref(), last.run_id.as_deref()),
            (Some("h1"), Some("r1"))
        );
    }
}
