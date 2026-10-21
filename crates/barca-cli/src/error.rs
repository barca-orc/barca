//! The CLI's one error emitter.
//!
//! Every way `barca` exits with an error goes through [`CliError::emit`]. In JSON output mode
//! (whenever results are JSON: see output.rs; `plan` always; `docs` with `--json`) the error is
//! one JSON line on stderr:
//!
//! ```text
//! {"error": "...", "code": 2, "kind": "usage", "remediation": "..."}
//! ```
//!
//! and `step_failed` adds `node`, `traceback` and `artifact_dir`. In human mode the prose is
//! unchanged, with the remediation appended. Errors always go to stderr; stdout is for results.
//! The schema is documented in `barca docs agents` (crates/barca-cli/docs/agents.md).

pub use barca_core::envelope::{
    Context, ErrorEnvelope as CliError, ErrorKind, list_hint, names_a_target, shell_quote,
};

pub trait CliErrorExt {
    fn from_clap(e: &clap::Error, argv: &[String]) -> Self;
    fn emit(&self, json: bool) -> !;
}

impl CliErrorExt for CliError {
    /// An argument-parser (clap) error for `argv`: always `usage`. A mistyped flag or
    /// subcommand gets the one "Did you mean" rule (`barca_core::suggest`) in place of clap's
    /// `-- --flag` tip.
    fn from_clap(e: &clap::Error, argv: &[String]) -> Self {
        let mut out = Self::from_prose(ErrorKind::Usage, clap_prose(e, argv));
        // "the following required arguments were not provided:" names them on the next line.
        if out.error.ends_with(':')
            && let Some(rem) = out.remediation.take()
        {
            let (next, rest) = rem.split_once('\n').unwrap_or((&rem, ""));
            out.error = format!("{} {}", out.error, next.trim());
            let rest = rest.trim();
            out.remediation = (!rest.is_empty()).then(|| rest.to_string());
        }
        out
    }

    /// Write the error to stderr and exit with its code.
    fn emit(&self, json: bool) -> ! {
        barca_core::errln!("{}", self.render(json));
        if let Some(line) = self.run_failure_line() {
            barca_core::errln!("{line}");
        }
        barca_core::term::exit(self.code())
    }
}

/// clap's rendered error, with "Did you mean ...?" after its first line for an unknown flag or
/// subcommand that is close to one `argv`'s command takes. clap's tip to pass an unknown flag
/// as a value (`use '-- --refesh'`) is dropped: it is wrong advice for a mistyped flag.
fn clap_prose(e: &clap::Error, argv: &[String]) -> String {
    use clap::error::{ContextKind, ContextValue, ErrorKind as Kind};
    let rendered = e.render().to_string();
    let invalid = |kind| match e.get(kind) {
        Some(ContextValue::String(s)) => Some(s.as_str()),
        _ => None,
    };
    let cmd = command_of(argv);
    let meant = match e.kind() {
        Kind::UnknownArgument => invalid(ContextKind::InvalidArg).and_then(|arg| {
            let typed = arg.strip_prefix("--")?.split('=').next()?;
            let longs: Vec<String> = cmd
                .get_arguments()
                .filter(|a| !a.is_hide_set())
                .flat_map(|a| {
                    a.get_long()
                        .into_iter()
                        .chain(a.get_visible_aliases().into_iter().flatten())
                })
                .chain(["help"])
                .map(str::to_string)
                .collect();
            let close = barca_core::suggest::closest(typed, longs.iter().map(String::as_str));
            let flags: Vec<String> = close.iter().map(|l| format!("--{l}")).collect();
            barca_core::suggest::did_you_mean(
                &flags.iter().map(String::as_str).collect::<Vec<_>>(),
                '\'',
            )
        }),
        Kind::InvalidSubcommand => invalid(ContextKind::InvalidSubcommand).and_then(|name| {
            barca_core::suggest::suggestion(
                name,
                cmd.get_subcommands()
                    .filter(|c| !c.is_hide_set())
                    .map(|c| c.get_name()),
            )
        }),
        _ => None,
    };
    let mut lines: Vec<&str> = Vec::new();
    let mut it = rendered.lines().peekable();
    while let Some(line) = it.next() {
        if e.kind() == Kind::UnknownArgument && line.trim_start().starts_with("tip: to pass '") {
            // Drop the tip and the blank line after it.
            if it.peek().is_some_and(|l| l.trim().is_empty()) {
                it.next();
            }
            continue;
        }
        lines.push(line);
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        out.push('\n');
        if i == 0
            && let Some(meant) = &meant
        {
            out.push_str(&format!("\n{meant}\n"));
        }
    }
    out
}

/// The (sub)command `argv` names: `barca get ...` is `get`, `barca state reset ...` is
/// `state reset`; the top-level command when none is named.
fn command_of(argv: &[String]) -> clap::Command {
    use clap::CommandFactory;
    let mut cmd = crate::args::Cli::command();
    for arg in argv.iter().skip(1) {
        if arg == "--" {
            break;
        }
        if arg.starts_with('-') {
            continue;
        }
        if let Some(sub) = cmd.find_subcommand(arg) {
            cmd = sub.clone();
        }
    }
    cmd
}

/// Whether raw argv asks for JSON output, for errors raised before the arguments parse. Same
/// precedence as results (see output.rs): an explicit `--json` / `--pretty` / `-o`, then
/// `BARCA_OUTPUT`, then whether stdout is a terminal. `plan` is always JSON; `docs` only with
/// `--json`; `serve` and `version` never.
pub fn json_mode_from_argv(argv: &[String]) -> bool {
    use std::io::IsTerminal;
    let env = std::env::var(crate::output::ENV_VAR).ok();
    json_mode_from_argv_with(argv, env.as_deref(), std::io::stdout().is_terminal())
}

/// [`json_mode_from_argv`] without process state, for tests.
pub fn json_mode_from_argv_with(argv: &[String], env: Option<&str>, stdout_is_tty: bool) -> bool {
    let args = argv.get(1..).unwrap_or_default();
    if args.iter().any(|a| a == "--json") {
        return true;
    }
    if args.iter().any(|a| a == "--pretty") {
        return false;
    }
    let Some(sub) = args.iter().find(|a| !a.starts_with('-')) else {
        return false;
    };
    let follows_rule = match sub.as_str() {
        "plan" => return true,
        "get" | "run" => true,
        s if s.ends_with(".py") => true,
        "list" | "history" | "stats" | "status" => true,
        _ => false,
    };
    if !follows_rule {
        return false;
    }
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        let value = match a.as_str() {
            "-o" | "--output" => it.peek().map(|s| s.as_str()),
            s => s
                .strip_prefix("--output=")
                .or_else(|| s.strip_prefix("-o="))
                .or_else(|| s.strip_prefix("-o").filter(|v| !v.is_empty())),
        };
        match value {
            Some("value" | "pretty") => return false,
            Some("json") => return true,
            _ => {}
        }
    }
    // An invalid BARCA_OUTPUT is reported separately; here it falls back to the terminal.
    crate::output::decide(None, env, stdout_is_tty)
        .map_or(!stdout_is_tty, |f| f == crate::output::Format::Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use barca_core::BarcaError;
    use serde_json::Value;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn exit_code_table() {
        assert_eq!(ErrorKind::StepFailed.exit_code(), 1);
        assert_eq!(ErrorKind::Usage.exit_code(), 2);
        assert_eq!(ErrorKind::Infra.exit_code(), 3);
        assert_eq!(ErrorKind::Cancelled.exit_code(), 130);
    }

    #[test]
    fn every_engine_error_maps_to_a_kind_and_parseable_envelope() {
        let ctx = Context {
            command: "get",
            files: vec!["p.py".into()],
        };
        let cases = [
            (
                BarcaError::AssetNotFound("x".into(), "p.py:a".into()),
                "usage",
            ),
            (BarcaError::Usage("bad\nfix it like this".into()), "usage"),
            (BarcaError::Parse("p.py: invalid syntax".into()), "usage"),
            (
                BarcaError::Dag(barca_core::dag::DagError::CycleDetected),
                "usage",
            ),
            (
                BarcaError::WorkerFailed(Box::new(barca_core::FailedStep {
                    node: "p.py:a".into(),
                    message: "ValueError: no\n  File \"p.py\", line 3, in a\n    raise".into(),
                    artifact_dir: Some(".barca/artifacts/p.py--a".into()),
                    run: None,
                })),
                "step_failed",
            ),
            (BarcaError::Cancelled, "cancelled"),
            (BarcaError::Db("locked".into()), "infra"),
            (BarcaError::Other("push conflicted".into()), "infra"),
            (BarcaError::Io(std::io::Error::other("disk")), "infra"),
        ];
        for (e, kind) in cases {
            let err = CliError::from_barca(e, &ctx);
            let line = err.render(true);
            assert!(!line.contains('\n'), "envelope must be one line: {line}");
            let v: Value = serde_json::from_str(&line).expect("envelope parses");
            assert_eq!(v["kind"], kind, "{line}");
            assert_eq!(v["code"], err.code());
            assert!(v["error"].as_str().is_some_and(|s| !s.is_empty()), "{line}");
            assert!(v["remediation"].is_string(), "{line}");
            if kind == "step_failed" {
                assert_eq!(v["node"], "p.py:a");
                assert_eq!(v["error"], "step 'p.py:a' failed: ValueError: no");
                assert!(v["traceback"].as_str().unwrap().contains("line 3"));
                assert_eq!(v["artifact_dir"], ".barca/artifacts/p.py--a");
            } else {
                assert!(v.get("node").is_none());
            }
        }
    }

    #[test]
    fn multi_line_usage_splits_message_from_remediation() {
        let e = CliError::from_barca(
            BarcaError::Usage(
                "--refresh: no upstream asset named 'nope'.\nUpstream assets you can refresh: a, b"
                    .into(),
            ),
            &Context::default(),
        );
        assert_eq!(e.error, "--refresh: no upstream asset named 'nope'.");
        assert_eq!(
            e.remediation.as_deref(),
            Some("Upstream assets you can refresh: a, b")
        );
        // Human mode: the prose already says what to do, so nothing is appended.
        assert_eq!(
            e.to_human(),
            "--refresh: no upstream asset named 'nope'.\nUpstream assets you can refresh: a, b"
        );
    }

    #[test]
    fn human_mode_appends_the_remediation() {
        let e = CliError::from_barca(BarcaError::Cancelled, &Context::default());
        assert!(
            e.to_human()
                .starts_with("run cancelled\n\nRe-run the same command")
        );
        let e = CliError::from_prose(
            ErrorKind::Usage,
            "error: no files provided\n\nUsage: barca get [TARGET] <FILES>...",
        );
        assert_eq!(e.error, "no files provided");
        assert_eq!(
            e.remediation.as_deref(),
            Some("Usage: barca get [TARGET] <FILES>...")
        );
        assert_eq!(
            e.to_human(),
            "error: no files provided\n\nUsage: barca get [TARGET] <FILES>..."
        );
    }

    #[test]
    fn json_mode_detection_before_parsing() {
        let piped = |s: &str| json_mode_from_argv_with(&argv(s), None, false);
        let tty = |s: &str| json_mode_from_argv_with(&argv(s), None, true);
        // Piped (agents, scripts): JSON unless a flag says otherwise.
        assert!(piped("barca get x p.py --bogus"));
        assert!(piped("barca run t p.py"));
        assert!(piped("barca p.py --bogus"));
        assert!(piped("barca list p.py --bogus"));
        assert!(!piped("barca get x p.py -o pretty --bogus"));
        assert!(!piped("barca get x p.py --output=value"));
        assert!(!piped("barca get x p.py -opretty"));
        assert!(!piped("barca list p.py --pretty --bogus"));
        // In a terminal: human unless a flag or BARCA_OUTPUT asks for JSON.
        assert!(!tty("barca get x p.py --bogus"));
        assert!(tty("barca get x p.py -o json --bogus"));
        assert!(tty("barca list --json --bogus"));
        assert!(json_mode_from_argv_with(
            &argv("barca get x p.py --bogus"),
            Some("json"),
            true
        ));
        assert!(!json_mode_from_argv_with(
            &argv("barca get x p.py"),
            Some("pretty"),
            false
        ));
        // Fixed modes.
        assert!(tty("barca plan"));
        assert!(!piped("barca docs nope"));
        assert!(!piped("barca frobnicate"));
        assert!(!piped("barca"));
    }
}
