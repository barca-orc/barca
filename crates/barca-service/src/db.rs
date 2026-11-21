//! The metadata database as callers of `barca-service` see it: the reads `barca-cli` and
//! `barca-server` make directly, the few writes the server owns, and the row types they render.
//! Everything else of `barca_db::db` stays inside `barca-service` (the glob below is
//! crate-private), so this list is the whole metadata surface callers get. An operation that is
//! more than one call (open the database, then write; read, then decide) belongs in a core
//! module instead: `pauses::{list, pause, resume}`, `serve::record_unstarted_run`. The one
//! defined here is [`open_metadata`], which every such operation starts with.

pub use barca_db::open::open_metadata;

/// This environment's metadata DB for a command that only reads it: its configuration with
/// `db_path` naming a [`ReadView`] of the database, kept open for as long as this lives.
///
/// Reads through a view take neither barca's cross-process lock nor the process-wide guard
/// (#1056, A7): `barca status` beside a recording run, or beside a `barca serve` that the
/// command was told not to use (`--local`), does not wait for a write and no write waits for
/// it.
///
/// Reading never changes what a command does to a database that cannot be read in place
/// (missing, written by an earlier release and not yet completed, not in WAL mode, an empty
/// file). With `create`, the commands that have always created or completed the database
/// first (`history`, `stats`, `logs`: [`open_metadata`]) still do, and then view it. Without,
/// nothing is opened here: the reads are made on the database itself, under the lock, exactly
/// as before there were views, so the caller's own steps decide what happens to it (`barca
/// status` refuses an empty file, and completes an earlier release's database only where it
/// always did, after its cache decisions).
pub struct MetadataRead {
    cfg: crate::config::ResolvedConfig,
    _view: Option<ReadView>,
}

impl MetadataRead {
    /// The configuration to read with: `db_path` is the path to read from.
    pub fn cfg(&self) -> &crate::config::ResolvedConfig {
        &self.cfg
    }

    /// The path to read from: a view's, or the database's own.
    pub fn path(&self) -> &str {
        &self.cfg.db_path
    }

    /// Whether the reads are made in place, without barca's lock.
    pub fn in_place(&self) -> bool {
        self._view.is_some()
    }
}

/// [`MetadataRead`] of `cfg`'s database. With `create`, a missing database is created (what
/// `barca history` in a new project has always done); without, it stays missing and the
/// caller reads nothing.
pub async fn read_metadata(
    cfg: &crate::config::ResolvedConfig,
    create: bool,
) -> Result<MetadataRead, crate::BarcaError> {
    let plain = |cfg: &crate::config::ResolvedConfig| MetadataRead {
        cfg: cfg.clone(),
        _view: None,
    };
    // Already a view (a server's route hands its own to the same functions).
    if is_read_view(&cfg.db_path) {
        return Ok(plain(cfg));
    }
    let viewed = |view: ReadView| {
        let mut read = cfg.clone();
        read.db_path = view.path().to_string();
        MetadataRead {
            cfg: read,
            _view: Some(view),
        }
    };
    if let Some(view) = ReadView::open(&cfg.db_path).await? {
        return Ok(viewed(view));
    }
    if !create {
        // Not this function's to create, migrate or convert: see [`MetadataRead`].
        return Ok(plain(cfg));
    }
    open_metadata(cfg).await?;
    Ok(match ReadView::open(&cfg.db_path).await? {
        Some(view) => viewed(view),
        None => plain(cfg),
    })
}

// Inside barca-service, `crate::db::<anything>` still names `barca_db::db`.
pub(crate) use barca_db::db::*;

// Opening the database: `barca serve` checks and creates it before it binds, and its scheduler
// and queue create it on first use; read views are the snapshots routes read from.
pub use barca_db::db::{
    DbSnapshot, ReadView, directory_check, ensure_env_dirs, env_local_paths, init_db, init_db_as,
    probe_metadata, serve_socket_path, unusable_file,
};

// Runs: the run list, one run in full (steps, logs, outcome, trace ids), and what the server
// records about runs it owns.
pub use barca_db::db::{
    AliveRun, LogEntry, RUN_STEPS_LIMIT, RunFilter, RunRecord, RunStep, RunTotals, StepsPage,
    alive_running_runs, alive_running_runs_in, attach_trace_ids, count_runs, count_runs_filtered,
    generate_run_id, get_logs, get_recent_runs, get_run, get_run_outcome, get_run_steps_page,
    get_run_steps_window, get_run_traced, get_runs_filtered, replace_run_outcome, run_artifact,
    run_node_summaries, set_cancellation, set_run_outcome,
};

// Assets: stats, ages, durations, partitions, step rows, pins and their versions.
pub use barca_db::db::{
    AssetPin, AssetRunEntry, AssetStats, BytesChange, NodeAge, NodeAgesIndex, PartitionKeys,
    PartitionKeysFilter, RowChange, RowFacts, StepRow, Version, failure_facts, get_asset_stats,
    node_ages, partition_keys, read_step_rows, recent_durations,
};

// The scheduler's durable state: last ticks, schedule pauses, failure streaks.
pub use barca_db::db::{
    ScheduleFailures, SchedulePause, clamp_schedule_cursors, get_schedule_failures,
    get_schedule_pauses, get_schedule_state, max_materialization_id, set_schedule_failures,
    set_schedule_pause, succeeded_since, upsert_schedule_state,
};

// The tick record (#960): what each due tick of a schedule decided and how its run ended. The
// server's scheduler and run recorder write it; `ticks_for_schedule`, `tick_outcomes_for_node`,
// `latest_ticks` (every node at once, what health reads) and `recent_ticks` (the rows of
// `barca.ticks`) are the reads.
pub use barca_db::db::{
    LatestTicks, NewTick, NodeTickOutcome, TICK_MAX_AGE_SECONDS, TICKS_PER_SCHEDULE,
    TICKS_WITH_TARGETS, TickCounts, TickDecision, TickRecord, TickTargetOutcome, TickTimings,
    components_of_rows, latest_ticks, recent_ticks, record_tick, record_tick_finished,
    record_tick_started, tick_outcomes_for_node, tick_timings, ticks_for_schedule, trim_ticks,
};

// Fixtures for the tests of `barca-cli` and `barca-server`, which write runs without running
// them (a dependency's `cfg(test)` items do not reach its dependents).
#[doc(hidden)]
pub use barca_db::db::{
    create_run, db_guard, finish_run, insert_logs, is_read_view, open_conn, persist_outputs,
};

/// What a command does with the metadata DB as it ends: the connections this process kept
/// are closed (the last one to close folds the log into the main file, as the close of every
/// operation used to), then the lock is reported ([`trace_lock_holds`]). The CLI calls this
/// before every exit: its one `exit` function does (`crates/barca-cli/src/main.rs`).
pub fn end_of_command() {
    barca_db::db::close_connections();
    trace_lock_holds();
}

/// With `BARCA_TRACE_TIMING` set: one stderr line with how often this process took a metadata
/// database's lock and for how long, in total and at most. The longest hold is the longest
/// another barca process can have waited for this one. Printed as a command ends.
pub fn trace_lock_holds() {
    if std::env::var_os("BARCA_TRACE_TIMING").is_none() {
        return;
    }
    let holds = barca_db::db::lock_holds();
    if holds.holds > 0 {
        crate::errln!(
            "[trace] metadata lock holds {} (total {:.1}ms, longest {:.1}ms)",
            holds.holds,
            holds.total.as_secs_f64() * 1000.0,
            holds.longest.as_secs_f64() * 1000.0
        );
    }
}
