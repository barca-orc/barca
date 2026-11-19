//! The site reference pages that are manual topics ([`SITE_PAGES`]): each is its topic with site
//! front matter and `barca docs <topic>` references turned into links, compared with the file
//! under `site/src/content/docs/` (or written there with `BARCA_UPDATE_SNAPSHOTS=1`:
//! `scripts/update-cli-snapshots.sh`). Skipped outside the repository (an sdist has no `site/`).

use crate::snapshot::{check_file, crate_dir};
use std::path::{Path, PathBuf};

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
         then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca site_pages_tests:: \
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
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site");
    dir.is_dir().then_some(dir)
}

fn site_path(site: &Path, page: &SitePage) -> PathBuf {
    site.join("src/content/docs")
        .join(format!("{}.md", page.slug))
}

/// The site publishes the contract topic verbatim. Called by the contract test after it has
/// updated contract.md. Skipped outside the repository (an sdist build has no `site/`).
pub(crate) fn check_site_page(topic: &str) {
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
