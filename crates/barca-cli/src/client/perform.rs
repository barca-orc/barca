//! The remote backend: a [`Request`] sent to a server, its answer printed here as the command
//! prints it on this machine.

use super::{RunArgs, Server};
use crate::error::{CliError, Context};
use crate::request::{HistoryView, Request, StatusOpts, StatusView};
use barca_core::outcome::RunCommand;
use barca_core::report::{GetShape, ResultFormat};

/// Run `request` on `server`. `doctor` and the commands without a route never reach here:
/// `dispatch` answers them on this machine.
pub(crate) async fn perform(
    request: Request,
    server: &Server,
    ctx: &Context,
) -> Result<(), CliError> {
    match request {
        Request::Get { run, mode } => {
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
                barca_core::outln!("{}", answer);
            } else {
                barca_core::outln!(
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
