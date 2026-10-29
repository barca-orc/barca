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

/// `-o` on get. `path` and `value` choose what is returned and print only that, unless
/// `--json` (or `--pretty`) asks for the whole result; `json` and `pretty` are the older
/// spellings of `--json` and `--pretty`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum GetOutput {
    /// Artifact paths, one per line (one per selected partition key); the default
    Path,
    /// The value, pretty-printed (a mapping of key to value for a partitioned target)
    Value,
    /// A signed HTTPS URL to each artifact in the store, one per line (experimental)
    Url,
    /// One-line JSON, as --json
    Json,
    /// Summary and paths for people, as --pretty
    Pretty,
}

#[derive(clap::Subcommand, Debug)]
pub(crate) enum TokenCommand {
    /// Print a one-use 15-minute bootstrap code; replaces any unconsumed grant
    #[command(
        after_help = "Examples:\n  barca tokens bootstrap    # on the deployed host, in the project root",
        after_long_help = "Examples:\n  barca tokens bootstrap    # on the deployed host, in the project root"
    )]
    Bootstrap,
}

/// `--stream` on logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum LogStreamArg {
    /// What the steps printed to stdout
    Stdout,
    /// What the steps wrote to stderr: logging, warnings, tracebacks of failed attempts
    Stderr,
}

impl From<LogStreamArg> for barca_core::LogStream {
    fn from(arg: LogStreamArg) -> Self {
        match arg {
            LogStreamArg::Stdout => Self::Stdout,
            LogStreamArg::Stderr => Self::Stderr,
        }
    }
}

/// `--server [<URL>]` and `--local`: where a command runs (RFC-0007 §3.1). On `get`, `run`,
/// `status`, `list`, `history`, `stats`, `logs` and `sql` only; the other commands always run
/// here.
#[derive(clap::Args, Clone, Debug, Default)]
pub(crate) struct ServerFlags {
    /// Run this command on a barca server (`barca serve`) and print its result here: the URL
    /// given, or with no URL the configured one (BARCA_SERVER, else [server] url in
    /// barca.toml). Without it, get and run run on this machine (against a configured server's
    /// ledger), status, history, stats and logs are answered by a configured server, and sql
    /// resolves its views from it (the query always runs here)
    #[arg(
        long,
        value_name = "URL",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "local"
    )]
    #[arg(help = "Use a server (URL or configured); get/run execute its deployed code", long_help = None)]
    pub(crate) server: Option<String>,
    /// Ignore any configured server for this command: run against this machine's own ledger and
    /// records; nothing is read from or sent to the server
    #[arg(long)]
    #[arg(help = "Use independent local state; ignore the configured server", long_help = None)]
    pub(crate) local: bool,
}

// ─── Help text ────────────────────────────────────────────────────────────────
//
// Every command carries runnable examples. The tests in tests.rs parse each
// `barca ...` example line against the real CLI, so these cannot drift from the flags.
// When you add or change a flag, update the examples here and the matching `barca docs`
// topic in crates/barca-cli/docs/.

const TOP_HELP: &str = "\
Examples:
  barca get report                            # get an asset; reuse unchanged results
  barca run publish                           # run a task and its dependencies
  barca get report --partition customer=acme   # select one partition
  barca get report --server http://host:8274   # run deployed code there

Learn: barca <command> --help | barca <command> --examples | barca docs <topic>
Agents: barca docs skill; full contract: barca docs agents
Output: --json for structured results; --pretty for people; BARCA_OUTPUT sets the default.";

const ROOT_HELP: &str = "\
{about}

Usage: {usage}

Run:
  get       Get an asset; reuse unchanged results
  run       Run a task and its dependencies
  serve     Manage runs, schedules and results on a server

Inspect:
  list      Discover assets, tasks and dependencies
  status    See what is cached or needs updating
  history   Inspect previous runs
  logs      Read a run's output

Tools:
  plan      Inspect the execution plan as JSON
  doctor    Check project prerequisites
  stats     Inspect asset execution statistics
  sql       Query cached results with SQL
  queue     Inspect or control the server queue
  gc        Free cache space
  state     Manage metadata backups
  tokens    Bootstrap project token access

Learn:
  docs      Read the built-in manual or list topics
  version   Print version information
  help      Show command help

Options:
{options}{after-help}";

/// Default help is a small recipe; --examples retains every tested example.
fn quick_help(name: &str, examples: &str) -> String {
    let selected: Vec<&str> = match name {
        "get" => vec![
            "  barca get report",
            "  barca get report --dry-run --json",
            "  barca get report --partition customer=acme",
        ],
        "run" => vec![
            "  barca run publish",
            "  barca run publish --server http://host:8274",
        ],
        _ => examples
            .lines()
            .filter(|line| line.trim_start().starts_with("barca "))
            .take(3)
            .collect(),
    };
    let more = examples
        .lines()
        .find(|line| line.starts_with("More:"))
        .unwrap_or("More: barca docs");
    format!(
        "Examples:\n{}\n\nAll examples: barca {name} --examples\n{more}",
        selected.join("\n")
    )
}

const GET_HELP: &str = "\
Examples:
  BARCA_REMOTE=off barca get total pipeline.py   # local artifacts and history for this process
  barca get total                          # find `total` anywhere in the project and get it
  barca get total pipeline.py --json --fields id,status,elapsed_seconds,attempts   # timing and retries
  barca get total pipelines/               # only read files under pipelines/
  barca get                                # every asset and sensor in the project, never tasks
  barca get pipeline.py                    # every asset and sensor, never tasks; prints the last asset's path
  barca get total pipeline.py              # one target and only its upstream cone
  barca get total,orders pipeline.py       # several targets in one run; shared upstream runs once
  barca get total pipeline.py other.py     # target defined across several files
  barca get orders pipeline.py --refresh orders --json  # check recomputed artifact bytes
  barca get total pipeline.py --refresh-all        # recompute everything in that cone
  barca get total pipeline.py --refresh clean      # recompute clean and everything downstream of it
  barca get total pipeline.py --refresh clean --no-cascade   # recompute only clean
  barca get total pipeline.py --dry-run    # what would run vs come from cache; no steps execute
  barca get total pipeline.py --dry-run --json   # also `uploads`: results still waiting for the store
  barca get scored pipeline.py --dry-run --json  # partitions.changed: per key, what changed (a dict sensor's new value)
  barca get total pipeline.py --json       # JSON even in a terminal (the default when piped)
  barca get total pipeline.py --pretty     # summary and artifact path for humans (the default in a terminal)
  barca get total pipeline.py -o path      # only the artifact path, one per line (pipe it to xargs cat)
  barca get total pipeline.py -o value     # only the value, pretty-printed (json, parquet, CSV, pickle)
  barca get total pipeline.py -o value --json   # the JSON result with the value in final_output
  barca get total pipeline.py -o url       # a signed HTTPS link to the result in the store (1 hour)
  barca get total pipeline.py -o url --expires 7d   # a link that works for 7 days (the maximum)
  barca get sales pipeline.py -o url --partition region=emea --expires 30m   # one key's link
  barca get sales pipeline.py              # a partitioned target: one path per key
  barca get scored pipeline.py             # with @sink(\"exports/scored/\"): writes the keys that ran as a Hive dataset (barca docs sinks)
  barca get sales pipeline.py --partition region=emea,amer   # only these keys: computed and returned
  barca get margin pipeline.py --partition region=apac   # upstream partitioned assets compute apac only
  barca get sales pipeline.py --partition region=emea --refresh sales   # recompute only that key
  barca get pipeline.py --partition region=emea   # no target: every asset and sensor, for emea only
  barca get pipeline.py --partition region=emea --dry-run   # what runs for emea, which fan-ins are skipped
  barca get scored pipeline.py --batch all       # a backfill: every key that needs to run in one call (batch= nodes only)
  barca get scored pipeline.py --batch 20        # at most 20 keys per call, in key order
  barca get scored pipeline.py --batch none      # one key per call, to find the key that fails
  barca get scored pipeline.py --batch scored=all --batch cleaned=100   # per node; repeatable
  barca get scored pipeline.py --dry-run --batch 20   # \"500 keys in 25 calls (batch=20)\" in the WHY column
  barca get total pipeline.py --agent      # plain progress lines on stderr
  barca get total pipeline.py --env dev    # separate cache and state per environment
  barca list pipeline.py                   # not sure of the name? list assets and tasks first
  BARCA_OUTPUT=json barca get total pipeline.py   # env override for CI; a flag still wins
  barca get total pipeline.py --fields id,status   # JSON with each entry of `steps` trimmed to these keys
  barca get total --server                 # run it on the configured barca server; prints here as a local run
  barca get total --server https://barca.example.com --json   # on that server (barca docs server-mode)
  barca get total --local                  # this machine's ledger only; nothing is sent to a configured server
  barca get --examples                     # the complete example collection; runs nothing

More: barca docs cache, barca docs partitions, barca docs types, barca docs sinks, barca docs agents";

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
  barca run notify pipeline.py --partition region=emea   # the task's cone, for the emea keys only
  barca run notify pipeline.py --batch all             # override batch= of the cone's batched nodes for this run
  barca run deploy pipeline.py --json                  # JSON even in a terminal (the default when piped)
  barca run deploy pipeline.py --pretty                # summary for humans (the default in a terminal)
  barca run deploy pipeline.py --fields id,status,reason   # JSON with each entry of `steps` trimmed
  barca run check_a,check_b pipeline.py                # several tasks in one run; shared upstream runs once
  barca run check_a,check_b pipeline.py --dry-run      # preview the union of both cones
  barca run deploy --server --agent                    # run it on the configured barca server (barca docs server-mode)
  barca run deploy pipeline.py --local                 # run here and post nothing to a configured server
  barca list pipeline.py                               # not sure of the name? list assets and tasks first

More: barca docs tasks, barca docs cache, barca docs agents";

const PLAN_HELP: &str = "\
Examples:
  barca plan                          # the whole project
  barca plan pipeline.py              # phases and steps that would run; nothing executes
  barca plan pipeline.py other.py     # several files form one DAG
  barca plan pipeline.py --partition region=emea   # only the emea keys; skipped fan-ins listed
  barca plan pipeline.py --json       # the same JSON: --json and --pretty are accepted and change nothing

More: barca docs agents";

const DOCTOR_HELP: &str = "\
Examples:
  barca doctor                      # diagnose the discovered project
  barca doctor pipeline.py --json    # structured checks, no steps executed
  barca doctor src/ --env local --pretty   # selected sources and environment
  barca doctor --server                # also the configured server: version, store, load errors, schedules
  barca doctor --server http://127.0.0.1:8274 --json   # a server named here
  barca doctor --local                 # this machine only, even with a server configured

More: barca docs doctor";

const LOGS_HELP: &str = "\
Examples:
  barca logs                            # the most recent run; stderr names which one
  barca logs -f                         # follow the newest running run until it ends
  barca logs 7f30b3e9 --follow --step report   # follow one step of a run
  barca logs --step report              # one step of the most recent run
  barca logs 7f30b3e9 --stream stderr   # only stderr: logging, warnings, tracebacks
  barca logs 7f30b3e9 --timestamps      # each line after its UTC arrival time
  barca logs 7f30b3e9 --pretty          # recorded lines, without progress messages
  barca logs 7f30b3e9 --step report --pretty   # one function name or exact node id
  barca logs 7f30b3e9 --json            # [{node_id, stream, ts, line}] in recorded order
  barca logs 7f30b3e9 --env dev         # read another environment's local history
  barca logs --server -f                # follow the newest running run on the server
  barca logs 7f30b3e9 --local           # this machine's records, despite a configured server

More: barca docs logs";

const HISTORY_HELP: &str = "\
Examples:
  barca history                # last 10 in a terminal, every run as JSON when piped
  barca history -l 25          # last 25
  barca history --all          # every recorded run
  barca history --reconcile RUN_ID --json   # record a verified-stopped run as interrupted
  barca history --json         # {runs: [...], total, truncated, hint?}, even in a terminal
  barca history --pretty       # the table, even when piped
  barca history --fields run_id,status,elapsed_seconds,attempts   # JSON with only these keys per run
  barca history --fields run_id,triggered_by,scheduled_at   # what started each run (manual, api, schedule, client)
  barca history --fields run_id,job,status   # what each run was for: `all` or its resolved node ids
  barca history --env dev      # runs recorded in another environment
  barca history --server -l 20 # the runs of the configured barca server (barca docs server-mode)
  barca history --local        # this machine's runs, even with a server configured
  barca history --status failed            # failed runs only, each with its failing step and error
  barca history --status failed,interrupted --since 24h   # what went wrong in the last day
  barca history --target clean             # runs whose target or job is clean
  barca history --triggered-by schedule    # runs started by a cron tick (manual, api, schedule, client)
  barca history --since 2026-10-22T08:00:00Z --json   # runs started at or after a time
  barca history 7f30b3e9       # one run: its steps, attempts, artifacts, errors and tracebacks
  barca history 7f30b3e9 --json   # {run, steps: [...], steps_page: {total, truncated}, hint?}
  barca history 7f30b3e9 --limit 50   # one run's first 50 recorded steps (failed steps always listed)
  barca history 7f30b3e9 --all --json # every step the run recorded
  barca history --status failed --json --fields run_id,failed_node,error   # why each run failed

More: barca docs agents, barca docs cache";

const GC_HELP: &str = "\
Examples:
  barca gc --dry-run              # what would be evicted, per tier, and what it leaves
  barca gc                        # evict now, down to 80% of [cache] max_size
  barca gc --max-size 5GB         # a one-off tighter budget
  barca gc --max-size off         # only leftovers and superseded results, all of them
  barca gc --json                 # {size_bytes, limit_bytes, after_bytes, tiers, evicted, ...}
  barca gc --env dev              # only the dev environment's results (leftovers are shared)

More: barca docs cache (\"Disk use\")";

const STATS_HELP: &str = "\
Examples:
  barca stats total                         # find `total` anywhere in the project
  barca stats total pipeline.py             # timing and peak memory (RSS) percentiles, re-executions
  barca stats total pipeline.py --json      # {id, total_runs, cache_hit_rate, ..., recent_runs}, even in a terminal
  barca stats total pipeline.py --pretty    # the text report, even when piped
  barca stats total pipeline.py --fields status,error_message   # JSON; trims recent_runs entries
  barca stats total --fields run_id,elapsed_seconds,max_rss_bytes   # each recent run's id, time and peak memory
  barca stats total --server                # timings recorded on the configured barca server
  barca stats total --local                 # timings recorded on this machine, even with a server configured

More: barca docs cache";

const SERVE_HELP: &str = "\
Examples:
  BARCA_REMOTE=off barca serve pipeline.py   # local artifacts and history
  BARCA_LOG=json barca serve pipeline.py     # each line a JSON object on stdout, with run_id and node
  BARCA_TELEMETRY=datadog barca serve pipeline.py   # a trace per run; barca.serve.up via DogStatsD
  barca serve                                # every file in the project; files added later need a restart
  barca serve pipeline.py                    # HTTP API on 127.0.0.1:8274 plus the scheduler
  barca serve pipeline.py --port 8400        # custom port
  barca serve pipeline.py --host 0.0.0.0     # all interfaces (containers, VMs); the API has no auth
  barca serve pipeline.py --watch            # dev: re-parse the DAG when files change
  barca serve pipeline.py --no-schedule      # API only; Schedule(...) nodes do not fire
  barca serve pipeline.py --timezone utc     # evaluate cron in UTC (default: local)
  barca serve pipeline.py --timezone America/New_York   # an IANA zone name
  barca serve pipeline.py --read-only        # inspect only: no runs, no scheduler, DB never written
  barca serve pipeline.py --drain-timeout 120   # on SIGTERM, give runs in flight 2 minutes
  barca serve pipeline.py --max-runs 2       # at most 2 runs at once; the others wait in the queue
  barca serve pipeline.py --capacity 4       # the weight= of every run's running steps adds up to at most 4
  barca serve pipeline.py --run-timeout 3600 # stop a run that takes longer than an hour
  barca serve pipeline.py --run-timeout 0    # no run timeout (the default)
  barca serve pipeline.py --env prod         # .barca/envs/prod/metadata.db, named on the startup line

More: barca docs scheduling";

const QUEUE_HELP: &str = "\
Examples:
  barca queue --server                 # the configured server's run queue: running and queued runs
  barca queue --server http://barca.internal:8274 --json  # {paused, paused_at, draining, max_runs, running, queued}
  barca queue pause --server           # queued runs wait (runs in flight go on); kept across restarts
  barca queue resume --server          # queued runs start again, in order
  barca queue                          # the server in BARCA_SERVER or [server] url

More: barca docs scheduling";

const LIST_HELP: &str = "\
Examples:
  barca list                         # project nodes (human table: at most 100)
  barca list pipelines/              # only files under pipelines/ (trailing / marks a directory)
  barca list .                       # the current directory and below
  barca list pipeline.py             # nodes, including qualified/aliased Barca decorators
  barca list pipeline.py --json      # {nodes: [{id, kind, freshness, schedule?, inputs, env, next_fire?}], total, truncated, root}
  barca list pipeline.py --groups --json  # organizational groups, separate from executable nodes
  barca list pipeline.py --pretty    # the table, even when piped
  barca list pipeline.py --fields id,inputs   # JSON with only these keys per node
  barca list big.py --limit 20       # first 20 nodes (topological order)
  barca list big.py --all            # every node in the human table (default: at most 100)
  barca list a.py b.py               # several files form one DAG
  barca list --server --json         # the nodes deployed on the configured barca server

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
  barca status scored pipeline.py --json   # partitions.changed: why each missing key will run
  barca status big.py --limit 20           # first 20 nodes; the summary counts all
  barca status total pipeline.py --json --sample 5   # add up to 5 sample rows per json/parquet/CSV artifact
  barca status pipeline.py --env dev       # state recorded in another environment
  barca status total --server              # cache state on the configured barca server
  barca status total --json                # with a server configured: its ledger, plus `local` per node
  barca status total --local               # this machine's own state, even with a server configured
  barca status --problems                  # only nodes that need attention: not cached, a failed run, a paused or failing schedule
  barca status --state stale,never_run     # only nodes in these cache states; the summary still counts every node
  barca status --problems --server         # what needs attention on the configured server

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
  barca sql \"select target, status, elapsed_seconds from barca.runs where status = 'failed' order by started_at desc limit 10\"
  barca sql \"select node_id, elapsed_seconds, size_bytes from barca.materializations order by created_at desc\"
  barca sql \"select name, state, reason from barca.nodes where state <> 'cached'\"
  barca sql \"select * from revenue\" --local                # this machine's results, not a configured server's
  barca sql \"select * from revenue\" --server https://barca.example.com   # views resolved by that server

More: barca docs sql";

const STATE_HELP: &str = "\
Examples:
  barca state backups                  # the metadata backups, local and in the store, newest first
  barca state backups --json           # {backups: [{index, name, where, size_bytes, run_id, rows, ...}], local_dir, store}
  barca state restore                  # put this machine's newest backup back in place of .barca/metadata.db
  barca state restore --from 1         # the newest, whichever machine took it
  barca state restore --from 3         # the third newest (the index in barca state backups)
  barca state restore --from 2026-10-14T18-02-11.482Z-3f9a1c2e.db  # one backup, by name
  barca state backups --env dev        # another environment's backups
  barca state reset --yes              # start over with an empty database (the old one is kept aside)

More: barca docs remote";

const STATE_BACKUPS_HELP: &str = "\
Examples:
  barca state backups                  # table in a terminal, JSON when piped
  barca state backups --json           # {backups, local_dir, store, store_error?}
  barca state backups --pretty         # the table, even when piped
  barca state backups --env dev        # another environment's backups

More: barca docs remote";

const STATE_RESTORE_HELP: &str = "\
Examples:
  barca state restore                  # this machine's newest backup (the newest if it has none)
  barca state restore --from 1         # the newest, whichever machine took it
  barca state restore --from 2         # the second newest
  barca state restore --from 2026-10-14T18-02-11.482Z-3f9a1c2e  # by name, .db optional
  barca state restore --json           # {restored, from, source, pre_restore, rows, run_id, host, project}
  barca state restore --from .barca/metadata.db.reset-2026-10-20T18-02-11.482Z  # a file barca state reset moved aside

More: barca docs remote";

const STATE_RESET_HELP: &str = "\
Examples:
  barca state reset                    # asks first in a terminal
  barca state reset --yes              # no question (scripts, agents)
  barca state reset --yes --json       # {database, moved_to, moved, undo}
  barca state reset --yes --env dev    # another environment's database

More: barca docs remote";

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
  barca docs cache --pretty     # markdown, the default even when piped

More: barca docs";

#[derive(Parser)]
#[command(
    name = "barca",
    about = "Run Python tasks and cache assets, locally or on a server.",
    disable_help_flag = true,
    arg = clap::Arg::new("help").short('h').long("help").global(true)
        .action(clap::ArgAction::HelpShort).help("Show concise command help"),
    arg = clap::Arg::new("examples").long("examples").global(true)
        .action(clap::ArgAction::HelpLong).help("Show all command examples without running anything"),
    help_template = ROOT_HELP,
    after_help = TOP_HELP,
    after_long_help = TOP_HELP,
    version
)]
pub(crate) enum Cli {
    /// Manage project authentication on the deployed host
    #[command(
        after_help = "Examples:\n  barca tokens bootstrap    # print a one-use bootstrap code",
        after_long_help = "Examples:\n  barca tokens bootstrap    # print a one-use bootstrap code"
    )]
    Tokens {
        #[command(subcommand)]
        command: TokenCommand,
    },

    /// Get assets; reuse unchanged results and print their artifact paths
    ///
    /// If the first positional arg ends in .py, all args are treated as files
    /// (no target — gets every asset and sensor; tasks are skipped, use `barca run` for
    /// them). Otherwise, the first arg is the target
    /// asset name (or several, comma-separated: `a,b`) and the rest are files.
    ///
    /// Each completed step writes a fully materialized artifact (never a lazy in-memory
    /// handle). If one computation should produce several cacheable outputs, define
    /// multiple assets or split the work inside a single step before returning.
    #[command(after_help = quick_help("get", GET_HELP), after_long_help = GET_HELP, long_about = None)]
    Get {
        /// [TARGET[,TARGET...]] [file.py|dir/ ...] — both optional: no target gets every asset
        /// and sensor; no files reads the whole project (`barca docs discovery`)
        #[arg(help = "Target(s), then optional files/directories; omit files to discover the project", long_help = None)]
        args: Vec<String>,
        /// What to print: artifact paths (the default), values, or signed URLs. Alone,
        /// -o path|value|url prints only that; with --json, it chooses what `final_output` holds
        #[arg(short, long)]
        #[arg(help = "Choose the result output mode (see possible values)", long_help = None)]
        output: Option<GetOutput>,
        /// With -o url: how long the links work, as 30m, 12h, 7d or seconds (default 1h, at
        /// most 7d). Experimental
        #[arg(long, value_name = "DURATION")]
        #[arg(help = "Signed URL lifetime: 30m, 12h, 7d or seconds (default: 1h; maximum: 7d)", long_help = None)]
        expires: Option<String>,
        #[command(flatten)]
        format: FormatFlags,
        /// Select partition keys across the cone: `region=emea,amer`, or a JSON object for
        /// compound keys (`{"region": "emea", "day": "2026-10-14"}`). Every partitioned node
        /// with a named dimension computes only those keys (--refresh too); a fan-in over them
        /// is skipped. Works without a target
        #[arg(long, value_name = "KEY=VALUE[,VALUE]|JSON")]
        #[arg(help = "Select keys, e.g. customer=acme; affected collect() fan-ins are skipped", long_help = None)]
        partition: Option<String>,
        /// Override `batch=` for this run: `none` (one key per call), `all` or a whole number,
        /// for every node that declares `batch=`; `NODE=V` (repeatable) for one node only.
        /// Nodes without `batch=` are never batched. Experimental
        #[arg(long, value_name = "[NODE=]none|all|N")]
        #[arg(help = "Override declared batching: none, all, N or NODE=N (experimental)", long_help = None)]
        batch: Vec<String>,
        /// Assets to force re-materialize, as ONE comma-separated list (`--refresh a,b`, not
        /// `--refresh a b`); the target itself may be named. Every asset downstream of them in
        /// the target's cone re-materializes too (see --no-cascade)
        #[arg(long, value_delimiter = ',', conflicts_with = "refresh_all")]
        #[arg(help = "Recompute named assets and their downstream dependents: --refresh a,b", long_help = None)]
        refresh: Option<Vec<String>>,
        /// With --refresh: re-materialize only the named assets, not what is downstream of
        /// them. Cached downstream assets then do not reflect the refresh; barca warns
        #[arg(long, requires = "refresh")]
        #[arg(help = "Refresh only named assets; cached downstream results may be outdated", long_help = None)]
        no_cascade: bool,
        /// Force re-materialize EVERY asset in the target's cone (nothing comes from cache)
        #[arg(long)]
        #[arg(help = "Recompute every asset needed by the target", long_help = None)]
        refresh_all: bool,
        /// Show what this command would do (each step cached or will-run, and why) without
        /// executing steps or recording a run
        #[arg(long)]
        #[arg(help = "Preview cached and planned work; execute nothing and record no run", long_help = None)]
        dry_run: bool,
        /// Agent-friendly output: plain structured progress lines instead of visual progress bar
        #[arg(long)]
        #[arg(help = "Print plain structured progress on stderr", long_help = None)]
        agent: bool,
        /// Keep only these keys (comma-separated) on each entry of `steps` in the JSON output.
        /// Implies --json; not valid with --pretty or -o pretty. An unknown key is a usage error
        /// listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STEP_FIELDS))]
        #[arg(help = "Select JSON fields as a comma-separated list; implies --json", long_help = None)]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Run a task and its dependencies; the task runs every time
    ///
    /// The task always re-runs. Upstream assets are served from cache when fresh
    /// (same as `barca get`). Use `--refresh` to force re-materialize specific
    /// upstream assets, or `--refresh-all` to refresh the entire upstream cone.
    #[command(after_help = quick_help("run", RUN_HELP), after_long_help = RUN_HELP, long_about = None)]
    Run {
        /// TARGET[,TARGET...] [file.py|dir/ ...] — one or more target tasks, comma-separated; no
        /// files reads the whole project (`barca docs discovery`)
        #[arg(required = true)]
        #[arg(help = "Target(s), then optional files/directories; omit files to discover the project", long_help = None)]
        args: Vec<String>,
        /// Upstream assets to force re-materialize, as ONE comma-separated list
        /// (`--refresh a,b`, not `--refresh a b`). Every asset downstream of them in the
        /// task's cone re-materializes too (see --no-cascade)
        #[arg(long, value_delimiter = ',', conflicts_with = "refresh_all")]
        #[arg(help = "Recompute named assets and their downstream dependents: --refresh a,b", long_help = None)]
        refresh: Option<Vec<String>>,
        /// With --refresh: re-materialize only the named assets, not what is downstream of
        /// them. Cached downstream assets then do not reflect the refresh; barca warns
        #[arg(long, requires = "refresh")]
        #[arg(help = "Refresh only named assets; cached downstream results may be outdated", long_help = None)]
        no_cascade: bool,
        /// Force re-materialize EVERY asset in the task's cone (nothing comes from cache)
        #[arg(long)]
        #[arg(help = "Recompute every asset needed by the target", long_help = None)]
        refresh_all: bool,
        /// Select partition keys across the task's cone, as for `barca get --partition`: every
        /// partitioned node with a named dimension computes only those keys
        #[arg(long, value_name = "KEY=VALUE[,VALUE]|JSON")]
        #[arg(help = "Select keys, e.g. customer=acme; affected collect() fan-ins are skipped", long_help = None)]
        partition: Option<String>,
        /// Override `batch=` for this run, as for `barca get --batch`. Experimental
        #[arg(long, value_name = "[NODE=]none|all|N")]
        #[arg(help = "Override declared batching: none, all, N or NODE=N (experimental)", long_help = None)]
        batch: Vec<String>,
        /// Show what this command would do (each step cached or will-run, and why) without
        /// executing steps or recording a run
        #[arg(long)]
        #[arg(help = "Preview cached and planned work; execute nothing and record no run", long_help = None)]
        dry_run: bool,
        /// Output format (kept for compatibility; --json / --pretty are the canonical spelling)
        #[arg(short, long, conflicts_with_all = ["json", "pretty"])]
        #[arg(help = "Choose the result output mode (see possible values)", long_help = None)]
        output: Option<OutputMode>,
        #[command(flatten)]
        format: FormatFlags,
        /// Agent-friendly output: plain structured progress lines instead of visual progress bar
        #[arg(long)]
        #[arg(help = "Print plain structured progress on stderr", long_help = None)]
        agent: bool,
        /// Keep only these keys (comma-separated) on each entry of `steps` in the JSON output.
        /// Not valid with -o value/pretty. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STEP_FIELDS))]
        #[arg(help = "Select JSON fields as a comma-separated list; implies --json", long_help = None)]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Parse source files and emit the execution plan as JSON
    #[command(after_help = quick_help("plan", PLAN_HELP), after_long_help = PLAN_HELP, long_about = None)]
    Plan {
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`)
        #[arg(help = "Python files or directories; default: discover the project", long_help = None)]
        files: Vec<PathBuf>,
        /// Plan only these partition keys, as `barca get --partition` does: narrowed nodes keep
        /// the selected keys, and fan-ins over them are left out (listed in partition_selection)
        #[arg(long, value_name = "KEY=VALUE[,VALUE]|JSON")]
        #[arg(help = "Select keys, e.g. customer=acme; affected collect() fan-ins are skipped", long_help = None)]
        partition: Option<String>,
        /// Accepted for symmetry with the other commands: the plan is always pretty-printed
        /// JSON, so this changes nothing
        #[arg(long)]
        json: bool,
        /// Accepted for symmetry with the other commands: the plan is always pretty-printed
        /// JSON, so this changes nothing (there is no table form)
        #[arg(long)]
        pretty: bool,
    },
    /// Diagnose project prerequisites without running pipeline steps
    #[command(after_help = quick_help("doctor", DOCTOR_HELP), after_long_help = DOCTOR_HELP, long_about = None)]
    Doctor {
        /// Python files or directories to inspect (default: discovered project sources)
        #[arg(help = "Python files or directories; default: discover the project", long_help = None)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        /// Also check a barca server: the URL given, or with no URL the configured one
        /// (BARCA_SERVER, else [server] url). A configured server is checked without it
        #[arg(
            long,
            value_name = "URL",
            num_args = 0..=1,
            default_missing_value = "",
            conflicts_with = "local"
        )]
        #[arg(help = "Use a server (URL or configured); get/run execute its deployed code", long_help = None)]
        server: Option<String>,
        /// Check this machine only, without the configured server
        #[arg(long)]
        #[arg(help = "Use independent local state; ignore the configured server", long_help = None)]
        local: bool,
    },
    /// Read a run's recorded step output (stdout and stderr), without progress messages
    #[command(after_help = quick_help("logs", LOGS_HELP), after_long_help = LOGS_HELP, long_about = None)]
    Logs {
        /// Exact run id from get/run JSON or history (default: the most recent run; with
        /// --follow, the newest running run)
        run_id: Option<String>,
        /// Function name or exact node id (including a partition key when desired)
        #[arg(long)]
        step: Option<String>,
        /// Only the lines of this stream (default: both)
        #[arg(long, value_enum)]
        stream: Option<LogStreamArg>,
        /// Print a running run's lines as they arrive and exit when it ends (exit 0 when it
        /// succeeded, 130 when it was cancelled, 1 otherwise). With --json, one JSON object per
        /// line
        #[arg(short, long)]
        follow: bool,
        /// Text output: put each line's arrival time (UTC, milliseconds) before it. JSON always
        /// has it as `ts`
        #[arg(long)]
        timestamps: bool,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment whose history to read
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Show recent run history, or one run's steps and errors
    #[command(after_help = quick_help("history", HISTORY_HELP), after_long_help = HISTORY_HELP, long_about = None)]
    History {
        /// One run in detail: its fields, the steps it recorded (the first 1000, failed steps
        /// always; --limit and --all change it), and each failed step's error, traceback and
        /// how its worker ended
        #[arg(value_name = "RUN_ID", conflicts_with_all = ["reconcile", "fields", "status", "target", "triggered_by", "since"])]
        run_id: Option<String>,
        /// After verifying a run stopped, record it as interrupted; refuses a proven live owner
        #[arg(long, value_name = "RUN_ID", conflicts_with_all = ["limit", "all", "status", "target", "triggered_by", "since"])]
        reconcile: Option<String>,
        /// Only runs with one of these statuses (comma-separated): success, failed, cancelled,
        /// error, running, interrupted
        #[arg(long, value_name = "STATUS[,...]")]
        status: Option<String>,
        /// Only runs whose target or job is this node (a function name or node id); runs of
        /// every node (`all`) are left out
        #[arg(long, value_name = "NAME")]
        target: Option<String>,
        /// Only runs started this way (comma-separated): manual, api, schedule, client
        #[arg(long, value_name = "TRIGGER[,...]")]
        triggered_by: Option<String>,
        /// Only runs started at or after this: a duration back from now (30m, 24h, 7d, 2w), an
        /// RFC 3339 time or a UTC date (2026-10-22)
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,
        /// Maximum recent runs (default: every run in JSON, 10 in a human table); with a
        /// RUN_ID, the most steps listed (default 1000)
        #[arg(short, long)]
        limit: Option<usize>,
        /// Show every recorded run (no limit); with a RUN_ID, every step it recorded
        #[arg(long, conflicts_with = "limit")]
        all: bool,
        #[command(flatten)]
        format: FormatFlags,
        /// Output JSON with only these keys (comma-separated) on each entry of `runs`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::HISTORY_FIELDS))]
        #[arg(help = "Select JSON fields as a comma-separated list; implies --json", long_help = None)]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Evict cached results to keep .barca/ under its size limit
    ///
    /// The same eviction barca runs after every get and run once .barca/ is over
    /// `[cache] max_size` (default 10% of the filesystem), on demand.
    #[command(after_help = quick_help("gc", GC_HELP), after_long_help = GC_HELP, long_about = None)]
    Gc {
        /// A limit for this run in place of the configured one: 20GB, 500MB, 20GiB, bytes,
        /// 10% (of the filesystem) or off (only leftovers and superseded results)
        #[arg(long, value_name = "SIZE")]
        max_size: Option<String>,
        /// Print what would be evicted without deleting anything
        #[arg(long)]
        #[arg(help = "Preview cached and planned work; execute nothing and record no run", long_help = None)]
        dry_run: bool,
        #[command(flatten)]
        format: FormatFlags,
        /// Evict only this environment's results (leftovers are shared by all)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
    },
    /// Show execution statistics for an asset
    #[command(after_help = quick_help("stats", STATS_HELP), after_long_help = STATS_HELP, long_about = None)]
    Stats {
        /// Target asset function name
        target: String,
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`)
        #[arg(help = "Python files or directories; default: discover the project", long_help = None)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        format: FormatFlags,
        /// Output JSON with only these keys (comma-separated) on each entry of `recent_runs`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STATS_FIELDS))]
        #[arg(help = "Select JSON fields as a comma-separated list; implies --json", long_help = None)]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Manage runs, schedules and results on a server
    ///
    /// Binds to 127.0.0.1 by default (local only); --host changes the address.
    /// Authentication is opt-in with --auth token. POST /run and /get trigger async runs; poll
    /// GET /status/<run_id> for results.
    #[command(after_help = quick_help("serve", SERVE_HELP), after_long_help = SERVE_HELP, long_about = None)]
    Serve {
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`). With --watch, files added later are not picked up until restart
        #[arg(help = "Python files or directories; default: discover the project", long_help = None)]
        files: Vec<PathBuf>,
        /// Require project bearer tokens; overrides BARCA_SERVER_AUTH and [server] auth
        #[arg(long)]
        auth: Option<barca_core::config::AuthMode>,
        /// Port to bind on
        #[arg(short, long, default_value = "8274")]
        port: u16,
        /// IP address to bind on; 0.0.0.0 (or ::) listens on every interface. Use --auth token to require bearer credentials
        #[arg(long, default_value = "127.0.0.1")]
        #[arg(help = "Bind address; 0.0.0.0 exposes the unauthenticated API", long_help = None)]
        host: std::net::IpAddr,
        /// Dev mode: re-parse the DAG when source files change
        #[arg(long)]
        #[arg(help = "Reload changed source files; restart to discover new files", long_help = None)]
        watch: bool,
        /// Disable the cron scheduler (Schedule(...) assets will not auto-fire)
        #[arg(long)]
        #[arg(help = "Disable scheduled runs", long_help = None)]
        no_schedule: bool,
        /// Timezone for cron evaluation: local (this machine's zone) or utc, in any letter case, or an IANA name such as America/New_York, which is case-sensitive. Any other value is a usage error
        #[arg(long, default_value = "local", value_parser = parse_timezone)]
        #[arg(help = "Cron timezone: local, utc or an IANA name such as America/New_York", long_help = None)]
        timezone: String,
        /// Inspect only: refuse runs, never schedule, read compatible WAL metadata in place
        #[arg(long)]
        read_only: bool,
        /// Seconds to wait for runs in flight after SIGTERM or SIGINT before stopping them and
        /// putting them back in the run queue for the next start (exit 0; a second signal
        /// cancels them instead, exit 1). Default: [server] drain_timeout in barca.toml, else
        /// 30. 0 requeues them at once
        #[arg(long, value_name = "SECONDS")]
        #[arg(help = "Shutdown grace period in seconds; unfinished runs requeue (default: config or 30)", long_help = None)]
        drain_timeout: Option<u64>,
        /// How many runs execute at once; the others wait in the run queue, in order. At least
        /// 1. Default: [server] max_runs in barca.toml, else one per core. Experimental
        #[arg(long, value_name = "N", value_parser = parse_max_runs)]
        #[arg(help = "Maximum concurrent runs; others queue (default: config or CPU count)", long_help = None)]
        max_runs: Option<usize>,
        /// The most the weight= of the steps running at once may add up to across every run
        /// of this server, admitted first come, first served. At least 1. Default: [server]
        /// capacity in barca.toml, else none (each run has only its own). Experimental
        #[arg(long, value_name = "N", value_parser = parse_capacity)]
        #[arg(help = "Limit total running step weight across server runs (experimental)", long_help = None)]
        capacity: Option<usize>,
        /// Seconds a run may take before the server stops it (recorded as an error, steps that
        /// finished stay cached). 0: no limit. Default: [server] run_timeout_seconds in
        /// barca.toml, else no limit; Schedule(..., timeout_seconds=N) wins for its ticks
        #[arg(long, value_name = "SECONDS")]
        #[arg(help = "Run time limit in seconds; 0 disables it (default: config or unlimited)", long_help = None)]
        run_timeout: Option<u64>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
    },
    /// Show a barca server's run queue, or pause or resume it (experimental)
    ///
    /// The queue holds the runs a server accepted and has not finished, in order. It lives on
    /// the server: the command needs one (--server URL, BARCA_SERVER or [server] url).
    #[command(after_help = quick_help("queue", QUEUE_HELP), after_long_help = QUEUE_HELP, long_about = None)]
    Queue {
        /// pause: queued runs wait until resumed (runs in flight go on); resume: they start
        /// again. Omit to show the queue
        #[arg(value_enum)]
        action: Option<QueueAction>,
        #[command(flatten)]
        format: FormatFlags,
        /// The server's environment, checked against it (as for the other --server commands)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// List all discovered definitions (assets, tasks, sensors) with their deps
    ///
    /// Scheduled definitions also show their next fire time, computed in this
    /// machine's local time (a server started with --timezone fires in that zone).
    #[command(after_help = quick_help("list", LIST_HELP), after_long_help = LIST_HELP, long_about = None)]
    List {
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`)
        #[arg(help = "Python files or directories; default: discover the project", long_help = None)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        format: FormatFlags,
        /// Show the organizational hierarchy (groups are never executable nodes)
        #[arg(long, conflicts_with_all = ["limit", "all", "fields"])]
        groups: bool,
        /// Maximum nodes, in topological order (default: every node in JSON, 100 in a human table)
        #[arg(short, long)]
        limit: Option<usize>,
        /// Show every node (no limit)
        #[arg(long, conflicts_with = "limit")]
        all: bool,
        /// Output JSON with only these keys (comma-separated) on each entry of `nodes`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::LIST_FIELDS))]
        #[arg(help = "Select JSON fields as a comma-separated list; implies --json", long_help = None)]
        fields: Option<Vec<String>>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Show every node's cache state, last materialization and artifact shape (read-only)
    ///
    /// One aggregated view: what `barca list`, `--dry-run`, `barca history` and a look inside the
    /// artifact would each tell you. If the first positional arg ends in .py, all args are files;
    /// otherwise the first is a target (or several, comma-separated: `a,b`) and only the
    /// upstream cones of the targets are shown.
    #[command(after_help = quick_help("status", STATUS_HELP), after_long_help = STATUS_HELP, long_about = None)]
    Status {
        /// [TARGET[,TARGET...]] [file.py|dir/ ...] — both optional; no files reads the whole
        /// project (`barca docs discovery`)
        #[arg(help = "Target(s), then optional files/directories; omit files to discover the project", long_help = None)]
        args: Vec<String>,
        #[command(flatten)]
        format: FormatFlags,
        /// Maximum nodes, in topological order (default: every node in JSON, 100 in a human table)
        #[arg(short, long)]
        limit: Option<usize>,
        /// Show every node (no limit)
        #[arg(long, conflicts_with = "limit")]
        all: bool,
        /// Output JSON with only these keys (comma-separated) on each entry of `nodes`.
        /// Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STATUS_FIELDS))]
        #[arg(help = "Select JSON fields as a comma-separated list; implies --json", long_help = None)]
        fields: Option<Vec<String>>,
        /// Include up to N sample rows from each json/parquet/CSV artifact (off by default; pickles
        /// are never sampled)
        #[arg(long, value_name = "N")]
        sample: Option<usize>,
        /// Show only nodes in these cache states (comma-separated): cached, unsynced, stale,
        /// never_run, partial, unknown, always_runs. The summary still counts every node
        #[arg(long, value_name = "STATE", value_delimiter = ',')]
        state: Vec<String>,
        /// Show only nodes that need attention: not cached (unsynced, stale, never_run, partial,
        /// unknown), a task or sensor whose last run failed, or a paused or failing schedule
        #[arg(long)]
        problems: bool,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Query cached results with SQL (DuckDB) — each asset is a named view
    ///
    /// No pipeline step executes or new run is recorded. Partition expressions may evaluate
    /// Python; explicit SQL COPY can write files.
    #[command(after_help = quick_help("sql", SQL_HELP), after_long_help = SQL_HELP, long_about = None)]
    Sql {
        /// The SQL query (DuckDB dialect); quote it as one argument
        query: String,
        /// Python files or directories to read (default: every .py file under the project root
        /// that imports barca; see `barca docs discovery`)
        #[arg(help = "Python files or directories; default: discover the project", long_help = None)]
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
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Metadata backups: list them, put one back in place of the local database, or start over
    ///
    /// With a remote store, the metadata DB is backed up on the minute checkpoint and at the
    /// end of each run, five copies kept locally and in the store. Nothing is restored unless
    /// you ask.
    #[command(after_help = quick_help("state", STATE_HELP), after_long_help = STATE_HELP, long_about = None, subcommand_required = true)]
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
    /// Show the built-in manual: concepts, output formats, examples, agent conventions
    ///
    /// Topics are compiled into the binary, so this works offline and always matches the
    /// installed version. With no topic it prints an index.
    #[command(after_help = quick_help("docs", DOCS_HELP), after_long_help = DOCS_HELP, long_about = None)]
    Docs {
        /// Topic to show (omit for the index), e.g. types, cache, examples/duckdb
        topic: Option<String>,
        /// Print every topic in one stream
        #[arg(long, conflicts_with = "topic")]
        all: bool,
        /// Emit JSON instead of markdown
        #[arg(long)]
        json: bool,
        /// Print markdown, the default even when piped (accepted like every read command's
        /// --pretty)
        #[arg(long, conflicts_with_all = ["json", "fields"])]
        pretty: bool,
        /// Output JSON with only these keys (comma-separated) on each entry of `topics`, or on
        /// the one topic. Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::DOCS_FIELDS))]
        #[arg(help = "Select JSON fields as a comma-separated list; implies --json", long_help = None)]
        fields: Option<Vec<String>>,
    },
    /// Print version information
    Version,
}

/// `barca queue pause|resume`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum QueueAction {
    /// Hold every queued run back until resumed
    Pause,
    /// Let queued runs start again, in order
    Resume,
}

/// `barca state ...`.
#[derive(clap::Subcommand)]
pub(crate) enum StateCommand {
    /// List the metadata backups, local and in the store, newest first
    #[command(after_help = quick_help("state backups", STATE_BACKUPS_HELP), after_long_help = STATE_BACKUPS_HELP, long_about = None)]
    Backups {
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
    },
    /// Put a metadata backup in place of the local database (the current one is kept aside)
    #[command(after_help = quick_help("state restore", STATE_RESTORE_HELP), after_long_help = STATE_RESTORE_HELP, long_about = None)]
    Restore {
        /// Which backup: its index in `barca state backups` (1 = newest) or its name; default
        /// the newest. Or the path of a database file, such as one `barca state reset` moved
        /// aside
        #[arg(long, value_name = "N|NAME|PATH")]
        #[arg(help = "Backup index (1 = newest), name or database path; default: newest backup", long_help = None)]
        from: Option<String>,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
    },
    /// Start the metadata database over: move it aside and create an empty one (experimental)
    #[command(after_help = quick_help("state reset", STATE_RESET_HELP), after_long_help = STATE_RESET_HELP, long_about = None)]
    Reset {
        /// Do not ask: required when stdin is not a terminal
        #[arg(long, short = 'y')]
        yes: bool,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        #[arg(help = "Use a separate cache and history for this environment", long_help = None)]
        env: Option<String>,
    },
}

fn parse_max_runs(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "'{value}' is not a number of runs: give 1 or more (how many runs execute at once)"
        )),
    }
}

fn parse_capacity(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "'{value}' is not a capacity: give 1 or more (the most the weight= of the steps \
             running at once may add up to)"
        )),
    }
}

fn parse_timezone(value: &str) -> Result<String, String> {
    barca_core::schedule::Zone::parse(value).map(|_| value.to_string())
}
