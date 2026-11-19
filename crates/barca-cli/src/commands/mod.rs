//! The local backend: a [`Request`] run on this machine, calling `barca_core`.

mod doctor;
pub(crate) use doctor::*;
mod execute;
mod plan;
pub(crate) use plan::*;
mod sql;
pub(crate) use sql::*;
mod list;
pub(crate) use list::*;
mod status;
pub(crate) use status::*;
pub(crate) mod logs;
pub(crate) use logs::*;
mod history;
pub(crate) use history::*;
mod gc;
pub(crate) use gc::*;
mod stats;
pub(crate) use stats::*;
mod serve;
pub(crate) use serve::*;
mod state;
pub(crate) use state::*;
mod get;
pub(crate) use get::*;
pub(crate) mod pin;
mod run;
pub(crate) use run::*;

fn project_root() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|root| root.display().to_string())
}

use crate::error::{CliError, Context};
use crate::input::{cache_policy, get_run_error};
use crate::print::emit;
use crate::request::{HistoryView, Request, StateAction, StatusOpts, StatusView};
use std::path::PathBuf;

/// Run `request` on this machine. `ledger` is the configured server a `get` or `run` reads
/// and posts through (RFC-0007 §4.1). `doctor` and the commands that need a server never
/// reach here: `dispatch` answers them.
pub(crate) async fn perform(
    request: Request,
    ledger: Option<barca_core::ledger::Ledger>,
    ctx: &Context,
) -> Result<(), CliError> {
    let python = barca_core::commands::find_python();
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, ctx);
    // An unknown target ends with the `barca list` pointer, naming the files as typed.
    let hint_files: Vec<PathBuf> = ctx.files.iter().map(PathBuf::from).collect();
    let target_error = |e: barca_core::BarcaError| get_run_error(e, ctx, &hint_files);

    match request {
        Request::Get { run, mode } => get_cmd(
            run.env.as_deref(),
            run.targets,
            run.files,
            &python,
            mode,
            cache_policy(run.refresh, run.no_cascade, run.refresh_all),
            run.selection,
            run.batch,
            run.dry_run,
            run.agent,
            run.fields.as_deref(),
            ledger,
            run.allow_paused,
        )
        .await
        .map_err(target_error),
        Request::Run { run, mode } => run_cmd(
            run.env.as_deref(),
            run.targets,
            run.files,
            &python,
            cache_policy(run.refresh, run.no_cascade, run.refresh_all),
            run.dry_run,
            mode,
            run.agent,
            run.fields.as_deref(),
            ledger,
            run.selection,
            run.batch,
            run.allow_paused,
        )
        .await
        .map_err(target_error),
        Request::Status(status) => {
            let env = status.env.as_deref();
            match status.view {
                StatusView::Keys {
                    target,
                    view,
                    limit,
                } => {
                    keys_cmd(
                        env,
                        &target,
                        status.files,
                        view,
                        status.json,
                        limit,
                        &python,
                    )
                    .await
                }
                StatusView::Rollup { .. } => {
                    rollup_cmd(env, status.targets, status.files, status.json, &python).await
                }
                StatusView::Nodes {
                    limit,
                    fields,
                    sample,
                    filter,
                } => {
                    status_cmd(
                        env,
                        status.targets,
                        status.files,
                        StatusOpts {
                            json: status.json,
                            limit,
                            fields: fields.as_deref(),
                            sample,
                            filter,
                        },
                        &python,
                    )
                    .await
                }
            }
            .map_err(target_error)
        }
        Request::List(list) => list_cmd(
            list.files,
            list.groups,
            list.json,
            list.limit,
            list.fields.as_deref(),
            &python,
        )
        .await
        .map_err(engine),
        Request::Logs(logs) => logs_cmd(logs.env.as_deref(), logs.run_id.as_deref(), logs.args())
            .await
            .map_err(engine),
        Request::History(history) => {
            let env = history.env.as_deref();
            match history.view {
                HistoryView::Run {
                    run_id,
                    limit,
                    select,
                } => run_detail_cmd(env, &run_id, limit, history.json, &select, &python).await,
                HistoryView::Activity { filter } => activity_cmd(env, &filter, history.json).await,
                HistoryView::Runs {
                    limit,
                    filter,
                    fields,
                    reconcile,
                } => {
                    history_cmd(
                        env,
                        HistoryArgs {
                            limit,
                            json: history.json,
                            fields: fields.as_deref(),
                            reconcile: reconcile.as_deref(),
                            filter,
                            python: &python,
                        },
                    )
                    .await
                }
            }
            .map_err(engine)
        }
        Request::Stats(stats) => stats_cmd(
            stats.env.as_deref(),
            stats.target,
            stats.files,
            stats.json,
            stats.fields.as_deref(),
            &python,
        )
        .await
        .map_err(engine),
        Request::Sql(sql) => sql_cmd(
            sql.env.as_deref(),
            &sql.query,
            sql.files,
            sql.limit,
            sql.json,
            &python,
        )
        .await
        .map_err(engine),
        Request::Pin(pin) => {
            pin::pin_cmd(
                pin.env.as_deref(),
                &pin.action,
                pin.files,
                pin.json,
                &python,
            )
            .await
        }
        Request::Unpin(unpin) => {
            pin::unpin_cmd(
                unpin.env.as_deref(),
                &unpin.target,
                unpin.files,
                unpin.json,
                &python,
            )
            .await
        }
        Request::Gc(gc) => {
            gc_cmd(
                gc.env.as_deref(),
                gc.max_size.as_deref(),
                gc.dry_run,
                gc.json,
            )
            .await
        }
        Request::Plan(plan) => plan_cmd(plan.files, &python, plan.partition)
            .await
            .map_err(engine),
        Request::State(state) => {
            let env = state.env.as_deref();
            match state.action {
                StateAction::Backups => state_backups_cmd(env, &python, state.json).await,
                StateAction::Restore { from } => {
                    state_restore_cmd(env, from.as_deref(), &python, state.json).await
                }
                StateAction::Reset { yes } => state_reset_cmd(env, yes, state.json).await,
            }
            .map_err(engine)
        }
        Request::Serve(args) => serve_cmd(args, &python, ctx).await,
        Request::Doctor(_)
        | Request::Queue(_)
        | Request::Pause(_)
        | Request::Resume(_)
        | Request::Cancel(_) => unreachable!("answered by dispatch, never by this backend"),
    }
}
