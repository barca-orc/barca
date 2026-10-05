//! `barca docs` — the manual, compiled into the binary so it works offline and always
//! matches the installed version.
//!
//! Topic bodies live in `crates/barca-cli/docs/*.md` (inside the crate, so they ship in the
//! sdist). Keep them in sync with CLI/decorator behavior: the tests below and in `main.rs`
//! fail if a topic is empty, unreferenced, or contains a `barca ...` command that no longer
//! parses.
//!
//! `docs/skill.md` is also the repo-root `SKILL.md` (Agent Skills format, so it starts with YAML
//! frontmatter). The root file is a copy for agents and skill installers that read the repo;
//! `skill_md_at_repo_root_matches_the_topic` keeps the two identical.

use serde_json::{Value, json};

pub struct Topic {
    pub name: &'static str,
    pub summary: &'static str,
    pub body: &'static str,
}

macro_rules! topic {
    ($name:literal, $summary:literal, $file:literal) => {
        Topic {
            name: $name,
            summary: $summary,
            body: include_str!(concat!("../docs/", $file)),
        }
    };
}

/// Every topic, in index order. Add new topics here (and a file under `docs/`).
pub const TOPICS: &[Topic] = &[
    topic!(
        "overview",
        "What barca is, the mental model, and the commands",
        "overview.md"
    ),
    topic!(
        "discovery",
        "Which files make up a project: the root, walks, [discovery], node ids",
        "discovery.md"
    ),
    topic!(
        "assets",
        "@asset, inputs, freshness, retries, node ids",
        "assets.md"
    ),
    topic!(
        "types",
        "Output formats, and how annotations pick readers (pandas, polars, pyarrow, duckdb)",
        "types.md"
    ),
    topic!("tasks", "@task, ordering-only deps, barca run", "tasks.md"),
    topic!(
        "cache",
        "Run hashes, artifacts, --refresh / --refresh-all, environments",
        "cache.md"
    ),
    topic!(
        "remote",
        "Share one cache across machines: S3, GCS, Azure, configured with environment variables",
        "remote.md"
    ),
    topic!(
        "partitions",
        "Fan-out over keys and fan-in with collect",
        "partitions.md"
    ),
    topic!(
        "sinks",
        "@sink exports to local or remote paths",
        "sinks.md"
    ),
    topic!(
        "scheduling",
        "Freshness, cron schedules, barca serve",
        "scheduling.md"
    ),
    topic!(
        "status",
        "barca status: cache state, last run and artifact shape per node",
        "status.md"
    ),
    topic!(
        "sql",
        "barca sql: query cached results with DuckDB, without writing a step",
        "sql.md"
    ),
    topic!(
        "agents",
        "Output contract, exit codes and workflows for scripts and AI agents",
        "agents.md"
    ),
    topic!(
        "contract",
        "The CLI contract: commands, flags, JSON schemas, exit codes, stability policy",
        "contract.md"
    ),
    topic!(
        "skill",
        "Agent skill (SKILL.md): the short guide an AI agent loads once",
        "skill.md"
    ),
    topic!(
        "examples",
        "Index of runnable example pipelines",
        "examples.md"
    ),
    topic!(
        "examples/duckdb",
        "DuckDB relations as a DAG, stored as parquet",
        "examples-duckdb.md"
    ),
    topic!(
        "examples/partitions",
        "Partitions with fan-in",
        "examples-partitions.md"
    ),
    topic!(
        "examples/deploy-task",
        "An asset feeding a task, run with barca run",
        "examples-deploy-task.md"
    ),
];

fn normalize(query: &str) -> String {
    query.trim().trim_matches('/').to_lowercase()
}

/// Look a topic up by name (case-insensitive; surrounding slashes ignored).
pub fn find(query: &str) -> Option<&'static Topic> {
    let q = normalize(query);
    TOPICS.iter().find(|t| t.name == q)
}

pub fn render_index() -> String {
    let width = TOPICS.iter().map(|t| t.name.len()).max().unwrap_or(0);
    let mut out = String::from(
        "Barca manual\n\n\
         Show a topic with `barca docs <topic>`; everything at once with `barca docs --all`.\n\
         Machine-readable: `barca docs --json`. Command flags: `barca <command> --help`.\n\n",
    );
    for t in TOPICS {
        out.push_str(&format!("  {:<width$}  {}\n", t.name, t.summary));
    }
    out
}

pub fn render_all() -> String {
    let mut out = String::new();
    for (i, t) in TOPICS.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("<!-- barca docs: {} -->\n", t.name));
        out.push_str(t.body);
        if !t.body.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn topic_json(t: &Topic) -> Value {
    json!({ "name": t.name, "summary": t.summary, "content": t.body })
}

pub fn index_json() -> Value {
    json!({
        "topics": TOPICS
            .iter()
            .map(|t| json!({ "name": t.name, "summary": t.summary }))
            .collect::<Vec<_>>()
    })
}

pub fn all_json() -> Value {
    json!({ "topics": TOPICS.iter().map(topic_json).collect::<Vec<_>>() })
}

/// Produce the text for `barca docs [topic] [--all] [--json]`.
/// `Ok` goes to stdout; `Err` is the stderr message (a usage error: exit code 2).
pub fn run(topic: Option<&str>, all: bool, json: bool) -> Result<String, String> {
    let pretty = |v: Value| serde_json::to_string_pretty(&v).unwrap_or_default() + "\n";
    match (topic, all) {
        (_, true) => Ok(if json {
            pretty(all_json())
        } else {
            render_all()
        }),
        (None, false) => Ok(if json {
            pretty(index_json())
        } else {
            render_index()
        }),
        (Some(name), false) => match find(name) {
            Some(t) => Ok(if json {
                pretty(topic_json(t))
            } else {
                let mut body = t.body.to_string();
                if !body.ends_with('\n') {
                    body.push('\n');
                }
                body
            }),
            None => {
                // No fuzzy "did you mean": a guess can read as confirmation. List every topic.
                let mut msg = format!("error: unknown docs topic '{name}'");
                msg.push_str("\n\nValid topics (run `barca docs` for summaries):\n");
                for t in TOPICS {
                    msg.push_str(&format!("  {}\n", t.name));
                }
                Err(msg)
            }
        },
    }
}

/// Topic names referenced as `barca docs <topic>` anywhere in `text`.
#[cfg(test)]
pub(crate) fn referenced_topics(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let marker = "barca docs ";
    let mut rest = text;
    while let Some(i) = rest.find(marker) {
        rest = &rest[i + marker.len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || *c == '/' || *c == '-')
            .collect();
        // Topic names start with a letter; this skips `--all`, `--json` and `<topic>`.
        if name.starts_with(|c: char| c.is_ascii_lowercase()) {
            found.push(name.trim_end_matches('-').to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn topic_names_are_unique_lowercase_and_nonempty() {
        let mut seen = HashSet::new();
        for t in TOPICS {
            assert!(!t.name.is_empty(), "empty topic name");
            assert_eq!(t.name, t.name.to_lowercase(), "topic names are lowercase");
            assert!(seen.insert(t.name), "duplicate topic {}", t.name);
            assert!(!t.summary.is_empty(), "{} needs a summary", t.name);
            assert!(
                t.body.trim().len() > 200,
                "{} body looks empty ({} bytes)",
                t.name,
                t.body.len()
            );
        }
    }

    /// The body after an optional `---` YAML frontmatter block (only the skill has one).
    fn without_frontmatter(body: &str) -> &str {
        match body
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---\n"))
        {
            Some((_, after)) => after.trim_start_matches('\n'),
            None => body,
        }
    }

    #[test]
    fn every_body_starts_with_an_h1_and_has_no_trailing_junk() {
        for t in TOPICS {
            assert!(
                without_frontmatter(t.body).starts_with("# "),
                "{} must start with '# Title'",
                t.name
            );
            assert!(t.body.ends_with('\n'), "{} must end with a newline", t.name);
        }
    }

    #[test]
    fn every_docs_reference_inside_topics_resolves() {
        for t in TOPICS {
            for r in referenced_topics(t.body) {
                assert!(
                    find(&r).is_some(),
                    "topic '{}' references unknown topic 'barca docs {}'",
                    t.name,
                    r
                );
            }
        }
    }

    #[test]
    fn every_topic_is_linked_from_the_overview_or_examples_index() {
        let linked = [
            find("overview").unwrap().body,
            find("examples").unwrap().body,
        ]
        .join("\n");
        let referenced: HashSet<String> = referenced_topics(&linked).into_iter().collect();
        for t in TOPICS.iter().filter(|t| t.name != "overview") {
            assert!(
                referenced.contains(t.name),
                "topic '{}' needs a `barca docs {}` link in the overview or the examples index",
                t.name,
                t.name
            );
        }
    }

    #[test]
    fn skill_has_agent_skills_frontmatter() {
        let body = find("skill").unwrap().body;
        let (front, _) = body
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---\n"))
            .expect("skill.md must start with a `---` YAML frontmatter block");
        let field = |key: &str| {
            front
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{key}: ")))
                .unwrap_or_else(|| panic!("skill frontmatter needs `{key}:`"))
                .trim()
        };
        assert_eq!(field("name"), "barca");
        let description = field("description");
        assert!(!description.is_empty());
        assert!(
            description.chars().count() <= 1024,
            "skill description is {} characters; the Agent Skills limit is 1024",
            description.chars().count()
        );
    }

    /// The repo-root SKILL.md is a copy of docs/skill.md. Skipped when the root file is not
    /// there (a crate or sdist build outside the repository).
    #[test]
    fn skill_md_at_repo_root_matches_the_topic() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../SKILL.md");
        let Ok(root_text) = std::fs::read_to_string(&root) else {
            return;
        };
        assert!(
            root_text == find("skill").unwrap().body,
            "SKILL.md at the repo root differs from crates/barca-cli/docs/skill.md; \
             edit docs/skill.md and copy it: cp crates/barca-cli/docs/skill.md SKILL.md"
        );
    }

    #[test]
    fn find_is_case_and_slash_insensitive() {
        assert_eq!(find("Types").unwrap().name, "types");
        assert_eq!(find("/examples/duckdb/").unwrap().name, "examples/duckdb");
        assert!(find("nope").is_none());
    }

    #[test]
    fn index_lists_every_topic_in_text_and_json() {
        let text = render_index();
        let json = index_json();
        let names: Vec<&str> = json["topics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), TOPICS.len());
        for t in TOPICS {
            assert!(text.contains(t.name), "index text missing {}", t.name);
            assert!(names.contains(&t.name));
        }
    }

    #[test]
    fn run_modes() {
        assert!(run(None, false, false).unwrap().contains("Barca manual"));
        let one = run(Some("cache"), false, false).unwrap();
        assert!(one.starts_with("# Caching"));
        let one_json: Value =
            serde_json::from_str(&run(Some("cache"), false, true).unwrap()).unwrap();
        assert_eq!(one_json["name"], "cache");
        assert!(
            one_json["content"]
                .as_str()
                .unwrap()
                .starts_with("# Caching")
        );
        let all = run(None, true, false).unwrap();
        for t in TOPICS {
            assert!(all.contains(&format!("<!-- barca docs: {} -->", t.name)));
        }
        let all_json: Value = serde_json::from_str(&run(None, true, true).unwrap()).unwrap();
        assert_eq!(all_json["topics"].as_array().unwrap().len(), TOPICS.len());
    }

    #[test]
    fn unknown_topic_lists_every_topic_without_guessing() {
        let err = run(Some("typs"), false, false).unwrap_err();
        assert!(err.contains("unknown docs topic 'typs'"));
        assert!(!err.to_lowercase().contains("did you mean"), "{err}");
        for t in TOPICS {
            assert!(err.contains(&format!("\n  {}\n", t.name)), "{err}");
        }
    }
}
