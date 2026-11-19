//! Barca CLI — invisible asset orchestrator.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
// Every fallible command ends with at most one `CliError` (the error envelope), built right
// before the process exits: the size of the `Err` variant does not matter anywhere here.
#![allow(clippy::result_large_err)]

mod args;
mod bounded;
mod client;
mod commands;
#[cfg(test)]
mod contract;
mod dispatch;
mod docs;
mod error;
mod input;
mod output;
mod print;
mod request;
mod route;
mod runtime;

use args::{Cli, TokenCommand};
use error::{CliError, CliErrorExt, Context, ErrorKind};

use clap::{CommandFactory, Parser};

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
    if let Some(err) = route::refuse_on_local_commands(&args) {
        err.emit(output::json_mode_from_argv(&args))
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
    let cli = match parsed {
        Ok(cli) => cli,
        // `--help` / `--version` are not errors: clap prints them to stdout and exits 0.
        Err((e, _)) if !e.use_stderr() => e.exit(),
        Err((e, argv)) => CliError::from_clap(&e, &argv, Cli::command()).emit(output::json_mode_from_argv(&args)),
    };
    let json = input::json_output(&cli);

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
            .map(|out| input::project_docs_json(out, fields.as_deref()))
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
    let ctx = input::context(&cli);

    // Where the command runs (dispatch.rs), then the command, checked once for that place.
    let mut runtime = runtime::Runtime::lazy(json, matches!(cli, Cli::Serve { .. }));
    let placed = dispatch::place(cli, &ctx, &mut runtime, |cli| {
        if matches!(
            cli,
            Cli::Tokens {
                command: TokenCommand::Bootstrap
            }
        ) {
            let root = std::env::current_dir().unwrap_or_else(|e| {
                CliError::from_prose(ErrorKind::Infra, e.to_string()).emit(false)
            });
            match barca_server::auth::Store::open(&root).and_then(|store| store.bootstrap()) {
                Ok(code) => barca_core::outln!("{code}"),
                Err(error) => CliError::from_prose(ErrorKind::Infra, error.to_string()).emit(false),
            }
            finish(false);
        }
    })
    .unwrap_or_else(|e| e.emit(json));
    if let Err(e) = runtime.get().block_on(dispatch::run(placed, &ctx)) {
        e.emit(json);
    }
    runtime.finish();
    finish(json);
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ui_layering_tests.rs"]
mod ui_layering_tests;

#[cfg(test)]
mod layering_tests;
