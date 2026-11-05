//! Worker processes: starting the pool and each worker, taking a worker that connected,
//! replacing one that died before connecting, retiring surplus workers and shutting down.

use super::*;

impl WorkerPool {
    /// Bind the coordination socket. Workers spawn on demand during phases.
    /// Must be called from within a tokio runtime context.
    pub fn start(config: IoConfig) -> Result<Self, String> {
        let socket_path = crate::protocol::socket_path(&config.run_id, "main")?;
        std::fs::remove_file(&socket_path).ok();
        let listener = UnixListener::bind(&socket_path).map_err(|e| format!("socket bind: {e}"))?;
        // Best effort: without the directory each worker prints a repeated warning once
        // instead of the run printing it once.
        let claims = warning_claims_dir(&socket_path);
        std::fs::remove_dir_all(&claims).ok();
        std::fs::create_dir_all(&claims).ok();
        let (event_tx, event_rx) = mpsc::channel::<IoEvent>(config.pool_size.max(1) * 8);
        sweep_branch_results(&branch_results_parent());
        let branch_root = branch_results_root(&config.run_id);
        let acceptor = Acceptor(tokio::spawn(accept_connections(listener, event_tx.clone())));
        let share = config.host.capacity_changes().map(|changed| Share {
            host: config.host.clone(),
            total: config.host.shared_capacity().map_or(1, |b| b.total),
            changed,
            run_id: config.run_id.clone(),
            held: 0,
            grant: None,
            waiting: false,
        });
        let steps = config.host.shares_steps().then(|| Steps {
            host: config.host.clone(),
            run_id: config.run_id.clone(),
            held: Default::default(),
            awaiting: HashMap::new(),
            from_others: Vec::new(),
        });
        Ok(Self {
            config,
            socket_path,
            acceptor,
            event_tx,
            event_rx,
            workers: HashMap::new(),
            pending: HashMap::new(),
            frozen: Vec::new(),
            reaping: Vec::new(),
            next_worker_id: 0,
            trace_start: std::time::Instant::now(),
            trace_on: std::env::var("BARCA_TRACE_TIMING").is_ok(),
            progress_interval: Duration::from_secs(
                std::env::var("BARCA_PROGRESS_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(15),
            ),
            running_hook: None,
            repeated_warnings: Vec::new(),
            branch_root,
            next_branch_group: 0,
            branch_dirs: HashMap::new(),
            failures_survived: 0,
            steps_dispatched: 0,
            share,
            steps,
        })
    }

    /// Gracefully terminate every worker and remove the socket. Runs on the
    /// blocking pool because this polls for exit between SIGTERM and SIGKILL.
    ///
    /// Signals every worker up front and then polls them all together for one
    /// shared 200ms grace window, rather than the previous sequential
    /// SIGTERM-then-sleep(200ms)-then-check per worker — that cost
    /// `pool_size * 200ms` in the common case (every worker still mid-cleanup
    /// at the first `try_wait()`, right after its own SIGTERM), which for a
    /// default `pool_size` of 4 meant shutdown alone could take ~800ms
    /// regardless of how little work the run actually did (confirmed via
    /// `BARCA_TRACE_TIMING=1` — see benchmarks/RESULTS.md's docker-harness
    /// re-run notes).
    pub async fn shutdown(self) {
        if self.trace_on {
            // How many workers the run started, against the failed steps that kept theirs (a
            // failing step costs no worker start), and how many steps were sent to workers,
            // re-sends included (#523).
            crate::errln!(
                "[trace]  {:>8.1}ms  workers started {}, failed steps that kept their worker {}, steps dispatched {}",
                self.trace_start.elapsed().as_secs_f64() * 1000.0,
                self.next_worker_id,
                self.failures_survived,
                self.steps_dispatched
            );
        }
        drop(self.acceptor);
        // The runs waiting on this run's steps execute them themselves.
        drop(self.steps);
        let workers = self.workers;
        let frozen = self.frozen;
        let pending = self.pending;
        let kill_task = tokio::task::spawn_blocking(move || {
            let mut children: Vec<Child> =
                Vec::with_capacity(workers.len() + frozen.len() + pending.len());

            // Started and never connected (a cancelled run, or one that ended first): not
            // members of the pool, and still stopped and reaped here.
            #[cfg(unix)]
            for (_, p) in pending {
                unsafe {
                    libc::kill(p.child.id() as i32, libc::SIGTERM);
                }
                children.push(p.child);
            }
            #[cfg(not(unix))]
            for (_, p) in pending {
                children.push(p.child);
            }

            #[cfg(unix)]
            for (_, w) in workers {
                unsafe {
                    libc::kill(w.child.id() as i32, libc::SIGTERM);
                }
                children.push(w.child);
            }
            #[cfg(not(unix))]
            for (_, w) in workers {
                children.push(w.child);
            }

            #[cfg(unix)]
            for fw in frozen {
                unsafe {
                    libc::kill(fw.child.id() as i32, libc::SIGCONT);
                    libc::kill(fw.child.id() as i32, libc::SIGTERM);
                }
                children.push(fw.child);
            }
            #[cfg(not(unix))]
            for fw in frozen {
                children.push(fw.child);
            }

            #[cfg(unix)]
            {
                let deadline = std::time::Instant::now() + Duration::from_millis(200);
                while std::time::Instant::now() < deadline
                    && children
                        .iter_mut()
                        .any(|c| !matches!(c.try_wait(), Ok(Some(_))))
                {
                    // A worker exits within a millisecond or two of SIGTERM: polling every
                    // 5 ms made each run's shutdown about 5 ms (#560).
                    std::thread::sleep(Duration::from_millis(1));
                }
            }

            for mut c in children {
                if !matches!(c.try_wait(), Ok(Some(_))) {
                    let _ = c.kill();
                }
                let _ = c.wait();
            }
        });
        let _ = kill_task.await;
        // Rejected starts have already left `pending`. Their off-loop kill/wait
        // jobs still belong to this pool, including when the CLI is about to exit.
        for task in self.reaping {
            let _ = task.await;
        }
        // Every worker is gone, so nothing writes or reads a branch result any more: the
        // run's directory goes, whatever way the run ended.
        let branch_root = self.branch_root;
        let _ = tokio::task::spawn_blocking(move || {
            std::fs::remove_dir_all(&branch_root).ok();
        })
        .await;
        std::fs::remove_file(&self.socket_path).ok();
        std::fs::remove_dir_all(warning_claims_dir(&self.socket_path)).ok();
    }

    /// Stop workers that have nothing leased while the pool is over strength.
    ///
    /// A worker that calls `parallel()` is frozen and another is started in its place. When
    /// it is resumed there is one worker more than `pool_size`. The one that goes is whichever
    /// has no step leased, now or when it next finishes its batch: never one in the middle of
    /// a step.
    pub(super) fn retire_surplus(&mut self) {
        while self.workers.len() + self.pending.len() > self.config.pool_size.max(1) {
            let Some(idle) = self
                .workers
                .iter()
                .find(|(_, w)| w.leases.is_empty() && !w.batch_open)
                .map(|(&id, _)| id)
            else {
                return;
            };
            if let Some(mut handle) = self.workers.remove(&idle) {
                tokio::task::spawn_blocking(move || {
                    let _ = handle.child.kill();
                    let _ = handle.child.wait();
                });
            }
        }
    }

    /// Start, all at once, the workers the ready queue can use that no worker already
    /// connected or starting will take: one per batch it would be leased as, up to
    /// `pool_size` in all. A one-step run starts one worker. None is waited for.
    pub(super) fn start_workers(&mut self, coord: &Coordinator, cost: &CostModel) {
        let room = self
            .config
            .pool_size
            .max(1)
            .saturating_sub(self.workers.len() + self.pending.len());
        if room == 0 {
            return;
        }
        // Each worker still starting takes the first batch left when it connects.
        // Only as many as the capacity lets start: a worker for a step that has to wait would
        // sit idle (#436).
        let capacity = self.capacity();
        let mut running = self.running_weight(coord);
        // Under a shared capacity, no more than it has room for beside what this run holds:
        // the head's admitted weight and what is free (#628).
        let mut shared_room = self.share.as_ref().map(|s| {
            let free = s.total.saturating_sub(s.host.capacity_in_use());
            s.grant.unwrap_or(0) + free
        });
        let mut ready = coord.ready_items().peekable();
        let mut taken = 0;
        let mut batches = 0;
        while taken < coord.ready_count() && batches < self.pending.len() + room {
            let Some(&head) = ready.peek() else {
                break;
            };
            let weight = coord.item(head).spec.weight;
            if !crate::coordinator::fits(running, weight, capacity) {
                break;
            }
            if let (Some(left), Some(share)) = (shared_room.as_mut(), self.share.as_ref()) {
                let w = share.host.weight_of(weight);
                // The head was admitted, so its worker always starts.
                if batches > 0 && w > *left {
                    break;
                }
                *left = left.saturating_sub(w);
            }
            running += crate::coordinator::effective_weight(weight, capacity);
            taken += self.batch_len(coord, cost, &mut ready, coord.ready_count() - taken);
            batches += 1;
        }
        let wanted = batches.saturating_sub(self.pending.len()).min(room);
        if self.trace_on && wanted > 0 {
            crate::errln!(
                "[trace]  {:>8.1}ms  starting {wanted} worker(s) ({} already starting)",
                self.trace_start.elapsed().as_secs_f64() * 1000.0,
                self.pending.len()
            );
        }
        for _ in 0..wanted {
            let wid = self.next_worker_id;
            self.next_worker_id += 1;
            let cmd = worker_command(&self.config, &self.socket_path, wid);
            if let Err(e) = self.launch(cmd, wid) {
                crate::errln!("[barca] failed to spawn worker: {e}");
                return;
            }
        }
    }

    /// Start `cmd` as worker `worker_id`, without waiting for it to connect.
    pub(super) fn launch(&mut self, mut cmd: Command, worker_id: usize) -> Result<(), String> {
        let started = std::time::Instant::now();
        let child = crate::helper_proc::spawn_std(&mut cmd).map_err(|e| format!("spawn: {e}"))?;
        if self.trace_on {
            crate::errln!(
                "[trace]  worker {worker_id} process spawned in {:.1}ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        self.pending
            .insert(worker_id, PendingWorker { child, started });
        Ok(())
    }

    /// A connection arrived: it joins the pool as the worker its hello names (or, without
    /// a `worker_id`, the one whose process it comes from). One that matches no worker still
    /// starting (stopped already, or not barca's) is closed. Returns whether a worker joined.
    pub(super) fn on_connected(
        &mut self,
        worker_id: Option<usize>,
        pid: Option<u32>,
        stream: Result<UnixStream, String>,
    ) -> bool {
        let wid = worker_id
            .filter(|id| self.pending.contains_key(id))
            .or_else(|| {
                let pid = pid?;
                self.pending
                    .iter()
                    .find(|(_, p)| p.child.id() == pid)
                    .map(|(&id, _)| id)
            });
        let Some(wid) = wid else {
            if self.trace_on {
                crate::errln!(
                    "[trace]  connection from an unknown process (worker id {worker_id:?}, pid {pid:?}) closed"
                );
            }
            return false;
        };
        let pending = self.pending.remove(&wid).expect("matched above");
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                crate::errln!("[barca] failed to spawn worker: worker {wid} {e}");
                self.reap(pending.child);
                return false;
            }
        };
        if self.trace_on {
            crate::errln!(
                "[trace]  worker {wid} connected (accept) in {:.1}ms",
                pending.started.elapsed().as_secs_f64() * 1000.0
            );
        }
        let (cmd_tx, cmd_rx) = mpsc::channel::<serde_json::Value>(16);
        let task = tokio::spawn(worker_io_task(wid, stream, cmd_rx, self.event_tx.clone()));
        self.workers.insert(
            wid,
            WorkerHandle {
                child: pending.child,
                cmd_tx,
                _task: task,
                leases: VecDeque::new(),
                call: false,
                batch_open: false,
                front_since: std::time::Instant::now(),
            },
        );
        true
    }

    /// Fail the starting workers whose process has exited, or that have not connected within
    /// [`WORKER_CONNECT_LIMIT`]. Each is reported and reaped. Returns why each failed.
    pub(super) fn fail_dead_starts(&mut self) -> Vec<String> {
        let mut failed = Vec::new();
        for (&wid, p) in &mut self.pending {
            match p.child.try_wait() {
                Ok(Some(status)) => failed.push((
                    wid,
                    format!("worker {wid} exited before it connected ({status})"),
                )),
                Err(e) => failed.push((
                    wid,
                    format!("worker {wid} exited before it connected (its status could not be read: {e})"),
                )),
                Ok(None) if p.started.elapsed() >= WORKER_CONNECT_LIMIT => failed.push((
                    wid,
                    format!(
                        "timeout waiting for worker {wid} to connect and announce protocol v{}; reinstall barca in this Python environment",
                        crate::protocol::PROTOCOL_VERSION
                    ),
                )),
                Ok(None) => {}
            }
        }
        failed
            .into_iter()
            .map(|(wid, why)| {
                crate::errln!("[barca] failed to spawn worker: {why}");
                if let Some(p) = self.pending.remove(&wid) {
                    self.reap(p.child);
                }
                why
            })
            .collect()
    }

    /// Kill and reap a worker that is not part of the pool, off the event loop.
    pub(super) fn reap(&mut self, mut child: Child) {
        self.reaping.retain(|task| !task.is_finished());
        self.reaping.push(tokio::task::spawn_blocking(move || {
            let _ = child.kill();
            let _ = child.wait();
        }));
    }
}

/// The command that starts worker `worker_id` against the socket at `socket_path`.
pub(super) fn worker_command(config: &IoConfig, socket_path: &Path, worker_id: usize) -> Command {
    let mut cmd = crate::helper_proc::python_module_std(
        &config.python,
        "barca._worker",
        config.storage_options_json.as_deref(),
    );
    cmd.arg("--daemon")
        .env("BARCA_SOCKET", socket_path.to_str().unwrap_or(""))
        .env("BARCA_WORKER", "1")
        .env("BARCA_WORKER_ID", worker_id.to_string())
        .env("BARCA_ARTIFACT_URI", &config.artifact_root)
        // A step's own print() output goes to barca's stderr, never stdout: stdout carries
        // only barca's result, so `barca run ... | jq` works when steps print.
        // Both streams are one destination, as they always were; when barca's stderr is a
        // pipe it is a pipe barca owns, so a reader that leaves cannot fail the step or
        // kill a process it starts (`crate::term`, "What barca's child processes write to").
        .stdout(crate::term::child_output())
        .stderr(crate::term::child_output())
        .stdin(Stdio::null());

    if config.agent_mode {
        cmd.env("BARCA_WORKER_AGENT", "1");
    } else {
        cmd.env_remove("BARCA_WORKER_AGENT");
    }
    if let Some(prefix) = &config.upload_snapshot_prefix {
        cmd.env("BARCA_UPLOAD_SNAPSHOT_PREFIX", prefix);
    } else {
        cmd.env_remove("BARCA_UPLOAD_SNAPSHOT_PREFIX");
    }

    // Ctrl-C means something to a worker only while it runs a step. It starts outside the
    // terminal's job, so that one arriving while the interpreter starts is not a traceback,
    // and joins the job once it handles the signal itself.
    #[cfg(unix)]
    crate::helper_proc::start_outside_the_job(&mut cmd);
    cmd
}

// ─── Process lifecycle ────────────────────────────────────────────────────────

/// The exit status of a worker whose socket closed, polled without blocking for up to
/// [`EXIT_STATUS_WAIT`]. `None` when the process has not exited by then.
pub(super) async fn reap_briefly(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + EXIT_STATUS_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            _ => return None,
        }
    }
}

/// The name of a signal that ends a worker, `signal N` for one not listed.
pub(super) fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGTERM => "SIGTERM",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGINT => "SIGINT",
        libc::SIGHUP => "SIGHUP",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGALRM => "SIGALRM",
        libc::SIGSYS => "SIGSYS",
        libc::SIGTRAP => "SIGTRAP",
        _ => return format!("signal {signal}"),
    };
    name.to_string()
}

/// The step error for a worker that disconnected while running `node` (or, `frozen`, while
/// it waited for its `parallel()` branches): the exit status when there is one, so a worker
/// killed by the kernel reads as such and not as a bare disconnect (#433).
/// A failed attempt as Python prints an uncaught exception: the traceback (barca's frames left
/// out of a failure in user code) under its header, then `ErrorType: message`. Recorded as the step's stderr lines.
pub(super) fn traceback_lines(error_type: &str, message: &str, traceback: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if !traceback.trim().is_empty() {
        lines.push("Traceback (most recent call last):".to_string());
        lines.extend(traceback.lines().map(str::to_string));
    }
    let mut last = format!("{error_type}: {message}");
    if message.is_empty() {
        last = error_type.to_string();
    }
    lines.extend(last.lines().map(str::to_string));
    lines
}

pub(super) fn disconnect_message(
    node: &str,
    status: Option<std::process::ExitStatus>,
    frozen: bool,
) -> String {
    use std::os::unix::process::ExitStatusExt;
    let waiting = if frozen {
        " while it waited for its parallel() branches"
    } else {
        ""
    };
    let how = match status {
        Some(s) => match (s.signal(), s.code()) {
            (Some(libc::SIGKILL), _) => "SIGKILL (possibly the OOM killer)".to_string(),
            (Some(sig), _) => signal_name(sig),
            (None, Some(code)) => format!("code {code}"),
            (None, None) => return format!("worker disconnected{waiting}"),
        },
        None => return format!("worker disconnected{waiting}"),
    };
    format!("worker for {node} exited with {how}{waiting}")
}

/// Gracefully terminate a child process. Sends SIGTERM first to let the process
/// flush buffered stdout/stderr, then falls back to SIGKILL after a brief wait.
pub(super) fn graceful_kill(child: &mut Child) {
    #[cfg(unix)]
    {
        // Send SIGTERM for a graceful shutdown.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        // Give the process a moment to flush and exit.
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
    }
    // Fallback: SIGKILL (or platform kill on non-unix).
    let _ = child.kill();
    let _ = child.wait();
}

// ─── Helpers ─────────────────────────────────────────────────────────────────
