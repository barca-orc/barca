//! Argument schema and command help.

use crate::{
    bounded,
    output::{self, Format, FormatFlags},
};
use clap::{Parser, ValueEnum, builder::PossibleValuesParser};
use std::path::PathBuf;

/// `-o` on get/run. Without `-o`, `--json` / `--pretty` / BARCA_OUTPUT / the terminal decide.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum OutputMode {
    /// One-line JSON (default when stdout is not a terminal)
    Json,
    /// Just the final_output value, pretty-printed
    Value,
    /// Human-friendly with timing info (default when stdout is a terminal)
    Pretty,
}

impl From<OutputMode> for barca_core::report::ResultFormat {
    fn from(mode: OutputMode) -> Self {
        match mode {
            OutputMode::Json => Self::Json,
            OutputMode::Value => Self::Value,
            OutputMode::Pretty => Self::Pretty,
        }
    }
}

impl OutputMode {
    /// `-o` wins; otherwise the shared rule in `output::resolve`.
    pub(crate) fn resolve(o: Option<OutputMode>, flags: FormatFlags) -> OutputMode {
        o.unwrap_or_else(|| match output::resolve(flags.explicit()) {
            Format::Json => OutputMode::Json,
            Format::Pretty => OutputMode::Pretty,
        })
    }
}

// ─── Help text ────────────────────────────────────────────────────────────────
//
// Every command carries runnable examples. The tests in tests.rs parse each
// `barca ...` example line against the real CLI, so these cannot drift from the flags.
// When you add or change a flag, update the examples here and the matching `barca docs`
// topic in crates/barca-cli/docs/.

const TOP_HELP: &str = "\
Quick start:
  barca list                        # discover every asset, task and dependency in the project
  barca list pipeline.py            # ... or only those in one file
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
  BARCA_REMOTE=off barca get total pipeline.py   # local artifacts and history for this process
  barca get total                          # find `total` anywhere in the project and get it
  barca get total pipelines/               # only read files under pipelines/
  barca get                                # every asset and sensor in the project, never tasks
  barca get pipeline.py                    # every asset and sensor, never tasks; prints the last asset's value
  barca get total pipeline.py              # one target and only its upstream cone
  barca get total,orders pipeline.py       # several targets in one run; shared upstream runs once
  barca get total pipeline.py other.py     # target defined across several files
  barca get total pipeline.py --refresh-all        # recompute everything in that cone
  barca get total pipeline.py --refresh clean      # recompute clean and everything downstream of it
  barca get total pipeline.py --refresh clean --no-cascade   # recompute only clean
  barca get total pipeline.py --dry-run    # what would run vs come from cache; no steps execute
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
secrets redacted). `warnings` is always an array: plan-time warnings for the steps this command
planned, [] when there are none; each is {kind, node, param, message} and is also one
`[barca] warning: ...` line on stderr. The only kind is unused_input: a step declares an input its
function never uses (barca docs assets, \"Unused inputs\"). Warnings never change the exit code.
--dry-run reports the same list. For parquet/pickle assets final_output is a pointer,
{\"_barca_artifact\": {\"path\", \"format\", \"size_bytes\"}}; the Python API (barca.get)
loads the value for you.
Several targets (`a,b`, comma-separated, no spaces): final_output is replaced by `targets`, keyed by
target, each {status: success, final_output} or {status: failed, failed_node, error}. Every target
runs even if another fails; exit 1 if any failed.
Refresh: the same vocabulary as `barca run`. --refresh takes ONE comma-separated list of assets in
the cone (the target itself may be named) and also re-runs everything downstream of them;
--no-cascade re-runs only the named ones. --refresh-all re-runs every asset in the cone.
--no-cache is a deprecated spelling of --refresh-all: it still works and warns on stderr.
A cached step whose artifact file is gone is computed again when something needs to read it (a
step that runs takes it as an input, or it is a target), with reason `artifact_missing` and a
warning on stderr. A missing artifact that nothing reads is left alone (barca docs cache).
Completed steps are recorded during the run; remote results require confirmed upload.
Use `barca status pipeline.py` in another terminal to inspect recorded progress. Shared history
is published when the run ends (barca docs remote).
Targets must be assets; use `barca run` for tasks. With no target, get materializes every asset and
sensor and skips tasks (previously it ran tasks too); stderr names the skipped tasks and the
`barca run` command. A file with only tasks gets nothing: exit 0, empty `steps`.
The target comes before the files: `barca get pipeline.py total` exits 2 and prints
`barca get total pipeline.py`.
Errors: in JSON mode the last stderr line is one JSON object {error, code, kind, remediation},
plus node, traceback and artifact_dir when a step failed. Exit 1 step failed, 2 usage error,
3 barca/infra failure, 130 cancelled. A failed step still prints a stdout result line with
status \"failed\" and failed_node.
A required history-write failure exits 3 and preserves previously committed history.
More: barca docs cache, barca docs types, barca docs agents";

const RUN_HELP: &str = "\
Examples:
  BARCA_REMOTE=off barca run deploy pipeline.py   # local artifacts and history for this process
  barca run deploy                                     # find the task anywhere in the project and run it
  barca run deploy pipelines/                          # only read files under pipelines/
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
`warnings` is always an array, as on `barca get`: plan-time warnings for the steps in the cone, such
as an input a step never uses (barca docs assets); [] when there are none.

--refresh takes ONE comma-separated list (`--refresh a,b`), never `--refresh a b`. It re-runs the
assets you name and every asset downstream of them in the task's cone (reason `refresh_cascade`),
so fresh data reaches the task. --no-cascade re-runs only the named assets; cached assets
downstream of them then do not reflect the refresh, and barca warns. A name that is not an
upstream asset is an error. --no-cache is a deprecated spelling of --refresh-all: it still works
and warns on stderr.
An upstream asset that is cached but whose artifact file is gone is computed again before the task
reads it (reason `artifact_missing`, a warning on stderr), so a deleted artifact does not fail the
run (barca docs cache).
The target must be a task; use `barca get` for assets. The target comes before the files:
`barca run pipeline.py deploy` exits 2 and prints `barca run deploy pipeline.py`. Every usage
error exits 2 and ends by pointing at `barca list` (with the files you gave, if any).
No files: barca reads every file in the project that imports barca (barca docs discovery).
Errors: in JSON mode the last stderr line is one JSON object {error, code, kind, remediation}
(see barca docs agents). Exit 1 step failed, 2 usage error, 3 barca/infra failure, 130 cancelled.
A raising task, or one that calls sys.exit(), fails the run: exit 1, and the stdout JSON line has
status \"failed\" and failed_node.
More: barca docs tasks, barca docs cache, barca docs agents";

const PLAN_HELP: &str = "\
Examples:
  barca plan                          # the whole project
  barca plan pipeline.py              # phases and steps that would run; nothing executes
  barca plan pipeline.py other.py     # several files form one DAG

Output: always pretty-printed JSON {total_steps, phases: [{reason, streams: [{stream_id, steps}]}],
warnings}. `warnings` is always an array ([] when there are none) of {kind, node, param, message}:
plan-time warnings such as an input a step never uses, each also one `[barca] warning: ...` line on
stderr (barca docs assets, \"Unused inputs\").
`reason` is an object: {\"type\": \"initial\"} or {\"type\": \"fan_in\", \"node_id\": ...}.
Planning uses the execution pool size (available cores, or BARCA_POOL_SIZE).
Parsing is static; partitions(<expression>) evaluates Python while loading its keys.
Planning reads no state and takes no --env.
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
  barca stats total                         # find `total` anywhere in the project
  barca stats total pipeline.py             # timing percentiles and cache hit rate
  barca stats total pipeline.py --json      # {id, total_runs, cache_hit_rate, ..., recent_runs}, even in a terminal
  barca stats total pipeline.py --pretty    # the text report, even when piped
  barca stats total pipeline.py --fields status,error_message   # JSON; trims recent_runs entries

More: barca docs cache";

const SERVE_HELP: &str = "\
Examples:
  BARCA_STATE=off barca serve pipeline.py    # local history; keep configured artifact sharing
  BARCA_REMOTE=off barca serve pipeline.py   # local artifacts and history
  barca serve                                # every file in the project; files added later need a restart
  barca serve pipeline.py                    # HTTP API on 127.0.0.1:8274 plus the scheduler
  barca serve pipeline.py --port 8400        # custom port
  barca serve pipeline.py --host 0.0.0.0     # all interfaces (containers, VMs); the API has no auth
  barca serve pipeline.py --watch            # dev: re-parse the DAG when files change
  barca serve pipeline.py --no-schedule      # API only; Schedule(...) nodes do not fire
  barca serve pipeline.py --timezone utc     # evaluate cron in UTC (default: local)
  barca serve pipeline.py --timezone America/New_York   # an IANA zone name
  barca serve pipeline.py --read-only        # inspect only: no runs, no scheduler, DB never written

Binds to 127.0.0.1 by default. There is no authentication: with --host 0.0.0.0, anyone who
can reach the port can trigger runs, so keep it on a private network or behind a proxy.
Invalid sources and their graph dependents are excluded; healthy definitions and schedules
continue. Startup logs, /health load_errors and the UI show unloaded files and affected nodes.
--watch restores repaired configured sources; one-shot commands remain strict.
More: barca docs scheduling";

const LIST_HELP: &str = "\
Examples:
  barca list                         # every node in the project (files that import barca)
  barca list pipelines/              # only files under pipelines/ (trailing / marks a directory)
  barca list .                       # the current directory and below
  barca list pipeline.py             # nodes, including qualified/aliased Barca decorators
  barca list pipeline.py --json      # {nodes: [{id, kind, freshness, schedule?, inputs, env, next_fire?}], total, truncated, root}
  barca list pipeline.py --pretty    # the table, even when piped
  barca list pipeline.py --fields id,inputs   # JSON with only these keys per node
  barca list big.py --limit 20       # first 20 nodes (topological order)
  barca list big.py --all            # every node (default: at most 100)
  barca list a.py b.py               # several files form one DAG

An ENV column (and `env` in JSON) lists the environment variables each node declares with
@asset(env=[...]); their values are part of the run hash. `freshness` is `always`, `manual` or
`schedule`; a scheduled node also has `schedule` (the cron expression) and `next_fire`.
`next_fire` (NEXT FIRE (LOCAL TIME) in the table) is the next match of the cron expression in
this machine's local time, which is when `barca serve` fires it unless the server was started
with --timezone. `list` does not know a server's zone: GET /schedule on the running server
reports the times it will fire at (barca docs scheduling).
`list` reads no state, so it takes no --env.

Node ids are relative to the project root (`root` in JSON; barca docs discovery).
Run this first to confirm barca discovered your nodes. When more nodes exist than are shown,
JSON says `\"truncated\": true` with the `total`, and the table prints a note on stderr.
More: barca docs assets, barca docs agents";

const STATUS_HELP: &str = "\
Examples:
  barca status                             # every node in the project
  barca status pipeline.py                 # table in a terminal (JSON when piped): kind, cache state, last run, shape
  barca status total pipeline.py           # only `total` and its upstream cone
  barca status total,orders pipeline.py    # several targets: the union of their cones
  barca status pipeline.py --json          # {target, targets, nodes, summary, total, truncated, root}, even in a terminal
  barca status pipeline.py --pretty        # the table, even when piped
  barca status pipeline.py --fields id,cache   # JSON with only these keys per node
  barca status big.py --limit 20           # first 20 nodes (default: at most 100); the summary counts all
  barca status total pipeline.py --json --sample 5   # add up to 5 sample rows per json/parquet artifact
  barca status pipeline.py --env dev       # state recorded in another environment

Cache state per node: cached, stale (ran before; code or inputs changed, or its artifact file is
gone and a run would read it: reason `artifact_missing`), never_run, partial
(some partition keys cached), unknown (dynamic partitions not yet known) or always_runs (tasks,
sensors), with a reason. JSON spells the states in snake_case, the same as the `summary` keys
(the table prints never-run, always-runs). It is the same decision `--dry-run` makes.
No steps execute. Optimistic shared history may synchronize locally; partitions(<expression>)
may import the source to resolve keys. Shape (rows, columns, type) is read from the
artifact file only. With remote storage it is read from the bucket: a parquet footer by ranged
requests, json and pickle by a download of up to 16 MB (larger ones get a `note`). A store that
cannot be reached is a `note` on each shape, not a failed command.
More: barca docs status, barca docs agents";

const SQL_HELP: &str = "\
Examples:
  barca sql \"select * from revenue\"                       # one view per asset, using its declared name or function fallback
  barca sql \"select region, sum(amount) from orders group by 1\"   # any DuckDB SQL over cached results
  barca sql \"select * from orders o join revenue r using (region)\" # join assets
  barca sql \"select * from weekly where partition = 'week=w1'\"   # partitioned: one view, a partition column
  barca sql \"select status from validate\"                  # a task's last result is a view too
  barca sql \"select * from orders\" pipeline.py              # only nodes in this file become views
  barca sql \"select * from orders\" --limit 20               # first 20 rows (default: at most 100)
  barca sql \"select * from orders\" --all                    # every row
  barca sql \"select * from orders\" --json                   # {columns, rows, total, truncated, hint?}
  barca sql \"select * from orders\" --env dev                # results cached in another environment

Every asset, sensor and task with a result on disk is a view named after its declared name
(or its function name when unnamed; full id, quoted, when two nodes share a name; stderr says so).
An asset whose code or inputs changed is still queryable at its last result, with a note on stderr.
Only parquet and json results can be queried; pickles cannot. Runs in an in-memory DuckDB over
artifact files; no pipeline step runs and no new run is recorded. Install SQL support: uv add 'barca[sql]'.
partitions(<expression>) may evaluate Python while loading its keys.
Partitioned views include current known keys only; removed keys stay in history. Unknown derived
keys require their source to materialize first. Zero current keys have no result view.
With remote storage, the artifacts of the views a query names are downloaded into
.barca/sql-cache/ (stderr says so) and reused while the objects are unchanged. Optimistic shared
history synchronizes locally; explicit SQL COPY ... TO writes the requested output file.
Errors exit 2: a node with no result yet names the `barca get` to run first; an unknown view lists
the views; a SQL error carries DuckDB's message. A remote artifact that cannot be fetched exits 3.
Experimental: barca docs contract.
More: barca docs sql";

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
    long_about = "Barca runs Python asset graphs with caching by run hash.\n\
                  Every asset output is fully materialized to an artifact file at step \
                  boundaries (json, pickle, or parquet) — that persistence is the cache \
                  checkpoint. pandas/polars DataFrames, pyarrow Tables and duckdb relations \
                  are written as parquet; parameter type annotations choose how downstream \
                  steps read it back (pandas by default, or polars, pyarrow, duckdb) but do \
                  not skip materialization. Start with `barca list` (run inside the project) to discover a \
                  project's assets and tasks; run `barca docs` for the manual.",
    after_help = TOP_HELP,
    version
)]
pub(crate) enum Cli {
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
        /// [TARGET[,TARGET...]] [file.py|dir/ ...] — both optional: no target gets every asset
        /// and sensor; no files reads the whole project (`barca docs discovery`)
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
        /// executing steps or recording a run (shared history may synchronize)
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
        /// TARGET[,TARGET...] [file.py|dir/ ...] — one or more target tasks, comma-separated; no
        /// files reads the whole project (`barca docs discovery`)
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
        /// executing steps or recording a run (shared history may synchronize)
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
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`)
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
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`)
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
    /// Binds to 127.0.0.1 by default (local only); --host changes the address.
    /// There is no authentication. POST /run and /get trigger async runs; poll
    /// GET /status/<run_id> for results.
    #[command(after_help = SERVE_HELP)]
    Serve {
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`). With --watch, files added later are not picked up until restart
        files: Vec<PathBuf>,
        /// Port to bind on
        #[arg(short, long, default_value = "8274")]
        port: u16,
        /// IP address to bind on; 0.0.0.0 (or ::) listens on every interface. The API has no authentication
        #[arg(long, default_value = "127.0.0.1")]
        host: std::net::IpAddr,
        /// Dev mode: re-parse the DAG when source files change
        #[arg(long)]
        watch: bool,
        /// Disable the cron scheduler (Schedule(...) assets will not auto-fire)
        #[arg(long)]
        no_schedule: bool,
        /// Timezone for cron evaluation: local (this machine's zone) or utc, in any letter case, or an IANA name such as America/New_York, which is case-sensitive. Any other value is a usage error
        #[arg(long, default_value = "local", value_parser = parse_timezone)]
        timezone: String,
        /// Inspect only: refuse runs, never schedule, read the metadata DB from snapshots
        #[arg(long)]
        read_only: bool,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// List all discovered definitions (assets, tasks, sensors) with their deps
    ///
    /// Scheduled definitions also show their next fire time, computed in this
    /// machine's local time (a server started with --timezone fires in that zone).
    #[command(after_help = LIST_HELP)]
    List {
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`)
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
        /// [TARGET[,TARGET...]] [file.py|dir/ ...] — both optional; no files reads the whole
        /// project (`barca docs discovery`)
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
    /// Query cached results with SQL (DuckDB) — each asset is a named view
    ///
    /// No pipeline step executes or new run is recorded. Shared history may synchronize;
    /// partition expressions may evaluate Python; explicit SQL COPY can write files.
    #[command(after_help = SQL_HELP)]
    Sql {
        /// The SQL query (DuckDB dialect); quote it as one argument
        query: String,
        /// Python files or directories to read (default: every .py file under the project root
        /// that imports barca; see `barca docs discovery`)
        files: Vec<PathBuf>,
        #[command(flatten)]
        format: FormatFlags,
        /// Maximum number of rows to return
        #[arg(short, long, default_value_t = bounded::LIST_DEFAULT_LIMIT)]
        limit: usize,
        /// Return every row (no limit)
        #[arg(long, conflicts_with = "limit")]
        all: bool,
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

fn parse_timezone(value: &str) -> Result<String, String> {
    barca_core::schedule::Zone::parse(value).map(|_| value.to_string())
}
