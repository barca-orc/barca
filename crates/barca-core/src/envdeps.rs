//! Declared environment dependencies — `@asset(env=["NAME", ...])`.
//!
//! The parser reads the names statically. At plan time the coordinator reads their values from
//! its own environment (which the workers inherit) and, for a name that is not set there, from
//! the project's dotenv file (`.env` in the project root, [`DOTENV`]), folds `name + value` into
//! the step's run hash (an unset variable is a distinct value from an empty one), and reports the
//! values per step. Names that look like secrets are hashed but redacted in every output.
//!
//! The dotenv file is read the way `python-dotenv`'s `load_dotenv()` loads it by default, so a
//! pipeline that loads it at import is hashed with the value it runs with (#575): the process
//! environment wins over the file. Barca reads the file without importing anything and does not
//! set its values for the workers; a pipeline that uses them loads the file itself.
//!
//! Environment variables a function reads without declaring them are not observed.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

/// The dotenv file declared values are also read from, relative to the project root (barca's
/// working directory).
pub const DOTENV: &str = ".env";

/// Shown in place of a secret-looking variable's value.
pub const REDACTED: &str = "<redacted>";

/// Name suffixes treated as secrets: `*_TOKEN`, `*_SECRET`, `*_KEY`, `*_PASSWORD`
/// (case-insensitive; the bare word counts too).
const SECRET_WORDS: [&str; 4] = ["TOKEN", "SECRET", "KEY", "PASSWORD"];

/// A declared variable and its value at plan time (`None` = unset).
pub type EnvValue = (String, Option<String>);

/// Read the declared variables, sorted by name: from this process's environment, else from the
/// project's dotenv file ([`DOTENV`]).
pub fn resolve(names: &[String]) -> Vec<EnvValue> {
    if names.is_empty() {
        return Vec::new();
    }
    let file = dotenv(Path::new(DOTENV));
    resolve_with(names, |n| {
        std::env::var(n)
            .ok()
            .or_else(|| file.as_ref().and_then(|f| f.get(n).cloned()))
    })
}

/// A dotenv file's values, shared by the steps a plan hashes.
type Values = std::sync::Arc<HashMap<String, String>>;

/// The values in the dotenv file at `path`, or `None` when there is no readable file. Kept for
/// as long as the file's size and modification time stay the same: plan-time hashing reads it
/// once per step.
fn dotenv(path: &Path) -> Option<Values> {
    type Seen = (std::path::PathBuf, u64, Option<SystemTime>);
    static CACHE: Mutex<Option<(Seen, Values)>> = Mutex::new(None);
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let seen: Seen = (path.to_path_buf(), meta.len(), meta.modified().ok());
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((key, values)) = cache.as_ref()
        && *key == seen
    {
        return Some(values.clone());
    }
    let text = std::fs::read_to_string(path).ok()?;
    let values = std::sync::Arc::new(parse_dotenv(&text, |n| std::env::var(n).ok()));
    *cache = Some((seen, values.clone()));
    Some(values)
}

/// Parse dotenv text the way `python-dotenv` does for the common forms: `KEY=VALUE` lines with an
/// optional `export ` prefix, `#` comments (a whole line, or after an unquoted value following
/// whitespace), values in single quotes (literal) or double quotes (with `\n`, `\t`, `\"` and
/// `\\` escapes, and spanning lines), and `${NAME}` / `${NAME:-default}` expanded in unquoted and
/// double-quoted values from `env` first, then from the lines above. A later line for the same
/// key wins. A line without `=` is ignored.
pub fn parse_dotenv(text: &str, env: impl Fn(&str) -> Option<String>) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let rest = rest.trim_start();
        let value = if let Some(body) = rest.strip_prefix('\'') {
            match body.find('\'') {
                Some(end) => body[..end].to_string(),
                None => body.trim_end().to_string(),
            }
        } else if let Some(body) = rest.strip_prefix('"') {
            let mut raw = body.to_string();
            // A double-quoted value may span lines until its closing quote.
            let end = loop {
                if let Some(end) = closing_quote(&raw) {
                    break Some(end);
                }
                match lines.next() {
                    Some(more) => {
                        raw.push('\n');
                        raw.push_str(more);
                    }
                    None => break None,
                }
            };
            let raw = match end {
                Some(end) => &raw[..end],
                None => raw.trim_end(),
            };
            expand(&unescape(raw), &env, &out)
        } else {
            let cut = rest
                .char_indices()
                .find(|&(i, c)| c == '#' && i > 0 && rest[..i].ends_with(char::is_whitespace))
                .map_or(rest.len(), |(i, _)| i);
            expand(rest[..cut].trim_end(), &env, &out)
        };
        out.insert(key.to_string(), value);
    }
    out
}

/// The byte index of the first unescaped `"` in `s`.
fn closing_quote(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(i),
            _ => {}
        }
    }
    None
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other @ ('"' | '\\' | '\'')) => out.push(other),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Expand `${NAME}` and `${NAME:-default}`: the environment first, then the file's lines above.
fn expand(
    s: &str,
    env: &impl Fn(&str) -> Option<String>,
    above: &HashMap<String, String>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start + 2..].find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        let inner = &rest[start + 2..start + 2 + len];
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (inner, None),
        };
        let value = env(name)
            .or_else(|| above.get(name).cloned())
            .filter(|v| default.is_none() || !v.is_empty());
        out.push_str(&value.unwrap_or_else(|| default.unwrap_or("").to_string()));
        rest = &rest[start + 3 + len..];
    }
    out.push_str(rest);
    out
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

    fn parse(text: &str) -> HashMap<String, String> {
        parse_dotenv(text, |n| (n == "HOME").then(|| "/home/me".to_string()))
    }

    #[test]
    fn dotenv_lines_comments_export_and_quotes() {
        let v = parse(
            "# a comment\n\
             PLAIN=a.csv\n\
             export EXPORTED = b.csv  # trailing comment\n\
             HASH=c#d\n\
             SINGLE='lit ${HOME} \\n'\n\
             DOUBLE=\"x \\\"y\\\" \\n z\" # after\n\
             EMPTY=\n\
             not a line\n\
             LATER=1\n\
             LATER=2\n",
        );
        assert_eq!(v["PLAIN"], "a.csv");
        assert_eq!(v["EXPORTED"], "b.csv");
        assert_eq!(v["HASH"], "c#d");
        assert_eq!(v["SINGLE"], "lit ${HOME} \\n");
        assert_eq!(v["DOUBLE"], "x \"y\" \n z");
        assert_eq!(v["EMPTY"], "");
        assert_eq!(v["LATER"], "2");
        assert!(!v.contains_key("not a line"));
    }

    #[test]
    fn dotenv_double_quoted_values_span_lines() {
        let v = parse("CERT=\"line one\nline two\"\nNEXT=n\n");
        assert_eq!(v["CERT"], "line one\nline two");
        assert_eq!(v["NEXT"], "n");
    }

    #[test]
    fn dotenv_expands_from_the_environment_then_the_lines_above() {
        let v = parse(
            "ROOT=data\nCSV=${ROOT}/x.csv\nCACHE=${HOME}/c\nDEF=${UNSET:-fallback}\nGONE=${UNSET}\n",
        );
        assert_eq!(v["CSV"], "data/x.csv");
        assert_eq!(v["CACHE"], "/home/me/c");
        assert_eq!(v["DEF"], "fallback");
        assert_eq!(v["GONE"], "");
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
