//! Where each command can run: the one table.
//!
//! A command runs in this process (calling `barca_core`), on a server named by `--server`
//! (a URL, or bare for the configured one), on the `barca serve` running in this project
//! (found by its socket), or, with a server configured and no flag, partly on that server.
//! [`ROUTES`] says which of these each command takes. Everything else that needs the answer is
//! derived from it: the commands that take `--server` and `--local` ([`Route::takes_flags`]),
//! the usage error for those flags on a command that always runs here
//! ([`refuse_on_local_commands`]), whether the project's socket is looked for
//! ([`Route::project`]), what a configured server is used for ([`Route::configured`]), and the
//! usage error of a command that has no server to run on ([`Route::needs_a_server`]).
//!
//! `dispatch` reads the table; nothing else decides where a command runs.

use crate::args::Cli;
use crate::error::{CliError, ErrorKind};

/// What `--server [URL]` does to a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Named {
    /// The command does not take `--server` or `--local`: it always runs on this machine.
    No,
    /// The whole command runs on the server; its result prints here.
    Runs,
    /// `sql`: the server resolves the views, and the query runs here, in the project.
    Views,
    /// `doctor`: the checks run here, and the server is checked too.
    Checks,
    /// `history --reconcile`: a usage error (it records a run on this machine).
    Refused,
}

/// What a configured server (`BARCA_SERVER`, else `[server] url`) is used for by a command
/// that named none and did not say `--local`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Configured {
    /// Nothing: the command runs here against this machine's own records.
    No,
    /// `get`, `run`: the command runs here, with the server as its ledger.
    Ledger,
    /// The server answers the command.
    Answers,
    /// `sql`: as [`Named::Views`].
    Views,
    /// `doctor`: as [`Named::Checks`].
    Checks,
}

/// One command's row.
#[derive(Debug)]
pub(crate) struct Route {
    /// The subcommand, as typed; one row may narrow it (`history --reconcile`).
    pub(crate) name: &'static str,
    /// The flag that selects this row over the command's own, if any.
    pub(crate) when: Option<&'static str>,
    /// With `--server`.
    pub(crate) named: Named,
    /// Whether a `barca serve` running in this project takes the command (its socket).
    pub(crate) project: bool,
    /// With a configured server and neither flag.
    pub(crate) configured: Configured,
    /// Whether the command can run on this machine. `false`: it needs a server
    /// ([`Route::needs_a_server`]).
    pub(crate) here: bool,
}

const fn route(
    name: &'static str,
    named: Named,
    project: bool,
    configured: Configured,
    here: bool,
) -> Route {
    Route {
        name,
        when: None,
        named,
        project,
        configured,
        here,
    }
}

/// A command that always runs on this machine.
const fn local(name: &'static str) -> Route {
    route(name, Named::No, false, Configured::No, true)
}

/// Every command. The commands that take `--server` come first, in the order the usage error
/// for the others names them.
pub(crate) const ROUTES: &[Route] = &[
    route("get", Named::Runs, true, Configured::Ledger, true),
    route("run", Named::Runs, true, Configured::Ledger, true),
    route("status", Named::Runs, true, Configured::Answers, true),
    // It reads source only, never the database: nothing to wait for beside a server.
    route("list", Named::Runs, false, Configured::No, true),
    route("history", Named::Runs, true, Configured::Answers, true),
    route("stats", Named::Runs, true, Configured::Answers, true),
    route("logs", Named::Runs, true, Configured::Answers, true),
    route("sql", Named::Views, true, Configured::Views, true),
    route("queue", Named::Runs, true, Configured::Answers, false),
    route("pause", Named::Runs, true, Configured::Answers, false),
    route("resume", Named::Runs, true, Configured::Answers, false),
    route("cancel", Named::Runs, true, Configured::Answers, false),
    route("pin", Named::Runs, true, Configured::Answers, true),
    route("unpin", Named::Runs, true, Configured::Answers, true),
    route("gc", Named::Runs, true, Configured::No, true),
    route("doctor", Named::Checks, false, Configured::Checks, true),
    // It records a run in this machine's history.
    Route {
        name: "history",
        when: Some("--reconcile"),
        named: Named::Refused,
        project: false,
        configured: Configured::No,
        here: true,
    },
    local("plan"),
    local("state"),
    local("serve"),
    local("tokens"),
    local("docs"),
    local("version"),
];

impl Route {
    /// Whether the command takes `--server` and `--local`.
    pub(crate) fn takes_flags(&self) -> bool {
        self.named != Named::No
    }

    /// The row of a subcommand name (the command's own row, not a narrowed one).
    pub(crate) fn named(name: &str) -> Option<&'static Route> {
        ROUTES.iter().find(|r| r.name == name && r.when.is_none())
    }

    /// The row of a parsed command.
    pub(crate) fn of(cli: &Cli) -> &'static Route {
        let name = crate::input::command_name(cli);
        let when = match cli {
            Cli::History {
                reconcile: Some(_), ..
            } => Some("--reconcile"),
            _ => None,
        };
        ROUTES
            .iter()
            .find(|r| r.name == name && r.when == when)
            .unwrap_or_else(|| unreachable!("every command has a row in ROUTES: {name}"))
    }

    /// The usage error of a command that runs only on a server ([`Route::here`] is `false`)
    /// when there is none: not named, not running in this project, not configured, or `--local`.
    pub(crate) fn needs_a_server(&self) -> CliError {
        let text = match self.name {
            "cancel" => "cancel needs a server; run it in the project a `barca serve` is running \
                         in, pass --server URL or configure a server"
                .to_string(),
            "queue" => "error: barca queue reads a barca server's run queue, and no server is \
                        given\n\
                        Run it in the project a `barca serve` is running in (without --local), \
                        pass --server URL, set BARCA_SERVER, or add `[server] url = \"...\"` to \
                        barca.toml (barca docs server-mode). This machine's own runs have no \
                        queue."
                .to_string(),
            command => format!(
                "error: barca {command} pauses a barca server's scheduled runs, and no server is \
                 given\n\
                 Run it in the project a `barca serve` is running in (without --local), pass \
                 --server URL, set BARCA_SERVER, or add `[server] url = \"...\"` to barca.toml \
                 (barca docs incidents). This machine runs nothing on a schedule."
            ),
        };
        CliError::from_prose(ErrorKind::Usage, text)
    }
}

/// The commands that take `--server` and `--local`, as a sentence lists them: `a, b and c`.
fn flag_commands() -> String {
    let mut names: Vec<&str> = Vec::new();
    for route in ROUTES.iter().filter(|r| r.takes_flags()) {
        if !names.contains(&route.name) {
            names.push(route.name);
        }
    }
    // The sentence ends with `gc and doctor`.
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.join(""),
    }
}

/// `--server` or `--local` on a command that always runs on this machine: a usage error that
/// says so, instead of clap's "unexpected argument". Read from argv, before it parses.
pub(crate) fn refuse_on_local_commands(args: &[String]) -> Option<CliError> {
    let sub = args.iter().skip(1).find(|a| !a.starts_with('-'))?;
    if Route::named(sub).is_some_and(Route::takes_flags) || sub.ends_with(".py") {
        return None;
    }
    let flag = args
        .iter()
        .take_while(|a| *a != "--")
        .find(|a| *a == "--local" || *a == "--server" || a.starts_with("--server="))?;
    let flag = flag.split('=').next().unwrap_or(flag);
    Some(CliError::from_prose(
        ErrorKind::Usage,
        format!(
            "error: `barca {sub}` always runs on this machine; it does not take {flag}\n\
             --server and --local apply to {} (barca docs server-mode).",
            flag_commands()
        ),
    ))
}

#[cfg(test)]
#[path = "route_tests.rs"]
mod tests;
