//! Barca CLI — invisible asset orchestrator.

mod bounded;
#[cfg(test)]
mod contract;
mod docs;
mod error;
mod output;

use error::{CliError, Context, ErrorKind, shell_quote};

use clap::builder::PossibleValuesParser;
use clap::{Parser, ValueEnum};
use output::{Format, FormatFlags};
use std::io::Write;
use std::path::{Path, PathBuf};

/// `-o` on get/run. Without `-o`, `--json` / `--pretty` / BARCA_OUTPUT / the terminal decide.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputMode {
    /// One-line JSON (default when stdout is not a terminal)
    Json,
    /// Just the final_output value, pretty-printed
    Value,
    /// Human-friendly with timing info (default when stdout is a terminal)
    Pretty,
}

impl OutputMode {
    /// `-o` wins; otherwise the shared rule in `output::resolve`.
    fn resolve(o: Option<OutputMode>, flags: FormatFlags) -> OutputMode {
        o.unwrap_or_else(|| match output::resolve(flags.explicit()) {
            Format::Json => OutputMode::Json,
            Format::Pretty => OutputMode::Pretty,
        })
    }
}

// ─── Help text ────────────────────────────────────────────────────────────────
//
// Every command carries runnable examples. The tests at the bottom of this file parse each
// `barca ...` example line against the real CLI, so these cannot drift from the flags.
// When you add or change a flag, update the examples here and the matching `barca docs`
// topic in crates/barca-cli/docs/.

const TOP_HELP: &str = "\
Quick start:
  barca list pipeline.py            # discover assets, tasks and their dependencies
  barca get total pipeline.py       # run only what `total` needs (cached on re-run)
  barca run deploy pipeline.py      # run a task and its dependency cone
  barca docs                        # built-in manual: concepts, formats, examples

Output: results go to stdout as tables/summaries in a terminal and as JSON when piped or
captured; --json / --pretty (or BARCA_OUTPUT=json|pretty) override. Progress and errors go to
stderr. In JSON mode an error is one JSON line on stderr: {error, code, kind, remediation}.
Exit codes: 0 ok, 1 step failed, 2 usage error, 3 barca/infra failure, 130 cancelled.
Scripts and AI agents: barca docs skill (short, start here), barca docs agents (full contract)";

const GET_HELP: &str = "\
Examples:
  barca get pipeline.py                    # every asset and sensor, never tasks; prints the last asset's value
  barca get total pipeline.py              # one target and only its upstream cone
  barca get total,orders pipeline.py       # several targets in one run; shared upstream runs once
  barca get total pipeline.py other.py     # target defined across several files
  barca get total pipeline.py --refresh-all        # recompute everything in that cone
  barca get total pipeline.py --refresh clean      # recompute clean and everything downstream of it
  barca get total pipeline.py --refresh clean --no-cascade   # recompute only clean
  barca get total pipeline.py --dry-run    # what would run vs come from cache; changes nothing
  barca get total pipeline.py --json       # JSON even in a terminal (the default when piped)
  barca get total pipeline.py --pretty     # summary and value for humans (the default in a terminal)
  barca get total pipeline.py -o value     # just the value, pretty-printed
  barca get total pipeline.py --agent      # plain progress lines on stderr
  barca get total pipeline.py --env dev    # separate cache and state per environment
  barca list pipeline.py                   # not sure of the name? list assets and tasks first
  BARCA_OUTPUT=json barca get total pipeline.py   # env override for CI; a flag still wins
  barca get total pipeline.py --fields id,status   # JSON with each entry of `steps` trimmed to these keys

Output format: --json / --pretty / -o, else BARCA_OUTPUT=json|pretty, else the terminal decides
(TTY -> pretty, piped -> JSON). JSON is one line on stdout with status (\"success\"), run_id,
steps_executed (0 = all cached), phases, final_output, and `steps`: what happened to each step
(ran or cached, and why; `env` holds the values of variables the node declares with env=[...],
secrets redacted). For parquet/pickle assets final_output is a pointer,
{\"_barca_artifact\": {\"path\", \"format\", \"size_bytes\"}}; the Python API (barca.get)
loads the value for you.
Several targets (`a,b`, comma-separated, no spaces): final_output is replaced by `targets`, keyed by
target, each {status: success, final_output} or {status: failed, failed_node, error}. Every target
runs even if another fails; exit 1 if any failed.
Refresh: the same vocabulary as `barca run`. --refresh takes ONE comma-separated list of assets in
the cone (the target itself may be named) and also re-runs everything downstream of them;
--no-cascade re-runs only the named ones. --refresh-all re-runs every asset in the cone.
--no-cache is a deprecated spelling of --refresh-all: it still works and warns on stderr.
Targets must be assets; use `barca run` for tasks. With no target, get materializes every asset and
sensor and skips tasks (previously it ran tasks too); stderr names the skipped tasks and the
`barca run` command. A file with only tasks gets nothing: exit 0, empty `steps`.
The target comes before the files: `barca get pipeline.py total` exits 2 and prints
`barca get total pipeline.py`.
Errors: in JSON mode the last stderr line is one JSON object {error, code, kind, remediation},
plus node, traceback and artifact_dir when a step failed. Exit 1 step failed, 2 usage error,
3 barca/infra failure, 130 cancelled. A failed step still prints a stdout result line with
status \"failed\" and failed_node.
More: barca docs cache, barca docs types, barca docs agents";

const RUN_HELP: &str = "\
Examples:
  barca run deploy pipeline.py                         # task runs; upstream assets come from cache
  barca run deploy pipeline.py --refresh fetch,clean   # re-materialize these and everything downstream of them
  barca run deploy pipeline.py --refresh fetch --no-cascade   # re-materialize only fetch; downstream stays cached
  barca run deploy pipeline.py --refresh-all           # re-materialize every upstream asset
  barca run deploy pipeline.py --dry-run --refresh fetch   # preview: which steps run, which are cached
  barca run deploy pipeline.py --json                  # JSON even in a terminal (the default when piped)
  barca run deploy pipeline.py --pretty                # summary for humans (the default in a terminal)
  barca run deploy pipeline.py --fields id,status,reason   # JSON with each entry of `steps` trimmed
  barca run check_a,check_b pipeline.py                # several tasks in one run; shared upstream runs once
  barca run check_a,check_b pipeline.py --dry-run      # preview the union of both cones
  barca list pipeline.py                               # not sure of the name? list assets and tasks first

Several targets: comma-separated, no spaces. Every target runs even if another fails (a failure
skips only what depends on it); exit 1 if any failed. JSON output then carries `targets`, keyed by
target, instead of `final_output` (see barca get --help, barca docs agents).

--refresh takes ONE comma-separated list (`--refresh a,b`), never `--refresh a b`. It re-runs the
assets you name and every asset downstream of them in the task's cone (reason `refresh_cascade`),
so fresh data reaches the task. --no-cascade re-runs only the named assets; cached assets
downstream of them then do not reflect the refresh, and barca warns. A name that is not an
upstream asset is an error. --no-cache is a deprecated spelling of --refresh-all: it still works
and warns on stderr.
The target must be a task; use `barca get` for assets. The target comes before the files:
`barca run pipeline.py deploy` exits 2 and prints `barca run deploy pipeline.py`. Every usage
error exits 2 and ends by pointing at `barca list <files>`.
Errors: in JSON mode the last stderr line is one JSON object {error, code, kind, remediation}
(see barca docs agents). Exit 1 step failed, 2 usage error, 3 barca/infra failure, 130 cancelled.
A raising task, or one that calls sys.exit(), fails the run: exit 1, and the stdout JSON line has
status \"failed\" and failed_node.
More: barca docs tasks, barca docs cache, barca docs agents";

const PLAN_HELP: &str = "\
Examples:
  barca plan pipeline.py              # phases and steps that would run; nothing executes
  barca plan pipeline.py other.py     # several files form one DAG

Output: always pretty-printed JSON {total_steps, phases: [{reason, streams: [{stream_id, steps}]}]}.
`reason` is an object: {\"type\": \"initial\"} or {\"type\": \"fan_in\", \"node_id\": ...}.
Planning is static analysis: it never imports your code or reads state, so it takes no --env.
Experimental: the layout may change between releases (barca docs contract).
More: barca docs agents";

const HISTORY_HELP: &str = "\
Examples:
  barca history                # last 10 runs: a table in a terminal, JSON when piped
  barca history -l 25          # last 25
  barca history --all          # every recorded run
  barca history --json         # {runs: [...], total, truncated, hint?}, even in a terminal
  barca history --pretty       # the table, even when piped
  barca history --fields run_id,status,elapsed_seconds   # JSON with only these keys per run
  barca history --env dev      # runs recorded in another environment

Newest first. When more runs exist than are shown, JSON says `\"truncated\": true` with the
`total`, and the table prints a one-line note on stderr. In JSON, `files` is an array: the .py
files the run was given.
More: barca docs agents, barca docs cache";

const STATS_HELP: &str = "\
Examples:
  barca stats total pipeline.py             # timing percentiles and cache hit rate
  barca stats total pipeline.py --json      # {id, total_runs, cache_hit_rate, ..., recent_runs}, even in a terminal
  barca stats total pipeline.py --pretty    # the text report, even when piped
  barca stats total pipeline.py --fields status,error_message   # JSON; trims recent_runs entries

More: barca docs cache";

const SERVE_HELP: &str = "\
Examples:
  barca serve pipeline.py                    # HTTP API on 127.0.0.1:8274 plus the scheduler (files required)
  barca serve pipeline.py --port 8400        # custom port
  barca serve pipeline.py --watch            # dev: re-parse the DAG when files change
  barca serve pipeline.py --no-schedule      # API only; Schedule(...) nodes do not fire
  barca serve pipeline.py --timezone utc     # evaluate cron in UTC (default: local)

Binds to localhost with no authentication.
More: barca docs scheduling";

const LIST_HELP: &str = "\
Examples:
  barca list pipeline.py             # table of nodes (in a terminal; JSON when piped)
  barca list pipeline.py --json      # {nodes: [{id, kind, freshness, schedule?, inputs, env, next_fire?}], total, truncated}
  barca list pipeline.py --pretty    # the table, even when piped
  barca list pipeline.py --fields id,inputs   # JSON with only these keys per node
  barca list big.py --limit 20       # first 20 nodes (topological order)
  barca list big.py --all            # every node (default: at most 100)
  barca list a.py b.py               # several files form one DAG

An ENV column (and `env` in JSON) lists the environment variables each node declares with
@asset(env=[...]); their values are part of the run hash. `freshness` is `always`, `manual` or
`schedule`; a scheduled node also has `schedule` (the cron expression) and `next_fire`.
`list` reads no state, so it takes no --env.

Run this first to confirm barca discovered your nodes. When more nodes exist than are shown,
JSON says `\"truncated\": true` with the `total`, and the table prints a note on stderr.
More: barca docs assets, barca docs agents";

const STATUS_HELP: &str = "\
Examples:
  barca status pipeline.py                 # table in a terminal (JSON when piped): kind, cache state, last run, shape
  barca status total pipeline.py           # only `total` and its upstream cone
  barca status total,orders pipeline.py    # several targets: the union of their cones
  barca status pipeline.py --json          # {target, targets, nodes, summary, total, truncated}, even in a terminal
  barca status pipeline.py --pretty        # the table, even when piped
  barca status pipeline.py --fields id,cache   # JSON with only these keys per node
  barca status big.py --limit 20           # first 20 nodes (default: at most 100); the summary counts all
  barca status total pipeline.py --json --sample 5   # add up to 5 sample rows per json/parquet artifact
  barca status pipeline.py --env dev       # state recorded in another environment

Cache state per node: cached, stale (ran before; code or inputs changed), never_run, partial
(some partition keys cached), unknown (dynamic partitions not yet known) or always_runs (tasks,
sensors), with a reason. JSON spells the states in snake_case, the same as the `summary` keys
(the table prints never-run, always-runs). It is the same decision `--dry-run` makes.
Read-only: never imports your code, never writes. Shape (rows, columns, type) is read from the
artifact file only.
More: barca docs status, barca docs agents";

const DOCS_HELP: &str = "\
Examples:
  barca docs                    # topic index with one-line summaries
  barca docs types              # one topic as markdown
  barca docs examples/duckdb    # a runnable example pipeline
  barca docs skill              # the agent skill (SKILL.md, with frontmatter): save it to install
  barca docs --all              # the whole manual in one stream (paste into context)
  barca docs --json             # topic index as JSON
  barca docs cache --json       # one topic as JSON {name, summary, content}
  barca docs --fields name      # JSON index with only topic names

Topics are compiled into the binary: offline, and always matching this version.";

#[derive(Parser)]
#[command(
    name = "barca",
    about = "Invisible asset orchestrator. Discover a project's assets and tasks with `barca list <file.py>`",
    long_about = "Barca runs Python asset graphs with content-addressed caching.\n\
                  Every asset output is fully materialized to an artifact file at step \
                  boundaries (json, pickle, or parquet) — that persistence is the cache \
                  checkpoint. pandas/polars DataFrames, pyarrow Tables and duckdb relations \
                  are written as parquet; parameter type annotations choose how downstream \
                  steps read it back (pandas by default, or polars, pyarrow, duckdb) but do \
                  not skip materialization. Start with `barca list <file.py>` to discover a \
                  project's assets and tasks; run `barca docs` for the manual.",
    after_help = TOP_HELP,
    version
)]
enum Cli {
    /// Get asset value(s) — cache-aware, runs only the needed subgraph
    ///
    /// If the first positional arg ends in .py, all args are treated as files
    /// (no target — gets every asset and sensor; tasks are skipped, use `barca run` for
    /// them). Otherwise, the first arg is the target
    /// asset name (or several, comma-separated: `a,b`) and the rest are files.
    ///
    /// Each completed step writes a fully materialized artifact (never a lazy in-memory
    /// handle). If one computation should produce several cacheable outputs, define
    /// multiple assets or split the work inside a single step before returning.
    #[command(after_help = GET_HELP)]
    Get {
        /// [TARGET[,TARGET...]] file.py [file.py ...] — target is optional
        #[arg(required = true)]
        args: Vec<String>,
        /// Output format (kept for compatibility; --json / --pretty are the canonical spelling)
        #[arg(short, long, conflicts_with_all = ["json", "pretty"])]
        output: Option<OutputMode>,
        #[command(flatten)]
        format: FormatFlags,
        /// Assets to force re-materialize, as ONE comma-separated list (`--refresh a,b`, not
        /// `--refresh a b`); the target itself may be named. Every asset downstream of them in
        /// the target's cone re-materializes too (see --no-cascade)
        #[arg(long, value_delimiter = ',', conflicts_with_all = ["refresh_all", "no_cache"])]
        refresh: Option<Vec<String>>,
        /// With --refresh: re-materialize only the named assets, not what is downstream of
        /// them. Cached downstream assets then do not reflect the refresh; barca warns
        #[arg(long, requires = "refresh")]
        no_cascade: bool,
        /// Force re-materialize EVERY asset in the target's cone (nothing comes from cache)
        #[arg(long)]
        refresh_all: bool,
        /// Deprecated spelling of --refresh-all (prints a warning; removed in a future minor)
        #[arg(long, hide = true, conflicts_with = "refresh_all")]
        no_cache: bool,
        /// Show what this command would do (each step cached or will-run, and why) without
        /// running or writing anything
        #[arg(long)]
        dry_run: bool,
        /// Agent-friendly output: plain structured progress lines instead of visual progress bar
        #[arg(long)]
        agent: bool,
        /// Keep only these keys (comma-separated) on each entry of `steps` in the JSON output.
        /// Not valid with -o value/pretty. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STEP_FIELDS))]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Run a task and its dependency cone — the task always re-runs
    ///
    /// The task always re-runs. Upstream assets are served from cache when fresh
    /// (same as `barca get`). Use `--refresh` to force re-materialize specific
    /// upstream assets, or `--refresh-all` to refresh the entire upstream cone.
    #[command(after_help = RUN_HELP)]
    Run {
        /// TARGET[,TARGET...] file.py [file.py ...] — one or more target tasks, comma-separated
        #[arg(required = true)]
        args: Vec<String>,
        /// Upstream assets to force re-materialize, as ONE comma-separated list
        /// (`--refresh a,b`, not `--refresh a b`). Every asset downstream of them in the
        /// task's cone re-materializes too (see --no-cascade)
        #[arg(long, value_delimiter = ',', conflicts_with_all = ["refresh_all", "no_cache"])]
        refresh: Option<Vec<String>>,
        /// With --refresh: re-materialize only the named assets, not what is downstream of
        /// them. Cached downstream assets then do not reflect the refresh; barca warns
        #[arg(long, requires = "refresh")]
        no_cascade: bool,
        /// Force re-materialize EVERY asset in the task's cone (nothing comes from cache)
        #[arg(long)]
        refresh_all: bool,
        /// Deprecated spelling of --refresh-all (prints a warning; removed in a future minor)
        #[arg(long, hide = true, conflicts_with = "refresh_all")]
        no_cache: bool,
        /// Show what this command would do (each step cached or will-run, and why) without
        /// running or writing anything
        #[arg(long)]
        dry_run: bool,
        /// Output format (kept for compatibility; --json / --pretty are the canonical spelling)
        #[arg(short, long, conflicts_with_all = ["json", "pretty"])]
        output: Option<OutputMode>,
        #[command(flatten)]
        format: FormatFlags,
        /// Agent-friendly output: plain structured progress lines instead of visual progress bar
        #[arg(long)]
        agent: bool,
        /// Keep only these keys (comma-separated) on each entry of `steps` in the JSON output.
        /// Not valid with -o value/pretty. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STEP_FIELDS))]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Parse source files and emit the execution plan as JSON
    #[command(after_help = PLAN_HELP)]
    Plan {
        /// Python source files containing @asset definitions
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Show recent run history
    #[command(after_help = HISTORY_HELP)]
    History {
        /// Number of recent runs to show
        #[arg(short, long, default_value = "10")]
        limit: usize,
        /// Show every recorded run (no limit)
        #[arg(long, conflicts_with = "limit")]
        all: bool,
        #[command(flatten)]
        format: FormatFlags,
        /// Output JSON with only these keys (comma-separated) on each entry of `runs`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::HISTORY_FIELDS))]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Show execution statistics for an asset
    #[command(after_help = STATS_HELP)]
    Stats {
        /// Target asset function name
        target: String,
        /// Python source files containing @asset definitions
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        format: FormatFlags,
        /// Output JSON with only these keys (comma-separated) on each entry of `recent_runs`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STATS_FIELDS))]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Run a long-running HTTP server exposing the orchestrator as a JSON API
    ///
    /// Binds to 127.0.0.1 (local only, no auth). POST /run and /get trigger
    /// async runs; poll GET /status/<run_id> for results.
    #[command(after_help = SERVE_HELP)]
    Serve {
        /// Python source files defining the DAG to serve
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Port to bind on
        #[arg(short, long, default_value = "8274")]
        port: u16,
        /// Dev mode: re-parse the DAG when source files change
        #[arg(long)]
        watch: bool,
        /// Disable the cron scheduler (Schedule(...) assets will not auto-fire)
        #[arg(long)]
        no_schedule: bool,
        /// Timezone for cron evaluation: local (default), utc, or an IANA name
        #[arg(long, default_value = "local")]
        timezone: String,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// List all discovered definitions (assets, tasks, sensors) with their deps
    ///
    /// Scheduled definitions also show their next fire time in local time.
    #[command(after_help = LIST_HELP)]
    List {
        /// Python source files containing definitions
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        format: FormatFlags,
        /// Maximum number of nodes to show, in topological order
        #[arg(short, long, default_value_t = bounded::LIST_DEFAULT_LIMIT)]
        limit: usize,
        /// Show every node (no limit)
        #[arg(long, conflicts_with = "limit")]
        all: bool,
        /// Output JSON with only these keys (comma-separated) on each entry of `nodes`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::LIST_FIELDS))]
        fields: Option<Vec<String>>,
    },
    /// Show every node's cache state, last materialization and artifact shape (read-only)
    ///
    /// One aggregated view: what `barca list`, `--dry-run`, `barca history` and a look inside the
    /// artifact would each tell you. If the first positional arg ends in .py, all args are files;
    /// otherwise the first is a target (or several, comma-separated: `a,b`) and only the
    /// upstream cones of the targets are shown.
    #[command(after_help = STATUS_HELP)]
    Status {
        /// [TARGET[,TARGET...]] file.py [file.py ...] — target is optional
        #[arg(required = true)]
        args: Vec<String>,
        #[command(flatten)]
        format: FormatFlags,
        /// Maximum number of nodes to show, in topological order
        #[arg(short, long, default_value_t = bounded::LIST_DEFAULT_LIMIT)]
        limit: usize,
        /// Show every node (no limit)
        #[arg(long, conflicts_with = "limit")]
        all: bool,
        /// Output JSON with only these keys (comma-separated) on each entry of `nodes`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STATUS_FIELDS))]
        fields: Option<Vec<String>>,
        /// Include up to N sample rows from each json/parquet artifact (off by default; pickles
        /// are never sampled)
        #[arg(long, value_name = "N")]
        sample: Option<usize>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Show the built-in manual: concepts, output formats, examples, agent conventions
    ///
    /// Topics are compiled into the binary, so this works offline and always matches the
    /// installed version. With no topic it prints an index.
    #[command(after_help = DOCS_HELP)]
    Docs {
        /// Topic to show (omit for the index), e.g. types, cache, examples/duckdb
        topic: Option<String>,
        /// Print every topic in one stream
        #[arg(long, conflicts_with = "topic")]
        all: bool,
        /// Emit JSON instead of markdown
        #[arg(long)]
        json: bool,
        /// Output JSON with only these keys (comma-separated) on each entry of `topics`, or on
        /// the one topic. Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::DOCS_FIELDS))]
        fields: Option<Vec<String>>,
    },
    /// Print version information
    Version,
}

/// The line every usage error ends with (see [`error::list_hint`]: one wording everywhere).
fn list_hint(files: &[PathBuf]) -> String {
    let files: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    error::list_hint(&files)
}

/// A `get`/`run` usage error (exit 2), ending with the `barca list` pointer.
fn usage_error(msg: &str, files: &[PathBuf]) -> CliError {
    CliError::from_prose(ErrorKind::Usage, format!("{msg}\n\n{}", list_hint(files)))
}

/// Usage line for `barca get` / `barca run`.
fn usage_line(sub: &str) -> &'static str {
    match sub {
        "run" => RUN_USAGE,
        "status" => STATUS_USAGE,
        _ => GET_USAGE,
    }
}

/// The cache policy of `get` / `run`: one vocabulary on both (`--refresh a,b`, `--no-cascade`,
/// `--refresh-all`). `--no-cache` is the deprecated spelling of `--refresh-all`.
fn cache_policy(
    refresh: Option<Vec<String>>,
    no_cascade: bool,
    refresh_all: bool,
    no_cache: bool,
) -> barca_core::commands::CachePolicy {
    use barca_core::commands::CachePolicy;
    if no_cache {
        eprintln!(
            "[barca] warning: --no-cache is deprecated and will be removed in a future minor \
             release; use --refresh-all"
        );
    }
    match (refresh_all || no_cache, refresh) {
        (true, _) => CachePolicy::RefreshAll,
        (false, Some(names)) => CachePolicy::RefreshSelective {
            names,
            cascade: !no_cascade,
        },
        (false, None) => CachePolicy::CacheAware,
    }
}

/// Positionals in the wrong order: the first ends in `.py` and a later one does not. With
/// exactly one such name there is one valid reading, so the error prints the corrected command
/// (same files and flags, target first). With several it states the rule without guessing.
/// `raw` is the command line after the program name, used to carry the flags over.
fn wrong_order_error(sub: &str, args: &[String], raw: &[String]) -> Option<String> {
    let first = args.first()?;
    if !first.ends_with(".py") {
        return None;
    }
    let targets: Vec<&String> = args.iter().filter(|a| !a.ends_with(".py")).collect();
    let files: Vec<PathBuf> = args
        .iter()
        .filter(|a| a.ends_with(".py"))
        .map(PathBuf::from)
        .collect();
    let hint = list_hint(&files);
    match targets.as_slice() {
        [] => None,
        [target] => {
            // Flags: the raw command line minus the subcommand and the positionals, in order.
            let mut rest = raw;
            if rest.first().map(String::as_str) == Some(sub) {
                rest = &rest[1..];
            }
            let mut pending = args.iter().peekable();
            let flags: Vec<String> = rest
                .iter()
                .filter(|tok| {
                    if pending.peek() == Some(tok) {
                        pending.next();
                        false
                    } else {
                        true
                    }
                })
                .map(|t| shell_quote(t))
                .collect();
            let mut cmd = vec!["barca".to_string(), sub.to_string(), shell_quote(target)];
            cmd.extend(files.iter().map(|f| shell_quote(&f.to_string_lossy())));
            cmd.extend(flags);
            Some(format!(
                "error: the target comes before the files\n\n  {}\n\n{hint}",
                cmd.join(" ")
            ))
        }
        several => {
            let names: Vec<String> = several.iter().map(|t| format!("'{t}'")).collect();
            Some(format!(
                "error: the target comes before the files (found {} after '{first}')\n\n{}\n\n{hint}",
                names.join(", "),
                usage_line(sub)
            ))
        }
    }
}

/// A usage error (exit 2) with the corrected command when the positionals are in the wrong
/// order.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
fn check_order(sub: &str, args: &[String]) -> Result<(), CliError> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match wrong_order_error(sub, args, &raw) {
        Some(msg) => Err(CliError::from_prose(ErrorKind::Usage, msg)),
        None => Ok(()),
    }
}

/// Reject file arguments that are not `.py` files, with a hint for the most common mistake:
/// passing several assets to `--refresh` separated by spaces instead of commas.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
fn check_py_files(files: &[PathBuf], refresh: Option<&[String]>) -> Result<(), CliError> {
    let Some(bad) = files.iter().find(|f| !f.to_string_lossy().ends_with(".py")) else {
        return Ok(());
    };
    let bad = bad.to_string_lossy();
    let mut msg = format!("error: '{bad}' is not a .py file.");
    if let Some(names) = refresh {
        let mut all: Vec<String> = names.to_vec();
        all.push(bad.to_string());
        msg.push_str(&format!(
            "\n\nIf you meant to refresh several assets, join them with commas: --refresh {}",
            all.join(",")
        ));
    }
    let py: Vec<PathBuf> = files
        .iter()
        .filter(|f| f.to_string_lossy().ends_with(".py"))
        .cloned()
        .collect();
    Err(usage_error(&msg, &py))
}

const GET_USAGE: &str =
    "Usage: barca get [TARGET] <FILES>... [--refresh a,b [--no-cascade] | --refresh-all]";
const STATUS_USAGE: &str = "Usage: barca status [TARGET] <FILES>...";
const RUN_USAGE: &str = "Usage: barca run <TARGET> <FILES>... [--refresh a,b | --refresh-all]";

/// Errors from a `get`/`run` that are the caller's mistake (unknown target, task/asset misuse,
/// unknown `--refresh` name) end with the `barca list` pointer, like every other get/run usage
/// error.
fn get_run_error(e: barca_core::BarcaError, ctx: &Context, files: &[PathBuf]) -> CliError {
    let usage = matches!(
        e,
        barca_core::BarcaError::Usage(_) | barca_core::BarcaError::AssetNotFound(..)
    );
    let err = CliError::from_barca(e, ctx);
    if usage {
        err.with_final_hint(list_hint(files))
    } else {
        err
    }
}

/// Whether this invocation's output mode is JSON, which makes errors a JSON envelope on
/// stderr (see error.rs). It follows the same rule as results (output.rs): get/run/list/
/// history/stats print JSON when a flag or BARCA_OUTPUT says so, or when stdout is not a
/// terminal; `plan` is always JSON; `docs` only with `--json`.
fn json_output(cli: &Cli) -> bool {
    match cli {
        Cli::Get {
            output,
            format,
            fields,
            ..
        }
        | Cli::Run {
            output,
            format,
            fields,
            ..
        } => get_run_mode(*output, *format, fields.as_deref())
            .is_ok_and(|m| matches!(m, OutputMode::Json)),
        Cli::Plan { .. } => true,
        Cli::History { format, fields, .. }
        | Cli::Stats { format, fields, .. }
        | Cli::List { format, fields, .. }
        | Cli::Status { format, fields, .. } => {
            fields_json(*format, fields.as_deref()).unwrap_or(false)
        }
        Cli::Docs { json, fields, .. } => *json || fields.is_some(),
        Cli::Serve { .. } | Cli::Version => false,
    }
}

/// The command name and files of an invocation, for remediation hints.
fn context(cli: &Cli) -> Context {
    let paths = |files: &[PathBuf]| files.iter().map(|p| p.display().to_string()).collect();
    let (command, files) = match cli {
        Cli::Get { args, .. } => ("get", paths(&split_target_files(args.clone()).1)),
        Cli::Run { args, .. } => ("run", paths(&split_target_files(args.clone()).1)),
        Cli::Plan { files, .. } => ("plan", paths(files)),
        Cli::Stats { files, .. } => ("stats", paths(files)),
        Cli::Serve { files, .. } => ("serve", paths(files)),
        Cli::List { files, .. } => ("list", paths(files)),
        Cli::Status { args, .. } => ("status", paths(&split_target_files(args.clone()).1)),
        Cli::History { .. } => ("history", Vec::new()),
        Cli::Docs { .. } => ("docs", Vec::new()),
        Cli::Version => ("version", Vec::new()),
    };
    Context { command, files }
}

/// The get/run output mode. `--fields` trims JSON, so it implies JSON; combined with an
/// explicit human mode (`-o value`, `-o pretty`, `--pretty`) it is a usage error.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
fn get_run_mode(
    output: Option<OutputMode>,
    format: FormatFlags,
    fields: Option<&[String]>,
) -> Result<OutputMode, CliError> {
    if fields.is_none() {
        return Ok(OutputMode::resolve(output, format));
    }
    match (output, format.pretty) {
        (Some(OutputMode::Value | OutputMode::Pretty), _) | (_, true) => Err(CliError::from_prose(
            ErrorKind::Usage,
            "error: --fields applies to JSON output\nDrop -o/--pretty, or use --json.",
        )),
        _ => Ok(OutputMode::Json),
    }
}

/// Whether an inspection command prints JSON. `--fields` implies JSON; with `--pretty` it is a
/// usage error.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
fn fields_json(format: FormatFlags, fields: Option<&[String]>) -> Result<bool, CliError> {
    match (fields, format.pretty) {
        (Some(_), true) => Err(CliError::from_prose(
            ErrorKind::Usage,
            "error: --fields applies to JSON output\nDrop --pretty, or use --json.",
        )),
        (Some(_), false) => Ok(true),
        (None, _) => Ok(is_json(format)),
    }
}

/// Apply `--fields` to `barca docs` JSON: each `topics[]` entry, or the single topic object.
fn project_docs_json(out: String, fields: Option<&[String]>) -> String {
    let Some(fields) = fields else { return out };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&out) else {
        return out;
    };
    if v.get("topics").is_some() {
        bounded::project_key(&mut v, "topics", Some(fields));
    } else {
        bounded::project_one(&mut v, fields);
    }
    serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
}

/// Change into the project root when it is above the cwd, rewriting every file argument so it
/// names the same file from there. A note on stderr says which root is in use. Without a
/// barca.toml above the cwd (or with the cwd being the root) nothing changes.
fn enter_project_root(cli: &mut Cli) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot determine cwd: {e}"))?;
    let Some(root) = barca_core::config::find_root(&cwd) else {
        return Ok(());
    };
    if root == cwd {
        return Ok(());
    }
    let rebase = |p: &Path| barca_core::config::rebase_onto_root(p, &cwd, &root);
    match cli {
        Cli::Get { args, .. } | Cli::Run { args, .. } | Cli::Status { args, .. } => {
            for arg in args.iter_mut().filter(|a| a.ends_with(".py")) {
                *arg = rebase(Path::new(arg.as_str()))
                    .to_string_lossy()
                    .into_owned();
            }
        }
        Cli::Plan { files, .. }
        | Cli::Stats { files, .. }
        | Cli::Serve { files, .. }
        | Cli::List { files, .. } => {
            for file in files.iter_mut() {
                *file = rebase(file);
            }
        }
        Cli::History { .. } | Cli::Docs { .. } | Cli::Version => {}
    }
    std::env::set_current_dir(&root)
        .map_err(|e| format!("cannot change into project root {}: {e}", root.display()))?;
    eprintln!(
        "barca: project root: {} ({} found above the cwd)",
        root.display(),
        barca_core::config::CONFIG_FILE
    );
    Ok(())
}

/// Split the raw positional args into (optional target, files).
/// If the first arg ends in `.py`, all args are files (no target).
/// Otherwise, the first arg is the target and the rest are files.
fn split_target_files(args: Vec<String>) -> (Option<String>, Vec<PathBuf>) {
    if args.is_empty() {
        return (None, Vec::new());
    }
    if args[0].ends_with(".py") {
        // All args are files.
        let files = args.into_iter().map(PathBuf::from).collect();
        (None, files)
    } else {
        // First arg is the target, rest are files.
        let target = args[0].clone();
        let files = args[1..].iter().map(PathBuf::from).collect();
        (Some(target), files)
    }
}

/// Split a target argument into its comma-separated names (`a,b` -> `[a, b]`), dropping
/// repeats. An empty name (`a,,b`, `a,`) is a usage error.
fn parse_targets(raw: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for name in raw.split(',') {
        if name.is_empty() {
            return Err(format!(
                "error: empty target name in '{raw}'\n\n\
                 Separate several targets with single commas, no spaces: barca run a,b pipeline.py"
            ));
        }
        if !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    Ok(out)
}

/// The target list of `get`/`run` (empty when no target was given); a malformed list is a
/// usage error (exit 2).
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
fn targets_arg(target: Option<&str>, files: &[PathBuf]) -> Result<Vec<String>, CliError> {
    match target.map(parse_targets) {
        None => Ok(Vec::new()),
        Some(Ok(names)) => Ok(names),
        Some(Err(msg)) => Err(usage_error(&msg, files)),
    }
}

fn main() {
    // Support `barca file.py [--flags]` as shorthand for `barca get file.py [--flags]`.
    let args: Vec<String> = std::env::args().collect();
    let parsed = Cli::try_parse_from(&args).or_else(|first| {
        if args.len() > 1 && !args[1].starts_with('-') && args[1].ends_with(".py") {
            // Insert "get" after the program name so clap handles all flags.
            let mut rewritten = vec![args[0].clone(), "get".to_string()];
            rewritten.extend_from_slice(&args[1..]);
            Cli::try_parse_from(rewritten)
        } else {
            Err(first)
        }
    });
    let mut cli = match parsed {
        Ok(cli) => cli,
        // `--help` / `--version` are not errors: clap prints them to stdout and exits 0.
        Err(e) if !e.use_stderr() => e.exit(),
        Err(e) => CliError::from_clap(&e).emit(error::json_mode_from_argv(&args)),
    };
    let json = json_output(&cli);

    // Version needs no runtime — answer before paying for thread spawns.
    if let Cli::Version = cli {
        println!("barca {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // The manual is compiled in: no runtime, no Python, no project files needed.
    if let Cli::Docs {
        topic,
        all,
        json,
        fields,
    } = &cli
    {
        match docs::run(topic.as_deref(), *all, *json || fields.is_some())
            .map(|out| project_docs_json(out, fields.as_deref()))
        {
            // Ignore write errors (e.g. a closed pipe from `barca docs --all | head`).
            Ok(out) => {
                let _ = std::io::stdout().lock().write_all(out.as_bytes());
            }
            Err(msg) => CliError::from_prose(ErrorKind::Usage, msg).emit(*json || fields.is_some()),
        }
        return;
    }

    // Hints in errors name files as the user typed them, so take them before rebasing.
    let ctx = context(&cli);
    // Run from the project root (the nearest barca.toml at or above the cwd), so `.barca/`,
    // node ids and relative paths inside steps are the same wherever barca is invoked.
    if let Err(msg) = enter_project_root(&mut cli) {
        CliError::from_barca(barca_core::BarcaError::Other(msg), &ctx).emit(json);
    }

    // The one runtime for the whole process — barca-core is async-native and
    // runs on whatever runtime the caller provides.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            CliError::from_barca(
                barca_core::BarcaError::Other(format!("failed to create runtime: {e}")),
                &Context::default(),
            )
            .emit(json)
        });
    if let Err(e) = rt.block_on(run_cli(cli, &ctx)) {
        // A failed run gets one greppable line naming the step, right before the error.
        if e.kind == ErrorKind::StepFailed
            && let Some(node) = &e.node
        {
            eprintln!(
                "[barca] run failed: step '{node}' failed (exit {})",
                e.code()
            );
        }
        e.emit(json);
    }
}

/// On a failed run in JSON mode, still print the one-line result on stdout so agents need not
/// parse stderr: `status: "failed"`, the failing step and its error, and what ran before it.
/// The error itself still goes to stderr as the envelope.
fn print_failed_run(err: &barca_core::BarcaError, mode: OutputMode) {
    let (barca_core::BarcaError::WorkerFailed(f), OutputMode::Json) = (err, mode) else {
        return;
    };
    let Some(run) = &f.run else { return };
    println!(
        "{}",
        serde_json::json!({
            "status": "failed",
            "run_id": run.run_id,
            "elapsed_seconds": run.elapsed_seconds,
            "steps_executed": run.steps_executed,
            "phases": run.phases,
            "failed_node": f.node,
            "error": f.summary(),
            "steps": &run.steps,
        })
    );
}

/// A token that cancels on Ctrl-C, so an interrupted run terminates its
/// workers and is recorded as `cancelled` instead of lingering as `running`.
fn cancel_on_ctrl_c() -> barca_core::CancellationToken {
    let cancel = barca_core::CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c.cancel();
        }
    });
    cancel
}

async fn run_cli(cli: Cli, ctx: &Context) -> Result<(), CliError> {
    let python = barca_core::commands::find_python();
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, ctx);

    match cli {
        Cli::Get {
            args,
            output,
            format,
            refresh,
            no_cascade,
            refresh_all,
            no_cache,
            dry_run,
            agent,
            fields,
            env,
        } => {
            check_order("get", &args)?;
            let output = get_run_mode(output, format, fields.as_deref())?;
            let (target, files) = split_target_files(args);
            check_py_files(&files, refresh.as_deref())?;
            if files.is_empty() {
                let what = if target.is_none() {
                    "files"
                } else {
                    ".py files"
                };
                return Err(usage_error(
                    &format!("error: no {what} provided\n\n{GET_USAGE}"),
                    &files,
                ));
            }
            let hint_files = files.clone();
            let targets = targets_arg(target.as_deref(), &files)?;
            let policy = cache_policy(refresh, no_cascade, refresh_all, no_cache);
            get_cmd(
                env.as_deref(),
                targets,
                files,
                &python,
                output,
                policy,
                dry_run,
                agent,
                fields.as_deref(),
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Run {
            args,
            refresh,
            no_cascade,
            refresh_all,
            no_cache,
            dry_run,
            output,
            format,
            agent,
            fields,
            env,
        } => {
            check_order("run", &args)?;
            let output = get_run_mode(output, format, fields.as_deref())?;
            let (target, files) = split_target_files(args);
            check_py_files(&files, refresh.as_deref())?;
            let Some(target) = target else {
                return Err(usage_error(
                    &format!("error: a target task is required\n\n{RUN_USAGE}"),
                    &files,
                ));
            };
            if files.is_empty() {
                return Err(usage_error(
                    &format!("error: no .py files provided\n\n{RUN_USAGE}"),
                    &files,
                ));
            }
            let hint_files = files.clone();
            let policy = cache_policy(refresh, no_cascade, refresh_all, no_cache);
            let targets = targets_arg(Some(&target), &files)?;
            run_cmd(
                env.as_deref(),
                targets,
                files,
                &python,
                policy,
                dry_run,
                output,
                agent,
                fields.as_deref(),
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Plan { files } => plan_cmd(files, &python).await.map_err(engine),
        Cli::History {
            limit,
            all,
            format,
            fields,
            env,
        } => {
            let json = fields_json(format, fields.as_deref())?;
            let limit = (!all).then_some(limit);
            history_cmd(env.as_deref(), limit, json, fields.as_deref())
                .await
                .map_err(engine)
        }
        Cli::Stats {
            target,
            files,
            format,
            fields,
            env,
        } => {
            let json = fields_json(format, fields.as_deref())?;
            stats_cmd(
                env.as_deref(),
                target,
                files,
                json,
                fields.as_deref(),
                &python,
            )
            .await
            .map_err(engine)
        }
        Cli::List {
            files,
            format,
            limit,
            all,
            fields,
        } => {
            let json = fields_json(format, fields.as_deref())?;
            let limit = (!all).then_some(limit);
            list_cmd(files, json, limit, fields.as_deref(), &python)
                .await
                .map_err(engine)
        }
        Cli::Status {
            args,
            format,
            limit,
            all,
            fields,
            sample,
            env,
        } => {
            let json = fields_json(format, fields.as_deref())?;
            check_order("status", &args)?;
            let (target, files) = split_target_files(args);
            check_py_files(&files, None)?;
            if files.is_empty() {
                return Err(usage_error(
                    &format!("error: no .py files provided\n\n{STATUS_USAGE}"),
                    &files,
                ));
            }
            let hint_files = files.clone();
            let targets = targets_arg(target.as_deref(), &files)?;
            let limit = (!all).then_some(limit);
            status_cmd(
                env.as_deref(),
                targets,
                files,
                StatusOpts {
                    json,
                    limit,
                    fields: fields.as_deref(),
                    sample: sample.unwrap_or(0),
                },
                &python,
            )
            .await
            .map_err(|e| get_run_error(e, ctx, &hint_files))
        }
        Cli::Serve {
            files,
            port,
            watch,
            no_schedule,
            timezone,
            env,
        } => serve_cmd(
            env.as_deref(),
            files,
            port,
            watch,
            !no_schedule,
            timezone,
            &python,
        )
        .await
        .map_err(engine),
        // Answered in main() before the runtime is built — never reaches here.
        Cli::Version => unreachable!("version is handled before runtime construction"),
        Cli::Docs { .. } => unreachable!("docs is handled before runtime construction"),
    }
}

/// Inspection commands: JSON or a table, by the shared rule in `output::resolve`.
fn is_json(flags: FormatFlags) -> bool {
    output::resolve(flags.explicit()) == Format::Json
}

#[allow(clippy::too_many_arguments)]
async fn get_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    python: &PathBuf,
    mode: OutputMode,
    policy: barca_core::commands::CachePolicy,
    dry_run: bool,
    agent: bool,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    if dry_run {
        return explain_cmd(
            &cfg, &targets, &file_args, python, policy, "get", mode, fields,
        )
        .await;
    }
    if targets.len() > 1 {
        let result = barca_core::commands::get_many(
            &cfg,
            &targets,
            &file_args,
            python,
            policy,
            agent,
            cancel_on_ctrl_c(),
        )
        .await?;
        return print_multi(&result, mode, "got", fields);
    }
    let target = targets.into_iter().next();
    let result = barca_core::commands::get(
        &cfg,
        target.as_deref(),
        &file_args,
        python,
        policy,
        agent,
        cancel_on_ctrl_c(),
    )
    .await
    .inspect_err(|e| print_failed_run(e, mode))?;
    let final_output = result.final_output.as_ref().map(read_final_output);

    match mode {
        OutputMode::Json => {
            let mut out = serde_json::json!({
                "status": "success",
                "run_id": result.run_id,
                "elapsed_seconds": result.elapsed_seconds,
                "steps_executed": result.steps_executed,
                "phases": result.phases,
                "final_output": final_output,
                "steps": &result.steps,
            });
            bounded::project_key(&mut out, "steps", fields);
            println!("{out}");
        }
        OutputMode::Value => {
            if let Some(ref val) = final_output {
                println!("{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
        OutputMode::Pretty => {
            let label = target
                .as_ref()
                .map(|t| format!("got '{t}'"))
                .unwrap_or_else(|| "all assets".to_string());
            println!(
                "Run {} | {} in {:.3}s ({} step{}, {} phase{})",
                result.run_id,
                label,
                result.elapsed_seconds,
                result.steps_executed,
                if result.steps_executed == 1 { "" } else { "s" },
                result.phases,
                if result.phases == 1 { "" } else { "s" }
            );
            if let Some(ref val) = final_output {
                println!("\nValue:\n{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    python: &PathBuf,
    policy: barca_core::commands::CachePolicy,
    dry_run: bool,
    mode: OutputMode,
    agent: bool,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    if dry_run {
        return explain_cmd(
            &cfg, &targets, &file_args, python, policy, "run", mode, fields,
        )
        .await;
    }
    if targets.len() > 1 {
        let result = barca_core::commands::run_many(
            &cfg,
            &targets,
            &file_args,
            python,
            policy,
            agent,
            cancel_on_ctrl_c(),
        )
        .await?;
        return print_multi(&result, mode, "ran", fields);
    }
    let target = targets.into_iter().next().unwrap_or_default();
    let result = barca_core::commands::run(
        &cfg,
        &target,
        &file_args,
        python,
        policy,
        agent,
        cancel_on_ctrl_c(),
    )
    .await
    .inspect_err(|e| print_failed_run(e, mode))?;
    let final_output = result.final_output.as_ref().map(read_final_output);

    match mode {
        OutputMode::Json => {
            let mut out = serde_json::json!({
                "status": "success",
                "run_id": result.run_id,
                "elapsed_seconds": result.elapsed_seconds,
                "steps_executed": result.steps_executed,
                "phases": result.phases,
                "final_output": final_output,
                "steps": &result.steps,
            });
            bounded::project_key(&mut out, "steps", fields);
            println!("{out}");
        }
        OutputMode::Value => {
            if let Some(ref val) = final_output {
                println!("{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
        OutputMode::Pretty => {
            println!(
                "Run {} | ran '{}' in {:.3}s ({} step{}, {} phase{})",
                result.run_id,
                target,
                result.elapsed_seconds,
                result.steps_executed,
                if result.steps_executed == 1 { "" } else { "s" },
                result.phases,
                if result.phases == 1 { "" } else { "s" }
            );
            if let Some(ref val) = final_output {
                println!("\nValue:\n{}", serde_json::to_string_pretty(val).unwrap());
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn explain_cmd(
    cfg: &barca_core::config::ResolvedConfig,
    targets: &[String],
    file_args: &[String],
    python: &PathBuf,
    policy: barca_core::commands::CachePolicy,
    label: &str,
    mode: OutputMode,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let result =
        barca_core::commands::explain(cfg, targets, file_args, python, policy, false, label)
            .await?;
    match mode {
        OutputMode::Json => {
            let mut out = serde_json::to_value(&result).unwrap();
            bounded::project_key(&mut out, "steps", fields);
            // `targets` (several targets) is keyed in the order given, like a real run; the
            // other keys sort before it.
            match out.as_object_mut().and_then(|o| o.remove("targets")) {
                Some(_) => {
                    let per_target: Vec<(String, serde_json::Value)> = result
                        .targets
                        .iter()
                        .map(|(n, p)| (n.clone(), serde_json::to_value(p).unwrap()))
                        .collect();
                    let rest = out.to_string();
                    println!(
                        "{},\"targets\":{}}}",
                        &rest[..rest.len() - 1],
                        ordered_object(&per_target)
                    );
                }
                None => println!("{out}"),
            }
        }
        OutputMode::Value => println!("{}", serde_json::to_string_pretty(&result.steps).unwrap()),
        OutputMode::Pretty => {
            println!(
                "Dry run: barca {label}{} (nothing executed, nothing written)\n",
                if result.targets.len() > 1 {
                    format!(" {}", result.target_names().join(","))
                } else {
                    result
                        .target
                        .as_deref()
                        .map(|t| format!(" {t}"))
                        .unwrap_or_default()
                }
            );
            print_step_table(&result.steps, true);
            println!(
                "\n{} will run, {} cached, {} unknown",
                result.summary.will_run, result.summary.cached, result.summary.unknown
            );
        }
    }
    Ok(())
}

/// A JSON object of `pairs` with keys in the given order (serde_json maps sort their keys).
fn ordered_object(pairs: &[(String, serde_json::Value)]) -> String {
    let body: Vec<String> = pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}:{}",
                serde_json::Value::from(k.as_str()),
                serde_json::to_string(v).unwrap()
            )
        })
        .collect();
    format!("{{{}}}", body.join(","))
}

/// Print a multi-target run (`barca get|run a,b`) and exit 1 if any target failed.
///
/// JSON: the run fields of a single-target run without `final_output`, plus `targets`, keyed by
/// target name in the order given: `{"status": "success", "final_output": ...}` or
/// `{"status": "failed", "failed_node": ..., "error": ...}`.
/// Print a multi-target result. When any target failed this returns the first failure as a
/// step failure (exit 1, and the error envelope on stderr), after the full result was printed.
fn print_multi(
    result: &barca_core::commands::MultiResult,
    mode: OutputMode,
    verb: &str,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let per_target: Vec<(String, serde_json::Value)> = result
        .targets
        .iter()
        .map(|(name, t)| {
            let mut obj = serde_json::Map::new();
            obj.insert("status".into(), t.status.clone().into());
            if t.status == "success" {
                let value = t
                    .final_output
                    .as_ref()
                    .map(read_final_output)
                    .unwrap_or(serde_json::Value::Null);
                obj.insert("final_output".into(), value);
            }
            if let Some(step) = &t.failed_node {
                obj.insert("failed_node".into(), step.clone().into());
            }
            if let Some(err) = &t.error {
                obj.insert("error".into(), err.clone().into());
            }
            (name.clone(), serde_json::Value::Object(obj))
        })
        .collect();

    match mode {
        OutputMode::Json => {
            // Keys of the run object sort before `targets`, which keeps the given order.
            let mut run = serde_json::json!({
                "status": if result.any_failed() { "failed" } else { "success" },
                "run_id": result.run_id,
                "elapsed_seconds": result.elapsed_seconds,
                "steps_executed": result.steps_executed,
                "phases": result.phases,
                "steps": &result.steps,
            });
            bounded::project_key(&mut run, "steps", fields);
            let run = run.to_string();
            println!(
                "{},\"targets\":{}}}",
                &run[..run.len() - 1],
                ordered_object(&per_target)
            );
        }
        OutputMode::Value => {
            // Each target's value (null when it failed), keyed by target name.
            let values: serde_json::Map<String, serde_json::Value> = per_target
                .iter()
                .map(|(name, t)| {
                    let v = t.get("final_output").cloned().unwrap_or_default();
                    (name.clone(), v)
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&values).unwrap());
        }
        OutputMode::Pretty => {
            let failed = result.targets.iter().filter(|(_, t)| t.status != "success");
            println!(
                "Run {} | {verb} {} targets in {:.3}s ({} step{}, {} phase{}, {} failed)",
                result.run_id,
                result.targets.len(),
                result.elapsed_seconds,
                result.steps_executed,
                if result.steps_executed == 1 { "" } else { "s" },
                result.phases,
                if result.phases == 1 { "" } else { "s" },
                failed.count(),
            );
            for (name, t) in &per_target {
                let status = t["status"].as_str().unwrap_or("?");
                println!("\n{name}: {status}");
                match t.get("final_output") {
                    Some(v) => println!("{}", serde_json::to_string_pretty(v).unwrap()),
                    None => println!(
                        "  failed at {}",
                        t["failed_node"].as_str().unwrap_or("(did not run)")
                    ),
                }
            }
        }
    }
    let _ = std::io::stdout().flush();

    if result.any_failed() {
        for (name, t) in &result.targets {
            if t.status == "success" {
                continue;
            }
            let at = t
                .failed_node
                .as_deref()
                .map(|s| format!(" (failed step: {s})"))
                .unwrap_or_default();
            eprintln!(
                "error: target '{name}' failed{at}: {}",
                t.error.as_deref().unwrap_or("unknown error")
            );
        }
        // The first failed target becomes the run's error: exit 1 with the error envelope.
        if let Some((name, t)) = result.targets.iter().find(|(_, t)| t.status != "success") {
            return Err(barca_core::BarcaError::WorkerFailed(Box::new(
                barca_core::FailedStep {
                    node: t.failed_node.clone().unwrap_or_else(|| name.clone()),
                    message: t.error.clone().unwrap_or_else(|| "unknown error".into()),
                    artifact_dir: None,
                    run: None,
                },
            )));
        }
    }
    Ok(())
}

/// STATUS / WHY / STEP table for dry runs and (in `-o pretty`) real runs.
fn print_step_table(steps: &[barca_core::commands::StepReport], dry: bool) {
    let rows: Vec<(String, String, &str)> = steps
        .iter()
        .map(|s| {
            let verdict = s.action.as_deref().or(s.status.as_deref()).unwrap_or("?");
            let label = match (dry, verdict) {
                (true, "run") => "will run",
                (_, v) => v,
            };
            let why = match (&s.partitions, &s.detail) {
                (Some(p), _) if verdict == "partial" => format!(
                    "{} of {} keys cached; will run: {}",
                    p.cached,
                    p.total,
                    p.will_run_keys.join(", ")
                ),
                (Some(p), Some(d)) => format!("{} keys; {d}", p.total),
                (None, Some(d)) => d.clone(),
                (Some(p), None) => format!("{} keys cached", p.total),
                (None, None) => "-".to_string(),
            };
            (label.to_string(), why, s.id.as_str())
        })
        .collect();
    let w_status = rows.iter().map(|r| r.0.len()).max().unwrap_or(6).max(6);
    let w_why = rows
        .iter()
        .map(|r| r.1.len())
        .max()
        .unwrap_or(3)
        .clamp(3, 70);
    println!("{:<w_status$}  {:<w_why$}  STEP", "STATUS", "WHY");
    for (label, why, id) in &rows {
        println!("{label:<w_status$}  {why:<w_why$}  {id}");
    }
    for s in steps {
        if let Some(w) = &s.warning {
            println!("\n  ! {w}");
        }
    }
}

async fn plan_cmd(files: Vec<PathBuf>, python: &PathBuf) -> Result<(), barca_core::BarcaError> {
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let result = barca_core::commands::plan(&file_args, python).await?;
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(())
}

/// `freshness` as `barca list` prints it: lowercase, like `kind`.
fn freshness_str(f: &barca_core::Freshness) -> &'static str {
    match f {
        barca_core::Freshness::Always => "always",
        barca_core::Freshness::Manual => "manual",
        barca_core::Freshness::Schedule(_) => "schedule",
    }
}

/// One `nodes[]` entry of `barca list --json`: `freshness` is a flat lowercase string, with the
/// cron expression in `schedule` and the next fire time in `next_fire` for scheduled nodes.
/// (The HTTP API's `GET /assets` keeps the engine's own serialization.)
fn list_node_json(
    a: &barca_core::commands::AssetSummary,
    next_fire: Option<&String>,
) -> serde_json::Value {
    let mut v = serde_json::to_value(a).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("freshness".into(), freshness_str(&a.freshness).into());
        if let barca_core::Freshness::Schedule(cron) = &a.freshness {
            obj.insert("schedule".into(), cron.0.clone().into());
        }
        if let Some(t) = next_fire {
            obj.insert("next_fire".into(), t.clone().into());
        }
    }
    v
}

async fn list_cmd(
    files: Vec<PathBuf>,
    json: bool,
    limit: Option<usize>,
    fields: Option<&[String]>,
    python: &PathBuf,
) -> Result<(), barca_core::BarcaError> {
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let mut assets = barca_core::commands::list_assets(&file_args, python).await?;
    // Bounded output: the first `limit` nodes in topological order (`--all` = no limit).
    let total = assets.len();
    assets.truncate(limit.unwrap_or(total));
    let page = bounded::Page::new(assets.len(), total);

    // Next fire times (local time) for scheduled definitions. Empty when nothing is
    // scheduled, so the table's NEXT FIRE column only appears when it carries information.
    let next_fires: std::collections::HashMap<String, String> =
        barca_server::describe_schedule(&file_args, python)
            .await
            .into_iter()
            .filter_map(|j| j.next_fire_local.map(|t| (j.id, t)))
            .collect();
    if json || fields.is_some() {
        let mut nodes: Vec<serde_json::Value> = assets
            .iter()
            .map(|a| list_node_json(a, next_fires.get(&a.id)))
            .collect();
        if let Some(f) = fields {
            bounded::project(&mut nodes, f);
        }
        let out = page.envelope("nodes", nodes, "nodes");
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }
    if assets.is_empty() {
        match page.note(0, "nodes") {
            Some(note) => eprintln!("{note}"),
            None => println!("No definitions found."),
        }
        return Ok(());
    }
    let has_schedule = !next_fires.is_empty();
    // Like NEXT FIRE, the ENV column only appears when some node declares env.
    let has_env = assets.iter().any(|a| !a.env.is_empty());

    // Render each row's cells up front so column widths fit the actual content.
    let mut header = vec!["NAME", "KIND", "FRESHNESS"];
    if has_schedule {
        header.push("NEXT FIRE");
    }
    header.push("DEPS");
    if has_env {
        header.push("ENV");
    }
    let list_or_dash = |v: &[String]| {
        if v.is_empty() {
            "-".to_string()
        } else {
            v.join(", ")
        }
    };
    let rows: Vec<Vec<String>> = assets
        .iter()
        .map(|a| {
            let kind = serde_json::to_value(&a.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{:?}", a.kind).to_lowercase());
            let freshness = match &a.freshness {
                barca_core::Freshness::Schedule(cron) => format!("cron: {}", cron.0),
                f => freshness_str(f).to_string(),
            };
            let mut row = vec![a.id.clone(), kind, freshness];
            if has_schedule {
                row.push(next_fires.get(&a.id).cloned().unwrap_or_else(|| "-".into()));
            }
            row.push(list_or_dash(&a.inputs));
            if has_env {
                row.push(list_or_dash(&a.env));
            }
            row
        })
        .collect();

    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .map(|r| r[i].len())
                .chain(std::iter::once(header[i].len()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    // Every column but the last is padded; the last runs to the end of the line.
    let last = header.len() - 1;
    let render = |cells: &[&str]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == last {
                    c.to_string()
                } else {
                    format!("{c:<w$}", w = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    println!("{}", render(&header));
    println!(
        "{}",
        "-".repeat(widths[..last].iter().sum::<usize>() + 2 * last + 4)
    );
    for row in &rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        println!("{}", render(&cells));
    }
    if let Some(note) = page.note(rows.len(), "nodes") {
        eprintln!("{note}");
    }
    Ok(())
}

/// How `barca status` prints: format, bounds, field projection and sampling.
struct StatusOpts<'a> {
    json: bool,
    limit: Option<usize>,
    fields: Option<&'a [String]>,
    sample: usize,
}

async fn status_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    opts: StatusOpts<'_>,
    python: &PathBuf,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let mut result =
        barca_core::status::status(&cfg, &targets, &file_args, python, opts.sample, true).await?;
    // Bounded like `list`: the summary still counts every node; `nodes` is cut to the limit.
    let total = result.nodes.len();
    if let Some(limit) = opts.limit {
        result.nodes.truncate(limit);
    }
    let page = bounded::Page::new(result.nodes.len(), total);
    if opts.json {
        let mut out = serde_json::to_value(&result).unwrap();
        bounded::project_key(&mut out, "nodes", opts.fields);
        if let serde_json::Value::Object(obj) = &mut out
            && let serde_json::Value::Object(env) = page.envelope("nodes", vec![], "nodes")
        {
            // Add `total`, `truncated` and (when truncated) `hint` beside `nodes`.
            for (k, v) in env {
                if k != "nodes" {
                    obj.insert(k, v);
                }
            }
        }
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }
    print_status_table(&result);
    if let Some(note) = page.note(result.nodes.len(), "nodes") {
        eprintln!("{note}");
    }
    Ok(())
}

/// One row per node: NAME KIND STATE WHY LAST RUN SHAPE DEPS.
fn print_status_table(result: &barca_core::status::StatusResult) {
    let short = |id: &str| -> String {
        let base = id.split('[').next().unwrap_or(id);
        base.rsplit(':').next().unwrap_or(base).to_string()
    };
    let rows: Vec<[String; 7]> = result
        .nodes
        .iter()
        .map(|n| {
            let why = match &n.partitions {
                Some(p) if n.cache.state != "cached" => {
                    format!(
                        "{} of {} keys cached; {}",
                        p.cached, p.total, n.cache.reason
                    )
                }
                _ => n.cache.reason.clone(),
            };
            let last = n
                .last_materialization
                .as_ref()
                .map(|m| {
                    let secs = m
                        .elapsed_seconds
                        .map(|e| format!(" {e:.2}s"))
                        .unwrap_or_default();
                    format!("{} {}{secs}", m.status, m.created_at)
                })
                .unwrap_or_else(|| "-".to_string());
            let deps = if n.inputs.is_empty() {
                "-".to_string()
            } else {
                n.inputs
                    .iter()
                    .map(|i| short(i))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            [
                n.name.clone(),
                n.kind.clone(),
                // Human form: `never-run`, `always-runs` (JSON says `never_run`, `always_runs`).
                n.cache.state.replace('_', "-"),
                why,
                last,
                n.shape
                    .as_ref()
                    .map(shape_cell)
                    .unwrap_or_else(|| "-".to_string()),
                deps,
            ]
        })
        .collect();
    let header = ["NAME", "KIND", "STATE", "WHY", "LAST RUN", "SHAPE", "DEPS"];
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .chain([header[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[String]| {
        let mut out = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                out.push_str(c);
            } else {
                out.push_str(&format!("{c:<w$}  ", w = widths[i]));
            }
        }
        println!("{}", out.trim_end());
    };
    line(&header.map(String::from));
    for r in &rows {
        line(r);
    }
    let s = &result.summary;
    println!(
        "\n{} cached, {} stale, {} never run, {} partial, {} unknown, {} always run",
        s.cached, s.stale, s.never_run, s.partial, s.unknown, s.always_runs
    );
}

/// Compact text for a shape object: `3 rows x 2 cols`, `dict (4 keys)`, `pandas.DataFrame`.
fn shape_cell(shape: &serde_json::Value) -> String {
    let rows = shape.get("rows").and_then(|v| v.as_u64());
    let cols = shape
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|c| c.len());
    let ty = shape.get("type").and_then(|v| v.as_str());
    match (rows, cols, ty) {
        (Some(r), Some(c), _) => format!("{} x {}", plural(r, "row"), plural(c as u64, "col")),
        (Some(r), None, _) => plural(r, "row"),
        (None, _, Some("dict")) => {
            let n = shape
                .get("key_count")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    shape
                        .get("keys")
                        .and_then(|k| k.as_array())
                        .map(|k| k.len() as u64)
                })
                .unwrap_or(0);
            format!("dict ({})", plural(n, "key"))
        }
        (None, _, Some(t)) => t.to_string(),
        _ => shape
            .get("note")
            .and_then(|v| v.as_str())
            .map(|n| format!("? ({n})"))
            .unwrap_or_else(|| "-".to_string()),
    }
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

async fn history_cmd(
    env: Option<&str>,
    limit: Option<usize>,
    json: bool,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let (runs, total) = barca_core::commands::history(&cfg, limit).await?;
    let page = bounded::Page::new(runs.len(), total);
    if json || fields.is_some() {
        let mut items: Vec<serde_json::Value> = runs
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Null))
            .collect();
        if let Some(f) = fields {
            bounded::project(&mut items, f);
        }
        let out = page.envelope("runs", items, "runs");
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }
    if runs.is_empty() {
        match page.note(0, "runs") {
            Some(note) => eprintln!("{note}"),
            None => println!("No run history found."),
        }
        return Ok(());
    }
    // Table header.
    println!(
        "{:<14} {:<7} {:<9} {:>5} {:>6} {:>6} {:<20}",
        "RUN_ID", "CMD", "STATUS", "STEPS", "CACHED", "TIME", "STARTED"
    );
    println!("{}", "-".repeat(75));
    for r in &runs {
        let elapsed_str = r
            .elapsed_seconds
            .map(|e| format!("{:.1}s", e))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<14} {:<7} {:<9} {:>5} {:>6} {:>6} {:<20}",
            r.run_id,
            r.command,
            r.status,
            r.steps_executed,
            r.steps_cached,
            elapsed_str,
            r.started_at,
        );
    }
    if let Some(note) = page.note(runs.len(), "runs") {
        eprintln!("{note}");
    }
    Ok(())
}

/// `barca stats --json`: the node id is `id`, as on every other command. (The HTTP API's
/// `GET /assets/<name>` keeps the engine's `node_id`.)
fn stats_json(stats: &barca_core::db::AssetStats) -> serde_json::Value {
    let mut out = serde_json::to_value(stats).unwrap();
    if let Some(obj) = out.as_object_mut()
        && let Some(id) = obj.remove("node_id")
    {
        obj.insert("id".into(), id);
    }
    out
}

async fn stats_cmd(
    env: Option<&str>,
    target: String,
    files: Vec<PathBuf>,
    json: bool,
    fields: Option<&[String]>,
    python: &PathBuf,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let stats = barca_core::commands::stats(&cfg, &target, &file_args, python).await?;
    if json || fields.is_some() {
        let mut out = stats_json(&stats);
        bounded::project_key(&mut out, "recent_runs", fields);
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }
    let fmt = |v: Option<f64>| v.map(|e| format!("{:.3}s", e)).unwrap_or("-".to_string());
    println!("Asset: {}", stats.node_id);
    println!("Total materializations: {}", stats.total_runs);
    println!(
        "Timing:  avg {}  median {}  p95 {}  max {}",
        fmt(stats.avg_elapsed_seconds),
        fmt(stats.median_elapsed_seconds),
        fmt(stats.p95_elapsed_seconds),
        fmt(stats.max_elapsed_seconds),
    );
    println!("Cache hit rate: {:.1}%", stats.cache_hit_rate * 100.0);
    if !stats.recent_runs.is_empty() {
        println!("\nRecent runs:");
        println!(
            "  {:<10} {:<9} {:<8} {:<20}",
            "ELAPSED", "STATUS", "ATTEMPTS", "CREATED"
        );
        for entry in &stats.recent_runs {
            let elapsed_str = entry
                .elapsed_seconds
                .map(|e| format!("{:.3}s", e))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {:<10} {:<9} {:<8} {:<20}",
                elapsed_str, entry.status, entry.attempts, entry.created_at,
            );
            if entry.status == "failed"
                && let Some(msg) = &entry.error_message
                && !msg.is_empty()
            {
                println!("      └─ {msg}");
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn serve_cmd(
    env: Option<&str>,
    files: Vec<PathBuf>,
    port: u16,
    watch: bool,
    schedule: bool,
    timezone: String,
    python: &std::path::Path,
) -> Result<(), barca_core::BarcaError> {
    let resolved = barca_core::config::resolve(env)?;
    if resolved.state == barca_core::config::StateMode::Optimistic && resolved.state_uri.is_some() {
        return Err(barca_core::BarcaError::Usage(
            "barca serve does not support shared remote state yet — set state = \"off\" \
             in barca.toml (or BARCA_STATE=off) to serve with a local metadata DB"
                .to_string(),
        ));
    }
    let config = barca_server::ServeConfig {
        files: files.iter().map(|p| p.display().to_string()).collect(),
        host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        port,
        watch,
        schedule,
        timezone,
        python: python.to_path_buf(),
        resolved,
    };
    barca_server::serve(config)
        .await
        .map_err(|e| barca_core::BarcaError::Other(e.to_string()))
}

/// Read an artifact for display: inline JSON values, show metadata for binary formats.
fn read_final_output(oref: &barca_core::dispatch::OutputRef) -> serde_json::Value {
    if oref.format == "json" {
        std::fs::read_to_string(&oref.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| artifact_metadata(oref))
    } else {
        artifact_metadata(oref)
    }
}

fn artifact_metadata(oref: &barca_core::dispatch::OutputRef) -> serde_json::Value {
    serde_json::json!({
        "_barca_artifact": {
            "path": oref.path,
            "format": oref.format,
            "size_bytes": oref.size_bytes,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Subcommands that must carry runnable examples in their `--help`.
    const DOCUMENTED: &[&str] = &[
        "get", "run", "plan", "history", "stats", "serve", "list", "status", "docs",
    ];

    fn after_help(cmd: &clap::Command) -> String {
        cmd.get_after_help()
            .or_else(|| cmd.get_after_long_help())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    /// `barca ...` command lines found in `text`: indented example lines in help text, or
    /// lines inside ```bash fences in a docs topic. Trailing `# comments` and anything after a
    /// pipe (`barca ... | jq ...`) are stripped.
    fn command_lines(text: &str, only_in_bash_fences: bool) -> Vec<String> {
        let mut out = Vec::new();
        let mut in_bash = false;
        for line in text.lines() {
            let t = line.trim();
            if only_in_bash_fences {
                if t.starts_with("```") {
                    in_bash = t == "```bash";
                    continue;
                }
                if !in_bash {
                    continue;
                }
            }
            if t.starts_with("barca ") {
                let cmd = t.split(" #").next().unwrap_or(t);
                let cmd = cmd.split(" | ").next().unwrap_or(cmd).trim();
                out.push(cmd.to_string());
            }
        }
        out
    }

    fn assert_parses(cmd_line: &str, ctx: &str) {
        let argv = cmd_line.split_whitespace();
        match Cli::try_parse_from(argv) {
            Ok(_) => {}
            // `--help` / `--version` "fail" with a display error; that is a valid command.
            Err(e)
                if matches!(
                    e.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) => {}
            Err(e) => panic!("{ctx}: `{cmd_line}` does not parse:\n{e}"),
        }
    }

    #[test]
    fn every_documented_subcommand_has_examples_and_a_docs_pointer() {
        let root = Cli::command();
        for name in DOCUMENTED {
            let sub = root
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("missing subcommand {name}"));
            let help = after_help(sub);
            assert!(
                help.contains("Examples:"),
                "`barca {name} --help` needs an `Examples:` section (after_help)"
            );
            assert!(
                !command_lines(&help, false).is_empty(),
                "`barca {name} --help` examples need at least one `barca ...` line"
            );
        }
    }

    #[test]
    fn top_level_help_points_at_the_manual() {
        let help = after_help(&Cli::command());
        assert!(
            help.contains("barca docs"),
            "top-level --help must mention `barca docs`"
        );
        assert!(
            help.contains("barca docs agents"),
            "top-level --help must point agents at `barca docs agents`"
        );
    }

    #[test]
    fn every_help_example_parses_against_the_real_cli() {
        let root = Cli::command();
        for name in DOCUMENTED {
            let help = after_help(root.find_subcommand(name).unwrap());
            for line in command_lines(&help, false) {
                assert_parses(&line, &format!("`barca {name} --help` example"));
            }
        }
        for line in command_lines(&after_help(&root), false) {
            assert_parses(&line, "top-level --help example");
        }
    }

    #[test]
    fn every_docs_topic_command_parses_against_the_real_cli() {
        for t in docs::TOPICS {
            for line in command_lines(t.body, true) {
                assert_parses(&line, &format!("docs topic '{}'", t.name));
            }
        }
    }

    #[test]
    fn docs_pointers_in_help_text_resolve_to_topics() {
        let root = Cli::command();
        let mut texts = vec![after_help(&root)];
        for name in DOCUMENTED {
            texts.push(after_help(root.find_subcommand(name).unwrap()));
        }
        for text in texts {
            for topic in docs::referenced_topics(&text) {
                assert!(
                    docs::find(&topic).is_some(),
                    "--help mentions unknown `barca docs {topic}`"
                );
            }
        }
    }

    #[test]
    fn every_flag_and_argument_has_help_text() {
        fn check(cmd: &clap::Command, path: &str) {
            for arg in cmd.get_arguments() {
                if arg.is_hide_set() || matches!(arg.get_id().as_str(), "help" | "version") {
                    continue;
                }
                assert!(
                    arg.get_help().is_some() || arg.get_long_help().is_some(),
                    "`{path}` argument '{}' has no help text — add a doc comment",
                    arg.get_id()
                );
            }
            for sub in cmd.get_subcommands() {
                check(sub, &format!("{path} {}", sub.get_name()));
            }
        }
        check(&Cli::command(), "barca");
    }

    #[test]
    fn every_subcommand_has_a_one_line_description() {
        for sub in Cli::command().get_subcommands() {
            assert!(
                sub.get_about().is_some(),
                "`barca {}` has no description",
                sub.get_name()
            );
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn files_before_target_prints_the_corrected_command() {
        let msg = wrong_order_error(
            "run",
            &strings(&["pipeline.py", "validate_foo"]),
            &strings(&["run", "pipeline.py", "validate_foo"]),
        )
        .expect("wrong order must be an error");
        assert_eq!(
            msg,
            "error: the target comes before the files\n\n  barca run validate_foo pipeline.py\n\n\
             Run `barca list pipeline.py` to see available assets and tasks."
        );
    }

    #[test]
    fn corrected_command_keeps_flags_and_handles_the_shorthand() {
        let msg = wrong_order_error(
            "get",
            &strings(&["a.py", "total", "b.py"]),
            &strings(&["a.py", "total", "b.py", "--env", "dev", "-o", "value"]),
        )
        .unwrap();
        assert!(
            msg.contains("\n  barca get total a.py b.py --env dev -o value\n"),
            "{msg}"
        );
        assert!(msg.ends_with("Run `barca list a.py b.py` to see available assets and tasks."));
    }

    #[test]
    fn several_non_py_positionals_after_a_file_do_not_guess() {
        let msg = wrong_order_error(
            "run",
            &strings(&["pipeline.py", "report", "mid"]),
            &strings(&["run", "pipeline.py", "report", "mid"]),
        )
        .unwrap();
        assert!(msg.starts_with("error: the target comes before the files"));
        assert!(!msg.contains("barca run report pipeline.py"), "{msg}");
        assert!(msg.contains("Usage: barca run <TARGET> <FILES>..."));
    }

    #[test]
    fn correct_order_and_files_only_are_not_wrong_order() {
        for args in [
            &["report", "pipeline.py"][..],
            &["pipeline.py"][..],
            &["a.py", "b.py"][..],
            &["report", "pipeline.py", "mid"][..], // `--refresh a b`: handled by check_py_files
        ] {
            let a = strings(args);
            assert!(wrong_order_error("run", &a, &a).is_none(), "{args:?}");
        }
    }

    /// barca never offers fuzzy suggestions (`barca docs agents`): a guess can read as
    /// confirmation. clap is built without its `suggestions` feature.
    #[test]
    fn argument_errors_carry_no_similar_argument_tip() {
        for argv in [
            "barca get total p.py --jsn",
            "barca run t p.py --refesh a",
            "barca lst p.py",
        ] {
            let Err(e) = Cli::try_parse_from(argv.split_whitespace()) else {
                panic!("`{argv}` must not parse");
            };
            let text = CliError::from_clap(&e).render(true).to_lowercase();
            assert!(!text.contains("similar"), "{argv}: {text}");
            assert!(!text.contains("did you mean"), "{argv}: {text}");
        }
    }

    #[test]
    fn unknown_target_remediation_is_one_wording_on_every_command() {
        let files = vec!["pipeline.py".to_string()];
        for command in ["get", "run", "status", "stats"] {
            let e = CliError::from_barca(
                barca_core::BarcaError::AssetNotFound("nope".into(), "pipeline.py:a".into()),
                &Context {
                    command,
                    files: files.clone(),
                },
            );
            assert_eq!(
                e.remediation.as_deref(),
                Some("Run `barca list pipeline.py` to see available assets and tasks."),
                "{command}"
            );
        }
    }

    #[test]
    fn list_hint_names_the_files_or_a_placeholder() {
        assert_eq!(
            list_hint(&[PathBuf::from("a.py"), PathBuf::from("my dir/b.py")]),
            "Run `barca list a.py 'my dir/b.py'` to see available assets and tasks."
        );
        assert_eq!(
            list_hint(&[]),
            "Run `barca list <file.py>` to see available assets and tasks."
        );
    }

    #[test]
    fn top_level_description_names_barca_list() {
        let cmd = Cli::command();
        for text in [cmd.get_about(), cmd.get_long_about()] {
            let text = text.map(|s| s.to_string()).unwrap_or_default();
            assert!(text.contains("barca list"), "{text}");
        }
    }

    #[test]
    fn fields_flag_exists_on_every_command_with_json_output() {
        let root = Cli::command();
        for name in ["get", "run", "list", "history", "stats", "docs"] {
            let sub = root.find_subcommand(name).unwrap();
            assert!(
                sub.get_arguments().any(|a| a.get_id() == "fields"),
                "`barca {name}` emits JSON, so it needs --fields"
            );
        }
    }

    #[test]
    fn list_shaped_commands_take_limit_and_all() {
        let root = Cli::command();
        for name in ["list", "history"] {
            let sub = root.find_subcommand(name).unwrap();
            for flag in ["limit", "all"] {
                assert!(
                    sub.get_arguments().any(|a| a.get_id() == flag),
                    "`barca {name}` is list-shaped, so it needs --{flag}"
                );
            }
        }
    }

    #[test]
    fn a_target_argument_is_a_comma_separated_list() {
        assert_eq!(parse_targets("total").unwrap(), vec!["total"]);
        assert_eq!(parse_targets("a,b").unwrap(), vec!["a", "b"]);
        assert_eq!(parse_targets("b,a,b").unwrap(), vec!["b", "a"]);
        assert_eq!(
            parse_targets("pipeline.py:a,b").unwrap(),
            vec!["pipeline.py:a", "b"]
        );
        for bad in ["a,,b", "a,", ",a"] {
            let err = parse_targets(bad).unwrap_err();
            assert!(err.contains("empty target name"), "{bad}: {err}");
        }
    }

    #[test]
    fn several_targets_split_from_the_files() {
        let (target, files) = split_target_files(vec!["a,b".into(), "pipeline.py".into()]);
        assert_eq!(target.as_deref(), Some("a,b"));
        assert_eq!(files, vec![PathBuf::from("pipeline.py")]);
    }

    #[test]
    fn json_flags_exist_on_inspection_commands() {
        let root = Cli::command();
        for name in ["list", "history", "stats", "status", "docs"] {
            let sub = root.find_subcommand(name).unwrap();
            assert!(
                sub.get_arguments().any(|a| a.get_id() == "json"),
                "`barca {name}` needs a --json flag for machine-readable output"
            );
        }
    }

    /// One override family everywhere: every command that follows the TTY rule takes both
    /// `--json` and `--pretty` (see output.rs), and the manual explains the rule.
    #[test]
    fn tty_aware_commands_take_json_and_pretty_and_the_rule_is_documented() {
        let root = Cli::command();
        for name in ["get", "run", "list", "history", "stats"] {
            let sub = root.find_subcommand(name).unwrap();
            for flag in ["json", "pretty"] {
                assert!(
                    sub.get_arguments().any(|a| a.get_id() == flag),
                    "`barca {name}` needs --{flag}"
                );
            }
        }
        for name in ["get", "run"] {
            let help = after_help(root.find_subcommand(name).unwrap());
            assert!(
                help.contains("--pretty"),
                "`barca {name} --help` needs a --pretty example"
            );
        }
        let agents = docs::find("agents").expect("agents topic").body;
        for needle in ["BARCA_OUTPUT", "--pretty", "--json", "terminal"] {
            assert!(
                agents.contains(needle),
                "`barca docs agents` must mention {needle}"
            );
        }
        assert!(after_help(&root).contains("BARCA_OUTPUT"));
    }
}
