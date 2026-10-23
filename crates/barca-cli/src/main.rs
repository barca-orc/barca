//! Barca CLI — invisible asset orchestrator.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

mod args;
mod input;
use input::*;
mod bounded;
use bounded::inspection_limit;
mod client;
mod commands;
use args::*;
use commands::*;
#[cfg(test)]
mod contract;
mod docs;
mod error;
mod output;

use error::{CliError, CliErrorExt, Context, ErrorKind};

use clap::Parser;
use std::path::PathBuf;

/// Finish a successful command, reporting non-pipe stdout write failures as infrastructure errors.
fn finish(json: bool) -> ! {
    if barca_core::term::stdout_write_failed() {
        CliError::from_barca(
            barca_core::BarcaError::Other(
                "could not write the result to stdout\nCheck the file or device stdout is redirected to (a full disk, a revoked terminal), then run the command again.".to_string(),
            ),
            &Context::default(),
        ).emit(json)
    }
    barca_core::term::exit(0)
}

fn main() {
    // Support `barca file.py [--flags]` as shorthand for `barca get file.py [--flags]`.
    // `--server` followed by a target or a flag is bare `--server` (the configured server).
    let args: Vec<String> = client::bare_server_flags(std::env::args().collect());
    if let Some(err) = client::refuse_on_local_commands(&args) {
        err.emit(error::json_mode_from_argv(&args))
    }
    let parsed = Cli::try_parse_from(&args).or_else(|first| {
        if args.len() > 1 && !args[1].starts_with('-') && args[1].ends_with(".py") {
            // Insert "get" after the program name so clap handles all flags.
            let mut rewritten = vec![args[0].clone(), "get".to_string()];
            rewritten.extend_from_slice(&args[1..]);
            Cli::try_parse_from(&rewritten).map_err(|e| (e, rewritten))
        } else {
            Err((first, args.clone()))
        }
    });
    let mut cli = match parsed {
        Ok(cli) => cli,
        // `--help` / `--version` are not errors: clap prints them to stdout and exits 0.
        Err((e, _)) if !e.use_stderr() => e.exit(),
        Err((e, argv)) => CliError::from_clap(&e, &argv).emit(error::json_mode_from_argv(&args)),
    };
    let json = json_output(&cli);

    // Version needs no runtime — answer before paying for thread spawns.
    if let Cli::Version = cli {
        barca_core::outln!("barca {}", env!("CARGO_PKG_VERSION"));
        finish(json);
    }

    // The manual is compiled in: no runtime, no Python, no project files needed.
    if let Cli::Docs {
        topic,
        all,
        json,
        fields,
        ..
    } = &cli
    {
        match docs::run(topic.as_deref(), *all, *json || fields.is_some())
            .map(|out| project_docs_json(out, fields.as_deref()))
        {
            Ok(out) => barca_core::term::stdout_str(&out),
            Err(msg) => CliError::from_prose(ErrorKind::Usage, msg).emit(*json || fields.is_some()),
        }
        finish(*json || fields.is_some());
    }

    // `BARCA_LOG=json`: structured lines on stdout, for `barca serve` only (#434).
    let barca_log = std::env::var(barca_core::jsonlog::ENV_VAR).ok();
    match (
        barca_core::jsonlog::requested(barca_log.as_deref()),
        matches!(cli, Cli::Serve { .. }),
    ) {
        (Ok(false), _) => {}
        (Ok(true), true) => barca_core::jsonlog::enable(barca_core::telemetry::is_on("datadog")),
        (Err(value), true) => CliError::from_prose(
            ErrorKind::Usage,
            format!("BARCA_LOG={value} is not a log format: use json, or text (the default)"),
        )
        .emit(json),
        (_, false) => {
            barca_core::warnings::warn_once(
                "BARCA_LOG applies to barca serve only; this command writes its usual lines",
            );
        }
    }

    // Hints in errors name files as the user typed them, so take them before rebasing.
    let ctx = context(&cli);

    // `--server`: the command runs on a barca server and prints here (client.rs). Nothing
    // below (the project root, discovery, Python) is needed on this machine.
    let server = server_flags(&cli).map_or(Ok(None), client::choose);
    // `barca sql --server`: the server resolves the views and the query runs here, in the
    // project (below).
    let mut sql_server = None;
    match server {
        Err(e) => e.emit(json),
        Ok(Some(server)) if matches!(cli, Cli::Sql { .. }) => sql_server = Some(server),
        Ok(Some(server)) => {
            let rt = runtime(json);
            if let Err(e) = rt.block_on(run_on_server(cli, &server, &ctx)) {
                e.emit(json);
            }
            drop(rt);
            finish(json);
        }
        Ok(None) => {}
    }
    // Files typed after a `barca sql` query; without any, a server resolves every deployed file
    // (not the files discovery finds here).
    let sql_files_given = matches!(&cli, Cli::Sql { files, .. } if !files.is_empty());
    // Run from the project root (the nearest barca.toml at or above the cwd), so `.barca/`,
    // node ids and relative paths inside steps are the same wherever barca is invoked.
    if let Err(e) = enter_project_root(&mut cli) {
        CliError::from_barca(e, &ctx).emit(json)
    }

    // A configured server without `--server` (and without `--local`): `get` and `run` run here
    // against its ledger; `status`, `history` and `stats` answer from it (RFC-0007 §4.1).
    let configured = match server_flags(&cli).map_or(Ok(None), client::configured) {
        Ok(configured) => configured,
        Err(e) => e.emit(json),
    };
    let mut ledger = None;
    match (configured, &cli) {
        (Some(server), Cli::Get { .. } | Cli::Run { .. }) => ledger = Some(server.into_ledger()),
        (Some(server), Cli::Sql { .. }) => {
            server.announce();
            sql_server = Some(server);
        }
        (Some(server), Cli::Status { .. } | Cli::Stats { .. } | Cli::Logs { .. })
        | (
            Some(server),
            Cli::History {
                reconcile: None, ..
            },
        ) => {
            server.announce();
            let rt = runtime(json);
            if let Err(e) = rt.block_on(run_on_server(cli, &server, &ctx)) {
                e.emit(json);
            }
            drop(rt);
            finish(json);
        }
        _ => {}
    }

    if let Some(server) = sql_server {
        let rt = runtime(json);
        if let Err(e) = rt.block_on(sql_on_server(cli, &server, sql_files_given)) {
            e.emit(json);
        }
        drop(rt);
        finish(json);
    }

    let rt = runtime(json);
    if let Err(e) = rt.block_on(run_cli(cli, &ctx, ledger)) {
        e.emit(json);
    }
    drop(rt);
    finish(json);
}

/// The one runtime for the whole process — barca-core is async-native and runs on whatever
/// runtime the caller provides.
fn runtime(json: bool) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            CliError::from_barca(
                barca_core::BarcaError::Other(format!("failed to create runtime: {e}")),
                &Context::default(),
            )
            .emit(json)
        })
}

/// The `--server` / `--local` flags of the commands that take them.
fn server_flags(cli: &Cli) -> Option<&ServerFlags> {
    match cli {
        Cli::Get { server, .. }
        | Cli::Run { server, .. }
        | Cli::Status { server, .. }
        | Cli::List { server, .. }
        | Cli::History { server, .. }
        | Cli::Stats { server, .. }
        | Cli::Logs { server, .. }
        | Cli::Sql { server, .. } => Some(server),
        _ => None,
    }
}

/// File arguments of a `--server` command: `.py` files and directories, checked as a local
/// command checks them (a directory selects the deployed files under it).
#[allow(clippy::result_large_err)] // cold path: one CliError per process, right before exiting
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

/// Run a `--server` command: the same argument checks as a local run, then the server.
#[allow(clippy::result_large_err)] // cold path: one CliError per process, right before exiting
async fn run_on_server(cli: Cli, server: &client::Server, ctx: &Context) -> Result<(), CliError> {
    match cli {
        Cli::Get {
            args,
            output,
            expires,
            format,
            partition,
            refresh,
            no_cascade,
            refresh_all,
            dry_run,
            agent,
            fields,
            env,
            ..
        } => {
            check_order("get", &args)?;
            let mode = get_mode(output, expires.as_deref(), format, fields.as_deref())?;
            // A served `get` returns the server's artifact pointers (its paths), or with
            // `-o value` the values the server read. Signed links are not in the run contract
            // yet.
            if mode.url_expires.is_some() {
                return Err(CliError::from_prose(
                    ErrorKind::Usage,
                    "error: -o url cannot be used with --server yet\nRun the command on this \
                     machine with --local, or leave out -o url.",
                ));
            }
            // Checked here as locally; the server parses it again.
            if let Some(raw) = partition.as_deref() {
                barca_core::selection::PartitionSelection::parse(raw).map_err(|msg| {
                    CliError::from_prose(ErrorKind::Usage, format!("error: {msg}"))
                })?;
            }
            let format = match mode.shape {
                barca_core::report::GetShape::Json => barca_core::report::ResultFormat::Json,
                barca_core::report::GetShape::Pretty => barca_core::report::ResultFormat::Pretty,
                barca_core::report::GetShape::Bare => barca_core::report::ResultFormat::Value,
            };
            let (target, files) = split_target_files(args);
            server_files("get", &files, refresh.as_deref())?;
            let targets = targets_arg(target.as_deref())?;
            client::run(
                server,
                client::RunArgs {
                    command: barca_core::outcome::RunCommand::Get,
                    targets,
                    files,
                    refresh,
                    no_cascade,
                    refresh_all,
                    dry_run,
                    agent,
                    mode: format,
                    values: mode.values,
                    partition,
                    fields,
                    env,
                },
                ctx,
            )
            .await
        }
        Cli::Run {
            args,
            output,
            format,
            refresh,
            no_cascade,
            refresh_all,
            partition,
            dry_run,
            agent,
            fields,
            env,
            ..
        } => {
            check_order("run", &args)?;
            let mode = get_run_mode(output, format, fields.as_deref())?;
            // Checked here as locally; the server parses it again.
            if let Some(raw) = partition.as_deref() {
                barca_core::selection::PartitionSelection::parse(raw).map_err(|msg| {
                    CliError::from_prose(ErrorKind::Usage, format!("error: {msg}"))
                })?;
            }
            let (target, files) = split_target_files(args);
            server_files("run", &files, refresh.as_deref())?;
            if target.is_none() {
                return Err(missing_target_error(
                    &format!("error: a target task is required\n\n{RUN_USAGE}"),
                    &files,
                ));
            }
            let targets = targets_arg(target.as_deref())?;
            client::run(
                server,
                client::RunArgs {
                    command: barca_core::outcome::RunCommand::Run,
                    targets,
                    files,
                    refresh,
                    no_cascade,
                    refresh_all,
                    dry_run,
                    agent,
                    mode: mode.into(),
                    values: false,
                    partition,
                    fields,
                    env,
                },
                ctx,
            )
            .await
        }
        Cli::Status {
            args,
            format,
            limit,
            all,
            fields,
            sample,
            env,
            ..
        } => {
            let json = fields_json(format, fields.as_deref())?;
            check_order("status", &args)?;
            let (target, files) = split_target_files(args);
            server_files("status", &files, None)?;
            let targets = targets_arg(target.as_deref())?;
            let limit = inspection_limit(limit, all, json, bounded::LIST_DEFAULT_LIMIT);
            let opts = StatusOpts {
                json,
                limit,
                fields: fields.as_deref(),
                sample: sample.unwrap_or(0),
            };
            client::status(server, &targets, &files, opts, env.as_deref()).await
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
            server_files("list", &files, None)?;
            let limit = inspection_limit(limit, all, json, bounded::LIST_DEFAULT_LIMIT);
            client::list(server, &files, groups, json, limit, fields.as_deref()).await
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
        } => {
            client::logs(
                server,
                run_id.as_deref(),
                env.as_deref(),
                crate::commands::logs::LogsArgs {
                    step: step.as_deref(),
                    stream: stream.map(Into::into),
                    follow,
                    timestamps,
                    json: is_json(format),
                },
            )
            .await
        }
        Cli::History {
            run_id,
            reconcile,
            status,
            target,
            triggered_by,
            since,
            limit,
            all,
            format,
            fields,
            env,
            server: flags,
        } => {
            if reconcile.is_some() {
                return Err(CliError::from_prose(
                    ErrorKind::Usage,
                    "error: --reconcile records a run on this machine's history; it does not \
                     take --server\nRun it on the server's machine, without --server.",
                ));
            }
            if let Some(run_id) = run_id {
                let select = same_history(&flags, env.as_deref());
                return client::run_detail(
                    server,
                    &run_id,
                    is_json(format),
                    &select,
                    env.as_deref(),
                )
                .await;
            }
            let filter = history_filter(
                status.as_deref(),
                target.as_deref(),
                triggered_by.as_deref(),
                since.as_deref(),
            )?;
            let json = fields_json(format, fields.as_deref())?;
            let limit = inspection_limit(limit, all, json, 10);
            client::history(
                server,
                limit,
                &filter,
                json,
                fields.as_deref(),
                env.as_deref(),
            )
            .await
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
            server_files("stats", &files, None)?;
            client::stats(
                server,
                &target,
                &files,
                json,
                fields.as_deref(),
                env.as_deref(),
            )
            .await
        }
        _ => unreachable!("only the commands that take --server reach here"),
    }
}

/// `barca sql` with a server (configured, or `--server`): run in the project root, so the
/// query, the store and `.barca/sql-cache/` are this machine's.
#[allow(clippy::result_large_err)] // cold path: one CliError per process, right before exiting
async fn sql_on_server(
    cli: Cli,
    server: &client::Server,
    files_given: bool,
) -> Result<(), CliError> {
    let Cli::Sql {
        query,
        mut files,
        format,
        limit,
        all,
        env,
        ..
    } = cli
    else {
        unreachable!("only barca sql reaches here")
    };
    if !files_given {
        files.clear();
    }
    server_files("sql", &files, None)?;
    let limit = (!all).then_some(limit);
    client::sql(
        server,
        &query,
        &files,
        limit,
        is_json(format),
        env.as_deref(),
    )
    .await
}

#[allow(clippy::result_large_err)] // cold path: one CliError per process, right before exiting
async fn run_cli(
    cli: Cli,
    ctx: &Context,
    ledger: Option<barca_core::ledger::Ledger>,
) -> Result<(), CliError> {
    let python = barca_core::commands::find_python();
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, ctx);

    match cli {
        Cli::Get {
            args,
            output,
            expires,
            format,
            partition,
            refresh,
            no_cascade,
            refresh_all,
            dry_run,
            agent,
            fields,
            env,
            ..
        } => {
            check_order("get", &args)?;
            let output = get_mode(output, expires.as_deref(), format, fields.as_deref())?;
            let partition = parse_partition(partition.as_deref())?;
            let (target, files) = split_target_files(args);
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
            let hint_files: Vec<PathBuf> = ctx.files.iter().map(PathBuf::from).collect();
            let targets = targets_arg(target.as_deref())?;
            let policy = cache_policy(refresh, no_cascade, refresh_all);
            get_cmd(
                env.as_deref(),
                targets,
                files,
                &python,
                output,
                policy,
                partition,
                dry_run,
                agent,
                fields.as_deref(),
                ledger,
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Run {
            args,
            refresh,
            no_cascade,
            refresh_all,
            partition,
            dry_run,
            output,
            format,
            agent,
            fields,
            env,
            ..
        } => {
            check_order("run", &args)?;
            let output = get_run_mode(output, format, fields.as_deref())?;
            let partition = parse_partition(partition.as_deref())?;
            let (target, files) = split_target_files(args);
            check_py_files("run", &files, refresh.as_deref())?;
            let Some(target) = target else {
                return Err(missing_target_error(
                    &format!("error: a target task is required\n\n{RUN_USAGE}"),
                    &files,
                ));
            };
            if files.is_empty() {
                return Err(usage_error(&format!(
                    "error: no .py files provided\n\n{RUN_USAGE}"
                )));
            }
            let hint_files: Vec<PathBuf> = ctx.files.iter().map(PathBuf::from).collect();
            let policy = cache_policy(refresh, no_cascade, refresh_all);
            let targets = targets_arg(Some(&target))?;
            run_cmd(
                env.as_deref(),
                targets,
                files,
                &python,
                policy,
                dry_run,
                output,
                agent,
                fields.as_deref(),
                ledger,
                partition,
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Plan { files, partition } => {
            let partition = parse_partition(partition.as_deref())?;
            plan_cmd(files, &python, partition).await.map_err(engine)
        }
        Cli::State { command } => match command {
            StateCommand::Backups { format, env } => {
                state_backups_cmd(env.as_deref(), &python, is_json(format))
                    .await
                    .map_err(engine)
            }
            StateCommand::Restore { from, format, env } => {
                state_restore_cmd(env.as_deref(), from.as_deref(), &python, is_json(format))
                    .await
                    .map_err(engine)
            }
            StateCommand::Reset { yes, format, env } => {
                state_reset_cmd(env.as_deref(), yes, is_json(format))
                    .await
                    .map_err(engine)
            }
        },
        Cli::Doctor { files, format, env } => {
            doctor_cmd(files, env.as_deref(), &python, is_json(format)).await
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
        } => logs_cmd(
            env.as_deref(),
            run_id.as_deref(),
            crate::commands::logs::LogsArgs {
                step: step.as_deref(),
                stream: stream.map(Into::into),
                follow,
                timestamps,
                json: is_json(format),
            },
        )
        .await
        .map_err(engine),
        Cli::History {
            run_id,
            reconcile,
            status,
            target,
            triggered_by,
            since,
            limit,
            all,
            format,
            fields,
            env,
            server,
        } => {
            if let Some(run_id) = run_id {
                let select = same_history(&server, env.as_deref());
                return run_detail_cmd(env.as_deref(), &run_id, is_json(format), &select)
                    .await
                    .map_err(engine);
            }
            let filter = history_filter(
                status.as_deref(),
                target.as_deref(),
                triggered_by.as_deref(),
                since.as_deref(),
            )?;
            let json = fields_json(format, fields.as_deref())?;
            let limit = inspection_limit(limit, all, json, 10);
            history_cmd(
                env.as_deref(),
                HistoryArgs {
                    limit,
                    json,
                    fields: fields.as_deref(),
                    reconcile: reconcile.as_deref(),
                    filter,
                },
            )
            .await
            .map_err(engine)
        }
        Cli::Gc {
            max_size,
            dry_run,
            format,
            env,
        } => {
            gc_cmd(
                env.as_deref(),
                max_size.as_deref(),
                dry_run,
                is_json(format),
            )
            .await
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
            stats_cmd(
                env.as_deref(),
                target,
                files,
                json,
                fields.as_deref(),
                &python,
            )
            .await
            .map_err(engine)
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
            let limit = inspection_limit(limit, all, json, bounded::LIST_DEFAULT_LIMIT);
            list_cmd(files, groups, json, limit, fields.as_deref(), &python)
                .await
                .map_err(engine)
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
            let limit = (!all).then_some(limit);
            sql_cmd(
                env.as_deref(),
                &query,
                files,
                limit,
                is_json(format),
                &python,
            )
            .await
            .map_err(engine)
        }
        Cli::Status {
            args,
            format,
            limit,
            all,
            fields,
            sample,
            env,
            ..
        } => {
            let json = fields_json(format, fields.as_deref())?;
            check_order("status", &args)?;
            let (target, files) = split_target_files(args);
            check_py_files("status", &files, None)?;
            if files.is_empty() {
                return Err(usage_error(&format!(
                    "error: no .py files provided\n\n{STATUS_USAGE}"
                )));
            }
            let hint_files: Vec<PathBuf> = ctx.files.iter().map(PathBuf::from).collect();
            let targets = targets_arg(target.as_deref())?;
            let limit = inspection_limit(limit, all, json, bounded::LIST_DEFAULT_LIMIT);
            status_cmd(
                env.as_deref(),
                targets,
                files,
                StatusOpts {
                    json,
                    limit,
                    fields: fields.as_deref(),
                    sample: sample.unwrap_or(0),
                },
                &python,
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Serve {
            files,
            port,
            host,
            watch,
            no_schedule,
            timezone,
            read_only,
            drain_timeout,
            env,
        } => {
            serve_cmd(
                env.as_deref(),
                files,
                host,
                port,
                watch,
                !no_schedule,
                timezone,
                read_only,
                drain_timeout,
                &python,
                ctx,
            )
            .await
        }
        // Answered in main() before the runtime is built — never reaches here.
        Cli::Version => unreachable!("version is handled before runtime construction"),
        Cli::Docs { .. } => unreachable!("docs is handled before runtime construction"),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// `--partition`, parsed; a usage error naming the accepted forms when it does not parse.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
fn parse_partition(
    raw: Option<&str>,
) -> Result<Option<barca_core::selection::PartitionSelection>, CliError> {
    raw.map(barca_core::selection::PartitionSelection::parse)
        .transpose()
        .map_err(|msg| CliError::from_prose(ErrorKind::Usage, format!("error: {msg}")))
}
