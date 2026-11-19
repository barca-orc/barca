//! The one place that decides where a command runs, and runs it.
//!
//! A command is one [`Request`] and three places it can go: this process (`commands`, which
//! calls `barca_core`), a server named by `--server`, `BARCA_SERVER` or `[server] url`, or the
//! `barca serve` running in this project, found by its socket (`client` for both). [`place`]
//! resolves which, from the command's row in the table (`route`), in the order the places are
//! looked for; [`run`] checks the command once for that place (`Request::of`) and hands it to
//! the backend. Nothing else in the crate chooses.
//!
//! The order, which is also the order of what a user sees on stderr:
//!
//! 1. `--server`: the server is chosen (and named on stderr when it came from configuration)
//!    before anything is read on this machine. The command goes to it from the directory it was
//!    typed in, except `sql`, whose query runs here, in the project.
//! 2. The project root is entered and discovery fills in the files.
//! 3. The project's own server, by its socket: one or two `stat` calls when there is none, and
//!    not even those for a command its row says never goes there, for `--local`, and inside a
//!    step (`BARCA_WORKER`).
//! 4. A configured server, for what the row says: the ledger of a `get` or `run` that runs
//!    here, the answer of an inspection command, the views of `sql`.
//! 5. This machine.

use crate::args::Cli;
use crate::client::{self, Server};
use crate::commands;
use crate::error::{CliError, Context};
use crate::input;
use crate::request::{DoctorRequest, Request, Side};
use crate::route::{Configured, Named, Route};
use crate::runtime::Runtime;

/// Where a command runs.
pub(crate) enum Target {
    /// In this process. `ledger` is the configured server a `get` or `run` reads and posts
    /// through.
    Here {
        ledger: Option<barca_core::ledger::Ledger>,
    },
    /// On a server: named, configured, or this project's own.
    Server(Server),
}

/// A command and where it runs, after [`place`].
pub(crate) struct Placed {
    pub(crate) cli: Cli,
    pub(crate) target: Target,
}

/// Decide where `cli` runs, entering the project root on the way unless a named server takes
/// the whole command. `in_root` runs once, right after the root is entered: `main` answers
/// there the one command that needs the root and nothing else. `runtime` is built only to ask
/// a server found by its socket whether it answers.
pub(crate) fn place(
    mut cli: Cli,
    ctx: &Context,
    runtime: &mut Runtime,
    in_root: impl FnOnce(&Cli),
) -> Result<Placed, CliError> {
    let route = Route::of(&cli);

    // 1. `--server`. `doctor` has no flags here (`input::server_flags`): it reads its own
    // once its checks of this machine are set up ([`doctor`]).
    let mut named = None;
    if let Some(flags) = input::server_flags(&cli) {
        match client::choose(flags)? {
            // `barca sql --server`: the server resolves the views and the query runs here,
            // in the project (below).
            Some(server) if route.named == Named::Views => named = Some(server),
            Some(server) => {
                return Ok(Placed {
                    cli,
                    target: Target::Server(server),
                });
            }
            None => {}
        }
    }

    // 2. Run from the project root (the nearest barca.toml at or above the cwd), so `.barca/`,
    // node ids and relative paths inside steps are the same wherever barca is invoked. Files
    // typed on the command line are remembered first: without any, a server uses every file
    // it has deployed, not the ones discovery finds here.
    let files_given = input::files_given(&cli);
    input::enter_project_root(&mut cli).map_err(|e| CliError::from_barca(e, ctx))?;
    in_root(&cli);

    // 3. A `barca serve` running in this project (its socket beside the metadata database):
    // the command goes to it instead of opening the database beside it, where it would wait
    // on the lock the server's writes hold (#1054). `get` and `run` join its queue; `--local`
    // runs here. No server (no socket file) costs a `stat` or two and nothing else.
    if named.is_none()
        && route.project
        && let Some(server) = input::server_flags(&cli)
            .and_then(|flags| client::project_server(flags, input::env_of(&cli)))
    {
        match runtime.get().block_on(server.answers()) {
            Ok(()) => {
                server.announce();
                if !files_given {
                    input::drop_discovered_files(&mut cli);
                }
                return Ok(Placed {
                    cli,
                    target: Target::Server(server),
                });
            }
            // Something holds the socket and does not answer: as with no server at all.
            Err(why) => barca_core::errln!(
                "[barca] warning: this project's server did not answer on {} ({why}); running \
                 on this machine",
                server.shown()
            ),
        }
    }

    // 4. A configured server without `--server` (and without `--local`): `get` and `run` run
    // here against its ledger; the inspection commands answer from it (RFC-0007 §4.1).
    let configured = match input::server_flags(&cli) {
        Some(flags) => client::configured(flags)?,
        None => None,
    };
    let mut ledger = None;
    match (configured, route.configured) {
        (Some(server), Configured::Ledger) => ledger = Some(server.into_ledger()),
        (Some(server), Configured::Views) => {
            server.announce();
            named = Some(server);
        }
        (Some(server), Configured::Answers) => {
            server.announce();
            return Ok(Placed {
                cli,
                target: Target::Server(server),
            });
        }
        (Some(_), Configured::No | Configured::Checks) | (None, _) => {}
    }
    if let Some(server) = named {
        // `sql` with a server: the files discovery found here are not the server's.
        if !files_given {
            input::drop_discovered_files(&mut cli);
        }
        return Ok(Placed {
            cli,
            target: Target::Server(server),
        });
    }

    // 5. This machine.
    Ok(Placed {
        cli,
        target: Target::Here { ledger },
    })
}

/// Check the command once for where it runs, then run it there.
pub(crate) async fn run(placed: Placed, ctx: &Context) -> Result<(), CliError> {
    let Placed { cli, target } = placed;
    let route = Route::of(&cli);
    match target {
        Target::Server(server) => {
            let request = Request::of(cli, Side::Server)?;
            client::perform(request, &server, ctx).await
        }
        // A command only a server answers, and none was found.
        Target::Here { .. } if !route.here => Err(route.needs_a_server()),
        Target::Here { ledger } => match Request::of(cli, Side::Here)? {
            Request::Doctor(request) => doctor(request).await,
            request => commands::perform(request, ledger, ctx).await,
        },
    }
}

/// `barca doctor`: this machine's checks, a server's when one is named or configured, and the
/// report. The one command both backends take part in.
async fn doctor(request: DoctorRequest) -> Result<(), CliError> {
    // `--server [<url>]` checks that server too; a configured one is checked without it, as
    // `status` answers from it, unless `--local`. A barca.toml that cannot be read is the
    // `configuration` check's failure, not a usage error here.
    let server = match client::choose(&request.server)? {
        Some(server) => Some(server),
        None => client::configured(&request.server)
            .ok()
            .flatten()
            .inspect(|s| s.announce()),
    };
    let env = request.env.as_deref();
    let mut report = commands::doctor_report(&request.files, env).await;
    if let Some(server) = &server {
        client::doctor_checks(server, &mut report, env).await;
        report.ok = report.checks.iter().all(|check| check.ok);
    }
    commands::print_doctor(&report, request.json)
}
