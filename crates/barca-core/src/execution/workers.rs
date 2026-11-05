//! Starting the worker pool, the transfer helper and the step recorder for a run, and the
//! uploads an earlier run left pending.

use super::*;

pub(super) async fn start_workers(
    request: &ExecuteRequest<'_>,
    prepared: &Prepared,
    started: &mut RunStart,
    pb: &Option<indicatif::ProgressBar>,
) -> Result<(Option<StoreSync>, crate::io_loop::WorkerPool, StepRecorder), BarcaError> {
    let cfg = request.cfg;
    let python = request.python;
    let cancel = &started.cancel;
    let pool_size = prepared.pool_size;
    let run_id = &started.run_id;
    let telemetry = &started.telemetry;
    let job_name = &prepared.job_name;
    let db_path = &cfg.db_path;
    // Separate artifact store: workers still read and write only the local
    // artifact dir; the transfer helper uploads finished artifacts in the
    // background and fetches cache hits recorded by other machines.
    //
    // A store checked in the last five minutes is not checked again (#593): the client starts
    // its helper at the first transfer, and a run that transfers nothing starts none. Results
    // an earlier command could not upload end that: they are retried first, as below.
    if started.store_checked && !crate::pending_uploads::list(db_path).await?.is_empty() {
        started.store_checked = false;
        started.transfer_start = Some(TransferClient::launch(python, cfg, run_id)?);
    }
    let store: Option<StoreSync> = if started.store_checked {
        trace_point!(
            started.trace,
            "store_check_skipped (checked within {}s)",
            crate::gc::STORE_CHECK_WINDOW.as_secs()
        );
        Some(
            StoreSync::new(
                TransferClient::deferred(python, cfg, run_id)?,
                cancel.clone(),
            )
            .check_before_fetching(std::time::Duration::from_secs(
                cfg.transfer_timeout_secs.min(10),
            )),
        )
    } else if let Some(start) = started.transfer_start.take() {
        let mut client = start.connect().await?;
        let limit = std::time::Duration::from_secs(cfg.transfer_timeout_secs.min(10));
        let location = crate::transfer::diagnostic_uri(&cfg.artifact_root);
        crate::run_line!(
            Always,
            "[barca] checking artifact store {location} (timeout {}s)",
            limit.as_secs()
        );
        let checked = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(BarcaError::Cancelled),
            checked = tokio::time::timeout(limit, client.preflight()) => match checked {
                Ok(Ok(())) => Ok(()),
                Ok(Err(why)) => Err(BarcaError::Other(format!(
                    "artifact store preflight at {location} failed: {why}\n{STORE_PREFLIGHT_FIX}"
                ))),
                Err(_) => Err(BarcaError::Other(format!(
                    "artifact store preflight at {location} timed out after {}s\n{STORE_PREFLIGHT_FIX}",
                    limit.as_secs()
                ))),
            },
        };
        if let Err(error) = checked {
            client.abort().await;
            crate::gc::forget_store_check(cfg);
            return Err(error);
        }
        crate::gc::remember_store_check(cfg);
        // Results an earlier command computed here and could not upload go first (#390), and
        // with a server, the runs they held back are posted to its ledger (#507).
        match retry_pending_uploads(&mut client, db_path, cancel, &location, None).await {
            Ok(settled) => {
                if let Some(ledger) = &cfg.ledger {
                    ledger.post_uploaded(cfg, &settled).await;
                }
            }
            Err(error) => {
                client.abort().await;
                return Err(error);
            }
        }
        Some(StoreSync::new(client, cancel.clone()))
    } else {
        None
    };
    trace_point!(
        started.trace,
        "store_sync_started (enabled={})",
        store.is_some()
    );
    // Store location of every output uploaded this run, by node id.
    // Set when an artifact cannot be fetched or uploaded: the run fails.
    let worker_artifact_root = match &store {
        Some(s) => s.layout.local_root().to_string_lossy().into_owned(),
        None => cfg.artifact_root.clone(),
    };

    // Persistent worker pool: one pool for the whole run, shared across
    // phases so workers keep their interpreter (and imported user modules)
    // warm between phases.
    let io_config = crate::io_loop::IoConfig {
        upload_snapshot_prefix: started
            .upload_snapshots
            .as_ref()
            .map(|s| s.prefix.to_string_lossy().into_owned()),
        python: python.to_path_buf(),
        pool_size,
        capacity: prepared.capacity,
        host: request.options.host.clone(),
        run_id: run_id.clone(),
        datadog: telemetry
            .iter()
            .any(|(name, _)| name == "datadog")
            .then(|| {
                std::sync::Arc::new(crate::telemetry::datadog::Dispatch::new(
                    &prepared.dag,
                    job_name,
                    run_id,
                ))
            }),
        artifact_root: worker_artifact_root,
        storage_options_json: cfg.storage_options_json.clone(),
        agent_mode: request.agent_mode,
    };
    let mut pool = crate::io_loop::WorkerPool::start(io_config).map_err(BarcaError::Other)?;
    // Finished steps are written to the local metadata DB while the run goes on (#214), and,
    // with a store, backed up on the minute checkpoint (RFC-0009).
    let backups = cfg.backup_uri.is_some().then(|| BackupCheckpoints {
        python: python.to_path_buf(),
        cfg: cfg.clone(),
        cancel: cancel.clone(),
    });
    let recorder = StepRecorder::start_with_backups(db_path.clone(), run_id.clone(), backups);
    {
        // A step that runs for a while must not look hung: report it periodically.
        let bar = pb.clone();
        let lines = crate::lines::Lines::current();
        pool.on_running(Box::new(move |running| match &bar {
            Some(bar) if !bar.is_hidden() => {
                if let Some((id, secs)) = running.first() {
                    bar.set_message(format!("{} running {}s", short_name(id), *secs as u64));
                }
            }
            _ => {
                for (id, secs) in running {
                    lines.print(
                        crate::lines::LineWhen::Always,
                        format_args!("[barca] still running ({}s): {id}", *secs as u64),
                    );
                }
            }
        }));
    }
    trace_point!(started.trace, "pool_started");

    Ok((store, pool, recorder))
}

/// Upload what earlier commands on this machine left pending (#390), before this command's own
/// work, and wait for it. One that fails again stays pending with its new error; the run goes
/// on either way, and reports what is still pending when it ends. A pending result whose local
/// file is gone has nothing left to upload and is dropped, and taken out of its run's held
/// ledger post. Returns the store locations of the pending results uploaded or dropped.
pub(super) async fn retry_pending_uploads(
    client: &mut TransferClient,
    db_path: &str,
    cancel: &CancellationToken,
    location: &str,
    only: Option<&std::collections::HashSet<&str>>,
) -> Result<std::collections::HashSet<String>, BarcaError> {
    use crate::pending_uploads::Settled;
    let mut pending = crate::pending_uploads::list(db_path).await?;
    if let Some(only) = only {
        pending.retain(|p| only.contains(p.store_path.as_str()));
    }
    if pending.is_empty() {
        return Ok(Default::default());
    }
    let mut settled = Vec::new();
    let mut queued: Vec<&str> = Vec::new();
    for p in &pending {
        let here = std::path::Path::new(&p.local_path).is_file();
        // The store moved (another `[remote] uri`): this object no longer belongs anywhere.
        let same_store = client.layout().store_for(&p.local_path).as_deref() == Some(&p.store_path);
        if !here || !same_store {
            crate::run_line!(
                Always,
                "[barca] warning: dropped the pending upload of {}: {}",
                p.node_id,
                if !here {
                    format!("its local file {} is gone", p.local_path)
                } else {
                    format!(
                        "it belongs to another store ({})",
                        crate::transfer::diagnostic_uri(&p.store_path)
                    )
                }
            );
            settled.push((p.store_path.clone(), Settled::Dropped));
            continue;
        }
        if client.upload(&p.store_path, &p.local_path).is_some() {
            queued.push(&p.store_path);
        }
    }
    if !queued.is_empty() {
        crate::run_line!(
            Always,
            "[barca] uploading {} pending result{} to {location} from an earlier run",
            queued.len(),
            if queued.len() == 1 { "" } else { "s" }
        );
        let report = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(BarcaError::Cancelled),
            report = client.drain() => report,
        };
        let failed = report.failures.len();
        for store_path in &queued {
            if !report.failures.iter().any(|f| f.key == *store_path) {
                let sha256 = report.hashes.get(*store_path).cloned();
                settled.push((store_path.to_string(), Settled::Uploaded(sha256)));
            }
        }
        let mut first_error = None;
        for f in report.failures {
            let error = format!(
                "upload to {} failed ({} attempt{}): {}",
                crate::transfer::diagnostic_uri(&f.store),
                f.attempts,
                if f.attempts == 1 { "" } else { "s" },
                f.message
            );
            first_error.get_or_insert_with(|| f.message.clone());
            settled.push((f.key, Settled::Failed(error)));
        }
        let uploaded = queued.len() - failed;
        match first_error {
            None => crate::run_line!(
                Always,
                "[barca] uploaded {uploaded} pending result{}",
                if uploaded == 1 { "" } else { "s" }
            ),
            Some(why) => crate::run_line!(
                Always,
                "[barca] {failed} pending result{} still not uploaded ({uploaded} uploaded): {why}",
                if failed == 1 { "" } else { "s" }
            ),
        }
    }
    crate::pending_uploads::settle(db_path, &settled).await?;
    let dropped: std::collections::HashSet<String> = settled
        .iter()
        .filter(|(_, s)| matches!(s, Settled::Dropped))
        .map(|(at, _)| at.clone())
        .collect();
    crate::ledger::posts::forget_steps(db_path, &dropped).await?;
    Ok(settled
        .into_iter()
        .filter(|(_, s)| !matches!(s, Settled::Failed(_)))
        .map(|(at, _)| at)
        .collect())
}

/// Upload this host's pending results (#390) among `store_paths` (artifact-store locations) now,
/// before something needs them in the store: `barca get -o url` signs only objects the store
/// holds. Starts the transfer helper only when one of them is pending; one that fails again
/// stays pending with its new error, and the caller finds it still missing from the store.
pub(crate) async fn upload_pending(
    python: &std::path::Path,
    cfg: &crate::config::ResolvedConfig,
    store_paths: &[String],
    cancel: &CancellationToken,
) -> Result<(), BarcaError> {
    if !cfg.remote_artifacts() {
        return Ok(());
    }
    let wanted: std::collections::HashSet<&str> = store_paths.iter().map(String::as_str).collect();
    let pending = crate::pending_uploads::list(&cfg.db_path).await?;
    if !pending
        .iter()
        .any(|p| wanted.contains(p.store_path.as_str()))
    {
        return Ok(());
    }
    let mut client = TransferClient::launch(python, cfg, &db::generate_run_id())?
        .connect()
        .await?;
    let location = crate::transfer::diagnostic_uri(&cfg.artifact_root);
    let retried =
        retry_pending_uploads(&mut client, &cfg.db_path, cancel, &location, Some(&wanted)).await;
    match retried {
        Ok(settled) => {
            client.shutdown().await;
            if let Some(ledger) = &cfg.ledger {
                ledger.post_uploaded(cfg, &settled).await;
            }
            Ok(())
        }
        Err(error) => {
            client.abort().await;
            Err(error)
        }
    }
}
