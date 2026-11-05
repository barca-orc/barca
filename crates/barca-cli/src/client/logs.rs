//! `barca logs` against a server: the recorded lines (`GET /runs/{run_id}/logs`) and, with
//! `--follow`, a live run's lines from its event stream as they arrive.

use super::follow::{Follow, Followed};
use super::inspect::{here_instead, local_run, started_key};
use super::{Server, encode, reconnect_window};
use crate::error::{CliError, Context, ErrorKind};
use barca_core::BarcaError;
use barca_core::RunEvent;
use barca_core::outcome::Outcome;

/// `barca logs` against a server: the run's recorded lines from `GET /runs/{run_id}/logs`, and
/// with `--follow` a running run's lines from `GET /runs/{run_id}/events` as they arrive.
/// Without a run id, the newest run in the server's history (with `--follow`, the newest
/// running one).
pub(crate) async fn logs(
    server: &Server,
    run_id: Option<&str>,
    env: Option<&str>,
    args: crate::commands::logs::LogsArgs<'_>,
) -> Result<(), CliError> {
    use crate::commands::logs::{follow_ended, follow_exit_code, name_run};
    let window = reconnect_window()?;
    server.health(env).await?;
    let run_id = match run_id {
        Some(id) => id.to_string(),
        None => {
            let limit = barca_core::queries::RUNNING_LOOKBACK;
            let body = server.get_json(&format!("/history?limit={limit}")).await?;
            let runs: Vec<barca_core::db::RunRecord> = serde_json::from_value(body["runs"].clone())
                .map_err(|e| server.unexpected("GET /history", &e.to_string()))?;
            let Some(run) = barca_core::queries::pick_run(runs, args.follow) else {
                return Err(CliError::from_prose(
                    ErrorKind::Usage,
                    format!(
                        "error: no run is recorded on the barca server at {} yet
                         Start one (`barca run --server ...`), or read this machine's runs \
                         with --local.",
                        server.shown_url
                    ),
                ));
            };
            name_run(&run, args.follow);
            // A configured server answers, but a newer run of this machine's is said once.
            if server.implicit
                && let Some(here) = local_run(env, None).await
                && here.run_id != run.run_id
                && started_key(&here.started_at) > started_key(&run.started_at)
            {
                barca_core::errln!(
                    "[barca] note: this machine has a newer run {} ({} {}, {}); barca logs \
                     --local reads it",
                    here.run_id,
                    here.command,
                    barca_core::report::history_target(&here),
                    here.status
                );
            }
            run.run_id
        }
    };
    let engine = |e: BarcaError| CliError::from_barca(e, &Context::default());
    if args.follow {
        let mut follow = Follow::new(server, &run_id, window).await;
        follow.complete_logs = true;
        // Not live (404): the run ended, or it is not one the server started; its recorded
        // lines answer.
        let live = match follow.open().await {
            Ok(live) => live,
            Err(error) => {
                let here = format!("barca logs {run_id} --local");
                return Err(here_instead(error, &run_id, env, &here).await);
            }
        };
        if live {
            let status = follow_events(&mut follow, args).await?;
            follow_ended(&run_id, &status);
            barca_core::term::exit(follow_exit_code(&status));
        }
    }
    let (status, lines) = match recorded_logs(server, &run_id, args).await {
        Ok(found) => found,
        Err(e) => {
            let here = format!("barca logs {run_id} --local");
            return Err(here_instead(e, &run_id, env, &here).await);
        }
    };
    args.print_all(&lines).map_err(engine)?;
    if args.follow {
        follow_ended(&run_id, &status);
        barca_core::term::exit(follow_exit_code(&status));
    }
    Ok(())
}

/// `GET /runs/{run_id}/logs`: the run's status and its recorded lines, filtered as asked.
async fn recorded_logs(
    server: &Server,
    run_id: &str,
    args: crate::commands::logs::LogsArgs<'_>,
) -> Result<(String, Vec<barca_core::db::LogEntry>), CliError> {
    let mut query = Vec::new();
    if let Some(step) = args.step {
        query.push(format!("step={}", encode(step)));
    }
    if let Some(stream) = args.stream {
        query.push(format!("stream={}", stream.as_str()));
    }
    let mut path = format!("/runs/{}/logs", encode(run_id));
    if !query.is_empty() {
        path = format!("{path}?{}", query.join("&"));
    }
    let body = server.get_json(&path).await?;
    let bad = |e: String| server.unexpected(&format!("GET {path}"), &e);
    let status = body["status"]
        .as_str()
        .ok_or_else(|| bad("no status".into()))?
        .to_string();
    let lines = serde_json::from_value(body["logs"].clone()).map_err(|e| bad(e.to_string()))?;
    Ok((status, lines))
}

/// Read a live run's event stream to its end, printing the lines `args` selects as they arrive,
/// through a server restart ([`Follow`]). Returns the run's final status.
async fn follow_events(
    follow: &mut Follow<'_>,
    args: crate::commands::logs::LogsArgs<'_>,
) -> Result<String, CliError> {
    let mut seq = 0usize;
    // Lines printed of the run as the current server runs it.
    let mut printed = 0usize;
    let mut restarts = 0usize;
    loop {
        let item = follow.next().await?;
        if follow.restarts != restarts {
            restarts = follow.restarts;
            printed = 0;
        }
        match item {
            Followed::Ended(body) => {
                // Ended while the stream was down: the lines it printed since are recorded
                // (on a restarted server, every line is of the attempt that ended there).
                let status = body["status"].as_str().unwrap_or("failed").to_string();
                let (_, lines) = recorded_logs(follow.server, &follow.run_id, args).await?;
                let missed: Vec<_> = lines.into_iter().skip(printed).collect();
                args.print_all(&missed)
                    .map_err(|e| CliError::from_barca(e, &Context::default()))?;
                return Ok(status);
            }
            Followed::Event(RunEvent::Log {
                node_id,
                line,
                stream,
                ts,
            }) => {
                let entry = barca_core::db::LogEntry {
                    node_id,
                    seq: seq as i64,
                    line,
                    stream,
                    ts,
                };
                seq += 1;
                if args.selects(&entry.node_id, entry.stream) {
                    args.print_one(&entry);
                    printed += 1;
                }
            }
            Followed::Event(RunEvent::RunFinished { ok, outcome, .. }) => {
                let status = outcome
                    .and_then(|raw| Outcome::from_text(raw.get().to_string()).ok())
                    .map(|o| o.status().as_str().to_string())
                    .unwrap_or_else(|| if ok { "success" } else { "failed" }.to_string());
                return Ok(status);
            }
            Followed::Event(
                RunEvent::RunStarted { .. } | RunEvent::Line { .. } | RunEvent::StepFinished { .. },
            ) => {}
        }
    }
}
