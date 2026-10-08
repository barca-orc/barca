//! Barca CLI — invisible asset orchestrator.

mod args;
mod input;
use input::*;
mod bounded;
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
use std::io::Write;
use std::path::PathBuf;

fn main() {
    // Support `barca file.py [--flags]` as shorthand for `barca get file.py [--flags]`.
    let args: Vec<String> = std::env::args().collect();
    let parsed = Cli::try_parse_from(&args).or_else(|first| {
        if args.len() > 1 && !args[1].starts_with('-') && args[1].ends_with(".py") {
            // Insert "get" after the program name so clap handles all flags.
            let mut rewritten = vec![args[0].clone(), "get".to_string()];
            rewritten.extend_from_slice(&args[1..]);
            Cli::try_parse_from(rewritten)
        } else {
            Err(first)
        }
    });
    let mut cli = match parsed {
        Ok(cli) => cli,
        // `--help` / `--version` are not errors: clap prints them to stdout and exits 0.
        Err(e) if !e.use_stderr() => e.exit(),
        Err(e) => CliError::from_clap(&e).emit(error::json_mode_from_argv(&args)),
    };
    let json = json_output(&cli);

    // Version needs no runtime — answer before paying for thread spawns.
    if let Cli::Version = cli {
        println!("barca {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // The manual is compiled in: no runtime, no Python, no project files needed.
    if let Cli::Docs {
        topic,
        all,
        json,
        fields,
    } = &cli
    {
        match docs::run(topic.as_deref(), *all, *json || fields.is_some())
            .map(|out| project_docs_json(out, fields.as_deref()))
        {
            // Ignore write errors (e.g. a closed pipe from `barca docs --all | head`).
            Ok(out) => {
                let _ = std::io::stdout().lock().write_all(out.as_bytes());
            }
            Err(msg) => CliError::from_prose(ErrorKind::Usage, msg).emit(*json || fields.is_some()),
        }
        return;
    }

    // Hints in errors name files as the user typed them, so take them before rebasing.
    let ctx = context(&cli);
    // Run from the project root (the nearest barca.toml at or above the cwd), so `.barca/`,
    // node ids and relative paths inside steps are the same wherever barca is invoked.
    if let Err(e) = enter_project_root(&mut cli) {
        let usage = matches!(e, barca_core::BarcaError::Usage(_));
        let err = CliError::from_barca(e, &ctx);
        if usage {
            err.with_final_hint(list_hint(&[])).emit(json)
        } else {
            err.emit(json)
        }
    }

    // The one runtime for the whole process — barca-core is async-native and
    // runs on whatever runtime the caller provides.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            CliError::from_barca(
                barca_core::BarcaError::Other(format!("failed to create runtime: {e}")),
                &Context::default(),
            )
            .emit(json)
        });
    if let Err(e) = rt.block_on(run_cli(cli, &ctx)) {
        // A failed run gets one greppable line naming the step, right before the error.
        if e.kind == ErrorKind::StepFailed
            && let Some(node) = &e.node
        {
            eprintln!(
                "[barca] run failed: step '{node}' failed (exit {})",
                e.code()
            );
        }
        e.emit(json);
    }
}

#[allow(clippy::result_large_err)] // cold path: one CliError per process, right before exiting
async fn run_cli(cli: Cli, ctx: &Context) -> Result<(), CliError> {
    let python = barca_core::commands::find_python();
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, ctx);

    match cli {
        Cli::Get {
            args,
            output,
            format,
            refresh,
            no_cascade,
            refresh_all,
            no_cache,
            dry_run,
            agent,
            fields,
            env,
        } => {
            check_order("get", &args)?;
            let output = get_run_mode(output, format, fields.as_deref())?;
            let (target, files) = split_target_files(args);
            check_py_files(&files, refresh.as_deref())?;
            if files.is_empty() {
                let what = if target.is_none() {
                    "files"
                } else {
                    ".py files"
                };
                return Err(usage_error(
                    &format!("error: no {what} provided\n\n{GET_USAGE}"),
                    &files,
                ));
            }
            let hint_files: Vec<PathBuf> = ctx.files.iter().map(PathBuf::from).collect();
            let targets = targets_arg(target.as_deref(), &hint_files)?;
            let policy = cache_policy(refresh, no_cascade, refresh_all, no_cache);
            get_cmd(
                env.as_deref(),
                targets,
                files,
                &python,
                output,
                policy,
                dry_run,
                agent,
                fields.as_deref(),
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Run {
            args,
            refresh,
            no_cascade,
            refresh_all,
            no_cache,
            dry_run,
            output,
            format,
            agent,
            fields,
            env,
        } => {
            check_order("run", &args)?;
            let output = get_run_mode(output, format, fields.as_deref())?;
            let (target, files) = split_target_files(args);
            check_py_files(&files, refresh.as_deref())?;
            let Some(target) = target else {
                return Err(usage_error(
                    &format!("error: a target task is required\n\n{RUN_USAGE}"),
                    &files,
                ));
            };
            if files.is_empty() {
                return Err(usage_error(
                    &format!("error: no .py files provided\n\n{RUN_USAGE}"),
                    &files,
                ));
            }
            let hint_files: Vec<PathBuf> = ctx.files.iter().map(PathBuf::from).collect();
            let policy = cache_policy(refresh, no_cascade, refresh_all, no_cache);
            let targets = targets_arg(Some(&target), &hint_files)?;
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
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Plan { files } => plan_cmd(files, &python).await.map_err(engine),
        Cli::History {
            limit,
            all,
            format,
            fields,
            env,
        } => {
            let json = fields_json(format, fields.as_deref())?;
            let limit = (!all).then_some(limit);
            history_cmd(env.as_deref(), limit, json, fields.as_deref())
                .await
                .map_err(engine)
        }
        Cli::Stats {
            target,
            files,
            format,
            fields,
            env,
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
            files,
            format,
            limit,
            all,
            fields,
        } => {
            let json = fields_json(format, fields.as_deref())?;
            let limit = (!all).then_some(limit);
            list_cmd(files, json, limit, fields.as_deref(), &python)
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
        } => {
            let json = fields_json(format, fields.as_deref())?;
            check_order("status", &args)?;
            let (target, files) = split_target_files(args);
            check_py_files(&files, None)?;
            if files.is_empty() {
                return Err(usage_error(
                    &format!("error: no .py files provided\n\n{STATUS_USAGE}"),
                    &files,
                ));
            }
            let hint_files: Vec<PathBuf> = ctx.files.iter().map(PathBuf::from).collect();
            let targets = targets_arg(target.as_deref(), &hint_files)?;
            let limit = (!all).then_some(limit);
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
            watch,
            no_schedule,
            timezone,
            read_only,
            env,
        } => serve_cmd(
            env.as_deref(),
            files,
            port,
            watch,
            !no_schedule,
            timezone,
            read_only,
            &python,
        )
        .await
        .map_err(engine),
        // Answered in main() before the runtime is built — never reaches here.
        Cli::Version => unreachable!("version is handled before runtime construction"),
        Cli::Docs { .. } => unreachable!("docs is handled before runtime construction"),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
