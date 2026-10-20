//! What the metadata database knows about a job, for integrations that post on a change
//! rather than on every run: the job's earlier finished runs (`runs.job`) and the last post
//! an integration made about it (the `notifications` table, one row per job and integration).

use chrono::NaiveDateTime;

/// SQLite's `datetime('now')` format, which `runs` timestamps use too.
const STAMP: &str = "%Y-%m-%d %H:%M:%S";

/// What an integration last posted about a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostState {
    Failing,
    StillFailing,
    Recovered,
    Succeeded,
}

impl PostState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failing => "failing",
            Self::StillFailing => "still_failing",
            Self::Recovered => "recovered",
            Self::Succeeded => "succeeded",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "failing" => Some(Self::Failing),
            "still_failing" => Some(Self::StillFailing),
            "recovered" => Some(Self::Recovered),
            "succeeded" => Some(Self::Succeeded),
            _ => None,
        }
    }

    pub fn is_failing(self) -> bool {
        matches!(self, Self::Failing | Self::StillFailing)
    }
}

#[derive(Debug, Clone)]
pub struct Post {
    /// UTC.
    pub posted_at: NaiveDateTime,
    pub state: PostState,
}

/// One job as of a finished run.
#[derive(Debug, Clone, Default)]
pub struct JobRecord {
    /// Status of the job's previous finished run (`success` or `failed`), not counting this one.
    pub previous: Option<String>,
    /// When the job's last successful run before this one finished (UTC).
    pub last_success_at: Option<NaiveDateTime>,
    /// When the first failed run since the last success started, as SQLite text (UTC).
    pub failing_since: Option<String>,
    /// Failed runs since the last success, this one included.
    pub failed_runs: u64,
    /// The last post the integration recorded for the job.
    pub last_post: Option<Post>,
}

/// Read access to a job's history, and a place to record a post. Without a database (a
/// skipped `barca serve` tick, a test) there is nothing to read.
#[derive(Debug, Clone, Default)]
pub struct JobHistory {
    db_path: Option<String>,
}

fn failed(e: impl std::fmt::Display) -> String {
    format!("metadata database: {e}")
}

fn stamp(raw: Option<String>) -> Option<NaiveDateTime> {
    raw.and_then(|s| NaiveDateTime::parse_from_str(&s, STAMP).ok())
}

impl JobHistory {
    pub fn new(db_path: &str) -> Self {
        Self {
            db_path: Some(db_path.to_string()),
        }
    }

    /// No database: [`Self::read`] returns `None` and nothing is recorded.
    pub fn none() -> Self {
        Self::default()
    }

    /// The job as of finished run `run_id`, which is already recorded. `None` without a
    /// database.
    pub async fn read(
        &self,
        job: &str,
        run_id: &str,
        integration: &str,
    ) -> Result<Option<JobRecord>, String> {
        let Some(db_path) = &self.db_path else {
            return Ok(None);
        };
        let _g = crate::db::db_guard().await;
        let (_db, conn) = crate::db::open_conn(db_path).await.map_err(failed)?;
        let mut record = JobRecord::default();

        let mut rows = conn
            .query(
                "SELECT status FROM runs INDEXED BY idx_runs_job WHERE job = ?1 AND run_id <> ?2 \
                 AND status IN ('success', 'failed') ORDER BY id DESC LIMIT 1",
                [job, run_id],
            )
            .await
            .map_err(failed)?;
        if let Some(row) = rows.next().await.map_err(failed)? {
            record.previous = row.get::<String>(0).ok();
        }
        drop(rows);

        let mut rows = conn
            .query(
                "SELECT id, finished_at FROM runs INDEXED BY idx_runs_job WHERE job = ?1 \
                 AND run_id <> ?2 AND status = 'success' ORDER BY id DESC LIMIT 1",
                [job, run_id],
            )
            .await
            .map_err(failed)?;
        let mut last_success_id = 0i64;
        if let Some(row) = rows.next().await.map_err(failed)? {
            last_success_id = row.get::<i64>(0).map_err(failed)?;
            record.last_success_at = stamp(row.get::<String>(1).ok());
        }
        drop(rows);

        let mut rows = conn
            .query(
                "SELECT COUNT(*), MIN(started_at) FROM runs INDEXED BY idx_runs_job \
                 WHERE job = ?1 AND status = 'failed' AND id > ?2",
                (job, last_success_id),
            )
            .await
            .map_err(failed)?;
        if let Some(row) = rows.next().await.map_err(failed)? {
            record.failed_runs = row.get::<i64>(0).map_or(0, |n| n.max(0) as u64);
            record.failing_since = row.get::<String>(1).ok();
        }
        drop(rows);

        let mut rows = conn
            .query(
                "SELECT posted_at, state FROM notifications WHERE job = ?1 AND integration = ?2",
                [job, integration],
            )
            .await
            .map_err(failed)?;
        if let Some(row) = rows.next().await.map_err(failed)? {
            let posted_at = stamp(row.get::<String>(0).ok());
            let state = row.get::<String>(1).ok().and_then(|s| PostState::parse(&s));
            if let (Some(posted_at), Some(state)) = (posted_at, state) {
                record.last_post = Some(Post { posted_at, state });
            }
        }
        Ok(Some(record))
    }

    /// Record that `integration` posted `state` about `job` at `posted_at` (UTC), replacing
    /// its previous post. `message_ref` stays empty: a webhook returns no message id.
    pub async fn record_post(
        &self,
        job: &str,
        integration: &str,
        state: PostState,
        posted_at: NaiveDateTime,
    ) -> Result<(), String> {
        let Some(db_path) = &self.db_path else {
            return Ok(());
        };
        let _g = crate::db::db_guard().await;
        let (_db, conn) = crate::db::open_conn(db_path).await.map_err(failed)?;
        conn.execute(
            "INSERT INTO notifications (job, integration, posted_at, state, message_ref) \
             VALUES (?1, ?2, ?3, ?4, '') ON CONFLICT(job, integration) DO UPDATE SET \
             posted_at = ?3, state = ?4, message_ref = ''",
            [
                job.to_string(),
                integration.to_string(),
                posted_at.format(STAMP).to_string(),
                state.as_str().to_string(),
            ],
        )
        .await
        .map_err(failed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn finished(db_path: &str, run_id: &str, job: &str, status: &str, started: &str) {
        let (_db, conn) = crate::db::open_conn(db_path).await.unwrap();
        conn.execute(
            "INSERT INTO runs (run_id, command, files, status, job, started_at, finished_at) \
             VALUES (?1, 'get', '[]', ?2, ?3, ?4, ?4)",
            [run_id, status, job, started],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn reads_the_previous_run_the_failing_streak_and_the_last_post() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("m.db").display().to_string();
        crate::db::init_db(&db_path).await.unwrap();
        let history = JobHistory::new(&db_path);
        let job = "p.py:a";
        finished(&db_path, "r1", job, "failed", "2026-10-18 05:00:00").await;
        finished(&db_path, "r2", job, "success", "2026-10-18 06:00:00").await;
        finished(&db_path, "r3", job, "failed", "2026-10-19 06:00:00").await;
        // Not outcomes of the job, and another job: ignored.
        finished(&db_path, "r4", job, "cancelled", "2026-10-19 06:30:00").await;
        finished(&db_path, "r5", job, "interrupted", "2026-10-19 06:40:00").await;
        finished(&db_path, "r6", "p.py:b", "success", "2026-10-19 06:50:00").await;
        finished(&db_path, "r7", job, "failed", "2026-10-19 07:00:00").await;

        let record = history.read(job, "r7", "slack").await.unwrap().unwrap();
        assert_eq!(record.previous.as_deref(), Some("failed"));
        assert_eq!(record.failed_runs, 2);
        assert_eq!(record.failing_since.as_deref(), Some("2026-10-19 06:00:00"));
        assert_eq!(
            record.last_success_at,
            stamp(Some("2026-10-18 06:00:00".into()))
        );
        assert!(record.last_post.is_none());

        let now = stamp(Some("2026-10-19 07:00:05".into())).unwrap();
        history
            .record_post(job, "slack", PostState::Failing, now)
            .await
            .unwrap();
        history
            .record_post(job, "slack", PostState::StillFailing, now)
            .await
            .unwrap();
        let post = history
            .read(job, "r7", "slack")
            .await
            .unwrap()
            .unwrap()
            .last_post
            .unwrap();
        assert_eq!((post.posted_at, post.state), (now, PostState::StillFailing));
        // Per integration.
        let other = history.read(job, "r7", "other").await.unwrap().unwrap();
        assert!(other.last_post.is_none());

        // A job with no earlier run.
        let fresh = history
            .read("p.py:new", "r8", "slack")
            .await
            .unwrap()
            .unwrap();
        assert!(fresh.previous.is_none() && fresh.last_success_at.is_none());
    }

    #[tokio::test]
    async fn without_a_database_there_is_nothing_to_read() {
        let history = JobHistory::none();
        assert!(history.read("j", "r", "slack").await.unwrap().is_none());
    }
}
