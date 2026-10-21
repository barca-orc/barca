# CLI contract

This page is the written-down CLI surface of this version of barca: every command, argument,
environment variable, exit code, JSON output schema, the stderr error envelope and the
`--agent` line formats. Scripts and AI agents can rely on everything marked `stable`.

Barca is pre-1.0, so the surface is **not frozen yet**. What this page guarantees is that no
part of it changes by accident: tests compare it with the real CLI, and a change to the surface
fails CI until this page and the snapshots are updated in the same pull request (see "Changing
the surface" below).

Most of this page is generated. The command and argument tables and the exit codes come from
the CLI definition (`cargo test -p barca`); every JSON schema table and the `--agent` lines come
from running the real binary on a fixture pipeline (`python/tests/test_cli_contract.py`). Text
outside the `GENERATED` blocks is written by hand.

## Stability

- **stable**: kept as documented. Pre-1.0 a breaking change to it needs a minor version bump and
  a "Breaking" line in the release notes. From 1.0 it only changes additively.
- **experimental**: may change in any release, with a note in the release notes. Each one says
  why it is not settled.

### Policy

- **Additive changes are always allowed**: a new command, a new flag, a new JSON key, a new
  value of an enum-like string (a new step `reason`, a new error `kind`), a new stderr line. Parse
  JSON by key and ignore keys you do not know; treat an unknown enum value as "other".
- **Breaking changes**: removing or renaming a command, flag, JSON key or environment variable;
  changing a key's type; changing what an exit code means; changing a stable `--agent` line;
  changing a default (for example what stdout prints when piped).
- **Before 1.0**: a breaking change is allowed. It ships in a minor release (`0.x.0`) whose
  release notes have a "Breaking" line naming it and the replacement, and the same pull request
  updates the snapshots and this page.
- **From 1.0**: changes are additive only. A flag that is replaced is deprecated, not removed: it
  keeps working for at least one minor release and prints a warning on stderr naming the
  replacement.
- **Not part of the contract**: human output (tables, `--pretty` summaries, the progress bar),
  the wording of error messages and remediations, the order of keys in a JSON object, whether
  JSON is printed on one line or indented, and the text of `barca docs` topics.

### Experimental items

| Item | Why |
|---|---|
| `barca doctor` and its JSON | prerequisite check names and provider-specific identity data may evolve; failed checks exit 3 with the report on stdout and the standard error envelope on stderr |
| `barca plan` and its JSON | prints the planner's internal phase/stream layout, which changes with scheduling work (`reason` is an object, `{"type": "initial"}` or `{"type": "fan_in", "node_id": ...}`) |
| `barca serve` and all its flags | the HTTP API and scheduler are young: no auth, routes may change. Its run outcomes (`GET /runs/{run_id}`, `run_finished`) and inspection routes (`/list`, `/nodes`, `/history`, `/stats/{name}`, `/assets`) are the CLI's `--json` bodies, built by the same functions, and its errors are this envelope. The older trigger routes (`POST /run`, `/run/{target}`, `/get/{target}`, `GET /status/{run_id}`, `/events/{run_id}`) are aliases until the next minor release |
| `get -o json\|pretty`, `run -o/--output` | kept for compatibility; `--json` / `--pretty` are the canonical spelling. `get -o path\|value` is stable |
| `get -o url`, `get --expires`, and `url` / `url_expires_at` in a `_barca_artifact` pointer | new (RFC-0011): signed HTTPS links to results in a cloud store. The URL's form is the cloud's own (SigV4, GCS V4, Azure SAS); `url_expires_at` is RFC 3339 UTC. `get -o url` with no cloud store, or an `--expires` over `7d`, is exit 2; a result missing from the store or credentials that cannot sign is exit 3 |
| `--server [<URL>]` and `--local` on `get`, `run`, `status`, `list`, `history`, `stats`; `BARCA_SERVER` and `[server] url` | server mode is new (RFC-0007). A `--server` command prints what the local command prints, with the differences `barca docs server-mode` lists (the server's paths, `triggered_by: api`, lines the Python workers print themselves stay on the server). Without `--server`, a configured server is the ledger of a local `get`/`run` (`POST /ledger/lookup`, `POST /ledger/runs`) and answers `status` (with `nodes[].local` and the `unsynced`/`not_in_ledger` state), `history` and `stats`; `--local` ignores it. How the server is chosen and the client's own messages may change |
| `status --sample` and `nodes[].shape` | read by a Python helper (`barca._inspect`) whose output may grow per format; the wording of `shape.note` (an unreadable, too large or unreachable artifact) may change |
| `BARCA_PROGRESS_SECS`, `BARCA_POOL_SIZE`, `BARCA_COMM_COST_SECONDS`, `BARCA_TRACE_TIMING` | tuning and benchmarking knobs |
| `BARCA_TELEMETRY` and the spans and metrics it sends | new: span names, resources, tags and metrics may change |
| `BARCA_SLACK_WEBHOOK_URL`, `BARCA_SLACK_NOTIFY`, `BARCA_UI_URL` and the Slack posts | new: the message text and layout are not a contract and may change in any release |
| `BARCA_LOG` and the JSON lines of `barca serve` | new: the fields and how a line's `level` is chosen may change |
| the `[barca] <n> more: ...` line | collapsing repeated library warnings is new; which output counts as a warning may change |
| `BARCA_ARTIFACT_URI` | 0.4.0 back-compat override, superseded by `BARCA_REMOTE_URI` / `[remote].artifacts_uri` |
| `barca gc` and its JSON, `[cache] max_size`, `BARCA_CACHE_MAX_SIZE`, the `cache` key of `get`/`run` results, the step and status reason `evicted`, the `cache_size` doctor check | new in 0.25 (RFC-0012): the low watermark, the eviction order and the JSON shapes may change after field use |
| `--agent` lines other than `step:`, the end-of-run line and `run failed:` | progress notes (`still running`, skipped tasks, the text of warnings, `SINK FAILED`) whose wording may change |

### Accepted exceptions

- `barca docs` prints markdown by default, even when piped, and has `--json` but no `--pretty`:
  its output is a manual meant to be read or pasted into a model's context, so the terminal rule
  does not apply. `--json` and `--fields` give the JSON index or topic.

## Commands

`barca <command> --help` (or `-h`) prints each command's flags and runnable examples. Shorthand:
`barca file.py [flags]` is `barca get file.py [flags]` (stable).

<!-- BEGIN GENERATED commands -->
| Command | Arguments | Stability | Purpose |
|---|---|---|---|
| `barca get` | `[<ARGS>...]` | stable | Get assets — cache-aware, runs only the needed subgraph; prints their artifact paths |
| `barca run` | `<ARGS>...` | stable | Run a task and its dependency cone — the task always re-runs |
| `barca plan` | `[<FILES>...]` | experimental: prints the planner's internal phase/stream layout, which changes with scheduling work | Parse source files and emit the execution plan as JSON |
| `barca doctor` | `[<FILES>...]` | experimental: new prerequisite report; check names and provider-specific data may evolve | Diagnose project prerequisites without running pipeline steps |
| `barca logs` | `<RUN_ID>` | stable | Read a run's captured step stdout, without progress messages |
| `barca history` | - | stable | Show recent run history |
| `barca gc` | - | experimental: new in 0.25 (RFC-0012): the eviction order, the low watermark and the JSON shape may change after field use | Evict cached results to keep .barca/ under its size limit |
| `barca stats` | `<TARGET> [<FILES>...]` | stable | Show execution statistics for an asset |
| `barca serve` | `[<FILES>...]` | experimental: the HTTP API and scheduler are young: no auth, routes may change; the older trigger routes are aliases until the next minor | Run a long-running HTTP server exposing the orchestrator as a JSON API |
| `barca list` | `[<FILES>...]` | stable | List all discovered definitions (assets, tasks, sensors) with their deps |
| `barca status` | `[<ARGS>...]` | stable | Show every node's cache state, last materialization and artifact shape (read-only) |
| `barca sql` | `<QUERY> [<FILES>...]` | experimental: new in 0.13: the view naming and the JSON result shape may change after field use | Query cached results with SQL (DuckDB) — each asset is a named view |
| `barca state` | - | stable | Metadata backups: list them, or put one back in place of the local database |
| `barca state backups` | - | stable | List the metadata backups, local and in the store, newest first |
| `barca state restore` | - | stable | Put a metadata backup in place of the local database (the current one is kept aside) |
| `barca docs` | `[<TOPIC>]` | stable | Show the built-in manual: concepts, output formats, examples, agent conventions |
| `barca version` | - | stable | Print version information |
| `barca help` | - | stable | Print this message or the help of the given subcommand(s) |
<!-- END GENERATED commands -->

Positional rules (stable), for `get`, `run` and `status`:

- The target comes before the files. If the first positional ends in `.py`, every positional is
  a file and there is no target (`run` then exits 2: it needs one). Otherwise the first
  positional is the target and the rest are files.
- A target is one name or several, comma-separated without spaces (`a,b`), on all three. A name
  is the function name or the full id `file.py:name`, and selects exactly that node (`deploy`
  never selects `prod_deploy`). A function name defined in more than one file, or an empty name
  (`a,,b`), is a usage error.
- An unknown target is a usage error (exit 2) on every command, with one remediation:
  ``Run `barca list <files>` to see available assets and tasks.`` So is a name that several
  nodes have, and `barca run` without a target. No other usage error carries that remediation:
  each states its own fix (the corrected command for positionals in the wrong order, the valid
  names for an unknown `--refresh` name, what to set for `-o url` without a cloud store, what to
  remove for a removed setting) or, with nothing more to say, `See barca <command> --help`.
- barca never offers fuzzy suggestions on the command line: no "did you mean" for targets or
  `barca docs` topics (an unknown topic lists every valid topic), and argument errors carry no
  "a similar argument exists" tip. A guess can read as confirmation. The one place a guess is
  made is a misspelt decorator argument in a pipeline file (`input=` for `inputs=`): the error
  names the accepted argument when exactly one is within one edit of what was written (two for
  a name longer than four characters), always next to the full list of accepted arguments
  (`barca docs assets`, "Accepted arguments").
- `--refresh` and `--fields` take one comma-separated list (`--refresh a,b`).

## Arguments

Every command also takes `-h, --help`. `Notes` lists whether an argument is required, its
default, and any aliases.

<!-- BEGIN GENERATED flags -->
#### barca

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `-V, --version` | - | - | stable | Print version |

#### barca get

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ARGS>...` | - | - | stable | [TARGET[,TARGET...]] [file.py\|dir/ ...] — both optional: no target gets every asset and sensor; no files reads the whole project (`barca docs discovery`) |
| `-o, --output` | `path\|value\|url\|json\|pretty` | - | stable | What to print: artifact paths (the default), values, or signed URLs. Alone, -o path\|value\|url prints only that; with --json, it chooses what `final_output` holds |
| `--expires` | `DURATION` | - | experimental: new with -o url (signed links to results in a cloud store); the link format is the cloud's | With -o url: how long the links work, as 30m, 12h, 7d or seconds (default 1h, at most 7d). Experimental |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--partition` | `KEY=VALUE[,VALUE]\|JSON` | - | stable | Select keys of a partitioned target: `region=emea,amer`, or a JSON object for compound keys (`{"region": "emea", "day": "2026-10-14"}`). Only those keys are computed and returned, and --refresh of the target refreshes only them |
| `--refresh` | comma-separated names | - | stable | Assets to force re-materialize, as ONE comma-separated list (`--refresh a,b`, not `--refresh a b`); the target itself may be named. Every asset downstream of them in the target's cone re-materializes too (see --no-cascade) |
| `--no-cascade` | - | default `false` | stable | With --refresh: re-materialize only the named assets, not what is downstream of them. Cached downstream assets then do not reflect the refresh; barca warns |
| `--refresh-all` | - | default `false` | stable | Force re-materialize EVERY asset in the target's cone (nothing comes from cache) |
| `--dry-run` | - | default `false` | stable | Show what this command would do (each step cached or will-run, and why) without executing steps or recording a run |
| `--agent` | - | default `false` | stable | Agent-friendly output: plain structured progress lines instead of visual progress bar |
| `--fields` | comma-separated: `id`, `kind`, `action`, `status`, `reason`, `detail`, `run_hash`, `artifact`, `warning`, `artifact_mismatch`, `bytes_changed`, `elapsed_seconds`, `attempts`, `import_seconds`, `partitions`, `env` | - | stable | Keep only these keys (comma-separated) on each entry of `steps` in the JSON output. Implies --json; not valid with --pretty or -o pretty. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Run this command on a barca server (`barca serve`) and print its result here: the URL given, or with no URL the configured one (BARCA_SERVER, else [server] url in barca.toml). Without it, get and run run on this machine (against a configured server's ledger) and status, history and stats are answered by a configured server |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats) | Ignore any configured server for this command: run against this machine's own ledger and records; nothing is read from or sent to the server |

#### barca run

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ARGS>...` | - | required | stable | TARGET[,TARGET...] [file.py\|dir/ ...] — one or more target tasks, comma-separated; no files reads the whole project (`barca docs discovery`) |
| `--refresh` | comma-separated names | - | stable | Upstream assets to force re-materialize, as ONE comma-separated list (`--refresh a,b`, not `--refresh a b`). Every asset downstream of them in the task's cone re-materializes too (see --no-cascade) |
| `--no-cascade` | - | default `false` | stable | With --refresh: re-materialize only the named assets, not what is downstream of them. Cached downstream assets then do not reflect the refresh; barca warns |
| `--refresh-all` | - | default `false` | stable | Force re-materialize EVERY asset in the task's cone (nothing comes from cache) |
| `--dry-run` | - | default `false` | stable | Show what this command would do (each step cached or will-run, and why) without executing steps or recording a run |
| `-o, --output` | `json\|value\|pretty` | - | experimental: kept for compatibility; --json / --pretty are the canonical spelling | Output format (kept for compatibility; --json / --pretty are the canonical spelling) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--agent` | - | default `false` | stable | Agent-friendly output: plain structured progress lines instead of visual progress bar |
| `--fields` | comma-separated: `id`, `kind`, `action`, `status`, `reason`, `detail`, `run_hash`, `artifact`, `warning`, `artifact_mismatch`, `bytes_changed`, `elapsed_seconds`, `attempts`, `import_seconds`, `partitions`, `env` | - | stable | Keep only these keys (comma-separated) on each entry of `steps` in the JSON output. Not valid with -o value/pretty. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Run this command on a barca server (`barca serve`) and print its result here: the URL given, or with no URL the configured one (BARCA_SERVER, else [server] url in barca.toml). Without it, get and run run on this machine (against a configured server's ledger) and status, history and stats are answered by a configured server |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats) | Ignore any configured server for this command: run against this machine's own ledger and records; nothing is read from or sent to the server |

#### barca plan

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`) |

#### barca doctor

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories to inspect (default: discovered project sources) |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Environment name (separates cache/state per environment) |

#### barca logs

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<RUN_ID>` | - | required | stable | Exact run id from get/run JSON or history |
| `--step` | `STEP` | - | stable | Function name or exact node id (including a partition key when desired) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | stable | Environment whose local history to read |

#### barca history

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--reconcile` | `RUN_ID` | - | experimental: explicit operator recovery for stopped runs whose owner cannot be verified | After verifying a run stopped, record it as interrupted; refuses a proven live owner |
| `-l, --limit` | `LIMIT` | - | stable | Maximum recent runs (default: every run in JSON, 10 in a human table) |
| `--all` | - | default `false` | stable | Show every recorded run (no limit) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--fields` | comma-separated: `run_id`, `command`, `files`, `target`, `status`, `steps_total`, `steps_executed`, `steps_cached`, `attempts`, `started_at`, `finished_at`, `elapsed_seconds`, `triggered_by`, `scheduled_at`, `schedule_id`, `client_run_id` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `runs`. Implies --json. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Run this command on a barca server (`barca serve`) and print its result here: the URL given, or with no URL the configured one (BARCA_SERVER, else [server] url in barca.toml). Without it, get and run run on this machine (against a configured server's ledger) and status, history and stats are answered by a configured server |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats) | Ignore any configured server for this command: run against this machine's own ledger and records; nothing is read from or sent to the server |

#### barca gc

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--max-size` | `SIZE` | - | experimental (with the command) | A limit for this run in place of the configured one: 20GB, 500MB, 20GiB, bytes, 10% (of the filesystem) or off (only leftovers and superseded results) |
| `--dry-run` | - | default `false` | experimental (with the command) | Print what would be evicted without deleting anything |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Evict only this environment's results (leftovers are shared by all) |

#### barca stats

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TARGET>` | - | required | stable | Target asset function name |
| `<FILES>...` | - | - | stable | Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--fields` | comma-separated: `elapsed_seconds`, `status`, `created_at`, `error_message`, `attempts` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `recent_runs`. Implies --json. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Run this command on a barca server (`barca serve`) and print its result here: the URL given, or with no URL the configured one (BARCA_SERVER, else [server] url in barca.toml). Without it, get and run run on this machine (against a configured server's ledger) and status, history and stats are answered by a configured server |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats) | Ignore any configured server for this command: run against this machine's own ledger and records; nothing is read from or sent to the server |

#### barca serve

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`). With --watch, files added later are not picked up until restart |
| `-p, --port` | `PORT` | default `8274` | experimental (with the command) | Port to bind on |
| `--host` | `HOST` | default `127.0.0.1` | experimental (with the command) | IP address to bind on; 0.0.0.0 (or ::) listens on every interface. The API has no authentication |
| `--watch` | - | default `false` | experimental (with the command) | Dev mode: re-parse the DAG when source files change |
| `--no-schedule` | - | default `false` | experimental (with the command) | Disable the cron scheduler (Schedule(...) assets will not auto-fire) |
| `--timezone` | `TIMEZONE` | default `local` | experimental (with the command) | Timezone for cron evaluation: local (this machine's zone) or utc, in any letter case, or an IANA name such as America/New_York, which is case-sensitive. Any other value is a usage error |
| `--read-only` | - | default `false` | experimental (with the command) | Inspect only: refuse runs, never schedule, read the metadata DB from snapshots |
| `--drain-timeout` | `SECONDS` | - | experimental (with the command) | Seconds to wait for runs in flight after SIGTERM or SIGINT before cancelling them (exit 1). Default: [server] drain_timeout in barca.toml, else 30. 0 cancels at once |
| `--env` | `ENV` | - | experimental (with the command) | Environment name (separates cache/state per environment) |

#### barca list

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | stable | Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--groups` | - | default `false` | experimental: Organizational group hierarchy and metadata shape are experimental | Show the organizational hierarchy (groups are never executable nodes) |
| `-l, --limit` | `LIMIT` | - | stable | Maximum nodes, in topological order (default: every node in JSON, 100 in a human table) |
| `--all` | - | default `false` | stable | Show every node (no limit) |
| `--fields` | comma-separated: `id`, `kind`, `freshness`, `schedule`, `inputs`, `env`, `next_fire` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `nodes`. Implies --json. An unknown key is a usage error listing the valid ones |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Run this command on a barca server (`barca serve`) and print its result here: the URL given, or with no URL the configured one (BARCA_SERVER, else [server] url in barca.toml). Without it, get and run run on this machine (against a configured server's ledger) and status, history and stats are answered by a configured server |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats) | Ignore any configured server for this command: run against this machine's own ledger and records; nothing is read from or sent to the server |

#### barca status

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ARGS>...` | - | - | stable | [TARGET[,TARGET...]] [file.py\|dir/ ...] — both optional; no files reads the whole project (`barca docs discovery`) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `-l, --limit` | `LIMIT` | - | stable | Maximum nodes, in topological order (default: every node in JSON, 100 in a human table) |
| `--all` | - | default `false` | stable | Show every node (no limit) |
| `--fields` | comma-separated: `id`, `name`, `kind`, `inputs`, `partitioned`, `cache`, `partitions`, `last_materialization`, `last_bytes_change`, `shape`, `env`, `local` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `nodes`. Implies --json. An unknown key is a usage error listing the valid ones |
| `--sample` | `N` | - | experimental: sample rows come from a Python helper whose output may grow | Include up to N sample rows from each json/parquet/CSV artifact (off by default; pickles are never sampled) |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Run this command on a barca server (`barca serve`) and print its result here: the URL given, or with no URL the configured one (BARCA_SERVER, else [server] url in barca.toml). Without it, get and run run on this machine (against a configured server's ledger) and status, history and stats are answered by a configured server |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, and the answers of status, history and stats) | Ignore any configured server for this command: run against this machine's own ledger and records; nothing is read from or sent to the server |

#### barca sql

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<QUERY>` | - | required | experimental (with the command) | The SQL query (DuckDB dialect); quote it as one argument |
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories to read (default: every .py file under the project root that imports barca; see `barca docs discovery`) |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `-l, --limit` | `LIMIT` | default `100` | experimental (with the command) | Maximum number of rows to return |
| `--all` | - | default `false` | experimental (with the command) | Return every row (no limit) |
| `--env` | `ENV` | - | experimental (with the command) | Environment name (separates cache/state per environment) |

#### barca state

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| - | - | - | - | no arguments besides `-h, --help` |

#### barca state backups

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |

#### barca state restore

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--from` | `N\|NAME` | - | stable | Which backup: its index in `barca state backups` (1 = newest) or its name; default the newest |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |

#### barca docs

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TOPIC>` | - | - | stable | Topic to show (omit for the index), e.g. types, cache, examples/duckdb |
| `--all` | - | default `false` | stable | Print every topic in one stream |
| `--json` | - | default `false` | stable | Emit JSON instead of markdown |
| `--pretty` | - | default `false` | stable | Print markdown, the default even when piped (accepted like every read command's --pretty) |
| `--fields` | comma-separated: `name`, `summary`, `content` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `topics`, or on the one topic. Implies --json. An unknown key is a usage error listing the valid ones |

#### barca version

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| - | - | - | - | no arguments besides `-h, --help` |

#### barca help

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| - | - | - | - | no arguments besides `-h, --help` |
<!-- END GENERATED flags -->

## Output selection (stable)

`get`, `run`, `logs`, `list`, `status`, `history` and `stats` choose JSON or human output on stdout by
one rule, first match wins:

1. a flag: `--json` or `--pretty` (on `get`/`run` also `-o json|pretty`, which cannot be
   combined with them); `--fields` implies JSON and is a usage error with `--pretty` or
   `-o pretty` (on `run` also with `-o value`);
2. `BARCA_OUTPUT=json|pretty`;
3. the terminal: stdout is a TTY, human output; anything else, JSON.

`plan` always prints JSON. `docs` prints markdown unless `--json` or `--fields` is given. Errors
follow the same rule: in JSON mode they are the envelope on stderr (see Errors).

### What `get` returns (stable)

`barca get` returns artifact paths: one per target, one per partition key. `-o path` (the
default) and `-o value` choose what is returned. Alone, `-o path` prints only the paths, one per
line, and `-o value` only the value, pretty-printed; with `--json` (or `--fields`) they choose
what `final_output` holds, and with `--pretty` they add the human summary. Without `-o`, the
rule above chooses between the JSON result and the summary, and paths are returned. In a
terminal the summary previews a small json value (at most 10 lines) under its path; the preview
is human output, not contract. `run` keeps `-o value`, the task's value, and prints a task's
value in `final_output` as before.

| Invocation | stdout |
|---|---|
| `get` (piped), `get --json`, `get -o path --json` | the JSON result, `final_output` a pointer `{"_barca_artifact": {"path", "format", "size_bytes"}}` for every format |
| `get -o value --json` | the JSON result, `final_output` the value |
| `get -o path` | the paths, one per line |
| `get -o value` | the value, pretty-printed |

A partitioned target returns every key, or the keys `--partition` selects
(`DIMENSION=VALUE[,VALUE...]`, or a JSON object of dimension to value or list of values). Its
`final_output` is an object keyed by partition key (`dim=value`, dimensions sorted and joined
with `,`), sorted, each a pointer or (under `-o value`) a value; `-o path` prints one line per
key in the same order. A `--partition` that names no target, a target that is not partitioned
or has no such dimension, a value that is not one of its keys, or a target another target of the
run reads, is a usage error (exit 2) before anything runs.

Up to 0.24, `final_output` held the value of a json artifact and a terminal printed the value;
`-o value` (with `--json` for the JSON line) prints it now. A partitioned target returned the
value of its first key only.

## Environment variables

| Variable | Effect | Stability |
|---|---|---|
| `BARCA_OUTPUT` | `json` or `pretty`: output format when no flag is given; any other value is a usage error (exit 2) | stable |
| `BARCA_ENV` | environment name when `--env` is not given (beats `default_env` in `barca.toml`) | stable |
| `BARCA_REMOTE` | `off` disables remote artifacts and history backups for this process; unset/empty retains normal config; other values are usage errors | stable |
| `BARCA_REMOTE_URI` | remote root for artifacts and metadata backups (`[remote].uri`) | stable |
| `BARCA_TRANSFER_CONCURRENCY` | positive integer: concurrent artifact transfers (`[remote].transfer_concurrency`, default 4); invalid values exit 3 | stable |
| `BARCA_TRANSFER_TIMEOUT` | positive integer seconds per transfer attempt (`[remote].transfer_timeout`, default 600); invalid values exit 3 | stable |
| `BARCA_STORAGE_OPTIONS` | JSON object keyed by protocol, merged over `[remote.storage_options.*]` | stable |
| `BARCA_ARTIFACT_URI` | literal artifact root, bypassing the environment prefix (warns with a non-default env) | experimental |
| `BARCA_PROGRESS_SECS` | seconds between `still running` lines (default 15, `0` turns them off) | experimental |
| `BARCA_POOL_SIZE` | number of Python workers (default: available cores) | experimental |
| `BARCA_COMM_COST_SECONDS` | the scheduler's per-dispatch cost estimate | experimental |
| `BARCA_TRACE_TIMING` | when set, prints a timing waterfall on stderr | experimental |
| `BARCA_CACHE_MAX_SIZE` | size limit for `.barca/` (`[cache] max_size`): `20GB`, `500MB`, `20GiB`, bytes, `10%` of the filesystem or `off`; default 10% of the filesystem; an invalid value is a usage error (exit 2) | experimental |
| `BARCA_SERVER` | the configured barca server (beats `[server] url` in `barca.toml`): the ledger of a local `get`/`run`, what `status`/`history`/`stats` answer from, and what a bare `--server` uses; empty is unset; an invalid URL is a usage error (`barca docs server-mode`) | experimental |
| `BARCA_TELEMETRY` | comma-separated telemetry integrations to send finished runs to; `datadog` is the only one (`barca docs telemetry`). Its settings are Datadog's own variables (`DD_SERVICE`, `DD_ENV`, `DD_VERSION`, `DD_TAGS`, `DD_TRACE_AGENT_URL`, `DD_AGENT_HOST`, `DD_TRACE_AGENT_PORT`, `DD_TRACE_ENABLED`, and for `barca serve` metrics `DD_DOGSTATSD_URL`, `DD_DOGSTATSD_SOCKET`, `DD_DOGSTATSD_PORT`) | experimental |
| `BARCA_SLACK_WEBHOOK_URL` | a Slack incoming webhook (`https://`); set and non-empty, barca posts when a job starts failing, recovers, or is still failing a day later (`barca docs telemetry`). Never printed. `SLACK_WEBHOOK_URL` is ignored | experimental |
| `BARCA_SLACK_NOTIFY` | `changes` (default), `failures` or `all`: which finished runs post to Slack; any other value is a warning and `changes` applies | experimental |
| `BARCA_UI_URL` | base URL of the web UI; Slack posts link the run id to it | experimental |
| `BARCA_TRACE_LIBRARIES` | `1` or `true`: with `BARCA_TELEMETRY=datadog` and `ddtrace` installed, workers patch every library `ddtrace` supports. Unset: only those `DD_PATCH_MODULES` or `DD_TRACE_<LIB>_ENABLED=true` name (`barca docs telemetry`) | experimental |
| `BARCA_LOG` | `json`: `barca serve` writes every line as one JSON object on stdout instead of text on stderr (`barca docs telemetry`, "Logs under barca serve"); `text` or unset: the default. Under `serve` any other value is a usage error (exit 2). On every other command it changes nothing and prints `[barca] warning: BARCA_LOG applies to barca serve only; this command writes its usual lines` | experimental |

`[server] url` in `barca.toml` or `BARCA_SERVER` (experimental) points `get` and `run` at a
server's ledger (`barca docs server-mode`): they still run here, read cache hits from the server
and post what they compute, over `http://` or `https://`. `status`, `history` and `stats` answer
from the server. `--local` ignores it for one command. A server that cannot be reached, answers
unexpectedly or speaks another API version is exit 3 naming its URL, with `--local` as the
remediation; a server whose store is not this project's `[remote]` (or a project with no
`[remote]`) is exit 2 naming both.

barca sets `BARCA_SOCKET`, `BARCA_WORKER`, `BARCA_WORKER_ID`, `BARCA_WORKER_AGENT` (and passes `BARCA_ARTIFACT_URI`,
`BARCA_STORAGE_OPTIONS`) for its own worker processes; these are internal, not part of the
contract. `BARCA_TEST_*` variables are hooks for barca's own test suite, also not part of the
contract. Variables your nodes declare with `@asset(env=[...])` are yours (`barca docs assets`).

Removed in 0.26.0 (breaking, RFC-0009): `BARCA_STATE`, `BARCA_STATE_URI`, `BARCA_PUSH_RETRIES`
and the `barca.toml` keys `[remote] state`, `[remote] state_uri`, `[remote] push_retries`. Any of
them set is a usage error (exit 2) naming it and the release, even under `BARCA_REMOTE=off`.

## Exit codes

Each error `kind` has exactly one exit code (stable):

<!-- BEGIN GENERATED exit-codes -->
| Code | `kind` | Stability |
|---|---|---|
| 0 | (success) | stable |
| 1 | `step_failed` | stable |
| 2 | `usage` | stable |
| 3 | `infra` | stable |
| 130 | `cancelled` | stable |
<!-- END GENERATED exit-codes -->

`step_failed`: one of your steps raised (fix the code). `usage`: bad arguments, unknown target,
task/asset misuse, a `.py` file that does not parse, a decorator called with an argument it does
not define (`@asset(after=x)`, `input=` for `inputs=`, `@asset(**options)`; `barca docs assets`,
"Accepted arguments"), a DAG that cannot be built (an input that
names no definition, a cycle, a partitioned asset in an unpartitioned asset's `inputs=` without
`collect()`, a `partitions_from()` the asset cannot mirror), invalid `--env` or `barca.toml`. `infra`:
barca or its environment failed (metadata DB, workers, the store, I/O); retrying may help.
`cancelled`: stopped by Ctrl-C (SIGINT) or by SIGTERM (what a supervisor, a CI timeout or
`docker stop` sends), at any point of a `get` or `run`: while steps run, and
with an artifact store also while artifacts upload or download or the history is backed up.
The exit code is 130 whatever the store does: a cancelled run spends at most 10 seconds backing
its record up, a second Ctrl-C or SIGTERM ends that at once, and a backup that fails then is a
line on stderr, not exit 3 (`barca docs remote`, "Ctrl-C").

An artifact upload or history-backup upload that fails after every step succeeded is not exit 3
either: the run exits 0 with a warning and an `uploads` object, and the results are uploaded
by the next command that opens the store (see "get and run: results the store did not
receive").

The exit code is 130 for SIGTERM as well as for SIGINT. Barca handles both signals the same way
(it stops the workers, records the run as `cancelled` and exits), and the exit code follows the
error `kind`, which has one code. 143, which a shell reports for a process that SIGTERM killed,
is never barca's own exit code. SIGKILL cannot be handled: the shell reports 137 and the run is
later reported as `interrupted` (`barca docs cache`).

`barca serve` is not a run. Stopped by SIGINT or SIGTERM it drains: it refuses new runs (HTTP
`503` with `Retry-After`) and waits for the runs in flight for up to `--drain-timeout` seconds
(`[server] drain_timeout` in `barca.toml`, default 30). It exits 0 when they all finished. When
the deadline passes or a second signal arrives first, it cancels the runs left (recorded as
`cancelled`, reason `server shutdown`) and exits 1 with a `step_failed` error naming how many
were cancelled, so deploy tooling can tell a forced stop from a clean drain (`barca docs
scheduling`, "Stopping the server"). Exit 1 from `serve` means a forced stop, not a failed step
of yours.

With `BARCA_LOG=json`, `barca serve` writes its lines, an error that stops it included, as JSON
objects on stdout instead of text on stderr. Exit codes do not change. `get`, `run` and every
other command keep progress and errors on stderr whatever `BARCA_LOG` says.

An error in a pipeline file (a syntax error, a decorator argument barca does not define, an
invalid `env=` or cron) fails every command that reads that file, whatever the target: `list`,
`plan`, `status`, `get`, `run`, `stats` and `sql` exit 2 and run nothing. With no file arguments
that is every pipeline file of the project. The envelope's `error` starts with `Parse error:
<file>:<function> (line <n>):` for a decorator argument, and its `remediation` says what to
change; the wording is not contract.

### A closed stdout or stderr (stable)

When the reader of barca's output goes away (`barca list | head -1`, a pager that quits, a
caller that stops reading), barca prints nothing about it and does not stop what it is doing:

- Output for the closed stream is dropped. There is no panic, no traceback and no message on
  the other stream.
- The command finishes. A `get` or `run` executes every step and records the run as it would
  with a reader, whether stdout, stderr or both are closed. A closed pipe is not a
  cancellation: to stop a run, interrupt it (Ctrl-C, exit 130).
- A step is not affected, whatever it does. Its `print` output, the output of a process it
  starts, a write to file descriptor 1 or 2 and the output of a C library are dropped like
  barca's own; none of them raises `BrokenPipeError` in the step or ends a child process with
  SIGPIPE. When barca's stderr is a pipe or a socket, the workers write to a pipe barca itself
  reads and copies to its stderr, so they never hold a pipe that can lose its reader. With a
  reader attached the output is the same as before, in the same order, on the same stream
  (stderr); when stderr is a terminal or a file the workers hold it directly, as before.
  SIGPIPE keeps its default for the processes a step starts, so `yes | head -1` inside a step
  ends as usual.
- The exit code does not change. It is the one the command would have had with a reader: 0
  when it succeeded, and 1, 2, 3 or 130 when it has an error of its own (the envelope is still
  written to stderr if stderr is open). A reader that stops early is not an error, so
  `barca list | head -1` and `barca list | grep -q name` exit 0, also under `set -o pipefail`.
  barca does not exit 141 and is not ended by SIGPIPE.

This holds for every command, `--help`, `--version` and `barca docs` included. Up to 0.18.1,
`docs` and `--help` behaved this way, every other command ended in a Rust panic (exit 101), and
a closed stderr abandoned a run in the middle (it stayed `interrupted` in `barca history`).

Limits: barca learns that a stream is closed only when it writes to it, so a run whose reader
has left goes on to its end; interrupt it to stop it.

**New in this release: a process a step leaves running.** If a step starts a process that
outlives the run (a server, a watcher) and lets it inherit the step's stdout and stderr, then,
when barca's stderr is a pipe or a socket, that process holds barca's pipe on both, not the
caller's. Once barca has exited nobody reads that pipe: the process's next write to its stdout
or its stderr ends it (SIGPIPE), or fails with `BrokenPipeError` if it ignores the signal. Up to
0.18.1 it held the caller's pipe instead, went on writing to it, and the caller's pipeline
(`barca run start_server 2>&1 | tee log`) did not finish until that process did. Nothing
changes when barca's stderr is a terminal or a file: the process holds that, as before. The
remedy is to give such a process its own output when starting it, for example
`subprocess.Popen(cmd, stdout=log, stderr=log)` or `stdout=subprocess.DEVNULL,
stderr=subprocess.DEVNULL`.

A caller's pipe in non-blocking mode is supported: while its reader is behind, barca waits for
it (no output is dropped, and the wait reaches the step that is printing), on stdout and on
stderr. A caller cannot tell from the exit code
that part of the result was not read: it is the caller that stopped reading. A write to stdout
that fails for any other reason (a full disk behind `> result.json`) is exit 3, with one line
on stderr.

Breaking in 0.24.0: the deprecated `get`/`run --no-cache` flag is removed (usage error,
exit 2). Use `--refresh-all`. Python `barca.get` also removes the deprecated keyword;
use `refresh_all=True`.

`list`, `status` and `history` return every item in JSON by default, including piped
stdout, `--fields` and `BARCA_OUTPUT=json`. An explicit `--limit N` applies in either
format; `--all` cannot be combined with it. Human tables default to 100 nodes for
`list`/`status` and 10 runs for `history`. Truncation still reports `total`, `truncated`
and a `hint` naming `--all`; the status summary still counts every node.

## JSON output schemas

How to read the tables: a key path is dotted, `[]` is "each item of the array", `<name>` stands
for keys you chose (target names, environment variable names) and `<user value>` is the value
your function returned. Types are JSON types; `a | b` means either. `sometimes` means the key is
present on some objects only (see each section for when). Values in the tables come from one
fixture run, so a nullable key can show only the type it had there; the notes say which keys
can be `null`.

Every schema below is stable unless its section says otherwise.

### Plan warnings

`plan`, `get`, `run` and `get|run --dry-run` carry a top-level `warnings` key in every JSON
document they print on stdout: success, failed (`status: "failed"`), several targets and dry run
alike. It is **always present and always an array**, `[]` when there is nothing to report, like
every other array in these schemas (an empty array is never dropped, and never `null`). In the
tables below a section whose fixture run had no warning shows only the `warnings` row; one that
had a warning also shows the item rows.

Each item is an object with exactly these keys, all strings and always present:

| Key | Value |
|---|---|
| `kind` | what was found. `unused_input` is the only value so far; treat an unknown value as "other" |
| `node` | the step's node id, `file.py:name` (never a partition key: a partitioned step is reported once) |
| `param` | the parameter concerned |
| `message` | the warning in words, with what to do about it. The wording is not contract |

The list covers the steps the command planned (the targets' cones; the whole project for `plan`
or `get` with no target), one item per step and parameter, sorted by node, parameter and kind. It is a function of the
source files and the targets only: cache state, `--refresh*`, `--env` and the output mode do not
change it, so a dry run reports exactly what the real run will. Each item is also printed once on
stderr as `[barca] warning: <message>` before any step runs, in every output mode. Warnings never
change the exit code, and the error envelope on stderr never contains them. `list`, `status`,
`history`, `stats` and `sql` have no `warnings` key. `barca serve` carries the same array in
`GET /plan` and in the run outcome of `GET /runs/{run_id}` (experimental with the rest of that
API).

`unused_input`: a step declares an input its function never uses (`barca docs assets`, "Unused
inputs", has the exact rule and what is never reported). Stability: the key, its presence and the
item keys are stable on `get` and `run`, and experimental with the rest of the output on `plan`;
the set of `kind` values grows additively.

### get and run: one target

`barca get <asset> files --json` and `barca run <task> files --json` print one JSON line.

<!-- BEGIN GENERATED schema get -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get -->

- `status` is `success` (see the failed result line below for `failed`).
- `steps_executed` is 0 when everything came from cache. On `get`, `final_output` is the
  pointer to the target's artifact (with no target, the last asset's); on `run` it is the
  task's value, or `null` for a task that returned nothing.
- `steps[]`: `status` is `ran`, `cached`, `partial` or `failed`; `reason` (why it ran) is one of
  `task`, `sensor`, `refresh`, `refresh_cascade`, `refresh_all`, `not_materialized`,
  `artifact_missing`, `evicted`, `partitions_unknown`, `sensor_output_unknown`, with `detail` in
  words. `artifact_missing` is a step that has a cached result whose artifact file is gone
  and is needed (`barca docs cache`); `evicted` is the same when the cache size limit deleted
  the file (experimental, "Disk use").
- `cache` (experimental) appears only when the check after the run found `.barca/` over its
  size limit: `{size_bytes, limit_bytes, evicted_files, evicted_bytes}`, sizes after eviction
  (`barca docs cache`, "Disk use"). It is on single- and multi-target results alike.
- `get` and `run` share one refresh vocabulary: `--refresh a,b` (cascading downstream),
  `--no-cascade`, `--refresh-all`. `artifact` appears on cached steps, `run_hash`
  on unpartitioned steps, `warning` on a cached step whose upstream was refreshed without
  cascading and on a step with `artifact_mismatch` (see "A store copy that differs from its
  recorded hash" below), `env` on nodes that declare `env=[...]` (`null` for an unset variable,
  `"<redacted>"` for secret-looking names).

Executed steps also carry `attempts` and `elapsed_seconds`, including failures.
Elapsed seconds sum active reported time across attempts; backoff is excluded. For
partition summaries, elapsed time sums executed keys while attempts is the maximum
per key. Cached steps and dry runs omit these keys. Worker disconnects use the
coordinator's observed in-flight time. Slow module imports (> 1 second) add
`import_seconds`, included in elapsed time and summed across attempts/keys. Reused
modules add no import time. The conditional import field is snapshotted with a
controlled slow-import/retry fixture below; ordinary schema fixtures exclude it to
avoid depending on CI machine speed. `barca docs status` explains the timing scopes.

<!-- BEGIN GENERATED schema get_timing_retry -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].import_seconds` | number | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_timing_retry -->

Under `-o value --json`, `final_output` is the value instead (a table is a list of row
objects):

<!-- BEGIN GENERATED schema get_value -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_value -->

A parquet or pickle artifact has the same pointer as a json one:

<!-- BEGIN GENERATED schema get_artifact_pointer -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_artifact_pointer -->

A partitioned step carries `partitions` (counted in keys; `will_run_keys` lists the first 20 keys in lexical order):

<!-- BEGIN GENERATED schema get_partitioned -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | sometimes |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].partitions.will_run_keys[]` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | sometimes |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partitioned -->

A partitioned target (`get per_key`) returns every key: `final_output` is an object keyed by
partition key, each a pointer.

<!-- BEGIN GENERATED schema get_partition_keys -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output.<key>` | object | always |
| `final_output.<key>._barca_artifact` | object | always |
| `final_output.<key>._barca_artifact.format` | string | always |
| `final_output.<key>._barca_artifact.path` | string | always |
| `final_output.<key>._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | always |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_keys -->

With `--partition k=a -o value --json`, only the selected keys, each its value:

<!-- BEGIN GENERATED schema get_partition_values -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output.<key>` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | always |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_values -->

`run` prints the same shape:

<!-- BEGIN GENERATED schema run -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
<!-- END GENERATED schema run -->

### get and run: a failed step

When a step raises in JSON mode, stdout still gets one result line (exit 1), and the error
envelope goes to stderr:

<!-- BEGIN GENERATED schema run_failed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `error` | string | always |
| `failed_node` | string | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema run_failed -->

A partitioned target of `get` with a failed key is a failed step as above (exit 1, `failed_node`
names the key's step, e.g. `pipeline.py:flaky[k=bad]`). The line also has `final_output`: the
keys that succeeded, keyed as above; the failed keys are absent and have no artifact, so the next
`get` computes them again. Stability: experimental. Whether a partial failure gets its own exit
code, and whether the result lists the failed keys, is not decided yet.

<!-- BEGIN GENERATED schema get_partition_failed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `error` | string | always |
| `failed_node` | string | always |
| `final_output` | object | always |
| `final_output.<key>` | object | always |
| `final_output.<key>._barca_artifact` | object | always |
| `final_output.<key>._barca_artifact.format` | string | always |
| `final_output.<key>._barca_artifact.path` | string | always |
| `final_output.<key>._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | always |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].partitions.will_run_keys[]` | string | always |
| `steps[].reason` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_failed -->

### get and run: a store copy that differs from its recorded hash

With an artifact store, an artifact fetched from the store is checked against the SHA-256
recorded when it was written (`barca docs remote`, "Checking a local copy against the store"). A copy with
other bytes is still used, the exit code does not change, and the result says so on the step
entries:

<!-- BEGIN GENERATED schema get_artifact_mismatch -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].artifact_mismatch` | boolean | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].bytes_changed` | boolean | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps[].warning` | string | sometimes |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_artifact_mismatch -->

- `steps[].artifact_mismatch` is the boolean `true`, or the key is absent. It is never `false`
  and never `null`: test for the key. It is on the step the artifact belongs to, whatever that
  step's `status`, and on every step that read the artifact as an input in this run (`status`
  `ran`, `partial` or `failed`). A step that only depends on such a reader does not have it.
- `steps[].warning` on the same entries is the finding in words: the store location, and the
  `--refresh` that recomputes the step. The wording is not contract; `artifact_mismatch` is.
- The key appears on `get` and `run`, with one target or several, in a failed run's result
  line, with `--agent`, and in the `steps` of a `barca serve` run's outcome (`GET /runs/{run_id}`).
  `--fields` accepts it (`--fields id,status,artifact_mismatch`). It never appears with
  `--dry-run`, which does not contact the store, nor in `status`, `history` or `stats`, which
  describe recorded results and do not fetch.
- It is not an item of the top-level `warnings` array. That array is the plan warnings (see
  "Plan warnings"): a function of the source files and the targets, the same in a dry run and
  whatever is cached. A mismatch is found only when a run fetches an artifact, so it is
  reported where the other facts about what happened to a step are.
- stderr has the same finding in every output mode, `--agent` included, as
  `[barca] warning: <id>: the copy at <location> is not the one this result was recorded
  with ...`. Like every `[barca] warning:` line its wording is experimental: read the key.

In the fixture run above, the store's copy of one asset was overwritten after it was recorded;
that asset and the step recomputed from it carry the key, and the two other steps do not.
Stability: stable.

### get and run: results the store did not receive

With an artifact store, a run whose steps all succeeded and whose only failure is an upload of
an artifact (or of the end-of-run history backup) is a success: exit 0, the usual
result, one `[barca] warning: <n> result(s) not uploaded to <store> (<ids>): <why>; ...` line
on stderr, and an `uploads` object (#390). The results are recorded and kept on this machine;
the next `get` or `run` that opens the store uploads them before its own work and says so on
stderr (`[barca] uploading <n> pending results ...`). There is no flag to give up on them.
A store that fails before any step runs (the startup check, a cached artifact that cannot be
fetched) is still exit 3.

<!-- BEGIN GENERATED schema get_uploads_pending -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `uploads` | object | always |
| `uploads.error` | string | always |
| `uploads.nodes` | array | always |
| `uploads.nodes[]` | string | always |
| `uploads.pending` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_uploads_pending -->

- `uploads.pending` counts this machine's results not yet in the store after the run,
  including earlier runs' that failed again; `uploads.nodes` names them (step ids, partition
  keys included, sorted); `uploads.error` is why, in words (`null` only when nothing says).
  A failed backup upload alone gives `pending: 0` and its error (`backup to <store> failed: ...`). The key is absent when nothing
  is pending: test for the key. It is on `get` and `run`, with one target or several, and on
  `result` of `barca serve`'s run status.
- `barca status` shows each such node with `cache.state` `unsynced` and `cache.reason`
  `local_only` (the table prints `unsynced  local only`), and `summary.unsynced` counts them:

<!-- BEGIN GENERATED schema status_unsynced -->
| Key | Type | Present |
|---|---|---|
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.artifact` | string | always |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | always |
| `nodes[].cache.state` | string | always |
| `nodes[].env` | array | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `nodes[].last_materialization` | object | always |
| `nodes[].last_materialization.artifact` | string | always |
| `nodes[].last_materialization.content_hash` | string | always |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | number | always |
| `nodes[].last_materialization.format` | string | always |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | integer | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].name` | string | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].shape` | object | always |
| `nodes[].shape.keys` | array | sometimes |
| `nodes[].shape.keys[]` | string | always |
| `nodes[].shape.rows` | integer | sometimes |
| `nodes[].shape.type` | string | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_unsynced -->

- `--dry-run` has the same `uploads` object when results are pending, and predicts them
  `cached`; the human dry run adds a line naming them:

<!-- BEGIN GENERATED schema get_dry_run_uploads_pending -->
| Key | Type | Present |
|---|---|---|
| `command` | string | always |
| `dry_run` | boolean | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].artifact` | string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].run_hash` | string | always |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `target` | string | always |
| `uploads` | object | always |
| `uploads.error` | string | always |
| `uploads.nodes` | array | always |
| `uploads.nodes[]` | string | always |
| `uploads.pending` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_dry_run_uploads_pending -->

With an artifact store, `barca status` also reports `unsynced` with `cache.reason` `store_only`
(in the store, not on this machine) and `mismatch` (the two copies differ in size, or in bytes
for a plain-directory store, or this machine's copy is not the recorded one) for unpartitioned
cached nodes; a result in neither place is `stale` / `artifact_missing` (RFC-0009 section 4.1,
reasons spelled as `cache.reason` snake_case values). `barca get` on such a target repairs it
without running a step: it uploads a `local_only` or `mismatch` copy (a `mismatch` also prints
the changed-bytes warning and sets `steps[].bytes_changed: true` on that step) and fetches a
`store_only` one; the exit code does not change. Stability: stable.

### get and run: several targets

With `a,b`, `final_output` is replaced by `targets`, keyed by target name in the order given.
A successful target has `final_output` (on `get` a pointer, or with `-o value --json` its value);
a failed partitioned target of `get` also has `final_output` with the keys that succeeded; a
failed one has `failed_node` (the step that raised:
the target or something upstream; the same key as a failed single-target run) and `error`. Top-level `status` is `failed` if any target
failed, and the exit code is then 1. Steps skipped because an upstream failed have `status`
`skipped` and `reason` `upstream_failed`.

<!-- BEGIN GENERATED schema get_multi_target -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `targets` | object | always |
| `targets.<name>` | object | always |
| `targets.<name>.final_output` | object | always |
| `targets.<name>.final_output._barca_artifact` | object | always |
| `targets.<name>.final_output._barca_artifact.format` | string | always |
| `targets.<name>.final_output._barca_artifact.path` | string | always |
| `targets.<name>.final_output._barca_artifact.size_bytes` | integer | always |
| `targets.<name>.status` | string | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_multi_target -->

<!-- BEGIN GENERATED schema run_multi_target_failed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `targets` | object | always |
| `targets.<name>` | object | always |
| `targets.<name>.error` | string | sometimes |
| `targets.<name>.failed_node` | string | sometimes |
| `targets.<name>.final_output` | `<user value>` | sometimes |
| `targets.<name>.status` | string | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
<!-- END GENERATED schema run_multi_target_failed -->

### Dry run

`--dry-run` on `get` or `run` executes no steps and records no new run; it prints what
would happen. `partitions(<expression>)` may evaluate Python while resolving keys. `steps[].action` is
`cached`, `run`, `partial` or `unknown` (in place of `status`). An `unknown` step has `reason`
`partitions_unknown` (dynamic partitions whose source has not run) or `sensor_output_unknown` (it,
or a step upstream of it, reads a sensor with no recorded output). A step that reads a sensor is
predicted from the sensor's last recorded output, and `detail` says so. With one target the
document has `target` (a name, or `null` for a whole file):

<!-- BEGIN GENERATED schema get_dry_run -->
| Key | Type | Present |
|---|---|---|
| `command` | string | always |
| `dry_run` | boolean | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].detail` | string | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `target` | string | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_dry_run -->

With several targets, `targets` replaces `target`: an object keyed by target name in the order
given (like a real multi-target run), each `{"summary": {...}}` counted over that target's cone.
The top-level `summary` counts the union once.

<!-- BEGIN GENERATED schema run_dry_run_multi_target -->
| Key | Type | Present |
|---|---|---|
| `command` | string | always |
| `dry_run` | boolean | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].artifact` | string | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `targets` | object | always |
| `targets.<name>` | object | always |
| `targets.<name>.summary` | object | always |
| `targets.<name>.summary.cached` | integer | always |
| `targets.<name>.summary.unknown` | integer | always |
| `targets.<name>.summary.will_run` | integer | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
<!-- END GENERATED schema run_dry_run_multi_target -->

### list

List-shaped output is an envelope: the items, `total` (how many exist) and `truncated`. `hint`
appears only when `truncated` is true. `--fields` keeps only the named keys on each item.

<!-- BEGIN GENERATED schema list -->
| Key | Type | Present |
|---|---|---|
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].env` | array | always |
| `nodes[].env[]` | string | always |
| `nodes[].freshness` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `root` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema list -->

### `list --groups`

<!-- BEGIN GENERATED schema list_groups -->
| Key | Type | Present |
|---|---|---|
| `groups` | array | always |
| `groups[]` | object | always |
| `groups[].description` | string | always |
| `groups[].id` | string | always |
| `groups[].members` | array | always |
| `groups[].members[]` | string | always |
| `groups[].name` | string | always |
| `groups[].output` | string | always |
<!-- END GENERATED schema list_groups -->


Truncated (`--limit 1`):

<!-- BEGIN GENERATED schema list_truncated -->
| Key | Type | Present |
|---|---|---|
| `hint` | string | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].env` | array | always |
| `nodes[].freshness` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].kind` | string | always |
| `root` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema list_truncated -->

`nodes[].kind` is `asset`, `task` or `sensor`; `nodes[].freshness` is `always`, `manual` or
`schedule`, lowercase like `kind`. A scheduled node also has `schedule` (the cron expression) and
`next_fire` (string, `YYYY-MM-DD HH:MM:SS`): the next match of the cron expression in the local
time of the machine `list` runs on. `list` talks to no server, so the value does not follow a
server's `--timezone`; `GET /schedule` on the running server does (`barca docs scheduling`).
`list` reads no state, so it takes no `--env`.

### sql (experimental)

The query result: `columns` in order and one object per row in `rows`, keyed by column name, with
the same bounded-output envelope as `list` (`total` counts every row the query returns; `hint`
appears when `truncated`). Row values are JSON scalars, arrays or objects; dates and times are ISO
8601 strings, NaN and infinities are `null`.

With remote storage, the artifacts of the views a query names are downloaded into
`.barca/sql-cache/`; a download is reported on stderr (`barca: fetched 1 remote artifact ...`) and
the JSON is the same. A remote artifact that cannot be downloaded exits 3 (`infra`); a missing
storage driver exits 2 (`usage`).

Partitioned views include only current identities from full cache-aware prediction,
including cached keys across worker chunks. Removed-key history is preserved. A
current key may show its stale last successful result; an unknown derived key set
requires its source to materialize first, and zero current keys have no result
view. Excluded historical remote artifacts are not fetched for that view.

<!-- BEGIN GENERATED schema sql -->
| Key | Type | Present |
|---|---|---|
| `columns` | array | always |
| `columns[]` | string | always |
| `rows` | `<user value>` | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema sql -->

### status

The status document: the same envelope keys as `list` (`total`, `truncated`, `hint`) beside
`target`, `targets`, `nodes` and `summary`. `target` is the one target given (`null` for the whole
file or several targets); `targets` lists every target given (`[]` for the whole file). `summary`
counts every node even when `nodes` is truncated.

<!-- BEGIN GENERATED schema status -->
| Key | Type | Present |
|---|---|---|
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.artifact` | string | sometimes |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | sometimes |
| `nodes[].cache.state` | string | always |
| `nodes[].env` | array | always |
| `nodes[].env[]` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `nodes[].last_materialization` | null \| object | always |
| `nodes[].last_materialization.artifact` | null \| string | always |
| `nodes[].last_materialization.content_hash` | string | sometimes |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | null \| number | always |
| `nodes[].last_materialization.error` | string | sometimes |
| `nodes[].last_materialization.format` | null \| string | always |
| `nodes[].last_materialization.partition` | string | sometimes |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | integer \| null | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].name` | string | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].partitions` | object | sometimes |
| `nodes[].partitions.cached` | integer | always |
| `nodes[].partitions.missing` | integer | always |
| `nodes[].partitions.missing_keys` | array | always |
| `nodes[].partitions.missing_keys[]` | string | always |
| `nodes[].partitions.total` | integer | always |
| `nodes[].shape` | null \| object | always |
| `nodes[].shape.columns` | array | sometimes |
| `nodes[].shape.columns[]` | object | always |
| `nodes[].shape.columns[].name` | string | always |
| `nodes[].shape.columns[].type` | string | always |
| `nodes[].shape.keys` | array | sometimes |
| `nodes[].shape.keys[]` | string | always |
| `nodes[].shape.rows` | integer | sometimes |
| `nodes[].shape.sample` | `<user value>` | always |
| `nodes[].shape.type` | string | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status -->

- `cache.state` is `cached`, `unsynced`, `stale`, `never_run`, `partial`, `unknown` or
  `always_runs`: the same snake_case spelling as the `summary` keys (the human table prints
  `never-run`; `summary.unsynced` is always present);
  `cache.reason` is `materialized`, `local_only`, `store_only`, `mismatch`, `not_in_ledger`, `changed`, `upstream_stale`, `failed`, `artifact_missing`,
  `evicted` (experimental), `no_record`, `partitions_missing`, `partitions_unknown`,
  `sensor_output_unknown`, `task` or `sensor`.
- With a configured server (experimental, `barca docs server-mode`), `barca status` without
  `--server` or `--local` is answered by the server and each node gets `local` (boolean: this
  machine has a copy of the result). A node the server has no result for whose result this
  machine computed and has not posted yet is `unsynced` with reason `not_in_ledger` (the table
  prints `not in ledger`); `summary.unsynced` counts `unsynced` nodes and is `0` otherwise.
  `cache.run_hash` and `cache.artifact` appear when known (`artifact` only when cached).
- `partitions` appears only on partitioned nodes. `last_materialization` is `null` when the node
  never ran; in it `partition` appears for a partitioned node and `error` for a failed run.
- `shape` is `null` unless the last materialization succeeded, and is experimental: `type`, and
  by format `rows`, `columns[]`, `keys`, `key_count`, `sample` (with `--sample`), or only `note`
  when it cannot be read.

### state backups and state restore

`barca state backups` prints `{backups, local_dir, store}` and, when the store could not be
listed, `store_error` (a string; the local backups are still listed and stderr has a warning).
`store` is the store's backup prefix with credentials redacted, or `null` without a store.
Each entry of `backups`, newest first: `index` (integer, 1 = newest), `name`
(`2026-10-14T18-02-11.482Z-3f9a1c2e.db`; `2026-10-14T18-02-11Z.db` from 0.26.0), `where`
(`local`, `store` or `both`), `size_bytes` (integer),
`run_id` (string or `null`), `rows` and `db_format` (integers or `null`), `digest` and
`barca_version` (strings or `null`; from the backup's sidecar), and `agree` (boolean when the
backup is in both places, else `null`). No backups is an empty array, exit 0.

`barca state restore [--from N|NAME]` puts a backup in place of the metadata DB and moves the
current one to `metadata.db.pre-restore`. One `[barca] restored ...` line on stderr (not
contract); with `--json` (or piped), stdout has `{restored, from, source, pre_restore, rows,
run_id}`: `from` is `local` or `store`, `source` the copy's path or redacted URI,
`pre_restore` the path of the replaced database or `null`. Exit 2 (`usage`): no backup, an
unknown `--from`, a copy that fails validation (the message names the next newest), a copy in a
format this barca cannot use. Exit 3 (`infra`): a `barca serve` holds the environment, or no
local backup exists and the store cannot be listed. Nothing is changed on any error.
Stability: stable.

### history

`runs[].attempts` is the sum of attempts in its recorded materialization rows, including
executed partition keys; an all-cached run has zero. Upload-failure rows recorded by
earlier versions (a failed upload no longer fails its step) retain their transfer-attempt counts. `stats.recent_runs[].attempts` continues to report
attempts per materialization, with the stored final-attempt duration.

Experimental `--reconcile RUN_ID` is an explicit write after the operator verifies the run
stopped. It returns just that run in the existing envelope (`total: 1`, `truncated: false`),
recorded as `interrupted`. It refuses a proven live owner, a non-running row or unknown id
(exit 2); database errors are exit 3. `--limit` and `--all` conflict with it; `--fields`,
output selection and `--env` still apply. Successful step rows/artifacts and progress are
preserved, and an unknown end time/duration is not invented. Ordinary history stays read-only.

`runs[].triggered_by` says what started a run: `manual` (a command typed on a machine), `api`
(an HTTP request to `barca serve`), `schedule` (a cron tick) or `client` (a run a laptop posted
to a server's ledger); `null` for runs recorded before barca recorded it. A scheduled run also
has `scheduled_at` (the tick, RFC 3339 in UTC) and `schedule_id` (the scheduled node); both are
`null` on every other run. The three keys are additive. `runs[].client_run_id` is the run id
a `client` run had on the laptop that posted it (`barca docs server-mode`), `null` on every other
run; additive and experimental.


<!-- BEGIN GENERATED schema history -->
| Key | Type | Present |
|---|---|---|
| `hint` | string | always |
| `runs` | array | always |
| `runs[]` | object | always |
| `runs[].attempts` | integer | always |
| `runs[].client_run_id` | null | always |
| `runs[].command` | string | always |
| `runs[].elapsed_seconds` | number | always |
| `runs[].files` | array | always |
| `runs[].files[]` | string | always |
| `runs[].finished_at` | string | always |
| `runs[].run_id` | string | always |
| `runs[].schedule_id` | null | always |
| `runs[].scheduled_at` | null | always |
| `runs[].started_at` | string | always |
| `runs[].status` | string | always |
| `runs[].steps_cached` | integer | always |
| `runs[].steps_executed` | integer | always |
| `runs[].steps_total` | integer | always |
| `runs[].target` | string | always |
| `runs[].triggered_by` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema history -->

Newest first. `files` is an array of the `.py` files the run was given. `target`,
`steps_total`, `finished_at` and `elapsed_seconds` can be `null` (no target; a run still in
progress or interrupted). `status` is `running`, `success`, `failed`, `cancelled` or
`interrupted`: local history records completed steps during execution, after confirmed upload
for remote results. New committed shared progress is published every minute in healthy
operation, followed by the final outcome at run end. A `running` run counts the steps
it has recorded so far in `steps_executed`, and
`interrupted` is a run whose process died without recording an outcome (`barca docs cache`,
"While a run is going, and after one is killed").

Terminal status/counts and successful/failed step rows commit in one transaction. A required
write failure reports the existing infrastructure error (exit 3) and preserves previous
history/progress and run state; captured output is written separately afterward.

Reconciled history (same envelope; unknown end timestamp and duration remain null):

<!-- BEGIN GENERATED schema history_reconciled -->
| Key | Type | Present |
|---|---|---|
| `runs` | array | always |
| `runs[]` | object | always |
| `runs[].attempts` | integer | always |
| `runs[].client_run_id` | null | always |
| `runs[].command` | string | always |
| `runs[].elapsed_seconds` | null | always |
| `runs[].files` | array | always |
| `runs[].finished_at` | null | always |
| `runs[].run_id` | string | always |
| `runs[].schedule_id` | null | always |
| `runs[].scheduled_at` | null | always |
| `runs[].started_at` | string | always |
| `runs[].status` | string | always |
| `runs[].steps_cached` | integer | always |
| `runs[].steps_executed` | integer | always |
| `runs[].steps_total` | integer | always |
| `runs[].target` | string | always |
| `runs[].triggered_by` | null | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema history_reconciled -->

### stats

<!-- BEGIN GENERATED schema stats -->
| Key | Type | Present |
|---|---|---|
| `avg_elapsed_seconds` | number | always |
| `cache_hit_rate` | number | always |
| `id` | string | always |
| `max_elapsed_seconds` | number | always |
| `median_elapsed_seconds` | number | always |
| `p95_elapsed_seconds` | number | always |
| `recent_runs` | array | always |
| `recent_runs[]` | object | always |
| `recent_runs[].attempts` | integer | always |
| `recent_runs[].created_at` | string | always |
| `recent_runs[].elapsed_seconds` | number | always |
| `recent_runs[].error_message` | null | always |
| `recent_runs[].status` | string | always |
| `total_runs` | integer | always |
<!-- END GENERATED schema stats -->

`id` is the node id (`file.py:name`), the same key every other command uses. The timing fields
are `null` when the asset never ran; `recent_runs[].error_message` is a string for failed runs.
`total_runs` counts executions (recorded materializations). Despite its name, `cache_hit_rate` is
not a cache hit rate: it is the share of those executions that repeated an earlier execution's
run hash, i.e. ran again with unchanged code and inputs (`--refresh`, a retry after a failure, an
evicted result). A cache hit executes nothing and is not recorded per asset; `barca history`
counts cache hits per run (`steps_cached`). The human report labels it "Re-executed with
unchanged inputs". Until 0.27.0 it counted every execution whose run hash appeared more than once,
the first included.

### gc (experimental)

<!-- BEGIN GENERATED schema gc_dry_run -->
| Key | Type | Present |
|---|---|---|
| `after_bytes` | integer | always |
| `dry_run` | boolean | always |
| `evicted` | array | always |
| `evicted[]` | object | always |
| `evicted[].bytes` | integer | always |
| `evicted[].node` | string | sometimes |
| `evicted[].path` | string | always |
| `evicted[].tier` | string | always |
| `evicted_bytes` | integer | always |
| `evicted_files` | integer | always |
| `failed` | array | always |
| `limit_bytes` | integer | always |
| `restricted` | null | always |
| `size_bytes` | integer | always |
| `target_bytes` | integer | always |
| `tiers` | array | always |
| `tiers[]` | object | always |
| `tiers[].bytes` | integer | always |
| `tiers[].files` | integer | always |
| `tiers[].tier` | string | always |
<!-- END GENERATED schema gc_dry_run -->

- `size_bytes` is `.barca/` before, `after_bytes` after (predicted, with `--dry-run`).
  `limit_bytes` and `target_bytes` (80% of the limit) are `null` with the limit `off`.
- `evicted[]` lists every entry in eviction order: `tier` is `leftovers`, `superseded`,
  `store_held` or `current`; `path` is relative to the project root; `node` is set for a result.
  `tiers[]` sums them per tier, in the same order. `failed[]` lists what could not be deleted
  (`path`, `error`); those are skipped, and the exit code is still 0.
- `restricted` is `null`, or why only leftovers and superseded results were considered (a run in
  progress on this `.barca/`).
- `dry_run` says whether anything was deleted. `--dry-run` lists exactly what `barca gc` then
  deletes, unless something changes in between.

### get and run: over the cache size limit (experimental)

<!-- BEGIN GENERATED schema get_cache_evicted -->
| Key | Type | Present |
|---|---|---|
| `cache` | object | always |
| `cache.evicted_bytes` | integer | always |
| `cache.evicted_files` | integer | always |
| `cache.limit_bytes` | integer | always |
| `cache.size_bytes` | integer | always |
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_cache_evicted -->

`cache` appears only when the check after the run found `.barca/` over its size limit; its sizes
are after eviction (`barca docs cache`, "Disk use").


### Project star-import discovery

A later project star import preserves imported Barca bindings absent from its single literal
`__all__` list/tuple with no other references to `__all__`. Colliding local names lose recognition; unknown, computed, mutated and
reflective export sets keep the conservative fallback. Source inspection does not import
user modules. This changes discovery and argument validation, with no new flags or JSON keys.

### plan (experimental)

Every plan includes `plan_version: 1`, independent of the package version, worker protocol,
and metadata database schema. Consumers of persisted plans must reject unsupported plan
versions; a missing field identifies a legacy, unversioned plan.

`phases[].reason` is `{"type": "initial"}` for the first phase or `{"type": "fan_in", "node_id":
"<id>"}` for a phase that waits on a node gathering several upstream results. `plan` reads no
state and takes no `--env`.

The phase/stream layout uses the same pool as execution: available cores, or a positive
`BARCA_POOL_SIZE`.

`warnings` is the plan-warnings array described under "Plan warnings" above.

<!-- BEGIN GENERATED schema plan -->
| Key | Type | Present |
|---|---|---|
| `phases` | array | always |
| `phases[]` | object | always |
| `phases[].reason` | object | always |
| `phases[].reason.node_id` | string | sometimes |
| `phases[].reason.type` | string | always |
| `phases[].streams` | array | always |
| `phases[].streams[]` | object | always |
| `phases[].streams[].steps` | array | always |
| `phases[].streams[].steps[]` | string | always |
| `phases[].streams[].stream_id` | string | always |
| `plan_version` | integer | always |
| `total_steps` | integer | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
<!-- END GENERATED schema plan -->

### docs

`barca docs --json` (and `--all --json`, where each topic also has `content`):

<!-- BEGIN GENERATED schema docs_index -->
| Key | Type | Present |
|---|---|---|
| `topics` | array | always |
| `topics[]` | object | always |
| `topics[].name` | string | always |
| `topics[].summary` | string | always |
<!-- END GENERATED schema docs_index -->

`barca docs <topic> --json`:

<!-- BEGIN GENERATED schema docs_topic -->
| Key | Type | Present |
|---|---|---|
| `content` | string | always |
| `name` | string | always |
| `summary` | string | always |
<!-- END GENERATED schema docs_topic -->

Topic names and contents are documentation, not contract.

## Project root (stable)

Every command except `docs` and `version` runs from the project root: the nearest directory at or
above the cwd holding `barca.toml`, else the cwd. File arguments are read relative to the cwd
they were typed in and rewritten relative to the root, so node ids, `.barca/` and the working
directory of steps do not depend on where barca was invoked. The stderr line
`barca: project root: <path> ...`, printed when the root is not the cwd, is informational and not
contract.

## stderr

stderr carries progress, warnings, your steps' own `print` output and errors. Only the error
envelope and the `--agent` lines below are contract. Other `[barca] ...` lines, such as
`[barca] backed up the metadata DB ...` and `[barca] restored ...` (`barca docs remote`), are
informational and not contract. The same goes for the wording of an error: a backup that fails
its check in `barca state restore` is exit 2 with `kind` `usage`, and the text that names it and
the next newest backup is for people.

### Error envelope (stable)

In JSON mode every error is one JSON object on stderr. A failed run follows it with one
summary line; read the last JSON object with `kind` and `error`. The same envelope comes
from argument errors, engine errors and failed steps:

<!-- BEGIN GENERATED schema error_usage -->
| Key | Type | Present |
|---|---|---|
| `code` | integer | always |
| `error` | string | always |
| `kind` | string | always |
| `remediation` | string | always |
<!-- END GENERATED schema error_usage -->

An argument the parser rejects (here `--jsn`) gives the same shape:

<!-- BEGIN GENERATED schema error_usage_parse -->
| Key | Type | Present |
|---|---|---|
| `code` | integer | always |
| `error` | string | always |
| `kind` | string | always |
| `remediation` | string | always |
<!-- END GENERATED schema error_usage_parse -->

`step_failed` adds `node`, `traceback` (your code's frames, or `null`) and `artifact_dir`:

<!-- BEGIN GENERATED schema error_step_failed -->
| Key | Type | Present |
|---|---|---|
| `artifact_dir` | string | always |
| `code` | integer | always |
| `error` | string | always |
| `kind` | string | always |
| `node` | string | always |
| `remediation` | string | always |
| `traceback` | string | always |
<!-- END GENERATED schema error_step_failed -->

`code` equals the exit code. `remediation` can be `null`. In human mode the same error is prose
with the remediation before the final run summary.

### --agent lines

With `--agent`, `get` and `run` print plain progress lines on stderr instead of a progress bar.
These are the lines the fixture run produced, with timings and counters replaced by
placeholders:

<!-- BEGIN GENERATED agent-lines -->
```
[barca] <n>/<total> steps | done in <secs>s
[barca] <n>/<total> steps | failed in <secs>s
[barca] checking artifact store <store>/default/artifacts (timeout <secs>s)
[barca] env default (built-in default); artifact store <store>/default/artifacts
[barca] importing <module>
[barca] run failed: step 'pipeline.py:broken' failed: ValueError: contract fixture failure; 0 downstream steps skipped (exit 1)
[barca] step:pipeline.py:broken failed: ValueError: contract fixture failure
[barca] step:pipeline.py:keys completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:numbers cached
[barca] step:pipeline.py:per_key[k=a] completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:per_key[k=b] completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:report completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:total cached env CONTRACT_API_TOKEN=<unset> CONTRACT_REGION=eu
[barca] step:pipeline.py:value completed <secs>s (<n>/<total>) attempts=2
[barca] uploaded <n> pending results
[barca] uploading <n> artifacts to <store>/default/artifacts (timeout <secs>s per attempt); waiting for confirmation
[barca] uploading <n> pending results to <store>/default/artifacts from an earlier run
[barca] warning: pipeline.py:report never uses its input `rows`. It is still loaded in full each time the step runs, and it counts toward the step's cache key. Use it, remove it from inputs=, or rename the parameter `_rows` if it is there for ordering only (a `_` input is not loaded and never flagged)
```
<!-- END GENERATED agent-lines -->

| Line | When | Stability |
|---|---|---|
| `[barca] step:<id> completed <secs>s (<n>/<total>)[ env NAME=VALUE ...]` | a step finished; `<id>` includes `[key=value]` for a partition | stable |
| `[barca] step:<id> cached[ env NAME=VALUE ...]` | a step was served from cache. Printed when the step is decided, or, when its artifact is not on disk, at the end of the run once it is settled that nothing needed it (if something did, the step prints `completed` or `failed` instead, never both). Exception: a result in a remote artifact store whose object proves to be deleted when it is fetched has already printed `cached`; a `[barca] warning: <id>: the artifact of its cached result is missing ...` line and then `completed` or `failed` for the same step follow | stable |
| `[barca] step:<id> failed: <first line of the error>` | a step raised | stable |
| `[barca] run failed: step '<id>' failed: <first error line>; <n> downstream steps skipped (exit <code>)` | last stderr line of a failed run, after the error envelope (every mode) | stable |
| `[barca] <n>/<total> steps \| done in <secs>s` | end of a run that executed steps, with or without `--agent`; `failed in` when a step failed, `cancelled after` on a Ctrl-C that arrives while steps are running (never `done` then). The line is about the steps: with an artifact store, a Ctrl-C after the last step finished (while artifacts upload or the history is backed up) follows a `done` line, and the exit code (130) and the error envelope still say `cancelled` (`barca docs remote`, "Ctrl-C") | stable |
| `[barca] env <env> (<source>); artifact store <store>` | once at startup of `get`, `run` and `serve` (every mode) when the environment comes from `default_env` in `barca.toml` or a separate artifact store is configured. `<source>` is `--env`, `BARCA_ENV`, `default_env in barca.toml` or `built-in default`; `<store>` is the store URI with credentials redacted, or `local (<dir>)` | experimental |
| `[barca] checking artifact store <store> (timeout <secs>s)` | before workers start whenever a separate artifact store is configured, including state-off runs; requires bucket/container-root listing and backend existence access, verifies reachability rather than write permission | experimental |
| `[barca] uploading <n> artifacts to <store> (timeout <secs>s per attempt); waiting for confirmation` | before awaiting queued artifact uploads (every mode) | experimental |
| `[barca] uploading <n> pending results to <store> from an earlier run` | before workers start, when results an earlier run could not upload are retried (every mode); followed by `[barca] uploaded <n> pending results` or `[barca] <n> pending results still not uploaded (<k> uploaded): <why>` | experimental |
| `[barca] still waiting for artifact uploads to <store> (<secs>s)` | every 10 seconds while the upload drain waits (every mode); destinations omit URI passwords and signed query parameters | experimental |
| `[barca] progress checkpoint failed; recorded results remain local: <cause>` | a mid-run shared-state publication failed; committed local results are retained and retried at the next minute tick, without changing successful user work into a failed step | experimental |
| `[barca] still running (<n>s): <id>` | a step in flight for `BARCA_PROGRESS_SECS` (every mode) | experimental |
| `[barca] <n> more: <first line of a warning>` | before the end-of-run line (every mode): a `logging` WARNING printed because logging is unconfigured, or a `warnings` warning, from a step's process was printed once and suppressed `<n>` more times in this run; beyond ten texts, one `[barca] <n> more: <k> other repeated warnings` (`barca docs agents`) | experimental |
| `[barca] server <url> (<source>)` | once when a command uses the configured server (`BARCA_SERVER` or `[server] url in barca.toml`, every mode): a local `get`/`run` reads and posts to its ledger, `status`/`history`/`stats` and a bare `--server` are answered by it (`barca docs server-mode`) | experimental |
| `[barca] recorded run <id> on the server (run <server id>)` | a run this machine could not post before was accepted by the server's ledger, before the command's own lookup | experimental |
| `[barca] warning: run <id> was not recorded on the server <url>: <why>; it stays on this machine (unsynced) and ...`, `[barca] warning: run <id> is still not recorded on the server <url>: <why>` | the server refused a run's post (`409`) or could not be reached after the run; the run's result and exit code are unchanged | experimental |
| `[barca] skipped N task(s) ...`, `[barca] nothing to get ...` | `get` with no target skipped tasks | experimental |
| `[barca] cache <size> over its <limit> limit; evicted <n> files (<size>): <n> <tier>, ...` | after `get`, `run` or a `serve` run, when `.barca/` was over `[cache] max_size` and something was evicted (every mode; `barca docs cache`, "Disk use") | experimental |
| `[barca] <id>: its cached result was evicted to stay under the cache limit; computing it again` | a step whose cached result the size limit evicted is computed again because something reads it (every mode) | experimental |
| `[barca] warning: ...`, `[barca] SINK FAILED: ...` | warnings (always this lowercase prefix; the text after it may change) and failed sinks. A plan warning (see "Plan warnings") is printed before the first `step:` line, once per run, with or without `--agent`; read it from the JSON `warnings` array rather than from this line | experimental |

Failed runs keep the complete `steps` fan-out and `failed_node` in JSON. Human output
reports blocked downstream nodes as one count in the final line. This count is per node,
including a partitioned node once. A multi-target failure uses its first failed step for
the summary and counts all blocked downstream nodes; detailed outcomes remain in JSON.
The change to the stable `run failed:` line and its position ships in the next minor release.

`env` values: `<unset>` for an unset variable, `<redacted>` for secret-looking names (`*_TOKEN`,
`*_SECRET`, `*_KEY`, `*_PASSWORD`), double quotes around values with spaces. With several targets
the first failed step is summarized once; each target's error remains in JSON.

## Changing the surface

A pull request that changes a command, flag, environment variable, exit code, JSON output, the
error envelope or an `--agent` line updates, in the same PR:

1. the snapshots, with one command from the repository root (it rebuilds barca first):

   ```
   scripts/update-cli-snapshots.sh
   ```

   This runs `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca` (help snapshots in
   `crates/barca-cli/snapshots/help/`, the command, argument and exit-code tables here) and
   `BARCA_UPDATE_SNAPSHOTS=1 pytest python/tests/test_cli_contract.py` (JSON schemas in
   `python/tests/snapshots/cli_contract/` and the schema tables here), then copies this page to
   the site (`site/src/content/docs/reference/cli-contract.md`). Review the diff: it is the
   change to the contract.
2. this page's hand-written parts (stability, the experimental table, notes), and the
   classification in `EXPERIMENTAL` in `crates/barca-cli/src/contract.rs` for a new experimental
   flag or command;
3. pre-1.0, for a breaking change: plan a minor bump and add a "Breaking" line to the release
   notes. From 1.0: keep the old spelling working for at least one minor release, with a stderr
   warning.

See also: `barca docs agents` (how to drive barca from scripts and agents), `barca docs skill`.

### Static decorator bindings

Qualified and imported aliases of Barca decorators/helpers receive the same node
semantics and argument validation as direct imports. Explicit foreign or rebound
names do not define Barca nodes. For a foreign wrapper intended to remain a node,
stack it with a genuine Barca decorator; see `barca docs assets`.

`list --groups` is experimental: JSON returns `{groups: [{id, name, description, members, output}]}`. IDs and member/output references are strings; members is an array of strings. The default `list` schema is unchanged. Groups are organizational metadata and cannot be execution targets.

### Recompute byte checks

Successful materializations record SHA-256 digests. A different digest for the same node
and run hash emits `[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic`
and adds `steps[].bytes_changed: true` (otherwise absent). Partitioned reports aggregate
that flag, and stderr identifies each changed key. This warning does not change exit codes.
Status nodes optionally expose `last_bytes_change: {node_id, run_hash, content_hash, created_at}`,
retained after later unchanged recomputes; latest materializations expose `content_hash`
when known. These additive keys are stable. See `barca docs cache` and
`specs/materialization-digests.md`. A `barca get` that re-uploads a `mismatch` target (no step
runs) prints the same warning and sets the same flag. Server-post semantics remain proposed.

#### Recomputed output with changed bytes

<!-- BEGIN GENERATED schema get_bytes_changed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].bytes_changed` | boolean | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_bytes_changed -->

#### Status retaining the last byte change

<!-- BEGIN GENERATED schema status_bytes_changed -->
| Key | Type | Present |
|---|---|---|
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.artifact` | string | always |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | always |
| `nodes[].cache.state` | string | always |
| `nodes[].env` | array | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].kind` | string | always |
| `nodes[].last_bytes_change` | object | always |
| `nodes[].last_bytes_change.content_hash` | string | always |
| `nodes[].last_bytes_change.created_at` | string | always |
| `nodes[].last_bytes_change.node_id` | string | always |
| `nodes[].last_bytes_change.run_hash` | string | always |
| `nodes[].last_materialization` | object | always |
| `nodes[].last_materialization.artifact` | string | always |
| `nodes[].last_materialization.content_hash` | string | always |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | number | always |
| `nodes[].last_materialization.format` | string | always |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | integer | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].name` | string | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].shape` | object | always |
| `nodes[].shape.type` | string | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_bytes_changed -->

### CSV artifact format

An artifact reference may have `format: "csv"` and a `.csv` path when explicitly selected
with `serializer="csv"`. CSV sink extensions select CSV automatically. Frames continue
to default to Parquet. CSV requires a supported tabular value; readers infer types.
Status samples represent CSV cells as strings, and SQL views support CSV artifacts.
See `barca docs types` and `specs/artifact-formats.md`.

### Task stdout and logs

`logs RUN_ID` is stable. JSON returns an array of `{node_id: string, line: string}` objects
in recorded order; text returns only the lines. `--step` selects an exact id/base id/function
name; ambiguous or absent steps and missing runs exit 2. Silent recorded steps return no
lines. `--env` selects local history; logs does not import source or contact a remote store.
Task return values remain `final_output` as for assets. Get/run JSON mode suppresses captured
stdout lines; non-JSON modes prefix them with `[node_id]` on stderr. Agent framework lines
remain `[barca]`-prefixed. Retrieve captured output with `barca logs`. See `barca docs logs`
and `specs/task-output.md` for flush, failure and ordering boundaries.

<!-- BEGIN GENERATED schema logs -->
| Key | Type | Present |
|---|---|---|
| `[]` | object | always |
| `[].line` | string | always |
| `[].node_id` | string | always |
<!-- END GENERATED schema logs -->

### Execution timing and imports

Under `--agent` (and in runs `barca serve` starts), import progress uses
`[barca] importing <module>` before a module is loaded; without `--agent` the line is not printed,
and `import_seconds` in the run JSON reports a slow import. Completion
and permanent failure step lines append `attempts=N` only when N > 1; their existing
prefixes and completion durations remain. Timeout messages identify the step, rather
than the internal callable wrapper.

Older read-only snapshots without attempt metadata report a null history attempt total.

### Doctor report (experimental)

<!-- BEGIN GENERATED schema doctor -->
| Key | Type | Present |
|---|---|---|
| `checks` | array | always |
| `checks[]` | object | always |
| `checks[].data` | object | always |
| `checks[].data.artifacts` | string | sometimes |
| `checks[].data.available_bytes` | integer | sometimes |
| `checks[].data.barca_version` | string | sometimes |
| `checks[].data.enabled` | boolean | sometimes |
| `checks[].data.executable` | string | sometimes |
| `checks[].data.file` | string | sometimes |
| `checks[].data.files` | array | sometimes |
| `checks[].data.files[]` | string | always |
| `checks[].data.filesystem_bytes` | integer | sometimes |
| `checks[].data.found` | boolean | sometimes |
| `checks[].data.limit_bytes` | integer | sometimes |
| `checks[].data.local_backups` | string | sometimes |
| `checks[].data.max_size` | string | sometimes |
| `checks[].data.metadata` | string | sometimes |
| `checks[].data.mode` | string | sometimes |
| `checks[].data.name` | string | sometimes |
| `checks[].data.nodes` | integer | sometimes |
| `checks[].data.path` | string | sometimes |
| `checks[].data.size_bytes` | integer | sometimes |
| `checks[].data.source` | string | sometimes |
| `checks[].data.uri` | null | sometimes |
| `checks[].data.version` | string | sometimes |
| `checks[].detail` | string | always |
| `checks[].name` | string | always |
| `checks[].ok` | boolean | always |
| `ok` | boolean | always |
<!-- END GENERATED schema doctor -->
