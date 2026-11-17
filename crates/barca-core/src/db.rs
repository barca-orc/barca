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
    alive_running_runs, alive_running_runs_in, attach_trace_ids, count_runs, count_runs_triggered,
    generate_run_id, get_logs, get_recent_runs, get_recent_runs_triggered, get_run,
    get_run_outcome, get_run_steps_page, get_run_steps_window, get_run_traced, replace_run_outcome,
    run_artifact, run_node_summaries, set_cancellation, set_run_outcome,
};

// Assets: stats, ages, durations, partitions, step rows, pins and their versions.
pub use barca_db::db::{
    AssetPin, AssetRunEntry, AssetStats, BytesChange, NodeAge, NodeAgesIndex, PartitionKeys,
    PartitionKeysFilter, RowChange, RowFacts, StepRow, Version, failure_facts, get_asset_stats,
    node_ages, partition_keys, read_step_rows, recent_durations,
};

// The scheduler's durable state: last ticks, schedule pauses, failure streaks.
pub use barca_db::db::{
    ScheduleFailures, SchedulePause, get_schedule_failures, get_schedule_pauses,
    get_schedule_state, max_materialization_id, set_schedule_failures, set_schedule_pause,
    succeeded_since, upsert_schedule_state,
};

// The tick record (#960): what each due tick of a schedule decided and how its run ended. The
// server's scheduler and run recorder write it; `ticks_for_schedule`, `tick_outcomes_for_node`
// and `latest_ticks` (every node at once, what health reads) are the reads.
pub use barca_db::db::{
    LatestTicks, NewTick, NodeTickOutcome, TICK_MAX_AGE_SECONDS, TICKS_PER_SCHEDULE,
    TICKS_WITH_TARGETS, TickCounts, TickDecision, TickRecord, TickTargetOutcome,
    components_of_rows, latest_ticks, record_tick, record_tick_finished, record_tick_started,
    tick_outcomes_for_node, ticks_for_schedule, trim_ticks,
};

// Fixtures for the tests of `barca-cli` and `barca-server`, which write runs without running
// them (a dependency's `cfg(test)` items do not reach its dependents).
#[doc(hidden)]
pub use barca_db::db::{
    create_run, db_guard, finish_run, insert_logs, is_read_view, persist_outputs,
};
