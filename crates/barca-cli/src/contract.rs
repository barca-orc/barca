//! The CLI contract (`docs/contract.md`, shown by `barca docs contract`), enforced by snapshots.
//!
//! Two things are compared against checked-in files, so any change to the CLI surface fails
//! `cargo test -p barca` until the files are updated in the same change:
//!
//! - `snapshots/help/<command>.txt`: the exact `--help` output of the top level and of every
//!   subcommand, rendered at a fixed width of 100 columns (what the binary prints: clap is built
//!   without `wrap_help`, so it never reads the terminal width).
//! - the generated blocks in `docs/contract.md`: the command table, every command's arguments
//!   with their stability, and the exit codes, all rendered from the clap definition and
//!   [`ErrorKind`].
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
        "doctor",
        "new prerequisite report; check names and provider-specific data may evolve",
    ),
    (
        "gc",
        "new in 0.25 (RFC-0012): the eviction order, the low watermark and the JSON shape may change after field use",
    ),
    (
        "history --reconcile",
        "explicit operator recovery for stopped runs whose owner cannot be verified",
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
        "serve",
        "the HTTP API and scheduler are young: no auth, routes may change; the older trigger routes are aliases until the next minor",
    ),
    (
        "run --output",
        "kept for compatibility; --json / --pretty are the canonical spelling",
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
        "get --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "get --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats)",
    ),
    (
        "run --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "run --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats)",
    ),
    (
        "status --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "status --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats)",
    ),
    (
        "list --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "list --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats)",
    ),
    (
        "history --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "history --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats)",
    ),
    (
        "stats --server",
        "server mode is new (RFC-0007): how the server is chosen and the client's messages may change",
    ),
    (
        "stats --local",
        "server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats)",
    ),
];

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
    let mut argv = vec!["barca"];
    argv.extend_from_slice(path);
    argv.push("--help");
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
        expected_files.push(file);
    }
    // A snapshot for a command that no longer exists is a removed command: fail on it too.
    for entry in std::fs::read_dir(&dir).unwrap() {
        let file = entry.unwrap().file_name().to_string_lossy().to_string();
        if !expected_files.contains(&file) {
            if updating() {
                std::fs::remove_file(dir.join(&file)).unwrap();
            } else {
                panic!("snapshots/help/{file} has no matching command.\n\n{UPDATE_HINT}");
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

/// An argument of an experimental command is experimental with it.
fn stability(key: &str) -> String {
    let command = key.split_once(' ').map(|(c, _)| c);
    match (experimental(key), command.and_then(experimental)) {
        (Some(why), _) => format!("experimental: {why}"),
        (None, Some(_)) => "experimental (with the command)".to_string(),
        (None, None) => "stable".to_string(),
    }
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
            cell(&stability(name.split(' ').next().unwrap_or(name))),
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

#[test]
fn every_experimental_key_names_a_real_command_or_argument() {
    let root = built();
    for (key, why) in EXPERIMENTAL {
        assert!(!why.is_empty(), "`{key}` needs a reason");
        let (cmd, arg) = key.split_once(' ').unwrap_or((key, ""));
        let command = if cmd == "barca" {
            &root
        } else {
            root.find_subcommand(cmd)
                .unwrap_or_else(|| panic!("EXPERIMENTAL names unknown command `{cmd}`"))
        };
        if !arg.is_empty() {
            assert!(
                command.get_arguments().any(|a| arg_key(a) == arg),
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
        slug: "reference/doctor",
        title: "Doctor and Local Debugging",
        description: "Check project prerequisites, stores and credential identity without \
                      running steps.",
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
        title: "Server Mode",
        description: "One barca serve as the team's ledger: laptops read its cache hits and post \
                      their runs, and --server runs a command on it.",
    },
    SitePage {
        topic: "sql",
        file: "sql.md",
        slug: "reference/sql",
        title: "SQL",
        description: "barca sql: query cached results with DuckDB while debugging, without \
                      writing a step.",
    },
    SitePage {
        topic: "telemetry",
        file: "telemetry.md",
        slug: "reference/telemetry",
        title: "Telemetry",
        description: "Report every run to Datadog as a trace of the pipeline and barca serve as \
                      DogStatsD metrics, and post to Slack when a job starts failing or recovers.",
    },
];

/// Manual topics with a site page that is not generated from them, for links.
const SITE_ROUTES: &[(&str, &str)] = &[
    ("scheduling", "/scheduling/"),
    ("skill", "/reference/agent-skill/"),
];

fn site_route(topic: &str) -> Option<String> {
    SITE_PAGES
        .iter()
        .find(|p| p.topic == topic)
        .map(|p| format!("/{}/", p.slug))
        .or_else(|| {
            SITE_ROUTES
                .iter()
                .find(|(t, _)| *t == topic)
                .map(|(_, r)| r.to_string())
        })
}

/// Turn `` `barca docs <topic>` `` into a link to the topic's site page, outside code blocks.
/// Topics without a site page stay as they are: the reader runs the command.
fn link_topics(body: &str) -> String {
    const OPEN: &str = "`barca docs ";
    let mut out = String::new();
    let mut fenced = false;
    for line in body.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if fenced {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        while let Some(i) = rest.find(OPEN) {
            let after = &rest[i + OPEN.len()..];
            let Some(close) = after.find('`') else { break };
            let topic = &after[..close];
            let span_end = i + OPEN.len() + close + 1;
            match site_route(topic) {
                Some(route) if !rest[..i].ends_with('[') => {
                    out.push_str(&rest[..i]);
                    out.push_str(&format!("[{}]({route})", &rest[i..span_end]));
                }
                _ => out.push_str(&rest[..span_end]),
            }
            rest = &rest[span_end..];
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
         :::note\n\
         This page is the manual topic `barca docs {topic}`: the CLI prints the same text.\n\
         :::\n\n\
         {body}",
        title = yaml(page.title),
        description = yaml(page.description),
        file = page.file,
        topic = page.topic,
        body = link_topics(body.trim_start_matches('\n')),
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
    let body = "See `barca docs sql` and `barca docs cache`.\n```\n`barca docs sql`\n```\n";
    assert_eq!(
        link_topics(body),
        "See [`barca docs sql`](/reference/sql/) and `barca docs cache`.\n```\n`barca docs sql`\n```\n"
    );
}
