---
title: "barca status: Fields and JSON"
description: "What barca status reports for each node beyond its state (last materialization, artifact shape, run timing, trigger), its JSON document, limits and disk usage."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/status-fields.md
---

<!-- Generated from crates/barca-cli/docs/status-fields.md (barca docs status-fields). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

What `barca status` reports for each node beyond its cache state: the last materialization, the
last changed bytes, the artifact's shape, run timing and attempts, and what started a run; the
JSON document; and the fine print on targets, limits, an artifact store and disk usage. Reading
status and finding problems is [barca status](/reference/cli/status/).

## Last materialization

`last_materialization` is the most recent execution recorded in the metadata DB, successful or
failed: `status`, `created_at` (UTC), `elapsed_seconds`, `run_hash`, `artifact`, `format`,
`size_bytes`, `rows`, and `error` for a failure. A cache hit is not an execution, so serving from
cache does not change it. It is `null` when the node never ran.

`rows` is how many rows a successful tabular result holds, counted by the worker when the step
wrote it ("Row counts" below); `null` when it was not counted.

## Row counts

The worker records the rows of a result when that is cheap to know exactly, and never guesses:

| result | `rows` |
|---|---|
| a pandas or polars DataFrame, a pyarrow Table, in any format (parquet, CSV, pickle) | its length |
| a polars LazyFrame or a DuckDB relation written to parquet, and each key of a batched call | the row count in the written file's footer (pyarrow, else duckdb, else polars reads it; only the footer is read) |
| a LazyFrame or relation written to CSV, anything that is not a frame (json, pickle of other values), a failed attempt | `null` |

Results recorded by an older barca, and results a server's ledger received from a client, have
`null`. The count is in `last_materialization.rows` here, in each step of `barca history RUN_ID
--json` (`steps[].rows`), in `barca stats --json` (`recent_runs[].rows`; for a partitioned
node the rows of the run's successful keys added up, `null` unless every one was counted, and
`recent_runs[].empty_keys`), in `barca sql` (`materializations.rows`, `nodes.last_rows`,
`nodes.partitions_empty`), and in the server's routes ([Server Mode: a Shared Ledger and --server: Data health](/reference/server-mode/#data-health)).
What the counts say about a node's data, and how many assets have one at all, is in "Data
signals and coverage" below.
A partitioned node's `partitions.empty` counts its keys whose latest successful result wrote no
rows (absent when none did).
For a partitioned node it is one key's row from the latest run, the first key in sort order
(`partition` names it), so two identical runs report the same key whatever order the keys
finished in.

## Health

Every node has a `health`: one word that says whether something needs doing about it, beside
its cache state and never in place of it. `barca status --json`, the server's `GET /nodes` and
`GET /state` and `barca.nodes` in `barca sql` carry the same word, computed once in barca, and
the web UI shows it. The words, most severe first:

| `health` | Printed as | Meaning |
|---|---|---|
| `failing` | failing | its own last attempt failed |
| `blocked` | blocked | something upstream keeps it from being brought up to date |
| `suspect` | suspect data | its last run succeeded but its recorded row counts look wrong |
| `behind` | behind | out of date against its inputs, partly computed, out of sync, or late |
| `undecided` | undecided | barca cannot decide yet (a cutoff or sensor upstream has not reported) |
| `never_run` | never run | nothing recorded |
| `ok` | ok | fresh, or a task or sensor whose last run succeeded |

`health_reason` names the rule that decided it. The first that applies wins:

| `health_reason` | `health` | When |
|---|---|---|
| `last_attempt_failed` | `failing` | `last_materialization.status` is `failed` |
| `keys_failed` | `failing` | `partitions.failed` is above 0 and the last materialization did not fail |
| `upstream_failing` | `blocked` | a node upstream is failing: an input, or an input of an input that is itself neither failing, `cached` nor `always_runs`. A pinned asset ([barca pin and unpin](/reference/cli/pin/)) is never blocked and a failure does not pass it |
| `cache_blocked` | `blocked` | `cache.state` is `blocked`, or `cache.reason` is `upstream_failed` |
| `empty` | `suspect` | it has an `empty` signal |
| `rows_changed` | `suspect` | it has a `rows_changed` signal |
| `behind_input` | `behind` | it has a `behind_input` signal |
| `schedule_late` | `behind` | its schedule missed a slot more than a minute ago (half the interval for a schedule more frequent than every two minutes) and is not paused |
| `stale`, `partial`, `unsynced` | `behind` | `cache.state` is that state |
| `checked_up_to_date` | `ok` | `cache.state` is `unknown`, and the newest tick that checked the node found it up to date or rebuilt it, with nothing seen to have changed since ("Last check" below) |
| `unknown` | `undecided` | `cache.state` is `unknown` |
| `never_run` | `never_run` | `cache.state` is `never_run` |
| `no_successful_run` | `never_run` | a task, sensor or `always` asset with no recorded run |
| `last_run_succeeded` | `ok` | a task, sensor or `always` asset whose last run did not fail |
| `cached` | `ok` | `cache.state` is `cached` |

A partitioned node has one health. A run working on a node does not change it. A node below
a failure is `blocked` even when its own state is `unknown` (it waits on a cutoff): the walk
passes through such nodes. `barca status` scoped to targets reads the targets' upstream cones,
so a failure above them is seen.

The table prints the words of the nodes it lists under the cache counts, most severe first:
`Health: 1 failing, 1 blocked, 2 ok`. With `--state` or `--problems` the line reads `Health
(nodes listed)`.

```bash
barca status --json | jq -r '.nodes[] | select(.health != "ok") | [.health, .health_reason, .id] | @tsv'
barca sql "select health, count(*) from barca.nodes group by all order by 2 desc"
```

Health says nothing of whether a `behind` or `never_run` node is a problem: its `cause` does
("Cause" below), and its `root` names what explains it ("Root").

Health is experimental ([CLI Contract](/reference/cli-contract/)).

## Cause

Every node that is not fresh has a `cause`: one word for why. A node is fresh when its
`health` is `ok` or `suspect` (suspect is about data, not freshness), and then `cause` is
`null`. The first row that applies decides:

| # | When | `cause` | Printed as |
|---|---|---|---|
| 1 | it is `failing`; or `blocked` by a failing node upstream; or its refresh path's schedule is held after `max_failures` failed ticks | `failing` | failing |
| 2 | its `health` is `undecided` | `undecided` | undecided |
| 3 | a running run computes it | `catching_up` | catching up |
| 4 | finished ticks had it as a target and did not leave it fresh (`refresh_path.missed_ticks` is 1 or more) | `overdue` | overdue |
| 5 | a run that computes it waits for a run slot; or a tick of its refresh path will come and that schedule's ticks wait: its newest tick is waiting now, or its typical wait exceeds its interval (`schedule.queue_bound`) | `queued` | queued |
| 6 | no tick will reach it: it has no refresh path, or its path's schedule is paused until resumed or fired by no scheduler | `no_refresh_path` | no refresh path |
| 7 | otherwise: the next tick of its refresh path will compute it | `catching_up` | catching up |

`catching_up` and `queued` are progress. `overdue`, `no_refresh_path` and `failing` are what
to look at. A cause is never a guess: where barca cannot tell, the word is `undecided` or
`no_refresh_path`.

**Whether a scheduler is firing.** Row 6 asks whether a tick will come. `refresh_path.live`
says whether the answering process fires the schedule, which a `--read-only` server and
`barca status` beside a server never do. For the cause, the tick record also counts
(`schedule_ticks`, "Last check"): when a schedule that is not paused fired its newest slot (it
has a tick and is not late), a scheduler is running somewhere, and every schedule that is not
paused, held as failing or late counts as fired. So a read-only server says `catching_up`
where the scheduling server would, with `refresh_path.live` still `false` and no
`expected_fresh_at` (it cannot estimate another process's queue). With no tick on record, or
only old ones, the cause is `no_refresh_path`.

The table of `barca status` sums the causes of every node in scope under the health words:
`Gap: 14 overdue · 152 no refresh path · 180 catching up (nodes); about 180 fresh within 5m,
166 with no estimate` (or `Gap: every node is fresh`). `barca.nodes` has `cause`.

```bash
barca status --json | jq -r '.nodes[] | select(.cause) | [.cause, .health, .id] | @tsv'
barca sql "select cause, count(*) from barca.nodes where cause is not null group by all order by 2 desc"
```

## Root

Every node with a cause has a `root`: the upstream fact that explains it, as `{"id":
"code:pipeline.py:orders", "kind": "code", "node": "pipeline.py:orders"}`. The id is
`<kind>:<node id>`; nodes with the same `root.id` are held by the same thing, so a few roots
usually explain hundreds of nodes (`barca status --rollup` lists them, "The rollup" below).

| `kind` | Printed as | `node` | Explains |
|---|---|---|---|
| `sensor_value` | sensor value | the sensor whose observation is newer than what a node below it read | catching up, overdue, undecided, queued |
| `code` | code change | the node whose code changed (its function, a decorator, a helper) | same |
| `env` | environment change | the node whose declared environment variable changed | same |
| `input` | input change | the fresh node whose result is newer than what a node below it read; or a node out of date with nothing above it or on it that says why | same |
| `never_run` | never run | the topmost never-run node of the chain (a partitioned node with keys that never ran) | same |
| `undecided` | undecided | the topmost node barca cannot decide, with nothing above it that explains it | undecided |
| `no_schedule` | no schedule | the topmost not-fresh node no tick reaches: where a schedule would have to be added, or a `Manual` or pinned node | no refresh path |
| `schedule_paused` | paused | the scheduled node of a paused schedule; or a paused node (`barca pause`) | no refresh path |
| `scheduler_off` | scheduler off | the scheduled node of a schedule no scheduler fires | no refresh path |
| `failure` | failure | the failing node: the node itself, or the nearest one upstream that blocks it; or the scheduled node of a schedule held after `max_failures` failed ticks | failing |
| `queue` | run queue | the scheduled node whose ticks wait for a run slot | queued |

How a root is found:

- **Failing:** itself, or the failing node that blocks it (the nearest upstream; with two
  failing inputs, the first by node id).
- **Queued** by a schedule's wait: that schedule (`queue`). A node queued only because a run
  someone asked for is waiting keeps the change as its root.
- **No refresh path:** its path's schedule when it has one (`schedule_paused` or
  `scheduler_off`); a paused node is its own root (`schedule_paused`); otherwise the topmost
  node of the chain of not-fresh nodes that no tick reaches (`no_schedule`). A stale `Manual`
  node and a pinned asset ([barca pin and unpin](/reference/cli/pin/)) are such a top: what is below them and has a
  path is brought up to date against what they hold.
- **Undecided** where no tick will come: as for no refresh path. Only a run decides the node,
  so what holds it is the missing tick, not the change above it.
- **Everything else** (catching up, overdue, undecided with a tick coming): barca walks up
  from the node through its inputs that are not fresh and stops at the first node whose own
  evidence explains it. That is `cache.changed`, what changed in the node's run hash: `code`,
  a decorator or a helper is `code`; `env NAME` is `env`; `sensor ID` or `input ID` names an
  input, which is the root when it is fresh (`sensor_value` for a sensor, `input` for an
  asset) and passes the question up when it is not. A node out of date only because of what
  is above it takes the root of its first not-fresh input in dependency order. A chain with
  no such evidence ends at its topmost node: `never_run`, `undecided`, or `input`.

When a node changed in several ways, its own definition wins: `code`, then `env`, then what
it reads. `barca.nodes` has `root_id`, `root_kind` and `root_node`.

```bash
barca status --json | jq -r '.nodes[] | select(.root) | [.root.kind, .root.node, .id] | @tsv'
barca sql "select root_kind, root_node, count(*) as held, sum(gap_seconds) as gap from barca.nodes where root_id is not null group by all order by held desc"
```

`gap_seconds` is the node's freshness gap: how long it has been out of date (`stale_since` to
now) plus how long until it is expected fresh (now to `expected_fresh_at`), in whole seconds
("Times"). A time that is `null` counts 0, and a fresh or `undecided` node has 0. It is read
at the start of the current minute, so it does not change with every second of the clock. A
project's gap is the sum over its nodes, in node-seconds; a node with no `expected_fresh_at`
has no forward term, which is why every sum also says how many such nodes it holds
(`unbounded`).

Cause, root and the gap are experimental with the rest of health.

## The rollup

`barca status --rollup --json` (and the server's `GET /state/rollup`) sums the nodes in scope
into one tree. Nothing else adds health up: the table of `barca status --rollup`, the web
UI's health map, bars and failures list all print this document.

```json
{
  "as_of": "2026-11-17T16:49:57Z",
  "entries": [
    {"id": "root", "kind": "root", "label": "all", "parent": null,
     "children": ["group:pipeline.py:ingest", "file:pipeline.py"],
     "nodes": 6, "assets": 4, "running": 0,
     "health": {"failing": 1, "blocked": 2, "suspect": 0, "behind": 0, "undecided": 0, "never_run": 0, "ok": 3},
     "asset_health": {"failing": 1, "blocked": 2, "suspect": 0, "behind": 0, "undecided": 0, "never_run": 0, "ok": 1},
     "data_coverage": {"assets": 4, "measured": 0, "suspect": 0},
     "causes": {"catching_up": 0, "queued": 0, "no_refresh_path": 0, "overdue": 0, "failing": 3, "undecided": 0},
     "asset_causes": {"catching_up": 0, "queued": 0, "no_refresh_path": 0, "overdue": 0, "failing": 3, "undecided": 0},
     "not_fresh": 3,
     "held_by": [
       {"root": "failure:pipeline.py:orders", "nodes": 3, "assets": 3,
        "causes": {"catching_up": 0, "queued": 0, "no_refresh_path": 0, "overdue": 0, "failing": 3, "undecided": 0},
        "gap_seconds": 5400, "unbounded": 3, "cumulative_nodes": 3}
     ],
     "gap_seconds": 5400, "unbounded": 3, "fresh_by": null, "also_in": []}
  ],
  "roots": [
    {"id": "failure:pipeline.py:orders", "kind": "failure", "node": "pipeline.py:orders",
     "detail": "ValueError: 12 rows rejected", "error_group": "ValueError: # rows rejected",
     "nodes": 3, "assets": 3,
     "causes": {"catching_up": 0, "queued": 0, "no_refresh_path": 0, "overdue": 0, "failing": 3, "undecided": 0},
     "gap_seconds": 5400, "unbounded": 3,
     "action": {"kind": "fix_failure", "command": "barca get orders", "estimated_seconds": 42}}
  ],
  "data_coverage": {"assets": 4, "measured": 0, "suspect": 0},
  "files": [
    {"file": "pipeline.py", "nodes": 6, "assets": 4, "running": 0,
     "health": {"failing": 1, "blocked": 2, "suspect": 0, "behind": 0, "undecided": 0, "never_run": 0, "ok": 3},
     "asset_health": {"failing": 1, "blocked": 2, "suspect": 0, "behind": 0, "undecided": 0, "never_run": 0, "ok": 1},
     "worst": "failing"}
  ]
}
```

`entries` lists the root first, then every entry depth first, a level's children in the order
of `children`.

| Key of an entry | Meaning |
|---|---|
| `id`, `kind` | `root` (id `root`), `group` (the group's id, `group:<file>:<name>`), `file` (`file:<path>`: the nodes of that file that are in no group) or `node` (the node id) |
| `label` | the group's or node's name, the file's path, `all` for the root |
| `parent`, `children` | the entry it sits under (`null` for the root) and the entries under it: worst health first, then most assets, then label |
| `nodes`, `assets` | nodes underneath (1 for a node), and how many of them are assets: tasks and sensors are nodes, not assets |
| `running` | nodes underneath a run is working on now; 0 without a server |
| `health`, `asset_health` | how many nodes, and how many assets, are in each health |
| `data_coverage` | the coverage of "Data signals and coverage", for the assets underneath |
| `causes`, `asset_causes` | how many nodes, and how many assets, that are not fresh have each cause ("Cause"); they add up to the nodes (assets) whose health is not `ok` or `suspect` |
| `not_fresh` | nodes underneath that are not fresh: what `causes` counts, as one number |
| `held_by` | the roots that hold those nodes, each with what it holds underneath this entry: `root` (an id of `roots`), `nodes`, `assets`, `causes`, `gap_seconds`, `unbounded`, and `cumulative_nodes` (the nodes held by this root and the ones listed before it). In the order of `roots`, applied to what is held here; their `nodes` add up to `not_fresh`. Empty on a node entry, whose root is on its row |
| `gap_seconds` | the freshness gap underneath, node-seconds: the sum of the nodes' `gap_seconds` ("Root") |
| `unbounded` | nodes underneath that are not fresh and have no `expected_fresh_at` ("Times"): nothing known will leave them fresh, so the gap has no forward term for them |
| `fresh_by` | the latest `expected_fresh_at` underneath (RFC 3339, UTC): when the nodes that have an estimate are all expected fresh. `null` when none has one. It speaks only for those nodes; `unbounded` counts the rest |
| `keys` | on a partitioned node only: its keys that `failed`, are `behind` (no cached result, and did not fail) and are `ok` |
| `also_in` | the other groups that declare this node or group, by id; empty for one declared in at most one group |

Every node has exactly one parent, so every count of an entry is the sum of its children's. A
node declared in several groups is counted under the first of them by group id and names the
others in `also_in`; a group with no node in scope is not listed.

`files` cuts the same nodes by source file: one object per file in scope, by path, with its
`nodes`, `assets`, `running`, `health` and `asset_health` summed over every node of the file,
in whichever group each sits, and `worst`, the most severe health among them. The tree has a
`file` entry only for the nodes of a file that are in no group, so a list of files (the web
UI's sidebar) reads `files`.

`roots` is the to-do list: what holds the nodes that are not fresh, and what clears each.
There is one entry per `root` the nodes carry ("Root"), and every node that is not fresh is
under exactly one, so the roots' `nodes` add up to the nodes that are not fresh.
A root's numbers are over every node in scope; what it holds under one group or file is that
entry's `held_by`, which is the same list cut to the entry, so a group's to-do list is its own
entry's. `cumulative_nodes` against `not_fresh` says how much the head of a list explains:
`barca status --rollup` prints "The first 3 hold 392 of them." under the list's heading when
there are more than three roots.

| Key of a root | Meaning |
|---|---|
| `id`, `kind`, `node` | the `root` its nodes carry |
| `detail` | the fact in a line: a failure's error (its first line, or the last line of a bare traceback; `3 of 8 keys failed` for a node whose keys failed with no error of its own), `the code of orders changed`, `no tick reaches ledger`, `inbox's schedule is paused`, `pulse's ticks wait about 2m for a run slot, every 1m: max_runs is 1` |
| `error_group` | a failure's error line with its numbers replaced by `#`, so failures that differ only in their numbers share one; `null` for every other kind and when no error was recorded |
| `nodes`, `assets` | how many nodes it holds, and how many of them are assets |
| `causes` | those nodes by cause |
| `gap_seconds` | the sum of their `gap_seconds` |
| `unbounded` | how many of them have no `expected_fresh_at` |
| `action` | what clears it: `{"kind", "command", "estimated_seconds"}` |

| `action.kind` | Printed as | For | `command` |
|---|---|---|---|
| `wait` | wait | a change the next tick or a run in flight computes; an undecided chain; a tick waiting for a slot on a schedule that is not queue-bound | `null` |
| `run` | run | a change whose nodes ticks already missed (`overdue`): `barca get` of the held nodes nothing else held reads (`barca run` when they are all tasks), which computes everything above them. A `Manual` root: the same with `--refresh <node>`, since a run reads a `Manual` node as it is unless it is named there. A pinned root: `barca unpin <node>` | the command |
| `add_schedule` | add a schedule | a chain no tick reaches: give its top, or a node above it, `freshness=Schedule(...)` | `null` |
| `resume_schedule` | resume | a paused node: `barca resume <node> --server`. A paused schedule: `barca resume --schedule <node> --server` ([Scheduling With Cron](/scheduling/), "Pausing one schedule") | the command |
| `start_server` | start a server | a schedule no scheduler fires | `barca serve` |
| `fix_failure` | fix the failure | a failing node: the command tries it again once the error is fixed (`barca get <node>`, `barca run <node>` for a task) | the command |
| `raise_max_runs` | raise max_runs | a queue-bound schedule: give the server more run slots (`barca serve --max-runs N`, `[server] max_runs`) or space the schedules ([Scheduling With Cron](/scheduling/), "When ticks wait for a run slot") | `null` |

A command names a node by its name, or by its id when two nodes in scope share the name, and
ends in `--server` when a server answered (`GET /state/rollup`, `barca status --rollup
--server`): the run belongs in that server's queue. `estimated_seconds` is the typical compute
time of the nodes the root holds: the sum of their typical step durations (the median of a
node's recent successful runs, once per key without a result for a partitioned node; the
project's median for a node with none), in whole seconds, and `null` when none of them has a
sample. It is work, not wall-clock time: steps of one run share the worker pool.

Roots that hold nodes with no estimate come first, the one holding the most nodes first: their
gap grows until someone acts. Then the rest by `gap_seconds`, then `nodes`, then `id`.
`data_coverage` at the top is the root entry's.

`barca status --rollup` prints the list under the tree:

```text
Gap: 152 no refresh path · 6 queued · 247 undecided · 17 catching up (nodes); about 23 fresh within 4m, 399 with no estimate

What holds the 422 nodes that are not fresh, and what clears it:
1. sensor value: inbox
     holds 180 nodes (180 assets): 170 undecided · 10 catching up; 3h of gap; 170 with no estimate
     why:  inbox observed a new value
     do:   wait (about 9m of compute)
2. no schedule: ledger
     holds 152 nodes (152 assets): 152 no refresh path; 152 with no estimate
     why:  no tick reaches ledger
     do:   add a schedule: freshness=Schedule(...) on it or on a node above it (about 7m of compute)
```

```bash
barca status --rollup --json | jq -r '.entries[] | select(.kind == "group") | [.label, .nodes, .assets, .health.failing] | @tsv'
barca status --rollup --json | jq -r '.roots[] | [.nodes, .kind, .node, .action.kind, .action.command // "-"] | @tsv'
barca status --rollup --json | jq '.entries[0] | {causes, gap_seconds, unbounded, fresh_by}'
barca status --rollup --json | jq -r '.entries[] | select(.kind == "group") | .label as $g | .held_by[] | [$g, .nodes, .root] | @tsv'
```

The rollup is experimental ([CLI Contract](/reference/cli-contract/)). A scope given as targets is their upstream
cones, so a rollup of one group also counts what that group reads from outside it. Known
limits: an
`input` root is named from what the node's run hash recorded, so a result recorded before
0.29.0 (no components) roots at the node itself; and the command of a `run` action lists
every held node nothing else held reads, which for a wide fan-out is a long line.

## Last check

`last_check` is what the newest finished tick that had the node as a target did with it
([Scheduling With Cron](/scheduling/)): `{"at": "2026-11-17T15:00:05Z", "outcome": "up_to_date", "schedule":
"pipeline.py:inbox"}`, or `null` when no retained tick did. `at` is when that tick's run ended
(RFC 3339, UTC), `schedule` the scheduled node whose tick it was, and `outcome` one of:

| `outcome` | The tick's run |
|---|---|
| `rebuilt` | executed its step, which succeeded. For a sensor that means it was polled, whatever it returned; for a task, that it fired |
| `up_to_date` | found a cached result for its code and inputs: a cache hit |
| `failed` | executed its step, which failed |
| `not_reached` | ended before it decided the node: a failure upstream, a cancellation, a run that never started |

It is a record, shown whatever its age. Ticks are recorded by a scheduling `barca serve`
(`schedule_ticks` in the metadata database); `barca status` on that machine and a
`--read-only` server read the same record. Each schedule keeps the outcomes of its newest 20
fired ticks, and with several schedules reaching a node the newest tick wins.

**When a check decides health.** `barca status` and `GET /state` predict what a `barca get`
would do, without running anything. Where the answer depends on what only a run produces, the
node's state is `unknown` ([barca status: Cache state](/reference/cli/status/#cache-state)): below an `@asset(cutoff=True)`
that would run, the bytes it will write. A tick does run. It also reads a node paused with
`barca pause` as it is, where a `get` (and so the prediction) recomputes it, so below a
paused node it can find a node up to date every five minutes while its state stays `unknown`.
(A `Manual` or pinned node is read as it is by every run, and by the prediction.) Such a
node is `ok` with `health_reason: "checked_up_to_date"` when all of this holds:

- its state is `unknown` and no earlier rule applies (it is not failing, blocked, suspect, or
  the scheduled node of a late schedule);
- its last check's `outcome` is `up_to_date` or `rebuilt`: a newer `failed` or `not_reached`
  check is the last check, and decides nothing;
- the check has not expired. It stands for one interval of its schedule after its run ended,
  plus the time that tick took from its slot to its end, plus the grace a late schedule gets
  (a minute; half the interval for a schedule more frequent than every two minutes): by then
  the next tick's check is due. A tick found up to date at 15:00:05 by a five-minute schedule
  stands until 15:06:10. The check of a schedule that is no longer in the project never stands;
- the node's definition (its code, counted decorator arguments and helpers) is the one its
  newest successful result recorded: an edit since is seen;
- no node above it has a materialization recorded after that tick's run ended, other than by
  a tick's run still in flight (whose own check is on its way).

Otherwise the node keeps the health its state gives (`undecided`). A node whose check stands
gets no `behind_input` signal: the tick read its inputs' newest results and found it up to
date. Not seen until the next tick or the expiry: an edit to a node above it (which shows on
that node), and a new value of an environment variable it declares.

The table adds the check to the `WHY` of a node whose state is `unknown`: `cutoff_pending;
checked 3m ago, up to date`. `barca.nodes` has it as `last_check_at` and `last_check_outcome`.

```bash
barca status --json | jq -r '.nodes[] | select(.last_check) | [.id, .last_check.outcome, .last_check.at] | @tsv'
barca sql "select name, health, last_check_outcome, last_check_at from barca.nodes where health_reason = 'checked_up_to_date'"
```

`last_check` and `checked_up_to_date` are experimental with the rest of health.

## Refresh path

`refresh_path` is the schedule whose tick brings the node up to date, or `null` when no tick
ever will:

```json
{"schedule": "pipeline.py:inbox", "next_tick": "2026-11-17T15:00:00Z",
 "paused": false, "live": true, "missed_ticks": 0}
```

A node has a path through a schedule exactly when that schedule's tick has the node as a
target ([Scheduling With Cron](/scheduling/), "What a tick does"): the scheduled node itself, the `Always`
nodes below it, and the `Always` nodes below every sensor without a schedule of its own that
the tick's run polls. It is the scheduler's own list, read back per node. So these have no
path:

- a `Manual` asset or task: a tick reads it as it is. The `Always` nodes below it still have
  theirs;
- an `Always` node with no scheduled node above it and no polled sensor above it;
- a sensor a tick only polls as an input (it is polled, not targeted);
- a paused node (`barca pause`), which a tick skips, and a pinned asset (`barca pin`), which a
  tick reads as its pinned result. What is below either keeps its own path.

| Field | Meaning |
|---|---|
| `schedule` | the scheduled node's id. With several schedules reaching the node, the one that fires earliest next; a schedule with no next tick comes last, and ties go to the lower id |
| `next_tick` | that schedule's next slot (RFC 3339, UTC); the first slot after a timed pause ends; `null` for a pause until resumed and when `live` is false |
| `paused` | the schedule is paused (`barca pause --schedule NODE`, `POST /schedule/{id}/pause`). A paused schedule is still the path |
| `live` | a running scheduler fires the schedule. `false` from `barca status` on a machine with no server, from a `--read-only` or `--no-schedule` server, and for a schedule held after `max_failures` failed ticks (`schedule.failing`) |
| `missed_ticks` | how many finished ticks of the path's schedules had the node as a target and did not leave it fresh |

`missed_ticks` reads the tick record (`last_check` above): it counts the ticks whose outcome
for the node was `failed` or `not_reached`, from the newest back to the last tick that found
the node up to date or rebuilt it, and only those that ended after the node's newest
successful result (a run nobody scheduled made the earlier ones good). It is `0` for a fresh
node (`health` `ok` or `suspect`). Each schedule keeps the targets of its newest 20 fired
ticks, so the count stops there; a tick that was skipped or never fired is not a miss, and a
database with no tick record counts none.

The table adds the path to the `WHY` of a node that is not fresh: `upstream_stale; next tick
15:05 UTC by inbox` (the day is named when it is not today), `inbox is paused`, `inbox is not
firing`, with `, missed 2 ticks` when ticks missed it; and `no refresh path` where nothing
will refresh it, when a node shown is scheduled or has a path (a project with no schedule
has none anywhere). A scheduled node's own schedule is in its `SCHEDULE` cell. `barca.nodes`
has `refresh_schedule`, `next_tick` and `missed_ticks`, all `null` for a node with no path.

```bash
barca status --json | jq -r '.nodes[] | select(.health != "ok" and .refresh_path == null) | .id'
barca sql "select name, health, refresh_schedule, next_tick, missed_ticks from barca.nodes where health <> 'ok' order by missed_ticks desc"
```

`refresh_path` is experimental with the rest of health.

## Times

Three times say how far a node that is not fresh is from fresh. Each is RFC 3339 UTC or
`null`, and none is a guess: a time barca cannot tell is `null`.

| Field | Meaning | `null` when |
|---|---|---|
| `stale_since` | since when the node has been out of date: the earliest recorded time from which it has been continuously not fresh | the node is fresh (`health` `ok` or `suspect`) or `undecided`; or no record holds the time |
| `expected_fresh_at` | when the node is expected to be fresh: an estimate, rounded up to 5 seconds | the node is fresh; or no run is known to leave it fresh (below) |
| `recheck_at` | when a tick next looks at the node: the `next_tick` of its refresh path | it has no refresh path, or the path has no next tick |

**"Fresh" and "rechecked" are different promises.** `expected_fresh_at` is given only where a
run will compute the node and leave it fresh. `recheck_at` only says a tick will look: the
node may be found up to date, be rebuilt, or be left as it is. A node with `recheck_at` and
no `expected_fresh_at` is rechecked then, not fresh then, and every surface words it so
(`fresh ~15:05 UTC` against `rechecked 15:00 UTC`).

`stale_since` is, for the node itself:

| The node is | `stale_since` |
|---|---|
| failing | its first failed attempt since its last clean run |
| stale because an input changed (`inputs_changed`: a sensor's new observation, an asset's new result) | when the earliest such input result newer than the node's own was written |
| behind an input (`behind_input`) | when that input's newer result was written |
| the scheduled node of a late schedule | the first slot no tick fired for (`schedule.late_since`) |
| stale because its code changed (`code_changed`), or never run | when its source file was last modified, if that is after the node's newest result |
| stale only because an environment variable changed (`env_changed`), `partial`, `unsynced` | `null`: nothing records when |

and for a node that is out of date because of what is above it (`upstream_stale`, blocked),
the earliest of its own time and the times of the nodes above it that are not fresh: it has
been out of date as long as they have. The file's modification time stands for a code change
because nothing records when barca first saw a definition. It is never before the edit, but
an unrelated later edit to the same file moves it forward, so the node has been out of date
for at least that long, possibly longer; a helper in another file that changed is not seen,
and the time is `null`.

`expected_fresh_at` is the start of the run that will compute the node plus the node's place
in that run:

- **The run** is the first that applies: a running run that computes the node, counted from
  its latest progress (its start, or the last step it finished); a run waiting in the run
  queue that computes it, from its `estimated_start_at` (`GET /queue`); else the next tick of
  the node's refresh path: the slot, plus the schedule's typical queue wait
  (`schedule.typical_queue_wait_seconds`), or when the run queue clears if that is later.
- **The node's place** is the typical duration of each step before it and its own: the median
  of a node's recent successful runs (the project's median for a node with none; once per
  key without a result for a partitioned node). A step starts when its inputs are done and a
  worker is free: steps are taken in the order they become ready, and each also waits for the
  work taken before it divided over the worker pool. With enough workers that is the longest
  chain above the node; with one it is the sum. A sensor's poll and a task that runs on every
  tick count as work.
- **No estimate** (`null`) for a node that is failing or blocked (the failure is what holds
  it), `undecided` or in cache state `unknown` (the run may find it up to date), below a node
  that is not fresh and that the run does not compute (a stale `Manual`, paused or pinned
  node: the tick reads it as it is), missed by a tick already (`refresh_path.missed_ticks`,
  unless a run is computing it now), without a refresh path, or while the run queue is paused.
  `barca status` on a machine with no server estimates nothing: no scheduler fires here.

What it does not model: a run's startup (workers importing the project), steps heavier than
one unit of capacity, other runs sharing the server's capacity, and retries. On a server with
one run slot, two schedules ticking every 10 seconds and steps of 0.4 to 3 seconds, the
estimates were 2 to 4 seconds later than the time each node became fresh, most of it the
rounding up; the test that measures it holds them to 8 seconds
(`python/tests/test_estimates.py`).

The table adds the estimate to the `WHY` of a node that is not fresh: `fresh ~15:05 UTC`
(to the minute, rounded up), or `overdue 16h` for a node ticks have missed (how long it has
been out of date). A node with only a recheck already names its next tick there. A rollup
entry carries `fresh_by`, the latest `expected_fresh_at` underneath, and `unbounded`, how
many nodes underneath are not fresh and have none. `barca.nodes` has `stale_since`,
`expected_fresh_at` and `recheck_at`.

```bash
barca status --server --json | jq -r '.nodes[] | select(.expected_fresh_at) | [.name, .stale_since, .expected_fresh_at] | @tsv'
barca sql "select name, health, stale_since, recheck_at from barca.nodes where health <> 'ok' and expected_fresh_at is null"
```

The times are experimental with the rest of health.

## Data signals and coverage

`signals` lists what a node's recorded results say about its data, from facts the metadata DB
holds (row counts workers recorded, when each result was written), never by reading an
artifact. Only assets have signals, in this order:

| `kind` | When | Fields |
|---|---|---|
| `empty` | its latest successful result wrote no rows: for a partitioned node, some keys' latest successful results | `empty_keys` of `keys` (the keys whose latest result recorded a count; 1 of 1 without keys), `previous_empty_keys`: how many were empty before its newest run, `null` when no key had a counted result before it |
| `rows_changed` | a latest successful result's row count differs from the one before it, same node and key, by more than half of it (any rows after an empty result count) | `rows` and `previous_rows` of `key` (`null` without keys), the key that moved the most relative to before; `changed_keys`: how many moved |
| `behind_input` | its cache state is `unknown` (nothing else says it is behind, typically a `collect()` fan-in below a pending cutoff), no tick's check of it stands ("Last check" above) and its latest successful result is older than the newest successful result of an asset it reads, from another run | `input` (the input with the newest result), `input_success_at`, `last_success_at` (UTC `YYYY-MM-DD HH:MM:SS`) |

A signal stays until the next successful result of the node (or key) replaces the one it is
about; there is no acknowledgement yet. A `stale`, `partial` or `blocked` node gets no
`behind_input`: its state already says it will run.

An empty `signals` does not mean the data is fine. A signal can only be raised for a result
whose rows were counted ("Row counts" above), so each asset says whether it was: `measured` is
`true` when its latest successful result has a recorded row count (for a partitioned asset,
any key's), `false` when it has none, and `null` for a task or sensor. An asset with
`measured: false` is not measured, never ok for data.

`data_coverage`, beside `summary`, counts this for every node in scope (a filter does not
change it): `assets`, how many of them are `measured`, and how many of those are `suspect`
(an `empty` or `rows_changed` signal). The table prints it as its last line but one, `Data: 7
of 556 assets measured`, with `, 2 suspect` when some are, so "no suspect data" is never said
without what it is out of.

```bash
barca status --json | jq '.data_coverage'
barca status --json | jq -r '.nodes[] | select(.signals != []) | [.id, (.signals | map(.kind) | join(","))] | @tsv'
barca sql "select count(*) filter (where measured) as measured, count(*) as assets from barca.nodes where kind = 'asset'"
```

## Last changed bytes

`last_bytes_change` is the most recent recompute of the node that produced different bytes for
the same code and inputs (the same run hash): `{node_id, run_hash, content_hash, created_at}`,
with the exact node id (a partition key included). It is absent when that never happened. The
table prints one line per such node under the rows:
`total: different bytes for the same code and inputs at 2026-10-02 11:56:43 UTC (pipeline.py:total)`.
It means the function is not deterministic ([The Cache](/reference/cache/)).

## Artifact shape

`shape` describes the artifact of `last_materialization` when that run succeeded (`null`
otherwise). It is read from the file alone by a small Python helper; your modules are never
imported.

| format | shape |
|---|---|
| parquet | `type: "table"`, `rows`, `columns: [{name, type}]` (arrow types) from the file footer |
| json list | `type: "list"`, `rows`; for a list of objects also `columns` with the JSON types seen (`str \| null`) |
| json object | `type: "dict"`, `keys` (first 100; `key_count` when there are more) |
| other json | `type` (`int`, `str`, ...) |
| pickle | `type` only, e.g. `myproject.Model`, read from the pickle opcodes without unpickling |

`--sample N` adds `sample`: the first N rows (parquet, json list), the first N entries (json
object), or the value (other json). It is off by default to keep output small. Pickles are never
sampled: loading one would import and run code.

When the shape cannot be read, `shape` has a `note` instead of `rows`/`columns`: parquet without
pyarrow installed (`pip install 'barca[parquet]'`), a file that no longer exists, or a remote
artifact that is too large or cannot be reached (below).

### Remote artifacts

With remote storage ([Remote Storage](/reference/remote-storage/)) the shape is read from the bucket, through the same
filesystem, credentials and `[remote.storage_options.*]` the steps use. There is no flag for it:

```bash
barca status total --json --sample 2    # rows, columns and 2 sample rows, read from the bucket
```

- parquet: `rows` and `columns` come from the file footer by ranged requests; the object is not
  downloaded. `--sample N` also reads the first row group, not the whole file. Measured on a
  160 MB file of 20 row groups in S3-compatible storage: 64 KB read for the shape, 8 to 9 MB with
  `--sample 5`.
- json and pickle have to be downloaded to be described, so only objects up to 16 MB are. A
  larger one has `"note": "remote json artifact too large to inspect: 25.1 MB (limit 16 MB)"`.
  `barca sql` can still query a large json result.

Each node with a result costs one or two requests, and up to 8 artifacts are read at a time.
`barca status` with no target reads every node; name a target to read only its upstream cone.

A store that cannot be read never fails the command. A missing driver
(`pip install 'barca[s3]'`), rejected credentials or a network error is the `note` of each
shape, the rest of the status is complete, and the exit code is 0. After the first such failure
the artifacts not yet read report it without another attempt, so an unreachable bucket costs one
wait for the driver's retries (about 10 seconds for an S3 endpoint that refuses connections),
not one per node.

## Partitioned assets

A partitioned asset is one node with a `partitions` summary: `total`, `cached`, `missing` and up
to 20 `missing_keys`, and `empty` when some keys' latest successful result wrote no rows (a
recorded count of 0; keys no longer in the key set count while their result is recorded). Its state is `cached` when every key is, `partial` when some are, and
`stale`/`never_run` when none are. `last_materialization` is one key of the latest run (the first in
sort order), named in its `partition` field (for example `k=a`), and `shape` describes that one key's artifact.

The keys themselves, each with its state, latest attempt, rows and error, are
`barca status NODE --keys`; every key's state at once is `--grid`, and one key along its chain
is `--key KEY` ([Partitions: Listing keys](/reference/partitions/#listing-keys-barca-status-node---keys---grid---key)).

## JSON

```json
{
  "target": "total",
  "targets": ["total"],
  "nodes": [
    {
      "id": "pipeline.py:orders",
      "name": "orders",
      "kind": "asset",
      "inputs": [],
      "partitioned": false,
      "cache": {"state": "cached", "reason": "materialized", "detail": "...", "run_hash": "6db9...", "artifact": ".barca/artifacts/pipeline.py--orders/6db9....parquet"},
      "health": "ok",
      "health_reason": "cached",
      "signals": [],
      "measured": true,
      "cause": null,
      "root": null,
      "gap_seconds": 0,
      "last_check": null,
      "refresh_path": null,
      "stale_since": null,
      "expected_fresh_at": null,
      "recheck_at": null,
      "last_materialization": {"status": "success", "created_at": "2026-10-02 11:56:43", "elapsed_seconds": 0.54, "run_hash": "6db9...", "artifact": ".barca/artifacts/pipeline.py--orders/6db9....parquet", "format": "parquet", "size_bytes": 2182, "rows": 3},
      "shape": {"type": "table", "rows": 3, "columns": [{"name": "id", "type": "int64"}, {"name": "amount", "type": "double"}]}
    }
  ],
  "summary": {"cached": 2, "unsynced": 0, "stale": 0, "never_run": 0, "partial": 0, "blocked": 0, "unknown": 0, "always_runs": 0},
  "data_coverage": {"assets": 2, "measured": 1, "suspect": 0},
  "as_of": "2026-10-02T11:57:10Z",
  "rollup": {"id": "root", "kind": "root", "label": "all", "parent": null, "children": [], "nodes": 2, "assets": 2, "running": 0, "health": {"ok": 2, "...": 0}, "asset_health": {"ok": 2, "...": 0}, "data_coverage": {"assets": 2, "measured": 1, "suspect": 0}, "causes": {"catching_up": 0, "...": 0}, "asset_causes": {"catching_up": 0, "...": 0}, "gap_seconds": 0, "unbounded": 0, "fresh_by": null, "also_in": []}
}
```

`as_of` is the instant the nodes were classified at (every derived time and `gap_seconds` are
as of it), and `rollup` the scope summed: the root entry of `barca status --rollup --json`
("The rollup"), for every node in scope whatever `--state`, `--problems` or `--limit` cut from
`nodes`. Its `children` are empty here; the tree and the roots are the rollup's own document.

A scheduled node adds `"schedule": {"cron": "0 6 * * *", "next_fire": "2026-10-23T06:00:00Z",
"last_fired": null, "last_status": null, "last_run_id": null, "late": false, "late_since": null,
"failures": 0, "max_failures": 5, "failing": false,
"paused": false, "paused_until": null, "skip_reason": null, "live": false,
"typical_queue_wait_seconds": 0, "typical_run_seconds": null, "queue_bound": false}` (the last
three: how long its ticks' runs typically wait for a run slot and take, in whole seconds, and
whether the wait exceeds its interval; [Scheduling With Cron](/scheduling/)), and `--state` or
`--problems` add `"filter"` beside `summary`. `health`, `health_reason`, `cause`, `root`, `gap_seconds`, `signals`,
`measured`, `last_check`, `refresh_path`, the three times and `data_coverage` are in "Health",
"Cause", "Root", "Last check", "Refresh path", "Times" and "Data signals and coverage" above; here `total` is a dict, which has no row count, so one of the two assets
is measured.

Keys inside a `sample` row are printed in alphabetical order; `columns` keeps the file's column
order. `--env <name>` reads another environment's state ([The Cache](/reference/cache/)).

An explicit `@asset(name="orders_clean")` uses `orders_clean` in status and as its SQL
view name, matching `list`, `get` and `stats`. Inspection reads the existing artifact
without importing or rerunning its producer.

## Run timing and attempts

`get --json` and `run --json` add `elapsed_seconds` and `attempts` to executed steps,
including failed steps. Cached steps and dry runs omit them. Elapsed time sums active
worker time across attempts: module import, input loading, the call and artifact
serialization; retry backoff is excluded. After a worker disconnect, elapsed time falls
back to the coordinator's observed in-flight time. For a partitioned step, elapsed time
sums its executed keys (and can exceed run wall time); attempts is the maximum for any
one key, not the number of keys.

```bash
barca get total pipeline.py --json --fields id,status,elapsed_seconds,attempts,import_seconds
barca history --json --fields run_id,status,attempts
barca stats total pipeline.py --json
```

Under `--agent` (and in every run `barca serve` starts), a worker prints
`[barca] importing <module>` on stderr before it imports a module; without `--agent` it prints
nothing of its own, and the import time is in the run JSON. Imports longer than one second add `import_seconds` to the step report, summed across
attempts and executed keys. This time is included in `elapsed_seconds`; it is not an
extra charge. A reused module adds no import time. Framework import diagnostics remain
visible under `--json`; task prints remain accessible through `barca logs`.

`--agent` completion and final failure lines append `attempts=N` when N exceeds one.
Completion lines retain their existing final-attempt duration. `history --json` adds
`attempts`, the total attempts in recorded materialization rows (zero for an all-cached
run; upload-failure rows from earlier versions retain their transfer-attempt counts). `stats` shows
attempts per recorded materialization and its stored final-attempt duration. For a partitioned
node (`barca stats scored`) an execution is a run: `total_runs` counts runs, `elapsed_seconds` and
`cpu_seconds` sum its keys, and each `recent_runs` entry adds `keys`, `failed_keys` and
`empty_keys` (keys that wrote no rows).
Timeout errors name the step id, including its partition key.

Older read-only snapshots without attempt metadata report a null history attempt total.

## What started a run

`barca history --json` rows carry `triggered_by`: `manual` for a command typed on a machine
(`barca get`, `barca run`), `api` for a run started over HTTP on `barca serve`, `schedule` for a
cron tick, and `client` for a run a laptop posted to a server's ledger. A scheduled run also
carries the tick it was started for in `scheduled_at` (RFC 3339, UTC) and the scheduled node in
`schedule_id`; both are `null` otherwise. Runs recorded before barca recorded this have
`triggered_by: null`.

```bash
barca history --json --fields run_id,triggered_by,scheduled_at,schedule_id
```

## Details

Status executes no pipeline step and records no new run, and it does not create `.barca/`.
Source parsing is static, but `partitions(<expression>)` may evaluate Python and import its
module while resolving keys. Times in the table (`LAST RUN`, like `STARTED` in `barca history`
and `CREATED` in `barca stats`) are UTC and say so; the JSON has the same UTC values. Like every inspection command, the result goes to stdout (a table in a terminal, JSON
when piped; `--json` / `--pretty` override) and errors to stderr; an unknown target is a usage
error (exit 2) that ends with ``Run `barca list <files>` to see available assets and tasks.``,
the same remediation as `get` and `run`. A target is one name or several, comma-separated with no
spaces (`a,b`), parsed exactly as `get` and `run` parse them; the JSON names them in `targets`
(and the single one in `target`).

With an artifact store ([Remote Storage](/reference/remote-storage/)), status also asks the store about every cached
unpartitioned node, with one `python -m barca._state stat` for all of them, and reports a node
whose copies do not agree as `unsynced` ([barca status: Cache state](/reference/cli/status/#cache-state)). Nothing is downloaded or uploaded.

Status reads the metadata DB as it is at that moment, and a run records each step as it
finishes. So while a `barca get` is running, status in another terminal already shows the steps
it has finished as `cached` (a partitioned asset as `partial`, with counts); see `barca docs
layout`, "While a run is going, and after one is killed".

Like `list`, status returns every node in JSON unless `--limit N` is explicit. The human
table shows at most 100 nodes unless you pass `--limit N` or `--all`. The
`summary` still counts every node, and the JSON adds `total` and `truncated` (with a `hint` when
truncated). Each node also lists `env`, the environment variables it declares with `env=[...]`
([Assets, Sensors and Inputs](/reference/assets/)), and, when its code reads variables it does not declare, `undeclared_env`
(experimental; [The Cache: Environment variables](/reference/cache/#environment-variables)). `--fields` keeps only the named keys
on each node.

## Disk usage and inspection cost

The experimental `disk_usage` summary reports project-wide storage across all environments,
even when node selection or filters narrow the graph: `budget_bytes`, `backup_bytes`,
physical `size_bytes` and `limit_bytes`. Recognized local metadata backups are outside the
cache budget and included in the physical total. Human output shows the same distinction.
Budgeted bytes use the last maintained GC measurement; status does not walk artifacts or
refresh that measurement. Backup directories are checked directly with a bounded survey
(at most 4096 directory entries across environments and backup files). Unmeasured counts,
including an incomplete backup survey, are `null` and displayed as unmeasured. Use `barca
doctor` or `barca gc --dry-run` for a full measurement. Server status reports server storage.
The UI's `/state` node array does no additional disk-accounting work.

Status inspection reuses bounded evidence instead of re-reading unchanged artifact bytes.
Local file identities include device, inode, size, mtime and ctime; changed or missing local
copies and directory-store objects are checked again immediately. Cloud availability and
remote shape observations can be up to five minutes old; cache hits do not extend that
window. Local materialization changes, confirmed transfers, credential changes and Python
import-environment changes invalidate the affected evidence. Inspection prefers an already
verified local copy for shape. This is availability
and shape evidence, not a new freshness proof. Required reads, transfers and server ledger
publication keep their direct verification paths.

Local inspection evidence is reusable only after the file's change timestamp is at least
one second old. Recently changed files are read directly without retaining evidence, so
inode reuse within a filesystem timestamp tick cannot preserve an old observation.

The disposable inspection cache is limited to 8 MiB and 8192 entries per environment, with
at most 64 KiB per value. Corruption, eviction or an unwritable cache causes another check;
a busy cache-write lock is skipped. Cold inspection still hashes selected local copies and
plain-directory store objects once, so its cost grows with their bytes. These limits do not
resolve the broader cold/per-key planning and inspection gaps (#655 and #656).

See also: [barca status](/reference/cli/status/), [The Cache](/reference/cache/), [Remote Storage](/reference/remote-storage/), [Disk Use](/reference/disk/).

---

*The same text is in your terminal: `barca docs status-fields`.*
