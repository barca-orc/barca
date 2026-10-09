---
title: CLI Reference
description: Every barca command with its arguments, flags, defaults and output, checked against barca 0.18.0.
---

Installing the `barca` package (`pip install barca` or `uv add barca`) puts the `barca` command
in the environment's `bin/`. This page describes barca 0.18.0; the output shown was pasted from
that version. `barca <command> --help` and `barca docs` describe the version you have installed.

## Command shape

The target comes first, then the files:

```bash
barca get <asset> [files]     # assets: served from cache when nothing changed
barca run <task> [files]      # tasks: the task always runs
```

- `get` is for assets and sensors, `run` is for tasks. Both serve unchanged upstream assets
  from cache.
- Using the wrong one exits 2 and names the other, for example
  ``'validate' is a task — use `barca run` instead``.
- A file before the target exits 2 and prints the corrected command:

  ```
  $ barca run pipeline.py notify
  error: the target comes before the files

    barca run notify pipeline.py

  Run `barca list pipeline.py` to see available assets and tasks.
  ```

`barca list`, `barca status`, `--dry-run` and `barca sql` show what barca sees without running
anything.

The [CLI contract](/reference/cli-contract/) lists every command, flag, environment variable, exit
code and JSON output schema, marked stable or experimental, with the policy for changing them.

## Commands

```
barca get [target[,target...]] [file.py|dir/ ...] [--refresh a,b [--no-cascade] | --refresh-all]
                                               Get asset value(s) — cache-aware
barca run <task[,task...]> [file.py|dir/ ...] [--refresh a,b [--no-cascade] | --refresh-all]  Run task(s) (always re-run)
barca plan [file.py|dir/ ...]                Emit the execution plan as JSON (experimental)
barca history [-l N | --all] [--json|--pretty]  Show recent runs
barca stats <target> [file.py|dir/ ...]       Show timing/cache stats for an asset
barca serve [file.py|dir/ ...] [--port N] [--host IP] [--watch] [--no-schedule] [--timezone TZ] [--read-only]
                                               Run the HTTP API server
barca list [file.py|dir/ ...] [-l N | --all] [--json]  List discovered definitions and their deps
barca status [target[,target...]] [file.py|dir/ ...] [--json] [--sample N]
                                               Cache state, last run and artifact shape per node
barca sql "<query>" [file.py|dir/ ...] [--json] [-l N | --all]
                                               Query cached results with DuckDB (experimental)
                                               (needs duckdb installed)
barca docs [topic] [--all] [--json]           Built-in manual
barca version                                 Print version
barca --help                                  Show help
```

`barca pipeline.py` is read as `barca get pipeline.py`. It is kept for compatibility; write
`barca get`.

Files are optional on every command. Without them barca reads every `.py` file under the project
root (the nearest directory holding `barca.toml`, else the current one) that imports barca.
Files or directories narrow it; a directory given first needs a trailing `/` or must be `.`.
Node ids are relative to the root. See [Discovery](/reference/discovery/).

## Output format

`get`, `run`, `list`, `status`, `sql`, `history` and `stats` choose what to print on stdout by
one rule, first match wins:

1. **A flag:** `--json` forces JSON; `--pretty` forces human output (tables, summaries). `get` and
   `run` also keep `-o json|value|pretty` for compatibility (`-o value` prints only the final
   value); `-o` cannot be combined with `--json` / `--pretty`.
2. **`BARCA_OUTPUT=json` or `BARCA_OUTPUT=pretty`** in the environment. Any other value is a usage
   error (exit code 2).
3. **The terminal:** stdout is a TTY → human output; a pipe, file or subprocess → JSON.

```bash
barca list pipeline.py            # a table in your terminal
barca list pipeline.py | cat      # JSON: {"nodes": [...], "total", "truncated", "root"}
barca list pipeline.py --json     # JSON even in a terminal
barca history --pretty            # a table even when piped
BARCA_OUTPUT=json barca get summary pipeline.py
```

`get` and `run` print their JSON as one line. `list`, `status`, `sql`, `stats`, `history` and
`plan` print one indented JSON document over several lines, so parse all of stdout, not its
last line.

`plan` always prints JSON and `docs` always prints markdown (`docs --json` for JSON). Progress and
errors always go to stderr; the progress bar (the only ANSI output) draws only when stderr is a
terminal, and `--agent` replaces it with plain progress lines.

A library warning that a step's process repeats is printed once per run and then counted
(`[barca] 79 more: ...`); see `barca docs agents`, "Repeated warnings".

`plan`, `get`, `run` and `--dry-run` also report plan-time warnings: one `[barca] warning: ...`
line each on stderr, and a `warnings` array in their JSON output (`[]` when there are none).
The one warning so far is `unused_input`: a step declares an input its function never uses,
which is still loaded and still part of the cache key. Warnings do not change the exit code
(`barca docs assets`, "Unused inputs"). A query receiver shadowed by a `match` capture
or nested function/class definition is treated conservatively, so dynamic SQL access does
not cause a false unused-input warning.

## get

Compute one or more assets and print the result. Only the target and what is upstream of it
are considered, and a step whose code and inputs are unchanged is served from cache.

```
$ barca get total pipeline.py --json
[barca] 2/2 steps | done in 0.4s
{"elapsed_seconds":0.4704835,"final_output":{"total":80.5},"phases":1,"run_id":"526915c3ef18","status":"success","steps":[{"detail":"no cached result for this code and these inputs","id":"pipeline.py:orders","kind":"asset","reason":"not_materialized","run_hash":"732a...","status":"ran"},{"detail":"no cached result for this code and these inputs","id":"pipeline.py:total","kind":"asset","reason":"not_materialized","run_hash":"f4db...","status":"ran"}],"steps_executed":2,"warnings":[]}
```

The first line is progress, on stderr. The JSON is one line on stdout (run hashes shortened
here). Run it again and every step is `"status":"cached"`, with its `artifact` path, and
`"steps_executed":0`. In a terminal the same run prints a summary and the value:

```
$ barca get total pipeline.py
Run 5269f89835e0 | got 'total' in 0.003s (0 steps, 1 phase)

Value:
{
  "total": 80.5
}
```

`final_output` is the value itself for a json result. For a parquet or pickle result it is a
pointer to the file:

```json
{"_barca_artifact":{"format":"parquet","path":".barca/artifacts/pipeline.py--orders/732a....parquet","size_bytes":2143}}
```

For a partitioned asset `final_output` is the result of one key, not of all of them; use
`barca sql` or a `collect` asset to read every key.

With no target (`barca get pipeline.py`, or `barca get`), barca gets every asset and sensor and
returns the last asset's value. Tasks are skipped, and stderr names them and the `barca run`
command. A sensor nothing depends on still runs. A file with only tasks gets nothing: exit 0,
`"steps": []`, and a stderr note pointing at `barca run`.

Every `get`/`run` usage error (wrong order, unknown target, `get` on a task or `run` on an
asset, an unknown `--refresh` name) exits 2 and ends with
``Run `barca list <files>` to see available assets and tasks.`` There is no "did you mean"
matching.

| Flag | Meaning |
|---|---|
| `--refresh a,b` | Recompute the named assets (one comma-separated list) and everything downstream of them in the target's cone. The target itself may be named. |
| `--no-cascade` | With `--refresh`: recompute only the named assets. Downstream assets stay cached and do not reflect the refresh; barca warns. |
| `--refresh-all` | Recompute every asset in the target's cone. |
| `--dry-run` | Report each step as cached or to run, and why, without running or writing anything. |
| `--json`, `--pretty` | Force JSON or human output. |
| `-o json\|value\|pretty` | Older spelling, kept for compatibility. `-o value` prints only the final value. |
| `--agent` | Plain progress lines on stderr in place of the progress bar. |
| `--fields a,b` | Keep only these keys on each entry of `steps`: `id`, `kind`, `action`, `status`, `reason`, `detail`, `run_hash`, `artifact`, `warning`, `partitions`, `env`. |
| `--env <name>` | Use a named environment's cache and history. |

`--no-cache` is a deprecated spelling of `--refresh-all`: it works and warns on stderr.

A dry run of the same target:

```
$ barca get total pipeline.py --dry-run
Dry run: barca get total (nothing executed, nothing written)

STATUS    WHY                                              STEP
will run  no cached result for this code and these inputs  pipeline.py:orders
will run  no cached result for this code and these inputs  pipeline.py:total

2 will run, 0 cached, 0 unknown
```

An asset that reads a `@sensor` is cached against the sensor's value: when the sensor returns a
new value, the asset and everything downstream of it re-run. `--dry-run` predicts from the
sensor's last recorded value and says so in `detail`; before the sensor has ever run, its
consumers are `unknown` (`reason: "sensor_output_unknown"`).

Each entry in the result's `steps` array says what happened to that step. A node that declares
environment variables with `env=[...]` also carries `env`, the values the step was hashed with
(`null` when unset, `<redacted>` for names like `*_TOKEN`), and its `--agent` progress line ends
with `env NAME=value ...`. See [Decorators](/reference/api/decorators/#declared-environment-variables-env).

## run

Run a task and what it depends on. The task always runs; it is never cached. The assets
upstream of it are served from cache when unchanged, as with `barca get`. `run` takes the same
flags as `get`.

```
$ barca run notify pipeline.py
[barca] 1/3 steps | done in 0.1s
Run 5269fc06ce80 | ran 'notify' in 0.192s (1 step, 1 phase)

Value:
{
  "sent": 80.5
}
```

```bash
barca run deploy pipeline.py --refresh fetch,transform  # recompute these and everything downstream of them
barca run deploy pipeline.py --dry-run --refresh fetch  # preview the cascade
```

To have assets pick up outside data that changed, put a `@sensor` in front of them
(see [Decorators](/reference/api/decorators/#sensor)).

### Several targets

`barca run` and `barca get` take one comma-separated list of targets (no spaces):

```bash
barca run validate_registry,validate_names pipeline.py             # both tasks, one run
barca get summary,orders pipeline.py --dry-run                     # preview the union
```

- The union of the targets' cones is planned once, so a shared upstream step runs (or is served
  from cache) once.
- Every name is checked before anything runs; an unknown name, a task passed to `get`, or an
  empty name (`a,,b`) is a usage error (exit 2).
- Every target runs even if another fails. A failure skips only the steps that depend on it
  (`"status": "skipped"`, reason `upstream_failed`), and the exit code is 1 if any target failed.
- With several targets, `final_output` is replaced by `targets`, keyed by target name in the
  order given:

```json
{"status": "success", "run_id": "...", "elapsed_seconds": 0.2, "steps_executed": 3, "phases": 2, "steps": [...],
 "targets": {"validate_registry": {"status": "success", "final_output": {"models": 2}},
             "validate_names": {"status": "success", "final_output": {"lowercase": true}}}}
```

A failed target is `{"status": "failed", "failed_node": "pipeline.py:...", "error": "..."}`.
`--dry-run` with several targets reports `targets` in place of `target`, each with a `summary`
counted over that target's cone. The full shapes are in `barca docs agents`.

## plan

Parse the source files and print the execution plan as JSON, without running anything.
The pool size matches execution: available cores, or a positive `BARCA_POOL_SIZE`.
Experimental: the layout follows the planner and may change between releases. Each phase's
`reason` is `{"type": "initial"}` or `{"type": "fan_in", "node_id": "..."}`. Planning reads no
state, so `plan` takes no `--env` and does not say what is cached; use `--dry-run` or
`barca status` for that. Its only option is `--help`.

```
$ barca plan pipeline.py
{
  "total_steps": 3,
  "phases": [
    {
      "reason": {
        "type": "initial"
      },
      "streams": [
        {
          "stream_id": "p0-w0",
          "steps": [
            "pipeline.py:orders",
            "pipeline.py:total",
            "pipeline.py:notify"
          ]
        }
      ]
    }
  ],
  "warnings": []
}
```

## history

Show recent runs from `.barca/metadata.db`: run id, command, status, step counts and timing.
The default is the 10 most recent.

```
$ barca history
RUN_ID         CMD     STATUS      STEPS CACHED   TIME STARTED
-----------------------------------------------------------------------------
5269fc06ce80   run     success         1      2   0.2s 2026-10-07 18:22:40
5269fa584a48   get     success         0      1   0.0s 2026-10-07 18:22:40
526915c3ef18   get     success         2      0   0.5s 2026-10-07 18:22:40
```

Times are UTC. `--fields` takes `run_id`, `command`, `files`, `target`, `status`,
`steps_total`, `steps_executed`, `steps_cached`, `started_at`, `finished_at`,
`elapsed_seconds`.

```bash
barca history -l 25               # last 25 (default 10); --all for every recorded run
barca history --json              # {"runs": [...], "total": N, "truncated": bool, "hint"?: "..."}
barca history --fields run_id,status   # JSON with only these keys per run
```

When more runs exist than are shown, the JSON has `"truncated": true`, the `total`, and a `hint`;
the table prints the same hint as one line on stderr. See [Bounded output](#bounded-output). Each
run's `files` is an array of the `.py` files it was given.

A run's `status` is `running`, `success`, `failed`, `cancelled` or `interrupted`. A run records
each step as it finishes, so a `running` run already counts them in `steps_executed`, and
`barca status` from another terminal shows them as `cached`. A run whose process was killed is
`interrupted` (no `finished_at`); the next `barca get` reuses the steps it had recorded. A run
killed in a container is `interrupted` too, once a container starts again on the same `.barca`
volume (from 0.20.0; earlier versions left it `running`). For runs started with 0.20.0 or later, a run stays `running`
whenever barca cannot establish that its process is gone. Earlier runs retain their legacy
process-id and host-name checks. See
`barca docs cache`, "While a run is going, and after one is killed", and the shared-state
[limitations](/reference/remote-storage/#limitations).

## stats

Show statistics for one node: how many times it ran, timings (average, median, p95, max),
cache hit rate and recent runs. The target is required.

```
$ barca stats total pipeline.py
Asset: pipeline.py:total
Total materializations: 1
Timing:  avg 0.001s  median 0.001s  p95 0.001s  max 0.001s
Cache hit rate: 0.0%

Recent runs:
  ELAPSED    STATUS    ATTEMPTS CREATED
  0.001s     success   1        2026-10-07 18:22:40
```

The JSON has `id`, `total_runs`, `cache_hit_rate`, `avg_elapsed_seconds`,
`median_elapsed_seconds`, `p95_elapsed_seconds`, `max_elapsed_seconds` and `recent_runs` (each
`elapsed_seconds`, `status`, `created_at`, `error_message`, `attempts`).

```bash
barca stats summary pipeline.py --json     # one JSON object; the node id is `id`
barca stats summary pipeline.py --fields status,error_message   # JSON; trims recent_runs entries
```

## serve

Start an HTTP server with a JSON API, the cron scheduler and the web UI at `/ui/`. It binds to
`127.0.0.1` by default; `--host 0.0.0.0` listens on every interface. It has no authentication. See [Server API](/reference/server-api/) for the
endpoints and [Deploying](/deploying/) for running it behind nginx.

```bash
barca serve pipeline.py                 # default port 8274
barca serve pipeline.py --host 0.0.0.0  # every interface (containers, VMs); no auth
barca serve --timezone utc              # every file in the project; cron evaluated in UTC
```

| Flag | Default | Meaning |
|---|---|---|
| `-p`, `--port <PORT>` | `8274` | Port to listen on. |
| `--host <IP>` | `127.0.0.1` | IP address to bind on; `0.0.0.0` (or `::`) listens on every interface. |
| `--watch` | off | Re-parse the DAG when a source file changes. Files added later still need a restart. |
| `--no-schedule` | off | Do not fire `Schedule(...)` nodes. |
| `--timezone <TZ>` | `local` | Timezone for cron: `local`, `utc` or an IANA name such as `America/New_York`. Any other value is a usage error (exit 2). |
| `--read-only` | off | Refuse runs, never schedule, read the metadata DB from copies. |
| `--env <name>` | `default` | Use a named environment's cache and history. |

`barca serve` does not support shared history. With a remote store and
`state = "optimistic"` (the default with a remote store) it exits 2:

```
barca serve does not support shared remote state yet — set state = "off" in barca.toml (or BARCA_STATE=off) to serve with a local metadata DB
```

See [Configuration](/reference/config/).

## list

List every asset, task and sensor barca finds, with its kind, freshness and inputs. A scheduled
node also shows its cron and its next fire time, to the second. The time is the next match of
the cron expression in the local time of the machine `list` runs on (the table column is
`NEXT FIRE (LOCAL TIME)`). `list` talks to no server, so it does not know a server's
`--timezone`: a server started with one fires at the same wall-clock time in that zone, and its
[`GET /schedule`](/reference/server-api/#get-schedule) reports the times it will fire at.

```
$ barca list pipeline.py
NAME                KIND   FRESHNESS  DEPS
------------------------------------------
pipeline.py:orders  asset  always     -
pipeline.py:total   asset  always     pipeline.py:orders
pipeline.py:notify  task   always     pipeline.py:total
```

The FRESHNESS column shows what the decorator declares. Only `schedule` has an effect, and only
under `barca serve`.

```bash
barca list                        # every node in the project
barca list pipelines/             # only files under pipelines/
barca list pipeline.py --json     # {"nodes": [{id, kind, freshness, schedule?, inputs, env, next_fire?}], "total", "truncated", "root"}
barca list pipeline.py --fields id,inputs   # JSON with only these keys per node
```

In JSON, `freshness` is `always`, `manual` or `schedule` (lowercase, like `kind`); a scheduled node
also has `schedule`, its cron expression. `list` reads no state, so it takes no `--env`.

`list` prints at most 100 nodes by default, in topological order; see
[Bounded output](#bounded-output).

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

## status

Show, for every node in scope, its kind, inputs, whether it is partitioned, its cache state
with the reason, its last run, and the shape of its result. It runs nothing.

```bash
barca status total pipeline.py                   # only `total` and its upstream cone
barca status total,notify pipeline.py            # several targets: the union of their cones
barca status pipeline.py --json                  # {target, targets, nodes, summary, total, truncated, root}
barca status pipeline.py --json --sample 5       # plus up to 5 sample rows per json/parquet artifact
```

```
NAME    KIND   STATE        WHY           LAST RUN                           SHAPE            DEPS
orders  asset  cached       materialized  success 2026-10-07 18:22:40 0.38s  3 rows x 2 cols  -
total   asset  cached       materialized  success 2026-10-07 18:22:40 0.00s  dict (1 key)     orders
notify  task   always-runs  task          success 2026-10-07 18:22:41 0.15s  dict (1 key)     total

2 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 1 always run
```

**Cache state** (`cache.state`) is the decision `--dry-run` makes, from the same code path. JSON
spells it in snake_case, exactly like the `summary` keys; the table prints `never-run` and
`always-runs`:

| state | meaning | reasons |
|---|---|---|
| `cached` | a successful result matches this code and these inputs | `materialized` |
| `stale` | ran before, but would run again | `changed`, `upstream_stale`, `failed`, `artifact_missing` |
| `never_run` | no successful materialization recorded | `no_record`, `failed` |
| `partial` | partitioned, some keys cached | `partitions_missing` |
| `unknown` | dynamic partitions whose source has not run | `partitions_unknown` |
| `always_runs` | tasks and sensors | `task`, `sensor` |

`changed` means the run hash differs from the last materialization: this function's code or its
upstream outputs changed. barca stores only the combined hash, so it cannot say which.
`artifact_missing` means the result is recorded but its artifact file is gone and a run would
have to read it, so it would be computed again; a missing artifact that nothing reads leaves the
node `cached` (`barca docs cache`, "A cached result whose artifact is missing").

**Last materialization** is the most recent execution recorded in the metadata DB, success or
failure. Cache hits do not change it.

**Shape** is read from the artifact file only, by a small Python helper; your code is never
imported. Parquet gives `rows` and `columns` from the file footer (needs pyarrow), json gives
`type` and, for a list or object, `rows`, `columns` or `keys`, and pickle gives `type` only.
Remote artifacts are read from the store; a failure is reported in `shape.note` and the
command still exits 0. Details: `barca docs status`.

**Partitioned assets** appear as one node with `partitions: {total, cached, missing,
missing_keys}` (up to 20 keys). `last_materialization` is the most recently run key (named in its
`partition` field), and `shape` describes that key's artifact.

Each node also lists `env`, the environment variables it declares with `env=[...]`. Like `list`,
status shows at most 100 nodes unless you pass `--limit N` or `--all`; the `summary` still counts
every node, and the JSON reports `total` and `truncated` (see [Bounded output](#bounded-output)).

With [shared history](/reference/remote-storage/), status first pulls it, as a run does.
Status reads the metadata DB as it is at that moment: while a `barca get` is running, the
steps it has finished already show as `cached` (a partitioned asset as `partial`).

Status writes nothing: no `.barca` directory is created and no run is recorded. An unknown target
is a usage error (exit 2). See `barca docs status`.

## sql

Query cached results with DuckDB. Every asset, sensor and task with a result on disk is a view
named after its function; a partitioned asset is one view with a `partition` column. No step
runs, your code is not imported, and no run is recorded. Only parquet and json results are
views; a pickled result cannot be queried. It needs `duckdb` installed in barca's Python
environment. With a remote store it first pulls the shared history into `.barca/`, and the
artifacts of the views a query names are downloaded into `.barca/sql-cache/` and reused.
Experimental.

```
$ barca sql "select region, sum(amount) as amount from orders group by 1 order by 1"
region  amount
amer    70.5
emea    10.0
```

| Flag | Default | Meaning |
|---|---|---|
| `-l`, `--limit <N>` | `100` | Maximum number of rows returned. |
| `--all` | off | Return every row. Cannot be combined with `--limit`. |
| `--json`, `--pretty` | by terminal | Force JSON or the table. |
| `--env <name>` | `default` | Query a named environment's results. |

See [barca sql](/reference/sql/) for views, errors and limits.

## docs

The manual, compiled into the binary, so it works offline and describes the installed version.
Every command's `--help` ends with examples.

```bash
barca docs                    # topic index with one-line summaries
barca docs types              # one topic as markdown (output formats, annotations, duckdb)
barca docs examples/duckdb    # a runnable example pipeline
barca docs skill              # the agent skill (SKILL.md), with its frontmatter
barca docs --all              # every topic in one stream
barca docs --json             # topic index as JSON; add a topic for its full text
barca docs --fields name      # JSON index with only topic names (name, summary, content)
```

`barca docs skill` is the short [agent skill](/reference/agent-skill/) an AI agent loads once.
`barca docs contract` is the [CLI contract](/reference/cli-contract/). `barca docs agents`
describes the output contract for scripts and AI agents.

## Errors and exit codes

| Code | `kind`        | Meaning                                                                    |
|------|---------------|----------------------------------------------------------------------------|
| 0    |               | success                                                                    |
| 1    | `step_failed` | a step of yours raised; the traceback is included and the run is recorded as failed |
| 2    | `usage`       | bad flags or arguments, unknown target, `get` on a task or `run` on an asset, unreadable or invalid `.py` file, invalid `--env` or barca.toml |
| 3    | `infra`       | barca or its environment failed: metadata DB, worker pool, remote state, I/O |
| 130  | `cancelled`   | stopped by Ctrl-C (SIGINT) or SIGTERM                                      |

A closed stdout or stderr never makes barca panic, never stops a run and is not an error:
output for the closed stream is dropped, a `get` or `run` finishes and is recorded, and the
exit code is the one the command would have had with a reader (`barca list | head -1` exits 0).
See [the CLI contract](/reference/cli-contract/#a-closed-stdout-or-stderr-stable).

An error in a pipeline file fails every command that reads the file with exit 2, whatever the
target: a syntax error, or a decorator called with an argument it does not define
(`@asset(after=other)`, `input=` for `inputs=`). See
[Accepted arguments](/reference/api/decorators/#accepted-arguments).

In JSON output mode (whenever results are JSON: piped or captured stdout, `--json`, `-o json` or
`BARCA_OUTPUT=json`; `plan` always; `docs` with `--json`), an error is a single JSON line, the
last line on stderr:

```
{"code":2,"error":"Asset 'nope' not found. Available: pipeline.py:orders, pipeline.py:total, pipeline.py:notify","kind":"usage","remediation":"Run `barca list pipeline.py` to see available assets and tasks."}
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

```
$ barca version
barca 0.18.0
```

`barca --version` prints the same. Neither takes `--json`.

## --env

`get`, `run`, `status`, `sql`, `history`, `stats` and `serve` accept `--env <name>` (default:
`BARCA_ENV`, then `default_env` in `barca.toml`, then `default`). `plan` and `list` read no
state and reject it. Each environment has its own cache, artifacts, history and remote
location. See [Configuration](/reference/config/#environments---env).

## Behavior changes from earlier versions

- `get` and `run` print a human summary in a terminal; they used to print JSON there. Piped
  or captured stdout is still JSON.
- `list --json` and `history --json` print an envelope (`.nodes`, `.runs`); up to 0.9.0 they
  printed a bare array.
- `barca run` serves upstream assets from cache; it used to recompute all of them. Pass
  `--refresh-all` for the old behavior. `--burst a,b` is now `--refresh a,b`.
- `--refresh` cascades downstream; it used to recompute only the named assets. Pass
  `--no-cascade` for the old behavior. A step re-run by the cascade reports
  `reason: "refresh_cascade"`.
- A sensor's value is part of its consumers' run hashes; it used not to be. Assets that read a
  sensor re-ran once after that upgrade.
- From 0.20.0: SIGTERM stops `get`, `run` and `serve` the way Ctrl-C does. `get` and `run`
  exit 130 (`cancelled`) and record the run as `cancelled`; before, SIGTERM killed barca at
  once (a shell reported 143) and the run was later reported as `interrupted`. As process 1 of
  a container barca used to ignore SIGTERM.
- From 0.20.0: `barca serve --timezone` with a value barca does not know exits 2; it used to
  print a warning and use local time.
- From 0.20.0: `POST /run/{target}` and `POST /get/{target}` answer 404, 409 or 400 for a
  target that cannot run; they used to answer 200 with a `run_id` of a run that then failed.
- From 0.20.0: the `barca list` table column `NEXT FIRE` is `NEXT FIRE (LOCAL TIME)`.
