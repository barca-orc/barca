---
title: CLI Contract
description: Every barca command, flag, environment variable, exit code and JSON schema, marked stable or experimental, and the policy for changing them.
---

<!-- Generated from crates/barca-cli/docs/contract.md (barca docs contract) by scripts/update-cli-snapshots.sh. Edit that file, not this one. -->

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
| `barca plan` and its JSON | prints the planner's internal phase/stream layout, which changes with scheduling work (`reason` is an object, `{"type": "initial"}` or `{"type": "fan_in", "node_id": ...}`) |
| `barca serve` and all its flags | the HTTP API and scheduler are young: no auth, no shared remote state, routes may change. Its JSON is the engine's own serialization (for example `GET /assets` has `freshness: {"type": "Always"}` and `stats.node_id`), not the CLI's |
| `get -o/--output`, `run -o/--output` | kept for compatibility; `--json` / `--pretty` are the canonical spelling |
| `get --no-cache`, `run --no-cache` | deprecated (hidden): the old spelling of `--refresh-all`. Still works, prints `[barca] warning: --no-cache is deprecated ...` on stderr, and will be removed in a future minor release |
| `status --sample` and `nodes[].shape` | read by a Python helper (`barca._inspect`) whose output may grow per format |
| `BARCA_PROGRESS_SECS`, `BARCA_POOL_SIZE`, `BARCA_COMM_COST_SECONDS`, `BARCA_TRACE_TIMING` | tuning and benchmarking knobs |
| `BARCA_ARTIFACT_URI` | 0.4.0 back-compat override, superseded by `BARCA_REMOTE_URI` / `[remote].artifacts_uri` |
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
| `barca get` | `<ARGS>...` | stable | Get asset value(s) — cache-aware, runs only the needed subgraph |
| `barca run` | `<ARGS>...` | stable | Run a task and its dependency cone — the task always re-runs |
| `barca plan` | `<FILES>...` | experimental: prints the planner's internal phase/stream layout, which changes with scheduling work | Parse source files and emit the execution plan as JSON |
| `barca history` | - | stable | Show recent run history |
| `barca stats` | `<TARGET> <FILES>...` | stable | Show execution statistics for an asset |
| `barca serve` | `<FILES>...` | experimental: the HTTP API and scheduler are young: no auth, no shared remote state, routes may change | Run a long-running HTTP server exposing the orchestrator as a JSON API |
| `barca list` | `<FILES>...` | stable | List all discovered definitions (assets, tasks, sensors) with their deps |
| `barca status` | `<ARGS>...` | stable | Show every node's cache state, last materialization and artifact shape (read-only) |
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
  ``Run `barca list <files>` to see available assets and tasks.``
- barca never offers fuzzy suggestions: no "did you mean" for targets or `barca docs` topics (an
  unknown topic lists every valid topic), and argument errors carry no "a similar argument
  exists" tip. A guess can read as confirmation.
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
| `<ARGS>...` | - | required | stable | [TARGET[,TARGET...]] file.py [file.py ...] — target is optional |
| `-o, --output` | `json\|value\|pretty` | - | experimental: kept for compatibility; --json / --pretty are the canonical spelling | Output format (kept for compatibility; --json / --pretty are the canonical spelling) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--refresh` | comma-separated names | - | stable | Assets to force re-materialize, as ONE comma-separated list (`--refresh a,b`, not `--refresh a b`); the target itself may be named. Every asset downstream of them in the target's cone re-materializes too (see --no-cascade) |
| `--no-cascade` | - | default `false` | stable | With --refresh: re-materialize only the named assets, not what is downstream of them. Cached downstream assets then do not reflect the refresh; barca warns |
| `--refresh-all` | - | default `false` | stable | Force re-materialize EVERY asset in the target's cone (nothing comes from cache) |
| `--no-cache` | - | default `false`; hidden from `--help` | experimental: deprecated: the old spelling of --refresh-all; warns on stderr and will be removed | Deprecated spelling of --refresh-all (prints a warning; removed in a future minor) |
| `--dry-run` | - | default `false` | stable | Show what this command would do (each step cached or will-run, and why) without running or writing anything |
| `--agent` | - | default `false` | stable | Agent-friendly output: plain structured progress lines instead of visual progress bar |
| `--fields` | comma-separated: `id`, `kind`, `action`, `status`, `reason`, `detail`, `run_hash`, `artifact`, `warning`, `partitions`, `env` | - | stable | Keep only these keys (comma-separated) on each entry of `steps` in the JSON output. Not valid with -o value/pretty. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |

#### barca run

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ARGS>...` | - | required | stable | TARGET[,TARGET...] file.py [file.py ...] — one or more target tasks, comma-separated |
| `--refresh` | comma-separated names | - | stable | Upstream assets to force re-materialize, as ONE comma-separated list (`--refresh a,b`, not `--refresh a b`). Every asset downstream of them in the task's cone re-materializes too (see --no-cascade) |
| `--no-cascade` | - | default `false` | stable | With --refresh: re-materialize only the named assets, not what is downstream of them. Cached downstream assets then do not reflect the refresh; barca warns |
| `--refresh-all` | - | default `false` | stable | Force re-materialize EVERY asset in the task's cone (nothing comes from cache) |
| `--no-cache` | - | default `false`; hidden from `--help` | experimental: deprecated: the old spelling of --refresh-all; warns on stderr and will be removed | Deprecated spelling of --refresh-all (prints a warning; removed in a future minor) |
| `--dry-run` | - | default `false` | stable | Show what this command would do (each step cached or will-run, and why) without running or writing anything |
| `-o, --output` | `json\|value\|pretty` | - | experimental: kept for compatibility; --json / --pretty are the canonical spelling | Output format (kept for compatibility; --json / --pretty are the canonical spelling) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--agent` | - | default `false` | stable | Agent-friendly output: plain structured progress lines instead of visual progress bar |
| `--fields` | comma-separated: `id`, `kind`, `action`, `status`, `reason`, `detail`, `run_hash`, `artifact`, `warning`, `partitions`, `env` | - | stable | Keep only these keys (comma-separated) on each entry of `steps` in the JSON output. Not valid with -o value/pretty. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |

#### barca plan

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | required | experimental (with the command) | Python source files containing @asset definitions |

#### barca history

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `-l, --limit` | `LIMIT` | default `10` | stable | Number of recent runs to show |
| `--all` | - | default `false` | stable | Show every recorded run (no limit) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--fields` | comma-separated: `run_id`, `command`, `files`, `target`, `status`, `steps_total`, `steps_executed`, `steps_cached`, `started_at`, `finished_at`, `elapsed_seconds` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `runs`. Implies --json. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |

#### barca stats

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TARGET>` | - | required | stable | Target asset function name |
| `<FILES>...` | - | required | stable | Python source files containing @asset definitions |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--fields` | comma-separated: `elapsed_seconds`, `status`, `created_at`, `error_message`, `attempts` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `recent_runs`. Implies --json. An unknown key is a usage error listing the valid ones |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |

#### barca serve

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | required | experimental (with the command) | Python source files defining the DAG to serve |
| `-p, --port` | `PORT` | default `8274` | experimental (with the command) | Port to bind on |
| `--watch` | - | default `false` | experimental (with the command) | Dev mode: re-parse the DAG when source files change |
| `--no-schedule` | - | default `false` | experimental (with the command) | Disable the cron scheduler (Schedule(...) assets will not auto-fire) |
| `--timezone` | `TIMEZONE` | default `local` | experimental (with the command) | Timezone for cron evaluation: local (default), utc, or an IANA name |
| `--env` | `ENV` | - | experimental (with the command) | Environment name (separates cache/state per environment) |

#### barca list

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | required | stable | Python source files containing definitions |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `-l, --limit` | `LIMIT` | default `100` | stable | Maximum number of nodes to show, in topological order |
| `--all` | - | default `false` | stable | Show every node (no limit) |
| `--fields` | comma-separated: `id`, `kind`, `freshness`, `schedule`, `inputs`, `env`, `next_fire` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `nodes`. Implies --json. An unknown key is a usage error listing the valid ones |

#### barca status

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ARGS>...` | - | required | stable | [TARGET[,TARGET...]] file.py [file.py ...] — target is optional |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `-l, --limit` | `LIMIT` | default `100` | stable | Maximum number of nodes to show, in topological order |
| `--all` | - | default `false` | stable | Show every node (no limit) |
| `--fields` | comma-separated: `id`, `name`, `kind`, `inputs`, `partitioned`, `cache`, `partitions`, `last_materialization`, `shape`, `env` | - | stable | Output JSON with only these keys (comma-separated) on each entry of `nodes`. Implies --json. An unknown key is a usage error listing the valid ones |
| `--sample` | `N` | - | experimental: sample rows come from a Python helper whose output may grow | Include up to N sample rows from each json/parquet artifact (off by default; pickles are never sampled) |
| `--env` | `ENV` | - | stable | Environment name (separates cache/state per environment) |

#### barca docs

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TOPIC>` | - | - | stable | Topic to show (omit for the index), e.g. types, cache, examples/duckdb |
| `--all` | - | default `false` | stable | Print every topic in one stream |
| `--json` | - | default `false` | stable | Emit JSON instead of markdown |
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

`get`, `run`, `list`, `status`, `history` and `stats` choose JSON or human output on stdout by
one rule, first match wins:

1. a flag: `--json` or `--pretty` (on `get`/`run` also `-o json|value|pretty`); `--fields`
   implies JSON and is a usage error with `--pretty`, `-o pretty` or `-o value`;
2. `BARCA_OUTPUT=json|pretty`;
3. the terminal: stdout is a TTY, human output; anything else, JSON.

`plan` always prints JSON. `docs` prints markdown unless `--json` or `--fields` is given. Errors
follow the same rule: in JSON mode they are the envelope on stderr (see Errors).

## Environment variables

| Variable | Effect | Stability |
|---|---|---|
| `BARCA_OUTPUT` | `json` or `pretty`: output format when no flag is given; any other value is a usage error (exit 2) | stable |
| `BARCA_ENV` | environment name when `--env` is not given (beats `default_env` in `barca.toml`) | stable |
| `BARCA_REMOTE_URI` | remote root for artifacts and shared state (`[remote].uri`) | stable |
| `BARCA_STATE_URI` | shared metadata DB location (`[remote].state_uri`) | stable |
| `BARCA_STATE` | `optimistic` or `off` (`[remote].state`); any other value is a usage error | stable |
| `BARCA_PUSH_RETRIES` | integer: retries when pushing shared state (`[remote].push_retries`, default 5) | stable |
| `BARCA_STORAGE_OPTIONS` | JSON object keyed by protocol, merged over `[remote.storage_options.*]` | stable |
| `BARCA_ARTIFACT_URI` | literal artifact root, bypassing the environment prefix (warns with a non-default env) | experimental |
| `BARCA_PROGRESS_SECS` | seconds between `still running` lines (default 15, `0` turns them off) | experimental |
| `BARCA_POOL_SIZE` | number of Python workers (default: available cores) | experimental |
| `BARCA_COMM_COST_SECONDS` | the scheduler's per-dispatch cost estimate | experimental |
| `BARCA_TRACE_TIMING` | when set, prints a timing waterfall on stderr | experimental |

barca sets `BARCA_SOCKET`, `BARCA_WORKER`, `BARCA_WORKER_ID` (and passes `BARCA_ARTIFACT_URI`,
`BARCA_STORAGE_OPTIONS`) for its own worker processes; these are internal, not part of the
contract. Variables your nodes declare with `@asset(env=[...])` are yours (`barca docs assets`).

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
task/asset misuse, a `.py` file that does not parse, a DAG that cannot be built (an input that
names no definition, a cycle, a partitioned asset in an unpartitioned asset's `inputs=` without
`collect()`, a `partitions_from()` the asset cannot mirror), invalid `--env` or `barca.toml`. `infra`:
barca or its environment failed (metadata DB, workers, remote state, I/O); retrying may help.
`cancelled`: interrupted with Ctrl-C.

## JSON output schemas

How to read the tables: a key path is dotted, `[]` is "each item of the array", `<name>` stands
for keys you chose (target names, environment variable names) and `<user value>` is the value
your function returned. Types are JSON types; `a | b` means either. `sometimes` means the key is
present on some objects only (see each section for when). Values in the tables come from one
fixture run, so a nullable key can show only the type it had there; the notes say which keys
can be `null`.

Every schema below is stable unless its section says otherwise.

### get and run: one target

`barca get <asset> files --json` and `barca run <task> files --json` print one JSON line.

<!-- BEGIN GENERATED schema get -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].detail` | string | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
<!-- END GENERATED schema get -->

- `status` is `success` (see the failed result line below for `failed`).
- `steps_executed` is 0 when everything came from cache. `final_output` is the target's value
  (with no target, the last asset's), or `null` for a task that returned nothing.
- `steps[]`: `status` is `ran`, `cached`, `partial` or `failed`; `reason` (why it ran) is one of
  `task`, `sensor`, `refresh`, `refresh_cascade`, `refresh_all`, `not_materialized`,
  `partitions_unknown`, `sensor_output_unknown`, with `detail` in words. (`no_cache` is gone:
  `--no-cache` now reports `refresh_all`.)
- `get` and `run` share one refresh vocabulary: `--refresh a,b` (cascading downstream),
  `--no-cascade`, `--refresh-all`. `artifact` appears on cached steps, `run_hash`
  on unpartitioned steps, `warning` on a cached step whose upstream was refreshed without
  cascading, `env` on nodes that declare `env=[...]` (`null` for an unset variable,
  `"<redacted>"` for secret-looking names).

For a parquet or pickle artifact, `final_output` is a pointer instead of the value:

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
| `steps[].detail` | string | sometimes |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
<!-- END GENERATED schema get_artifact_pointer -->

A partitioned step carries `partitions` (counted in keys; `will_run_keys` is capped at 20):

<!-- BEGIN GENERATED schema get_partitioned -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].detail` | string | always |
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
<!-- END GENERATED schema get_partitioned -->

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
| `steps[].detail` | string | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
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
| `steps[].detail` | string | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
<!-- END GENERATED schema run_failed -->

### get and run: several targets

With `a,b`, `final_output` is replaced by `targets`, keyed by target name in the order given.
A successful target has `final_output`; a failed one has `failed_node` (the step that raised:
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
| `targets.<name>.final_output` | `<user value>` \| object | always |
| `targets.<name>.final_output._barca_artifact` | object | sometimes |
| `targets.<name>.final_output._barca_artifact.format` | string | always |
| `targets.<name>.final_output._barca_artifact.path` | string | always |
| `targets.<name>.final_output._barca_artifact.size_bytes` | integer | always |
| `targets.<name>.status` | string | always |
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
| `steps[].detail` | string | sometimes |
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
<!-- END GENERATED schema run_multi_target_failed -->

### Dry run

`--dry-run` on `get` or `run` changes nothing and prints what would happen. `steps[].action` is
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
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema list -->

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
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema list_truncated -->

`nodes[].kind` is `asset`, `task` or `sensor`; `nodes[].freshness` is `always`, `manual` or
`schedule`, lowercase like `kind`. A scheduled node also has `schedule` (the cron expression) and
`next_fire` (string, local time). `list` reads no state, so it takes no `--env`.

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
| `nodes[].last_materialization` | object | always |
| `nodes[].last_materialization.artifact` | null \| string | always |
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
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status -->

- `cache.state` is `cached`, `stale`, `never_run`, `partial`, `unknown` or `always_runs`: the
  same snake_case spelling as the `summary` keys (the human table prints `never-run`);
  `cache.reason` is `materialized`, `changed`, `upstream_stale`, `failed`, `no_record`,
  `partitions_missing`, `partitions_unknown`, `sensor_output_unknown`, `task` or `sensor`.
  `cache.run_hash` and `cache.artifact` appear when known (`artifact` only when cached).
- `partitions` appears only on partitioned nodes. `last_materialization` is `null` when the node
  never ran; in it `partition` appears for a partitioned node and `error` for a failed run.
- `shape` is `null` unless the last materialization succeeded, and is experimental: `type`, and
  by format `rows`, `columns[]`, `keys`, `key_count`, `sample` (with `--sample`), or only `note`
  when it cannot be read.

### history

<!-- BEGIN GENERATED schema history -->
| Key | Type | Present |
|---|---|---|
| `hint` | string | always |
| `runs` | array | always |
| `runs[]` | object | always |
| `runs[].command` | string | always |
| `runs[].elapsed_seconds` | number | always |
| `runs[].files` | array | always |
| `runs[].files[]` | string | always |
| `runs[].finished_at` | string | always |
| `runs[].run_id` | string | always |
| `runs[].started_at` | string | always |
| `runs[].status` | string | always |
| `runs[].steps_cached` | integer | always |
| `runs[].steps_executed` | integer | always |
| `runs[].steps_total` | integer | always |
| `runs[].target` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema history -->

Newest first. `files` is an array of the `.py` files the run was given. `target`,
`steps_total`, `finished_at` and `elapsed_seconds` can be `null` (no target; a run still in
progress).

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

### plan (experimental)

`phases[].reason` is `{"type": "initial"}` for the first phase or `{"type": "fan_in", "node_id":
"<id>"}` for a phase that waits on a node gathering several upstream results. `plan` reads no
state and takes no `--env`.

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
| `total_steps` | integer | always |
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
envelope and the `--agent` lines below are contract.

### Error envelope (stable)

In JSON mode every error is one JSON object, the last line on stderr. The same envelope comes
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
with the remediation on the last lines.

### --agent lines

With `--agent`, `get` and `run` print plain progress lines on stderr instead of a progress bar.
These are the lines the fixture run produced, with timings and counters replaced by
placeholders:

<!-- BEGIN GENERATED agent-lines -->
```
[barca] <n>/<total> steps | done in <secs>s
[barca] <n>/<total> steps | failed in <secs>s
[barca] run failed: step 'pipeline.py:broken' failed (exit 1)
[barca] step:pipeline.py:broken failed: ValueError: contract fixture failure
[barca] step:pipeline.py:keys completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:numbers cached
[barca] step:pipeline.py:per_key[k=a] completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:per_key[k=b] completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:total cached env CONTRACT_API_TOKEN=<unset> CONTRACT_REGION=eu
```
<!-- END GENERATED agent-lines -->

| Line | When | Stability |
|---|---|---|
| `[barca] step:<id> completed <secs>s (<n>/<total>)[ env NAME=VALUE ...]` | a step finished; `<id>` includes `[key=value]` for a partition | stable |
| `[barca] step:<id> cached[ env NAME=VALUE ...]` | a step was served from cache | stable |
| `[barca] step:<id> failed: <first line of the error>` | a step raised | stable |
| `[barca] run failed: step '<id>' failed (exit <code>)` | just before the error envelope of a failed step (every mode) | stable |
| `[barca] <n>/<total> steps \| done in <secs>s` | end of a run that executed steps, with or without `--agent`; `failed in` when a step failed, `cancelled after` on Ctrl-C (never `done` then) | stable |
| `[barca] still running (<n>s): <id>` | a step in flight for `BARCA_PROGRESS_SECS` (every mode) | experimental |
| `[barca] skipped N task(s) ...`, `[barca] nothing to get ...` | `get` with no target skipped tasks | experimental |
| `[barca] warning: ...`, `[barca] SINK FAILED: ...` | warnings (always this lowercase prefix; the text after it may change) and failed sinks | experimental |

`env` values: `<unset>` for an unset variable, `<redacted>` for secret-looking names (`*_TOKEN`,
`*_SECRET`, `*_KEY`, `*_PASSWORD`), double quotes around values with spaces. With several targets
each failed target also gets an `error: target '<name>' failed ...` line before the envelope.

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
