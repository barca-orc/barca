//! Declared environment dependencies — `@asset(env=["NAME", ...])`.
//!
//! The parser reads the names statically. At plan time the coordinator reads their values from
//! its own environment (which the workers inherit), folds `name + value` into the step's run hash
//! (an unset variable is a distinct value from an empty one), and reports the values per step.
//! Names that look like secrets are hashed but redacted in every output.
//!
//! Environment variables a function reads without declaring them are not observed.

use std::collections::BTreeMap;

/// Shown in place of a secret-looking variable's value.
pub const REDACTED: &str = "<redacted>";

/// Name suffixes treated as secrets: `*_TOKEN`, `*_SECRET`, `*_KEY`, `*_PASSWORD`
/// (case-insensitive; the bare word counts too).
const SECRET_WORDS: [&str; 4] = ["TOKEN", "SECRET", "KEY", "PASSWORD"];

/// A declared variable and its value at plan time (`None` = unset).
pub type EnvValue = (String, Option<String>);

/// Read the declared variables from this process's environment, sorted by name.
pub fn resolve(names: &[String]) -> Vec<EnvValue> {
    resolve_with(names, |n| std::env::var(n).ok())
}

/// [`resolve`] with an injectable lookup (tests).
pub fn resolve_with(names: &[String], lookup: impl Fn(&str) -> Option<String>) -> Vec<EnvValue> {
    let mut out: Vec<EnvValue> = names.iter().map(|n| (n.clone(), lookup(n))).collect();
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The canonical string folded into the run hash, or `None` when nothing is declared — so a node
/// without `env=` hashes exactly as it did before this feature existed.
pub fn hash_input(values: &[EnvValue]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    // JSON array of [name, value|null]: unambiguous for any value, and unset != "".
    Some(serde_json::to_string(values).expect("env values serialize"))
}

/// Does this variable name look like a secret?
pub fn is_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_WORDS
        .iter()
        .any(|w| upper == *w || upper.ends_with(&format!("_{w}")))
}

/// The values as reported in output: secrets redacted, unset as `None` (JSON `null`).
pub fn report(values: &[EnvValue]) -> BTreeMap<String, Option<String>> {
    values
        .iter()
        .map(|(name, value)| {
            let shown = match value {
                Some(_) if is_secret(name) => Some(REDACTED.to_string()),
                v => v.clone(),
            };
            (name.clone(), shown)
        })
        .collect()
}

/// The suffix for an `--agent` step line: ` env NAME=value OTHER=<unset>` (empty when nothing is
/// declared). Values containing whitespace or quotes, or empty values, are double-quoted.
pub fn agent_suffix(values: &[EnvValue]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = report(values)
        .into_iter()
        .map(|(name, value)| match value {
            None => format!("{name}=<unset>"),
            Some(v) if v.is_empty() || v.contains(|c: char| c.is_whitespace() || c == '"') => {
                format!("{name}={v:?}")
            }
            Some(v) => format!("{name}={v}"),
        })
        .collect();
    format!(" env {}", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_declared_contributes_nothing_to_the_hash() {
        assert_eq!(hash_input(&resolve_with(&[], |_| None)), None);
    }

    #[test]
    fn unset_and_empty_are_distinct() {
        let unset = resolve_with(&names(&["A"]), |_| None);
        let empty = resolve_with(&names(&["A"]), |_| Some(String::new()));
        assert_ne!(hash_input(&unset), hash_input(&empty));
    }

    #[test]
    fn declaration_order_does_not_matter() {
        let look = |n: &str| Some(format!("v_{n}"));
        assert_eq!(
            hash_input(&resolve_with(&names(&["A", "B"]), look)),
            hash_input(&resolve_with(&names(&["B", "A"]), look))
        );
    }

    #[test]
    fn secret_names() {
        for n in [
            "API_TOKEN",
            "aws_secret",
            "Db_Password",
            "SIGNING_KEY",
            "token",
        ] {
            assert!(is_secret(n), "{n}");
        }
        for n in ["SOURCE_CSV", "KEYRING_PATH", "TOKENIZER", "MONKEY"] {
            assert!(!is_secret(n), "{n}");
        }
    }

    #[test]
    fn report_redacts_secrets_and_keeps_unset_as_null() {
        let vals = resolve_with(&names(&["API_TOKEN", "SOURCE", "MISSING"]), |n| match n {
            "MISSING" => None,
            _ => Some("x".into()),
        });
        let r = report(&vals);
        assert_eq!(r["API_TOKEN"].as_deref(), Some(REDACTED));
        assert_eq!(r["SOURCE"].as_deref(), Some("x"));
        assert_eq!(r["MISSING"], None);
        assert_eq!(
            agent_suffix(&vals),
            " env API_TOKEN=<redacted> MISSING=<unset> SOURCE=x"
        );
    }

    #[test]
    fn agent_suffix_quotes_awkward_values() {
        let vals = resolve_with(&names(&["A", "B"]), |n| match n {
            "A" => Some("a b".into()),
            _ => Some(String::new()),
        });
        assert_eq!(agent_suffix(&vals), r#" env A="a b" B="""#);
        assert_eq!(agent_suffix(&[]), "");
    }
}
