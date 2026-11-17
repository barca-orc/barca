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
Health: 1 never run, 2 ok
Gap: 1 no refresh path (nodes); 1 with no estimate
Data: 1 of 2 assets measured
```

Under the cache counts, `Health` says whether anything needs doing, in the words `failing`,
`blocked`, `suspect data`, `behind`, `undecided`, `never run` and `ok` (here the task has no
recorded run). `Gap` says how far the scope is from fresh and why: the nodes that are not
fresh by cause (`failing`, `overdue`, `no refresh path`, `queued`, `undecided`, `catching
up`), then how many are expected fresh and within how long, and how many have no estimate
(here nothing scheduled will ever run the task); `Gap: every node is fresh` when none is
behind. `Data` says how many assets had their rows counted: `orders` is a frame, `total` a
dict, which has no row count. They are in [barca status: Fields and JSON: Health](/reference/cli/status-fields/#health), "Cause" and
"Data signals and coverage"; a last line reports the cache's size on disk. What explains the
nodes that are not fresh, and the command that clears each, is `barca status --rollup`
below.

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
barca status pipeline.py --rollup               # counts by health per group and file ("The rollup" below)
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

`cache.state` is the same decision `barca get --dry-run` makes (both call one function, with
the rules a run follows), so the two cannot disagree about a node a `get` would plan. In JSON
the states are snake_case, spelled exactly like the `summary` keys (`never_run`,
`always_runs`); the table prints them as `never-run` and `always-runs`.

A run reads a `Manual` node, and an asset pinned with `barca pin`, as it is and never
recomputes it ([Forcing a Recompute](/reference/refresh/), [barca pin and unpin](/reference/cli/pin/)). Status says so in two parts:

- The held node has its own state: `stale` when its code or what its inputs would give moved
  on. No run will recompute it; `barca get NAME --refresh NAME` (`barca run NAME` for a task,
  `barca unpin NAME` for a pin) does. `--dry-run` lists the same node as `cached`, with
  `Manual and stale: ...` in its `detail` or the reason `pinned`: that is the one node the
  two word differently.
- Everything below it has the state a run finds, reading the held node as it is. A node that
  is up to date against it is `cached`, and its `detail` (the WHY column) names the nearest
  stale held node above and the command that recomputes it: `up to date against 'baseline'
  as it is: 'baseline' is Manual and stale, and a run reads its last result without
  recomputing it; ...`. So `barca status`, `barca get --dry-run` and what a scheduled tick
  records (`last_check`) agree for every node below a `Manual` or pinned node, with or
  without an `@asset(cutoff=True)` in between. Once the held node is refreshed and has a new
  result, what reads it is `stale` (`changed`).

Status also lists the nodes above a held node, which a `get` of something below it does not
plan. A `Manual` node that has never been computed holds nothing: what is below it reads
`never_run` or `stale`, and a `get` of it exits 2 naming the node.

One difference between a request and a tick remains. A node paused with `barca pause` is
read as it is by scheduled ticks only; `barca get` recomputes it when it reads it. Status
says what a request would do, so below a stale paused node it says `stale` (as `--dry-run`
says `run`) while each tick finds the node up to date; what the last tick found is
`last_check` ([barca status: Fields and JSON: Last check](/reference/cli/status-fields/#last-check)), which
schedule's tick will next bring a node up to date is `refresh_path` ("Refresh path"), and since
when a node is out of date and when it is expected fresh or only rechecked are `stale_since`,
`expected_fresh_at` and `recheck_at` ("Times").

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
Health (nodes listed): 2 failing, 1 behind
```

The `Health` line counts the nodes listed, so it says so under a filter; the `Gap` and `Data`
lines that follow it are for every node in scope, like the counts. `--problems` selects by cache state,
last run and schedule as listed above, not by health: a task that never ran is `never run` and
is not a problem.

A node whose last run failed shows the first line of its error in WHY: `failed:
ZeroDivisionError: division by zero` for reason `failed`, `last run failed: ...` for a task or
sensor, and `<reason>; last run failed: ...` otherwise (a cached node is not marked). The whole
message, with the traceback, is `last_materialization.error` in JSON; `barca logs RUN_ID --step
NODE --stream stderr` has the step's own stderr.

With a server configured, the server answers and the filter is applied here, after this
machine's `not_in_ledger` results are marked, so `--state unsynced` finds them. `GET
/nodes?state=stale&problems=1` filters the same way over HTTP.

## One node's keys: `--keys`, `--overview`, `--grid`, `--key`

A partitioned node is one row with key counts. To see its keys, name it and pass one of
(experimental; [Partitions: Listing keys](/reference/partitions/#listing-keys-barca-status-node---keys---overview---grid---key)):

```bash
barca status scored --keys --state failed,blocked   # each key: state, last attempt, rows, error
barca status scored --keys --needs-run --value region=apac --range day=2026-01-01..2026-01-31   # filters: all must hold
barca status scored --keys --state failed --error 0 --match 2026-03 --sort attempted --limit 20 --offset 20
barca status scored --overview                      # counts, what is wrong and where, the grid's rows
barca status scored --grid                          # every key's state, one character each
barca status scored --key region=apac               # one key: its chain, attempts, what it wrote and read
```

`--keys` takes filters (`--state`, `--needs-run`, `--value DIM=V[,V]`, `--range DIM=FROM..TO`,
`--error N`, `--match TEXT`), an order (`--sort key|state|attempted|elapsed|size|rows`) and a
page (`--limit`, `--offset`); its output says how many keys match and the `barca get
--partition` line that computes them. Each view prints the same on this machine and from a
server, which filters, orders and pages the keys itself, and `--json` gives the documents the
web UI's key explorer renders (`--keys` and `--overview`).

## The rollup: `--rollup`

`barca status --rollup` prints, instead of the nodes, how they add up: one tree of everything
in scope, then each group ([Groups](/reference/groups/)), then, for nodes in no group, each source
file. Every level is the exact sum of what is under it, and every count names its unit:
`NODES` counts assets, tasks and sensors, `ASSETS` counts assets only, and the health words
count nodes. Within a level the worst health comes first. Under the tree it prints the gap
and then the to-do list: what holds the nodes that are not fresh, and what clears each. On a
project where `orders` (in the group `ingest`, with a sensor) failed and blocks two assets
below it:

```
SCOPE          NODES  ASSETS  HEALTH (nodes)
all                6       4  1 failing · 2 blocked · 3 ok · 3 with no estimate
  ingest           2       1  1 failing · 1 ok · 1 with no estimate
  pipeline.py      4       3  2 blocked · 2 ok · 2 with no estimate

Gap: 3 failing (nodes); 3 with no estimate

What holds the 3 nodes that are not fresh, and what clears it:
1. failure: orders
     holds 3 nodes (3 assets): 3 failing; 3 with no estimate
     why:  ValueError: 12 rows rejected
     do:   fix the failure: barca get orders

Data: 0 of 4 assets measured
As of 2026-11-17T16:49:57Z
```

Each entry of the list is a root: one upstream fact (a failing node, a sensor's new value,
changed code, a missing or paused schedule, a run queue that is behind) with every node it
explains, so three nodes that need attention here are one thing to fix. Every node that is not
fresh is under exactly one root. `holds` counts them by cause and adds their gap (`3h of
gap`: node-time out of date plus node-time until expected fresh) and how many have no
estimate; `why` is the fact in a line; `do` is the action that clears it, with the command
when there is one and the typical compute time of what it holds (`about 7m of compute`).
Roots whose nodes have no estimate come first, the one holding the most nodes first; then by
gap. With a server, a level of the tree whose nodes have estimates adds `fresh by ~15:05 UTC`
(the latest of them). `Data` is the scope's data coverage and `As of` the instant the nodes
were classified at. The kinds of root and of action are in [barca status: Fields and JSON](/reference/cli/status-fields/),
"Root" and "The rollup".

```bash
barca status --rollup                    # the whole project
barca status total pipeline.py --rollup  # `total` and its upstream cone
barca status --rollup --json             # the document below (the default when piped)
barca status --rollup --server           # the configured server's: GET /state/rollup
```

The JSON document is `{as_of, entries, roots, data_coverage}`; [barca status: Fields and JSON](/reference/cli/status-fields/),
"The rollup", has every key. It is the document the server's `GET /state/rollup` answers and
the web UI's health map, bars, to-do list and failures list draw, so all of them say the same
numbers. Each entry carries the roots that hold its nodes (`held_by`), so a group's to-do list
is in the document too. `barca status --json` carries its root entry as `rollup`, beside `summary`.
`--rollup` cannot be combined with `--limit`, `--all`, `--fields`, `--sample`, `--state` or
`--problems`: it always counts every node in scope. It is experimental (`barca docs
contract`): causes, the freshness gap and roots other than failures are added to the same
document by later releases. Without a server no run is tracked, so `running` is 0.

## Schedules

A node with `freshness=Schedule(...)` has `schedule` in JSON (absent on other nodes), and the
table adds a SCHEDULE column when a node it shows is scheduled:

| key | meaning |
|---|---|
| `cron` | the cron expression |
| `next_fire` | when it next matches (RFC 3339, UTC) |
| `last_fired` | its newest tick: the slot of the newest run the schedule started or queued (RFC 3339, UTC), or `null` |
| `last_run_id`, `last_status` | the run that tick started and its status as `barca history` words it (`pending` while it waits in the server's run queue), or `null` |
| `late`, `late_since` | `late_since` is the first slot after `last_fired` that has passed (RFC 3339, UTC, or `null`); `late` is true when that is further back than the schedule's grace plus its queue wait ([Scheduling With Cron](/scheduling/), "A schedule that missed its slot") |
| `failures`, `max_failures`, `failing` | consecutive failed ticks, `Schedule(..., max_failures=N)` (default 5), and `failures >= max_failures`: ticks are skipped ([Scheduling With Cron](/scheduling/)) |
| `paused`, `paused_until` | held back by `barca pause --schedule NODE` (`POST /schedule/{id}/pause`); `paused_until` is `null` for a pause until resumed |
| `skip_reason` | why the last due tick was skipped: `in_flight` or `paused`, or `null` |
| `live` | `true` when a running scheduler reported this entry |

The SCHEDULE cell is `<cron>; failing 2/2`, `<cron>; paused` (or `paused until <time> UTC`), or
`<cron>; next <time> UTC`, with `1 failure, ` before `next` while failures are counting and
`late since <time> UTC, ` before `next` while the schedule is late.

- **From a server** (configured, or `--server`): the server's scheduler's view, the same numbers
  `GET /schedule`, `GET /state` and the web UI's Schedules page show, with `next_fire` in the server's
  `--timezone`, and `live: true`. A server started with `--no-schedule` or `--read-only` fires
  nothing: its entries come from its metadata DB as below, with `live: false`.
- **On this machine** (`--local`, or no server): no scheduler runs, so the entry is what the
  metadata DB holds: the failures, pauses, runs and run queue a `barca serve` of this `.barca/`
  recorded, so `last_fired`, `last_run_id`, `last_status`, `late` and `late_since` are the ones
  that server reports. `next_fire` is the next match in this machine's local time, as `barca
  list` computes it, and slots are read in local time. `skip_reason` is `null` and `live` is
  `false`. A laptop that never ran `barca serve` shows `failures: 0`, `paused: false` and
  `late: false`.

## Finding and reading a failed run

`barca history` filters its runs; the filters combine with AND, and `--limit`, `total` and
`truncated` count the runs they keep:

- `--status success,failed,...`: any of `success`, `failed`, `cancelled`, `error`, `running`,
  `interrupted`. `not_successful` is every one of a run that ended without succeeding
  (`failed`, `error`, `cancelled`, `interrupted`).
- `--target NAME`: runs whose target (as typed) or job (resolved node ids) is that node, by
  function name or node id. A run of every node (`all`) is left out.
- `--triggered-by manual,api,schedule,client`.
- `--not-triggered-by schedule` (experimental): without the runs started that way. Unlike
  `--triggered-by manual,api,client`, it keeps the runs recorded before barca stored what
  started a run.
- `--search TEXT` (experimental): runs whose run id, target or job contains the text, whatever
  its case. Unlike `--target` it matches part of a name, and a run of every node whose id
  holds the text.
- `--since WHEN`: started at or after a duration back from now (`30m`, `24h`, `7d`, `2w`), an
  RFC 3339 time or a UTC date (`2026-10-22`).
- `--until WHEN` (experimental): started before a time, written as `--since` is. With
  `--since`, a window: `--since 2026-10-22T08:00:00Z --until 2026-10-22T09:00:00Z` is the runs
  started in that hour.

The database keeps the runs, each filter from an index of its own, so the cost is the runs
kept and not the runs recorded: on a history of 500,000 runs, a status, a trigger or a window
answers in a few milliseconds. `--search` reads every run's id, target and job from one index
(about a tenth of a second there), and `--target` reads the runs whose target or job holds the
node's name and checks each.

A failed run's row ends with its first failed step and the first line of its error, and JSON
carries it as `failure: {node, error}` and again as the flat `failed_node` and `error` (`null`
for every other run, and for a run that failed before any step did, such as a file that does
not parse), so `--fields run_id,failed_node,error` needs no nesting.

Each run of the list also has `line` (experimental), the run in the words of one row, the
same the web UI's run list prints: `steps` (`7 ok · 1 failed · 0 cached`, or `3 started · 0
cached` for a run that did not finish; steps are keys, a partitioned node counts one per key),
`steps_detail` (what `steps` counts, the nodes planned and the step the run stopped at),
`failure` (`stopped at orders · KeyError: 'region'`, or for a partitioned node
`demand_forecast: 7 keys failed · BatchKeyFailed: ...`; `null` with nothing wrong to say) and
`failure_detail` (the same in full).

```bash
barca history --status failed --since 24h
barca history --status failed --target clean --json --fields run_id,started_at,failure
barca history --status failed --json --fields run_id,failed_node,error
barca history --triggered-by schedule --status failed,interrupted
barca history --status not_successful --not-triggered-by schedule
barca history --search daily_report --since 7d
```

### Runs in progress and the last day's activity

When `--status` names `running`, each listed run that is in progress also says what it is
working on now (experimental): a `now: executing 3: orders, clean, report · 12 queued ·
240/900 keys` line under its row, and in JSON `now`, the object the web UI's run panel shows:
`running` (the nodes executing, the one furthest from done first, each with `keys` such as
`3,120 of 5,000 keys` and its `share` done), `queued` (how many nodes wait), `keys` (`{done,
total}` over the partitioned nodes in flight, or `null`), `groups` (the same counted by group)
and `line` (the line above; it names the first five executing nodes).

```bash
barca history --status running
barca history --status running --json --fields run_id,now
```

Where `now` comes from, and what it cannot say:

- Against a server ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)), a run the server started is followed through
  its step events: a node is executing once its function printed a line or its upstream steps
  finished, and queued before that. A step served from the cache sends no event until the run
  ends, so a node whose result is cached and that the run has not touched counts as done: it
  is not listed as executing, and what depends on it does not wait for it. A run asked to
  refresh (`--refresh`, `--refresh-all`) computes cached nodes again, so there they count as
  any other. The cache state is read as the request is answered: a node that stops being
  cached while the run goes (an input of it changed) shows from the next poll.
- Without a server, and for a `barca get` or `barca run` in a shell beside a server, the run
  streams its steps to nobody. barca reads what it recorded: every node of its job that has
  no result since the run started counts as executing, and `queued` is always 0. A run of
  named targets counts those targets only, not the upstream nodes it also runs.
- A run has no `now` when its owner cannot be seen alive from this machine (another host's
  run), when it was started by a barca older than 0.26 (no recorded job), or when its files no
  longer parse.

`barca history --activity` prints, instead of the runs, how many started in each of the last
24 hours (experimental): one line for the window, then a row per hour that had runs, with the
runs that did not succeed, the ones still running, the count by trigger, the median time of
the hour's finished runs and what most of its unsuccessful runs say went wrong.

```bash
barca history --activity
barca history --activity --json
barca history --activity --not-triggered-by schedule
barca history --activity --status not_successful --search daily_report
```

JSON is `{"activity": {...}}`: `since` and `until` (the window, RFC 3339 UTC: 24 whole hours
ending with the current one), `hours` (24 entries, oldest first, the empty hours included),
`total` (the window as a whole) and `summary` (the line the table starts with). An hour has
`at` (its start) and `until` (its end: `--since AT --until UNTIL` lists its runs), `runs`,
`ok`, `not_successful` (failed, error, cancelled or interrupted), `in_progress`, `by_status`
(`success`, `failed`, `error`, `cancelled`, `interrupted`, `running`), `by_trigger`
(`schedule`, `api`, `manual`, `client`, and `unknown` for runs recorded before barca stored
what started them), `median_seconds` (the median time of its finished runs; `null` when none
finished) and `failure` (`{text, runs}`: what most of its unsuccessful runs say went wrong,
`orders: KeyError: 'region'` for a failed step with a partitioned node's keys under its name,
or how they ended when no step failed, such as `cancelled`; the latest on a tie; `null` when
none ended that way).

- The counts are over every recorded run of the window, read from an index: they do not
  depend on `--limit`, and their cost is the runs of the window, not the length of the
  history. The times read the window's finished runs and the failures its unsuccessful ones.
- Hours are hours of UTC, also on a server started with `--timezone`: a reader converts `at`.
  In a zone at a half-hour offset an hour's bar starts at `:30` local time.
- `--activity` takes `--status`, `--triggered-by`, `--not-triggered-by` and `--search`, and
  counts the runs they keep; the answer says them back (`statuses`, `triggered_by`,
  `not_triggered_by`, `search`). `--target`, `--since`, `--until`, `--limit` and `--fields` are
  usage errors with it: the window is always the last 24 hours.
- With `--triggered-by`, runs that recorded no trigger are left out. With `--not-triggered-by`
  they are counted, and `excluded` is the window's runs the flag left out (an hour's keys over
  the whole window; `null` without the flag), so what is hidden is said: the table prints
  `left out: 1,438 schedule runs, 3 not successful`.
- A run counts in the hour it started, with the status it has now.

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
`rows` (rows of a tabular result, `null` when not counted: [barca status: Fields and JSON: Row counts](/reference/cli/status-fields/#row-counts)), `run_hash`, `artifact_path`, `artifact_format`, `artifact_size_bytes`, `error` (as recorded),
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

Each node's fields (health, what the last tick found, data signals and coverage, last materialization, artifact shape,
run timing, what started a run), the rollup's keys, the JSON document, output limits and what status asks an artifact store are in [barca status: Fields and JSON](/reference/cli/status-fields/).

See also: [The Cache](/reference/cache/), [barca logs](/reference/cli/logs/), [barca sql](/reference/cli/sql/), [Scripts and AI Agents](/reference/agents/).

---

*The same text is in your terminal: `barca docs status`.*
