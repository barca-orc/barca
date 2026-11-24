//! A command as a request: what was asked, checked once.
//!
//! [`Request::of`] turns the parsed command line into a [`Request`]. It is the one place a
//! command's arguments are checked, and every usage error (exit 2) a command can end with
//! before it does anything comes from here, with the same text wherever the command then runs.
//! The two backends take the request as it is: `commands` runs it on this machine, `client`
//! sends it to a server. `dispatch` picks between them (the table in `route`).
//!
//! The checks are the same for both, with one difference, which [`Side`] carries: the files a
//! command names. On this machine they are `.py` files, already expanded by discovery, and
//! `get`, `run` and `status` need at least one. For a server they select among its deployed
//! files: a directory is allowed, and none means all of them. Two flags are refused for a
//! server: `get -o url` (signed links are not in the run contract yet) and
//! `history --reconcile` (it records a run in this machine's history).

use crate::args::{Cli, LogStreamArg, OutputMode, QueueAction, ServerFlags, StateCommand};
use crate::bounded::{self, inspection_limit, steps_limit};
use crate::error::{CliError, ErrorKind};
use crate::input::{
    GET_USAGE, GetMode, RUN_USAGE, STATUS_USAGE, check_order, check_py_files, fields_json,
    get_mode, get_run_mode, is_json, missing_target_error, split_target_files, targets_arg,
    usage_error,
};
use barca_service::LogStream;
use barca_service::history_filter::HistoryFilter;
use barca_service::pins::Choice;
use barca_service::selection::PartitionSelection;
use std::path::PathBuf;

/// Where the request is going, for the one check that differs: the files it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// This machine.
    Here,
    /// A server: named, configured, or this project's own.
    Server,
}

/// One command, checked.
pub(crate) enum Request {
    Get { run: RunSpec, mode: GetMode },
    Run { run: RunSpec, mode: OutputMode },
    Status(StatusRequest),
    List(ListRequest),
    Logs(LogsRequest),
    History(HistoryRequest),
    Stats(StatsRequest),
    Sql(SqlRequest),
    Queue(QueueRequest),
    Pause(PauseRequest),
    Resume(ResumeRequest),
    Cancel(CancelRequest),
    Pin(PinRequest),
    Unpin(UnpinRequest),
    Gc(GcRequest),
    Doctor(DoctorRequest),
    Plan(PlanRequest),
    State(StateRequest),
    Serve(ServeArgs),
}

/// What `get` and `run` share: what to compute and how.
pub(crate) struct RunSpec {
    pub(crate) targets: Vec<String>,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) refresh: Option<Vec<String>>,
    pub(crate) no_cascade: bool,
    pub(crate) refresh_all: bool,
    /// `--partition`, as typed (a server parses it again) and parsed.
    pub(crate) partition: Option<String>,
    pub(crate) selection: Option<PartitionSelection>,
    pub(crate) batch: barca_service::batch::BatchOverride,
    pub(crate) dry_run: bool,
    pub(crate) agent: bool,
    pub(crate) fields: Option<Vec<String>>,
    pub(crate) env: Option<String>,
    pub(crate) allow_paused: bool,
    /// `--queue`: where the run joins the server's run queue.
    pub(crate) queue: barca_service::outcome::QueuePlace,
}

/// `barca status`.
pub(crate) struct StatusRequest {
    pub(crate) targets: Vec<String>,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
    pub(crate) view: StatusView,
}

/// Which of `barca status`'s answers is asked for.
pub(crate) enum StatusView {
    /// `NODE --keys | --overview | --grid | --key KEY`: the keys of one partitioned node.
    Keys {
        target: String,
        view: KeysView,
        limit: Option<usize>,
    },
    /// `--rollup`; `now` is `--now` (a server computes it again instead of serving its last).
    Rollup { now: bool },
    /// The table of nodes.
    Nodes {
        limit: Option<usize>,
        fields: Option<Vec<String>>,
        sample: usize,
        filter: barca_service::status::StatusFilter,
    },
}

/// `barca list`.
pub(crate) struct ListRequest {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) groups: bool,
    pub(crate) json: bool,
    pub(crate) limit: Option<usize>,
    pub(crate) fields: Option<Vec<String>>,
}

/// `barca logs`.
pub(crate) struct LogsRequest {
    pub(crate) run_id: Option<String>,
    pub(crate) step: Option<String>,
    pub(crate) stream: Option<LogStream>,
    pub(crate) follow: bool,
    pub(crate) timestamps: bool,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

impl LogsRequest {
    /// What to print of the run's lines.
    pub(crate) fn args(&self) -> LogsArgs<'_> {
        LogsArgs {
            step: self.step.as_deref(),
            stream: self.stream,
            follow: self.follow,
            timestamps: self.timestamps,
            json: self.json,
        }
    }
}

/// `barca history`.
pub(crate) struct HistoryRequest {
    pub(crate) env: Option<String>,
    pub(crate) json: bool,
    pub(crate) view: HistoryView,
}

/// Which of `barca history`'s answers is asked for.
pub(crate) enum HistoryView {
    /// `RUN_ID`: one run and its steps. `select` is what a hint's command adds to read the
    /// same history ([`same_history`]).
    Run {
        run_id: String,
        limit: Option<usize>,
        select: String,
    },
    /// `--activity`.
    Activity { filter: HistoryFilter },
    /// The table of runs; `reconcile` is `--reconcile RUN_ID` (this machine only).
    Runs {
        limit: Option<usize>,
        filter: HistoryFilter,
        fields: Option<Vec<String>>,
        reconcile: Option<String>,
    },
}

/// `barca stats`.
pub(crate) struct StatsRequest {
    pub(crate) target: String,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) json: bool,
    pub(crate) fields: Option<Vec<String>>,
    pub(crate) env: Option<String>,
}

/// `barca sql`.
pub(crate) struct SqlRequest {
    pub(crate) query: String,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) limit: Option<usize>,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca queue [pause|resume]`.
pub(crate) struct QueueRequest {
    pub(crate) action: Option<QueueAction>,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca pause`.
pub(crate) struct PauseRequest {
    pub(crate) target: Option<String>,
    pub(crate) downstream: bool,
    /// `--dry-run`: pause nothing, answer what the pause would cover.
    pub(crate) dry_run: bool,
    pub(crate) schedule: Option<String>,
    pub(crate) until: Option<String>,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca resume`.
pub(crate) struct ResumeRequest {
    pub(crate) target: Option<String>,
    pub(crate) downstream: bool,
    pub(crate) schedule: Option<String>,
    pub(crate) all: bool,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca cancel`.
pub(crate) struct CancelRequest {
    pub(crate) run_id: String,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca pin`.
pub(crate) struct PinRequest {
    pub(crate) action: PinAction,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca unpin`.
pub(crate) struct UnpinRequest {
    pub(crate) target: String,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca gc`. `max_size` is as typed: both backends parse it where they use it.
pub(crate) struct GcRequest {
    pub(crate) max_size: Option<String>,
    pub(crate) dry_run: bool,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
}

/// `barca doctor`. It always runs here; `server` says which server it checks too.
pub(crate) struct DoctorRequest {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
    pub(crate) server: ServerFlags,
}

/// `barca plan`.
pub(crate) struct PlanRequest {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) partition: Option<PartitionSelection>,
}

/// `barca state backups | restore | reset`.
pub(crate) struct StateRequest {
    pub(crate) json: bool,
    pub(crate) env: Option<String>,
    pub(crate) action: StateAction,
}

pub(crate) enum StateAction {
    Backups,
    Restore { from: Option<String> },
    Reset { yes: bool },
}

/// `barca serve`'s flags, as typed.
pub(crate) struct ServeArgs {
    pub(crate) env: Option<String>,
    pub(crate) auth: Option<barca_service::serve::AuthMode>,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) host: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) watch: bool,
    /// `!--no-schedule`.
    pub(crate) schedule: bool,
    pub(crate) timezone: String,
    pub(crate) read_only: bool,
    pub(crate) drain_timeout: Option<u64>,
    pub(crate) max_runs: Option<usize>,
    pub(crate) capacity: Option<usize>,
    pub(crate) run_timeout: Option<u64>,
}

impl Request {
    /// The request `cli` makes, checked for `side`. `Err` is a usage error (exit 2).
    pub(crate) fn of(cli: Cli, side: Side) -> Result<Request, CliError> {
        Ok(match cli {
            Cli::Get {
                cached_only,
                args,
                output,
                expires,
                format,
                partition,
                batch,
                refresh,
                no_cascade,
                refresh_all,
                dry_run,
                agent,
                fields,
                env,
                allow_paused,
                queue,
                ..
            } => {
                check_order("get", &args)?;
                let queue = queue_place("get", queue, side)?;
                let mut mode = get_mode(output, expires.as_deref(), format, fields.as_deref())?;
                mode.cached_only = cached_only.as_deref().map(|s| {
                    if s == "local" {
                        barca_service::cached::Tier::Local
                    } else {
                        barca_service::cached::Tier::Store
                    }
                });
                if mode.cached_only == Some(barca_service::cached::Tier::Local)
                    && mode.url_expires.is_some()
                {
                    return Err(usage_error("--cached-only=local cannot sign URLs"));
                }
                // A served `get` returns the server's artifact pointers (its paths), or with
                // `-o value` the values the server read. Signed links are not in the run
                // contract yet.
                if side == Side::Server && mode.url_expires.is_some() && mode.cached_only.is_none()
                {
                    return Err(CliError::from_prose(
                        ErrorKind::Usage,
                        "error: -o url cannot be used on a server yet\nRun the command on this \
                         machine with --local, or leave out -o url.",
                    ));
                }
                let selection = parse_partition(partition.as_deref())?;
                let (target, files) = split_target_files(args);
                match side {
                    Side::Server => server_files("get", &files, refresh.as_deref())?,
                    Side::Here => {
                        check_py_files("get", &files, refresh.as_deref())?;
                        if files.is_empty() {
                            let what = if target.is_none() {
                                "files"
                            } else {
                                ".py files"
                            };
                            return Err(usage_error(&format!(
                                "error: no {what} provided\n\n{GET_USAGE}"
                            )));
                        }
                    }
                }
                let targets = targets_arg(target.as_deref())?;
                let batch = parse_batch(&batch)?;
                Request::Get {
                    run: RunSpec {
                        targets,
                        files,
                        refresh,
                        no_cascade,
                        refresh_all,
                        partition,
                        selection,
                        batch,
                        dry_run,
                        agent,
                        fields,
                        env,
                        allow_paused,
                        queue,
                    },
                    mode,
                }
            }
            Cli::Run {
                args,
                refresh,
                no_cascade,
                refresh_all,
                partition,
                batch,
                dry_run,
                output,
                format,
                agent,
                fields,
                env,
                allow_paused,
                queue,
                ..
            } => {
                check_order("run", &args)?;
                let queue = queue_place("run", queue, side)?;
                let mode = get_run_mode(output, format, fields.as_deref())?;
                let selection = parse_partition(partition.as_deref())?;
                let (target, files) = split_target_files(args);
                match side {
                    Side::Server => server_files("run", &files, refresh.as_deref())?,
                    Side::Here => check_py_files("run", &files, refresh.as_deref())?,
                }
                let Some(target) = target else {
                    return Err(missing_target_error(
                        &format!("error: a target task is required\n\n{RUN_USAGE}"),
                        &files,
                    ));
                };
                if side == Side::Here && files.is_empty() {
                    return Err(usage_error(&format!(
                        "error: no .py files provided\n\n{RUN_USAGE}"
                    )));
                }
                let targets = targets_arg(Some(&target))?;
                let batch = parse_batch(&batch)?;
                Request::Run {
                    run: RunSpec {
                        targets,
                        files,
                        refresh,
                        no_cascade,
                        refresh_all,
                        partition,
                        selection,
                        batch,
                        dry_run,
                        agent,
                        fields,
                        env,
                        allow_paused,
                        queue,
                    },
                    mode,
                }
            }
            Cli::Status {
                args,
                format,
                limit,
                all,
                fields,
                sample,
                state,
                cause,
                health,
                root,
                problems,
                rollup,
                now,
                keys,
                needs_run,
                value,
                range,
                error,
                text,
                sort,
                offset,
                overview,
                grid,
                key,
                env,
                ..
            } => {
                let json = fields_json(format, fields.as_deref())?;
                check_order("status", &args)?;
                let view = KeysView::of(KeysFlags {
                    keys,
                    overview,
                    grid,
                    key,
                    states: state.clone(),
                    needs_run,
                    values: value,
                    range,
                    error,
                    text,
                    sort,
                    offset,
                    sample,
                })
                .map_err(|e| usage_error(&e))?;
                let (target, files) = split_target_files(args);
                match side {
                    Side::Server => server_files("status", &files, None)?,
                    Side::Here => {
                        check_py_files("status", &files, None)?;
                        if files.is_empty() {
                            return Err(usage_error(&format!(
                                "error: no .py files provided\n\n{STATUS_USAGE}"
                            )));
                        }
                    }
                }
                let targets = targets_arg(target.as_deref())?;
                let view = if let Some(view) = view {
                    let target = view.target(&targets).map_err(|e| usage_error(&e))?;
                    let limit = inspection_limit(limit, all, json, bounded::LIST_DEFAULT_LIMIT);
                    StatusView::Keys {
                        target,
                        view,
                        limit,
                    }
                } else {
                    let filter = status_filter(&state, problems, cause, health, root)?;
                    if rollup {
                        StatusView::Rollup { now }
                    } else {
                        StatusView::Nodes {
                            limit: inspection_limit(limit, all, json, bounded::LIST_DEFAULT_LIMIT),
                            fields,
                            sample: sample.unwrap_or(0),
                            filter,
                        }
                    }
                };
                Request::Status(StatusRequest {
                    targets,
                    files,
                    json,
                    env,
                    view,
                })
            }
            Cli::List {
                groups,
                files,
                format,
                limit,
                all,
                fields,
                ..
            } => {
                let json = fields_json(format, fields.as_deref())?;
                if side == Side::Server {
                    server_files("list", &files, None)?;
                }
                Request::List(ListRequest {
                    limit: inspection_limit(limit, all, json, bounded::LIST_DEFAULT_LIMIT),
                    files,
                    groups,
                    json,
                    fields,
                })
            }
            Cli::Logs {
                run_id,
                step,
                stream,
                follow,
                timestamps,
                format,
                env,
                ..
            } => Request::Logs(LogsRequest {
                run_id,
                step,
                stream: stream.map(LogStreamArg::into),
                follow,
                timestamps,
                json: is_json(format),
                env,
            }),
            Cli::History {
                run_id,
                reconcile,
                status,
                target,
                triggered_by,
                not_triggered_by,
                search,
                activity,
                since,
                until,
                limit,
                all,
                format,
                fields,
                env,
                server,
            } => {
                if side == Side::Server && reconcile.is_some() {
                    return Err(CliError::from_prose(
                        ErrorKind::Usage,
                        "error: --reconcile records a run on this machine's history; it does not \
                         take --server\nRun it on the server's machine, without --server.",
                    ));
                }
                if let Some(run_id) = run_id {
                    let select = same_history(&server, env.as_deref());
                    return Ok(Request::History(HistoryRequest {
                        json: is_json(format),
                        env,
                        view: HistoryView::Run {
                            run_id,
                            limit: steps_limit(limit, all),
                            select,
                        },
                    }));
                }
                let filter = history_filter(barca_service::history_filter::HistoryFlags {
                    status: status.as_deref(),
                    target: target.as_deref(),
                    triggered_by: triggered_by.as_deref(),
                    not_triggered_by: not_triggered_by.as_deref(),
                    since: since.as_deref(),
                    until: until.as_deref(),
                    search: search.as_deref(),
                })?;
                if activity {
                    return Ok(Request::History(HistoryRequest {
                        json: is_json(format),
                        env,
                        view: HistoryView::Activity { filter },
                    }));
                }
                let json = fields_json(format, fields.as_deref())?;
                Request::History(HistoryRequest {
                    json,
                    env,
                    view: HistoryView::Runs {
                        limit: inspection_limit(limit, all, json, 10),
                        filter,
                        fields,
                        reconcile,
                    },
                })
            }
            Cli::Stats {
                target,
                files,
                format,
                fields,
                env,
                ..
            } => {
                let json = fields_json(format, fields.as_deref())?;
                if side == Side::Server {
                    server_files("stats", &files, None)?;
                }
                Request::Stats(StatsRequest {
                    target,
                    files,
                    json,
                    fields,
                    env,
                })
            }
            Cli::Sql {
                query,
                files,
                format,
                limit,
                all,
                env,
                ..
            } => {
                if side == Side::Server {
                    server_files("sql", &files, None)?;
                }
                Request::Sql(SqlRequest {
                    query,
                    files,
                    limit: (!all).then_some(limit),
                    json: is_json(format),
                    env,
                })
            }
            Cli::Queue {
                action,
                format,
                env,
                ..
            } => Request::Queue(QueueRequest {
                action,
                json: is_json(format),
                env,
            }),
            Cli::Pause {
                target,
                downstream,
                dry_run,
                schedule,
                until,
                format,
                env,
                ..
            } => Request::Pause(PauseRequest {
                target,
                downstream,
                dry_run,
                schedule,
                until,
                json: is_json(format),
                env,
            }),
            Cli::Resume {
                target,
                downstream,
                schedule,
                all,
                format,
                env,
                ..
            } => Request::Resume(ResumeRequest {
                target,
                downstream,
                schedule,
                all,
                json: is_json(format),
                env,
            }),
            Cli::Cancel {
                run_id,
                format,
                env,
                ..
            } => Request::Cancel(CancelRequest {
                run_id,
                json: is_json(format),
                env,
            }),
            Cli::Pin {
                target,
                files,
                run_hash,
                previous,
                from_file,
                versions,
                limit,
                reason,
                format,
                env,
                ..
            } => {
                if side == Side::Server {
                    server_files("pin", &files, None)?;
                }
                let action = pin_action(
                    target, run_hash, previous, from_file, versions, limit, reason,
                )?;
                Request::Pin(PinRequest {
                    action,
                    files,
                    json: is_json(format),
                    env,
                })
            }
            Cli::Unpin {
                target,
                files,
                format,
                env,
                ..
            } => {
                if side == Side::Server {
                    server_files("unpin", &files, None)?;
                }
                Request::Unpin(UnpinRequest {
                    target,
                    files,
                    json: is_json(format),
                    env,
                })
            }
            Cli::Gc {
                max_size,
                dry_run,
                format,
                env,
                ..
            } => Request::Gc(GcRequest {
                max_size,
                dry_run,
                json: is_json(format),
                env,
            }),
            Cli::Doctor {
                files,
                format,
                env,
                server,
                local,
            } => Request::Doctor(DoctorRequest {
                files,
                json: is_json(format),
                env,
                server: ServerFlags { server, local },
            }),
            Cli::Plan {
                files, partition, ..
            } => Request::Plan(PlanRequest {
                files,
                partition: parse_partition(partition.as_deref())?,
            }),
            Cli::State { command } => {
                let (format, env, action) = match command {
                    StateCommand::Backups { format, env } => (format, env, StateAction::Backups),
                    StateCommand::Restore { from, format, env } => {
                        (format, env, StateAction::Restore { from })
                    }
                    StateCommand::Reset { yes, format, env } => {
                        (format, env, StateAction::Reset { yes })
                    }
                };
                Request::State(StateRequest {
                    json: is_json(format),
                    env,
                    action,
                })
            }
            Cli::Serve {
                files,
                auth,
                port,
                host,
                watch,
                no_schedule,
                timezone,
                read_only,
                drain_timeout,
                max_runs,
                capacity,
                run_timeout,
                env,
            } => Request::Serve(ServeArgs {
                env,
                auth,
                files,
                host,
                port,
                watch,
                schedule: !no_schedule,
                timezone,
                read_only,
                drain_timeout,
                max_runs,
                capacity,
                run_timeout,
            }),
            // Answered in main() before anything is dispatched.
            Cli::Tokens { .. } => unreachable!("tokens is answered before dispatch"),
            Cli::Version => unreachable!("version is answered before dispatch"),
            Cli::Docs { .. } => unreachable!("docs is answered before dispatch"),
        })
    }
}

// ─── the checks ─────────────────────────────────────────────────────────────

/// File arguments of a `--server` command: `.py` files and directories, checked as a local
/// command checks them (a directory selects the deployed files under it).
fn server_files(sub: &str, files: &[PathBuf], refresh: Option<&[String]>) -> Result<(), CliError> {
    let not_dirs: Vec<PathBuf> = files
        .iter()
        .filter(|f| {
            let text = f.to_string_lossy();
            !(text.ends_with('/') || text == "." || text == ".." || f.is_dir())
        })
        .cloned()
        .collect();
    check_py_files(sub, &not_dirs, refresh)
}

/// `--queue`: a place in a server's run queue. On this machine a run starts at once and there
/// is no queue to place it in, so the flag is a usage error there, whatever its value.
fn queue_place(
    sub: &str,
    queue: Option<crate::args::QueueArg>,
    side: Side,
) -> Result<barca_service::outcome::QueuePlace, CliError> {
    use barca_service::outcome::QueuePlace;
    match (queue, side) {
        (None, _) => Ok(QueuePlace::Next),
        (Some(_), Side::Here) => Err(CliError::from_prose(
            ErrorKind::Usage,
            format!(
                "error: --queue places a run in a barca server's run queue, and this `barca \
                 {sub}` runs on this machine, where a run starts at once\nRun it on a server \
                 (--server, or in the project a `barca serve` is running in, without --local), \
                 or leave out --queue (barca docs serve-ops)."
            ),
        )),
        (Some(crate::args::QueueArg::Next), Side::Server) => Ok(QueuePlace::Next),
        (Some(crate::args::QueueArg::Last), Side::Server) => Ok(QueuePlace::Last),
    }
}

/// `--batch`, parsed (RFC-0013); a usage error naming the accepted forms when it does not parse.
fn parse_batch(values: &[String]) -> Result<barca_service::batch::BatchOverride, CliError> {
    barca_service::batch::BatchOverride::parse(values)
        .map_err(|msg| CliError::from_prose(ErrorKind::Usage, format!("error: {msg}")))
}

/// `--partition`, parsed; a usage error naming the accepted forms when it does not parse.
fn parse_partition(
    raw: Option<&str>,
) -> Result<Option<barca_service::selection::PartitionSelection>, CliError> {
    raw.map(barca_service::selection::PartitionSelection::parse)
        .transpose()
        .map_err(|msg| CliError::from_prose(ErrorKind::Usage, format!("error: {msg}")))
}

// ─── status ──────────────────────────────────────────────────────────────────

pub(crate) struct StatusOpts<'a> {
    pub(crate) json: bool,
    pub(crate) limit: Option<usize>,
    pub(crate) fields: Option<&'a [String]>,
    pub(crate) sample: usize,
    /// `--state` / `--problems`.
    pub(crate) filter: barca_service::status::StatusFilter,
}

/// `barca status --state a,b --problems`: an unknown state is a usage error naming the valid
/// ones.
fn status_filter(
    states: &[String],
    problems: bool,
    cause: Option<String>,
    health: Option<String>,
    root: Option<String>,
) -> Result<barca_service::status::StatusFilter, CliError> {
    let states = barca_service::status::StatusFilter::parse_states(states)
        .map_err(|e| usage_error(&format!("error: --state: {e}")))?;
    Ok(barca_service::status::StatusFilter {
        states,
        cause,
        health,
        root,
        problems,
        matched: 0,
    })
}

/// `barca status NODE --keys`, `--overview`, `--grid` or `--key KEY`: which view of one
/// partitioned node's keys is printed.
pub(crate) enum KeysView {
    /// `--keys [--state a,b] [--needs-run] [--value D=V] [--range D=A..B] [--error N]
    /// [--match TEXT] [--sort ORDER] [--offset N]`.
    Keys(barca_service::key_query::KeyQuery),
    /// `--overview`.
    Overview,
    /// `--grid`.
    Grid,
    /// `--key KEY [--sample N]`.
    Key { key: String, sample: usize },
}

/// The flags of `barca status` that pick a keys view and narrow `--keys`.
pub(crate) struct KeysFlags {
    pub(crate) keys: bool,
    pub(crate) overview: bool,
    pub(crate) grid: bool,
    pub(crate) key: Option<String>,
    pub(crate) states: Vec<String>,
    pub(crate) needs_run: bool,
    pub(crate) values: Vec<String>,
    pub(crate) range: Option<String>,
    pub(crate) error: Option<usize>,
    pub(crate) text: Option<String>,
    pub(crate) sort: Option<String>,
    pub(crate) offset: Option<usize>,
    pub(crate) sample: Option<usize>,
}

impl KeysView {
    /// The view the flags ask for, if any. `--state` names key states with `--keys`; a state,
    /// an order, a value or a range barca does not read is a usage error saying what it does.
    pub(crate) fn of(flags: KeysFlags) -> Result<Option<KeysView>, String> {
        if let Some(key) = flags.key {
            let sample = flags
                .sample
                .unwrap_or(0)
                .min(barca_service::partition_grid::KEY_SAMPLE_MAX);
            return Ok(Some(KeysView::Key { key, sample }));
        }
        if flags.grid {
            return Ok(Some(KeysView::Grid));
        }
        if flags.overview {
            return Ok(Some(KeysView::Overview));
        }
        if flags.keys {
            let args = barca_service::key_query::KeyQueryArgs {
                states: &flags.states,
                needs_run: flags.needs_run,
                values: &flags.values,
                range: flags.range.as_deref(),
                error: flags.error,
                text: flags.text.as_deref(),
                sort: flags.sort.as_deref(),
                offset: flags.offset.unwrap_or(0),
                limit: None,
            };
            return barca_service::key_query::KeyQuery::parse(&args)
                .map(|query| Some(KeysView::Keys(query)))
                .map_err(|e| format!("error: --keys: {e}"));
        }
        Ok(None)
    }

    /// The one node a keys view is of: exactly one target, or a usage error.
    pub(crate) fn target(&self, targets: &[String]) -> Result<String, String> {
        match targets {
            [one] => Ok(one.clone()),
            _ => Err(format!(
                "error: {} shows the keys of one partitioned node: name exactly one target\n\
                 For example: barca status NODE {}",
                self.flag(),
                self.flag()
            )),
        }
    }

    fn flag(&self) -> &'static str {
        match self {
            KeysView::Keys(_) => "--keys",
            KeysView::Overview => "--overview",
            KeysView::Grid => "--grid",
            KeysView::Key { .. } => "--key KEY",
        }
    }
}

// ─── history ─────────────────────────────────────────────────────────────────

/// The filter flags of `barca history`, checked here (also before a server sees them).
pub(crate) fn history_filter(
    flags: barca_service::history_filter::HistoryFlags<'_>,
) -> Result<HistoryFilter, CliError> {
    HistoryFilter::parse(flags, chrono::Utc::now())
        .map_err(|msg| CliError::from_prose(ErrorKind::Usage, format!("error: {msg}")))
}

/// What a hint's command adds so it reads the same history as this one: `--server URL`,
/// `--server`, `--local` and `--env NAME`, as given.
pub(crate) fn same_history(server: &crate::args::ServerFlags, env: Option<&str>) -> String {
    let mut out = String::new();
    match server.server.as_deref() {
        Some("") => out.push_str(" --server"),
        Some(url) => out.push_str(&format!(" --server {url}")),
        None if server.local => out.push_str(" --local"),
        None => {}
    }
    if let Some(env) = env {
        out.push_str(&format!(" --env {env}"));
    }
    out
}

// ─── pin ─────────────────────────────────────────────────────────────────────

/// What one `barca pin` invocation asks for.
#[derive(Debug, Clone)]
pub(crate) enum PinAction {
    /// No target: every pin.
    List,
    Versions {
        target: String,
        limit: usize,
    },
    Pin {
        target: String,
        choice: Choice,
        reason: Option<String>,
    },
    /// Pin to data the operator supplies (`--from-file`).
    Upload {
        target: String,
        path: PathBuf,
        reason: Option<String>,
    },
}

/// The action the flags of `barca pin` name; a target without one is a usage error.
pub(crate) fn pin_action(
    target: Option<String>,
    run_hash: Option<String>,
    previous: bool,
    from_file: Option<PathBuf>,
    versions: bool,
    limit: Option<usize>,
    reason: Option<String>,
) -> Result<PinAction, CliError> {
    let usage = |text: &str| CliError::from_prose(ErrorKind::Usage, text.to_string());
    let Some(target) = target else {
        if run_hash.is_some() || previous || from_file.is_some() || versions || reason.is_some() {
            return Err(usage(
                "error: name the asset to pin: barca pin <TARGET> --previous\n\
                 Without a target, `barca pin` lists every pin.",
            ));
        }
        return Ok(PinAction::List);
    };
    if versions {
        return Ok(PinAction::Versions {
            target,
            limit: limit.unwrap_or(barca_service::pins::DEFAULT_VERSIONS),
        });
    }
    if let Some(path) = from_file {
        return Ok(PinAction::Upload {
            target,
            path,
            reason,
        });
    }
    let choice = match (run_hash, previous) {
        (Some(hash), _) => Choice::RunHash(hash),
        (None, true) => Choice::Previous,
        (None, false) => {
            return Err(usage(&format!(
                "error: say which materialization to pin: --run-hash <HASH>, --previous or \
                 --from-file <PATH>\n\
                 List them with: barca pin {target} --versions"
            )));
        }
    };
    Ok(PinAction::Pin {
        target,
        choice,
        reason,
    })
}

/// The file `--from-file` names, read whole (refused over the upload limit before reading).
pub(crate) fn read_supplied(
    path: &std::path::Path,
) -> Result<barca_service::pins::Supplied, CliError> {
    let usage = |text: String| CliError::from_prose(ErrorKind::Usage, text);
    let meta = std::fs::metadata(path).map_err(|e| {
        usage(format!(
            "error: cannot read --from-file {}: {e}",
            path.display()
        ))
    })?;
    if !meta.is_file() {
        return Err(usage(format!(
            "error: --from-file {} is not a file",
            path.display()
        )));
    }
    if meta.len() > barca_service::pins::MAX_UPLOAD_BYTES as u64 {
        return Err(usage(format!(
            "error: --from-file {} is {} bytes, more than the {} an upload may be",
            path.display(),
            meta.len(),
            barca_service::gc::size::human(barca_service::pins::MAX_UPLOAD_BYTES as u64)
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| {
        usage(format!(
            "error: cannot read --from-file {}: {e}",
            path.display()
        ))
    })?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    barca_service::pins::upload_format(&file_name, None)
        .map_err(|e| usage(format!("error: {e}")))?;
    Ok(barca_service::pins::Supplied {
        bytes,
        file_name,
        format: None,
    })
}

// ─── logs ────────────────────────────────────────────────────────────────────

/// What `barca logs` prints, wherever the lines come from.
#[derive(Clone, Copy)]
pub(crate) struct LogsArgs<'a> {
    pub(crate) step: Option<&'a str>,
    pub(crate) stream: Option<LogStream>,
    pub(crate) follow: bool,
    pub(crate) timestamps: bool,
    pub(crate) json: bool,
}

impl LogsArgs<'_> {
    /// Whether a line arriving while following is one to print. Nothing is checked against
    /// the run's steps: a step that has not started yet has no lines.
    pub(crate) fn selects(&self, node_id: &str, stream: LogStream) -> bool {
        self.stream.is_none_or(|s| s == stream)
            && self
                .step
                .is_none_or(|step| barca_service::queries::step_matches(node_id, step))
    }
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
