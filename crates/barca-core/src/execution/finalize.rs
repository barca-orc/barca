//! The end of a run: the outputs it returns, the ledger and history it records, stopping the
//! workers, draining the store transfer, the final backup and repairing targets whose copies
//! disagree.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn finalize_outputs(
    dag: &Dag,
    targets: &[(String, String)],
    exec_plan: &ExecutionPlan,
    keep_going: bool,
    selection: Option<&crate::selection::PartitionSelection>,
    all_outputs: &HashMap<String, dispatch::OutputRef>,
    all_failures: &[dispatch::StepFailure],
    step_reports: Vec<StepReport>,
    failed_bases: &std::collections::HashSet<String>,
    skipped_bases: &std::collections::HashSet<String>,
    cached: &std::collections::HashSet<String>,
    store: &mut Option<StoreSync>,
    cancel: &CancellationToken,
    adopt_digests_in: Option<&str>,
) -> Result<
    (
        Vec<StepReport>,
        Vec<(String, TargetOutcome)>,
        Option<OutputRef>,
        Option<Vec<(String, OutputRef)>>,
        Option<crate::results::KeySummary>,
    ),
    BarcaError,
> {
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    // Steps reported as `ran` before dispatch that failed, or were skipped because something
    // upstream failed, say so.
    let mut step_reports = merge_partition_reports(step_reports);
    for report in &mut step_reports {
        if !matches!(
            report.status.as_deref().and_then(StepStatus::parse),
            Some(StepStatus::Ran | StepStatus::Partial)
        ) {
            continue;
        }
        let base = report.id.split('[').next().unwrap_or(&report.id);
        if failed_bases.contains(base) {
            report.status = Some(StepStatus::Failed.to_string());
        } else if skipped_bases.contains(base) {
            report.status = Some(StepStatus::Skipped.to_string());
            report.reason = Some("upstream_failed".to_string());
            report.detail = Some("a step it depends on failed".to_string());
        }
    }

    // Later phases may never have been decided after a single-target failure. Keep
    // their blocked nodes in the detailed result too, once per base node (as with
    // merged partition reports), so the human count covers the whole planned cone.
    let mut reported: std::collections::HashSet<String> =
        step_reports.iter().map(|s| s.id.clone()).collect();
    for step in exec_plan
        .phases
        .iter()
        .flat_map(|p| &p.streams)
        .flat_map(|s| &s.steps)
    {
        let base = step.step_id.base_id();
        if !reported.contains(base)
            && let Some(up) = blocking_failure(dag, base, failed_bases)
        {
            reported.insert(base.to_string());
            step_reports.push(StepReport {
                id: base.to_string(),
                kind: kind_str(dag.get_node(base).map(|n| n.kind())),
                status: Some(StepStatus::Skipped.to_string()),
                reason: Some("upstream_failed".to_string()),
                detail: Some(format!("depends on '{}', which failed", short_name(up))),
                ..Default::default()
            });
        }
    }

    let mut outcomes = target_outcomes(dag, targets, all_outputs, all_failures, selection);
    // Many keys: `final_output` lists the computed ones and counts the rest (#623).
    for ((_, tid), (_, outcome)) in targets.iter().zip(outcomes.iter_mut()) {
        outcome.key_summary = outcome.partitions.as_deref().and_then(|keys| {
            crate::results::KeySummary::of(tid, keys, selection.is_some(), cached)
        });
    }

    let returned_node = returned_node(exec_plan, &target_ids, keep_going);
    let partitions = returned_node
        .as_deref()
        .and_then(|node| crate::selection::partition_outputs(dag, node, all_outputs, selection));
    let key_summary = match (&returned_node, &partitions) {
        (Some(node), Some(keys)) => {
            crate::results::KeySummary::of(node, keys, selection.is_some(), cached)
        }
        _ => None,
    };
    let final_output = match &partitions {
        Some(keys) => keys.first().map(|(_, o)| o.clone()),
        None => final_output_of(exec_plan, &target_ids, keep_going, all_outputs),
    };

    // The outputs this command returns were made readable here when the last phase finished
    // (`Work::Returned`). A run that stopped at a failed step did not get that far and may still
    // return an earlier output, so make sure of it. Then stop the transfer helper.
    if let Some(mut s) = store.take() {
        let paths: Vec<String> = final_output
            .iter()
            .chain(partitions.iter().flatten().map(|(_, o)| o))
            .chain(outcomes.iter().flat_map(|(_, o)| {
                o.final_output
                    .iter()
                    .chain(o.partitions.iter().flatten().map(|(_, r)| r))
            }))
            .map(|o| o.path.clone())
            .collect();
        let fetched = s.ensure_local(paths.iter().map(String::as_str), None).await;
        if fetched.is_err() && cancel.is_cancelled() {
            // Ctrl-C while the returned output was being fetched. The run itself is over:
            // its record is written (and, with a store, backed up), and it stays as it is.
            // Only the command is cancelled.
            s.client.abort().await;
            return Err(BarcaError::Cancelled);
        }
        s.client.shutdown().await;
        if let Err(e) = fetched {
            return Err(BarcaError::Other(e));
        }
        crate::mismatch::mark(dag, &mut step_reports, &s.mismatched);
        // A fetched store copy that is not the recorded one was used: record its digest, so
        // the finding is this run's only (#576).
        if let Some(db_path) = adopt_digests_in {
            db::adopt_store_digests(db_path, &s.adopted).await;
        }
    }

    Ok((
        step_reports,
        outcomes,
        final_output,
        partitions,
        key_summary,
    ))
}

/// The node a single-target run (or a run with no target) returns.
pub(super) fn returned_node(
    exec_plan: &ExecutionPlan,
    target_ids: &[&str],
    several_targets: bool,
) -> Option<String> {
    match target_ids {
        [one] => Some(one.to_string()),
        _ if several_targets => None,
        _ => Some(
            crate::planner::returned_step(exec_plan)?
                .step_id
                .base_id()
                .to_string(),
        ),
    }
}

/// The keys a single-target run (or a run with no target) returns when the node it returns
/// is partitioned; see [`crate::selection::partition_outputs`].
pub(super) fn returned_partitions(
    dag: &Dag,
    exec_plan: &ExecutionPlan,
    target_ids: &[&str],
    several_targets: bool,
    all_outputs: &HashMap<String, OutputRef>,
    selection: Option<&crate::selection::PartitionSelection>,
) -> Option<Vec<(String, OutputRef)>> {
    let node = returned_node(exec_plan, target_ids, several_targets)?;
    crate::selection::partition_outputs(dag, &node, all_outputs, selection)
}

pub(super) struct FinalizeRun<'a> {
    pub(super) results: &'a mut RunResults,
    pub(super) incremental: Option<&'a crate::incremental::Incremental>,
    pub(super) proven_cached: usize,
    /// What the run read ([`crate::persist::RunLedger::touched`]).
    pub(super) touched: &'a [String],
    pub(super) pool: crate::io_loop::WorkerPool,
    pub(super) recorder: StepRecorder,
    pub(super) pb: &'a Option<indicatif::ProgressBar>,
    pub(super) agent_mode: bool,
    pub(super) steps_executed: usize,
    pub(super) completed_steps: usize,
    pub(super) total_steps: usize,
    pub(super) cancel: &'a CancellationToken,
    pub(super) phase_error: &'a Option<String>,
    pub(super) step_failure: &'a Option<(String, String)>,
    pub(super) store: &'a mut Option<StoreSync>,
    pub(super) store_paths: &'a mut HashMap<String, String>,
    pub(super) transfer_error: &'a mut Option<String>,
    pub(super) uploads: &'a mut Option<crate::results::UploadsReport>,
    pub(super) metadata_seconds: &'a mut Option<f64>,
    pub(super) cached_node_ids: &'a std::collections::HashSet<String>,
    pub(super) t0: Instant,
    pub(super) cost_model: &'a crate::cost::CostModel,
    pub(super) run_id: &'a str,
    pub(super) command_label: &'a str,
    pub(super) file_args: &'a [String],
    pub(super) target_label: &'a Option<String>,
    pub(super) decide_state: &'a DecideState,
    pub(super) db_path: &'a str,
    pub(super) logs_buffer: &'a mut crate::run_logs::Recorder,
    pub(super) telemetry: &'a [(String, Box<dyn crate::telemetry::Integration>)],
    pub(super) dag: &'a Dag,
    pub(super) run_started: std::time::SystemTime,
    pub(super) skipped_bases: &'a std::collections::HashSet<String>,
    pub(super) job_name: &'a str,
    /// The command's targets, (name, node id): what a `get` repairs in the store.
    pub(super) targets: &'a [(String, String)],
    pub(super) repaired_mismatches: &'a mut Vec<String>,
    pub(super) python: &'a std::path::Path,
    pub(super) cfg: &'a crate::config::ResolvedConfig,
    pub(super) interrupt: &'a crate::interrupt::Interrupt,
    pub(super) trigger: &'a crate::outcome::Trigger,
    pub(super) server_owned: bool,
}

pub(super) async fn finalize_run(
    ctx: FinalizeRun<'_>,
    trace: impl Fn(&str),
) -> Result<f64, BarcaError> {
    let FinalizeRun {
        results,
        incremental,
        proven_cached,
        touched,
        pool,
        recorder,
        pb,
        agent_mode,
        steps_executed,
        completed_steps,
        total_steps,
        cancel,
        phase_error,
        step_failure,
        store,
        store_paths,
        transfer_error,
        uploads,
        metadata_seconds,
        cached_node_ids,
        t0,
        cost_model,
        run_id,
        command_label,
        file_args,
        target_label,
        decide_state,
        db_path,
        logs_buffer,
        telemetry,
        dag,
        run_started,
        skipped_bases,
        job_name,
        targets,
        repaired_mismatches,
        python,
        cfg,
        interrupt,
        trigger,
        server_owned,
    } = ctx;
    macro_rules! trace_point {
        ($($arg:tt)*) => { trace(&format!($($arg)*)); };
    }
    let batch_lines: Vec<String> = results
        .batch_execution
        .iter()
        .map(|(node, calls)| batch_line(node, calls))
        .collect();
    for line in &batch_lines {
        logs_buffer.push_note(line.clone());
    }
    stop_workers(
        StopProgress {
            batch_lines: &batch_lines,
            pool,
            pb,
            agent_mode,
            steps_executed,
            completed_steps,
            total_steps,
            // The run's wall time so far, not the sum of its steps' times (they overlap).
            wall_seconds: t0.elapsed().as_secs_f64(),
            cancel,
            phase_error,
            step_failure,
        },
        &trace,
    )
    .await;
    // A `get` of a target that is cached here but missing from the store, or different there,
    // repairs the store from this machine's copy without a step running (RFC-0009). The
    // uploads are drained, and kept pending on failure, with the run's own below.
    //
    // A run inside the store check window that started no helper (it transferred nothing)
    // does not ask the store either (#593).
    if !cancel.is_cancelled()
        && let Some(s) = store.as_mut()
        && s.client.started()
    {
        // What is already pending was retried before the run; it is not repaired twice.
        let pending: std::collections::HashSet<String> = crate::pending_uploads::list(db_path)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.store_path)
            .collect();
        repair_targets(
            s,
            targets,
            cached_node_ids,
            &results.outputs,
            &pending,
            repaired_mismatches,
        )
        .await;
    }
    let mut pending = Vec::new();
    let mut upload_error = None;
    let store_location = store
        .as_ref()
        .map(|s| crate::transfer::diagnostic_uri(s.layout.store_root()));
    let mut was_cancelled = drain_store(
        DrainStore {
            store,
            timeout_secs: cfg.transfer_timeout_secs,
            cancel,
            all_outputs: &mut results.outputs,
            store_paths,
            pending: &mut pending,
            upload_error: &mut upload_error,
        },
        &trace,
    )
    .await;
    // Before the rows: a result recorded with a store location whose upload failed is always
    // known to be pending. (A crash in between leaves a pending upload with no row, which the
    // next command uploads harmlessly.)
    for p in &mut pending {
        p.run_hash = decide_state.run_hashes().get(&p.node_id).cloned();
        p.run_id = Some(run_id.to_string());
    }
    crate::pending_uploads::record(db_path, &pending).await?;
    // A transfer that failed: the next run checks the store again (#593).
    if store_location.is_some() && (upload_error.is_some() || transfer_error.is_some()) {
        crate::gc::forget_store_check(cfg);
    }

    let steps_cached = cached_node_ids.len() + proven_cached;
    let elapsed = t0.elapsed().as_secs_f64();

    // Stop the step recorder before persistence: the ledger below writes whatever it had not
    // written yet, and the backup checkpoints the WAL, which requires no other open handle
    // on the file.
    let recording = recorder.busy_seconds();
    let recorder_error = recorder.finish().await.err();
    trace_point!("recorder_stopped");

    // Each sink a step wrote holds its run hash (`crate::sink_publications`).
    results.note_written_sinks(dag, decide_state.run_hashes());
    // Build the immutable terminal projection only after uploads and recorder shutdown.
    let evidence = RunEvidence::new(results, dag, decide_state, cached_node_ids, cost_model);
    let mut ledger = results.ledger(
        LedgerContext {
            run_id,
            status: if was_cancelled {
                "cancelled"
            } else if phase_error.is_some() || step_failure.is_some() || transfer_error.is_some() {
                "failed"
            } else {
                "success"
            },
            command: command_label,
            files: file_args,
            target: target_label.as_deref(),
            job: job_name,
            steps_total: total_steps,
            steps_executed,
            steps_cached,
            elapsed,
            cached_node_ids,
            decisions: decide_state,
            store_paths,
            touched,
        },
        &evidence,
    );
    let writing_ledger = std::time::Instant::now();
    persist_run(db_path, &ledger).await?;
    if !batch_lines.is_empty() {
        let seconds = recording + writing_ledger.elapsed().as_secs_f64();
        *metadata_seconds = Some(seconds);
        let line = format!("[barca] metadata {seconds:.1}s recording this run's results");
        logs_buffer.push_note(line.clone());
        if pb.is_some() || agent_mode {
            crate::run_line!(Always, "{line}");
        }
    }
    if let Some(error) = recorder_error {
        crate::run_line!(
            Always,
            "[barca] {error}; terminal results have been recorded"
        );
    }
    // Why the run was cancelled, when whoever cancelled it said (`barca serve` stopping
    // before its drain finished): a line in the run's logs and `barca.reason` on its span.
    let cancel_reason = was_cancelled
        .then(crate::interrupt::cancel_reason)
        .flatten();
    if let Some(reason) = &cancel_reason {
        logs_buffer.push_note(format!("[barca] run cancelled: {reason}"));
    }
    // Persist captured output. Rust owns persistence — logs land in the DB
    // regardless of how the run was triggered (CLI or server). The spool, which readers of a
    // run in progress used, goes once the lines are in.
    let spool = logs_buffer.close_spool();
    db::insert_logs(db_path, run_id, logs_buffer.lines()).await?;
    if let Some(spool) = spool {
        let _ = std::fs::remove_file(spool);
    }
    trace_point!("persist_run_done");

    if !telemetry.is_empty() {
        let mut report = telemetry_report(
            &ledger,
            dag,
            crate::persist::TelemetryRun {
                started: run_started,
                clocks: &results.clocks,
                skipped: skipped_bases,
                files: file_args,
                env: Some(&cfg.env),
            },
        );
        if let Some(incremental) = incremental {
            incremental.telemetry(dag, &mut report);
        }
        if report.reason.is_none() {
            report.reason = cancel_reason;
        }
        report.trigger = trigger.clone();
        // A tick reports as its scheduled node's job; its span names every target it ran.
        if decide_state.tick.is_some() {
            let mut names: Vec<String> = targets
                .iter()
                .map(|(_, id)| crate::telemetry::short_name(id).to_string())
                .collect();
            names.sort();
            names.dedup();
            report.targets = names;
        }
        let history = crate::telemetry::JobHistory::new(db_path);
        crate::telemetry::export(telemetry, &report, &history).await;
        trace_point!("telemetry_exported");
    }

    // With a store: back the history up, here and in the store (RFC-0009). Only the local
    // copy failing, or Ctrl-C, can change the command's outcome; a failed upload is a warning
    // (#390), and the next backup sends the copy.
    let mut publish_error = None;
    // A run that recorded no new result takes no backup (#554): nothing failed, nothing was
    // uploaded (no new artifact, no repair, no pending retry), every step it ran was a sensor
    // whose observation stayed unchanged, and every earlier run is in a backup the store
    // confirmed. Its run row reaches the store with the next backup.
    let nothing_new = cfg.backup_uri.is_some()
        && !was_cancelled
        && results.failures.is_empty()
        && step_failure.is_none()
        && phase_error.is_none()
        && transfer_error.is_none()
        && store.as_ref().is_some_and(|s| {
            s.client.uploads_queued() == 0 && s.unchanged_observations == steps_executed
        })
        && crate::backup::covered(cfg, run_id).await;
    if nothing_new {
        crate::backup::mark_covered(cfg, run_id);
        trace_point!("backup_skipped (no new result)");
    }
    if cfg.backup_uri.is_some() && !nothing_new {
        match final_backup(
            FinalBackup {
                python,
                cfg,
                request: if server_owned {
                    crate::backup::Request::Routine
                } else {
                    crate::backup::Request::Final
                },
                run_id,
                ledger: &mut ledger,
                interrupt,
                cancelled: &mut was_cancelled,
            },
            &trace,
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(why)) => {
                crate::run_line!(
                    Always,
                    "[barca] warning: the metadata backup was not uploaded: {why}; the run is \
                     recorded on this machine, a copy is in {}, and the next backup uploads it",
                    cfg.local_backup_dir().display()
                );
                publish_error = Some(why);
            }
            Err(
                e @ (BarcaError::Cancelled
                | BarcaError::Db(_)
                | BarcaError::MetadataUnusable { .. }),
            ) => return Err(e),
            Err(e) => {
                let e = e.to_string();
                let e = e.lines().next().unwrap_or_default().to_string();
                crate::run_line!(
                    Always,
                    "[barca] warning: the metadata backup failed: {e}; the run is recorded on \
                     this machine and the next backup tries again"
                );
                publish_error = Some(format!("metadata backup failed: {e}"));
            }
        }
    }
    if store_location.is_some() || publish_error.is_some() {
        let left = crate::pending_uploads::list(db_path).await?;
        if !pending.is_empty() {
            let mut nodes: Vec<&str> = pending.iter().map(|p| p.node_id.as_str()).collect();
            nodes.sort_unstable();
            let shown = nodes
                .iter()
                .take(10)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            let more = match nodes.len().saturating_sub(10) {
                0 => String::new(),
                n => format!(" and {n} more"),
            };
            crate::run_line!(
                Always,
                "[barca] warning: {} result{} not uploaded to {} ({shown}{more}): {}; every step \
                 succeeded, the results are kept on this machine (`unsynced`, local only) and the \
                 next command that uses the store uploads them",
                pending.len(),
                if pending.len() == 1 { "" } else { "s" },
                store_location.as_deref().unwrap_or_default(),
                upload_error.as_deref().unwrap_or("upload failed"),
            );
        }
        let error = match (upload_error, publish_error) {
            (Some(u), Some(p)) => Some(format!("{u}; {p}")),
            (u, p) => u.or(p),
        };
        *uploads = crate::pending_uploads::report(&left, error);
    }

    // Propagate cancellation/worker error after persisting partial results.
    if was_cancelled {
        return Err(BarcaError::Cancelled);
    }
    if let Some(error) = phase_error {
        // Only the pool itself (e.g. no worker could be spawned) sets `phase_error`; user step
        // failures are in `step_failure`. Infra, not user code: exit 3.
        return Err(BarcaError::Other(format!("Worker failed: {error}")));
    }
    if let Some(error) = transfer_error.take() {
        return Err(BarcaError::Other(error));
    }

    Ok(elapsed)
}

/// What to do when the artifact store cannot be reached before a run: the second line of the
/// error, so it is the envelope's `remediation` instead of the generic infra one.
pub(super) const STORE_PREFLIGHT_FIX: &str =
    concat!("Nothing ran. ", crate::store_sync::store_check_fix!());

pub(super) struct StopProgress<'a> {
    pub(super) batch_lines: &'a [String],
    pub(super) pool: crate::io_loop::WorkerPool,
    pub(super) pb: &'a Option<indicatif::ProgressBar>,
    pub(super) agent_mode: bool,
    pub(super) steps_executed: usize,
    pub(super) completed_steps: usize,
    pub(super) total_steps: usize,
    pub(super) wall_seconds: f64,
    pub(super) cancel: &'a CancellationToken,
    pub(super) phase_error: &'a Option<String>,
    pub(super) step_failure: &'a Option<(String, String)>,
}

pub(super) async fn stop_workers(ctx: StopProgress<'_>, trace: &impl Fn(&str)) {
    let StopProgress {
        batch_lines,
        mut pool,
        pb,
        agent_mode,
        steps_executed,
        completed_steps,
        total_steps,
        wall_seconds,
        cancel,
        phase_error,
        step_failure,
    } = ctx;
    macro_rules! trace_point { ($($arg:tt)*) => { trace(&format!($($arg)*)); }; }
    // All phases done (or aborted/cancelled) — release the worker pool before
    // persisting.
    let repeated_warnings = pool.take_repeated_warnings();
    pool.shutdown().await;
    trace_point!("pool_shutdown");

    // Finish progress bar. The end-of-run line is the same with and without --agent.
    if let Some(bar) = pb.as_ref() {
        bar.finish_and_clear();
    }
    // Library warnings the workers printed once and then suppressed (every mode).
    // The ten most repeated get a line each, so the summary cannot become the noise it removes.
    const SUMMARY_LINES: usize = 10;
    for (text, n) in repeated_warnings.iter().take(SUMMARY_LINES) {
        crate::run_line!(Always, "[barca] {n} more: {text}");
    }
    if repeated_warnings.len() > SUMMARY_LINES {
        let rest = &repeated_warnings[SUMMARY_LINES..];
        crate::run_line!(
            Always,
            "[barca] {} more: {} other repeated warnings",
            rest.iter().map(|(_, n)| n).sum::<u64>(),
            rest.len()
        );
    }
    if (pb.is_some() || agent_mode) && steps_executed > 0 {
        for line in batch_lines {
            crate::run_line!(Always, "{line}");
        }
        let outcome = if cancel.is_cancelled() {
            RunOutcome::Cancelled
        } else if phase_error.is_some() || step_failure.is_some() {
            RunOutcome::Failed
        } else {
            RunOutcome::Done
        };
        crate::run_line!(
            Always,
            "{}",
            end_of_run_line(completed_steps, total_steps, wall_seconds, outcome)
        );
    }
}

pub(super) struct DrainStore<'a> {
    pub(super) timeout_secs: u64,
    pub(super) store: &'a mut Option<StoreSync>,
    pub(super) cancel: &'a CancellationToken,
    pub(super) all_outputs: &'a mut HashMap<String, OutputRef>,
    pub(super) store_paths: &'a mut HashMap<String, String>,
    /// Uploads that failed: kept pending, their steps recorded (#390).
    pub(super) pending: &'a mut Vec<crate::pending_uploads::PendingUpload>,
    /// The first upload failure's message, for the run's warning and `uploads.error`.
    pub(super) upload_error: &'a mut Option<String>,
}

pub(super) async fn drain_store(ctx: DrainStore<'_>, trace: &impl Fn(&str)) -> bool {
    let DrainStore {
        timeout_secs,
        store,
        cancel,
        all_outputs,
        store_paths,
        pending,
        upload_error,
    } = ctx;
    macro_rules! trace_point { ($($arg:tt)*) => { trace(&format!($($arg)*)); }; }
    let mut was_cancelled = cancel.is_cancelled();

    // Artifact store, before anything is recorded: wait for every upload. Rows are recorded
    // only for artifacts confirmed in the store, so the metadata never points at a missing
    // object. The transfer client stays up to fetch the final outputs below.
    //
    // Ctrl-C during the wait cancels the run like one during a step: the uploads still in
    // flight are abandoned and their steps are not recorded.
    let drained = match store.as_mut() {
        Some(s) if !was_cancelled => {
            let queued = s.client.pending_uploads();
            let t_drain = Instant::now();
            let location = crate::transfer::diagnostic_uri(s.layout.store_root());
            if queued > 0 {
                crate::run_line!(
                    Always,
                    "[barca] uploading {queued} artifacts to {location} (timeout {timeout_secs}s per attempt); waiting for confirmation"
                );
            }
            let drain = s.client.drain();
            tokio::pin!(drain);
            let mut progress = tokio::time::interval_at(
                tokio::time::Instant::now() + std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(10),
            );
            let report = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break None,
                    report = &mut drain => break Some(report),
                    _ = progress.tick(), if queued > 0 => crate::run_line!(Always,
                        "[barca] still waiting for artifact uploads to {location} ({:.0}s)",
                        t_drain.elapsed().as_secs_f64()
                    ),
                }
            };
            trace_point!("store_sync_drained ({queued} uploads)");
            was_cancelled = report.is_none();
            report.map(|report| (report, t_drain))
        }
        _ => None,
    };
    if was_cancelled {
        if let Some(s) = store.take() {
            let (unconfirmed, hashes) = s.client.abort().await;
            for node in unconfirmed {
                all_outputs.remove(&node);
                store_paths.remove(&node);
            }
            for (node, sha256) in hashes {
                if let Some(oref) = all_outputs.get_mut(&node) {
                    oref.content_hash.get_or_insert(sha256);
                }
            }
        }
    } else if let Some((report, t_drain)) = drained {
        // The hash of the bytes that reached the store is recorded with the row, so any
        // machine can check its copy of the artifact against it.
        for (node, sha256) in &report.hashes {
            if let Some(oref) = all_outputs.get_mut(node) {
                oref.content_hash.get_or_insert_with(|| sha256.clone());
            }
        }
        if report.transferred > 0 {
            crate::run_line!(
                Always,
                "[barca] uploaded {} artifact{} ({}); waited {:.1}s at end of run",
                report.transferred,
                if report.transferred == 1 { "" } else { "s" },
                crate::gc::size::human(report.bytes),
                t_drain.elapsed().as_secs_f64()
            );
        }
        // Every step that reached here succeeded; a failed upload does not undo that (#390).
        // The row is recorded with its store location like any other, and the upload is kept
        // pending: the next command that opens the store uploads it before its own work.
        if !report.failures.is_empty() {
            let messages: Vec<&str> = report.failures.iter().map(|f| f.message.as_str()).collect();
            let remedy = transfer_remedy(&messages, "");
            for f in &report.failures {
                let error = format!(
                    "upload to {} failed ({} attempt{}): {}",
                    crate::transfer::diagnostic_uri(&f.store),
                    f.attempts,
                    if f.attempts == 1 { "" } else { "s" },
                    f.message
                );
                if upload_error.is_none() {
                    *upload_error = Some(match remedy.as_str() {
                        "" => error.clone(),
                        remedy => format!("{error} {remedy}"),
                    });
                }
                let Some(oref) = all_outputs.get(&f.key) else {
                    continue;
                };
                pending.push(crate::pending_uploads::PendingUpload {
                    store_path: f.store.clone(),
                    node_id: f.key.clone(),
                    run_hash: None,
                    run_id: None,
                    local_path: oref.path.clone(),
                    error: Some(error),
                });
            }
        }
    }

    was_cancelled
}

/// The backup at the end of a run. `Ok(Some(why))` when only its upload failed. Ctrl-C during it
/// cancels the command (the run is recorded as cancelled); a cancelled run's wrap-up backup
/// gets [`crate::interrupt::WRAP_UP_LIMIT`], and a second Ctrl-C ends it at once.
pub(super) struct FinalBackup<'a, 'run> {
    pub(super) python: &'a std::path::Path,
    pub(super) cfg: &'a crate::config::ResolvedConfig,
    pub(super) request: crate::backup::Request,
    pub(super) run_id: &'a str,
    pub(super) ledger: &'a mut RunLedger<'run>,
    pub(super) interrupt: &'a crate::interrupt::Interrupt,
    pub(super) cancelled: &'a mut bool,
}

pub(super) async fn final_backup(
    ctx: FinalBackup<'_, '_>,
    trace: &impl Fn(&str),
) -> Result<Option<String>, BarcaError> {
    let FinalBackup {
        python,
        cfg,
        request,
        run_id,
        ledger,
        interrupt,
        cancelled,
    } = ctx;
    macro_rules! trace_point { ($($arg:tt)*) => { trace(&format!($($arg)*)); }; }
    let started = Instant::now();
    let until = if *cancelled {
        crate::process::Until {
            cancel: Some(&interrupt.abandon),
            deadline: Some(Instant::now() + crate::interrupt::WRAP_UP_LIMIT),
        }
    } else {
        crate::process::Until::cancelled(&interrupt.cancel)
    };
    let wrap_up = *cancelled;
    // Whether `until` fired: the backup (its copy, or the upload of a copy already taken) was
    // stopped, which is not a store failure.
    let stopped = || {
        if wrap_up {
            interrupt.abandon.is_cancelled() || started.elapsed() >= crate::interrupt::WRAP_UP_LIMIT
        } else {
            interrupt.cancel.is_cancelled()
        }
    };
    let taken = match crate::backup::take_requested(python, cfg, Some(run_id), request, until).await
    {
        Ok(taken) if !(matches!(taken.upload, Some(Err(_))) && stopped()) => taken,
        Ok(_) | Err(BarcaError::Cancelled) => {
            if !wrap_up {
                // The run's work is done and recorded; the command was stopped during its
                // backup, so it is cancelled.
                ledger.status = "cancelled";
                cancel_recorded_run(&cfg.db_path, ledger).await?;
                *cancelled = true;
                crate::run_line!(
                    Always,
                    "[barca] the metadata backup was stopped; the run is recorded on this \
                     machine and the next backup takes it"
                );
            } else {
                crate::run_line!(
                    Always,
                    "[barca] the metadata backup did not finish ({}); the run is recorded on \
                     this machine and the next backup takes it",
                    if interrupt.abandon.is_cancelled() {
                        "stopped by a second interrupt".to_string()
                    } else {
                        format!("not within {}s", crate::interrupt::WRAP_UP_LIMIT.as_secs())
                    }
                );
            }
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    trace_point!("backup_done (written={})", taken.name.is_some());
    match taken.upload {
        Some(Ok(at)) if taken.name.is_some() => {
            crate::run_line!(
                Always,
                "[barca] backed up the metadata DB ({}) to {at} in {:.2}s",
                crate::gc::size::human(taken.size_bytes),
                started.elapsed().as_secs_f64()
            );
            Ok(None)
        }
        Some(Err(why)) if wrap_up => {
            crate::run_line!(
                Always,
                "[barca] the metadata backup was not uploaded ({why}); the next backup uploads it"
            );
            Ok(None)
        }
        Some(Err(why)) => Ok(Some(why)),
        _ => Ok(None),
    }
}

/// For each target served from cache with a copy on this machine: ask the store for its
/// object, and upload this machine's copy when the store has none (`local_only`) or a
/// different one (`mismatch`, recorded in `repaired` for the changed-bytes warning). A local
/// copy that is not the one recorded is left to the fetch, which replaces it with the store's
/// (#247). A target with no copy here is fetched when its output is returned (`store_only`).
/// Only unpartitioned targets are checked.
pub(super) async fn repair_targets(
    s: &mut StoreSync,
    targets: &[(String, String)],
    cached: &std::collections::HashSet<String>,
    outputs: &HashMap<String, OutputRef>,
    pending: &std::collections::HashSet<String>,
    repaired: &mut Vec<String>,
) {
    // Every target that can be checked, then one round of questions to the store (#554).
    let mut checks: Vec<(&String, &OutputRef, String, u64, String)> = Vec::new();
    for (_, id) in targets {
        if !cached.contains(id) {
            continue;
        }
        let Some(oref) = outputs.get(id) else {
            continue;
        };
        let local = std::path::Path::new(&oref.path);
        let Ok(meta) = std::fs::metadata(local) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(store) = s.layout.store_for(&oref.path) else {
            continue;
        };
        if pending.contains(&store) || checks.iter().any(|c| c.2 == store) {
            continue;
        }
        let Ok(local_sha) = crate::hash::file_sha256(local) else {
            continue;
        };
        if oref.content_hash.as_deref().is_some_and(|h| h != local_sha) {
            continue;
        }
        checks.push((id, oref, store, meta.len(), local_sha));
    }
    if checks.is_empty() {
        return;
    }
    let stores: Vec<String> = checks.iter().map(|c| c.2.clone()).collect();
    let answers = s.client.stat_all(&stores).await;
    for ((id, oref, store, len, local_sha), answer) in checks.into_iter().zip(answers) {
        let differs = match answer {
            Ok(None) => false,
            Ok(Some((size, sha))) => {
                if size == len && sha.as_deref().is_none_or(|h| h == local_sha) {
                    continue;
                }
                true
            }
            // The store cannot say: leave it. `barca status` reports what it finds.
            Err(_) => continue,
        };
        crate::run_line!(
            Always,
            "[barca] {id}: {}; uploading this machine's copy to {}",
            if differs {
                "the artifact store holds different bytes"
            } else {
                "not in the artifact store"
            },
            crate::transfer::diagnostic_uri(&store)
        );
        s.client.upload(id, &oref.path);
        if differs {
            repaired.push(id.clone());
        }
    }
}
