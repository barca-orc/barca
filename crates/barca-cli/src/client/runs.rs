//! Starting a run on the server and following it to its outcome (`barca get|run --server`):
//! `POST /runs`, the event stream ([`super::follow`]), Ctrl-C and the outcome, printed as a
//! local run prints them.

use super::follow::{Follow, Followed, outcome_of};
use super::{Server, in_files, reconnect_window};
use crate::error::{CliError, Context, ErrorKind, list_hint};
use barca_core::BarcaError;
use barca_core::outcome::{Outcome, RunCommand, RunRequest};
use barca_core::{RunEvent, lines::LineWhen};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// A `get` or `run` to start on the server, with how this machine prints it.
pub(crate) struct RunArgs {
    pub(crate) command: RunCommand,
    pub(crate) targets: Vec<String>,
    /// The files as typed (rebased onto the root by [`Server::deployed_files`]).
    pub(crate) files: Vec<PathBuf>,
    pub(crate) refresh: Option<Vec<String>>,
    pub(crate) no_cascade: bool,
    pub(crate) refresh_all: bool,
    pub(crate) dry_run: bool,
    pub(crate) agent: bool,
    /// How the result prints: for `get`, `Json` is `--json`, `Pretty` the summary and paths,
    /// and `Value` the paths alone (`-o path`); for `run`, as its `-o` says.
    pub(crate) mode: barca_core::report::ResultFormat,
    /// `get -o value`: the server reads the values, and they print as locally.
    pub(crate) values: bool,
    /// `get --partition`, as typed.
    pub(crate) partition: Option<String>,
    /// `--batch`, as typed (each `V` or `NODE=V`).
    pub(crate) batch: Vec<String>,
    pub(crate) fields: Option<Vec<String>>,
    pub(crate) env: Option<String>,
    /// `--allow-paused`.
    pub(crate) allow_paused: bool,
}

/// `barca get|run ... --server`: start the run on the server, print its lines as they come,
/// then its outcome, and end as a local run of the same command ends.
pub(crate) async fn run(server: &Server, args: RunArgs, ctx: &Context) -> Result<(), CliError> {
    let window = reconnect_window()?;
    server.health(args.env.as_deref()).await?;
    let files = server.deployed_files(&args.files).await?;
    let request = RunRequest {
        command: args.command,
        targets: args.targets.clone(),
        files,
        refresh: args.refresh.clone().unwrap_or_default(),
        refresh_all: args.refresh_all,
        cascade: !args.no_cascade,
        dry_run: args.dry_run,
        agent: args.agent,
        values: args.values,
        partition: args.partition.clone(),
        batch: args.batch.clone(),
        allow_paused: args.allow_paused,
    };
    let body = serde_json::to_value(&request).expect("a run request is JSON");
    let answer = server
        .post_json("/runs", &body)
        .await
        .map_err(|e| with_typed_files(e, ctx))?;
    if args.dry_run {
        return print_dry_run(server, &answer, &args, &request.files).await;
    }
    let run_id = serde_json::from_str::<Value>(&answer)
        .ok()
        .and_then(|v| v["run_id"].as_str().map(str::to_string))
        .ok_or_else(|| server.unexpected("POST /runs", "no run_id in the answer"))?;
    let outcome = stream(server, &run_id, &args, window).await?;
    let (rendered, error) = outcome
        .render(
            args.command,
            &args.targets,
            args.mode,
            args.values,
            args.fields.as_deref(),
        )
        .map_err(|e| server.unexpected("the run's outcome", &e))?;
    crate::commands::emit(rendered);
    match error {
        Some(envelope) => Err(with_typed_files(envelope, ctx)),
        None => Ok(()),
    }
}

/// A server's target error (unknown or ambiguous) ends with the `barca list` pointer naming the
/// files the user typed, as a local `get`/`run` ends it (`get_run_error`): the server names its
/// own files, or none. Other usage errors keep the server's own remediation (#492).
fn with_typed_files(mut error: CliError, ctx: &Context) -> CliError {
    if error.kind != ErrorKind::Usage {
        return error;
    }
    error.remediation = Some(match error.remediation.take() {
        Some(r) if r.starts_with("Run `barca list") => list_hint(&ctx.files),
        Some(r) => r,
        // The envelope's remediation is always a string (`barca docs contract`).
        None => format!("See `barca {} --help`.", ctx.command),
    });
    error
}

/// `--dry-run`: the server answers with the `--dry-run --json` body at once. The stderr lines a
/// local dry run prints before it are rebuilt here: the tasks a `get` without a target skips
/// (from the server's `/list`) and the plan warnings (from the body).
async fn print_dry_run(
    server: &Server,
    answer: &str,
    args: &RunArgs,
    files: &[String],
) -> Result<(), CliError> {
    let result: barca_core::results::ExplainResult = serde_json::from_str(answer)
        .map_err(|e| server.unexpected("POST /runs (dry run)", &e.to_string()))?;
    if args.command == RunCommand::Get && args.targets.is_empty() {
        let list = server.get_json("/list").await?;
        let nodes: Vec<barca_core::report::ListNode> =
            serde_json::from_value(list["nodes"].clone())
                .map_err(|e| server.unexpected("GET /list", &e.to_string()))?;
        let kinds: Vec<(&str, barca_core::NodeKind)> = nodes
            .iter()
            .filter(|n| in_files(&n.id, files))
            .map(|n| (n.id.as_str(), n.kind))
            .collect();
        let file_args = if files.is_empty() {
            server.deployed().await?
        } else {
            files.to_vec()
        };
        if let Some(note) = barca_core::targets::skipped_tasks_note_among(&kinds, &file_args) {
            barca_core::errln!("{note}");
        }
    }
    if let Some(note) = barca_core::selection::explain_note(&result) {
        barca_core::errln!("{note}");
    }
    barca_core::warnings::print(&result.warnings);
    crate::commands::emit(barca_core::report::render_explain(
        &result,
        args.command.as_str(),
        // A `get` dry run prints its plan as `--json` or as text, as locally (`-o path` alone
        // has no plan form of its own).
        match (args.command, args.mode) {
            (RunCommand::Get, barca_core::report::ResultFormat::Value) => {
                barca_core::report::ResultFormat::Pretty
            }
            (_, mode) => mode,
        },
        args.fields.as_deref(),
    ));
    Ok(())
}

/// Stop signals during a streamed run: the first cancels the run on the server and keeps
/// streaming until it ends; the second ends this command at once (exit 130).
fn stop_signals() -> tokio::sync::mpsc::UnboundedReceiver<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
        let Ok(mut stream) = signal(kind) else {
            continue;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
    }
    rx
}

/// Read `GET /runs/{run_id}/events` to its `run_finished`, printing `line` and `log` events as
/// a local run prints them in this mode. Returns the run's outcome. A stream that breaks is
/// followed again when the server comes back within `window` ([`Follow`]).
async fn stream(
    server: &Server,
    run_id: &str,
    args: &RunArgs,
    window: Duration,
) -> Result<Outcome, CliError> {
    let path = format!("/runs/{run_id}/events");
    let mut signals = stop_signals();
    let mut follow = Follow::new(server, run_id, window).await;
    if !follow.open().await? {
        // Not live: it already ended (a restart in between, or a very short run).
        return follow.recorded_outcome().await;
    }
    let mut cancelled = false;
    loop {
        let item = tokio::select! {
            item = follow.next() => item?,
            Some(()) = signals.recv() => {
                if cancelled {
                    return Err(CliError::from_barca(BarcaError::Cancelled, &Context::default()));
                }
                cancelled = true;
                cancel(server, run_id).await;
                continue;
            }
        };
        let event = match item {
            Followed::Event(event) => event,
            Followed::Ended(body) => return outcome_of(server, &path, &body),
        };
        match event {
            RunEvent::Line { line, when } => {
                if when == LineWhen::Always || args.agent {
                    barca_core::errln!("{line}");
                }
            }
            RunEvent::Log {
                node_id,
                line,
                stream,
                ..
            } => {
                // A step's stderr stays on the server's stderr, as a local run's stays on
                // this machine's; it is recorded with the run either way (`barca logs`).
                if args.mode != barca_core::report::ResultFormat::Json
                    && stream == barca_core::LogStream::Stdout
                {
                    barca_core::errln!("[{node_id}] {line}");
                }
            }
            RunEvent::RunFinished { outcome, .. } => {
                let outcome = outcome.ok_or_else(|| {
                    server.unexpected(&format!("GET {path}"), "run_finished has no outcome")
                })?;
                return Outcome::from_text(outcome.get().to_string())
                    .map_err(|e| server.unexpected(&format!("GET {path}"), &e));
            }
            RunEvent::RunStarted { .. } | RunEvent::StepFinished { .. } => {}
        }
    }
}

/// `DELETE /runs/{run_id}`. A failure is reported, not fatal: the stream still says how the
/// run ended.
async fn cancel(server: &Server, run_id: &str) {
    let path = format!("/runs/{run_id}");
    let result = match server.http.request(reqwest::Method::DELETE, &path) {
        Ok(request) => server.send(request, &format!("DELETE {path}")).await,
        Err(error) => Err(error),
    };
    if let Err(e) = result {
        barca_core::errln!(
            "[barca] warning: could not cancel run {run_id} on the server: {}",
            e.error
        );
    }
}
