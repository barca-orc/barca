---
title: CLI Reference
description: Every barca command with its arguments, flags, defaults and output.
---

Installing the `barca` package (`pip install barca` or `uv add barca`) puts the `barca` command
in the environment's `bin/`. The output on this page was captured from a terminal running the
current release, with long paths shortened. `barca <command> --help` and `barca docs` describe
the version you have installed.

Use `barca --help` to discover commands by purpose, `barca <command> --help` for concise
usage, and `barca <command> --examples` for the complete runnable example collection.
For detailed behavior, load one topic with `barca docs <topic>`; `barca docs --json` lists
available topics for scripts and agents. Help and examples never load a project or run work.

## Common tasks

| I want to… | Command | More |
|---|---|---|
| Get an asset, from cache when nothing changed | `barca get revenue` | [get](/reference/cli/get/) |
| Run a task | `barca run publish` | [run](/reference/cli/run/) |
| See what would run, without running it | `barca get revenue --dry-run` | [get](/reference/cli/get/) |
| Recompute an asset and what is downstream | `barca get revenue --refresh orders` | [The Cache](/reference/cache/#forcing-a-recompute) |
| See every node's cache state and why | `barca status` | [status](/reference/cli/status/) |
| See only what failed or is stale | `barca status --problems` | [status](/reference/cli/status/) |
| See recent runs, or one run's errors | `barca history` | [history](/reference/cli/history/) |
| Read a step's printed output | `barca logs` | [Logs](/reference/logs/) |
| Query cached results with SQL | `barca sql "select * from revenue"` | [sql](/reference/cli/sql/) |
| Roll an asset back one version | `barca pin revenue --previous` | [pin and unpin](/reference/cli/pin-and-unpin/) |
| Run schedules, the web UI and the HTTP API | `barca serve` | [serve](/reference/cli/serve/) |
| Free disk space under `.barca/` | `barca gc` | [gc](/reference/cli/gc/) |
| Check the setup without running anything | `barca doctor` | [Doctor](/reference/doctor/) |
| Read the manual in the terminal | `barca docs cache` | [docs](/reference/cli/docs/) |

`get`, `run`, `list`, `status`, `sql`, `history` and `stats` print JSON when their output is
piped ([Output format](#output-format)).

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
  ```

`barca list`, `barca status`, `--dry-run` and `barca sql` show what barca sees without running
anything.

The [CLI contract](/reference/cli-contract/) lists every command, flag, environment variable, exit
code and JSON output schema, marked stable or experimental, with the policy for changing them.

## All commands

```
barca get [target[,target...]] [file.py|dir/ ...] [--refresh a,b [--no-cascade] | --refresh-all]
                                               Get assets — cache-aware; prints their artifact paths
barca run <task[,task...]> [file.py|dir/ ...] [--refresh a,b [--no-cascade] | --refresh-all]  Run task(s) (always re-run)
barca plan [file.py|dir/ ...]                Emit the execution plan as JSON (experimental)
barca doctor [file.py|dir/ ...] [--json] [--server [URL] | --local]
                                               Diagnose project prerequisites (and a server) without running steps
barca logs [run_id] [--step NAME] [--stream S] [-f] [--json]  A run's recorded step stdout and stderr (default: the most recent run)
barca history [RUN_ID] [--status S] [--target NAME] [--triggered-by T] [--since WHEN] [-l N | --all] [--json|--pretty]
                                               Show recent runs, or one run's steps and errors
barca stats <target> [file.py|dir/ ...]       Show timing/cache stats for an asset
barca pin [target] [--previous|--run-hash H|--from-file F|--versions]  Hold an asset at an earlier result or supplied data (experimental)
barca unpin <target>                         Release a pinned asset (experimental)
barca gc [--dry-run] [--max-size SIZE] [--json]  Evict cached results to keep .barca/ under its size limit (experimental)
barca serve [file.py|dir/ ...] [--port N] [--host IP] [--watch] [--no-schedule] [--timezone TZ] [--read-only] [--drain-timeout SECONDS] [--max-runs N] [--capacity N] [--run-timeout SECONDS]
                                               Run the HTTP API server
barca queue [pause|resume] [--server [URL]] [--json]  A server's run queue; pause or resume it (experimental)
barca list [file.py|dir/ ...] [-l N | --all] [--json]  List discovered definitions and their deps
barca status [target[,target...]] [file.py|dir/ ...] [--json] [--sample N] [--problems] [--state S,S]
                                               Cache state, last run and artifact shape per node
barca sql "<query>" [file.py|dir/ ...] [--json] [-l N | --all]
                                               Query cached results with DuckDB (experimental)
                                               (install: uv add 'barca[sql]')
barca state backups [--json]                  List the metadata backups, local and in the store
barca state restore [--from N|NAME|PATH]      Put a metadata backup back (the current DB is kept aside)
barca state reset [--yes]                     Start the metadata DB over (the old one is moved aside)
barca docs [topic] [--all] [--json]           Built-in manual
barca version                                 Print version
barca --help                                  Show help
```

`get`, `run`, `status`, `list`, `history` and `stats` also take `--server [<url>]`, which runs
the command on a `barca serve` and prints it here as the local command would, and `--local`,
which ignores a configured server (experimental; [Server mode](/reference/config/#server-mode),
`barca docs server-mode`). `sql` takes both too: a server resolves its views, and the query
always runs here. `doctor` takes both too: `--server` checks that server beside this
machine (a configured one is checked without it), `--local` checks this machine only.

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
   `run` also keep `-o json|pretty`, older spellings that cannot be combined with `--json` /
   `--pretty`. `get -o path|value` choose what `get` returns (see [get](/reference/cli/get/)); `run -o value`
   prints only the task's value.
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

## Errors and exit codes

| Code | `kind` | Meaning |
|---|---|---|
| 0 | | Success |
| 1 | `step_failed` | A step of yours raised. The traceback is included and the run is recorded as failed |
| 2 | `usage` | Bad flags or arguments, an unknown target, `get` on a task or `run` on an asset, an unreadable or invalid `.py` file, an invalid `--env` or barca.toml |
| 3 | `infra` | Barca or its environment failed: metadata DB, worker pool, remote state, I/O |
| 130 | `cancelled` | Ctrl-C or SIGTERM |

The same table, generated from the code, is in the [CLI contract](/reference/cli-contract/#exit-codes).

A closed stdout or stderr never makes barca panic, never stops a run and is not an error:
output for the closed stream is dropped, a `get` or `run` finishes and is recorded, and the
exit code is the one the command would have had with a reader (`barca list | head -1` exits 0).
See [the CLI contract](/reference/cli-contract/#a-closed-stdout-or-stderr-stable).

An error in a pipeline file fails every command that reads the file with exit 2, whatever the
target: a syntax error, or a decorator called with an argument it does not define
(`@asset(after=other)`, `input=` for `inputs=`). See
[Accepted arguments](/reference/api/decorators/#accepted-arguments). A syntax error in a helper
module a pipeline file imports fails, with exit 2 before anything runs, every command that plans
a step of that file (`plan`, `status`, `get`, `run`, `--dry-run`), naming the module, line,
column and source line; `list` and steps of other files are not affected.

In JSON output mode (whenever results are JSON: piped or captured stdout, `--json`, `-o json` or
`BARCA_OUTPUT=json`; `plan` always; `docs` with `--json`), an error is a single JSON line, the
last JSON error object on stderr (a failed run adds its summary afterwards):

```
{"code":2,"error":"Asset 'nope' not found. Available: pipeline.py:orders, pipeline.py:total, pipeline.py:notify","kind":"usage","remediation":"Run `barca list pipeline.py` to see available assets and tasks."}
```

`error`, `code`, `kind` and `remediation` are always present. When a step fails (`kind:
"step_failed"`) the envelope also has `node` (the failing step's id), `traceback` (the Python
traceback of your code, with barca's own frames removed), `artifact_dir` (where that step's
artifacts are stored; a local path or remote URI, which may not exist if the step never
succeeded) and `phase` (where the step failed: `user_code` for your code; `load_inputs`,
`split_output`, `write_output` or `sink` for barca's own work around it, whose traceback keeps
barca's frames and whose prose says `failed in barca`; `null` when unknown, such as a worker
that died):

```
{"artifact_dir":".barca/artifacts/pipeline.py--clean","code":1,"error":"step 'pipeline.py:clean' failed: ZeroDivisionError: division by zero","kind":"step_failed","node":"pipeline.py:clean","phase":"user_code","remediation":"Fix the error in 'pipeline.py:clean' (see the traceback) and re-run the same command. Steps that succeeded are cached and will not re-run.","traceback":"  File \"/home/you/project/pipeline.py\", line 11, in clean\n    return x / 0\n           ~~^~~"}
```

In human mode (a terminal, `--pretty`, `-o pretty`, `-o path` or `-o value`) the error is prose, with the remediation
on the last lines. Errors never go to stdout. The Python API raises `barca.BarcaError` with the
same fields as attributes (`kind`, `code`, `remediation`, `node`, `traceback`, `artifact_dir`,
`phase`).

When a step fails in JSON mode, `get`/`run` still print one result line on stdout, so you can
read the outcome without parsing stderr: `{"status": "failed", "failed_node": ..., "error": ...,
"run_id", "steps", ...}`, where the failed step's `status` is `failed`. A successful result has
`"status": "success"`. After the error, stderr ends with one greppable line:
`[barca] run failed: step 'pipeline.py:clean' failed: ZeroDivisionError: division by zero; 0 downstream steps skipped (exit 1)`.

### A worker that dies

A worker process that dies while it runs a step (the kernel's out-of-memory killer, a segfault
in a C extension, `os._exit()` in your code) fails the step with exit 1, and the error names
how the process ended, in the result JSON, the envelope and the `run failed:` line:

```
worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)
worker for pipeline.py:train exited with SIGSEGV
worker for pipeline.py:train exited with code 3
```

SIGKILL is what the out-of-memory killer sends: check the container's or host's memory limit.
Several heavy steps running at once in separate workers add up; give them a `weight=` so fewer
run at once (see [Decorators](/reference/api/decorators/#heavy-steps-weight)). A worker frozen waiting for
its `parallel()` branches adds `while it waited for its parallel() branches`. When the exit
status is not available within a few hundred milliseconds, the error is the plain
`worker disconnected`.

## Commands in detail

Each command has its own page with its flags, examples and output. `barca <command> --help`
prints the same syntax for the version you have installed.

| Command | What it does |
|---|---|
| [`barca get`](/reference/cli/get/) | Get assets, from cache when nothing changed; prints where their results are. |
| [`barca run`](/reference/cli/run/) | Run tasks. A task always runs; the assets it reads come from cache. |
| [`barca plan`](/reference/cli/plan/) | Print the execution plan as JSON without running anything (experimental). |
| [`barca logs`](/reference/logs/) | A run's recorded step output; follow a live run. |
| [`barca doctor`](/reference/doctor/) | Check the project's prerequisites without running anything. |
| [`barca history`](/reference/cli/history/) | Show recent runs, or one run with its steps and errors. |
| [`barca pin and unpin`](/reference/cli/pin-and-unpin/) | Hold an asset at an earlier result or supplied data, and release it (experimental). |
| [`barca gc`](/reference/cli/gc/) | Evict cached results to keep .barca/ under its size limit (experimental). |
| [`barca stats`](/reference/cli/stats/) | Timing and cache statistics for one asset. |
| [`barca serve`](/reference/cli/serve/) | Run the HTTP API, the cron scheduler and the web UI. |
| [`barca queue`](/reference/cli/queue/) | A server's run queue; pause or resume it (experimental). |
| [`barca list`](/reference/cli/list/) | List the assets, tasks and sensors barca finds, with their inputs. |
| [`barca status`](/reference/cli/status/) | Cache state and why, last run and artifact shape for each node. |
| [`barca sql`](/reference/cli/sql/) | Query cached results with DuckDB (experimental). |
| [`barca state`](/reference/cli/state/) | List the metadata backups, put one back, or start the database over. |
| [`barca docs`](/reference/cli/docs/) | The built-in manual, for the installed version. |
| [`barca version`](/reference/cli/version/) | Print the installed version. |

## Run timing

Run JSON includes `steps[].elapsed_seconds` and `attempts` for executed steps, including
failures; cache hits and dry runs omit them. Elapsed time sums active worker time across
attempts, including import/input loading/call/serialization and excluding retry backoff.
After a worker disconnect, the coordinator's observed in-flight time is used. For
partitioned steps it sums executed keys, while attempts is the maximum for one key.

Under `--agent` (and in every run `barca serve` starts) a worker prints
`[barca] importing <module>` before loading a module; otherwise it prints nothing. Imports longer
than one second add `import_seconds`, included in elapsed time and summed across
attempts/keys. Reused modules add no import time. Agent completion and permanent failure
lines append `attempts=N` for retries. `history --json` sums recorded materialization
attempts (including existing upload-failure retry counts); `stats` retains each
materialization's attempts and final-attempt duration. Timeout errors name the step id.

## Bounded output

List-shaped commands (`list`, `status`, `history`) return every item in JSON unless `--limit N`
is explicit, including piped output and `--fields`. Human tables show at most 100 nodes or
10 history runs by default; `--all` removes the bound. Their JSON keeps its envelope:

```json
{"nodes": [...], "total": 312, "truncated": true,
 "hint": "pass --limit N for more, or --all for all 312 nodes"}
```

`truncated` and `total` are always present, `hint` only when truncated. `--limit N` and `--all`
choose how many items to print.

`--fields a,b` keeps only those keys on each item of any JSON output: `nodes` (`list`, `status`), `runs`
(`history`), `recent_runs` (`stats`), `steps` (`get`/`run`, including `--dry-run`) and `topics`
(`docs`). It implies JSON everywhere; combining it with `--pretty`, `-o pretty` or `run -o value` is
a usage error (exit 2). An unknown key is a usage error (exit 2) that lists the valid ones. Limits bound
how many items are printed, never their content: error messages and tracebacks are always
complete. `plan` has no per-item objects (its steps are id strings), so it takes no `--fields`.

## `--env`

`get`, `run`, `status`, `sql`, `history`, `stats`, `serve`, `state backups`, `state restore` and `state reset` accept `--env <name>` (default:
`BARCA_ENV`, then `default_env` in `barca.toml`, then `default`). `plan` and `list` read no
state and reject it. Each environment has its own cache, artifacts, history and remote
location. `get`, `run` and `serve` print the environment and artifact store once at startup
on stderr when `default_env` or an artifact store is in effect. See
[Configuration](/reference/config/#environments---env).

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
- From 0.25.0: `get` prints artifact paths by default. `final_output` in the JSON is always a
  pointer, and a terminal shows the path with a small json value previewed under it; `-o value`
  prints the value. A partitioned target returns every key, and `--partition` selects keys.
  (From 0.30.2, past 1000 keys only the keys a run computed are listed.)
  `.barca/` has a size limit (`[cache] max_size`, default 10% of the filesystem) and is evicted
  past it.
- From 0.26.0: a remote store fails soft once the steps are done: an upload that fails after
  every step succeeded exits 0 with a warning (it was 3), and the result is `unsynced` until the
  next command uploads it.
- From 0.26.0: the shared history (`optimistic` mode) is gone. `[remote] state`, `state_uri`,
  `push_retries`, `BARCA_STATE`, `BARCA_STATE_URI` and `BARCA_PUSH_RETRIES` are usage errors;
  nothing is pulled before a run, and the store holds backups instead
  ([Remote storage](/reference/remote-storage/)). `barca serve` works with a store.
- From 0.27.0: `barca serve` drains on SIGTERM or SIGINT: runs in flight finish, for up to
  `--drain-timeout` seconds, before it exits; it used to cancel them at once
  ([serve](/reference/cli/serve/)).
- From 0.29.4: a draining `barca serve` accepts new runs into its run queue (`202`, `pending`)
  instead of refusing them with `503`, and at `--drain-timeout` it puts the runs left back in
  the queue (exit 0) instead of cancelling them (exit 1); only a second signal cancels them.
  `--server` clients and `logs -f --server` wait up to 60 seconds (`BARCA_SERVER_RECONNECT`) for
  a restarting server instead of exiting 3 at once ([serve](/reference/cli/serve/)).

## Recorded output

`barca logs RUN_ID --pretty` prints what a run's steps wrote, without progress or prefixes:
their stdout, and their Python stderr (`logging`, `warnings`, `sys.stderr`) with the traceback
of each failed attempt. Writes straight to file descriptors 1 and 2 (native code, subprocesses)
are not recorded.
Without a run id, `barca logs` reads the most recent run (running or finished) and names it in a
line on stderr: `barca: logs of the most recent run: 07c4a64d0540 (run notify, success, started
2026-10-21 06:40:36 UTC)`.
`--json` returns `[{node_id, stream, ts, line}]` in recorded order: `stream` is `stdout` or
`stderr`, `ts` when the line was written (RFC 3339 UTC, milliseconds). `--stream stderr` keeps
one stream, `--timestamps` puts the time before each text line. `--step report` selects a
function name; use an exact node id if names collide, or a partitioned id to select one key.
`--env dev` reads that environment's history. It reads no pipeline source and contacts no
store. Unknown runs/steps and ambiguous names exit 2; silent executed steps return no lines.
Lines written before a failed step are retained. Recording stops after the first complete user line that reaches or exceeds 16 MiB of
cumulative text; the crossing line and a marker are retained, and framework notes may follow.
This is a soft threshold, not a hard byte or memory bound.

`barca logs -f` follows a run: it prints lines as they arrive (one JSON object per line with
`--json`) and exits when the run ends, 0 when it succeeded, 130 when it was cancelled, 1
otherwise. Without a run id it follows the newest running run.

```bash
barca run report pipeline.py        # in one terminal
barca logs -f --step report         # in another
barca logs --step report --stream stderr --timestamps   # after a failure: its logging and traceback
```

With a server configured (or `--server`), `barca logs` reads the server's runs, scheduled ones
included; `--local` reads this machine's (`barca docs logs`, "Debugging a live pipeline").

Task return values remain usable through `barca run --json`, `-o value`, and `barca.run(...)`.
During get/run, non-JSON modes print stdout lines to stderr with `[node_id]` prefixes;
`--agent` framework lines keep `[barca]` prefixes. JSON output suppresses captured prints;
retrieve them with `barca logs` using the result's run id.

See [Doctor and local debugging](/reference/doctor/) for prerequisite checks and a local-only debugging profile.
