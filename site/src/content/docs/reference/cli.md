---
title: CLI Reference
description: All barca CLI commands — get, run, plan, history, stats, serve, list, status, version.
---

The `barca` binary is the entry point. Once installed (e.g. `uv add barca`), the `barca` command
is on your PATH.

The [CLI contract](/reference/cli-contract/) lists every command, flag, environment variable, exit
code and JSON output schema, marked stable or experimental, with the policy for changing them.

## Commands

```
barca get [target[,target...]] [file.py|dir/ ...] [--refresh a,b [--no-cascade] | --refresh-all]
                                               Get asset value(s) — cache-aware
barca run <task[,task...]> [file.py|dir/ ...] [--refresh a,b [--no-cascade] | --refresh-all]  Run task(s) (always re-run)
barca plan [file.py|dir/ ...]                Emit the execution plan as JSON (experimental)
barca history [-l N | --all] [--json|--pretty]  Show recent run history
barca stats <target> [file.py|dir/ ...]       Show timing/cache stats for an asset
barca serve [file.py|dir/ ...] [--port N] [--watch] [--no-schedule] [--timezone TZ]
                                               Run the HTTP API server
barca list [file.py|dir/ ...] [-l N | --all] [--json]  List discovered definitions and their deps
barca status [target[,target...]] [file.py|dir/ ...] [--json] [--sample N]
                                               Cache state, last run and artifact shape per node
barca sql "<query>" [file.py|dir/ ...] [--json] [-l N | --all]
                                               Query cached results with DuckDB (experimental)
barca docs [topic] [--all] [--json]           Built-in manual
barca version                                 Print version
barca --help                                  Show help
```

Shorthand: `barca pipeline.py` is rewritten to `barca get pipeline.py`.

Files are optional on every command. Without them barca reads every `.py` file under the project
root (the nearest directory holding `barca.toml`, else the current one) that imports barca.
Files or directories narrow it; a directory given first needs a trailing `/` or must be `.`.
Node ids are relative to the root. See [Discovery](/reference/discovery/).

## Output format

`get`, `run`, `list`, `history` and `stats` choose what to print on stdout by one rule, first
match wins:

1. **A flag:** `--json` forces JSON; `--pretty` forces human output (tables, summaries). `get` and
   `run` also keep `-o json|value|pretty` for compatibility (`-o value` prints only the final
   value); `-o` cannot be combined with `--json` / `--pretty`.
2. **`BARCA_OUTPUT=json` or `BARCA_OUTPUT=pretty`** in the environment. Any other value is a usage
   error (exit code 2).
3. **The terminal:** stdout is a TTY → human output; a pipe, file or subprocess → JSON.

```bash
barca list pipeline.py            # a table in your terminal
barca list pipeline.py | cat      # a JSON array
barca list pipeline.py --json     # JSON even in a terminal
barca history --pretty            # a table even when piped
BARCA_OUTPUT=json barca get summary pipeline.py
```

`plan` always prints JSON and `docs` always prints markdown (`docs --json` for JSON). Progress and
errors always go to stderr; the progress bar (the only ANSI output) draws only when stderr is a
terminal, and `--agent` replaces it with plain progress lines.

> **Behavior change:** `get` and `run` used to print JSON by default even in a terminal.
> They now print the human summary there; scripts and agents that capture stdout still get JSON.
> The Python API (`barca.get`, `barca.history`, ...) always requests JSON.

## get

Execute the computation graph and return asset value(s). Cache-aware — only the needed subgraph
runs, and unchanged steps are served from cache.

Each completed step **fully materializes** its output to an artifact file under `.barca/artifacts/`
(json, pickle, or parquet). That write is the cache checkpoint — barca does not pass lazy
in-memory frames or query plans between workers. Downstream steps read the artifact back;
parameter type annotations (e.g. `data: pl.DataFrame`) select the parquet *reader* only.
To cache several outputs from one efficient computation, define multiple assets (or compute
them in one step and return the value you want cached).

If the first positional argument ends in `.py`, all arguments are treated as files: barca gets
every asset and sensor and returns the last asset's value. Tasks are skipped (previously a bare
`get` ran them too); stderr names them and the `barca run` command, for example
`[barca] skipped 1 task (report): ... Run a task with: barca run report pipeline.py`. A sensor
nothing depends on is still observed, since `get` accepts sensors as targets. A file with only
tasks gets nothing: exit 0, `"steps": []`, and a stderr note pointing at `barca run`. Otherwise
the first argument is the target asset name and the rest are files.

The target comes before the files. `barca get pipeline.py summary` (a file first, then a name)
has only one valid reading, so it exits 2 and prints the corrected command,
`barca get summary pipeline.py`, instead of running. Every `get`/`run` usage error (wrong order,
missing target or files, unknown target, using `get` on a task or `run` on an asset, an unknown
`--refresh` name) exits 2 and ends with
``Run `barca list <files>` to see available assets and tasks.`` (the same remediation `status`
and `stats` give). There is no fuzzy "did you mean" matching anywhere, and a mistyped flag gets no
"a similar argument exists" tip: a guess can read as confirmation.

```bash
barca get pipeline.py                 # all assets and sensors (never tasks)
barca get summary pipeline.py         # a specific target
barca get summary,orders pipeline.py  # several targets in one run (see "Several targets" below)
barca get pipeline.py --refresh-all   # execute everything fresh
barca get summary pipeline.py --refresh orders   # re-run orders and everything downstream of it
barca get pipeline.py --agent         # plain progress lines instead of a progress bar
barca get pipeline.py --json          # JSON even in a terminal (the default when piped)
barca get pipeline.py --pretty        # summary and value (the default in a terminal)
barca get pipeline.py -o value        # print just the final value (also: json | pretty)
barca get pipeline.py --fields id,status   # trim each entry of `steps` in the JSON
```

An asset that reads a `@sensor` is cached against the sensor's output: when the sensor returns a
new value (a new blob etag, say), the asset and everything downstream of it re-run. Sensors run
in a phase before their consumers so the decision uses this run's value. `--dry-run` predicts
from the sensor's last recorded output and says so in `detail`; before the sensor ever ran, its
consumers are `unknown` (`reason: "sensor_output_unknown"`).

> **Behavior change:** a sensor's output previously did not reach its consumers' run hashes, so an
> asset reading a sensor was served from cache whatever the sensor returned. Assets that read a
> sensor re-run once after upgrading. Pipelines without sensors keep their run hashes.

Each entry in the result's `steps` array says what happened to that step. A node that declares
environment variables with `env=[...]` also carries `env`, the values the step was hashed with
(`null` when unset, `<redacted>` for names like `*_TOKEN`), and its `--agent` progress line ends
with `env NAME=value ...`. See [Decorators](/reference/api/decorators/#declared-environment-variables-env).

## run

Execute a task and its dependency cone. Tasks always re-run (they are never cached). Upstream
assets are cache-aware by default, exactly like `barca get`. Use `--refresh` to force
re-materialize named upstream assets and every asset downstream of them in the task's cone, add
`--no-cascade` to re-materialize only the named assets, or use `--refresh-all` to refresh every
upstream asset in the cone. `barca get` takes the same three flags (on `get` the target asset may
be named in `--refresh`). `--no-cache` is the deprecated spelling of `--refresh-all` on both
commands: it still works, warns on stderr, and will be removed in a future minor release.

```bash
barca run deploy pipeline.py                          # run task, upstream assets from cache
barca run deploy pipeline.py --refresh fetch,transform  # re-materialize these and their downstream
barca run deploy pipeline.py --refresh fetch --no-cascade  # re-materialize only fetch
barca run deploy pipeline.py --dry-run --refresh fetch  # preview the cascade
barca run deploy pipeline.py --refresh-all            # re-materialize all upstream assets
```

Unlike `barca get`, which targets assets and respects the cache, `barca run` is for tasks that
produce side effects (deploys, notifications, reports). If a task must see fresh upstream data,
pass `--refresh-all`.

> **Behavior change:** `barca run` previously force-rerun every upstream asset by default and took
> `--burst`. Add `--refresh-all` to restore the old default; `--burst a,b` is now `--refresh a,b`.

> **Behavior change:** `--refresh` previously did not cascade: it re-ran only the named assets and
> left assets downstream of them cached (with a warning). It now re-materializes everything
> downstream of the named assets too; pass `--no-cascade` for the old behavior. A step re-run by the
> cascade reports `reason: "refresh_cascade"`.

### Several targets

`barca run` and `barca get` take one comma-separated list of targets (no spaces), matching the
`--refresh a,b` convention:

```bash
barca run validate_registry,validate_names pipeline.py             # both tasks, one run
barca run validate_registry,validate_names pipeline.py --dry-run   # preview the union
barca get summary,orders pipeline.py                               # several assets
```

- The union of the targets' cones is planned once, so an upstream step shared by several targets
  runs (or is served from cache) once. `--refresh` names may come from any target's cone.
- Every target runs even if another fails. A failure skips only the steps that depend on it
  (`"status": "skipped"`, reason `upstream_failed`). The exit code is 1 if any target failed,
  stdout still carries the JSON (with `"status": "failed"`), and the last stderr line is the error
  envelope for the first failed target.
- Every name is checked before anything runs; an unknown name, a task passed to `get`, or an
  empty name (`a,,b`) is a usage error (exit 2) and runs nothing.
- With one target the output is unchanged. With several, `final_output` is replaced by `targets`,
  keyed by target name in the order given:

```json
{"status": "success", "run_id": "...", "elapsed_seconds": 0.2, "steps_executed": 3, "phases": 2, "steps": [...],
 "targets": {"validate_registry": {"status": "success", "final_output": {"models": 2}},
             "validate_names": {"status": "success", "final_output": {"lowercase": true}}}}
```

A failed target is `{"status": "failed", "failed_node": "pipeline.py:...", "error": "..."}`, where
`failed_node` is the target itself or the upstream step that failed (the same key a failed
single-target run uses). `--dry-run` with several targets reports `targets` in place of `target`:
an object keyed by target name in the order given, each `{"summary": {"will_run", "cached",
"unknown"}}` counted over that target's cone. `-o value` prints `{target: value}`.

## plan

Parse the source files and emit the tiered execution plan as JSON, without running anything.
Experimental: the layout follows the planner. Each phase's `reason` is `{"type": "initial"}` or
`{"type": "fan_in", "node_id": "..."}`. Planning reads no state, so `plan` takes no `--env`.

```bash
barca plan pipeline.py
```

## history

Show recent runs from `.barca/metadata.db` — run id, command, status, step counts, and timing.

```bash
barca history                     # last 10 runs: a table in a terminal, JSON when piped
barca history -l 25               # last 25
barca history --all               # every recorded run
barca history --json              # {"runs": [...], "total": N, "truncated": bool, "hint"?: "..."}
barca history --pretty            # the table, even when piped
barca history --fields run_id,status   # JSON with only these keys per run
```

When more runs exist than are shown, the JSON has `"truncated": true`, the `total`, and a `hint`;
the table prints the same hint as one line on stderr. See [Bounded output](#bounded-output). Each
run's `files` is an array of the `.py` files it was given.

## stats

Show aggregated execution statistics for a single asset: total materializations, timing
percentiles (avg / median / p95 / max), cache hit rate, and recent runs.

```bash
barca stats summary pipeline.py
barca stats summary pipeline.py --json     # the same as one JSON object; the node id is `id`
barca stats summary pipeline.py --pretty   # the text report, even when piped
barca stats summary pipeline.py --fields status,error_message   # JSON; trims recent_runs entries
```

## serve

Start a long-running HTTP server that exposes the orchestrator as a JSON API. Binds to
`127.0.0.1` (local only, no auth). See [Server API](/reference/server-api/) for the full endpoint
reference.

```bash
barca serve pipeline.py                 # default port 8274
barca serve pipeline.py --port 8400     # custom port
barca serve pipeline.py --watch         # dev mode: re-parse the DAG on file change
barca serve pipeline.py --no-schedule   # disable the cron scheduler
barca serve pipeline.py --timezone utc  # evaluate cron in UTC (default: local)
```

`--watch` is a local-development convenience and is off by default; a production deployment serves
a fixed set of files and does not need it.

`barca serve` does not yet support shared remote state — if `barca.toml` resolves to
`state = "optimistic"` with a state URI, `serve` refuses to start with an error telling you to set
`state = "off"` (or `BARCA_STATE=off`) to serve with a local metadata DB. See
[Configuration](/reference/config/).

## list

List all discovered definitions (assets, tasks, sensors) with their kind, freshness, and
dependencies. Scheduled definitions also show their next fire time in local time (to the
second, so sub-minute schedules are legible).

```bash
barca list                        # every node in the project
barca list pipelines/             # only files under pipelines/
barca list pipeline.py
barca list pipeline.py --json     # {"nodes": [{id, kind, freshness, schedule?, inputs, env, next_fire?}], "total", "truncated", "root"}
barca list pipeline.py --pretty   # the table, even when piped
barca list pipeline.py --limit 20   # first 20 nodes in topological order
barca list pipeline.py --all        # every node (default: at most 100)
barca list pipeline.py --fields id,inputs   # JSON with only these keys per node
```

In JSON, `freshness` is `always`, `manual` or `schedule` (lowercase, like `kind`); a scheduled node
also has `schedule`, its cron expression. `list` reads no state, so it takes no `--env`.

`list` prints at most 100 nodes by default, which covers typical pipelines; larger DAGs are cut
off in topological order and say so (`"truncated": true` in JSON, a note on stderr for the table).

When any node declares environment variables (`@asset(env=["SOURCE_CSV"])`), the table gains an
ENV column listing them; `--json` always includes `env` (an empty list when none are declared).

## Bounded output

List-shaped commands (`list`, `status`, `history`) are bounded by default so a large project cannot flood
a terminal or an agent's context. Their JSON is an envelope:

```json
{"nodes": [...], "total": 312, "truncated": true,
 "hint": "pass --limit N for more, or --all for all 312 nodes"}
```

`truncated` and `total` are always present, `hint` only when truncated. `--limit N` and `--all`
choose how many items to print.

`--fields a,b` keeps only those keys on each item of any JSON output: `nodes` (`list`, `status`), `runs`
(`history`), `recent_runs` (`stats`), `steps` (`get`/`run`, including `--dry-run`) and `topics`
(`docs`). It implies JSON everywhere; combining it with `--pretty`, `-o pretty` or `-o value` is
a usage error (exit 2). An unknown key is a usage error (exit 2) that lists the valid ones. Limits bound
how many items are printed, never their content: error messages and tracebacks are always
complete. `plan` has no per-item objects (its steps are id strings), so it takes no `--fields`.

> **Behavior change (after 0.9.0):** `list --json` and `history --json` used to print a bare
> array. They now print the envelope above; read `.nodes` / `.runs` (e.g. `jq '.nodes[].id'`).

## status

One read-only view of every node in scope: kind, inputs, whether it is partitioned, its cache
state with the reason, its last materialization, and the shape of its artifact. It replaces the
round trip through `list`, `--dry-run`, `history` and a one-off script that opens the artifact.

```bash
barca status pipeline.py                         # table in a terminal, JSON when piped
barca status total pipeline.py                   # only `total` and its upstream cone
barca status total,notify pipeline.py            # several targets: the union of their cones
barca status pipeline.py --json                  # {target, targets, nodes, summary, total, truncated}
barca status pipeline.py --pretty                # the table, even when piped
barca status pipeline.py --fields id,cache       # JSON with only these keys per node
barca status pipeline.py --json --sample 5       # plus up to 5 sample rows per json/parquet artifact
```

```
NAME    KIND   STATE        WHY           LAST RUN                           SHAPE            DEPS
orders  asset  cached       materialized  success 2026-10-02 11:56:43 0.54s  3 rows x 2 cols  -
total   asset  cached       materialized  success 2026-10-02 11:56:43 0.00s  dict (1 key)     orders
notify  task   always-runs  task          -                                  -                total

2 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 1 always run
```

**Cache state** (`cache.state`) is the decision `--dry-run` makes, from the same code path. JSON
spells it in snake_case, exactly like the `summary` keys; the table prints `never-run` and
`always-runs`:

| state | meaning | reasons |
|---|---|---|
| `cached` | a successful result matches this code and these inputs | `materialized` |
| `stale` | ran before, but would run again | `changed`, `upstream_stale`, `failed` |
| `never_run` | no successful materialization recorded | `no_record`, `failed` |
| `partial` | partitioned, some keys cached | `partitions_missing` |
| `unknown` | dynamic partitions whose source has not run | `partitions_unknown` |
| `always_runs` | tasks and sensors | `task`, `sensor` |

`changed` means the run hash differs from the last materialization: this function's code or its
upstream outputs changed. barca stores only the combined hash, so it cannot say which.

**Last materialization** is the most recent execution recorded in the metadata DB (success or
failure): `status`, `created_at` (UTC), `elapsed_seconds`, `run_hash`, `artifact`, `format`,
`size_bytes`, and `error` for a failure. Cache hits do not change it.

**Shape** is read from the artifact file only, by a small Python helper; your code is never
imported:

- parquet: `rows` and `columns` (name and arrow type) from the file footer. Needs pyarrow;
  without it, `shape.note` says so.
- json: `type`; a list adds `rows` (and `columns` with the JSON types seen, for a list of
  objects); an object adds `keys`.
- pickle: `type` only (e.g. `myproject.Model`), read from the pickle opcodes without unpickling.
  Pickles are never sampled.
- Remote artifacts are read from the bucket with the credentials the steps use: a parquet footer
  by ranged requests (the object is not downloaded; `--sample N` also reads the first row group),
  json and pickle by a download of up to 16 MB. A larger one, a missing driver, rejected
  credentials or a network error is reported in `shape.note`; the command still exits 0.

**Partitioned assets** appear as one node with `partitions: {total, cached, missing,
missing_keys}` (up to 20 keys). `last_materialization` is the most recently run key (named in its
`partition` field), and `shape` describes that key's artifact.

Each node also lists `env`, the environment variables it declares with `env=[...]`. Like `list`,
status shows at most 100 nodes unless you pass `--limit N` or `--all`; the `summary` still counts
every node, and the JSON reports `total` and `truncated` (see [Bounded output](#bounded-output)).

Status writes nothing: no `.barca` directory is created and no run is recorded. An unknown target
is a usage error (exit 2). See `barca docs status`.

## sql

Query cached results with DuckDB. Every asset, sensor and task with a result on disk is a view
named after its function; a partitioned asset is one view with a `partition` column. Nothing
runs, user code is never imported, and nothing is recorded. With remote storage, the artifacts of
the views a query names are downloaded into `.barca/sql-cache/` and reused. Experimental.

```bash
barca sql "select * from revenue"
barca sql "select region, sum(amount) from orders group by 1" --json   # {columns, rows, total, truncated}
barca sql "select * from orders" --limit 20
```

See [barca sql](/reference/sql/) for views, errors and limits.

## docs

The manual, compiled into the binary: it works offline and always matches the installed version.
Every command's `--help` also ends with runnable examples.

```bash
barca docs                    # topic index with one-line summaries
barca docs types              # one topic as markdown (output formats, annotations, duckdb)
barca docs examples/duckdb    # a runnable example pipeline
barca docs skill              # the agent skill (SKILL.md), with its frontmatter
barca docs --all              # every topic in one stream
barca docs --json             # topic index as JSON; add a topic for its full text
```

Topics: `overview`, `assets`, `types`, `tasks`, `cache`, `partitions`, `sinks`, `scheduling`,
`status`, `agents`, `contract`, `skill`, and `examples/*`. `barca docs skill` is the short [agent skill](/reference/agent-skill/)
(`SKILL.md` in the repository) an AI agent loads once. `barca docs contract` is the [CLI contract](/reference/cli-contract/). `barca docs agents` describes the output contract for scripts and AI
agents: JSON on stdout, progress and errors on stderr, and the exit codes and error envelope below.

## Errors and exit codes

| Code | `kind`        | Meaning                                                                    |
|------|---------------|----------------------------------------------------------------------------|
| 0    |               | success                                                                    |
| 1    | `step_failed` | a step of yours raised; the traceback is included and the run is recorded as failed |
| 2    | `usage`       | bad flags or arguments, unknown target, `get` on a task or `run` on an asset, unreadable or invalid `.py` file, invalid `--env` or barca.toml |
| 3    | `infra`       | barca or its environment failed: metadata DB, worker pool, remote state, I/O |
| 130  | `cancelled`   | interrupted (Ctrl-C)                                                       |

In JSON output mode (whenever results are JSON: piped or captured stdout, `--json`, `-o json` or
`BARCA_OUTPUT=json`; `plan` always; `docs` with `--json`), an error is a single JSON line, the
last line on stderr:

```
{"code":2,"error":"Asset 'nope' not found. Available: pipeline.py:src, pipeline.py:total, pipeline.py:clean","kind":"usage","remediation":"Run `barca list pipeline.py` to see every node and its kind."}
```

`error`, `code`, `kind` and `remediation` are always present. When a step fails (`kind:
"step_failed"`) the envelope also has `node` (the failing step's id), `traceback` (the Python
traceback of your code, with barca's own frames removed) and `artifact_dir` (where that step's
artifacts are stored; a local path or remote URI, which may not exist if the step never
succeeded):

```
{"artifact_dir":".barca/artifacts/pipeline.py--clean","code":1,"error":"step 'pipeline.py:clean' failed: ZeroDivisionError: division by zero","kind":"step_failed","node":"pipeline.py:clean","remediation":"Fix the error in 'pipeline.py:clean' (see the traceback) and re-run the same command. Steps that succeeded are cached and will not re-run.","traceback":"  File \"/abs/path/pipeline.py\", line 11, in clean\n    return x / 0\n           ~~^~~"}
```

In human mode (a terminal, `--pretty`, `-o pretty` or `-o value`) the error is prose, with the remediation
on the last lines. Errors never go to stdout. The Python API raises `barca.BarcaError` with the
same fields as attributes (`kind`, `code`, `remediation`, `node`, `traceback`, `artifact_dir`).

When a step fails in JSON mode, `get`/`run` still print one result line on stdout, so you can
read the outcome without parsing stderr: `{"status": "failed", "failed_node": ..., "error": ...,
"run_id", "steps", ...}`, where the failed step's `status` is `failed`. A successful result has
`"status": "success"`. Just before the error, stderr gets one greppable line:
`[barca] run failed: step 'pipeline.py:clean' failed (exit 1)`.

## version

```bash
barca version
```

## --env

`get`, `run`, `plan`, `serve`, `history`, `stats`, and `status` accept `--env <name>`
(default: `BARCA_ENV`, then `default_env` in barca.toml, then `default`).
Environments fully separate cache, artifacts, and shared remote state — see
[Configuration](/reference/config/).
