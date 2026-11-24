//! The remote backend: a [`Request`] sent to a server, its answer printed here as the command
//! prints it on this machine.

use super::{RunArgs, Server};
use crate::error::{CliError, Context};
use crate::request::{HistoryView, Request, StatusOpts, StatusView};
use barca_service::outcome::RunCommand;
use barca_service::report::{GetShape, ResultFormat};

/// Run `request` on `server`. `doctor` and the commands without a route never reach here:
/// `dispatch` answers them on this machine.
pub(crate) async fn perform(
    request: Request,
    server: &Server,
    ctx: &Context,
) -> Result<(), CliError> {
    match request {
        Request::Get { run, mode } => {
            if let Some(tier) = mode.cached_only {
                server.health(run.env.as_deref()).await?;
                let request = barca_service::cached::Request {
                    targets: run.targets,
                    files: server.deployed_files(&run.files).await?,
                    tier,
                    partition: run.partition,
                    values: mode.values,
                    url_expires: mode.url_expires,
                };
                let response = server
                    .http
                    .request(reqwest::Method::POST, "/cached")?
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(serde_json::json!(request).to_string())
                    .send()
                    .await
                    .map_err(|e| server.unreachable(&e))?;
                let status = response.status();
                let body = response.bytes().await.map_err(|e| server.unreachable(&e))?;
                let answer: barca_service::cached::Answer = match serde_json::from_slice(&body) {
                    Ok(answer)
                        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND =>
                    {
                        answer
                    }
                    _ => return Err(server.refusal(status, &body, "POST /cached")),
                };
                return crate::print::print_cached(&answer, mode, run.fields.as_deref())
                    .map_err(|e| CliError::from_barca(e, ctx));
            }
            let format = match mode.shape {
                GetShape::Json => ResultFormat::Json,
                GetShape::Pretty => ResultFormat::Pretty,
                GetShape::Bare => ResultFormat::Value,
            };
            let args = RunArgs {
                command: RunCommand::Get,
                targets: run.targets,
                files: run.files,
                refresh: run.refresh,
                no_cascade: run.no_cascade,
                refresh_all: run.refresh_all,
                dry_run: run.dry_run,
                agent: run.agent,
                mode: format,
                values: mode.values,
                partition: run.partition,
                batch: run.batch.to_flags(),
                fields: run.fields,
                env: run.env,
                allow_paused: run.allow_paused,
                queue: run.queue,
            };
            super::run(server, args, ctx).await
        }
        Request::Run { run, mode } => {
            let args = RunArgs {
                command: RunCommand::Run,
                targets: run.targets,
                files: run.files,
                refresh: run.refresh,
                no_cascade: run.no_cascade,
                refresh_all: run.refresh_all,
                dry_run: run.dry_run,
                agent: run.agent,
                mode: mode.into(),
                values: false,
                partition: run.partition,
                batch: run.batch.to_flags(),
                fields: run.fields,
                env: run.env,
                allow_paused: run.allow_paused,
                queue: run.queue,
            };
            super::run(server, args, ctx).await
        }
        Request::Status(status) => {
            let env = status.env.as_deref();
            match status.view {
                StatusView::Keys {
                    target,
                    view,
                    limit,
                } => {
                    super::partition_keys(
                        server,
                        &target,
                        &status.files,
                        view,
                        status.json,
                        limit,
                        env,
                    )
                    .await
                }
                StatusView::Rollup { now } => {
                    super::rollup(
                        server,
                        &status.targets,
                        &status.files,
                        status.json,
                        now,
                        env,
                    )
                    .await
                }
                StatusView::Nodes {
                    limit,
                    fields,
                    sample,
                    filter,
                } => {
                    let opts = StatusOpts {
                        json: status.json,
                        limit,
                        fields: fields.as_deref(),
                        sample,
                        filter,
                    };
                    super::status(server, &status.targets, &status.files, opts, env).await
                }
            }
        }
        Request::List(list) => {
            super::list(
                server,
                &list.files,
                list.groups,
                list.json,
                list.limit,
                list.fields.as_deref(),
            )
            .await
        }
        Request::Logs(logs) => {
            super::logs(
                server,
                logs.run_id.as_deref(),
                logs.env.as_deref(),
                logs.args(),
            )
            .await
        }
        Request::History(history) => {
            let env = history.env.as_deref();
            match history.view {
                HistoryView::Run {
                    run_id,
                    limit,
                    select,
                } => super::run_detail(server, &run_id, limit, history.json, &select, env).await,
                HistoryView::Activity { filter } => {
                    super::activity(server, &filter, history.json, env).await
                }
                HistoryView::Runs {
                    limit,
                    filter,
                    fields,
                    // Refused for a server before it gets here (`Request::of`).
                    reconcile: _,
                } => {
                    super::history(server, limit, &filter, history.json, fields.as_deref(), env)
                        .await
                }
            }
        }
        Request::Stats(stats) => {
            super::stats(
                server,
                &stats.target,
                &stats.files,
                stats.json,
                stats.fields.as_deref(),
                stats.env.as_deref(),
            )
            .await
        }
        Request::Sql(sql) => {
            super::sql(
                server,
                &sql.query,
                &sql.files,
                sql.limit,
                sql.json,
                sql.env.as_deref(),
            )
            .await
        }
        Request::Queue(queue) => {
            super::queue(server, queue.action, queue.json, queue.env.as_deref()).await
        }
        Request::Pause(pause) => {
            let env = pause.env.as_deref();
            match pause.schedule {
                Some(schedule) => {
                    super::pause_schedule(
                        server,
                        &schedule,
                        Some(pause.until.as_deref()),
                        pause.json,
                        env,
                    )
                    .await
                }
                None => {
                    super::pause(
                        server,
                        pause.target.as_deref(),
                        pause.downstream,
                        pause.dry_run,
                        pause.json,
                        env,
                    )
                    .await
                }
            }
        }
        Request::Resume(resume) => {
            let env = resume.env.as_deref();
            match resume.schedule {
                Some(schedule) => {
                    super::pause_schedule(server, &schedule, None, resume.json, env).await
                }
                None => {
                    super::resume(
                        server,
                        resume.target.as_deref(),
                        resume.downstream,
                        resume.all,
                        resume.json,
                        env,
                    )
                    .await
                }
            }
        }
        Request::Cancel(cancel) => {
            server.health(cancel.env.as_deref()).await?;
            let answer = super::cancel_run(server, &cancel.run_id).await?;
            if cancel.json {
                barca_service::outln!("{}", answer);
            } else {
                barca_service::outln!(
                    "{}: {}",
                    cancel.run_id,
                    answer["status"].as_str().unwrap_or("cancelling")
                );
            }
            Ok(())
        }
        Request::Pin(pin) => {
            super::pins::pin(
                server,
                &pin.action,
                &pin.files,
                pin.json,
                pin.env.as_deref(),
            )
            .await
        }
        Request::Unpin(unpin) => {
            super::pins::unpin(
                server,
                &unpin.target,
                &unpin.files,
                unpin.json,
                unpin.env.as_deref(),
            )
            .await
        }
        Request::Gc(gc) => {
            super::gc(
                server,
                gc.max_size.as_deref(),
                gc.dry_run,
                gc.json,
                gc.env.as_deref(),
            )
            .await
        }
        Request::Doctor(_) | Request::Plan(_) | Request::State(_) | Request::Serve(_) => {
            unreachable!("only the commands a server has a route for reach here")
        }
    }
}
