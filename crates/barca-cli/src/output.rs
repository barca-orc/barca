//! The one rule for what a command prints on stdout: human text or JSON.
//!
//! Precedence, highest first:
//! 1. an explicit flag: `--json` / `--pretty` (and `-o json|value|pretty` on get/run);
//! 2. the `BARCA_OUTPUT=json|pretty` environment variable;
//! 3. whether stdout is a terminal: TTY -> human (tables / pretty), anything else -> JSON.
//!
//! Every result-producing command resolves its format through [`resolve`], so the rule
//! cannot drift between commands. Progress on stderr is separate: the progress bar draws
//! only when stderr is a terminal and `--agent` is not set.

use clap::Args;
use std::io::IsTerminal;

/// Environment variable that overrides TTY detection (a flag still wins).
pub const ENV_VAR: &str = "BARCA_OUTPUT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Pretty,
}

/// `--json` / `--pretty`, flattened into every command that prints results.
#[derive(Args, Clone, Copy, Debug, Default)]
pub struct FormatFlags {
    /// Emit JSON on stdout (the default when stdout is not a terminal)
    #[arg(long, conflicts_with = "pretty")]
    pub json: bool,
    /// Emit human-readable output (the default when stdout is a terminal)
    #[arg(long)]
    pub pretty: bool,
}

impl FormatFlags {
    pub fn explicit(self) -> Option<Format> {
        match (self.json, self.pretty) {
            (true, _) => Some(Format::Json),
            (_, true) => Some(Format::Pretty),
            _ => None,
        }
    }
}

/// The decision itself, free of process state so it can be unit-tested.
pub fn decide(
    flag: Option<Format>,
    env: Option<&str>,
    stdout_is_tty: bool,
) -> Result<Format, String> {
    if let Some(f) = flag {
        return Ok(f);
    }
    match env.map(str::trim) {
        None | Some("") => {}
        Some(v) if v.eq_ignore_ascii_case("json") => return Ok(Format::Json),
        Some(v) if v.eq_ignore_ascii_case("pretty") => return Ok(Format::Pretty),
        Some(v) => {
            return Err(format!(
                "error: {ENV_VAR}='{v}' is not a valid output format\nUse {ENV_VAR}=json or \
                 {ENV_VAR}=pretty, or unset it to follow the terminal."
            ));
        }
    }
    Ok(if stdout_is_tty {
        Format::Pretty
    } else {
        Format::Json
    })
}

/// Resolve the format for this process. An invalid `BARCA_OUTPUT` is a usage error (exit 2).
pub fn resolve(flag: Option<Format>) -> Format {
    let env = std::env::var(ENV_VAR).ok();
    let tty = std::io::stdout().is_terminal();
    decide(flag, env.as_deref(), tty).unwrap_or_else(|msg| {
        // The format itself is in question: fall back to the terminal rule for the error.
        crate::error::CliError::from_prose(crate::error::ErrorKind::Usage, msg).emit(!tty)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_gets_pretty_and_everything_else_gets_json() {
        assert_eq!(decide(None, None, true), Ok(Format::Pretty));
        assert_eq!(decide(None, None, false), Ok(Format::Json));
    }

    #[test]
    fn env_overrides_the_terminal() {
        assert_eq!(decide(None, Some("json"), true), Ok(Format::Json));
        assert_eq!(decide(None, Some("pretty"), false), Ok(Format::Pretty));
        assert_eq!(decide(None, Some("JSON"), true), Ok(Format::Json));
        // Empty means unset.
        assert_eq!(decide(None, Some(""), false), Ok(Format::Json));
    }

    #[test]
    fn flag_overrides_env_and_terminal() {
        assert_eq!(
            decide(Some(Format::Json), Some("pretty"), true),
            Ok(Format::Json)
        );
        assert_eq!(
            decide(Some(Format::Pretty), Some("json"), false),
            Ok(Format::Pretty)
        );
    }

    #[test]
    fn invalid_env_names_the_valid_values() {
        let err = decide(None, Some("yaml"), false).unwrap_err();
        assert!(err.contains("BARCA_OUTPUT") && err.contains("json") && err.contains("pretty"));
        // A flag makes the env irrelevant, so a bad value does not block it.
        assert_eq!(
            decide(Some(Format::Json), Some("yaml"), false),
            Ok(Format::Json)
        );
    }
}
