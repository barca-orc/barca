//! Transport-neutral error classification, remediation, and rendering.

use crate::BarcaError;
use crate::dispatch::ErrorPhase;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// What went wrong, as an agent needs to know it: fix the command, fix the code, retry, or
/// nothing (the user cancelled). Each kind has exactly one exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
            | BarcaError::AmbiguousTarget(_)
            | BarcaError::Usage(_)
            | BarcaError::Parse(_)
            | BarcaError::Dag(_) => ErrorKind::Usage,
            BarcaError::Cancelled => ErrorKind::Cancelled,
            BarcaError::Io(_)
            | BarcaError::Db(_)
            | BarcaError::MetadataUnusable { .. }
            | BarcaError::Other(_) => ErrorKind::Infra,
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

/// The one remediation for an unknown or ambiguous target, on every command: `barca list` is how
/// you discover the assets, tasks and sensors a project defines. Other usage errors carry their
/// own remediation (#492).
pub fn list_hint(files: &[String]) -> String {
    format!(
        "Run `{}` to see available assets and tasks.",
        list_cmd(files)
    )
}

/// Whether `e` is about which node a target names (unknown or ambiguous), the one kind of
/// error the `barca list` hint fixes.
pub fn names_a_target(e: &BarcaError) -> bool {
    matches!(
        e,
        BarcaError::AssetNotFound(..) | BarcaError::AmbiguousTarget(_)
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

/// The envelope `reason` of [`BarcaError::MetadataUnusable`]: the metadata DB is there but this
/// barca cannot use it.
pub const METADATA_UNUSABLE: &str = "metadata_unusable";

/// What to do about a metadata DB at `path` that barca cannot use.
pub fn metadata_remediation(path: &str) -> String {
    format!(
        "Not a problem in your code. If a newer barca wrote this database, upgrade barca. \
         Otherwise put a backup back with `barca state restore` (it moves {path} aside first), \
         or start an empty history with `barca state reset` (the Reset database button in the \
         web UI of a running `barca serve`), which moves {path} aside and prints how to undo it."
    )
}

/// The wire representation is the existing JSON error schema. Human prose is local presentation
/// state and is not serialized: deserialization reconstructs prose from the error and traceback,
/// then appends remediation once. A wire round trip preserves fields, not the original prose.
#[derive(Clone, Debug)]
pub struct ErrorEnvelope {
    pub kind: ErrorKind,
    /// The message: what went wrong, without the fix.
    pub error: String,
    /// What to do next.
    pub remediation: Option<String>,
    /// `step_failed` only: the failing node id.
    pub node: Option<String>,
    /// `step_failed` only: the traceback (the user's code's frames; every frame for a failure
    /// in barca's own code).
    pub traceback: Option<String>,
    /// `step_failed` only: where the step failed (`dispatch::ErrorPhase`, by name), when its
    /// worker said so (#664).
    pub phase: Option<String>,
    /// `step_failed` only: the failing step's artifact directory (local path or URI).
    pub artifact_dir: Option<String>,
    /// A stable name for an error a client may handle on its own, when barca has one:
    /// `metadata_unusable` (the metadata DB cannot be used). Absent from the JSON otherwise.
    pub reason: Option<String>,
    /// Human-mode text (the existing prose).
    prose: String,
    /// Whether `prose` already contains the remediation (so it is not printed twice).
    prose_has_remediation: bool,
    /// Local CLI presentation only; the wire error schema is unchanged.
    run_failure_line: Option<String>,
}

impl Serialize for ErrorEnvelope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ErrorEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WireEnvelope {
            kind: ErrorKind,
            code: i32,
            error: String,
            remediation: Option<String>,
            #[serde(default)]
            node: Option<String>,
            #[serde(default)]
            traceback: Option<String>,
            #[serde(default)]
            artifact_dir: Option<String>,
            #[serde(default)]
            phase: Option<String>,
            #[serde(default)]
            reason: Option<String>,
        }
        let wire = WireEnvelope::deserialize(deserializer)?;
        if wire.code != wire.kind.exit_code() {
            return Err(serde::de::Error::custom(format!(
                "kind '{}' requires code {}, got {}",
                wire.kind.as_str(),
                wire.kind.exit_code(),
                wire.code
            )));
        }
        if wire.kind != ErrorKind::StepFailed
            && (wire.node.is_some()
                || wire.traceback.is_some()
                || wire.artifact_dir.is_some()
                || wire.phase.is_some())
        {
            return Err(serde::de::Error::custom(
                "step details require kind 'step_failed'",
            ));
        }
        // A failed step reads as a local run prints it: `Step '<node>' failed: <summary>` and
        // the traceback.
        let phase = wire.phase.as_deref().and_then(ErrorPhase::parse);
        let head = match (&wire.kind, &wire.node) {
            (ErrorKind::StepFailed, Some(node)) => wire
                .error
                .strip_prefix(&format!("step '{node}' failed: "))
                .map_or_else(
                    || wire.error.clone(),
                    |s| format!("{}{s}", failed_head(node, phase)),
                ),
            _ => wire.error.clone(),
        };
        let prose = match &wire.traceback {
            Some(traceback) => format!("{head}\n{traceback}"),
            None => head,
        };
        Ok(Self {
            kind: wire.kind,
            error: wire.error,
            remediation: wire.remediation,
            node: wire.node,
            traceback: wire.traceback,
            phase: wire.phase,
            artifact_dir: wire.artifact_dir,
            reason: wire.reason,
            prose,
            prose_has_remediation: false,
            run_failure_line: None,
        })
    }
}

/// How a failed step's text begins: `Step '<node>' failed: `, or for a failure in barca's own
/// code, `Step '<node>' failed in barca while loading its inputs (phase load_inputs): `.
fn failed_head(node: &str, phase: Option<ErrorPhase>) -> String {
    match phase.filter(|p| p.in_barca()) {
        Some(p) => format!(
            "Step '{node}' failed in barca while {} (phase {}): ",
            p.doing(),
            p.as_str()
        ),
        None => format!("Step '{node}' failed: "),
    }
}

/// The verb of the one-line run failure: `failed in barca (load_inputs): ` for a failure in
/// barca's own code, else `failed: `.
fn failed_word(phase: Option<ErrorPhase>) -> String {
    match phase.filter(|p| p.in_barca()) {
        Some(p) => format!("failed in barca ({}): ", p.as_str()),
        None => "failed: ".to_string(),
    }
}

/// What to do about a failed step, by how it failed: a timeout, a worker that died without a
/// Python exception, a failure in barca's own code around the step, or an exception (with or
/// without a traceback to look at).
fn step_remediation(step: &crate::FailedStep) -> String {
    const RERUN: &str =
        "re-run the same command. Steps that succeeded are cached and will not re-run.";
    let summary = step.summary();
    let node = &step.node;
    if let Some(phase) = step.phase.filter(|p| p.in_barca()) {
        let check = match phase {
            ErrorPhase::LoadInputs => {
                "An input artifact may be missing or unreadable: recompute the step that \
                 produced it with --refresh."
            }
            ErrorPhase::SplitOutput => {
                "Check that the batched function returns a result for each key it was given \
                 (see `barca docs partitions`)."
            }
            ErrorPhase::WriteOutput => {
                "If the message is about the value the step returned (it cannot be pickled, or \
                 is not a table for parquet), return something barca can write or set \
                 serializer= on the decorator."
            }
            ErrorPhase::Sink => {
                "Check each failed sink's path and serializer (the message names them; see \
                 `barca docs sinks`)."
            }
            ErrorPhase::UserCode => "",
        };
        return format!(
            "The failure is in barca's own code while it was {} for '{node}', not in the \
             step's function; the traceback shows barca's frames. {check} If it is not \
             something in the step or its inputs, report it with the traceback at \
             https://github.com/barca-orc/barca/issues. Then {RERUN}",
            phase.doing()
        );
    }
    if summary.starts_with("TimeoutError: ") && summary.contains(" exceeded timeout of ") {
        format!(
            "'{node}' ran longer than its timeout_seconds= (300 when not set). Raise it on the \
             decorator or make the step faster, then {RERUN}"
        )
    } else if summary.starts_with("worker for ") || summary.starts_with("worker disconnected") {
        format!(
            "The worker running '{node}' died without a Python exception: os._exit(), a crash \
             in native code, or the kernel (SIGKILL is often the out-of-memory killer). Check \
             its memory use and the native libraries it calls, then {RERUN}"
        )
    } else if step.traceback().is_some() {
        format!("Fix the error in '{node}' (see the traceback) and {RERUN}")
    } else {
        format!("Fix the error in '{node}' and {RERUN}")
    }
}

impl ErrorEnvelope {
    /// An error written as prose: an optional `error: ` prefix, the message on the first line,
    /// then (after the first line) what to do about it. Human mode prints the prose verbatim;
    /// JSON splits it into `error` and `remediation`.
    pub fn from_prose(kind: ErrorKind, prose: impl Into<String>) -> Self {
        let prose = prose.into();
        let text = prose.strip_prefix("error: ").unwrap_or(&prose);
        let (head, rest) = text.split_once('\n').unwrap_or((text, ""));
        let rest = rest.trim();
        ErrorEnvelope {
            kind,
            error: head.trim().to_string(),
            remediation: (!rest.is_empty()).then(|| rest.to_string()),
            node: None,
            traceback: None,
            phase: None,
            artifact_dir: None,
            reason: None,
            prose_has_remediation: true,
            run_failure_line: None,
            prose: prose.trim_end().to_string(),
        }
    }

    /// Like [`ErrorEnvelope::from_prose`], with a remediation for when the prose has none.
    pub fn from_prose_or(kind: ErrorKind, prose: impl Into<String>, fallback: String) -> Self {
        let mut e = Self::from_prose(kind, prose);
        if e.remediation.is_none() {
            e.remediation = Some(fallback);
            e.prose_has_remediation = false;
        }
        e
    }

    /// An engine error, with a remediation that names the next command to run.
    pub fn from_barca(e: BarcaError, ctx: &Context) -> Self {
        let kind = ErrorKind::of(&e);
        if let BarcaError::WorkerFailed(step) = &e {
            return ErrorEnvelope {
                kind,
                error: format!("step '{}' failed: {}", step.node, step.summary()),
                remediation: Some(step_remediation(step)),
                node: Some(step.node.clone()),
                traceback: step.traceback().map(str::to_string),
                phase: step.phase.map(|p| p.as_str().to_string()),
                artifact_dir: step.artifact_dir.clone(),
                reason: None,
                prose: format!("{}{}", failed_head(&step.node, step.phase), step.message),
                prose_has_remediation: false,
                run_failure_line: Some(format!(
                    "[barca] run failed: step '{}' {}{}; {} downstream steps skipped (exit 1)",
                    step.node,
                    failed_word(step.phase),
                    step.summary().lines().next().unwrap_or("unknown error"),
                    step.run.as_ref().map_or(0, |run| run
                        .steps
                        .iter()
                        .filter(|s| s.status.as_deref() == Some("skipped"))
                        .count()),
                )),
            };
        }
        let fallback = match &e {
            BarcaError::AssetNotFound(name, available) => {
                let ids: Vec<&str> = available.split(", ").collect();
                match crate::targets::target_suggestion(name, &ids) {
                    Some(meant) => format!("{meant} {}", list_hint(&ctx.files)),
                    None => list_hint(&ctx.files),
                }
            }
            BarcaError::AmbiguousTarget(_) => list_hint(&ctx.files),
            BarcaError::Parse(_) => format!(
                "Fix the Python syntax error, then run `{}` to confirm discovery.",
                ctx.list_cmd()
            ),
            BarcaError::Dag(d) => match d.remediation() {
                Some(fix) => fix,
                // Named files are read with the pipeline files they import inputs from, but a
                // bare name or a re-export through a package `__init__.py` is found only by
                // reading the whole project (#551).
                None if !ctx.files.is_empty()
                    && matches!(
                        d,
                        crate::dag::DagError::UnresolvedInput { .. }
                            | crate::dag::DagError::UnresolvedImport { .. }
                    ) =>
                {
                    format!(
                        "Only the named files ({}) and the pipeline files they import inputs \
                         from were read. Add the file that defines it to the command line, or \
                         drop the file arguments to read the whole project (`{}`).",
                        ctx.files.join(", "),
                        match ctx.command {
                            "get" | "run" | "status" => format!("barca {} <target>", ctx.command),
                            command => format!("barca {command}"),
                        }
                    )
                }
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
            BarcaError::MetadataUnusable { path, .. } => metadata_remediation(path),
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
        let reason =
            matches!(e, BarcaError::MetadataUnusable { .. }).then(|| METADATA_UNUSABLE.to_string());
        let mut out = match &e {
            // These messages put the fix after the first line (e.g. the valid `--refresh`
            // names, or what to close when the DB is locked): split it out.
            BarcaError::Usage(_)
            | BarcaError::Db(_)
            | BarcaError::Other(_)
            | BarcaError::Parse(_) => {
                let mut out = Self::from_prose_or(kind, prose.clone(), fallback);
                // Engine messages carry no `error: ` prefix; keep their prose exactly.
                out.prose = prose.trim_end().to_string();
                out
            }
            _ => ErrorEnvelope {
                kind,
                error: prose.trim().to_string(),
                remediation: Some(fallback),
                node: None,
                traceback: None,
                phase: None,
                artifact_dir: None,
                reason: None,
                prose: prose.trim_end().to_string(),
                prose_has_remediation: false,
                run_failure_line: None,
            },
        };
        out.reason = reason;
        out
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
        // A generic fallback remediation is replaced, not repeated, by the more specific hint;
        // a leading "Did you mean ...?" stays in front of it.
        if !self.prose_has_remediation {
            let meant = self
                .remediation
                .as_deref()
                .filter(|r| r.starts_with("Did you mean "))
                .and_then(|r| r.split_once("? "))
                .map(|(meant, _)| format!("{meant}? "))
                .unwrap_or_default();
            self.remediation = Some(format!("{meant}{hint}"));
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
        if let Some(reason) = &self.reason {
            m.insert("reason".into(), json!(reason));
        }
        if self.kind == ErrorKind::StepFailed {
            m.insert("node".into(), json!(self.node));
            m.insert("traceback".into(), json!(self.traceback));
            m.insert("artifact_dir".into(), json!(self.artifact_dir));
            m.insert("phase".into(), json!(self.phase));
        }
        Value::Object(m)
    }

    /// The final one-line CLI run summary, after the detailed error.
    pub fn run_failure_line(&self) -> Option<&str> {
        self.run_failure_line.as_deref()
    }

    /// For a `step_failed` envelope read back from the wire (which does not carry it): the
    /// one-line run summary a local run prints after the error, given how many downstream
    /// steps were skipped. Any other kind is returned unchanged.
    pub fn with_failed_run_summary(mut self, skipped: usize) -> Self {
        if self.kind == ErrorKind::StepFailed {
            let node = self.node.clone().unwrap_or_default();
            let prefix = format!("step '{node}' failed: ");
            let summary = self.error.strip_prefix(&prefix).unwrap_or(&self.error);
            let phase = self.phase.as_deref().and_then(ErrorPhase::parse);
            self.run_failure_line = Some(format!(
                "[barca] run failed: step '{node}' {}{}; {skipped} downstream steps skipped (exit 1)",
                failed_word(phase),
                summary.lines().next().unwrap_or("unknown error"),
            ));
        }
        self
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
}

/// The HTTP error response preserves the existing public schema.
pub fn http_error(message: impl Into<String>) -> Value {
    json!({"error": message.into()})
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn a_failure_in_barca_says_so_and_keeps_its_phase_on_the_wire() {
        let step = |phase| {
            BarcaError::WorkerFailed(Box::new(crate::FailedStep {
                node: "p.py:a".into(),
                message: "FileNotFoundError: Input artifact not found: x.json\n  File \"/site/barca/_worker.py\", line 9, in _load_artifact".into(),
                phase,
                artifact_dir: None,
                run: None,
            }))
        };
        let e = ErrorEnvelope::from_barca(step(Some(ErrorPhase::LoadInputs)), &Context::default());
        let wire = e.to_json();
        assert_eq!(wire["phase"], "load_inputs");
        assert!(wire["traceback"].as_str().unwrap().contains("_worker.py"));
        assert!(
            e.to_human().starts_with(
                "Step 'p.py:a' failed in barca while loading its inputs (phase load_inputs): FileNotFoundError"
            ),
            "{}",
            e.to_human()
        );
        assert!(
            wire["remediation"]
                .as_str()
                .unwrap()
                .contains("barca's own code")
        );
        assert_eq!(
            e.run_failure_line(),
            Some(
                "[barca] run failed: step 'p.py:a' failed in barca (load_inputs): FileNotFoundError: Input artifact not found: x.json; 0 downstream steps skipped (exit 1)"
            )
        );
        // Read back from the wire (a server's error), the text is the same.
        let restored: ErrorEnvelope = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(restored.to_human(), e.to_human());
        assert_eq!(
            restored.with_failed_run_summary(0).run_failure_line(),
            e.run_failure_line()
        );

        // A failure in the step's own code reads as before; `phase` is still there.
        let user = ErrorEnvelope::from_barca(step(Some(ErrorPhase::UserCode)), &Context::default());
        assert_eq!(user.to_json()["phase"], "user_code");
        assert!(
            user.to_human()
                .starts_with("Step 'p.py:a' failed: FileNotFoundError")
        );
        // Unknown (a worker that died): null.
        let unknown = ErrorEnvelope::from_barca(step(None), &Context::default());
        assert!(unknown.to_json()["phase"].is_null());
        // Only a step failure carries a phase.
        let bad =
            json!({"kind": "usage", "code": 2, "error": "x", "remediation": null, "phase": "sink"});
        assert!(serde_json::from_value::<ErrorEnvelope>(bad).is_err());
    }

    #[test]
    fn phases_are_a_closed_set_named_on_the_wire() {
        for phase in ErrorPhase::ALL {
            assert_eq!(ErrorPhase::parse(phase.as_str()), Some(phase));
            assert_eq!(serde_json::to_value(phase).unwrap(), phase.as_str());
            assert_eq!(phase.in_barca(), phase != ErrorPhase::UserCode);
        }
        assert_eq!(ErrorPhase::parse("compile"), None);
    }

    #[test]
    fn envelopes_roundtrip_the_existing_wire_schema_without_local_prose() {
        let failed = BarcaError::WorkerFailed(Box::new(crate::FailedStep {
            node: "p.py:a".into(),
            message: "ValueError: bad\n  File \"p.py\", line 4, in a\n    raise".into(),
            phase: None,
            artifact_dir: Some("s3://bucket/a".into()),
            run: None,
        }));
        let errors = [
            failed,
            BarcaError::Usage("bad argument\nRun `barca --help`.".into()),
            BarcaError::Db("locked".into()),
            BarcaError::Cancelled,
        ];
        for error in errors {
            let original = ErrorEnvelope::from_barca(error, &Context::default());
            let wire = original.to_json();
            let encoded = serde_json::to_string(&original).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), wire);
            assert!(wire.get("prose").is_none());
            assert!(wire.get("prose_has_remediation").is_none());
            let restored: ErrorEnvelope = serde_json::from_str(&encoded).unwrap();
            assert_eq!(restored.to_json(), wire);
            assert_eq!(restored.kind, original.kind);
            assert_eq!(serde_json::to_value(restored.kind).unwrap(), wire["kind"]);
            assert_eq!(
                restored
                    .to_human()
                    .matches(restored.remediation.as_deref().unwrap())
                    .count(),
                1
            );
            if restored.kind == ErrorKind::StepFailed {
                assert_eq!(restored.node.as_deref(), Some("p.py:a"));
                assert!(restored.to_human().contains("line 4"));
                // The text a local run prints, rebuilt from the wire fields.
                assert_eq!(restored.to_human(), original.to_human());
                let summary = restored.clone().with_failed_run_summary(2);
                assert_eq!(
                    summary.run_failure_line(),
                    Some(
                        "[barca] run failed: step 'p.py:a' failed: ValueError: bad; 2 downstream \
                         steps skipped (exit 1)"
                    )
                );
            }
        }
    }

    #[test]
    fn deserialized_prose_is_reconstructed_while_native_prose_is_unchanged() {
        let original = ErrorEnvelope::from_prose(
            ErrorKind::Usage,
            "error: bad argument\n\nRun `barca --help`.",
        );
        assert_eq!(
            original.to_human(),
            "error: bad argument\n\nRun `barca --help`."
        );
        let restored: ErrorEnvelope = serde_json::from_value(original.to_json()).unwrap();
        assert_eq!(restored.to_human(), "bad argument\n\nRun `barca --help`.");
        assert_eq!(restored.to_json(), original.to_json());
    }

    #[test]
    fn deserialization_rejects_every_inconsistent_kind_code_pair() {
        let kinds = [
            ErrorKind::StepFailed,
            ErrorKind::Usage,
            ErrorKind::Infra,
            ErrorKind::Cancelled,
        ];
        for kind in kinds {
            for code in [0, 1, 2, 3, 130] {
                let wire = json!({"kind":kind,"code":code,"error":"failed","remediation":null});
                let decoded = serde_json::from_value::<ErrorEnvelope>(wire);
                assert_eq!(
                    decoded.is_ok(),
                    code == kind.exit_code(),
                    "{kind:?} code={code}"
                );
            }
        }
        assert!(
            serde_json::from_value::<ErrorEnvelope>(
                json!({"kind":"unknown","code":3,"error":"failed","remediation":null})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ErrorEnvelope>(
                json!({"kind":"usage","code":2,"error":"failed","remediation":null,"node":"p.py:a"})
            )
            .is_err()
        );
    }
}
