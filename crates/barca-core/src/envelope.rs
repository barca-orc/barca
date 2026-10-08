//! Transport-neutral error classification, remediation, and rendering.

use crate::BarcaError;
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
    /// `step_failed` only: the user-code traceback.
    pub traceback: Option<String>,
    /// `step_failed` only: the failing step's artifact directory (local path or URI).
    pub artifact_dir: Option<String>,
    /// Human-mode text (the existing prose).
    prose: String,
    /// Whether `prose` already contains the remediation (so it is not printed twice).
    prose_has_remediation: bool,
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
            && (wire.node.is_some() || wire.traceback.is_some() || wire.artifact_dir.is_some())
        {
            return Err(serde::de::Error::custom(
                "step details require kind 'step_failed'",
            ));
        }
        let prose = match &wire.traceback {
            Some(traceback) => format!("{}\n{traceback}", wire.error),
            None => wire.error.clone(),
        };
        Ok(Self {
            kind: wire.kind,
            error: wire.error,
            remediation: wire.remediation,
            node: wire.node,
            traceback: wire.traceback,
            artifact_dir: wire.artifact_dir,
            prose,
            prose_has_remediation: false,
        })
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
            artifact_dir: None,
            prose_has_remediation: true,
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
}

/// The HTTP error response preserves the existing public schema.
pub fn http_error(message: impl Into<String>) -> Value {
    json!({"error": message.into()})
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn envelopes_roundtrip_the_existing_wire_schema_without_local_prose() {
        let failed = BarcaError::WorkerFailed(Box::new(crate::FailedStep {
            node: "p.py:a".into(),
            message: "ValueError: bad\n  File \"p.py\", line 4, in a\n    raise".into(),
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
