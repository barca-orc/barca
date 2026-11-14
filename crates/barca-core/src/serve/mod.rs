//! What exists only for `barca serve`: the durable run queue, the capacity its runs share
//! (#628), the asset steps they share while in flight (#654) and the server's auth mode.
//! A `barca get` or `barca run` on its own uses none of it: the hot loop reaches what is
//! here only through [`crate::run_host::RunHost`], as [`ServeHost`].

pub mod capacity;
pub mod claims;
mod host;
pub mod run_queue;

pub use crate::config::AuthMode;
pub use host::ServeHost;

/// A run the server accepted and never started, as [`record_unstarted_run`] records it.
pub struct UnstartedRun<'a> {
    pub run_id: &'a str,
    pub command: &'a str,
    pub files: &'a [String],
    /// The run's targets joined with `,`, `None` for every node.
    pub target: Option<&'a str>,
    pub trigger: &'a crate::outcome::Trigger,
}

/// Record `run` in the metadata DB at `db_path` as ended with `status` and `outcome_json`
/// although no step of it started (refused, cancelled while queued, or interrupted by a
/// stopped server), so the run list and `barca history` show it.
pub async fn record_unstarted_run(
    db_path: &str,
    run: UnstartedRun<'_>,
    status: &str,
    outcome_json: &str,
) -> Result<(), crate::BarcaError> {
    crate::db::record_unstarted_run(
        db_path,
        crate::db::NewRun {
            run_id: run.run_id,
            command: run.command,
            files: &crate::db::encode_files(run.files),
            target: run.target,
            steps_total: None,
            job: None,
            trigger: run.trigger,
            traced: false,
        },
        status,
        outcome_json,
    )
    .await
}
