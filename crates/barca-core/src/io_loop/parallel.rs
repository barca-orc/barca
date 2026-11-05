//! `parallel()`: resuming a frozen caller once its branches end, and the directory its
//! branches' results are handed back through.

use super::*;

impl WorkerPool {
    /// Remove what the branches of `item_id`'s parallel groups returned. Called when the step
    /// has ended (finished, failed, or its worker died): it has read every result it was
    /// going to read, lazily loaded frames included, since its own result is written by then.
    pub(super) fn drop_branch_results(&mut self, item_id: ItemId) {
        if let Some(dirs) = self.branch_dirs.remove(&item_id) {
            tokio::task::spawn_blocking(move || {
                for dir in dirs {
                    std::fs::remove_dir_all(dir).ok();
                }
            });
        }
    }

    /// Resume frozen workers whose parallel groups completed.
    pub(super) async fn resume_frozen(&mut self, coord: &mut Coordinator) {
        // Partition: completed groups get drained out
        let mut i = 0;
        while i < self.frozen.len() {
            if coord.is_group_complete(self.frozen[i].group_id) {
                let fw = self.frozen.swap_remove(i);

                // The worker started in this one's place is not stopped here. It may be in
                // the middle of a step, and it may itself be the parent of a group by now:
                // killing it lost that step for good (nothing ran it again, and the run never
                // ended). The pool is over strength by one until a worker has nothing leased
                // (`retire_surplus`).

                // SIGCONT the original worker
                #[cfg(unix)]
                unsafe {
                    libc::kill(fw.child.id() as i32, libc::SIGCONT);
                }

                // Tell the parent how each branch ended (`branch_result`).
                let group = coord.group(fw.group_id);
                let failed: HashMap<ItemId, &str> = coord.failed_items().into_iter().collect();
                let results: Vec<ParallelResult> = group
                    .items
                    .iter()
                    .map(|iid| {
                        let outcome = if coord.is_done(*iid) {
                            Ok(coord.outputs().get(iid))
                        } else {
                            Err(failed.get(iid).copied().unwrap_or("failed"))
                        };
                        branch_result(outcome, fw.artifact_results)
                    })
                    .collect();
                let response = CoordinatorMessage::ParallelResponse { results };
                let msg = serde_json::to_value(&response).unwrap_or_default();
                let _ = fw.cmd_tx.send(msg).await;

                // Re-insert using the original worker_id — the worker_io_task
                // was spawned with this ID, so StepCompleted events arrive keyed
                // by it.
                self.workers.insert(
                    fw.original_worker_id,
                    WorkerHandle {
                        child: fw.child,
                        cmd_tx: fw.cmd_tx,
                        _task: fw._task,
                        // Still executing the parent task, with the rest of its batch
                        // behind it.
                        leases: std::iter::once(fw.parent_item).chain(fw.rest).collect(),
                        call: false,
                        batch_open: fw.batch_open,
                        front_since: std::time::Instant::now(),
                    },
                );
                // Don't increment i — swap_remove moved the last element here
            } else {
                i += 1;
            }
        }
    }
}

/// The directory that holds branch results, under the project's `.barca`.
pub(super) fn branch_results_parent() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".barca")
        .join("branches")
}

/// Where one run's `parallel()` branches write what they return:
/// `.barca/branches/<run id>-<coordinator pid>/<group>/<branch>.<ext>`.
///
/// Branch results used to be written into the artifact directory under a name made of the
/// branch's file, its function and its number within the phase. Two runs of one pipeline at
/// the same time (two runs under one `barca serve`, two `barca` processes in one project)
/// wrote and read the same files, and a caller received results another run's branches had
/// written (#332). The run id keeps runs apart, the group number keeps a run's groups apart
/// (nested ones, and the same call made twice), and the branch number its branches. Nothing
/// else is ever written there, so the whole directory can be removed when the run ends.
///
/// The pid is for the sweep ([`sweep_branch_results`]): it tells whether the run that owns a
/// directory can still be alive.
pub(super) fn branch_results_root(run_id: &str) -> PathBuf {
    branch_results_parent().join(format!("{run_id}-{}", std::process::id()))
}

/// Remove the branch results of runs whose coordinator is gone (killed, out of memory): a run
/// that ends in any other way removes its own. Like the staging directories of dead workers,
/// they are found by the pid in their name.
pub(super) fn sweep_branch_results(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let owner = name
            .to_str()
            .and_then(|n| n.rsplit_once('-'))
            .and_then(|(_, pid)| pid.parse::<i64>().ok());
        // A name this code did not make is left alone.
        if owner.is_some_and(|pid| !crate::db::pid_alive(pid)) {
            std::fs::remove_dir_all(entry.path()).ok();
        }
    }
    // The directory itself stays, empty: another run may be creating its own inside it now.
}

/// What the step that called `parallel()` is told about one branch.
///
/// `outcome` is the branch's recorded artifact when it finished, or its error when it failed.
///
/// A branch that finished is answered with where its artifact is, and the calling worker
/// reads it with the code that reads any step's input: whatever a step can return, a branch
/// can return. (A small JSON result has no file: the branch's worker sent its text, which is
/// passed on as it is.) Nothing of a result is read or interpreted here. The coordinator used to read the file itself and pass the value inline, which
/// it could do for JSON only: any other value (a set, a date, a DataFrame: written as pickle
/// or parquet) reached the caller as `None`, with no error (#285), and a JSON value went
/// through this process's JSON types on the way (an integer beyond 64 bits arrived as a
/// float, a dict with its keys sorted, a NaN as `None`).
///
/// A worker that did not ask for artifact results (a barca Python package from before this
/// change, run by this binary) still gets JSON values inline. A value that cannot be passed
/// that way is an error for that worker, not `None`.
pub(super) fn branch_result(
    outcome: Result<Option<&serde_json::Value>, &str>,
    artifact_results: bool,
) -> ParallelResult {
    let artifact = match outcome {
        Ok(artifact) => artifact,
        Err(error) => {
            return ParallelResult::Error {
                error: error.to_string(),
            };
        }
    };
    let field = |name: &str| {
        artifact
            .and_then(|a| a.get(name))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
    };
    if !artifact_results || artifact.is_some_and(|a| a.get("json").is_some()) {
        return ParallelResult::Error {
            error:
                "branch results require artifact file references in the worker protocol; reinstall barca"
                    .into(),
        };
    }
    let (Some(format), Some(path)) = (field("format"), field("path")) else {
        return ParallelResult::Error {
            error: "the branch finished but did not report its result file".into(),
        };
    };
    ParallelResult::Ok {
        artifact: crate::protocol::BranchArtifact {
            path: path.to_string(),
            format: format.to_string(),
            frame_type: field("frame_type").map(str::to_string),
        },
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────
