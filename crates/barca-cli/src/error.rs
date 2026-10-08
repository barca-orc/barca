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

use barca_core::BarcaError;
use serde_json::{Map, Value, json};

/// What went wrong, as an agent needs to know it: fix the command, fix the code, retry, or
/// nothing (the user cancelled). Each kind has exactly one exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// A user step raised (traceback included). Fix the code; retrying barca will not help.
    StepFailed,
    /// Bad arguments, unknown target, wrong command for the node kind, invalid config.
    Usage,
    /// barca or its environment failed: metadata DB, worker spawn, remote state, I/O.
    Infra,
    /// Interrupted (Ctrl-C).
    Cancelled,
}

impl ErrorKind {
    /// The exit code table — defined here and nowhere else.
    ///
    /// | code | kind          |
    /// |------|---------------|
    /// | 0    | success       |
    /// | 1    | `step_failed` |
    /// | 2    | `usage`       |
    /// | 3    | `infra`       |
    /// | 130  | `cancelled`   |
    pub const fn exit_code(self) -> i32 {
        match self {
            ErrorKind::StepFailed => 1,
            ErrorKind::Usage => 2,
            ErrorKind::Infra => 3,
            ErrorKind::Cancelled => 130,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorKind::StepFailed => "step_failed",
            ErrorKind::Usage => "usage",
            ErrorKind::Infra => "infra",
            ErrorKind::Cancelled => "cancelled",
        }
    }

    /// The mapping from engine errors to kinds.
    pub fn of(e: &BarcaError) -> Self {
        match e {
            BarcaError::WorkerFailed(_) => ErrorKind::StepFailed,
            BarcaError::AssetNotFound(..)
            | BarcaError::Usage(_)
            | BarcaError::Parse(_)
            | BarcaError::Dag(_) => ErrorKind::Usage,
            BarcaError::Cancelled => ErrorKind::Cancelled,
            BarcaError::Io(_) | BarcaError::Db(_) | BarcaError::Other(_) => ErrorKind::Infra,
        }
    }
}

/// What the command was asked to do, so a remediation can name a runnable next command.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// The subcommand (`get`, `run`, ...).
    pub command: &'static str,
    /// The `.py` files the user passed.
    pub files: Vec<String>,
}

/// Quote a word for display in a copy-pasteable shell command.
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:,=@+%".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `barca list <files>`, quoted for the shell (plain `barca list`, the whole project, when no
/// file is known).
pub fn list_cmd(files: &[String]) -> String {
    if files.is_empty() {
        "barca list".to_string()
    } else {
        let quoted: Vec<String> = files.iter().map(|f| shell_quote(f)).collect();
        format!("barca list {}", quoted.join(" "))
    }
}

/// The one remediation for an unknown target or a misused name, on every command: `barca list`
/// is how you discover the assets, tasks and sensors a project defines.
pub fn list_hint(files: &[String]) -> String {
    format!(
        "Run `{}` to see available assets and tasks.",
        list_cmd(files)
    )
}

impl Context {
    fn list_cmd(&self) -> String {
        list_cmd(&self.files)
    }

    fn help_cmd(&self) -> String {
        if self.command.is_empty() {
            "barca --help".to_string()
        } else {
            format!("barca {} --help", self.command)
        }
    }
}

#[derive(Clone, Debug)]
pub struct CliError {
    pub kind: ErrorKind,
    /// The message: what went wrong, without the fix.
    pub error: String,
    /// What to do next.
    pub remediation: Option<String>,
    /// `step_failed` only: the failing node id.
    pub node: Option<String>,
    /// `step_failed` only: the user-code traceback.
    pub traceback: Option<String>,
    /// `step_failed` only: the failing step's artifact directory (local path or URI).
    pub artifact_dir: Option<String>,
    /// Human-mode text (the existing prose).
    prose: String,
    /// Whether `prose` already contains the remediation (so it is not printed twice).
    prose_has_remediation: bool,
}

impl CliError {
    /// An error written as prose: an optional `error: ` prefix, the message on the first line,
    /// then (after the first line) what to do about it. Human mode prints the prose verbatim;
    /// JSON splits it into `error` and `remediation`.
    pub fn from_prose(kind: ErrorKind, prose: impl Into<String>) -> Self {
        let prose = prose.into();
        let text = prose.strip_prefix("error: ").unwrap_or(&prose);
        let (head, rest) = text.split_once('\n').unwrap_or((text, ""));
        let rest = rest.trim();
        CliError {
            kind,
            error: head.trim().to_string(),
            remediation: (!rest.is_empty()).then(|| rest.to_string()),
            node: None,
            traceback: None,
            artifact_dir: None,
            prose_has_remediation: true,
            prose: prose.trim_end().to_string(),
        }
    }

    /// Like [`CliError::from_prose`], with a remediation for when the prose has none.
    pub fn from_prose_or(kind: ErrorKind, prose: impl Into<String>, fallback: String) -> Self {
        let mut e = Self::from_prose(kind, prose);
        if e.remediation.is_none() {
            e.remediation = Some(fallback);
            e.prose_has_remediation = false;
        }
        e
    }

    /// An argument-parser (clap) error: always `usage`.
    pub fn from_clap(e: &clap::Error) -> Self {
        let mut out = Self::from_prose(ErrorKind::Usage, e.render().to_string());
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

    /// An engine error, with a remediation that names the next command to run.
    pub fn from_barca(e: BarcaError, ctx: &Context) -> Self {
        let kind = ErrorKind::of(&e);
        if let BarcaError::WorkerFailed(step) = &e {
            return CliError {
                kind,
                error: format!("step '{}' failed: {}", step.node, step.summary()),
                remediation: Some(format!(
                    "Fix the error in '{}' (see the traceback) and re-run the same command. \
                     Steps that succeeded are cached and will not re-run.",
                    step.node
                )),
                node: Some(step.node.clone()),
                traceback: step.traceback().map(str::to_string),
                artifact_dir: step.artifact_dir.clone(),
                prose: e.to_string(),
                prose_has_remediation: false,
            };
        }
        let fallback = match &e {
            BarcaError::AssetNotFound(..) => list_hint(&ctx.files),
            BarcaError::Parse(_) => format!(
                "Fix the Python syntax error, then run `{}` to confirm discovery.",
                ctx.list_cmd()
            ),
            BarcaError::Dag(d) => match d.remediation() {
                Some(fix) => fix,
                None => format!(
                    "Fix the inputs between definitions, then run `{}` to check each node's \
                     inputs.",
                    ctx.list_cmd()
                ),
            },
            BarcaError::Usage(_) => format!("See `{}`.", ctx.help_cmd()),
            BarcaError::Cancelled => "Re-run the same command; steps that finished before the \
                                      cancel are cached and will not re-run."
                .to_string(),
            BarcaError::Db(_) => "Not a problem in your code. Retry the command; if it keeps \
                                  failing, check that .barca/ is writable and that no other \
                                  program holds the metadata DB."
                .to_string(),
            BarcaError::Io(_) | BarcaError::Other(_) | BarcaError::WorkerFailed(_) => {
                "Not a problem in your code: barca or its environment failed. Retrying may \
                 succeed."
                    .to_string()
            }
        };
        let prose = e.to_string();
        match &e {
            // These messages put the fix after the first line (e.g. the valid `--refresh`
            // names, or what to close when the DB is locked): split it out.
            // A parse error that knows its fix (an argument a decorator does not define)
            // carries it the same way; a syntax error is one line and gets the fallback.
            BarcaError::Usage(_)
            | BarcaError::Db(_)
            | BarcaError::Other(_)
            | BarcaError::Parse(_) => {
                let mut out = Self::from_prose_or(kind, prose.clone(), fallback);
                // Engine messages carry no `error: ` prefix; keep their prose exactly.
                out.prose = prose.trim_end().to_string();
                out
            }
            _ => CliError {
                kind,
                error: prose.trim().to_string(),
                remediation: Some(fallback),
                node: None,
                traceback: None,
                artifact_dir: None,
                prose: prose.trim_end().to_string(),
                prose_has_remediation: false,
            },
        }
    }

    /// Append a final line to the remediation (and to the human prose when it already carries
    /// the remediation), unless the remediation already says it.
    pub fn with_final_hint(mut self, hint: String) -> Self {
        if self
            .remediation
            .as_deref()
            .is_some_and(|r| r.contains(&hint))
        {
            return self;
        }
        // A generic fallback remediation is replaced, not repeated, by the more specific hint.
        if !self.prose_has_remediation {
            self.remediation = Some(hint);
            return self;
        }
        self.remediation = Some(match self.remediation.take() {
            Some(r) => format!("{r}\n{hint}"),
            None => hint.clone(),
        });
        if self.prose_has_remediation {
            self.prose = format!("{}\n\n{hint}", self.prose);
        }
        self
    }

    pub fn code(&self) -> i32 {
        self.kind.exit_code()
    }

    /// The JSON envelope.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("error".into(), json!(self.error));
        m.insert("code".into(), json!(self.code()));
        m.insert("kind".into(), json!(self.kind.as_str()));
        m.insert("remediation".into(), json!(self.remediation));
        if self.kind == ErrorKind::StepFailed {
            m.insert("node".into(), json!(self.node));
            m.insert("traceback".into(), json!(self.traceback));
            m.insert("artifact_dir".into(), json!(self.artifact_dir));
        }
        Value::Object(m)
    }

    /// The human-mode text: the prose, then the remediation unless the prose already has it.
    pub fn to_human(&self) -> String {
        match (&self.remediation, self.prose_has_remediation) {
            (Some(r), false) => format!("{}\n\n{r}", self.prose),
            _ => self.prose.clone(),
        }
    }

    /// The line(s) written to stderr.
    pub fn render(&self, json: bool) -> String {
        if json {
            self.to_json().to_string()
        } else {
            self.to_human()
        }
    }

    /// Write the error to stderr and exit with its code. A closed stderr drops the text; the
    /// exit code is the error's either way (`barca_core::term`).
    pub fn emit(&self, json: bool) -> ! {
        barca_core::errln!("{}", self.render(json));
        barca_core::term::exit(self.code())
    }
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
