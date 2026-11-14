//! The metadata database as callers of `barca-core` see it: the reads `barca-cli` and
//! `barca-server` make directly, the few writes the server owns, and the row types they render.
//! Everything else of `barca_db::db` stays inside `barca-core` (the glob below is
//! crate-private), so this list is the whole metadata surface callers get. An operation that is
//! more than one call (open the database, then write; read, then decide) belongs in a core
//! module instead: `queries::open_metadata`, `pauses::{list, pause, resume}`,
//! `serve::record_unstarted_run`.

// Inside barca-core, `crate::db::<anything>` still names `barca_db::db`.
pub(crate) use barca_db::db::*;

// Opening the database: `barca serve` checks and creates it before it binds, and its scheduler
// and queue create it on first use; read views are the snapshots routes read from.
pub use barca_db::db::{
    DbSnapshot, ReadView, directory_check, ensure_env_dirs, env_local_paths, init_db, init_db_as,
    probe_metadata, unusable_file,
};

// Runs: the run list, one run in full (steps, logs, outcome, trace ids), and what the server
// records about runs it owns.
pub use barca_db::db::{
    AliveRun, LogEntry, RUN_STEPS_LIMIT, RunRecord, RunStep, RunTotals, StepsPage,
    alive_running_runs, alive_running_runs_in, attach_trace_ids, count_runs, generate_run_id,
    get_logs, get_recent_runs, get_run, get_run_outcome, get_run_steps_page, get_run_traced,
    replace_run_outcome, run_artifact, run_node_summaries, set_run_outcome,
};

// Assets: stats, ages, durations, partitions, step rows, pins and their versions.
pub use barca_db::db::{
    AssetPin, AssetRunEntry, AssetStats, BytesChange, NodeAge, PartitionKeys, PartitionKeysFilter,
    StepRow, Version, failure_facts, get_asset_stats, node_ages, partition_keys, read_step_rows,
    recent_durations,
};

// The scheduler's durable state: last ticks, schedule pauses, failure streaks.
pub use barca_db::db::{
    ScheduleFailures, SchedulePause, get_schedule_failures, get_schedule_pauses,
    get_schedule_state, max_materialization_id, set_schedule_failures, set_schedule_pause,
    succeeded_since, upsert_schedule_state,
};

// Fixtures for the tests of `barca-cli` and `barca-server`, which write runs without running
// them (a dependency's `cfg(test)` items do not reach its dependents).
#[doc(hidden)]
pub use barca_db::db::{
    create_run, db_guard, finish_run, insert_logs, is_read_view, persist_outputs,
};
