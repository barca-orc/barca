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

/// `--server [<URL>]` and `--local`: where a command runs (RFC-0007 §3.1). On `get`, `run`,
/// `status`, `list`, `history` and `stats` only; the other commands always run here.
#[derive(clap::Args, Clone, Debug, Default)]
pub(crate) struct ServerFlags {
    /// Run this command on a barca server (`barca serve`) and print its result here: the URL
    /// given, or with no URL the configured one (BARCA_SERVER, else [server] url in
    /// barca.toml). Without it, get and run run on this machine (against a configured server's
    /// ledger) and status, history and stats are answered by a configured server
    #[arg(
        long,
        value_name = "URL",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "local"
    )]
    pub(crate) server: Option<String>,
    /// Ignore any configured server for this command: run against this machine's own ledger and
    /// records; nothing is read from or sent to the server
    #[arg(long)]
    pub(crate) local: bool,
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
  barca get total pipeline.py --json       # JSON even in a terminal (the default when piped)
  barca get total pipeline.py --pretty     # summary and artifact path for humans (the default in a terminal)
  barca get total pipeline.py -o path      # only the artifact path, one per line (pipe it to xargs cat)
  barca get total pipeline.py -o value     # only the value, pretty-printed (json, parquet, CSV, pickle)
  barca get total pipeline.py -o value --json   # the JSON result with the value in final_output
  barca get total pipeline.py -o url       # a signed HTTPS link to the result in the store (1 hour)
  barca get total pipeline.py -o url --expires 7d   # a link that works for 7 days (the maximum)
  barca get sales pipeline.py -o url --partition region=emea --expires 30m   # one key's link
  barca get sales pipeline.py              # a partitioned target: one path per key
  barca get sales pipeline.py --partition region=emea,amer   # only these keys: computed and returned
  barca get sales pipeline.py --partition region=emea --refresh sales   # recompute only that key
  barca get total pipeline.py --agent      # plain progress lines on stderr
  barca get total pipeline.py --env dev    # separate cache and state per environment
  barca list pipeline.py                   # not sure of the name? list assets and tasks first
  BARCA_OUTPUT=json barca get total pipeline.py   # env override for CI; a flag still wins
  barca get total pipeline.py --fields id,status   # JSON with each entry of `steps` trimmed to these keys
  barca get total --server                 # run it on the configured barca server; prints here as a local run
  barca get total --server https://barca.example.com --json   # on that server (barca docs server-mode)
  barca get total --local                  # this machine's ledger only; nothing is sent to a configured server

Output format: --json / --pretty, else BARCA_OUTPUT=json|pretty, else the terminal decides
(TTY -> pretty, piped -> JSON). get returns artifact paths: the pretty summary lists the path (a
small json value is previewed under it in a terminal), and in JSON final_output is the pointer
{\"_barca_artifact\": {\"path\", \"format\", \"size_bytes\"}} for every format. -o path prints only
the paths, one per line; -o value prints only the value. With --json, -o path|value choose what
final_output holds (-o path is the default). -o json|pretty are older spellings of --json|--pretty.
-o url (experimental) prints a signed HTTPS URL per result instead of its path: anyone with the
link can download the artifact from the store until it expires (--expires, default 1h, at most
7d; it cannot be revoked sooner). With --json each _barca_artifact pointer gains url and
url_expires_at. It needs a cloud store (s3://, gs://, abfs://; a local store is exit 2). A result
whose upload is pending (`local only`) is uploaded first; one that still is not in the store, or
credentials that cannot sign, is exit 3 (barca docs remote).
A partitioned target returns every key, sorted: final_output is an object keyed by partition key
(\"region=emea\") and -o path prints one path per key. --partition region=emea,amer (or a JSON
object for compound keys) selects keys of the target: only they run, are returned and are
refreshed by --refresh. When some keys fail the run fails (exit 1) and final_output in the
failed result line holds the keys that succeeded.
JSON is one line on stdout with status (\"success\"), run_id,
steps_executed (0 = all cached), phases, final_output, and `steps`: what happened to each step
(ran or cached, and why; `env` holds the values of variables the node declares with env=[...],
secrets redacted). `warnings` is always an array: plan-time warnings for the steps this command
planned, [] when there are none; each is {kind, node, param, message} and is also one
`[barca] warning: ...` line on stderr. The only kind is unused_input: a step declares an input its
function never uses (barca docs assets, \"Unused inputs\"). Warnings never change the exit code.
--dry-run reports the same list. The Python API (barca.get) loads the value for you.
Several targets (`a,b`, comma-separated, no spaces): final_output is replaced by `targets`, keyed by
target, each {status: success, final_output} or {status: failed, failed_node, error}. Every target
runs even if another fails; exit 1 if any failed. -o path prints each target's paths in order.
Refresh: the same vocabulary as `barca run`. --refresh takes ONE comma-separated list of assets in
the cone (the target itself may be named) and also re-runs everything downstream of them;
--no-cascade re-runs only the named ones. --refresh-all re-runs every asset in the cone.
A cached step whose artifact file is gone is computed again when something needs to read it (a
step that runs takes it as an input, or it is a target), with reason `artifact_missing` and a
warning on stderr (reason `evicted` when the cache size limit deleted it). A missing artifact that
nothing reads is left alone (barca docs cache).
After the run, if .barca/ is over [cache] max_size (default 10% of the filesystem;
BARCA_CACHE_MAX_SIZE), barca evicts what is least needed down to 80% of it, never this command's
own results; stderr says what, and JSON gains `cache` (barca gc, barca docs cache \"Disk use\").
Completed steps are recorded during the run; remote results require confirmed upload.
With an artifact store, an upload that fails after every step succeeded is a warning, not a
failure: exit 0, JSON `uploads` {pending, error, nodes}, the result is kept on this machine
(`unsynced` in barca status) and the next get or run uploads it first (barca docs remote).
Use `barca status pipeline.py` in another terminal to inspect recorded progress. With a store
([remote] uri), the metadata DB is backed up there every minute and when the run ends; a failed
backup upload is a warning (barca docs remote). A cached target that is missing from the store, or
different there, is uploaded from this machine without a step running.
Targets must be assets; use `barca run` for tasks. With no target, get materializes every asset and
sensor and skips tasks (previously it ran tasks too); stderr names the skipped tasks and the
`barca run` command. A file with only tasks gets nothing: exit 0, empty `steps`.
The target comes before the files: `barca get pipeline.py total` exits 2 and prints
`barca get total pipeline.py`.
Errors: in JSON mode stderr contains one JSON error object {error, code, kind, remediation},
plus node, traceback and artifact_dir when a step failed. Exit 1 step failed, 2 usage error,
3 barca/infra failure, 130 cancelled. A failed step still prints a stdout result line with
status \"failed\" and failed_node. Stderr ends with the failed step, its first error line,
and the count of skipped downstream nodes.
A required history-write failure exits 3 and preserves previously committed history.
More: barca docs cache, barca docs partitions, barca docs types, barca docs agents";

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
  barca run deploy --server --agent                    # run it on the configured barca server (barca docs server-mode)
  barca run deploy pipeline.py --local                 # run here and post nothing to a configured server
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
upstream asset is an error.
An upstream asset that is cached but whose artifact file is gone is computed again before the task
reads it (reason `artifact_missing`, a warning on stderr; `evicted` when the cache size limit
deleted it), so a deleted artifact does not fail the run (barca docs cache). Afterwards .barca/ is
kept under [cache] max_size, as after `barca get` (barca gc).
The target must be a task; use `barca get` for assets. The target comes before the files:
`barca run pipeline.py deploy` exits 2 and prints `barca run deploy pipeline.py`. Every usage
error exits 2 and ends by pointing at `barca list` (with the files you gave, if any).
No files: barca reads every file in the project that imports barca (barca docs discovery).
Errors: in JSON mode stderr contains one JSON error object {error, code, kind, remediation}
(see barca docs agents). Exit 1 step failed, 2 usage error, 3 barca/infra failure, 130 cancelled.
A raising task, or one that calls sys.exit(), fails the run: exit 1, and the stdout JSON line has
status \"failed\" and failed_node. Stderr ends with the failed step, its first error line,
and the count of skipped downstream nodes.
More: barca docs tasks, barca docs cache, barca docs agents";

const PLAN_HELP: &str = "\
Examples:
  barca plan                          # the whole project
  barca plan pipeline.py              # phases and steps that would run; nothing executes
  barca plan pipeline.py other.py     # several files form one DAG

Output: always pretty-printed JSON {plan_version, total_steps, phases: [{reason, streams: [{stream_id, steps}]}],
warnings}. `warnings` is always an array ([] when there are none) of {kind, node, param, message}:
plan-time warnings such as an input a step never uses, each also one `[barca] warning: ...` line on
stderr (barca docs assets, \"Unused inputs\").
`plan_version` is 1: the plan document format, independent of the package and database versions.
`reason` is an object: {\"type\": \"initial\"} or {\"type\": \"fan_in\", \"node_id\": ...}.
Planning uses the execution pool size (available cores, or BARCA_POOL_SIZE).
Parsing is static; partitions(<expression>) evaluates Python while loading its keys.
Planning reads no state and takes no --env.
Experimental: the layout may change between releases (barca docs contract).
More: barca docs agents";

const DOCTOR_HELP: &str = "\
Examples:
  barca doctor                      # diagnose the discovered project
  barca doctor pipeline.py --json    # structured checks, no steps executed
  barca doctor src/ --env local --pretty   # selected sources and environment

Checks project root, Python, config, backups, cache writability, metadata integrity,
static source parsing, and configured stores. Never imports pipeline modules or evaluates
dynamic partition expressions. Prints the report on stdout; exit 0 when all checks pass,
3 when a prerequisite fails. Store probes list but never create buckets or objects.
More: barca docs doctor";

const LOGS_HELP: &str = "\
Examples:
  barca logs 7f30b3e9 --pretty          # captured stdout lines, without progress messages
  barca logs 7f30b3e9 --step report --pretty   # one function name or exact node id
  barca logs 7f30b3e9 --json            # [{node_id, line}] in recorded order
  barca logs 7f30b3e9 --env dev         # read another environment's local history

Replace the example id with run_id from barca run/get --json or barca history.
Reads the local recorded stdout lines; does not execute or import pipeline code. Unknown runs
and ambiguous step names are usage errors. Missing/unknown steps are usage errors; a recorded
step that printed nothing returns no lines. Prints emitted before a failed step are retained.
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
  barca history --env dev      # runs recorded in another environment
  barca history --server -l 20 # the runs of the configured barca server (barca docs server-mode)
  barca history --local        # this machine's runs, even with a server configured

--reconcile is an explicit write: first verify the run stopped. It refuses a proven live
owner and terminal rows. It prints only that run, preserving progress and cached results;
no end time or duration is invented. Ordinary history inspection never reconciles an
unknown owner automatically.
Newest first. When more runs exist than are shown, JSON says `\"truncated\": true` with the
`total`, and the table prints a one-line note on stderr. In JSON, `files` is an array: the .py
files the run was given.
JSON returns every item unless --limit is given; human tables keep their default bound.
More: barca docs agents, barca docs cache";

const GC_HELP: &str = "\
Examples:
  barca gc --dry-run              # what would be evicted, per tier, and what it leaves
  barca gc                        # evict now, down to 80% of [cache] max_size
  barca gc --max-size 5GB         # a one-off tighter budget
  barca gc --max-size off         # only leftovers and superseded results, all of them
  barca gc --json                 # {size_bytes, limit_bytes, after_bytes, tiers, evicted, ...}
  barca gc --env dev              # only the dev environment's results (leftovers are shared)

Order: leftovers (dead processes' staging, *.moved-aside* directories, sql-cache copies), then
superseded results, then local copies the artifact store holds, then the least recently read
results. Never: the metadata DBs, a storeless Manual node's current result, or (while another
run is in progress on this .barca/) anything but leftovers and superseded results. An evicted
result is fetched or computed again when something needs it. The store is never touched.
Exit 3 only when a metadata DB cannot be read; a file that cannot be deleted is skipped.
More: barca docs cache (\"Disk use\")";

const STATS_HELP: &str = "\
Examples:
  barca stats total                         # find `total` anywhere in the project
  barca stats total pipeline.py             # timing percentiles and re-executions
  barca stats total pipeline.py --json      # {id, total_runs, cache_hit_rate, ..., recent_runs}, even in a terminal
  barca stats total pipeline.py --pretty    # the text report, even when piped
  barca stats total pipeline.py --fields status,error_message   # JSON; trims recent_runs entries
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
  barca serve pipeline.py --env prod         # .barca/envs/prod/metadata.db, named on the startup line

Binds to 127.0.0.1 by default. There is no authentication: with --host 0.0.0.0, anyone who
can reach the port can trigger runs, so keep it on a private network or behind a proxy.
Invalid sources and their graph dependents are excluded; healthy definitions and schedules
continue. Startup logs, /health load_errors and the UI show unloaded files and affected nodes.
--watch restores repaired configured sources; one-shot commands remain strict.
From another machine, `--server <url>` on get, run, status, list, history and stats runs the
command on this server and prints it there as a local run would (barca docs server-mode).
`serve` itself always runs here: --server on it is a usage error.
The first line names the env, the artifact store and the metadata DB with its format:
[barca] env <env> (<source>); artifact store <store>; metadata <path> (db format <n>).
A writer sets an existing DB up before it binds: one it cannot use (empty, not SQLite,
a newer format, a malformed schema) or cannot open exits 3 naming the file and what to do.
A --read-only server keeps serving and reports it in /health metadata_error, and its routes
answer with an error whose reason is metadata_unusable. /health scheduler_enabled says
whether the scheduler is running.
With a store ([remote] uri), the metadata DB is backed up there every minute it changed
(barca docs remote); barca state restore refuses to run while the server is up.
SIGTERM or SIGINT drains: new runs get 503 with Retry-After, the scheduler stops, /health
says draining, and runs in flight finish (uploads included) for up to --drain-timeout seconds
(default 30, [server] drain_timeout in barca.toml). Exit 0 when they all finished. The deadline
or a second signal cancels the rest with reason \"server shutdown\": exit 1.
Lines go to stderr as text; BARCA_LOG=json writes them to stdout as JSON (barca docs telemetry).
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
JSON returns every item unless --limit is given; human tables keep their default bound.
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
  barca status big.py --limit 20           # first 20 nodes; the summary counts all
  barca status total pipeline.py --json --sample 5   # add up to 5 sample rows per json/parquet/CSV artifact
  barca status pipeline.py --env dev       # state recorded in another environment
  barca status total --server              # cache state on the configured barca server
  barca status total --json                # with a server configured: its ledger, plus `local` per node
  barca status total --local               # this machine's own state, even with a server configured

Cache state per node: cached, unsynced (cached here, its upload to the artifact store still
pending: reason `local_only`), stale (ran before; code or inputs changed, or its artifact file is
gone and a run would read it: reason `artifact_missing`, or `evicted` when the cache size limit
deleted it), never_run, partial
(some partition keys cached), unknown (dynamic partitions not yet known) or always_runs (tasks,
sensors), with a reason. JSON spells the states in snake_case, the same as the `summary` keys
(the table prints never-run, always-runs). It is the same decision `--dry-run` makes.
With a server configured (BARCA_SERVER, [server] url) and no --local, the server answers, each
node gets `local` (a copy is here), and a result computed here whose ledger post is pending is
unsynced: reason `not_in_ledger` (barca docs server-mode).
No steps execute; nothing is written. partitions(<expression>) may import the source to resolve
keys. With a store, a cached node is `unsynced` unless this machine's copy, the store's object and
the recorded digest agree: reason `local_only`, `store_only` or `mismatch` (barca docs remote). Shape (rows, columns, type) is read from the
artifact file only. With remote storage it is read from the bucket: a parquet footer by ranged
requests, json and pickle by a download of up to 16 MB (larger ones get a `note`). A store that
cannot be reached is a `note` on each shape, not a failed command.
JSON returns every item unless --limit is given; human tables keep their default bound.
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
Only parquet, CSV and json results can be queried; pickles cannot. Runs in an in-memory DuckDB over
artifact files; no pipeline step runs and no new run is recorded. Install SQL support: uv add 'barca[sql]'.
partitions(<expression>) may evaluate Python while loading its keys.
Partitioned views include current known keys only; removed keys stay in history. Unknown derived
keys require their source to materialize first. Zero current keys have no result view.
With remote storage, the artifacts of the views a query names are downloaded into
.barca/sql-cache/ (stderr says so) and reused while the objects are unchanged. Explicit SQL
COPY ... TO writes the requested output file.
Errors exit 2: a node with no result yet names the `barca get` to run first; an unknown view lists
the views; a SQL error carries DuckDB's message. A remote artifact that cannot be fetched exits 3.
Experimental: barca docs contract.
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

With a remote store ([remote] uri), the metadata DB is backed up every minute of a run, when the
run ends, and every minute from barca serve: to .barca/backups/ and to
{uri}/{env}/state/backups/, five copies kept in each. Nothing is ever downloaded before a run.
restore never runs by itself: it validates the copy, moves the current database to
metadata.db.pre-restore and puts the copy in its place. No backup: exit 2. Refused while
barca serve is using the environment: exit 3. reset moves the database aside to
metadata.db.reset-<UTC> and starts empty; nothing is deleted.
More: barca docs remote";

const STATE_BACKUPS_HELP: &str = "\
Examples:
  barca state backups                  # table in a terminal, JSON when piped
  barca state backups --json           # {backups, local_dir, store, store_error?}
  barca state backups --pretty         # the table, even when piped
  barca state backups --env dev        # another environment's backups

Newest first. `where` is local, store or both; `agree` says whether the two copies of a backup
match (null with one copy). `host` and `project` name the machine and project directory that
took it, `mine` whether it was this one (null for a backup from barca 0.28 or earlier). A store
that cannot be listed is a warning on stderr and `store_error` in JSON; the local copies are
still listed. A backup another machine took after this one's newest is a warning on stderr:
two machines on one store are not supported (use a server).
More: barca docs remote";

const STATE_RESTORE_HELP: &str = "\
Examples:
  barca state restore                  # this machine's newest backup (the newest if it has none)
  barca state restore --from 1         # the newest, whichever machine took it
  barca state restore --from 2         # the second newest
  barca state restore --from 2026-10-14T18-02-11.482Z-3f9a1c2e  # by name, .db optional
  barca state restore --json           # {restored, from, source, pre_restore, rows, run_id, host, project}
  barca state restore --from .barca/metadata.db.reset-2026-10-20T18-02-11.482Z  # a file barca state reset moved aside

The copy is checked first (a whole SQLite file, integrity, barca's tables, a format this barca
can use): the local copy when there is one, else the store's. A copy that fails is a usage error
naming the next newest, and nothing changes. Then the current database moves to
metadata.db.pre-restore (replacing an older one) and the copy takes its place; one line on
stderr says what was restored and from where. Stop barca get/run in this project first.
Exit 2: no backup, unknown --from, a copy that is not valid. Exit 3: barca serve is running.
--from also takes the path of a database file (the one barca state reset moved aside), with its
-wal beside it when there is one.
More: barca docs remote";

const STATE_RESET_HELP: &str = "\
Examples:
  barca state reset                    # asks first in a terminal
  barca state reset --yes              # no question (scripts, agents)
  barca state reset --yes --json       # {database, moved_to, moved, undo}
  barca state reset --yes --env dev    # another environment's database

Moves .barca/metadata.db and its -wal and -shm aside to metadata.db.reset-<UTC> in the same
directory and creates an empty database in its place: run history, cache records, partition
keys and schedule pauses start over. Nothing is deleted and artifacts are left alone (the cache
limit evicts what nothing references). Undo: barca state restore --from <the moved file>.
For a database that cannot be opened (corrupt, or a format this barca does not read) too.
Without --yes and not in a terminal: exit 2. Exit 3: a run is in progress, or barca serve is
using the environment (reset from its UI, or POST /state/reset).
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

Topics are compiled into the binary: offline, and always matching this version.";

#[derive(Parser)]
#[command(
    name = "barca",
    about = "Invisible asset orchestrator. Discover a project's assets and tasks with `barca list <file.py>`",
    long_about = "Barca runs Python asset graphs with caching by run hash.\n\
                  Every asset output is fully materialized to an artifact file at step \
                  boundaries (json, pickle, parquet, or CSV) — that persistence is the cache \
                  checkpoint. pandas/polars DataFrames, pyarrow Tables and duckdb relations \
                  are written as parquet; parameter type annotations choose how downstream \
                  steps read it back (pandas by default, or polars, pyarrow, duckdb) but do \
                  not skip materialization. Start with `barca list` (run inside the project) to discover a \
                  project's assets and tasks; run `barca docs` for the manual.",
    after_help = TOP_HELP,
    version
)]
pub(crate) enum Cli {
    /// Get assets — cache-aware, runs only the needed subgraph; prints their artifact paths
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
        /// What to print: artifact paths (the default), values, or signed URLs. Alone,
        /// -o path|value|url prints only that; with --json, it chooses what `final_output` holds
        #[arg(short, long)]
        output: Option<GetOutput>,
        /// With -o url: how long the links work, as 30m, 12h, 7d or seconds (default 1h, at
        /// most 7d). Experimental
        #[arg(long, value_name = "DURATION")]
        expires: Option<String>,
        #[command(flatten)]
        format: FormatFlags,
        /// Select keys of a partitioned target: `region=emea,amer`, or a JSON object for
        /// compound keys (`{"region": "emea", "day": "2026-10-14"}`). Only those keys are
        /// computed and returned, and --refresh of the target refreshes only them
        #[arg(long, value_name = "KEY=VALUE[,VALUE]|JSON")]
        partition: Option<String>,
        /// Assets to force re-materialize, as ONE comma-separated list (`--refresh a,b`, not
        /// `--refresh a b`); the target itself may be named. Every asset downstream of them in
        /// the target's cone re-materializes too (see --no-cascade)
        #[arg(long, value_delimiter = ',', conflicts_with = "refresh_all")]
        refresh: Option<Vec<String>>,
        /// With --refresh: re-materialize only the named assets, not what is downstream of
        /// them. Cached downstream assets then do not reflect the refresh; barca warns
        #[arg(long, requires = "refresh")]
        no_cascade: bool,
        /// Force re-materialize EVERY asset in the target's cone (nothing comes from cache)
        #[arg(long)]
        refresh_all: bool,
        /// Show what this command would do (each step cached or will-run, and why) without
        /// executing steps or recording a run
        #[arg(long)]
        dry_run: bool,
        /// Agent-friendly output: plain structured progress lines instead of visual progress bar
        #[arg(long)]
        agent: bool,
        /// Keep only these keys (comma-separated) on each entry of `steps` in the JSON output.
        /// Implies --json; not valid with --pretty or -o pretty. An unknown key is a usage error
        /// listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::STEP_FIELDS))]
        fields: Option<Vec<String>>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
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
        #[arg(long, value_delimiter = ',', conflicts_with = "refresh_all")]
        refresh: Option<Vec<String>>,
        /// With --refresh: re-materialize only the named assets, not what is downstream of
        /// them. Cached downstream assets then do not reflect the refresh; barca warns
        #[arg(long, requires = "refresh")]
        no_cascade: bool,
        /// Force re-materialize EVERY asset in the task's cone (nothing comes from cache)
        #[arg(long)]
        refresh_all: bool,
        /// Show what this command would do (each step cached or will-run, and why) without
        /// executing steps or recording a run
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
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Parse source files and emit the execution plan as JSON
    #[command(after_help = PLAN_HELP)]
    Plan {
        /// Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`)
        files: Vec<PathBuf>,
    },
    /// Diagnose project prerequisites without running pipeline steps
    #[command(after_help = DOCTOR_HELP)]
    Doctor {
        /// Python files or directories to inspect (default: discovered project sources)
        files: Vec<PathBuf>,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Read a run's captured step stdout, without progress messages
    #[command(after_help = LOGS_HELP)]
    Logs {
        /// Exact run id from get/run JSON or history
        run_id: String,
        /// Function name or exact node id (including a partition key when desired)
        #[arg(long)]
        step: Option<String>,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment whose local history to read
        #[arg(long)]
        env: Option<String>,
    },
    /// Show recent run history
    #[command(after_help = HISTORY_HELP)]
    History {
        /// After verifying a run stopped, record it as interrupted; refuses a proven live owner
        #[arg(long, value_name = "RUN_ID", conflicts_with_all = ["limit", "all"])]
        reconcile: Option<String>,
        /// Maximum recent runs (default: every run in JSON, 10 in a human table)
        #[arg(short, long)]
        limit: Option<usize>,
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
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Evict cached results to keep .barca/ under its size limit
    ///
    /// The same eviction barca runs after every get and run once .barca/ is over
    /// `[cache] max_size` (default 10% of the filesystem), on demand.
    #[command(after_help = GC_HELP)]
    Gc {
        /// A limit for this run in place of the configured one: 20GB, 500MB, 20GiB, bytes,
        /// 10% (of the filesystem) or off (only leftovers and superseded results)
        #[arg(long, value_name = "SIZE")]
        max_size: Option<String>,
        /// Print what would be evicted without deleting anything
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        format: FormatFlags,
        /// Evict only this environment's results (leftovers are shared by all)
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
        #[command(flatten)]
        server: ServerFlags,
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
        /// Seconds to wait for runs in flight after SIGTERM or SIGINT before cancelling them
        /// (exit 1). Default: [server] drain_timeout in barca.toml, else 30. 0 cancels at once
        #[arg(long, value_name = "SECONDS")]
        drain_timeout: Option<u64>,
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
    #[command(after_help = STATUS_HELP)]
    Status {
        /// [TARGET[,TARGET...]] [file.py|dir/ ...] — both optional; no files reads the whole
        /// project (`barca docs discovery`)
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
        fields: Option<Vec<String>>,
        /// Include up to N sample rows from each json/parquet/CSV artifact (off by default; pickles
        /// are never sampled)
        #[arg(long, value_name = "N")]
        sample: Option<usize>,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Query cached results with SQL (DuckDB) — each asset is a named view
    ///
    /// No pipeline step executes or new run is recorded. Partition expressions may evaluate
    /// Python; explicit SQL COPY can write files.
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
    /// Metadata backups: list them, put one back in place of the local database, or start over
    ///
    /// With a remote store, the metadata DB is backed up on the minute checkpoint and at the
    /// end of each run, five copies kept locally and in the store. Nothing is restored unless
    /// you ask.
    #[command(after_help = STATE_HELP, subcommand_required = true)]
    State {
        #[command(subcommand)]
        command: StateCommand,
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
        /// Print markdown, the default even when piped (accepted like every read command's
        /// --pretty)
        #[arg(long, conflicts_with_all = ["json", "fields"])]
        pretty: bool,
        /// Output JSON with only these keys (comma-separated) on each entry of `topics`, or on
        /// the one topic. Implies --json. An unknown key is a usage error listing the valid ones
        #[arg(long, value_delimiter = ',', value_parser = PossibleValuesParser::new(bounded::DOCS_FIELDS))]
        fields: Option<Vec<String>>,
    },
    /// Print version information
    Version,
}

/// `barca state ...`.
#[derive(clap::Subcommand)]
pub(crate) enum StateCommand {
    /// List the metadata backups, local and in the store, newest first
    #[command(after_help = STATE_BACKUPS_HELP)]
    Backups {
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Put a metadata backup in place of the local database (the current one is kept aside)
    #[command(after_help = STATE_RESTORE_HELP)]
    Restore {
        /// Which backup: its index in `barca state backups` (1 = newest) or its name; default
        /// the newest. Or the path of a database file, such as one `barca state reset` moved
        /// aside
        #[arg(long, value_name = "N|NAME|PATH")]
        from: Option<String>,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
    /// Start the metadata database over: move it aside and create an empty one (experimental)
    #[command(after_help = STATE_RESET_HELP)]
    Reset {
        /// Do not ask: required when stdin is not a terminal
        #[arg(long, short = 'y')]
        yes: bool,
        #[command(flatten)]
        format: FormatFlags,
        /// Environment name (separates cache/state per environment)
        #[arg(long)]
        env: Option<String>,
    },
}

fn parse_timezone(value: &str) -> Result<String, String> {
    barca_core::schedule::Zone::parse(value).map(|_| value.to_string())
}
