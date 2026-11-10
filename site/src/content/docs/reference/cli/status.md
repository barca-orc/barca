---
title: "barca status"
description: "barca status: every node's cache state and why, its last run, its artifact's shape, --problems and --state."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/status.md
---

<!-- Generated from crates/barca-cli/docs/status.md (barca docs status). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

One read-only view of every node: what it is, whether it is cached and why, when it last ran,
and what its artifact looks like. It answers in one call what would otherwise take `barca list`,
`--dry-run`, `barca history` and a script that opens the artifact. After one `barca get`,
`barca status pipeline.py` prints:

```
NAME    KIND   STATE        WHY           LAST RUN                               SHAPE            DEPS
orders  asset  cached       materialized  success 2026-10-02 11:56:43 UTC 0.54s  3 rows x 2 cols  -
total   asset  cached       materialized  success 2026-10-02 11:56:43 UTC 0.00s  dict (1 key)     orders
notify  task   always-runs  task          -                                      -                total

2 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 1 always run
```

The forms it takes:

```bash
barca get total pipeline.py
barca status pipeline.py --pretty               # table (the default in a terminal)
barca status total pipeline.py                  # only `total` and its upstream cone
barca status total,notify pipeline.py           # several targets: the union of their cones
barca status pipeline.py --json                 # one JSON document (the default when piped)
barca status pipeline.py --fields id,cache      # JSON with only these keys per node
barca status pipeline.py --json --sample 2      # plus up to 2 sample rows per json/parquet artifact
barca status pipeline.py --problems             # only the nodes that need attention (below)
barca status pipeline.py --state stale,never_run   # only nodes in these cache states
```

The examples run on this `pipeline.py`:

```python
import pandas as pd
from barca import asset, task

@asset()
def orders() -> pd.DataFrame:
    return pd.DataFrame({"id": [1, 2, 3], "amount": [9.5, 3.0, 12.25]})

@asset(inputs={"df": orders})
def total(df: pd.DataFrame) -> dict:
    return {"total": float(df["amount"].sum())}

@task(inputs={"t": total})
def notify(t: dict) -> None:
    print(t)
```

Status runs nothing and records nothing; it reads the metadata DB as it is at that moment, so
during a run it already shows the steps that run has finished. Each node's fields, the JSON
document and the fine print are in [barca status: Fields and JSON](/reference/cli/status-fields/).

## Cache state

`cache.state` is the same decision `barca get --dry-run` makes (both call one function), so the
two cannot disagree. In JSON the states are snake_case, spelled exactly like the `summary` keys
(`never_run`, `always_runs`); the table prints them as `never-run` and `always-runs`.

| state | meaning | `reason` |
|---|---|---|
| `cached` | a successful result matches this code and these inputs; `get` serves it | `materialized` |
| `unsynced` | cached, but not where it should be: with an artifact store, this machine's copy, the store's object and the recorded digest do not all agree ([Remote Storage: `cached` and `unsynced` with a store](/reference/remote-storage/#cached-and-unsynced-with-a-store)), and `get` on it repairs that without running a step; with a configured server, the server has no result but this machine computed one whose post to the server's ledger is pending ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)) | `local_only`, `store_only`, `mismatch`, `not_in_ledger` (the table prints `local only`, `store only`, `mismatch`, `not in ledger`) |
| `stale` | it ran before, but `get` would run it again | `changed`, `upstream_stale`, `failed`, `artifact_missing`, `evicted` |
| `never_run` | no successful materialization is recorded | `no_record`, `failed` |
| `partial` | a partitioned asset with some keys cached | `partitions_missing` |
| `blocked` | a partitioned asset whose missing keys wait on the same keys of an input, which failed: a run computes its other keys and skips these ([Partitions: Failed keys block their own keys](/reference/partitions/#failed-keys-block-their-own-keys)) | `upstream_failed` (`detail`: `39 of 39 missing keys wait on 'demand_forecast': the same keys of it failed`) |
| `unknown` | dynamic partitions (`partitions_from`) whose source has not run yet, an asset reading a sensor with no recorded output, or an asset reading an `@asset(cutoff=True)` that would run first (and what depends on any of them); `not_planned` when the dry run status is computed from has no step for the node | `partitions_unknown`, `sensor_output_unknown`, `cutoff_pending`, `not_planned` |
| `always_runs` | tasks and sensors are never cached | `task`, `sensor` |

- `cache.change_causes` (on a `stale` node, and a failed one that once succeeded) sorts the same
  evidence by cause: `code_changed`, `inputs_changed`, `env_changed`, `upstream_stale`. It is
  absent when the last success recorded no components. `cache.reason` keeps its values.
- `changed`: the run hash differs from the last successful materialization. `cache.changed` lists
  what differs, and `detail` and the table's WHY column name the first and count the rest
  (`changed: helper pipelines/common.py:bucket_ltv (+1 more)`):

  | entry | what changed |
  |---|---|
  | `code` | the function, from `def` on |
  | `decorator argument NAME` | an argument of `@asset` / `@sensor` / `@task` that counts (`serializer`, `inputs`, ...; [The Cache](/reference/cache/)) |
  | `decorator NAME` | another decorator that counts (`@sink`, `@functools.lru_cache`, ...) |
  | `helper FILE:NAME` | a definition the function uses, in its own file or a project module (`pipelines/common.py:bucket_ltv`); `helper FILE` for a module it uses whole |
  | `input ID` | an upstream's run hash: its code or its own inputs changed |
  | `sensor ID` | a sensor it reads returned a new value (or its code changed) |
  | `value ID` | a dict `partitions_from` source returned a new value for this key (or its code changed; [Partitions](/reference/partitions/)) |
  | `env NAME` | a declared environment variable's value |
  | `kind` | the node became an asset, task or sensor |

  An entry appears when it was added or removed too (a helper the function now uses). Each
  materialization records a short digest of each of these; a result recorded by barca before
  0.29.0 has none, and the node is then a plain `changed` with no `cache.changed`. A `stale`
  node with reason `upstream_stale` carries `cache.changed` as well when it can be told.
- A node that reads a `@sensor` is judged by the sensor's last recorded output: status runs
  nothing, so it assumes the sensor returns the same value next time, and `detail` says so. If
  the sensor recorded a new value since the node last ran (for example a scheduled sensor), the
  node shows as `stale` ([External Data and Early Cutoff: External data that changes in place](/reference/external-data/#external-data-that-changes-in-place)).
- A `partial` node's `partitions` has `changed` (experimental) when a key of `missing_keys`
  ran before: key -> what changed in its run hash, in the entries above. A new key has no
  entry. With keys from a dict sensor, `{"retailer=r2": ["value pipeline.py:versions"]}` says
  the sensor returned a new value for that retailer.
- `upstream_stale`: an input is itself not cached, so this node's inputs will change when it runs.
  `detail` names the input.
- `failed`: the last attempt at exactly this code and these inputs raised; `detail` carries the
  message.
- `artifact_missing`: the result is recorded and its run hash is unchanged, but the artifact file
  is gone and a run over these nodes would have to read it (a step that runs takes it as an
  input, or it is the output the run returns: a target, or with no target the last asset), so
  it would be computed again. A missing
  artifact that nothing would read does not make a node stale: it stays `cached`, and
  `cache.artifact` then names a file that is not there ([Run Reports and Missing Artifacts: A cached result whose artifact is missing](/reference/artifacts/#a-cached-result-whose-artifact-is-missing)).
- `evicted`: the same, because the cache size limit deleted the artifact file ([Disk Use](/reference/disk/)). A result the artifact store holds is not stale: a run downloads it again.

- `local_only`, `store_only`, `mismatch`: only with an artifact store. `local_only`: the result
  is on this machine and not in the store, or its upload is pending (`detail` then carries the
  upload's last error); the next `get` or `run` uploads it. `store_only`: in the store, not on
  this machine; `get` fetches it. `mismatch`: both copies are there and differ in size (or in
  bytes, for a store that is a plain directory), or this machine's copy is not the one
  recorded. When this machine's copy is the recorded one, `get` uploads it again, with the
  changed-bytes warning; otherwise `get` fetches the store's copy, warns once and records its
  digest, so the result is `cached` afterwards ([Remote Storage](/reference/remote-storage/)).
  A result in neither place is `stale` with `artifact_missing`. Object stores give no checksum,
  so a same-size object with other bytes is not noticed there. A partitioned asset is
  `unsynced` (`local_only`) only when one of its keys has a pending upload. A store that cannot
  be asked leaves the state as it is, with a note in `detail`. The summary line counts these
  nodes (`1 cached, 1 unsynced, 0 stale, ...`) and `summary.unsynced` is always present.

`cache.run_hash` is the cache key for the current code and inputs; `cache.artifact` is the file a
`get` would serve (only when `cached` or `unsynced`).

### With a server configured

With `BARCA_SERVER` or `[server] url` set, `barca status` (without `--server` or `--local`) is
answered by the server's ledger for its deployed code, after `[barca] server <url> (<source>)`
on stderr ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)). Each node gets `local`: `true` when this machine has a copy
of the result the server would serve, which is what `barca sql` can query here. A node the
server has no result for, but whose result this machine computed and has not posted yet (the
post was refused or the server could not be reached), is `unsynced` with reason `not_in_ledger`
and `local: true`; the next `get` or `run` against the server posts it. `summary.unsynced`
counts unsynced nodes. A partitioned node's `local` is `false` (it has no single artifact).
`barca status --local` shows this machine's own state.

## Finding problems: `--problems` and `--state`

`--state <state[,state...]>` keeps the nodes whose `cache.state` is one of those named, spelled
as in JSON (`never-run` is accepted for `never_run`). An unknown state is a usage error (exit 2)
naming the valid ones; `failed` is not a state (a failed node is `stale` or `never_run` with
reason `failed`), and the error says to use `--problems`.

`--problems` keeps every node that needs attention:

- every state but `cached` and `always_runs`: `unsynced`, `stale`, `never_run`, `partial`,
  `blocked` and `unknown`. A node that never ran counts: nothing it would serve exists yet;
- a task or sensor (`always_runs`) whose last run failed;
- a node whose schedule is paused or failing (below).

A cached node and a task that last succeeded are fine. Given both, a node must pass both
(`--problems --state stale` is the stale nodes). The filter applies after the targets' cones and
before `--limit`; `total` counts the nodes that passed. The **summary still counts every node in
scope**, so the counts do not change with the filter, and the JSON names the filter in `filter`
(`{"problems": true}`, `{"states": ["stale"]}`); it is absent without one. The table says how many
nodes matched before the summary line, or `No node matches --problems.` when none did:

```bash
barca status --problems --json --fields id,cache,last_materialization,schedule
```

```
NAME          KIND   STATE        WHY                                                       LAST RUN                               SHAPE         SCHEDULE                    DEPS
nightly_sync  task   always-runs  last run failed: RuntimeError: upstream API returned 503  failed 2026-10-22 22:51:16 UTC         -             */2 * * * * *; failing 2/2  -
prices        asset  stale        changed: code                                             success 2026-10-22 22:51:12 UTC 0.00s  dict (1 key)  -                           -
ratio         asset  never-run    failed: ZeroDivisionError: division by zero               failed 2026-10-22 22:51:12 UTC         -             -                           orders

3 of 4 nodes match --problems; the counts below are for every node

1 cached, 1 stale, 1 never run, 0 partial, 0 unknown, 1 always run
```

A node whose last run failed shows the first line of its error in WHY: `failed:
ZeroDivisionError: division by zero` for reason `failed`, `last run failed: ...` for a task or
sensor, and `<reason>; last run failed: ...` otherwise (a cached node is not marked). The whole
message, with the traceback, is `last_materialization.error` in JSON; `barca logs RUN_ID --step
NODE --stream stderr` has the step's own stderr.

With a server configured, the server answers and the filter is applied here, after this
machine's `not_in_ledger` results are marked, so `--state unsynced` finds them. `GET
/nodes?state=stale&problems=1` filters the same way over HTTP.

## Schedules

A node with `freshness=Schedule(...)` has `schedule` in JSON (absent on other nodes), and the
table adds a SCHEDULE column when a node it shows is scheduled:

| key | meaning |
|---|---|
| `cron` | the cron expression |
| `next_fire` | when it next matches (RFC 3339, UTC) |
| `last_fired` | the last tick fired (RFC 3339, UTC), or `null` |
| `last_status` | the status of the run that tick started, while the server remembers it (`null` after a restart) |
| `failures`, `max_failures`, `failing` | consecutive failed ticks, `Schedule(..., max_failures=N)` (default 5), and `failures >= max_failures`: ticks are skipped ([Scheduling With Cron](/scheduling/)) |
| `paused`, `paused_until` | held back by `POST /schedule/{id}/pause`; `paused_until` is `null` for a pause until resumed |
| `skip_reason` | why the last due tick was skipped: `in_flight` or `paused`, or `null` |
| `live` | `true` when a running scheduler reported this entry |

The SCHEDULE cell is `<cron>; failing 2/2`, `<cron>; paused` (or `paused until <time> UTC`), or
`<cron>; next <time> UTC`, with `1 failure, ` before `next` while failures are counting.

- **From a server** (configured, or `--server`): the server's scheduler's view, the same numbers
  `GET /schedule` and the web UI's Schedules page show, with `next_fire` in the server's
  `--timezone`, and `live: true`. A server started with `--no-schedule` or `--read-only` fires
  nothing: its entries come from its metadata DB as below, with `live: false`.
- **On this machine** (`--local`, or no server): no scheduler runs, so the entry is what the
  metadata DB holds: the failures and pauses a `barca serve` of this `.barca/` recorded, and the
  last tick it fired. `next_fire` is the next match in this machine's local time, as `barca
  list` computes it. `last_status` and `skip_reason` are `null` and `live` is `false`. A laptop
  that never ran `barca serve` shows `failures: 0` and `paused: false`.

## Finding and reading a failed run

`barca history` filters its runs; the filters combine with AND, and `--limit`, `total` and
`truncated` count the runs they keep:

- `--status success,failed,...`: any of `success`, `failed`, `cancelled`, `error`, `running`,
  `interrupted`.
- `--target NAME`: runs whose target (as typed) or job (resolved node ids) is that node, by
  function name or node id. A run of every node (`all`) is left out.
- `--triggered-by manual,api,schedule,client`.
- `--since WHEN`: started at or after a duration back from now (`30m`, `24h`, `7d`, `2w`), an
  RFC 3339 time or a UTC date (`2026-10-22`).

A failed run's row ends with its first failed step and the first line of its error, and JSON
carries it as `failure: {node, error}` and again as the flat `failed_node` and `error` (`null`
for every other run, and for a run that failed before any step did, such as a file that does
not parse), so `--fields run_id,failed_node,error` needs no nesting.

```bash
barca history --status failed --since 24h
barca history --status failed --target clean --json --fields run_id,started_at,failure
barca history --status failed --json --fields run_id,failed_node,error
barca history --triggered-by schedule --status failed,interrupted
```

`barca history RUN_ID` shows one run: its own fields (status, target, job, timing, attempts,
what started it, the schedule tick and `client_run_id` when it has them), then each step it
recorded with its status, attempts, time, peak memory (`PEAK_RSS`), run hash and artifact
(path, format and size), then each failed step's error, its full traceback, when its worker
died how the process ended (`worker exit: SIGKILL (possibly the OOM killer)`, another signal,
an exit code), and the last 20 lines the step printed before it failed (stdout and stderr in
the order they were written, without the traceback). A hint on stderr names the command that
prints all of the failed step's stderr. JSON is `{run, steps, hint?}`: `run` is the run as
`barca history --json` lists it, and each step has `node_id`, `partition_key`, `status`
(`success` or `failed`), `attempts`, `elapsed_seconds`, `cpu_seconds`, `max_rss_bytes`,
`run_hash`, `artifact_path`, `artifact_format`, `artifact_size_bytes`, `error` (as recorded),
`error_type`, `error_message`, `error_traceback`, `error_phase` (where it failed: `user_code`,
or barca's own `load_inputs`, `split_output`, `write_output` or `sink`; `null` when unknown),
`worker_exit` and `output_tail` (a failed step's last lines, `null` for the others). A step that
failed in barca's own code says so: `p.py:clean failed in barca while writing its result
(phase write_output): ...`, followed by a traceback that keeps barca's frames.

`max_rss_bytes` is the peak resident memory of the worker process while the step ran, from the
start of the step to the end of writing its result, so a returned DuckDB relation that does
its work while it is written is counted. It includes what the worker already held (Python,
imported modules, a DuckDB connection's buffers). On Linux each step's peak is its own (barca
resets the kernel's peak counter, `VmHWM`, when a step starts); elsewhere it is known only for
a step that raised the worker's peak, and is `null` otherwise. A failed step has none. Rows
recorded before 0.31 hold the worker's peak over every step it had run, not the step's own.
Bytes DuckDB spilled to disk are not recorded: DuckDB keeps no total or peak of them, and its
temporary files are gone when the query ends, so only sampling during the step could find
them.

```bash
barca history 7f30b3e9
barca history 7f30b3e9 --json
barca logs 7f30b3e9 --step clean --stream stderr
```

Replace the example id with a `run_id` from `barca history`. Only the steps a run executed or
that failed are listed: cached steps are counted in the run's `steps_cached` (they recorded no
new row), and steps a failure skipped are not recorded. With a server configured, both forms
answer from the server ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)); a run id only this machine has is an error
whose remediation is `barca history RUN_ID --local`.

## Fields, JSON and fine print

Each node's fields (last materialization, artifact shape, run timing, what started a run), the
JSON document, output limits and what status asks an artifact store are in [barca status: Fields and JSON](/reference/cli/status-fields/).

See also: [The Cache](/reference/cache/), [barca logs](/reference/cli/logs/), [barca sql](/reference/cli/sql/), [Scripts and AI Agents](/reference/agents/).

---

*The same text is in your terminal: `barca docs status`.*
