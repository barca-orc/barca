# Health, causes and the freshness gap

## Scope and status

Specification version: 1. Status: approved intent, implemented in part. The 0.44.0 series
([#974](https://github.com/barca-orc/barca/issues/974)) lands it issue by issue; a field, enum
value, route, flag or table named here exists only once the issue that owns it has landed.
Implemented: "Health from existing state" (#954); of "Node fields and where they appear",
`health`, `health_reason`, `signals`, `measured` and `data_coverage` (#957), experimental in
the CLI contract; the tick record ("What a tick records", #960), as the `schedule_ticks`
table and its reads; `last_check` with the `checked_up_to_date` reason ("Last check", #964);
and the rollup ("Rollup", #955): `GET /state/rollup` and `barca status --rollup`, with the
counts in both units, `keys`, and of `root` and the root list the `failure` kind only, as
that section's "As implemented" states; and `refresh_path` with its `barca.nodes` columns
("Refresh path", #961). Everything else below is intent: `cause`, every root
kind but `failure`, the other times, `gap_seconds`, the cause and gap fields
of a rollup entry and of a root, `as_of` and `rollup` in `barca status --json`, the new
tables and the query route do not exist yet.
Scope: how far a project is from a fresh DAG, why, and what closes the distance: a node's
health word, its cause bucket and root, its refresh path, when it is expected fresh or
rechecked, the freshness gap, the rollup tree, what a tick records, the `barca.schedules`,
`barca.ticks` and `barca.queue` tables, the server's query route, and the words every surface
uses for them.
Terms: [glossary](../../GLOSSARY.md). Cache states and their reasons stay owned by
[inspection](inspection.md); what a tick reconciles by [freshness](freshness.md) and the
[scheduling manual](../../crates/barca-cli/docs/scheduling.md). Exact wire shapes move to
[OpenAPI](../interfaces/server-api.openapi.yaml) and the
[CLI contract](../../crates/barca-cli/docs/contract.md) in the PR that implements each; until
then the names here are the ones those PRs use. Each rule names its implementing issue.

## Entities and state

### Four layers

| Layer | Owner | Does | Never does |
| --- | --- | --- | --- |
| Records | the scheduler and run recorder (`barca-server`, `barca-db`) | writes what was decided: runs, materializations, ticks, the queue | computes health, causes, estimates or aggregates |
| Classification | `barca-core` (a `health` module beside `status.rs`) | derives `health`, `cause`, `root`, `refresh_path`, the times and the rollup from records and the DAG | writes to the metadata database |
| Aggregation | `barca sql` tables in the `barca` schema, queried with DuckDB | trends, rankings and drill-downs over rows Rust hands to DuckDB | reads `metadata.db` through DuckDB's sqlite extension |
| Presentation | the CLI and the web UI | render fields and query results: layout, formatting, fetching | derive a state word, a count, a cause, a time or a sum |

A static map from an enum value to a display word, tone or glyph is presentation. Anything
that reads two fields to produce a third is classification and lives in Rust.

### Vocabulary

Health words are barca vocabulary, defined once in `barca-core` and shown by the CLI and the UI
alike. They sit beside the cache state (`cache.state`), never replace it.

`health` (string enum, most severe first; this order sorts and ranks everywhere):

| Value | Display word | Meaning |
| --- | --- | --- |
| `failing` | failing | its own last attempt failed |
| `blocked` | blocked | something upstream keeps it from being brought up to date |
| `suspect` | suspect data | its last run succeeded but its recorded row counts look wrong |
| `behind` | behind | out of date against its inputs, partly computed, out of sync, or late |
| `undecided` | undecided | barca cannot decide yet (a cutoff or sensor upstream has not reported) |
| `never_run` | never run | nothing recorded |
| `ok` | ok | fresh, or a task or sensor whose last run succeeded |

`undecided` replaces the UI's `waiting`; it is the one word for the grey states.

`cause` (string enum, `null` exactly when the node is fresh):

| Value | Display word | Meaning |
| --- | --- | --- |
| `catching_up` | catching up | a run is computing it, or the next tick of its refresh path will |
| `queued` | queued | what would refresh it waits for a run slot |
| `no_refresh_path` | no refresh path | nothing scheduled will ever refresh it |
| `overdue` | overdue | ticks reached it and did not leave it fresh |
| `failing` | failing | it failed, or a failing node upstream holds it |
| `undecided` | undecided | the read path cannot decide; the last tick's verdict is shown |

A node is **fresh** when its `health` is `ok` or `suspect`. `suspect` is a statement about
data, not freshness: a suspect node has no cause and no gap.

### Identities and times

Node ids are those of `barca status --json` (`pipeline.py:orders`). A schedule is identified by
its scheduled node's id (`schedule_id`, as in `barca history --json`). A tick is
`(schedule_id, scheduled_at)`. A root is `<kind>:<node id>` (`code:pipeline.py:orders`).
Every new time is an RFC 3339 UTC string (`2026-11-17T15:02:00Z`), like `schedule.next_fire`
and `scheduled_at`; every new duration is seconds, with a `_seconds` suffix. Classification
happens at one instant, `as_of`, carried by every response that holds a derived time.

## Rules and transitions

### Health from existing state (#954)

Implemented in `crates/barca-core/src/health.rs` (`classify`), ported from the web UI's
TypeScript without change of meaning. Inputs: the node's
`NodeStatus`, the graph, the node's data signals, and whether its schedule is late.

`last_attempt_failed` is `last_materialization.status == "failed"` or `partitions.failed > 0`.

`blocked_by` is the nearest failing node upstream: walking a node's inputs in declared order,
an input whose last attempt failed is the answer; an input that is itself neither failing,
`cached` nor `always_runs` passes the question to its own inputs. Only a node that is neither
failing, `cached` nor `always_runs` can be blocked. A pinned asset (`barca pin`) is never
blocked and stops the walk. This walk passes through `unknown` nodes, so a node waiting on a
cutoff below a failure is blocked by the failure. As implemented, "declared order" is the
order of the node's `inputs` in `NodeStatus`, which is sorted by node id: two failing inputs
answer with the first by id.

The first matching row decides `health`; `health_reason` (string enum) names the row.

| # | Condition | `health` | `health_reason` |
| --- | --- | --- | --- |
| 1 | `last_attempt_failed` | `failing` | `last_attempt_failed`; `keys_failed` when only `partitions.failed > 0` |
| 2 | `blocked_by` names a node | `blocked` | `upstream_failing` |
| 3 | `cache.state == blocked` or `cache.reason == upstream_failed` | `blocked` | `cache_blocked` |
| 4 | a data signal `empty` or `rows_changed` | `suspect` | `empty`, else `rows_changed` |
| 5 | a data signal `behind_input` | `behind` | `behind_input` |
| 6 | its schedule is `late` (below) | `behind` | `schedule_late` |
| 7 | `cache.state` is `stale`, `partial` or `unsynced` | `behind` | the state: `stale`, `partial`, `unsynced` |
| 7a | `cache.state == unknown` and the last check's verdict stands ("Last check", #964) | `ok` | `checked_up_to_date` |
| 8 | `cache.state == unknown` | `undecided` | `unknown` |
| 9 | `cache.state == never_run` | `never_run` | `never_run` |
| 10 | `cache.state == always_runs` and `last_materialization` is `null` | `never_run` | `no_successful_run` |
| 11 | `cache.state == always_runs` | `ok` | `last_run_succeeded` |
| 12 | `cache.state == cached` | `ok` | `cached` |

A partitioned node is one node with one health. A run working on a node (`running`) is a
separate fact shown beside its health; it does not change it.

### What estimates change (#958, #962, #964)

- Row 6 reads one definition of late. A schedule is `late` when `late_since` is set (the first
  slot after the tick of the newest run it started, when that slot has passed), it is not
  paused, and `as_of - late_since` exceeds its grace plus its typical queue wait. Grace is 60
  seconds, or half the interval for a schedule more frequent than every two minutes. Typical
  queue wait is the median `queue_wait_seconds` of its newest 20 fired ticks (0 with none).
  `GET /schedule`, `GET /state` and `barca status` read this one function.
  Implemented (#958) as `barca_core::schedule_standing::standing`, with these fields on
  `GET /schedule` entries and in the `schedule` block of `GET /state`, `GET /nodes` and `barca
  status --json`: `last_fired`, `last_run_id`, `last_status`, `next_fire`, `late`,
  `late_since`. As implemented, a tick is one whose run was started or queued, so
  `late_since` is the first slot after the newest such tick; the queue wait is the typical
  one or, when longer, the newest tick's own (to now while its run still waits), so a tick
  waiting for a run slot is not late; and until the tick records of #960 exist, the waits
  are read from the `runs` rows a schedule started (slot to start), not from
  `queue_wait_seconds`. Conformance: `a_queued_tick_reads_the_same_on_every_surface`
  (`crates/barca-server/tests/running.rs`) and the tests of `schedule_standing`.
- Row 8 gains a row before it (7a, implemented): when the newest finished tick that checked
  the node found it up to date or rebuilt it and nothing the read path can see has changed
  since, `health` is `ok` with `health_reason: "checked_up_to_date"`, and `last_check` carries
  the tick. "Last check" below states the condition.
- `health` does not say whether a `behind` or `never_run` node is a problem. `cause` does.

### Last check (#964)

Implemented in `crates/barca-core/src/health.rs` (`checks`, `verdict_stands`).

**Why the read path cannot decide.** `cache.state` predicts what a request would do, without
running anything ([inspection](inspection.md)), by the rules a run follows: a `Manual` or
pinned node is read as it is ([cache decisions](cache-decisions.md), "The rules the
prediction follows", #985). Where the answer depends on what only a run produces, it is
`unknown`: below a cutoff asset the prediction says would run (`cutoff_pending`: the bytes it
will write), a sensor with no recorded output (`sensor_output_unknown`), a `partitions_from`
source whose output is not available (`partitions_unknown`). A run does produce it. A tick's
run also follows one rule a request does not: it reads a node paused with `barca pause` as it
is (`RunRules::tick`), where a request, and so the prediction, recomputes it. So below a
paused node that is stale against a sensor polled every tick, the prediction says a cutoff
asset under it would run, and everything below that is `unknown`, until the node is resumed;
each tick reruns the cutoff asset, which writes the same bytes, and finds what is below it up
to date. The run decided what the read path cannot, and the tick record holds its verdict.

`last_check` is `{"at", "outcome", "schedule"}` of the newest finished fired tick that had the
node as a target, across schedules, newest by when its run ended: `at` is that time,
`outcome` what the tick recorded (`rebuilt`, `up_to_date`, `failed`, `not_reached`),
`schedule` the scheduled node's id. It is `null` when no retained tick lists the node (the
newest 20 fired ticks of each schedule do). It is a record: served whatever its age, for every
node kind.

The verdict of a last check **stands**, and row 7a applies, when all hold at `as_of`:

1. `outcome` is `up_to_date` or `rebuilt`. A newer `failed` or `not_reached` outcome is the
   last check and decides nothing; a recorded failure is row 1.
2. It has not expired: `as_of <= finished_at + (finished_at - scheduled_at) + interval +
   grace`, where `interval` is from the tick's slot to its schedule's next slot and `grace` is
   row 6's (60 seconds, or half the interval under two minutes). That is when the next tick's
   verdict is due: one interval after this run ended (a run longer than the interval skips
   ticks), taking as long as this one did from slot to end. The schedule must still be a
   scheduled node of the graph; a check of one that is not never stands.
3. The node's definition hash is the one its newest successful result recorded
   (`materializations.components`; a row older than that key is compared digest by digest; a
   node with no successful result or no recorded components does not pass).
4. No node in its upstream cone has a materialization recorded after `finished_at`, other
   than rows of a run that a fired tick started and that has not ended (its own verdict is on
   its way).

A node whose verdict stands raises no `behind_input` signal: the tick read its inputs' newest
results and found it up to date. Rows 1 to 6 still come first, so a failing, blocked or
suspect node, or the scheduled node of a late schedule, is not `ok`. A node the read path
decides (`stale`, `cached`, ...) keeps that answer whatever its last check says. The
prediction and a tick decide a node below a `Manual` or pinned node alike (#985); they still
differ below a stale paused node with no cutoff asset in between, which reads `stale`,
`upstream_stale` (a request would recompute the paused node) while ticks find it up to date,
and this rule does not apply there (a limit below).

Sensors and tasks: a polled sensor is recorded `rebuilt` whether or not its observation
changed ("What a tick records"), and a task that fired is `rebuilt`. Their state is
`always_runs`, never `unknown`, so no check decides their health; `last_check` carries the
record as written, and `rebuilt` on a sensor means "polled". An asset's outcome is its own
step's result and does not depend on that limitation.

Cost: two reads per classification of a scope, never one per node. One reads
`schedule_ticks` once for every node (`db::latest_ticks`: the retained target lists, and the
runs of ticks in flight); one reads by primary key the newest successful row of each asset
whose check could stand (`db::components_of_rows`). The server keeps the result per graph and
database state (`AppState::cached_checks`), so a poll that finds nothing changed reads
nothing; expiry is a time the result carries, compared with the clock per answer.

What it does not see, bounded by the expiry: an edit to a node above (shown on that node,
which the next tick recomputes), and a new value of a declared environment variable.

### Refresh path (#961)

Implemented in `crates/barca-core/src/refresh_path.rs` (`Paths`, `path_schedule`,
`refresh_path`). `refresh_path` is the schedule whose tick reaches the node, or `null`.

```json
{"schedule": "pipeline.py:inbox", "next_tick": "2026-11-17T15:00:00Z",
 "paused": false, "live": true, "missed_ticks": 0}
```

- The walk is the scheduler's own target walk (`freshness::tick_targets`, read through
  `freshness::tick_reach`), called from both places, never a second copy. A node's refresh
  path is a schedule that reaches it as a `Target`: its scheduled node, a scheduled node above
  it, or a schedule whose run polls an unscheduled sensor above it (#978). A node reached only as an
  `Input` has none. Which nodes a tick reaches is that walk's answer and is specified in
  [freshness](freshness.md) and the scheduling manual, not here.
- A scheduled node's refresh path is its own schedule. When several schedules reach a node,
  the one with the earliest `next_tick` is reported, one with no `next_tick` last; ties break
  by `schedule` id. The flags are that schedule's, so a node reached by a paused schedule and
  a firing one reports the firing one.
- `next_tick` is the schedule's next slot; the first slot after `paused_until` for a timed
  pause; `null` for an indefinite pause or when `live` is false.
- `live` is whether a running scheduler fires it. It is false when the answering process runs
  no scheduler for the schedule (`barca status` without a server, a `--read-only` or
  `--no-schedule` server: `schedule.live` is false too) and when the scheduler holds the
  schedule as `failing` past `max_failures` (`schedule.live` stays true there; `schedule.failing`
  says why). Without a firing scheduler ticks never happen and no time is estimated. A cron
  that does not parse is refused when the graph loads, so no loaded graph has one.
- A paused schedule (`paused`) and a schedule that is not `live` are still the path: the
  field says which schedule would refresh the node and why it does not. What makes the path
  `null` is on the node itself: a paused node (`barca pause`) is a skipped step of every tick,
  and a pinned asset (`barca pin`) is read as its pinned result, so neither has a path. The
  nodes below one keep theirs: the target list is fixed by the graph, and a tick brings them
  up to date against what the paused or pinned node holds.
- `missed_ticks` counts the finished fired ticks of the path's schedules (every schedule that
  reaches the node as a target, not only the one reported) that had the node as a target and
  did not leave it fresh: outcome `failed` or `not_reached`. It counts back from the newest
  finished tick to the last one, of any schedule, whose outcome for the node was `up_to_date`
  or `rebuilt`, and only ticks that ended after the node's newest successful result (a run no
  tick started made the earlier ones good). It is 0 for a fresh node. A node still not fresh
  after an `up_to_date` or `rebuilt` outcome was not missed by that tick. Until `stale_since`
  exists (#962) this is what "since the node went out of date" means. The count is bounded by
  the tick record: the newest 20 fired ticks of each schedule keep their target lists, so a
  schedule contributes at most 20; a skipped, paused or unfired tick is not a miss.

Cost: the walk depends on the graph alone and takes seconds on a graph of thousands of nodes
and dozens of schedules, so it runs once per loaded graph (`AppState::cached_paths`; once per
CLI command), never per poll or per node. The schedule state is each schedule's standing,
already computed per answer; the misses come with the one read "Last check" makes.

Entry points for what is derived from a path (#962, #963): `refresh_path::path_schedule(node,
evidence)` answers the path's schedule with its `PathSchedule` (`next_tick`, `paused`, `live`,
`failing` and the schedule's `Standing`: last tick, typical queue wait, lateness);
`health::Evidence::path` carries that evidence into classification.

### Cause (#963, #973)

Every node that is not fresh gets exactly one cause: the first matching row.

| # | Condition | `cause` |
| --- | --- | --- |
| 1 | `health` is `failing`; or `blocked` with a `blocked_by` node; or its refresh path's schedule is `failing` (past `max_failures`) | `failing` |
| 2 | `health` is `undecided` | `undecided` |
| 3 | a running run plans the node | `catching_up` |
| 4 | `refresh_path.missed_ticks >= 1` | `overdue` |
| 5 | a run that plans the node waits in the run queue (`run_queue` state `queued`, run status `pending`); or its refresh path's schedule is queue-bound | `queued` |
| 6 | `refresh_path` is `null`, or its `next_tick` is `null` | `no_refresh_path` |
| 7 | otherwise | `catching_up` |

A schedule is **queue-bound** when its typical queue wait exceeds its interval.

### Root (#963)

Every node with a cause has one `root`: the upstream fact that explains it.

```json
{"id": "sensor_value:pipeline.py:inbox", "kind": "sensor_value", "node": "pipeline.py:inbox"}
```

| `kind` | Cause it explains | `node` | Clearing `action.kind` |
| --- | --- | --- | --- |
| `sensor_value` | catching up, overdue, queued | the sensor whose observation changed | `wait`, or `run` when overdue |
| `code` | same | the node whose code changed (`code_changed`) | same |
| `env` | same | the node whose declared environment variable changed (`env_changed`) | same |
| `input` | same | the node whose input changed (`inputs_changed`) with no changed node above it | same |
| `never_run` | same | the topmost never-run node of the chain | same |
| `no_schedule` | no refresh path | the topmost not-fresh node with no refresh path | `add_schedule` |
| `schedule_paused` | no refresh path | the paused scheduled node | `resume_schedule` |
| `scheduler_off` | no refresh path | the scheduled node, when no scheduler is live | `start_server` |
| `failure` | failing | the failing node (`blocked_by`, or itself) | `fix_failure` |
| `queue` | queued | the queue-bound scheduled node | `raise_max_runs` |
| `undecided` | undecided | the topmost undecided node of the chain | `wait` |

Attribution walks upstream from the node through inputs that are not fresh and stops at the
first node whose own evidence explains it (`cache.change_causes` other than `upstream_stale`,
a changed observation, a failure). "Topmost" and every tie break by dependency order, then
node id, so attribution is deterministic. A `queue` root wins over a change root for a node
whose cause is `queued`.

The list of roots (in the rollup, below) carries what each holds and what clears it:

```json
{"id": "no_schedule:pipeline.py:orders", "kind": "no_schedule", "node": "pipeline.py:orders",
 "detail": "no tick reaches orders", "error_group": null,
 "nodes": 152, "assets": 140, "causes": {"no_refresh_path": 152}, "gap_seconds": 8208000,
 "unbounded": 152,
 "action": {"kind": "add_schedule", "command": null, "estimated_seconds": 420}}
```

`action.command` is a runnable `barca` command when one exists (`barca get orders --server`
for `run`), else `null`. `action.estimated_seconds` is the typical compute time of the nodes
held (the duration rule below), `null` when no node has samples. `error_group` is a failure
root's error headline with its numbers replaced by `#`; failure roots sharing one are shown
as one entry. Roots are ranked by `gap_seconds`, then `nodes`, then `id`.

### Times (#962, #964)

| Field | Meaning | `null` when |
| --- | --- | --- |
| `stale_since` | the earliest recorded time from which the node has been continuously not fresh | fresh; undecided; or no record holds the time |
| `expected_fresh_at` | when the node is expected to be fresh | fresh; or cause is not `catching_up` or `queued`; or the path has an undecided node |
| `recheck_at` | when a tick next evaluates the node | no refresh path, or its `next_tick` is `null` |
| `last_check` | `{"at", "outcome", "schedule"}`: the newest finished tick that had the node as a target (implemented, "Last check") | no retained tick did |

- `stale_since` by root kind: a changed observation or input is the `created_at` of the root's
  current result; a failure is `failing_since`; code, environment and never-run use the time
  the definition was first recorded. Which record holds that time is #962's to fix, with no
  write added to the tick path beyond the tick record.
- `expected_fresh_at = start + path`. `start` is `as_of` for a node in a running run; the
  queue entry's `estimated_start_at` for a waiting run; else `next_tick` plus the schedule's
  typical queue wait. `path` is the longest chain of typical step durations over the not-fresh
  nodes the run computes at or above the node, the node included. A typical duration is the
  median of the node's recent successful runs (`durations.median_seconds`); a node without
  samples takes the project's median. Worker contention is not modelled; #962 states the
  error measured on the seeded server simulation.
- An estimate only promises what barca will do. Where a result upstream may come out
  unchanged, the node may be found up to date or left as it is, so only `recheck_at` is given.
  An overdue node has no `expected_fresh_at`: its estimate already failed.
- A queue entry's `estimated_start_at` is `as_of` plus the typical run time of the runs ahead
  of it, divided over `max_runs` slots.

### The freshness gap

Per node, in seconds, at `as_of`:

```text
behind_seconds = as_of - stale_since                    (0 when stale_since is null)
ahead_seconds  = max(0, expected_fresh_at - as_of)      (0 when expected_fresh_at is null)
gap_seconds    = behind_seconds + ahead_seconds         (0 for a fresh or undecided node)
```

A project's gap is the sum over its nodes, in node-seconds: one number that trends to zero. A
partitioned node counts once. A not-fresh node with no `expected_fresh_at` is **unbounded**:
its gap grows until someone acts, and every sum states how many such nodes it holds
(`unbounded`) instead of pretending a forward term.

### Node fields and where they appear

All are fields of `NodeStatus`, so one struct serves every surface.

| Field | Type | `barca status --json`, `GET /nodes`, `GET /state` | `barca.nodes` column |
| --- | --- | --- | --- |
| `health` | string enum | yes | `health` |
| `health_reason` | string enum | yes | `health_reason` |
| `cause` | string enum or `null` | yes | `cause` |
| `root` | object or `null` | yes | `root_id`, `root_kind`, `root_node` |
| `refresh_path` | object or `null` | yes | `refresh_schedule`, `next_tick`, `missed_ticks` |
| `stale_since` | time or `null` | yes | `stale_since` |
| `expected_fresh_at` | time or `null` | yes | `expected_fresh_at` |
| `recheck_at` | time or `null` | yes | `recheck_at` |
| `last_check` | object or `null` | yes | `last_check_at`, `last_check_outcome` |
| `gap_seconds` | number | yes | `gap_seconds` |
| `signals` | array (today's `GET /state` shape) | yes (moves from `GET /state` only) | `signals` (list of kinds) |
| `measured` | boolean, `null` for a task or sensor | yes | `measured` |

`measured` is whether the asset's latest successful result has a recorded row count (for a
partitioned asset, any current key). An unmeasured asset raises no signal and is never `ok`
for data: it is "not measured". As implemented, a partitioned asset is measured when the latest
successful result of any key recorded a count, a key no longer in the key set included (the
per-key row facts of the metadata database do not know the current key set). `data_coverage`
counts assets only; `suspect` is the measured assets with an `empty` or `rows_changed` signal,
whatever their `health` (a failing asset whose last good result was empty is still counted).

`barca status --json` adds, beside `summary`: `as_of`; `rollup`, the root entry of the tree
below; and `data_coverage`: `{"assets": 556, "measured": 7, "suspect": 0}`. Without a server,
`refresh_path.live` is false, `next_tick` and the times are `null`, and a scheduled chain's cause is
`no_refresh_path` with root kind `scheduler_off`.

### Rollup (#955)

One tree: root, group, file, node. The root's children are the top-level groups and, for
nodes in no group, their source files; a group's children are its member groups and nodes; a
file's children are its nodes. Every node has exactly one parent, so every level is the exact
sum of its children; a node declared in several groups sits under the first by group id.

Route `GET /state/rollup`, CLI `barca status --rollup`: the rollup is the rows of `GET /state`
summed, so it lives under that name, takes the same scope (`targets`, `files`; the CLI's
targets) and never collides with `GET /health`, which is liveness.

```json
{"as_of": "2026-11-17T14:57:00Z",
 "entries": [{"id": "group:ingest", "kind": "group", "label": "ingest", "parent": "root",
   "children": ["pipeline.py:inbox", "pipeline.py:orders"],
   "nodes": 2, "assets": 1, "running": 0,
   "health": {"failing": 0, "blocked": 0, "suspect": 0, "behind": 1, "undecided": 0, "never_run": 0, "ok": 1},
   "asset_health": {"failing": 0, "blocked": 0, "suspect": 0, "behind": 1, "undecided": 0, "never_run": 0, "ok": 0},
   "causes": {"catching_up": 1, "queued": 0, "no_refresh_path": 0, "overdue": 0, "failing": 0, "undecided": 0},
   "asset_causes": {"catching_up": 1, "queued": 0, "no_refresh_path": 0, "overdue": 0, "failing": 0, "undecided": 0},
   "gap_seconds": 95, "unbounded": 0, "fresh_by": "2026-11-17T15:00:20Z"}],
 "roots": [],
 "data_coverage": {"assets": 1, "measured": 0, "suspect": 0}}
```

- `kind` is `root`, `group`, `file` or `node`; ids are `root`, `group:<id>`, `file:<path>` and
  the node id. `children` are ordered worst health first, then most assets, then label.
- `nodes` counts every node underneath; `assets` counts assets only (tasks and sensors are
  nodes, not assets). `health` and `causes` count nodes; `asset_health` and `asset_causes`
  count assets. Both units are always present and every display names the one it shows.
- `fresh_by` is the latest `expected_fresh_at` underneath, `null` when none is estimated. It
  speaks only for the nodes that have one; `unbounded` and `causes.undecided` say the rest.
- A node entry adds `keys` for a partitioned node: `{"failed", "behind", "ok"}`.
- `roots` is the root list above, scoped to the nodes in the response.

As implemented (#955), in `crates/barca-core/src/rollup.rs` (`build`), which the route, the
CLI and the web UI all read:

- Served on an entry: `id`, `kind`, `label`, `parent` (`null` on the root), `children`,
  `nodes`, `assets`, `running`, `health`, `asset_health`, and `keys` on a partitioned node.
  An entry also carries `data_coverage` (`{"assets", "measured", "suspect"}` for the assets
  underneath), which this section did not name: it is what lets a surface state the coverage
  of a group without adding it up, and it sums like every other count. Absent, not `null`,
  until their issues land: `causes`, `asset_causes`, `gap_seconds`, `unbounded`, `fresh_by`
  (#961 to #963).
- A group entry's id is the group's own id, which already reads `group:<file>:<name>`
  (`GET /groups`); it is not prefixed again. A file entry holds the nodes of that file that
  are in no group, so a file whose nodes are all grouped has no entry. A group with no node
  in scope is not listed. A group that is a member of several groups is placed like a node:
  under the first by group id, and never under a group it contains.
- `entries` lists the root first, then depth first in `children` order. A node's `label` is
  its declared name; the root's is `all`.
- `children` order is worst health first (the most severe health with a node underneath),
  then most assets, then label, then id.
- `running` counts the nodes a run is working on (`running` of `GET /state`). `barca status
  --rollup` on a machine without a server tracks no run and answers 0.
- `root` on a node (`NodeStatus.root`) and `roots` hold the `failure` kind only: a `failing`
  node's root is itself, and a node `blocked` by a failing node (`blocked_by`) has that node.
  Every other node, a `cache_blocked` one included, has `root: null` and is under no root, so
  INV-HEALTH-04 holds for failures only. A root carries `id`, `kind`, `node`, `detail` (the
  error headline as recorded: its first line, or the last line of a bare traceback; `N of M
  keys failed` for a node whose keys failed with no error of its own), `error_group` (`null`
  when no error was recorded), `nodes` and `assets`. Absent until #961 to #963: `causes`,
  `gap_seconds`, `unbounded`, `action`. Roots are ranked by `nodes`, then `id`.
- The scope is that of `GET /state`: targets stand for their upstream cones, so the rollup of
  one group or file also counts what it reads from outside it.
- The server sums the status result `GET /state` is built from (no status is computed for the
  rollup) and classifies per answer. Its `as_of` is the start of the current minute, the
  instant lateness is read at, so an unchanged poll within a minute answers `304`; the CLI's
  is the instant of the command.
- `barca status --json` does not carry `as_of` or `rollup` yet ("Node fields and where they
  appear"); the rollup is its own document.

### What a tick records (#960)

Implemented. One bounded table in the metadata database, `schedule_ticks`
([schema](../interfaces/metadata-schema.md), [owner](../../crates/barca-db/src/db/ticks.rs)),
written by the scheduler and the run recorder of `barca serve` from values they already hold;
no read is added to the tick path. `freshness_proofs` is not this table. Logical content, one
row per due tick of a schedule:

| Field | Content |
| --- | --- |
| `schedule_id`, `scheduled_at` | the scheduled node and the slot |
| `action` | `fired`, `paused`, `skipped` (a run of the node in flight) or `failing` (past `max_failures`) |
| `skipped_for` | the run in flight, for `skipped` |
| `run_id`, `started_at`, `finished_at`, `run_status` | the run a fired tick started |
| `queue_wait_seconds` | from the run joining the run queue (`run_queue.queued_at`) to its start; `null` until it starts |
| per target of a fired tick | `rebuilt`, `up_to_date`, `failed` or `not_reached` |

Three writes, each one transaction: the decision, when the scheduler makes it (the row names
the run before the run can start); the run's start, when it leaves the queue; the run's end,
with the outcomes. A startup catch-up records its decision at the time it is made, the slot
its run carries. A fired tick whose run could not join the queue has no `run_id`. A run put
back in the queue by a draining server records its latest start; a run its server was killed
under ends its tick as its run row ends (`interrupted`, or `error` for a run that never wrote
one) with every target `not_reached`. A plain CLI run, a run someone asked a server for and a
`--read-only` server write nothing.

Outcomes come from the run's own step results: a step that executed is `rebuilt` (a task that
fired, a partitioned node with at least one key computed), a cache hit `up_to_date`, a failed
step `failed`. `not_reached` covers a target a failure upstream, a cancellation or the run's
end kept the run from deciding. A partitioned target counts once, by its worst key.

Known limitation: a polled sensor is recorded `rebuilt` whether or not its observation
changed. The intent is `up_to_date` for an unchanged observation; a run's step results do not
carry that today, and the writer adds no read to learn it.

Retention: per schedule the newest 1,000 ticks and none older than 14 days; per-target outcomes
for the newest 20 fired ticks of each schedule, counts only for older ones. Trimming happens on
write, inside the write's transaction; the ticks of a schedule that no longer fires are trimmed
by age at scheduler startup and about hourly while ticks are recorded.

Reads (`barca_core::db`): `ticks_for_schedule(db_path, schedule_id, limit)`, newest first;
`tick_outcomes_for_node(db_path, node_id, limit)`, what the newest finished fired ticks that
still hold their target list did with one node, across schedules; and `latest_ticks(db_path)`,
the newest such outcome of every node at once with the runs of the ticks still in flight
(what "Last check" reads). All answer empty for a database without the table.

### Tables (#965)

Added to the `barca` schema beside `barca.runs`, `barca.materializations` and `barca.nodes`,
with the same rules (read when named, text times that sort, a read-only copy).

| Table | One row per | Columns |
| --- | --- | --- |
| `barca.schedules` | scheduled node | `id`, `name`, `kind`, `cron`, `interval_seconds`, `targets` (list), `next_fire`, `last_fired`, `last_status`, `last_run_id`, `late`, `late_since`, `paused`, `paused_until`, `failing`, `failures`, `max_failures`, `skip_reason`, `live`, `typical_queue_wait_seconds`, `typical_run_seconds`, `queue_bound` |
| `barca.ticks` | recorded tick | `schedule_id`, `scheduled_at`, `action`, `skipped_for`, `run_id`, `run_status`, `started_at`, `finished_at`, `queue_wait_seconds`, `rebuilt`, `up_to_date`, `failed`, `not_reached` (counts), `targets` (list of `{node, outcome}`, `null` past the newest 20) |
| `barca.queue` | run in the run queue | `run_id`, `position` (1 = next; `null` when running), `state` (`queued`, `running` or `draining`, as `run_queue` records it), `queued_at`, `started_at`, `estimated_start_at`, `wait_seconds`, `command`, `target`, `triggered_by`, `schedule_id`, `scheduled_at` |

`barca.nodes` gains the columns of the node field table. The manual ships three queries: the
gap (`select sum(gap_seconds), count(*) filter (where cause is not null and expected_fresh_at is null) from barca.nodes`),
the rollup by group, and the top roots.

### Server query route (#972)

`POST /sql/query`, body `{"query": "...", "limit": 1000}`, answers the document `barca sql
--json` prints (`columns`, `rows`, `total`, `truncated`, `hint`).

- Only `barca.*` tables exist in the helper's database. A query naming a result view, any
  other relation, or more than one statement, or one that is not a `SELECT` (or `WITH ...
  SELECT`), is refused with `400` and the usage error envelope.
- `limit` defaults to 1,000 and is at most 10,000; `truncated` and `total` say what was cut.
- A query has 5 seconds, waiting included; past it the answer is `408` and the query is
  cancelled.
- The server's auth applies as on every route (project tokens when enabled). It is a read: a
  `--read-only` server serves it.
- One long-lived DuckDB helper, started on first use, with external access disabled (no file,
  network or extension access) and a memory limit; Rust hands it rows and refreshes a table
  when the metadata database state changes. Never a process per request.
- `barca sql --server` sends a query that names only `barca.*` tables to this route. A query
  naming result views runs on the client as today. This narrows the manual's "the server never
  runs SQL" to: the server never runs SQL over results or files.
- Overview polling stays on `GET /state` and `GET /state/rollup`; this route serves trends and
  drill-downs.

### Wording

- Catching up and queued are neutral: progress, not a warning. Only `overdue` and
  `no_refresh_path` alarm, beside `failing`, which keeps its own tone. `suspect` is a data
  warning independent of freshness.
- "Fresh" is said only with `expected_fresh_at` ("fresh by about 15:02"). With only
  `recheck_at` the word is "rechecked" ("rechecked at 15:00"). `last_check` reads "checked 3m
  ago, up to date". Estimates say "about".
- Grey has one word, `undecided`. "Waiting", "unknown", "checking" and "not known yet" are not
  health words on any surface; a row with no state loaded is a loading state, not a health.
- A count names its unit: "14 overdue assets", or "nodes". A sum over a scope with unbounded
  nodes says so: "about 180 fresh within 5m · 152 no refresh path".
- Data coverage is always stated: "7 of 556 measured"; never "no suspect data" alone.
- A display word comes from the enum tables above. The CLI prints the same word the UI shows.

## Invariants

- **INV-HEALTH-01:** Every node has exactly one `health`, and exactly one `cause` when it is
  not fresh; a fresh node has none.

- **INV-HEALTH-02:** A node's `health`, `cause`, `root` and times are equal in `barca status
  --json`, `GET /nodes`, `GET /state`, `GET /state/rollup` and `barca.nodes` for the same
  metadata state and `as_of`.

- **INV-HEALTH-03:** Every count and `gap_seconds` of a rollup entry equals the sum over its
  children, and every node is under exactly one parent.

- **INV-HEALTH-04:** Every node with a cause is held by exactly one root, and the roots' `nodes`
  add up to the not-fresh nodes in scope.

- **INV-HEALTH-05:** `expected_fresh_at` is never given where a result upstream may come out
  unchanged or a tick already missed the node; `recheck_at` is given instead.

- **INV-HEALTH-06:** Refresh-path membership equals the scheduler's own target set for the same
  DAG: a node has a refresh path through a schedule exactly when that schedule's tick has it
  as a target.

- **INV-HEALTH-07:** The scheduler writes records only; recording a tick adds no metadata read
  to the tick path, and the tick table stays within its retention.

- **INV-HEALTH-08:** The UI shows no state word, count, cause, time or sum that `barca status
  --json` or a `barca.*` table does not carry.

- **INV-HEALTH-09:** The server query route reads `barca.*` tables only and can read or write
  no file, result view or network resource.

## Failures and recovery

- Classification never fails a read. A value it cannot derive is `null` (a time) or the most
  conservative word (`undecided`, `no_refresh_path`), never a guess.
- The tick table is evidence, not state. Losing it (a reset, a restored backup, a trim) loses
  `last_check`, `missed_ticks` and typical queue waits until new ticks record them: an overdue
  node reads `catching_up` until the next finished tick misses it. A failed tick write is
  logged and does not fail the tick.
- After a restart, `GET /schedule`'s in-memory skip fields are empty as today; the tick record
  keeps what was skipped.
- A `--read-only` server classifies from the shared metadata: it reads the ticks the
  scheduling server wrote (`last_check`, `missed_ticks` and row 7a work there as on the
  scheduling server). It runs no scheduler, so its `refresh_path.live` is false and its
  `next_tick` is `null`: it cannot tell whether the scheduling server is up.
- When ticks stop (the scheduler is down, the schedule paused or removed), a standing verdict
  expires within one interval plus the last tick's duration and grace, and the node reads
  `undecided` again; `last_check` keeps the record.
- The query helper is restarted when it dies or exceeds its memory limit; the request in
  flight answers `503`. Without DuckDB installed the route answers `503` naming the extra;
  every other route is unaffected.

## Acceptance and evidence

Each issue lands its scenario with the code and replaces the matching source link. Passing
today (with the #961 row): #954 ([health rules](../../crates/barca-core/src/health.rs): the twelve rows, the walk
through `unknown` nodes and past pins, the signal rules), #957 (the same file's coverage
test, the [CLI contract snapshot](../../python/tests/snapshots/cli_contract/status.txt), and
the web UI's [coverage line](../../ui/src/lib/dataSignals.test.ts)), the #960 row and the
#964 row and the #985 row. The rest do not pass yet.

| Scenario | Issue |
| --- | --- |
| The twelve health rows, `blocked_by` through `unknown` nodes and past pins, as Rust tests ported from the TypeScript ones | #954 |
| The map, the bars and `barca status --rollup` print the same counts from one response; asset and node units both present | #955: [rollup rules](../../crates/barca-core/src/rollup.rs) (INV-HEALTH-03 on every fixture, one parent per node, both units, the failure roots), [the route against `GET /state`](../../crates/barca-server/tests/openapi.rs), [no second status computation](../../crates/barca-server/src/read_cache.rs), [the CLI contract snapshot](../../python/tests/snapshots/cli_contract/status_rollup.txt), [the map's layout over the response](../../ui/src/lib/rollupView.test.ts), [the bar, the map and a group row against one response of a served project](../../ui/e2e-groups/groups.spec.ts) |
| A project with 7 of 556 assets counted reads "7 of 556 measured" in status JSON and the UI | #957 |
| A fixture with a queued tick: `GET /schedule`, `GET /state` and `barca status` agree on last tick, last run and late (passes: `a_queued_tick_reads_the_same_on_every_surface`) | #958 |
| Re-adding a derived word in TypeScript fails CI | #959 |
| A fired, paused, skipped and failing tick each leave one row; the perf gate is unchanged | #960: [owner tests](../../crates/barca-db/src/db/ticks.rs), [scheduler tests](../../crates/barca-server/src/scheduler/tests.rs) ("the tick record"), [outcomes](../../crates/barca-core/src/serve/ticks.rs), [a served pipeline](../../python/tests/test_serve_schedule.py) |
| A scheduled sensor with `Always` assets below, an unscheduled chain and a `Manual` node: refresh paths equal the tick's targets | #952, #961: [the shapes, a paused schedule, no live scheduler, paused and pinned nodes, missed ticks](../../crates/barca-core/src/refresh_path.rs) ("membership is the scheduler's own target list"), [the batched misses](../../crates/barca-db/src/db/ticks.rs), [the walk once per graph](../../crates/barca-server/src/read_cache.rs), [CLI, `GET /state`, `GET /nodes`, `barca.nodes`, a scheduling and a read-only server](../../python/tests/test_refresh_path.py) |
| Estimates within the stated error on the seeded server simulation; a node below a pending cutoff has `recheck_at` only | #962 |
| On the audit fixture the top three roots hold at least 90% of not-fresh nodes; every not-fresh node is under one root | #963 |
| An asset a tick checks every 5 minutes and finds up to date reads "checked 3m ago, up to date" on a read-only server | #964: [the rule](../../crates/barca-core/src/health.rs) ("a node the read path cannot decide takes the verdict of the tick that checked it"), [the batched read](../../crates/barca-db/src/db/ticks.rs), [CLI, `GET /state`, `GET /nodes`, `barca.nodes`, a read-only server and a live tick](../../python/tests/test_last_check.py), [the node panel's line](../../ui/src/components/assets/LastCheckLine.test.tsx) |
| A stale `Manual` node above `Always` assets, with and without a cutoff asset in between, and a pinned asset: `barca status`, `barca get --dry-run` and a tick's recorded outcome say the same of every node below it | #985: [status against the dry run, a run and live ticks](../../python/tests/test_status_run_rules.py) |
| `python/tests/test_docs_examples.py` runs the gap, rollup and top-roots queries | #965 |
| A query naming a result view is refused with a usage error; a 6-second query answers `408` | #972 |
| One run slot and overlapping schedules: status names the queue as the cause, with the wait | #973 |

Sources the rules are ported from, until each is deleted by its issue (node health and the
data signals are deleted: their rules are `crates/barca-core/src/health.rs`; the health tree,
the causes list's grouping and the failures list's impact are deleted: their rules are
`crates/barca-core/src/rollup.rs`):
[tick targets](../../crates/barca-core/src/freshness.rs),
[status](../../crates/barca-core/src/status.rs), [tables](../../crates/barca-core/src/sql/tables.rs).

## Open questions and gaps

- Row 6 reads a provisional late (`health::schedule_late`): the grace alone, without the
  typical queue wait, evaluated wherever status is classified. On a machine without a server
  that is this machine's clock and time zone against the last tick the metadata database
  holds, so a schedule no scheduler is firing reads `behind`, `schedule_late`. #958 replaces
  it with the one definition.
- The web UI no longer attributes a blocked node to its failing node or sums health counts and
  data coverage: it reads `root` and the rollup. What it still derives: a group row's "Last
  success" (its oldest member's, and how many members are failing, from the rows of
  `GET /state`), which `stale_since` and the times (#962) replace; a file's one state in the
  sidebar and the asset list's filter counts, which read rows, not the rollup; and a node
  panel's count of nodes downstream, a graph walk for navigation. A node declared in several
  groups is listed in each of them by the UI but counted in one by the rollup, so such a
  group's row lists more members than its bar counts.

- A node below a stale paused node (not behind a cutoff asset) reads `stale`,
  `upstream_stale`, and health `behind`, while every tick finds it up to date: a tick reads
  a paused node as it is and a request recomputes it, and status says what a request would
  do, as `barca get --dry-run` does. "Last check" does not cover it. Whether such a node
  counts against the freshness gap while the pause lasts is left to the paused root (#961).
- "Last check" compares the node's own definition and the times of rows above it. An edit
  above the node, and a declared environment variable's new value, are not seen until the next
  tick or the expiry.
- Which nodes a tick reaches when a polled sensor changes below-`Always` assets (#952) is
  unresolved; this document takes the walk's answer and adds no rule of its own.
- Where the time of a code or environment change is recorded for `stale_since` is #962's
  decision; until then such nodes may have `stale_since: null` and contribute no backward term.
- Assets paused with `barca pause` and pinned assets keep the health their cache state gives
  and have no refresh path (#961); whether #963 gives them a root kind of their own, instead
  of `no_schedule`, is its decision.
- `refresh_path` does not see a schedule that is late or whose server is down from a
  `--read-only` server: `live` there is that process's scheduler, which it does not have.
- The estimate ignores worker contention and heavy-step capacity; its error is unmeasured.
- A node declared in several groups is placed by group id (#955 keeps the rule: it is the one
  that keeps every level an exact sum). Whether overlapping groups should instead be refused
  at parse time is open.
- The numbers fixed here (20 ticks for typical values, 1,000 ticks and 14 days of retention, a
  5-second query, 1,000 and 10,000 rows) are defaults chosen without measurement; an issue that
  measures otherwise revises this document in its PR.
