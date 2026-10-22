# Using barca from scripts and AI agents

Conventions that make barca easy to drive programmatically. Everything here is stable CLI
behavior. When stdout is not a terminal (a pipe, a subprocess, an agent) every result is JSON
without any flag. List-shaped output is bounded by default (see "Bounded output" below).

The complete surface (every command and flag, environment variable, exit code, JSON schema and
`--agent` line), each marked stable or experimental, and the rules for changing it are in
`barca docs contract`. It is checked against the real CLI in CI, so it matches this version.

For a short version to load once, read `barca docs skill`: the same rules in about 1500 tokens,
in the Agent Skills format (it is `SKILL.md` at the repository root). To install it as a skill,
save it as a file, for example `barca docs skill > .claude/skills/barca/SKILL.md`.

## Output format: JSON unless stdout is a terminal

`get`, `run`, `logs`, `list`, `status`, `history` and `stats` pick their stdout format by one rule, first match wins:

1. **A flag:** `--json` forces JSON, `--pretty` forces human output (tables, summaries).
   `get`/`run` also keep `-o json|pretty`, older spellings of `--json`/`--pretty`. `get` takes
   `-o path|value`, which choose what it returns (below); alone they print only that. `run`
   keeps `-o value`, which prints only the task's value. `--fields` implies JSON.
2. **`BARCA_OUTPUT=json` or `BARCA_OUTPUT=pretty`** in the environment (for CI or a shell
   profile). Any other value is a usage error (exit 2).
3. **The terminal:** stdout is a TTY → human output; anything else → JSON.

So `barca list pipeline.py` shows a table in your terminal, and `barca list pipeline.py | cat`
or a subprocess call gets JSON. Pass `--json` anyway in scripts: it states the intent
and survives someone setting `BARCA_OUTPUT=pretty`. `plan` always prints JSON and `docs` always
prints markdown (`barca docs --json` for JSON, and `--pretty` is accepted and changes nothing);
neither follows the rule.

```bash
barca list pipeline.py --json        # JSON even in a terminal
barca history --pretty               # a table even when piped
BARCA_OUTPUT=json barca get total pipeline.py
```

## Output contract

- **stdout** carries the result: one JSON object for `get`/`run`, the plan JSON for `plan`, and
  JSON for `list`/`history`/`stats` (whenever the rule above picks JSON). It is safe to parse.
- **stderr** carries progress (`[barca] 2/2 steps | done in 0.0s`), your own `print` output from
  steps in non-JSON modes, warnings and errors. JSON mode retains captured prints in
  `barca logs RUN_ID --json`; non-JSON modes prefix them with `[node_id]`. The progress bar draws only when stderr is a terminal; barca
  writes no ANSI colour or cursor codes to a stream that is not one. Use `--agent` for plain
  progress lines (`[barca] 1/2 ...`) on stderr instead of a progress bar.
- **Exit codes:** one per kind of failure, so you can decide what to do from the code alone.
  On failure stderr explains (see Errors below). stdout is empty, except that a failed step in
  JSON mode still prints a result line with `"status": "failed"` (and a run with several targets
  prints its full result; see below).

| Code | `kind`        | Meaning                                                                         | What to do                    |
|------|---------------|---------------------------------------------------------------------------------|-------------------------------|
| 0    |               | success                                                                         |                               |
| 1    | `step_failed` | a step of yours raised (traceback included); the run is recorded as failed      | fix the code, re-run          |
| 2    | `usage`       | bad flags or arguments, unknown target, task/asset misuse, unreadable or invalid `.py` file, invalid `--env` or barca.toml | fix the command |
| 3    | `infra`       | barca or its environment failed: metadata DB, worker pool, remote state, I/O    | not your code; retrying may help |
| 130  | `cancelled`   | stopped by Ctrl-C (SIGINT) or SIGTERM                                           | re-run                        |

A reader that stops early is not an error. `barca list | head -1` and
`barca list | grep -q name` exit 0 (also under `set -o pipefail`), and a `get` or `run` whose
output nobody reads still finishes and is recorded, so
`barca run deploy pipeline.py 2>&1 | head -20` runs the whole deploy (`barca docs contract`,
"A closed stdout or stderr").

A decorator argument barca does not define (`@asset(after=other)`, `input=` for `inputs=`) is a
`usage` error on every command that reads the file; the envelope names the node, the line, the
argument and the accepted ones (`barca docs assets`, "Accepted arguments").

```bash
barca get total pipeline.py --agent > result.json 2> progress.log
echo $?
```

## Errors

In JSON output mode (whenever the output rule above picks JSON: piped or captured stdout,
`--json`, `-o json` or `BARCA_OUTPUT=json`; `plan` always; `docs` with `--json`) an error is
**one JSON line** on stderr (a failed run adds its summary afterwards):

```
{"code":2,"error":"Asset 'nope' not found. Available: pipeline.py:src, pipeline.py:total, pipeline.py:clean","kind":"usage","remediation":"Run `barca list pipeline.py` to see available assets and tasks."}
```

| Field          | Always | Meaning                                                              |
|----------------|--------|----------------------------------------------------------------------|
| `error`        | yes    | what went wrong                                                      |
| `code`         | yes    | the exit code (table above)                                          |
| `kind`         | yes    | `usage`, `step_failed`, `infra` or `cancelled`                       |
| `remediation`  | yes    | what to do next, often a command to run                              |
| `node`         | `step_failed` | the failing step's id, e.g. `pipeline.py:clean`               |
| `traceback`    | `step_failed` | the Python traceback of your code (barca frames removed), or null |
| `artifact_dir` | `step_failed` | where that step's artifacts are stored (a path or remote URI); it may not exist if the step never succeeded |

A failed step looks like this (one line; wrapped here):

```
{"artifact_dir":".barca/artifacts/pipeline.py--clean","code":1,
 "error":"step 'pipeline.py:clean' failed: ZeroDivisionError: division by zero","kind":"step_failed",
 "node":"pipeline.py:clean","remediation":"Fix the error in 'pipeline.py:clean' (see the traceback) and re-run the same command. Steps that succeeded are cached and will not re-run.",
 "traceback":"  File \"/abs/path/pipeline.py\", line 11, in clean\n    return x / 0\n           ~~^~~"}
```

A worker process that dies while it runs a step (killed by the kernel's out-of-memory killer,
a segfault in a C extension, `os._exit()` in your code) fails that step with `kind`
`step_failed` and exit 1, and the error names how the process ended, in the result JSON, the
envelope, the `step:` line and the final `run failed:` line:

```
worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)
worker for pipeline.py:train exited with SIGSEGV
worker for pipeline.py:train exited with code 3
```

SIGKILL is what the out-of-memory killer sends, so check the container's or host's memory
limit (`dmesg`, `docker inspect`, the pod's `OOMKilled` status) when you see it; several heavy
steps running at once in separate workers add up. Give those steps a `weight=` so fewer run at
once (`barca docs assets`, "Heavy steps"). A worker that was frozen waiting for its
`parallel()` branches adds `while it waited for its parallel() branches`. When the exit
status is not available within a few hundred milliseconds of the connection closing, the
error is the plain `worker disconnected`. There is no traceback, since the process could not
write one; retries (`retries=`) apply as for any failed step.

Read the last JSON object on stderr with `kind` and `error`; other lines are progress,
your steps' own output and the final run summary.
In human mode (a terminal, `--pretty`, `-o pretty`, `-o path` or `-o value`) the same error is plain prose with the
remediation before the final run summary. Errors never go to stdout. From Python, `barca.BarcaError` carries
the envelope as attributes: `kind`, `code`, `remediation`, `node`, `traceback`, `artifact_dir`.

When a step fails in JSON mode, `get`/`run` still print one result line on stdout, so you can
read the outcome without parsing stderr: `{"status": "failed", "failed_node": ..., "error": ...,
"run_id", "steps", ...}`, where the failed step's `status` is `failed`. A successful result has
`"status": "success"`. After the error, stderr ends with one greppable line:
`[barca] run failed: step 'pipeline.py:clean' failed: ZeroDivisionError: division by zero; 0 downstream steps skipped (exit 1)`.

`get`/`run` JSON fields: `status` (`success`, or `failed` as above), `run_id`, `elapsed_seconds`,
`steps_executed` (0 means everything was a cache hit), `phases`, `steps` (what happened to each
step: `status` ran/cached/partial and why, plus `env`, the declared environment variable values
used, for nodes with `env=[...]`, and `artifact_mismatch: true` when the artifact store's copy
of the step's result, or of an input it read, is not the one that was recorded: `barca docs
remote`),
`final_output`. On `run`, `final_output` is the task's value for a json artifact and
`{"_barca_artifact": {"path", "format", "size_bytes"}}` for parquet and pickle
(`barca docs types`). On `get` it is always that pointer, whatever the format; see the next
section.

### What `barca get` returns: paths by default

`barca get` returns where its result is, not the result: the artifact path, one per target and
one per partition key. Read the file, query it with `barca sql`, or ask for the value:

| Command | stdout |
|---|---|
| `barca get total pipeline.py` in a terminal | a summary line, then the path; a small json value (up to 10 lines) is shown under it |
| `barca get total pipeline.py --json` (or piped) | the JSON result; `final_output` is `{"_barca_artifact": {"path", "format", "size_bytes"}}` for every format |
| `barca get total pipeline.py -o path` | only the path, one per line (for `xargs`, `$(...)`) |
| `barca get total pipeline.py -o value` | only the value, pretty-printed |
| `barca get total pipeline.py -o value --json` | the JSON result with the value in `final_output` |
| `barca get total pipeline.py -o url` | a signed HTTPS link per result in a cloud store (experimental; `barca docs remote`) |

`-o path` is the default spelled out: with `--json` it prints the default JSON. `-o value` reads
json, parquet, CSV and pickle with the reader `barca.get()` uses; a table becomes a list of row
objects, and dates become ISO strings. A value that cannot be read (a pickle of a class that is
not importable, parquet without pyarrow or pandas) keeps its pointer, with a warning on stderr.

```bash
barca get total pipeline.py -o path | xargs cat     # the json artifact itself
barca get total pipeline.py -o value                # just the value
```

Up to 0.24, `final_output` was the value for a json artifact and the pointer for any other, and
a terminal showed the value: pass `-o value` (with `--json` for the JSON line) for that.

**A partitioned target returns every key**, sorted by key, never one of them. `final_output` is
an object keyed by partition key (`dim=value`, dimensions sorted and joined with `,`):

```json
{"final_output": {"region=amer": {"_barca_artifact": {...}}, "region=emea": {"_barca_artifact": {...}}}}
```

Under `-o value` each key maps to its value. `--partition region=emea,amer` selects keys across
the whole run: every partitioned node with that dimension, upstream and downstream, computes only
those keys, the target returns them, and `--refresh` refreshes only them. Compound keys take a
JSON object, `--partition '{"region": "emea", "day": ["2026-10-13", "2026-10-14"]}'`: a node is
narrowed by the named dimensions it has. A selected key has the run hash of a full run, so a full
run later serves it from cache. A `collect()` fan-in over a narrowed node, and what is below it,
is not run: a step with status `skipped`, reason `partition_selection`, one stderr note, exit
code unchanged. No target means every asset and sensor for those keys. The JSON result carries
`partition_selection` (`selection`, `nodes` with `selected`/`skipped` counts, `skipped_nodes`).
A dimension no node in the cone has, a value no node with that dimension has, or a target the
selection skips is a usage error (exit 2). The keys of a `partitions_from(<list>)` node are known
only once its source has run; a selection that matches none of them returns `{}` with a warning
on stderr.

If some keys fail, the run is a failed step as before (exit 1): the result line has
`"status": "failed"` and `failed_node` names the first key to fail (when several keys fail, which
one that is can differ between identical runs). The keys that succeeded are
in `final_output`; the failed ones are absent and have no artifact, so the next `barca get`
computes them again. (Whether a partial failure should get its own exit code or list the
failed keys is not decided yet.)

```bash
barca get sales pipeline.py --partition region=emea -o value
barca get sales pipeline.py --partition region=emea,amer --refresh sales
barca get pipeline.py --partition region=emea --dry-run --json   # what runs for emea; skipped_nodes
```

### Several targets in one call

`barca run a,b pipeline.py` (or `barca get a,b ...`) runs every named target in one invocation:
one plan over the union of their cones, so shared upstream steps run once. Use it instead of N
calls. The output has the same run fields (`status` is `failed` when any target failed), but
`final_output` is replaced by `targets`, keyed by target name in the order given:

```json
{"status": "failed", "run_id": "...", "elapsed_seconds": 0.2, "steps_executed": 5, "phases": 2, "steps": [...],
 "targets": {"check_a": {"status": "success", "final_output": {"a_ok": true}},
             "boom": {"status": "failed", "failed_node": "pipeline.py:boom",
                      "error": "ValueError: check failed\n  File ..."}}}
```

- Every target runs even if another fails; a failure skips only the steps that depend on it
  (`"status": "skipped"`, reason `upstream_failed`, in `steps`). `failed_node` names the step that
  raised: the target itself or something upstream of it (the same key a failed single-target run
  uses).
- Exit code 1 if any target failed. stdout carries the full JSON (so you see which targets
  passed); stderr has the error envelope and final summary
  for the first failed target (`kind: "step_failed"`).
- One target (or a repeated name, `a,a`) gives exactly the single-target output.
- `--dry-run` with several targets reports the union once (`steps`, `summary`), and `targets`
  in place of `target`: an object keyed by target name in the order given, like a real run, each
  `{"summary": {"will_run", "cached", "unknown"}}` counted over that target's cone.
  `-o value` prints `{target: value}` (null for a failed target). On `get`, each target's
  `final_output` is its pointer (a mapping per key when partitioned), or its value under
  `-o value --json`, and `-o path` prints every target's paths, one per line.
- Every name is checked before anything runs: an unknown name, a task passed to `get`, or an
  empty name (`a,,b`) is a usage error, exit 2, and nothing runs.

```bash
barca run check_a,check_b pipeline.py --agent > result.json
barca run check_a,check_b pipeline.py --dry-run
```

## Parallel runs

It is safe to run several `barca` commands in one project at the same time: they queue briefly
on the metadata DB (see `barca docs cache`) instead of failing with a lock error.

## Long-running steps

Nothing is printed while a step is executing, so a slow step used to look hung. Any step that
stays in flight for 15 seconds or more is now reported on stderr, in every mode, and again on
each later interval:

```
[barca] still running (45s): pipeline.py:fetch_orders
```

Set `BARCA_PROGRESS_SECS` to change the interval (`0` turns it off). In `--agent` mode a step
appears as `[barca] step:<id> completed ...`, `cached`, or `failed: <first line of the error>`. If
neither a completion nor a "still running" line has appeared for much longer than your slowest
step, the process is genuinely stuck. The last progress line of a run that executed steps is the
same with and without `--agent`: `[barca] N/M steps | done in Xs`, or `| failed in Xs` when a step
failed (never "done").

## Repeated warnings

Workers write straight to barca's stderr, so a library that warns on every call would print the
same line hundreds of times: aiohttp logs `Could not parse .netrc file` for each client session
when `~/.netrc` is malformed, which is every Azure operation. Within one run a warning with the
same text is printed the first time only, by whichever worker reaches it first, and the rest are
counted on stderr just before the end-of-run line, in every mode:

```
Could not parse .netrc file
[barca] step:pipeline.py:fetch[k=a] completed 0.1s (1/9)
...
[barca] 79 more: Could not parse .netrc file
[barca] 9/9 steps | done in 0.5s
```

- What is collapsed: a `logging` record at exactly WARNING level that is printed only because
  logging is not configured (Python then writes the bare message to stderr), and a warning
  shown by the `warnings` module. Two warnings are the same when their text is the same.
- If the project configures logging (`logging.basicConfig()`, or any handler on the logger or
  one of its parents), nothing from `logging` is collapsed, library warnings included: every
  record is printed by your handlers, in your format. That is also how to see every line.
- What is never touched: `print` and direct writes to stdout or stderr, log records at any other
  level, a record logged with a traceback (`exc_info`), any handler you or a library installed
  (terminal or file), a step's error and traceback, barca's own `[barca]` lines and the error
  envelope.
- A warning seen once prints no summary line. The count is the number of suppressed repeats; repeats
  from a worker process that dies before its step reports (the step crashed the interpreter)
  are not in it.
- The ten most repeated texts get a summary line each; any others share one,
  `[barca] 502 more: 502 other repeated warnings` (suppressed lines, then distinct texts).
- A worker stops collapsing after 512 distinct texts, so warnings that differ every time (an id
  or a counter in the text) are printed as they come.

Fix the cause when you can (here: repair or remove `~/.netrc`); the summary line tells you how
much of it there was.

## Plan warnings

`plan`, `get`, `run` and `--dry-run` check the steps they plan before anything runs and report
what they find twice: one `[barca] warning: <message>` line per finding on stderr (every mode,
before the first step line), and the `warnings` array in the JSON on stdout, always present and
`[]` when there is nothing:

```json
{"kind": "unused_input", "node": "pipeline.py:report", "param": "raw", "message": "..."}
```

Read the array, not stderr. `unused_input` is the only kind so far: the step declares an input its
function never uses, which is still loaded and still part of its cache key. The fix is in the
message: use the input, remove it from `inputs=`, or rename the parameter with a leading `_` when
it is there for ordering only. Any of the three makes the warning go away; there is no flag or
configuration key that turns it off. A name that appears inside a string in the body (SQL, a
pandas `query` expression) counts as used. The list is the same on every run of the same command, cached or not, and never
changes the exit code (`barca docs assets`, "Unused inputs"; `barca docs contract`).

The array holds plan warnings only. What a run finds out while it runs is on the step it
concerns: `steps[].warning`, and for a store copy that differs from its recorded hash the marker
`steps[].artifact_mismatch` (`barca docs remote`, "Checking a local copy against the store").

## Environment variables

A node that declares `env=["SOURCE_CSV"]` has those values in its cache key (from barca's
environment, else from the project's `.env`), and each `--agent` step line ends with them so a
log records which inputs a run used:

```
[barca] step:pipeline.py:raw completed 0.0s (1/2) env API_TOKEN=<unset> SOURCE_CSV=b.csv
[barca] step:pipeline.py:raw cached env API_TOKEN=<unset> SOURCE_CSV=b.csv
```

`<unset>` means the variable was not set; secret-looking names (`*_TOKEN`, `*_SECRET`, `*_KEY`,
`*_PASSWORD`) show `<redacted>`. Values with spaces are double-quoted. Undeclared variables are
invisible to barca (`barca docs assets`).

## Refreshing: syntax and pitfalls

`get` and `run` share one vocabulary: `--refresh a,b`, `--no-cascade`, `--refresh-all`.

- Several assets are one comma-separated list: `--refresh a,b`. Never `--refresh a b`.
- `--refresh a` re-runs `a` and every asset downstream of it in the target's cone (reason
  `refresh_cascade`). `--no-cascade` re-runs only what you name; cached downstream assets then
  do not reflect the refresh and barca warns on stderr. Details: `barca docs cache`.
- `--refresh-all` re-runs every asset in the cone. The deprecated `--no-cache` alias was
  removed in 0.24.0; it is a usage error (exit 2). Python uses `refresh_all=True`.
- On `get` the target itself may be named (`barca get total pipeline.py --refresh total`).
- An unknown name is an error (exit 2, `kind: usage`) listing the valid upstream assets.

## Inspect before you run

Preview any `get`/`run` with `--dry-run`: it says which steps would run, which come from cache,
and why, without executing steps or recording a run (`barca docs cache`):

```bash
barca run report pipeline.py --dry-run --refresh src
```

One call answers "what is here and what state is it in": `barca status` gives, per node, its
kind and inputs, its cache state with the reason (the same decision `--dry-run` makes), its last
materialization, and the artifact's row count and columns, without executing steps
(`barca docs status`):

```bash
barca status pipeline.py --json                 # every node: cache state, last run, shape
barca status total pipeline.py --json --sample 3   # one cone, with 3 sample rows per artifact
barca status total,orders pipeline.py --json    # several targets: the union of their cones
```

```bash
barca list pipeline.py --json       # {nodes: [{id, kind, freshness, inputs, env}], total, truncated}; freshness: always|manual|schedule
barca plan pipeline.py              # phases and steps that would run, nothing executes
barca history --json                # {runs: [...], total, truncated}: every recorded run unless --limit is given
barca logs --json                   # the most recent run's recorded stdout and stderr; stderr names the run
barca logs -f --json                # follow the newest running run, one JSON object per line
barca stats total pipeline.py --json  # timings and re-executions for one asset
```

Source parsing is static and planning executes no pipeline step. `partitions(<expression>)`
can evaluate Python and import its module while resolving keys.

`.barca/` is kept under a size limit (`[cache] max_size` or `BARCA_CACHE_MAX_SIZE`, default 10%
of the filesystem): after a `get` or `run` it may evict results the command did not produce,
read or return, and stderr says so. An evicted result is fetched or computed again when needed
(reason `evicted`). To see or free space on demand:

```bash
barca gc --dry-run --json           # {size_bytes, limit_bytes, after_bytes, tiers, evicted, ...}: nothing is deleted
barca gc --json                     # evict now, down to 80% of the limit
```

For an old or container-owned run that still reads `running` after you verified it stopped,
use `barca history --reconcile RUN_ID --json`. It records `interrupted`, prints just that
run using the normal history envelope, preserves successful results, and refuses a proven
live owner. Ordinary history inspection stays read-only. See `barca docs cache`.

## Debugging pipelines

To find what is wrong and why, ask in this order. Each step is read-only: nothing runs, nothing
is written. With a server configured (`barca docs server-mode`), every command below answers
from the server, after `[barca] server <url> (<source>)` on stderr; add `--local` for this
machine's own records, or `--server <url>` for another server.

1. **`barca doctor`**: is the setup sound? Exit 0 when every check passes, 3 when one fails,
   and each failed check's detail ends with what to do. With a server it also checks the
   server: its version and API version against this client, its store against this project's
   `[remote]`, whether it is draining, the files it failed to load, its scheduler (schedules
   paused or failing) and the runs this machine has not posted to it yet (`barca docs doctor`).
2. **`barca status --problems`**: which nodes need attention, and why. Every state but `cached`
   and `always_runs`, a task or sensor whose last run failed, and a schedule that is paused or
   failing. A failed node's error is `last_materialization.error` in JSON and its first line
   is in the WHY column; a scheduled node has `schedule` (`failures`, `max_failures`,
   `failing`, `paused`, `next_fire`). The summary still counts every node (`barca docs status`).
3. **The failed run**: `barca history` lists runs newest first, with `run_id` and `status`.
4. **`barca logs RUN_ID --step NODE --stream stderr`**: the step's traceback, warnings and
   logging, as recorded (`barca docs logs`).
5. **Fix, then re-run the same command.** Steps that succeeded stay cached. For a schedule that
   reached `max_failures`, a successful `barca get NODE --server` (`barca run` for a task) resets
   the count and its ticks resume.

```bash
barca doctor --json                                 # {ok, checks: [{name, ok, detail, data}]}; exit 3 if a check failed
barca status --problems --json                      # only what needs attention; summary counts every node
barca status --state stale,never_run --json         # only these cache states
barca status --problems --json --fields id,cache,last_materialization,schedule
barca history --json --fields run_id,target,status,started_at
barca logs 843a9c91af56 --step ratio --stream stderr   # the traceback of one step
barca status --problems --local                     # this machine's own state, with a server configured
```

Against a server with an asset that raises, an asset whose code changed, and a scheduled task
that failed `max_failures=2` ticks in a row:

```text
$ barca status --problems
[barca] server http://127.0.0.1:8391 ([server] url in barca.toml)
NAME          KIND   STATE        WHY                                                       LAST RUN                               SHAPE         SCHEDULE                    DEPS
nightly_sync  task   always-runs  last run failed: RuntimeError: upstream API returned 503  failed 2026-10-22 22:51:16 UTC         -             */2 * * * * *; failing 2/2  -
prices        asset  stale        changed: code                                             success 2026-10-22 22:51:12 UTC 0.00s  dict (1 key)  -                           -
ratio         asset  never-run    failed: ZeroDivisionError: division by zero               failed 2026-10-22 22:51:12 UTC         -             -                           orders

3 of 4 nodes match --problems; the counts below are for every node

1 cached, 1 stale, 1 never run, 0 partial, 0 unknown, 1 always run
```

```text
$ barca logs 843a9c91af56 --step ratio --stream stderr --pretty
[barca] server http://127.0.0.1:8391 ([server] url in barca.toml)
Traceback (most recent call last):
  File "/srv/project/pipeline.py", line 11, in ratio
    return orders["n"] / 0
           ~~~~~~~~~~~~^~~
ZeroDivisionError: division by zero
```

`barca doctor` on the same laptop, whose `[remote]` is not the server's store and whose server
has a file that does not parse (the lines about this machine are left out):

```text
$ barca doctor
[barca] server http://127.0.0.1:8391 ([server] url in barca.toml)
...
[ok] server: http://127.0.0.1:8391 ([server] url in barca.toml) answers
[ok] server_version: barca 0.29.3, API version 1, the same as this client
[error] server_store: the server writes artifacts to /srv/store/default/artifacts, this project to /home/me/store2/default/artifacts: a result one of them records could not be read by the other; Set [remote] uri in barca.toml to the uri the server's barca.toml has, and use the server's environment.
[ok] server_health: serving
[error] server_sources: the server did not load 1 file, so its nodes are missing there: broken.py: Parse error: syntax error in broken.py: line 1, column 12: Expected a parameter or the end of the parameter list; Fix the file and deploy it again: a `barca serve --watch` reloads it, otherwise restart barca serve.
[error] server_scheduler: running; 1 schedule, 1 failing (pipeline.py:nightly_sync): pipeline.py:nightly_sync failed 2 ticks in a row (max_failures=2) and its ticks are skipped; Run `barca status --problems` for the error and fix it, then `barca run pipeline.py:nightly_sync --server`: a success resets the count and the ticks resume.
[ok] server_posts: every run this machine computed against the server is recorded there
```

Coming in 0.29.3 (not in this build yet): filters and a detail view on `barca history`, and
`barca sql` over the run history, against a server too. Until then, filter `barca history
--json` with `jq`:

```text
barca history --status failed                       # only failed runs
barca history RUN_ID                                # one run: its steps, errors and timings
barca sql "select run_id, target, status, started_at from barca.runs where status = 'failed'"
```

## Bounded output: --limit, --all, --fields

`list`, `status` and `history` return every item in JSON unless `--limit N` is explicit.
This includes piped output, `--fields`, and `BARCA_OUTPUT=json`. Human tables show at most
100 nodes for `list`/`status` and the 10 most recent runs for `history`; `--all` removes
these bounds. JSON keeps the envelope that says whether you saw everything:

```json
{"nodes": [...], "total": 312, "truncated": true,
 "hint": "pass --limit N for more, or --all for all 312 nodes"}
```

`truncated` and `total` are always present; `hint` only when `truncated` is true. The human table
prints the same hint as one line on stderr, so stdout stays just the table.

```bash
barca list pipeline.py --limit 20   # first 20 nodes
barca list pipeline.py --all        # every node
barca history -l 50 --json          # last 50 runs
barca history --all --json          # every recorded run
```

`--fields a,b` keeps only those keys on each item: `nodes` for `list`, `runs` for `history`,
`recent_runs` for `stats`, `steps` for `get`/`run` (including `--dry-run`), and `topics` for
`docs`. The rest of the JSON is unchanged. `--fields` implies JSON on every command, even in a
terminal; combining it with `--pretty` or `-o pretty` is a usage error, and so is `-o value`
on `run`. On `get`, `--fields` with `-o path|value` prints the JSON result with paths or values. A key that is valid but absent on an item (for example
`next_fire` on an unscheduled node) is simply omitted. An unknown key is a usage error (exit 2)
that lists the valid keys, and nothing runs; `barca <command> --help` lists them too.

```bash
barca list pipeline.py --fields id,inputs
barca history --fields run_id,status,elapsed_seconds
barca get total pipeline.py --fields id,status,reason
```

Limits bound how many items are printed, never what an item says: error messages and the Python
traceback of a failed step are always complete.

Breaking change after 0.9.0: `list --json` and `history --json` used to print a bare array; they now print
the envelope above. Read `.nodes` / `.runs` (for example `jq '.nodes[].id'`).

## Getting values, not pointers

On the command line, `barca get total pipeline.py -o value` prints the value. From Python,
`barca.get` runs the command and deserializes the result:

```python
import barca

df = barca.get("orders", "pipeline.py")        # parquet artifact -> pandas DataFrame
total = barca.get("total", "pipeline.py")      # json artifact -> dict
by_region = barca.get("sales", "pipeline.py")  # partitioned -> {"region=amer": ..., "region=emea": ...}
emea = barca.get("sales", "pipeline.py", partition="region=emea")
barca.run("send_email", "pipeline.py", refresh=["report"])
```

`barca.plan`, `barca.history` and `barca.stats` return parsed dicts, and `barca.BarcaError` is
raised on failure; for `get`/`run`/`plan` its `kind`, `code`, `remediation` (and for a failed step
`node`, `traceback`, `artifact_dir`) come from the error envelope. Or read a parquet `path` directly with duckdb/pandas/polars.
`barca.signed_url("s3://bucket/key.csv", expires=3600)` (experimental) returns a time-limited
HTTPS link to any object in S3, GCS or Azure; `barca get <target> -o url` gives one for a result
(`barca docs remote`, "Sharing a result: signed URLs").

## Looking at cached results

To inspect data (which rows failed a check, what an asset holds), query it instead of writing a
script: `barca sql "select * from revenue where amount < 0" --json`. Every asset with a result is
a view named after its function; the JSON is `{columns, rows, total, truncated}`. It never runs a
step or imports user code. To recompute first, use `--refresh`; never delete files under
`.barca/`. See `barca docs sql`.

## Targets and files

- Files are optional: with none, barca reads every `.py` file in the project that imports barca
  (`barca list`, `barca run validate`). Files or directories narrow it (`barca get total
  pipelines/`); a directory as the first argument needs a trailing `/` (or `.`), otherwise it is
  read as a target name. See `barca docs discovery`.
- `barca get file.py` gets every asset and sensor (`final_output` is the last asset's). It never runs
  tasks (it used to): stderr names the skipped tasks and the `barca run` command. A file with only
  tasks gets nothing and exits 0 with `"steps": []`.
- `barca get name file.py [more.py ...]` gets one target; `name` can be the bare function name
  or the full id `file.py:name`. A name selects exactly that node: `deploy` never selects
  `prod_deploy`. A function name defined in more than one file is a usage error (exit 2) that
  lists the full ids to choose from. Cross-file inputs are ordinary imports (`from pipelines.sources
  import raw`, then `inputs={"r": raw}`); `asset_ref("path.py:fn")` names a node without an
  import. A bare input name defined in several files is an error listing the candidates.
- `barca get a,b file.py` / `barca run a,b file.py` take several targets in one run (see above);
  `barca status a,b file.py` shows the union of their cones.
- `barca file.py` is shorthand for `barca get file.py`.
- You can run barca from any directory inside a project with a `barca.toml`: barca changes into
  that directory (the project root) first, reads file arguments relative to where you typed
  them, and uses the root's `.barca/` cache. Node ids are relative to the root. See
  `barca docs cache` ("Where things live"). `barca list --json` and `barca status --json` carry the
  absolute `root`.
- `get` is for assets and `run` is for tasks; using the wrong one exits 2 and says which to use.
- The target comes before the files. If the first positional ends in `.py` and a later one does
  not, there is exactly one valid reading, so barca exits 2 and prints the corrected command
  (same files and flags) instead of running anything:

  ```
  $ barca run pipeline.py report
  error: the target comes before the files

    barca run report pipeline.py
  ```

  With more than one non-`.py` name after a file it states the rule and does not guess. An unknown
  or ambiguous target ends with ``Run `barca list <files>` to see available assets and tasks.``
  on every command; other usage errors carry their own fix (the corrected command, `barca run`
  for a task, the valid `--refresh` names) and no `barca list` pointer.
- A mistyped name gets "Did you mean ...?": a flag, a subcommand, a target, a `--refresh` name,
  a `barca sql` view, a `barca docs` topic and a decorator argument. The rule is the same
  everywhere: the closest names within one edit of what was written (two for a name longer than
  four characters), the best match only, or two when they tie; nothing when no name is that
  close. The suggestion is in the `remediation` (and on its own line in human mode), never in
  `error`, and it never changes the exit code: the command still exits 2 and runs nothing.
  barca does not act on a guess; check it before you retry, since a near name can be a
  different node.

  ```
  $ barca get amount pipeline.py --pretty
  Asset 'amount' not found. Available: pipeline.py:amounts, pipeline.py:report

  Did you mean 'amounts'? Run `barca list pipeline.py` to see available assets and tasks.

  $ barca statsu
  error: unrecognized subcommand 'statsu'

  Did you mean 'stats' or 'status'?
  ...
  ```

## Editing a barca project: a safe loop

1. Write or change the function with parameter annotations (`barca docs types`).
2. `barca list pipeline.py` — confirm the node, its kind and its dependencies were discovered.
3. `barca get <target> pipeline.py` — check exit code, `steps_executed`, and the artifact it
   points to (`-o value` prints the value).
4. Run it again — `steps_executed` should be 0 (cached). If not, something upstream changed.
5. `barca status pipeline.py --json` when you need to explain what is cached, what ran last, and
   what an artifact holds; `barca history --json` for past runs.

## Finding more

```bash
barca docs                    # topic index
barca docs <topic>            # one topic as markdown
barca docs --all              # the whole manual in one stream (paste into context)
barca docs --json             # topic index as JSON; add a topic for its full text
barca get --help               # flags and runnable examples (every command has --help)
```

Online: https://barca.sh/

Plan documents include `plan_version: 1`, independent of the package and database versions.
Workers and the transfer helper negotiate their socket protocol before receiving work.
A version mismatch exits 3 and names the helper version and expected version; reinstall
Barca in the Python environment used for the run. The handshake does not import pipeline code.

See also: `barca docs skill`, `barca docs contract`, `barca docs status`.
