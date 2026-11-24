//! The CLI contract (`docs/contract.md`, shown by `barca docs contract`), enforced by snapshots.
//!
//! Two things are compared against checked-in files, so any change to the CLI surface fails
//! `cargo test -p barca` until the files are updated in the same change:
//!
//! - `snapshots/help/<command>.txt` and `snapshots/examples/<command>.txt`: the exact
//!   `--help` and `--examples` output of the top level and of every
//!   subcommand, rendered at a fixed width of 100 columns (what the binary prints: clap is built
//!   without `wrap_help`, so it never reads the terminal width).
//! - the generated blocks in `docs/contract.md`: the command table, every command's arguments
//!   with their stability, the exit codes and the experimental list, all rendered from the clap
//!   definition, [`EXPERIMENTAL`] and [`ErrorKind`]; and the command table of the repository's
//!   `README.md`, from the same definition.
//!
//! The site reference pages that are manual topics are compared the same way, in
//! `site_pages_tests.rs`; that every `BARCA_*` variable is documented, in `env_vars_tests.rs`.
//!
//! The JSON output schemas and the error envelope are snapshotted by
//! `python/tests/test_cli_contract.py`, which runs the real binary on a fixture pipeline.
//!
//! Update everything after a deliberate change:
//!
//! ```text
//! scripts/update-cli-snapshots.sh
//! ```
//!
//! or just this half with `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca`.

use super::Cli;
use crate::error::ErrorKind;
use crate::site_pages_tests::check_site_page;
use crate::snapshot::{UPDATE_HINT, check_file, crate_dir, first_difference, updating};
use clap::CommandFactory;

/// Help is rendered at this width. clap's default when `wrap_help` is off, so the snapshots are
/// byte-identical to what `barca <command> --help` prints, in any terminal.
const HELP_WIDTH: usize = 100;

/// Parts of the surface that may still change before 1.0 without a deprecation period, each with
/// the reason. Everything else in the generated tables is `stable`. A key is a command
/// (`serve`), or a command and one of its arguments as the table shows it (`get --output`,
/// `docs <TOPIC>`); `barca` is the top level.
pub const EXPERIMENTAL: &[(&str, &str)] = &[
    (
        "get --cached-only",
        "new in 0.47: completed-result reads and per-key outcome shapes may evolve",
    ),
    (
        "tokens",
        "new in 0.36 (RFC-0016): local bootstrap for opt-in project token access",
    ),
    (
        "tokens bootstrap",
        "prints a one-use bootstrap credential; token formats may evolve",
    ),
    (
        "pin",
        "new in 0.41 (barca docs pins): the JSON shapes, the version list and what a pin covers may change after field use",
    ),
    (
        "unpin",
        "new in 0.41 (barca docs pins): the JSON shape may change after field use",
    ),
    (
        "doctor",
        "new prerequisite report; check names and provider-specific data may evolve",
    ),
    (
        "gc",
        "new in 0.25 (RFC-0012): the eviction order, the low watermark and the JSON shape may change after field use",
    ),
    (
        "history <RUN_ID>",
        "new in 0.29.3: the run detail's step keys and human layout may change after field use",
    ),
    (
        "history --reconcile",
        "explicit operator recovery for stopped runs whose owner cannot be verified",
    ),
    (
        "history --status",
        "new in 0.29.3: the history filters' values and matching may change after field use",
    ),
    (
        "history --target",
        "new in 0.29.3: the history filters' values and matching may change after field use",
    ),
    (
        "history --triggered-by",
        "new in 0.29.3: the history filters' values and matching may change after field use",
    ),
    (
        "history --activity",
        "new in 0.44 (#991): the activity's keys, its window and its human table may change after field use",
    ),
    (
        "history --since",
        "new in 0.29.3: the history filters' values and matching may change after field use",
    ),
    (
        "history --not-triggered-by",
        "new in 0.44 (#969): the history filters' values and matching may change after field use",
    ),
    (
        "history --until",
        "new in 0.44 (#969): the history filters' values and matching may change after field use",
    ),
    (
        "history --search",
        "new in 0.44 (#969): what the text is looked for in, and how, may change after field use",
    ),
    (
        "list --groups",
        "Organizational group hierarchy and metadata shape are experimental",
    ),
    (
        "plan",
        "prints the planner's internal phase/stream layout, which changes with scheduling work",
    ),
    (
        "sql",
        "new in 0.13: the view naming and the JSON result shape may change after field use",
    ),
    (
        "cancel",
        "Cancel a queued or running server run; the response follows DELETE /runs/{id}",
    ),
    (
        "queue",
        "new in 0.29.4: the run queue's JSON shape, its table and the pause's scope may change after field use",
    ),
    (
        "pause",
        "new in 0.41: pausing assets for scheduled runs; the JSON shape, the table and what a pause covers may change after field use",
    ),
    (
        "resume",
        "new in 0.41: resuming paused assets; the JSON shape and the table may change after field use",
    ),
    (
        "get --allow-paused",
        "new in 0.41: runs a paused target by hand; which requests need it may change after field use",
    ),
    (
        "run --allow-paused",
        "new in 0.41: runs a paused target by hand; which requests need it may change after field use",
    ),
    (
        "get --queue",
        "new in 0.46.0: a place in a server's run queue; the values, what `last` is ordered with and where it shows may change after field use",
    ),
    (
        "run --queue",
        "new in 0.46.0: a place in a server's run queue; the values, what `last` is ordered with and where it shows may change after field use",
    ),
    (
        "serve",
        "the HTTP API and scheduler are young: opt-in project tokens, routes may change; the older trigger routes remain deprecated aliases for Python Client compatibility, with no scheduled removal; a removal migrates Client first and lands in a minor release; runs share identical in-flight asset steps (new in 0.39.4, #654), with cancellation survival, a durable handoff and a decide-to-lease race still open",
    ),
    (
        "serve --capacity",
        "new in 0.31.0 (#628): the admission order across runs and the shape of capacity in /queue and /health may change after field use",
    ),
    (
        "state reset",
        "new in 0.28 (#508): the JSON shape and the name of the moved-aside file may change after field use",
    ),
    (
        "get --expires",
        "new with -o url (signed links to results in a cloud store); the link format is the cloud's",
    ),
    (
        "status --sample",
        "sample rows come from a Python helper whose output may grow",
    ),
    (
        "status --problems",
        "orders problems by distance from fresh; work already in hand is omitted unless its cause is requested",
    ),
    ("status --health", "experimental health inspection (#1043)"),
    ("status --root", "experimental health inspection (#1043)"),
    ("status --cause", "experimental health inspection (#1043)"),
    ("status --now", "experimental health inspection (#1043)"),
    (
        "status --rollup",
        "new in 0.44.0 (specs/behavior/health.md): counts by health and by cause, the freshness gap and the ranked roots with their actions; the wording of a root's detail and of the printed list may change after field use",
    ),
    (
        "status --keys",
        "new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use",
    ),
    (
        "status --grid",
        "new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use",
    ),
    (
        "status --key",
        "new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use",
    ),
    (
        "status --needs-run",
        "new in 0.44.1 (#1036), with --keys: the keys a run computes; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use",
    ),
    (
        "status --value",
        "new in 0.44.1 (#1036), with --keys: a dimension's values; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use",
    ),
    (
        "status --range",
        "new in 0.44.1 (#1036), with --keys: a stretch of a dimension; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use",
    ),
    (
        "status --error",
        "new in 0.44.1 (#1036), with --keys: one error group's failed keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use",
    ),
    (
        "status --match",
        "new in 0.44.1 (#1036), with --keys: text over the key; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use",
    ),
    (
        "status --sort",
        "new in 0.44.1 (#1036), with --keys: the order of the keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use",
    ),
    (
        "status --offset",
        "new in 0.44.1 (#1036), with --keys: the page of keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use",
    ),
    (
        "status --overview",
        "new in 0.44.1 (#1036): a partitioned node's keys in one picture (counts, issues and the grid's rows); the JSON shape, the wording of an issue and the bound on the rows may change after field use",
    ),
    (
        "get --batch",
        "new in 0.32.0 (RFC-0013): batched partitions; the override's forms and the dry run's batch object may change after field use",
    ),
    (
        "run --batch",
        "new in 0.32.0 (RFC-0013): batched partitions; the override's forms may change after field use",
    ),
    (
        "get --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "get --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "run --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "run --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "status --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "status --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "list --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "list --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "history --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "history --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "stats --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "stats --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "logs --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "logs --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "sql --server",
        "new in 0.29.3: the views and barca's tables resolved from a server (GET /sql/views, GET /materializations) may change after field use; new in 0.44.0: a query naming only barca's tables is answered by the server (POST /sql/query), and which queries are sent there, its limits (5 seconds, 10000 rows and 4MB an answer, four queries at once) and its errors may change",
    ),
    (
        "sql --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock)",
    ),
    (
        "logs --follow",
        "new in 0.29.2: how the end of a followed run is reported may change after field use",
    ),
];

/// Compatibility spellings that keep working but have a canonical replacement, each with the
/// replacement. Keyed like `EXPERIMENTAL`. One is removed only in a minor release, at least one
/// minor after a stderr notice naming its replacement first ships (contract.md, "Deprecated
/// compat flags").
pub const DEPRECATED: &[(&str, &str)] = &[(
    "run --output",
    "-o json|pretty are the older spellings of --json / --pretty; -o value has no replacement yet and keeps working",
)];

/// What `barca <path...> --help` prints.
fn render_help(path: &[&str]) -> String {
    render_help_flag(path, "--help")
}

fn render_help_flag(path: &[&str], flag: &str) -> String {
    let mut argv = vec!["barca"];
    argv.extend_from_slice(path);
    argv.push(flag);
    let err = match Cli::command()
        .term_width(HELP_WIDTH)
        .try_get_matches_from(argv)
    {
        Ok(_) => panic!("--help must not parse as a command"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    normalize(&err.render().to_string())
}

/// Trailing whitespace stripped from every line and exactly one final newline, so the
/// repository's whitespace hooks (prek.toml) never rewrite a snapshot. clap pads some blank
/// lines with spaces; nobody parses those.
fn normalize(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_end().to_string() + "\n"
}

/// The top level and every subcommand (and theirs: `barca state backups`), as (snapshot name,
/// argv path).
fn help_targets() -> Vec<(String, Vec<String>)> {
    let mut out = vec![("barca".to_string(), Vec::new())];
    for sub in Cli::command().get_subcommands() {
        let name = sub.get_name().to_string();
        out.push((format!("barca-{name}"), vec![name.clone()]));
        for nested in sub.get_subcommands() {
            let inner = nested.get_name().to_string();
            out.push((format!("barca-{name}-{inner}"), vec![name.clone(), inner]));
        }
    }
    out
}

/// Every command a user types, by its words after `barca` (`get`, `state backups`), with its
/// definition: the subcommands and theirs.
fn every_command(root: &clap::Command) -> Vec<(String, &clap::Command)> {
    let mut out = Vec::new();
    for sub in root.get_subcommands() {
        out.push((sub.get_name().to_string(), sub));
        // clap's generated `help` subcommands are not commands of their own.
        if sub.get_name() == "help" {
            continue;
        }
        for nested in sub.get_subcommands().filter(|n| n.get_name() != "help") {
            out.push((format!("{} {}", sub.get_name(), nested.get_name()), nested));
        }
    }
    out
}

#[test]
fn help_output_matches_the_snapshots() {
    let dir = crate_dir().join("snapshots/help");
    let mut expected_files = Vec::new();
    for (name, path) in help_targets() {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        let file = format!("{name}.txt");
        check_file(&dir.join(&file), &render_help(&path));
        check_file(
            &crate_dir().join("snapshots/examples").join(&file),
            &render_help_flag(&path, "--examples"),
        );
        expected_files.push(file);
    }
    // Removing a command removes both views of its help.
    for kind in ["help", "examples"] {
        let dir = crate_dir().join("snapshots").join(kind);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let file = entry.unwrap().file_name().to_string_lossy().to_string();
            if !expected_files.contains(&file) {
                if updating() {
                    std::fs::remove_file(dir.join(&file)).unwrap();
                } else {
                    panic!("snapshots/{kind}/{file} has no matching command.\n\n{UPDATE_HINT}");
                }
            }
        }
    }
}

#[test]
fn help_snapshots_do_not_depend_on_the_terminal() {
    // Render the way the binary does (no explicit width) in a "40-column terminal". clap reads
    // COLUMNS only with its `wrap_help` feature; if someone enables it, `--help` would follow
    // the terminal and the snapshots (taken at HELP_WIDTH) would no longer be what users see.
    // SAFETY: no other test reads or writes COLUMNS.
    unsafe { std::env::set_var("COLUMNS", "40") };
    let as_the_binary_prints = Cli::command()
        .try_get_matches_from(["barca", "get", "--help"])
        .unwrap_err()
        .render()
        .to_string();
    unsafe { std::env::remove_var("COLUMNS") };
    assert_eq!(
        normalize(&as_the_binary_prints),
        render_help(&["get"]),
        "--help no longer renders at a fixed {HELP_WIDTH} columns (is clap's wrap_help on?)"
    );
}

// ─── Generated blocks in docs/contract.md ────────────────────────────────────

fn experimental(key: &str) -> Option<&'static str> {
    EXPERIMENTAL
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, why)| *why)
}

/// An argument of an experimental command (`state reset --yes` of `state reset`) is
/// experimental with it.
fn stability(key: &str) -> String {
    if let Some((_, why)) = DEPRECATED.iter().find(|(k, _)| *k == key) {
        return format!("deprecated: {why}");
    }
    if let Some(why) = experimental(key) {
        return format!("experimental: {why}");
    }
    let mut rest = key;
    while let Some((command, _)) = rest.rsplit_once(' ') {
        if experimental(command).is_some() {
            return "experimental (with the command)".to_string();
        }
        rest = command;
    }
    "stable".to_string()
}

/// The CLI definition as the binary sees it, with the generated `--help` / `--version` flags.
fn built() -> clap::Command {
    let mut cmd = Cli::command();
    cmd.build();
    cmd
}

/// Escape a table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn one_line(s: Option<&clap::builder::StyledStr>) -> String {
    let s = s.map(|s| s.to_string()).unwrap_or_default();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The name an argument has in the tables: `<ARGS>...`, `-o, --output`, `--json`.
fn arg_label(arg: &clap::Arg) -> String {
    if arg.is_positional() {
        let name = arg
            .get_value_names()
            .and_then(|n| n.first().map(|s| s.to_string()))
            .unwrap_or_else(|| arg.get_id().to_string().to_uppercase());
        let many = arg.get_num_args().is_some_and(|r| r.max_values() > 1);
        return format!("<{name}>{}", if many { "..." } else { "" });
    }
    let mut parts = Vec::new();
    if let Some(s) = arg.get_short() {
        parts.push(format!("-{s}"));
    }
    if let Some(l) = arg.get_long() {
        parts.push(format!("--{l}"));
    }
    parts.join(", ")
}

/// The key `EXPERIMENTAL` uses for an argument: its long flag, or its positional label.
fn arg_key(arg: &clap::Arg) -> String {
    match arg.get_long() {
        Some(l) => format!("--{l}"),
        None => arg_label(arg),
    }
}

fn takes_value(arg: &clap::Arg) -> bool {
    matches!(
        arg.get_action(),
        clap::ArgAction::Set | clap::ArgAction::Append
    )
}

fn arg_value(arg: &clap::Arg) -> String {
    if arg.is_positional() || !takes_value(arg) {
        return "-".to_string();
    }
    let possible: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|p| !p.is_hide_set())
        .map(|p| p.get_name().to_string())
        .collect();
    let comma = arg.get_value_delimiter() == Some(',');
    match (possible.is_empty(), comma) {
        (false, true) => format!("comma-separated: `{}`", possible.join("`, `")),
        (false, false) => format!("`{}`", possible.join("|")),
        (true, true) => "comma-separated names".to_string(),
        (true, false) => {
            let name = arg
                .get_value_names()
                .and_then(|n| n.first().map(|s| s.to_string()))
                .unwrap_or_else(|| arg.get_id().to_string().to_uppercase());
            format!("`{name}`")
        }
    }
}

fn arg_notes(arg: &clap::Arg) -> String {
    let mut notes = Vec::new();
    if arg.is_required_set() {
        notes.push("required".to_string());
    }
    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().to_string())
        .collect();
    if !defaults.is_empty() {
        notes.push(format!("default `{}`", defaults.join(",")));
    }
    if arg.is_hide_set() {
        notes.push("hidden from `--help`".to_string());
    }
    if let Some(aliases) = arg.get_all_aliases() {
        for a in aliases {
            notes.push(format!("alias `--{a}`"));
        }
    }
    if notes.is_empty() {
        "-".to_string()
    } else {
        notes.join("; ")
    }
}

fn is_implicit(arg: &clap::Arg) -> bool {
    matches!(arg.get_id().as_str(), "help")
}

/// The `commands` block: one row per subcommand.
fn commands_block() -> String {
    let root = built();
    let mut out =
        String::from("| Command | Arguments | Stability | Purpose |\n|---|---|---|---|\n");
    for (name, sub) in every_command(&root) {
        let name = name.as_str();
        let positionals: Vec<String> = sub
            .get_arguments()
            .filter(|a| a.is_positional())
            .map(|a| {
                let l = arg_label(a);
                if a.is_required_set() {
                    l
                } else {
                    format!("[{l}]")
                }
            })
            .collect();
        out.push_str(&format!(
            "| `barca {name}` | {} | {} | {} |\n",
            if positionals.is_empty() {
                "-".to_string()
            } else {
                format!("`{}`", positionals.join(" "))
            },
            cell(&stability(name)),
            cell(&one_line(sub.get_about())),
        ));
    }
    out
}

/// The `flags` block: every argument of the top level and of each subcommand.
fn flags_block() -> String {
    let root = built();
    let mut out = String::new();
    let mut table = |title: &str, key: &str, cmd: &clap::Command| {
        out.push_str(&format!(
            "#### {title}\n\n| Argument | Value | Notes | Stability | Description |\n|---|---|---|---|---|\n"
        ));
        let mut any = false;
        for arg in cmd.get_arguments().filter(|a| !is_implicit(a)) {
            any = true;
            let k = format!("{key} {}", arg_key(arg));
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                arg_label(arg),
                cell(&arg_value(arg)),
                cell(&arg_notes(arg)),
                cell(&stability(&k)),
                cell(&one_line(arg.get_help()))
            ));
        }
        if !any {
            out.push_str("| - | - | - | - | no arguments besides `-h, --help` |\n");
        }
        out.push('\n');
    };
    table("barca", "barca", &root);
    for (name, sub) in every_command(&root) {
        table(&format!("barca {name}"), &name, sub);
    }
    out.trim_end().to_string() + "\n"
}

/// The `exit-codes` block, from [`ErrorKind`].
fn exit_codes_block() -> String {
    let mut out =
        String::from("| Code | `kind` | Stability |\n|---|---|---|\n| 0 | (success) | stable |\n");
    for kind in [
        ErrorKind::StepFailed,
        ErrorKind::CacheMiss,
        ErrorKind::Usage,
        ErrorKind::Infra,
        ErrorKind::Cancelled,
    ] {
        out.push_str(&format!(
            "| {} | `{}` | stable |\n",
            kind.exit_code(),
            kind.as_str()
        ));
    }
    out
}

/// The `experimental` block: every key of [`EXPERIMENTAL`] with its reason, so the contract's
/// list of experimental commands and arguments cannot drift from the stability column.
fn experimental_block() -> String {
    let mut out = String::from("| Command or argument | Why it is experimental |\n|---|---|\n");
    for (key, why) in EXPERIMENTAL {
        out.push_str(&format!("| `barca {}` | {} |\n", cell(key), cell(why)));
    }
    out
}

/// The README's command table: one row per command a user types (parents with subcommands and
/// clap's `help` left out), its positionals and its one-line purpose, with experimental ones
/// marked. The same clap definition as the contract, so the README cannot describe a command
/// the binary does not have or call a stable one experimental.
fn readme_commands_block() -> String {
    let root = built();
    let mut out = String::from("| Command | What it does |\n|---|---|\n");
    for (name, sub) in every_command(&root) {
        if name == "help" || sub.get_subcommands().any(|n| n.get_name() != "help") {
            continue;
        }
        let positionals: Vec<String> = sub
            .get_arguments()
            .filter(|a| a.is_positional())
            .map(|a| {
                let l = arg_label(a);
                if a.is_required_set() {
                    l
                } else {
                    format!("[{l}]")
                }
            })
            .collect();
        let command = if positionals.is_empty() {
            format!("barca {name}")
        } else {
            format!("barca {name} {}", positionals.join(" "))
        };
        let mut purpose = one_line(sub.get_about());
        if stability(&name).starts_with("experimental") && !purpose.contains("(experimental)") {
            purpose.push_str(" (experimental)");
        }
        out.push_str(&format!("| `{}` | {} |\n", cell(&command), cell(&purpose)));
    }
    out
}

fn begin_marker(name: &str) -> String {
    format!("<!-- BEGIN GENERATED {name} -->")
}

fn end_marker(name: &str) -> String {
    format!("<!-- END GENERATED {name} -->")
}

/// The text between a block's markers, without the newlines that frame it.
pub fn block<'a>(doc: &'a str, name: &str) -> Option<&'a str> {
    let begin = begin_marker(name);
    let start = doc.find(&begin)? + begin.len();
    let end = doc[start..].find(&end_marker(name))? + start;
    Some(doc[start..end].trim_matches('\n'))
}

pub fn replace_block(doc: &str, name: &str, content: &str) -> Option<String> {
    let begin = begin_marker(name);
    let start = doc.find(&begin)? + begin.len();
    let end = doc[start..].find(&end_marker(name))? + start;
    Some(format!(
        "{}\n{}\n{}",
        &doc[..start],
        content.trim_matches('\n'),
        &doc[end..]
    ))
}

#[test]
fn generated_blocks_in_the_contract_match_the_cli() {
    let path = crate_dir().join("docs/contract.md");
    let mut doc = std::fs::read_to_string(&path).expect("docs/contract.md");
    let blocks = [
        ("commands", commands_block()),
        ("flags", flags_block()),
        ("exit-codes", exit_codes_block()),
        ("experimental", experimental_block()),
    ];
    if updating() {
        for (name, content) in &blocks {
            doc = replace_block(&doc, name, content)
                .unwrap_or_else(|| panic!("docs/contract.md has no `{name}` block markers"));
        }
        std::fs::write(&path, &doc).unwrap();
        // In this test, not its own: it must read contract.md after the write above.
        check_site_page(&doc);
        return;
    }
    for (name, content) in &blocks {
        let actual = block(&doc, name)
            .unwrap_or_else(|| panic!("docs/contract.md has no `{name}` block markers"));
        if actual != content.trim_matches('\n') {
            panic!(
                "the `{name}` block of docs/contract.md is out of date.\n\n{}\n\n{UPDATE_HINT}",
                first_difference(actual, content)
            );
        }
    }
    check_site_page(&doc);
}

/// The README's command table is generated from the same clap definition as the contract.
/// Skipped outside the repository (an sdist build has no README at the workspace root).
#[test]
fn readme_commands_table_matches_the_cli() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    let Ok(doc) = std::fs::read_to_string(&path) else {
        return;
    };
    let content = readme_commands_block();
    if updating() {
        let doc = replace_block(&doc, "commands", &content)
            .expect("README.md has no `commands` block markers");
        std::fs::write(&path, doc).unwrap();
        return;
    }
    let actual = block(&doc, "commands").expect("README.md has no `commands` block markers");
    if actual != content.trim_matches('\n') {
        panic!(
            "the `commands` block of README.md is out of date.\n\n{}\n\n{UPDATE_HINT}",
            first_difference(actual, &content)
        );
    }
}

#[test]
fn every_experimental_key_names_a_real_command_or_argument() {
    let root = built();
    for (key, why) in EXPERIMENTAL.iter().chain(DEPRECATED) {
        assert!(!why.is_empty(), "`{key}` needs a reason");
        assert!(
            !(EXPERIMENTAL.iter().any(|(k, _)| k == key)
                && DEPRECATED.iter().any(|(k, _)| k == key)),
            "`{key}` is both experimental and deprecated"
        );
        let (cmd, arg) = key.split_once(' ').unwrap_or((key, ""));
        let command = if cmd == "barca" {
            &root
        } else {
            root.find_subcommand(cmd)
                .unwrap_or_else(|| panic!("EXPERIMENTAL names unknown command `{cmd}`"))
        };
        if !arg.is_empty() {
            // `state reset`: a nested command.
            let nested = command.find_subcommand(arg).is_some();
            assert!(
                nested || command.get_arguments().any(|a| arg_key(a) == arg),
                "EXPERIMENTAL names unknown argument `{arg}` of `{cmd}`"
            );
        }
    }
}

#[test]
fn block_markers_round_trip() {
    let doc = "a\n<!-- BEGIN GENERATED x -->\nold\n<!-- END GENERATED x -->\nb\n";
    assert_eq!(block(doc, "x"), Some("old"));
    let new = replace_block(doc, "x", "new\n").unwrap();
    assert_eq!(
        new,
        "a\n<!-- BEGIN GENERATED x -->\nnew\n<!-- END GENERATED x -->\nb\n"
    );
    assert_eq!(block(&new, "x"), Some("new"));
    assert!(block(doc, "y").is_none());
}
