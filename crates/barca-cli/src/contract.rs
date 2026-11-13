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
//! - the site reference pages that are manual topics (`SITE_PAGES`): each is its topic with
//!   site front matter and `barca docs <topic>` references turned into links.
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
use clap::CommandFactory;
use std::path::{Path, PathBuf};

/// Help is rendered at this width. clap's default when `wrap_help` is off, so the snapshots are
/// byte-identical to what `barca <command> --help` prints, in any terminal.
const HELP_WIDTH: usize = 100;

/// Parts of the surface that may still change before 1.0 without a deprecation period, each with
/// the reason. Everything else in the generated tables is `stable`. A key is a command
/// (`serve`), or a command and one of its arguments as the table shows it (`get --output`,
/// `docs <TOPIC>`); `barca` is the top level.
pub const EXPERIMENTAL: &[(&str, &str)] = &[
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
        "history --since",
        "new in 0.29.3: the history filters' values and matching may change after field use",
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
        "new in 0.29.3: what counts as needing attention may grow (it is every state but cached and always_runs, a failed task or sensor, a paused or failing schedule)",
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
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
    ),
    (
        "run --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "run --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
    ),
    (
        "status --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "status --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
    ),
    (
        "list --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "list --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
    ),
    (
        "history --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "history --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
    ),
    (
        "stats --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "stats --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
    ),
    (
        "logs --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "logs --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
    ),
    (
        "sql --server",
        "new in 0.29.3: the views and barca's tables resolved from a server (GET /sql/views, GET /materializations) may change after field use",
    ),
    (
        "sql --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql)",
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

fn updating() -> bool {
    std::env::var("BARCA_UPDATE_SNAPSHOTS").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const UPDATE_HINT: &str = "If this change to the CLI surface is deliberate, update the snapshots \
     and the contract in the same PR: scripts/update-cli-snapshots.sh (or \
     BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca), then review the diff. Pre-1.0 a breaking \
     change needs a minor bump and a \"Breaking\" line in the release notes (barca docs contract).";

/// Compare `actual` with the file at `path`, or write it when updating.
fn check_file(path: &Path, actual: &str) {
    if updating() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|_| {
        panic!("missing snapshot {}\n\n{UPDATE_HINT}", path.display());
    });
    if expected != actual {
        panic!(
            "{} is out of date.\n\n{}\n\n{UPDATE_HINT}",
            path.display(),
            first_difference(&expected, actual)
        );
    }
}

fn first_difference(expected: &str, actual: &str) -> String {
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    for i in 0..e.len().max(a.len()) {
        let (el, al) = (e.get(i).copied(), a.get(i).copied());
        if el != al {
            return format!(
                "first difference at line {}:\n  snapshot: {}\n  now:      {}",
                i + 1,
                el.unwrap_or("<end of file>"),
                al.unwrap_or("<end of file>")
            );
        }
    }
    "the files differ only in trailing whitespace or line endings".to_string()
}

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
    let path = crate_dir().join("../../README.md");
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

// ─── The site copies ─────────────────────────────────────────────────────────

/// A site reference page generated from a manual topic. The site renders the topic without
/// its `# ` title (Starlight renders the frontmatter title) and with `barca docs <topic>`
/// references that have a page of their own turned into links.
struct SitePage {
    /// The manual topic, as `barca docs <topic>` names it.
    topic: &'static str,
    /// Its file under `docs/`.
    file: &'static str,
    /// The page's slug under `site/src/content/docs/`.
    slug: &'static str,
    title: &'static str,
    description: &'static str,
}

/// Every site page that is a manual topic. A site page that explains something the manual does
/// not (`reference/cli`, `reference/config`, `scheduling`, ...) is written by hand and is not here.
const SITE_PAGES: &[SitePage] = &[
    SitePage {
        topic: "contract",
        file: "contract.md",
        slug: "reference/cli-contract",
        title: "CLI Contract",
        description: "Every barca command, flag, environment variable, exit code and JSON \
                      schema, marked stable or experimental, and the policy for changing them.",
    },
    SitePage {
        topic: "discovery",
        file: "discovery.md",
        slug: "reference/discovery",
        title: "Discovery",
        description: "Which files make up a barca project: the root, tree walks, [discovery] in \
                      barca.toml, and node ids.",
    },
    SitePage {
        topic: "doctor",
        file: "doctor.md",
        slug: "reference/cli/doctor",
        title: "barca doctor",
        description: "Check project prerequisites, stores and credential identity without \
                      running steps.",
    },
    SitePage {
        topic: "duckdb",
        file: "duckdb.md",
        slug: "reference/duckdb",
        title: "DuckDB Versions",
        description: "duckdb 1.x and 2.x: what a pipeline must change for duckdb 2, and why a \
                      database file duckdb 2 writes cannot be opened by 1.x.",
    },
    SitePage {
        topic: "groups",
        file: "groups.md",
        slug: "reference/groups",
        title: "Groups",
        description: "Organize pipeline nodes in the UI and CLI without changing execution or \
                      caching.",
    },
    SitePage {
        topic: "remote",
        file: "remote.md",
        slug: "reference/remote-storage",
        title: "Remote Storage",
        description: "Keep results and backups of the run history in S3, GCS, Azure or \
                      Cloudflare R2, configured with environment variables; restore the history \
                      with one command.",
    },
    SitePage {
        topic: "server-mode",
        file: "server-mode.md",
        slug: "reference/server-mode",
        title: "Server Mode: a Shared Ledger and --server",
        description: "One barca serve as the team's ledger: laptops read its cache hits and post \
                      their runs, and --server runs a command on it.",
    },
    SitePage {
        topic: "assets",
        file: "assets.md",
        slug: "reference/assets",
        title: "Assets, Sensors and Inputs",
        description: "@asset and @sensor: inputs, freshness, retries, environment variables, \
                      heavy steps and how a node is identified.",
    },
    SitePage {
        topic: "tasks",
        file: "tasks.md",
        slug: "reference/tasks",
        title: "Tasks",
        description: "@task and barca run: steps that always run, ordering-only dependencies, \
                      fan-out from inside a task, and what is recorded.",
    },
    SitePage {
        topic: "partitions",
        file: "partitions.md",
        slug: "reference/partitions",
        title: "Partitions",
        description: "Fan-out over keys and fan-in with collect: updating only what changed, \
                      batched calls, failed keys and selecting keys.",
    },
    SitePage {
        topic: "cache",
        file: "cache.md",
        slug: "reference/cache",
        title: "The Cache",
        description: "What the run hash covers, artifacts, --refresh and --refresh-all, early \
                      cutoff, dry runs, environments and what a run reports.",
    },
    SitePage {
        topic: "refresh",
        file: "refresh.md",
        slug: "reference/refresh",
        title: "Forcing a Recompute",
        description: "barca get --refresh, --refresh-all and --no-cascade: what reruns, refreshing \
                      selected partition keys, Manual upstreams, and --dry-run.",
    },
    SitePage {
        topic: "external-data",
        file: "external-data.md",
        slug: "reference/external-data",
        title: "External Data and Early Cutoff",
        description: "Rerun a step when a file or table changes in place by reading it through a \
                      @sensor, and keep what is downstream cached with @asset(cutoff=True).",
    },
    SitePage {
        topic: "artifacts",
        file: "artifacts.md",
        slug: "reference/artifacts",
        title: "Run Reports and Missing Artifacts",
        description: "What barca get and barca run report for each step, recompute byte \
                      checks, and what happens when a cached result's file is gone.",
    },
    SitePage {
        topic: "hashing",
        file: "hashing.md",
        slug: "reference/hashing",
        title: "The Run Hash",
        description: "Which project code a step's run hash follows from its function, which \
                      decorator arguments count, and what barca does not follow.",
    },
    SitePage {
        topic: "batched-partitions",
        file: "batched-partitions.md",
        slug: "reference/batched-partitions",
        title: "Batched Partitions",
        description: "batch=: one call of a partitioned step computes many keys; grouping, \
                      the batch function, what stays the same, calls and known limits.",
    },
    SitePage {
        topic: "disk",
        file: "disk.md",
        slug: "reference/disk",
        title: "Disk Use",
        description: "The size limit for .barca/, what barca gc evicts and in which order, and \
                      how the metadata database keeps to its share.",
    },
    SitePage {
        topic: "layout",
        file: "layout.md",
        slug: "reference/layout",
        title: "Where Things Live",
        description: "The project root, the .barca/ directory, concurrent runs, killed runs and \
                      upgrading.",
    },
    SitePage {
        topic: "backup",
        file: "backup.md",
        slug: "reference/backup",
        title: "Metadata Backups",
        description: "Backups of the metadata database in the store, barca state backups and \
                      barca state restore, and how a server's backups differ.",
    },
    SitePage {
        topic: "serve-ops",
        file: "serve-ops.md",
        slug: "reference/serve-ops",
        title: "Operating barca serve",
        description: "Operating barca serve: the run queue, stopping and restarting without \
                      losing work, the stop grace a supervisor needs, a shared capacity and the \
                      run timeout.",
    },
    SitePage {
        topic: "incidents",
        file: "incidents.md",
        slug: "reference/cli/pause",
        title: "barca pause and resume",
        description: "Containing an incident: barca pause and barca resume, and what scheduled \
                      runs and requests do with a paused asset.",
    },
    SitePage {
        topic: "types",
        file: "types.md",
        slug: "reference/types",
        title: "Output Formats and Types",
        description: "json, pickle, parquet and CSV artifacts, and how annotations pick readers \
                      (pandas, polars, pyarrow, duckdb).",
    },
    SitePage {
        topic: "sinks",
        file: "sinks.md",
        slug: "reference/sinks",
        title: "Sinks",
        description: "@sink exports results to local or remote paths, as one file or a \
                      partitioned dataset.",
    },
    SitePage {
        topic: "status",
        file: "status.md",
        slug: "reference/cli/status",
        title: "barca status",
        description: "barca status: every node's cache state and why, its last run, its \
                      artifact's shape, --problems and --state.",
    },
    SitePage {
        topic: "status-fields",
        file: "status-fields.md",
        slug: "reference/cli/status-fields",
        title: "barca status: Fields and JSON",
        description: "What barca status reports for each node beyond its state (last materialization, \
                      artifact shape, run timing, trigger), its JSON document, limits and disk usage.",
    },
    SitePage {
        topic: "pins",
        file: "pins.md",
        slug: "reference/cli/pin",
        title: "barca pin and unpin",
        description: "barca pin and unpin: roll an asset back to an earlier materialization, \
                      held for every run until unpinned.",
    },
    SitePage {
        topic: "logs",
        file: "logs.md",
        slug: "reference/cli/logs",
        title: "barca logs",
        description: "barca logs: a run's recorded step stdout and stderr, following a live \
                      run, and how lines are kept.",
    },
    SitePage {
        topic: "agents",
        file: "agents.md",
        slug: "reference/agents",
        title: "Scripts and AI Agents",
        description: "The output contract for scripts and AI agents: JSON on stdout, the error \
                      envelope, exit codes, bounded output and a safe editing loop.",
    },
    SitePage {
        topic: "sql",
        file: "sql.md",
        slug: "reference/cli/sql",
        title: "barca sql",
        description: "barca sql: query cached results with DuckDB while debugging, without \
                      writing a step.",
    },
    SitePage {
        topic: "telemetry",
        file: "telemetry.md",
        slug: "reference/telemetry",
        title: "Telemetry (Datadog)",
        description: "Report every step and every run to Datadog as a trace of its own, and \
                      barca serve as DogStatsD metrics and JSON logs.",
    },
    SitePage {
        topic: "slack",
        file: "slack.md",
        slug: "reference/slack",
        title: "Slack Alerts",
        description: "Post to Slack when a job starts failing, recovers, or is still failing a \
                      day later, with BARCA_SLACK_WEBHOOK_URL.",
    },
];

/// Manual topics with a site page that is not generated from them, for links: topic, route,
/// page title.
const SITE_ROUTES: &[(&str, &str, &str)] = &[
    ("scheduling", "/scheduling/", "Scheduling With Cron"),
    ("skill", "/reference/agent-skill/", "Agent Skill"),
];

/// A topic's site page: its route and title.
fn site_route(topic: &str) -> Option<(String, &'static str)> {
    SITE_PAGES
        .iter()
        .find(|p| p.topic == topic)
        .map(|p| (format!("/{}/", p.slug), p.title))
        .or_else(|| {
            SITE_ROUTES
                .iter()
                .find(|(t, _, _)| *t == topic)
                .map(|(_, r, title)| (r.to_string(), *title))
        })
}

/// The anchor Starlight gives a heading (github-slugger): lowercase, punctuation dropped,
/// spaces to hyphens.
fn heading_anchor(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The anchor of the heading in `topic` that `section` names (a heading that starts with it,
/// ignoring case and backticks), if there is one.
fn section_anchor(topic: &str, section: &str) -> Option<String> {
    // A page not generated from the topic has its own headings.
    SITE_PAGES.iter().find(|p| p.topic == topic)?;
    let body = crate::docs::find(topic)?.body;
    let want = section.replace('`', "").to_lowercase();
    let mut fenced = false;
    body.lines().find_map(|line| {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let heading = line
            .strip_prefix("## ")
            .or_else(|| line.strip_prefix("### "))?;
        let text = heading.replace('`', "");
        (!fenced && text.to_lowercase().starts_with(&want)).then(|| heading_anchor(&text))
    })
}

/// A quoted section name right after a topic reference: `, "Section"` or ` ("Section")`.
/// Returns the section and how many bytes of `after` it spans.
fn quoted_section(after: &str) -> Option<(String, usize)> {
    let (open, close) = if after.starts_with(", \"") {
        (3, "\"")
    } else if after.starts_with(" (\"") {
        (3, "\")")
    } else {
        return None;
    };
    let end = after[open..].find('"')?;
    if !after[open + end..].starts_with(close) {
        return None;
    }
    let section = after[open..open + end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some((section, open + end + close.len()))
}

/// Turn `` `barca docs <topic>` `` into a link to the topic's site page, titled by the page,
/// outside code blocks. A quoted section name after it (`, "Section"`) joins the link, which
/// then goes to that heading. Topics without a site page stay as they are: the reader runs
/// the command.
fn link_topics(body: &str) -> String {
    const OPEN: &str = "`barca docs ";
    let mut out = String::new();
    // Prose between fences, so a quoted section name may wrap onto the next line.
    let mut chunks: Vec<(bool, String)> = vec![(false, String::new())];
    let mut fenced = false;
    for line in body.split_inclusive('\n') {
        let fence = line.trim_start().starts_with("```");
        if fence && !fenced {
            fenced = true;
            chunks.push((true, String::new()));
        }
        chunks.last_mut().expect("one chunk").1.push_str(line);
        if fence && fenced && chunks.last().expect("one chunk").1.len() > line.len() {
            fenced = false;
            chunks.push((false, String::new()));
        }
    }
    for (fenced, chunk) in chunks {
        if fenced {
            out.push_str(&chunk);
            continue;
        }
        let mut rest = chunk.as_str();
        while let Some(i) = rest.find(OPEN) {
            let after = &rest[i + OPEN.len()..];
            let Some(close) = after.find('`') else { break };
            let topic = &after[..close];
            let span_end = i + OPEN.len() + close + 1;
            // Inside an existing link, keep the command as written.
            let in_link = rest[..i].ends_with('[');
            match site_route(topic) {
                Some((route, title)) if !in_link => {
                    out.push_str(&rest[..i]);
                    let tail = &rest[span_end..];
                    let section = quoted_section(tail)
                        .and_then(|(s, n)| section_anchor(topic, &s).map(|a| (s, a, n)));
                    match section {
                        Some((name, anchor, n)) => {
                            out.push_str(&format!("[{title}: {name}]({route}#{anchor})"));
                            rest = &tail[n..];
                        }
                        None => {
                            out.push_str(&format!("[{title}]({route})"));
                            rest = tail;
                        }
                    }
                    continue;
                }
                _ => out.push_str(&rest[..span_end]),
            }
            rest = &rest[span_end..];
        }
        out.push_str(rest);
    }
    out
}

/// The title of the site page at `route` (`/deploying/`): a generated page's, else the
/// `title:` of its source under `site/src/content/docs`.
fn site_title(route: &str) -> Option<String> {
    let slug = route.trim_matches('/');
    if slug.is_empty() {
        return Some("barca.sh".to_string());
    }
    if let Some(p) = SITE_PAGES.iter().find(|p| p.slug == slug) {
        return Some(p.title.to_string());
    }
    let docs = site_dir()?.join("src/content/docs");
    ["md", "mdx"].iter().find_map(|ext| {
        let text = std::fs::read_to_string(docs.join(format!("{slug}.{ext}"))).ok()?;
        let line = text.lines().find(|l| l.starts_with("title: "))?;
        Some(line["title: ".len()..].trim().trim_matches('"').to_string())
    })
}

/// Turn a bare `https://barca.sh/<path>` in prose (the manual's pointer for terminal readers)
/// into a link to the page on this site, titled by the page, so it stays on the site it is
/// read on. Code blocks and code spans are left alone.
fn link_site_urls(body: &str) -> String {
    const SITE: &str = "https://barca.sh";
    let mut out = String::new();
    let mut fenced = false;
    for line in body.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if fenced || !line.contains(SITE) {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        while let Some(i) = rest.find(SITE) {
            let before = &rest[..i];
            let in_code = before.matches('`').count() % 2 == 1;
            let in_link = before.ends_with("](") || before.ends_with('<');
            let after = &rest[i + SITE.len()..];
            let len = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || "/-_#".contains(c)))
                .unwrap_or(after.len());
            let route = &after[..len];
            let end = i + SITE.len() + len;
            let title = (route.starts_with('/') && !in_code && !in_link)
                .then(|| site_title(route.split('#').next().unwrap_or(route)))
                .flatten();
            out.push_str(before);
            match title {
                Some(title) => out.push_str(&format!("[{title}]({route})")),
                None => out.push_str(&rest[i..end]),
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
    }
    out
}

fn site_page(page: &SitePage, topic: &str) -> String {
    let (title_line, body) = topic
        .split_once('\n')
        .unwrap_or_else(|| panic!("docs/{} is empty", page.file));
    assert!(
        title_line.starts_with("# "),
        "docs/{} must start with a `# ` title",
        page.file
    );
    let yaml = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!(
        "---\n\
         title: {title}\n\
         description: {description}\n\
         editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/{file}\n\
         ---\n\n\
         <!-- Generated from crates/barca-cli/docs/{file} (barca docs {topic}). Edit that file, \
         then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: \
         (or scripts/update-cli-snapshots.sh). -->\n\n\
         {body}\n\n\
         ---\n\n\
         *The same text is in your terminal: `barca docs {topic}`.*\n",
        title = yaml(page.title),
        description = yaml(page.description),
        file = page.file,
        topic = page.topic,
        body = link_site_urls(&link_topics(body.trim_start_matches('\n').trim_end())),
    )
}

fn site_dir() -> Option<PathBuf> {
    let dir = crate_dir().join("../../site");
    dir.is_dir().then_some(dir)
}

fn site_path(site: &Path, page: &SitePage) -> PathBuf {
    site.join("src/content/docs")
        .join(format!("{}.md", page.slug))
}

/// The site publishes the contract topic verbatim. Called by the contract test after it has
/// updated contract.md. Skipped outside the repository (an sdist build has no `site/`).
fn check_site_page(topic: &str) {
    if let Some(site) = site_dir() {
        let page = &SITE_PAGES[0];
        check_file(&site_path(&site, page), &site_page(page, topic));
    }
}

#[test]
fn site_pages_match_the_manual() {
    let Some(site) = site_dir() else { return };
    for page in SITE_PAGES.iter().filter(|p| p.topic != "contract") {
        assert!(
            crate::docs::find(page.topic).is_some_and(|t| t.name == page.topic),
            "SITE_PAGES names `{}`, which is not a manual topic",
            page.topic
        );
        let topic = std::fs::read_to_string(crate_dir().join("docs").join(page.file))
            .unwrap_or_else(|e| panic!("docs/{}: {e}", page.file));
        check_file(&site_path(&site, page), &site_page(page, &topic));
    }
}

#[test]
fn site_pages_are_in_the_sidebar() {
    let Some(site) = site_dir() else { return };
    let config = std::fs::read_to_string(site.join("astro.config.mjs")).unwrap();
    for page in SITE_PAGES {
        assert!(
            config.contains(&format!("slug: '{}'", page.slug)),
            "site/astro.config.mjs has no sidebar entry for {}",
            page.slug
        );
    }
}

#[test]
fn site_links_skip_code_and_unknown_topics() {
    let body = "See `barca docs sql` and `barca docs big-inputs`.\n```\n`barca docs sql`\n```\n";
    assert_eq!(
        link_topics(body),
        "See [barca sql](/reference/cli/sql/) and `barca docs big-inputs`.\n```\n`barca docs sql`\n```\n"
    );
}

#[test]
fn site_links_take_a_quoted_section() {
    let body =
        "See `barca docs cache`, \"Environment\nvariables\", and `barca docs cache`, \"Nope\".\n";
    assert_eq!(
        link_topics(body),
        "See [The Cache: Environment variables](/reference/cache/#environment-variables), and \
         [The Cache](/reference/cache/), \"Nope\".\n"
    );
}

/// `BARCA_*` variables barca sets or reads that are not documented in contract.md, each with
/// why. A name ending in `*` stands for every name with that prefix.
const INTERNAL_ENV_VARS: &[(&str, &str)] = &[
    (
        "BARCA_WORKER*",
        "set by the coordinator on its own Python worker processes",
    ),
    (
        "BARCA_SOCKET",
        "the coordinator's Unix socket, set for its own workers and helpers",
    ),
    (
        "BARCA_LIFELINE",
        "set for Python helpers so they exit when the coordinator does",
    ),
    (
        "BARCA_JOB_PGID",
        "the coordinator's process group, set for Python helpers",
    ),
    (
        "BARCA_DOCTOR_REQUEST",
        "the request `barca doctor` passes to its Python helper",
    ),
    (
        "BARCA_RUN_ID",
        "sample user source in env_reads' tests, not a variable barca sets",
    ),
    (
        "BARCA_UPDATE_SNAPSHOTS",
        "regenerates barca's own test snapshots",
    ),
    (
        "BARCA_UPLOAD_SNAPSHOT_PREFIX",
        "set by the coordinator for its workers and transfer helper",
    ),
];

/// The only file that may name a `BARCA_TEST_*` variable (hooks for barca's own tests).
const TEST_HOOKS: &str = "crates/barca-model/src/test_hooks.rs";

/// Every `"BARCA_..."` or `'BARCA_...'` string literal in `text`. A bare prefix (`"BARCA_"`, a
/// name ending in `_`) is not a variable.
fn barca_env_literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for quote in ['"', '\''] {
        let needle = format!("{quote}BARCA_");
        let mut rest = text;
        while let Some(at) = rest.find(&needle) {
            let tail = &rest[at + 1..];
            let len = tail
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(tail.len());
            let name = &tail[..len];
            if !name.ends_with('_') {
                out.push(name.to_string());
            }
            rest = &tail[len..];
        }
    }
    out
}

fn source_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

#[test]
fn every_barca_env_var_is_in_the_contract() {
    let root = crate_dir().join("../..");
    let python = root.join("python/barca");
    if !python.is_dir() {
        return; // an sdist of this crate alone
    }
    let mut files = Vec::new();
    for krate in [
        "barca-model",
        "barca-db",
        "barca-core",
        "barca-cli",
        "barca-server",
    ] {
        source_files(
            &root.join("crates").join(krate).join("src"),
            "rs",
            &mut files,
        );
    }
    source_files(&python, "py", &mut files);
    files.sort();
    assert!(files.len() > 50, "found only {} source files", files.len());

    let contract = std::fs::read_to_string(crate_dir().join("docs/contract.md")).unwrap();
    let start = contract
        .find("\n## Environment variables\n")
        .expect("contract.md has an Environment variables section");
    let section = &contract[start + 1..];
    let section = &section[..section[3..].find("\n## ").map_or(section.len(), |e| e + 3)];

    let test_prefix = concat!("BARCA", "_TEST_");
    let internal = |name: &str| {
        INTERNAL_ENV_VARS
            .iter()
            .any(|(pattern, _)| match pattern.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => name == *pattern,
            })
    };
    let mut missing = std::collections::BTreeSet::new();
    let mut stray_test_hooks = std::collections::BTreeSet::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        for name in barca_env_literals(&text) {
            if name.starts_with(test_prefix) {
                if !file.ends_with(TEST_HOOKS) {
                    stray_test_hooks.insert(format!("{name} in {shown}"));
                }
            } else if !internal(&name) && !section.contains(&format!("`{name}`")) {
                missing.insert(format!("{name} in {shown}"));
            }
        }
    }
    assert!(
        stray_test_hooks.is_empty(),
        "read {test_prefix}* variables only in {TEST_HOOKS}: {stray_test_hooks:#?}"
    );
    assert!(
        missing.is_empty(),
        "these variables are not in contract.md's \"Environment variables\" section; document \
         them there and in site/src/content/docs/reference/config.mdx, or add an internal one \
         to INTERNAL_ENV_VARS with why: {missing:#?}"
    );
}

#[test]
fn env_literals_are_found_in_both_quotes() {
    // Spelled with `X_` so this file names no variable of its own.
    let text = r#"env("X_A") os.environ.get('X_B_2', "x") "X_" 'X_P_' X_C"#.replace("X_", "BARCA_");
    let want = ["X_A", "X_B_2"].map(|n| n.replace("X_", "BARCA_"));
    assert_eq!(barca_env_literals(&text), want);
}
