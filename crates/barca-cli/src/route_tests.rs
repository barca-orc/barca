use super::*;
use clap::{CommandFactory, Parser};

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

#[test]
fn local_commands_refuse_server_and_local() {
    for line in [
        "barca serve p.py --server",
        "barca plan --server=http://h",
        "barca plan p.py --local",
        "barca docs --server",
        "barca version --server",
    ] {
        let err = refuse_on_local_commands(&argv(line)).expect(line);
        assert_eq!(err.kind, ErrorKind::Usage, "{line}");
        assert!(err.error.contains("always runs on this machine"), "{line}");
    }
    for line in [
        "barca get x --server",
        "barca status --local",
        "barca sql q --local",
        "barca sql q --server",
        "barca p.py --server",
        "barca serve p.py",
    ] {
        assert!(refuse_on_local_commands(&argv(line)).is_none(), "{line}");
    }
}

/// The sentence is part of an error users read (and the contract's envelope): its wording is
/// the table's order.
#[test]
fn the_refusal_names_the_commands_in_the_tables_order() {
    assert_eq!(
        flag_commands(),
        "get, run, status, list, history, stats, logs, sql, queue, pause, resume, cancel, pin, \
         unpin, gc and doctor"
    );
    let err = refuse_on_local_commands(&argv("barca plan p.py --local")).unwrap();
    assert_eq!(
        err.render(false).trim_end(),
        "error: `barca plan` always runs on this machine; it does not take --local\n\
         --server and --local apply to get, run, status, list, history, stats, logs, sql, queue, \
         pause, resume, cancel, pin, unpin, gc and doctor (barca docs server-mode)."
    );
}

/// The table and clap agree on which commands exist and which take `--server` and `--local`.
#[test]
fn every_command_has_one_row_and_the_flags_it_says() {
    let root = Cli::command();
    let mut seen = Vec::new();
    for sub in root.get_subcommands() {
        let name = sub.get_name();
        if name == "help" {
            continue;
        }
        let route = Route::named(name)
            .unwrap_or_else(|| panic!("`barca {name}` has no row in route::ROUTES"));
        let has = |flag: &str| sub.get_arguments().any(|a| a.get_long() == Some(flag));
        assert_eq!(
            has("server"),
            route.takes_flags(),
            "`barca {name}`: --server in clap and in ROUTES disagree"
        );
        assert_eq!(
            has("local"),
            route.takes_flags(),
            "`barca {name}`: --local in clap and in ROUTES disagree"
        );
        seen.push(name.to_string());
    }
    for route in ROUTES {
        assert!(
            seen.iter().any(|name| name == route.name),
            "ROUTES names `{}`, which is not a command",
            route.name
        );
    }
    let own_rows = ROUTES.iter().filter(|r| r.when.is_none()).count();
    assert_eq!(own_rows, seen.len(), "a command has two rows of its own");
}

/// A row is coherent: only a command that takes the flags goes to a server, and a command
/// that cannot run here can at least be sent to one.
#[test]
fn rows_are_coherent() {
    for route in ROUTES {
        let name = match route.when {
            Some(when) => format!("{} {when}", route.name),
            None => route.name.to_string(),
        };
        if !route.takes_flags() {
            assert!(!route.project, "{name}: the socket without --local");
            assert_eq!(route.configured, Configured::No, "{name}");
            assert!(route.here, "{name}: runs nowhere");
        }
        if route.project {
            assert!(
                matches!(route.named, Named::Runs | Named::Views),
                "{name}: this project's server takes what --server takes"
            );
        }
        assert_eq!(
            route.named == Named::Views,
            route.configured == Configured::Views,
            "{name}"
        );
        assert_eq!(
            route.named == Named::Checks,
            route.configured == Configured::Checks,
            "{name}"
        );
        if !route.here {
            assert_eq!(route.named, Named::Runs, "{name}");
            assert!(route.project, "{name}");
            assert_eq!(route.configured, Configured::Answers, "{name}");
            let err = route.needs_a_server();
            assert_eq!(err.kind, ErrorKind::Usage, "{name}");
            assert!(err.render(false).contains("server"), "{name}");
        }
    }
}

/// The least a command line needs to parse, per row.
fn sample(route: &Route) -> Vec<String> {
    let rest = match (route.name, route.when) {
        ("history", Some(_)) => "--reconcile r1",
        ("stats" | "unpin" | "run" | "resume", _) => "node",
        ("cancel", _) => "r1",
        ("sql", _) => "select",
        ("tokens", _) => "bootstrap",
        ("state", _) => "backups",
        _ => "",
    };
    argv(&format!("barca {} {rest}", route.name))
}

/// `Route::of` finds the row of every parsed command, and `input::server_flags` hands
/// `dispatch` the flags of exactly the commands the table sends somewhere (`doctor` reads its
/// own).
#[test]
fn a_parsed_command_finds_its_row_and_its_flags() {
    for route in ROUTES {
        let line = sample(route);
        let cli = Cli::try_parse_from(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        let found = Route::of(&cli);
        assert!(std::ptr::eq(found, route), "{line:?} found {found:?}");
        assert_eq!(
            crate::input::server_flags(&cli).is_some(),
            matches!(route.named, Named::Runs | Named::Views | Named::Refused),
            "{line:?}"
        );
        // `--env` reaches the socket's path for every command that may use the socket.
        if route.project {
            let mut with_env = line.clone();
            with_env.extend(argv("--env staging"));
            let cli =
                Cli::try_parse_from(&with_env).unwrap_or_else(|e| panic!("{with_env:?}: {e}"));
            assert_eq!(crate::input::env_of(&cli), Some("staging"), "{with_env:?}");
        }
    }
}

fn names(routes: impl Iterator<Item = &'static Route>) -> Vec<String> {
    routes
        .map(|r| match r.when {
            Some(when) => format!("{} {when}", r.name),
            None => r.name.to_string(),
        })
        .collect()
}

/// The commands a table row of the manual names: every `` `name` `` in its first cell.
fn first_cell_commands(row: &str) -> Vec<String> {
    let cell = row.trim_start_matches('|').split('|').next().unwrap_or("");
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// `barca docs server-mode` states where each command runs twice: the table of what a server
/// running in this project takes, and the sentence listing the commands `--server` and
/// `--local` apply to. Both are held to the table.
#[test]
fn the_manual_agrees_with_the_table() {
    let manual = include_str!("../docs/server-mode.md");

    // "In the server's own project": the rows up to `state restore` go to the server; the last
    // row runs here.
    let section = manual
        .split("## In the server's own project: the project socket")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("server-mode.md has the project socket section");
    let rows: Vec<&str> = section.lines().filter(|l| l.starts_with("| `")).collect();
    let mut to_server: Vec<String> = Vec::new();
    let mut here: Vec<String> = Vec::new();
    for row in rows {
        let commands = first_cell_commands(row);
        if row.contains("run here, as without a server") {
            here.extend(commands);
        } else if !row.contains("refused") {
            to_server.extend(commands);
        }
    }
    to_server.sort();
    let mut want = names(ROUTES.iter().filter(|r| r.project));
    want.sort();
    assert_eq!(
        to_server, want,
        "server-mode.md, the project socket table: the commands that go to the server"
    );
    // `state backups` stands for `state` (its other two subcommands are refused beside a
    // server); `serve` and `version` are not in the table.
    let here: Vec<String> = here
        .into_iter()
        .map(|c| c.replace("state backups", "state"))
        .collect();
    for route in ROUTES.iter().filter(|r| !r.project) {
        let name = names(std::iter::once(route)).remove(0);
        if name == "serve" || name == "version" {
            continue;
        }
        assert!(
            here.contains(&name),
            "server-mode.md, the project socket table: `{name}` runs here and is not listed so"
        );
    }
    for name in &here {
        assert!(
            ROUTES
                .iter()
                .any(|r| !r.project && names(std::iter::once(r))[0] == *name),
            "server-mode.md lists `{name}` as running here; ROUTES sends it to the server"
        );
    }

    // "Which server": the sentence, in the table's order.
    let listed: Vec<String> = flag_commands()
        .replace(" and ", ", ")
        .split(", ")
        .map(|name| format!("`{name}`"))
        .collect();
    let (last, rest) = listed.split_last().unwrap();
    let sentence = format!(
        "`--server` and `--local` apply to {} and {last}.",
        rest.join(", ")
    );
    let flat = manual.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(&sentence),
        "server-mode.md (\"Which server\") should say: {sentence}"
    );
}

/// The table of where every command can run, as the engineering note and a PR print it:
/// `cargo test -p barca route::tests::print_the_table -- --nocapture`.
#[test]
fn print_the_table() {
    let mut out = String::from(
        "| Command | `--server` | This project's `barca serve` | A configured server | This machine |\n\
         |---|---|---|---|---|\n",
    );
    for route in ROUTES {
        let name = names(std::iter::once(route)).remove(0);
        let named = match route.named {
            Named::No => "usage error",
            Named::Runs => "runs there",
            Named::Views => "views from it, query here",
            Named::Checks => "checked too",
            Named::Refused => "usage error (records a run here)",
        };
        let configured = match route.configured {
            Configured::No => "not used",
            Configured::Ledger => "the run's ledger",
            Configured::Answers => "answers",
            Configured::Views => "views from it, query here",
            Configured::Checks => "checked too",
        };
        out.push_str(&format!(
            "| `{name}` | {named} | {} | {configured} | {} |\n",
            match (route.project, route.named) {
                (false, _) => "not used",
                (true, Named::Views) => named,
                (true, _) => "runs there",
            },
            if route.here {
                "runs here"
            } else {
                "usage error (needs a server)"
            },
        ));
    }
    println!("{out}");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/engineering/cli-layering.md");
    // An sdist of this crate alone has no engineering notes.
    let Ok(note) = std::fs::read_to_string(path) else {
        return;
    };
    assert!(
        note.contains(out.trim_end()),
        "docs/engineering/cli-layering.md: the table of where commands run is out of date. \
         Replace it with:\n\n{out}"
    );
}
