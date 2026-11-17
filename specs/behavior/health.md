# Health, causes and the freshness gap

## Scope and status

Specification version: 1. Status: approved intent, implemented in part. The 0.44.0 series
([#974](https://github.com/barca-orc/barca/issues/974)) lands it issue by issue; a field, enum
value, route, flag or table named here exists only once the issue that owns it has landed.
Implemented: "Health from existing state" (#954); of "Node fields and where they appear",
`health`, `health_reason`, `signals`, `measured` and `data_coverage` (#957), experimental in
the CLI contract; the tick record ("What a tick records", #960), as the `schedule_ticks`
table and its reads; `last_check` with the `checked_up_to_date` reason ("Last check", #964);
the rollup ("Rollup", #955): `GET /state/rollup` and `barca status --rollup`, with the
counts in both units and `keys`; `refresh_path` with its `barca.nodes` columns
("Refresh path", #961); the times ("Times", #962): `stale_since`, `expected_fresh_at` and
`recheck_at` with their `barca.nodes` columns, `unbounded` and `fresh_by` of a rollup entry,
`estimated_start_at` and `wait_seconds` of a queue entry, and `typical_queue_wait_seconds`,
`typical_run_seconds` and `queue_bound` of a schedule, as that section's "As implemented"
states; and the cause, the root and the gap ("Cause", "Root", "The freshness gap"; #963,
#973): `cause`, `root` with every kind, `gap_seconds` and their `barca.nodes` columns,
`causes`, `asset_causes` and `gap_seconds` of a rollup entry, the root list with `causes`,
`gap_seconds`, `unbounded` and `action`, and `as_of` and `rollup` in `barca status --json`,
as those sections' "As implemented" state; `no_estimate` ("Times", #968) and the web UI's
estimates over the served times ("The web UI's estimates", #968); the weight ("Distance from fresh", #1028): `weight`
and `weights` of a rollup entry, `weight` of a root and of a `held_by` entry, and the root
list ranked by it; the server query route ("Server query
route", #972); a scope's own root list (`not_fresh` and `held_by` of a rollup entry) with
the web UI's health map and to-do list over it ("Rollup", "The web UI's overview"; #966,
#967); and the tables ("Tables", #965): `barca.schedules`, `barca.ticks` and
`barca.queue`, as that section's "As implemented" states. Everything else below is intent.
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
  waiting for a run slot is not late; and the waits are the tick record's
  `queue_wait_seconds` (#962: `db::tick_timings`, one read for every schedule), or, for a
  schedule whose ticks recorded none, the `runs` rows it started (slot to start). Conformance: `a_queued_tick_reads_the_same_on_every_surface`
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

Implemented in `crates/barca-core/src/causes.rs` (`attribute`), called by `health::classify`.
Every node that is not fresh gets exactly one cause: the first matching row.

| # | Condition | `cause` |
| --- | --- | --- |
| 1 | `health` is `failing`; or `blocked` with a `blocked_by` node; or its refresh path's schedule is `failing` (past `max_failures`) | `failing` |
| 2 | `health` is `undecided` | `undecided` |
| 3 | a running run plans the node | `catching_up` |
| 4 | `refresh_path.missed_ticks >= 1` | `overdue` |
| 5 | a run that plans the node waits in the run queue (`run_queue` state `queued`, run status `pending`); or a tick of its refresh path's schedule will come (row 6) and that schedule is queue-bound or its newest tick's run is `pending` | `queued` |
| 6 | no tick of a refresh path will come: `refresh_path` is `null`, or its schedule does not fire (below) | `no_refresh_path` |
| 7 | otherwise | `catching_up` |

A schedule is **queue-bound** when its typical queue wait exceeds its interval.

The order where rows overlap, and why: a failure outranks everything (nothing else clears the
node); `undecided` comes before the rows that promise something, because barca cannot say the
node needs a run at all; a running run (3) outranks a miss (4), since the miss is being made
good now; a miss (4) outranks the queue (5) and the missing path (6), because a tick already
reached the node and running it by hand is the most direct action; the queue (5) is read only
for a schedule that will fire, so a paused or unfired schedule is `no_refresh_path`, not
`queued`.

As implemented:

- **A run plans a node** as in "Times": a tick's run its schedule's targets (never a paused
  or pinned node, which has no path), another run its targets and what is above them, a run of
  everything every node. The runs in flight are the server's; `barca status` on a machine
  without a server and a `--read-only` server know none, and read a waiting tick from the
  schedule's standing (`last_status` is `pending`).
- **Whether a schedule fires.** It does when it has a `next_tick` (this process's scheduler
  fires it, or it is paused until a time). Otherwise, when it is not paused and not held as
  `failing`: when a scheduler is seen firing and the schedule is not `late`. A scheduler is
  **seen firing** when any schedule is `live`, or the tick record shows it: a schedule that is
  not paused has a fired tick and is not late (its newest slot fired, within the grace and the
  queue wait). So `refresh_path.live` stays what the answering process fires, and the cause
  does not call every schedule unfired merely because a `--read-only` server, or `barca
  status` beside a server, runs no scheduler. Where that proof is missing (no tick record, or
  the newest tick is far behind its slots) the cause is `no_refresh_path`. `expected_fresh_at`
  is still given only by a process that runs the scheduler.
- `blocked` by the cache state alone (`cache_blocked`, no `blocked_by` node) is not row 1: it
  takes the first of rows 2 to 7 that applies.

Conformance: `the_first_matching_row_of_the_cause_table_decides` and
`the_tick_record_says_whether_a_scheduler_is_firing_where_none_runs_here`
(`crates/barca-core/src/causes.rs`).

### Root (#963)

Implemented in `crates/barca-core/src/causes.rs` (`attribute`, `origin`). Every node with a
cause has one `root`: the upstream fact that explains it.

```json
{"id": "sensor_value:pipeline.py:inbox", "kind": "sensor_value", "node": "pipeline.py:inbox"}
```

| `kind` | Cause it explains | `node` | Clearing `action.kind` |
| --- | --- | --- | --- |
| `sensor_value` | catching up, overdue, undecided, queued by a requested run | the sensor whose observation changed | `wait`, or `run` when a node it holds is overdue |
| `code` | same | the node whose code changed | same |
| `env` | same | the node whose declared environment variable changed | same |
| `input` | same | the fresh node whose result is newer than what a node below it read; or the topmost node that is out of date with no evidence of why | same |
| `never_run` | same | the topmost never-run node of the chain (or a node with keys that never ran, `partial`) | same |
| `undecided` | undecided | the topmost undecided node of the chain, with nothing above it that explains it | `wait` |
| `no_schedule` | no refresh path; undecided where no tick comes | the topmost not-fresh node no tick reaches | `add_schedule`; `run` for a `Manual` or pinned node |
| `schedule_paused` | same | the paused scheduled node; or a paused node (`barca pause`) | `resume_schedule` |
| `scheduler_off` | same | the scheduled node, when no scheduler fires it | `start_server` (`wait` when its nodes are catching up) |
| `failure` | failing | the failing node (`blocked_by`, or itself); or the scheduled node of a schedule held past `max_failures` | `fix_failure` |
| `queue` | queued | the scheduled node whose ticks wait for a run slot | `raise_max_runs` when queue-bound, else `wait` |

The root of a node, by its cause:

- **`failing`:** the failure root `health::classify` names (itself, or `blocked_by`). A node
  whose only failure is its path's held schedule roots at `failure:<scheduled node>`.
- **`queued`:** `queue:<schedule>` when the schedule's wait is why (its path's schedule is
  queue-bound or has a waiting tick, or a waiting tick's run plans the node): a `queue` root
  wins over a change root. A node queued only by a requested run keeps its change root.
- **`no_refresh_path`,** and **`undecided` where no tick of its path will come** (only a run
  decides the node, so what holds it is the missing tick, not the change above it): with a
  path, `schedule_paused:<schedule>` or `scheduler_off:<schedule>`; without one, a paused
  node is its own root (`schedule_paused:<node>`), and any other node takes the root of its
  first input that has no path and is rooted `no_schedule` or `schedule_paused`, else
  `no_schedule:<node>`. So the root is the topmost node of the chain that nothing refreshes.
- **Everything else:** the walk. Attribution walks upstream from the node through inputs that
  are not fresh and stops at the first node whose own evidence explains it, read from
  `cache.changed` (what changed in its run hash since its last successful result): its own
  definition first (`code`, a decorator, a helper that is not a node's function: `code`), then
  `env NAME` (`env`), then what it reads (`sensor ID`, `input ID`, and `helper ID` naming a
  node in scope, which is how a reader's hash records an upstream function): that input is
  the root when it is fresh (`sensor_value` for a sensor, `input` for anything else), and
  passes the question to its own root when it is not. A `behind_input` signal names its input
  the same way. A node with none of that takes the root of its first not-fresh input in
  dependency order. A chain with no evidence ends at its topmost node: `never_run`
  (`partial` too), `undecided`, `scheduler_off` for the scheduled node of a late schedule,
  else `input`. A failure is never inherited by a node that is not blocked by it.

**Paused nodes and pinned assets** (decided here, as "Open questions" asked): a paused node
has no path and is skipped by every tick, so it is its own root, kind `schedule_paused`,
cleared by `barca resume`; a pinned asset is read as its pinned result, so it roots at
`no_schedule:<itself>`, cleared by `barca unpin`. A stale `Manual` node roots at
`no_schedule:<itself>`: no tick computes it, and its action is a run naming it in
`--refresh`. What is below any of them and has a path is decided against what they hold
(#985) and attributed on its own.

"Topmost" and every tie break by dependency order (the order of the status rows), then node
id, so attribution is deterministic.

The list of roots (in the rollup, below) carries what each holds and what clears it:

```json
{"id": "no_schedule:pipeline.py:orders", "kind": "no_schedule", "node": "pipeline.py:orders",
 "detail": "no tick reaches orders", "error_group": null,
 "nodes": 152, "assets": 140,
 "causes": {"catching_up": 0, "queued": 0, "no_refresh_path": 152, "overdue": 0, "failing": 0, "undecided": 0},
 "gap_seconds": 8208000, "unbounded": 152, "weight": 912,
 "action": {"kind": "add_schedule", "command": null, "estimated_seconds": 420}}
```

As implemented (`crates/barca-core/src/rollup.rs`, `roots`):

- `action.command` is a runnable `barca` command when one clears the root, else `null`:

  | `action.kind` | When | `command` |
  | --- | --- | --- |
  | `wait` | a change root with no overdue node; an `undecided` root; a `queue` root that is not queue-bound; a `scheduler_off` root whose nodes are not `no_refresh_path` | `null` |
  | `run` | a change root holding an overdue node | `barca get <ends>`: the held nodes no other held node reads, by name, comma-separated (`barca run` when they are all tasks; tasks are left out otherwise) |
  | `run` | a `Manual` `no_schedule` root | the same with `--refresh <root node>`: a run reads a `Manual` node as it is unless it is named there, and `--refresh` recomputes what is below it |
  | `run` | a pinned `no_schedule` root | `barca unpin <node>` |
  | `add_schedule` | any other `no_schedule` root | `null` |
  | `resume_schedule` | a paused node | `barca resume <node> --server` |
  | `resume_schedule` | a paused schedule | `barca resume --schedule <scheduled node> --server` |
  | `start_server` | a `scheduler_off` root holding `no_refresh_path` nodes | `barca serve` |
  | `fix_failure` | a `failure` root | `barca get <node>` (`barca run` for a task): the retry, once the error is fixed |
  | `raise_max_runs` | a queue-bound `queue` root | `null`; `detail` names the wait, the interval and `max_runs` |

  A node is named by its declared name, or by its id when two nodes in scope share the name.
  A command ends in `--server` when a server answers (`GET /state/rollup`).
- `action.estimated_seconds` is the typical compute time of the nodes held: the sum of their
  typical step durations (the duration rule of "Times": the median of a node's recent
  successful runs, once per key without a result; the project's median for a node without
  samples), whole seconds, `null` when no held node has a sample. It is work, not wall time.
- `detail` is the fact in a line: a failure's error headline; `inbox observed a new value`;
  `the code of orders changed`; `no tick reaches ledger`; `reviewed is Manual: no tick
  computes it`; `inbox's schedule is paused`; `no scheduler has fired inbox's schedule for
  57m`; `pulse's ticks wait about 2m for a run slot, every 1m: max_runs is 1` (#973).
  `max_runs` is the server's own; `barca status` without a server names the number only when
  `barca.toml` sets `[server] max_runs`.
- `error_group` is a failure root's error headline with its numbers replaced by `#`; failure
  roots sharing one are shown as one entry. `null` for every other kind.
- Ranking (#1028): by `weight` held, the most first; among equals a failure first, then
  `nodes`, `gap_seconds`, `id` ("Distance from fresh"). This replaces #963's order (roots
  holding a node with no estimate first, by nodes), under which a `never_run` root that only
  waits outranked a change holding four times as many nodes and a failure that blocks 14
  nodes fell below the first three.

Conformance: `a_root_is_the_change_the_walk_up_stops_at`,
`a_node_nothing_refreshes_roots_where_a_schedule_is_missing_paused_or_not_fired`,
`ticks_that_missed_what_a_root_holds_make_its_action_a_run` and
`on_the_audit_shape_three_roots_hold_nine_in_ten_of_what_is_not_fresh`
(`crates/barca-core/src/causes.rs`); `python/tests/test_causes.py` runs the commands of a
`run` and a `resume_schedule` action and sees their roots clear.

### Ticks waiting on a run slot (#973)

Nothing new is recorded: the tick record's `queue_wait_seconds` is the evidence. A node on a
refresh path whose schedule will fire and is queue-bound, or whose newest tick's run waits for
a run slot now, has cause `queued` and root `queue:<scheduled node>`; the root's `detail`
states the typical wait, the interval and `max_runs`, and its action is `raise_max_runs`
(more run slots, or schedules spaced so their runs do not overlap) for a queue-bound schedule
and `wait` for one that is only waiting now. What a run costs outside its steps is measured
in [#951](https://github.com/barca-orc/barca/issues/951) and not repeated here. Conformance:
`ticks_that_wait_longer_than_they_are_apart_name_the_queue`
(`crates/barca-core/src/causes.rs`) and, on a real server with one run slot and two
overlapping schedules, `test_ticks_waiting_on_one_run_slot_name_the_queue_as_the_cause`
(`python/tests/test_causes.py`), which also reads the same cause from a `--read-only` server.

### Times (#962, #964)

| Field | Meaning | `null` when |
| --- | --- | --- |
| `stale_since` | the earliest recorded time from which the node has been continuously not fresh | fresh; undecided; or no record holds the time |
| `expected_fresh_at` | when the node is expected to be fresh | fresh; or cause is not `catching_up` or `queued`; or the path has an undecided node |
| `recheck_at` | when a tick next evaluates the node | no refresh path, or its `next_tick` is `null` |
| `last_check` | `{"at", "outcome", "schedule"}`: the newest finished tick that had the node as a target (implemented, "Last check") | no retained tick did |
| `no_estimate` | why a node that is not fresh has no `expected_fresh_at`, in one word (#968, below) | fresh; or it has an estimate |

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

As implemented (#962), in `crates/barca-core/src/estimates.rs` (`times`, `queue_starts`),
called by `health::classify` once per status result:

- **`stale_since`**, a node's own time by what made it not fresh: a failure is
  `failing_since` (the first failed attempt since its last clean run); `inputs_changed` is
  the earliest `created_at` among its inputs' newest successful results that are newer than
  its own (a sensor's observation or an asset's result alike); `behind_input` is that input's
  newer result; the scheduled node of a late schedule is `late_since`; `code_changed` and
  never-run are the source file's modification time, when that is after the node's newest
  successful result. A node that is not fresh because of what is above it (`upstream_stale`,
  blocked) takes the earliest of its own time and those of its not-fresh inputs, so the time
  is the root's without naming the root (#963 names it). `null`: fresh, undecided,
  `env_changed` alone, `partial`, `unsynced`, and a code change no file time dates.
- **Which record holds a code change.** None does: no table records when a definition hash
  was first seen, and recording it would add a write to a read path (status) or a read to the
  tick path. The file's modification time is read once per loaded graph (the server reloads
  the graph on an edit; once per CLI command). Its error is one-sided: it is never before the
  change, and an unrelated later edit to the same file moves it forward, so `behind_seconds`
  is a lower bound. A changed helper in another file leaves the node's own file untouched
  and the time `null`. For `inputs_changed`, an input that changed twice since the node's
  result dates the staleness from its newest result, also a lower bound.
- **`recheck_at`** is `refresh_path.next_tick`, for every node whose path has one, fresh or
  not: the slot itself, exact, without the queue wait. A node that also has
  `expected_fresh_at` is said to be fresh then; one with `recheck_at` alone, rechecked.
- **`expected_fresh_at = start + place`**, rounded up to 5 seconds and never before `as_of`.
  `start` is, first that applies: for a running run that computes the node, its latest
  progress (when it started executing or its last step finished, not `as_of`: an unchanged
  poll then answers the same time); for a waiting run, the queue entry's
  `estimated_start_at`; else `next_tick` plus the schedule's typical queue wait, or when the
  run queue clears if later. A tick's run computes its schedule's targets; another run its
  targets and what is above them. `place` models workers, unlike the intent above: steps are
  taken in the order they become ready on the longest chain of typical durations, and each
  starts when its inputs are done and the work taken before it, divided over the worker
  pool, is done. With enough workers that is the longest chain; with one, the sum. Nodes that
  run on every tick (`always_runs`) count as work. A partitioned node counts its typical
  duration once per key without a result.
- **Only `recheck_at`** (INV-HEALTH-05) for a node that is failing or blocked; `undecided`
  or whose cache state is `unknown` (below a cutoff asset that would run, a sensor with no
  recorded output: the run may find it up to date or leave it); below a not-fresh node the
  run does not compute (a stale `Manual`, paused or pinned node, read as it is); missed by a
  tick (`missed_ticks >= 1`), unless a running run computes it; and while the run queue is
  paused or draining. Without a live scheduler `next_tick` is `null` and so are both times.
- **`no_estimate`** (#968; `health::no_estimate`, after the cause and the root are known):
  the first of `failing` (cause `failing`); `paused`, `scheduler_off`, `no_path` (the root is
  `schedule_paused`, `scheduler_off`, `no_schedule`; or the cause is `no_refresh_path`);
  `undecided` (cause `undecided`); `missed` (cause `overdue`); then, for a `queued` or
  `catching_up` node, `queue_stopped` (the run queue is paused or draining),
  `no_scheduler_here` (no `recheck_at`: the answering process fires no scheduler) and
  `not_promised` (a tick will look, and a result above may come out unchanged or a node
  above is read as it is). Every node that is not fresh has an estimate or a reason, never
  neither, so no surface shows a blank. Conformance: `a_node_with_no_estimate_says_why`
  (`crates/barca-core/src/causes.rs`).
- **Queue entries** (`GET /queue`): each of `max_runs` slots frees when its run ends (its
  start plus its typical run time, never before `as_of`), and a waiting run takes the slot
  that frees first, in queue order. A run's typical time is its schedule's
  `typical_run_seconds`; for a run no schedule started, or a schedule with no sample, the
  median of the schedules that have one (0 with none). The extra slot a requested run may
  take past scheduled work is not modelled. `wait_seconds` is from `queued_at` to
  `started_at` (a running run) or to `estimated_start_at` (a waiting one), not to `as_of`,
  so it does not move with the clock. A `--read-only` server gives neither.
- **Schedules** (`GET /schedule` and the `schedule` block): `typical_queue_wait_seconds` (the
  median `queue_wait_seconds` of the newest 20 fired ticks, 0 with none), `typical_run_seconds`
  (the median start-to-end of the newest 20 whose run succeeded, `null` with none), both
  whole seconds, and `queue_bound` (the typical wait exceeds the interval).
- **Rollup entries** carry `fresh_by` (the latest `expected_fresh_at` underneath, `null` when
  none) and `unbounded` (not-fresh nodes underneath with no `expected_fresh_at`).
- **Entry points for the gap (#963):** `estimates::behind_seconds(node, now)` and
  `estimates::ahead_seconds(node, now)` read the two times off a classified row.
- **Clock.** No estimate is a function of `as_of` except through its floor (an estimate that
  has passed reads `as_of`, rounded up to 5 seconds), so a poll that finds nothing changed
  and nothing overdue answers the same body.
- **Cost.** No read is added per node or per poll: typical durations and the schedules'
  recorded ticks are the reads `GET /state` already keeps per database state, the runs in
  flight are the server's own, and the estimate is one pass over the rows per schedule or
  run that has a not-fresh node.
- **Measured error.** `python/tests/test_estimates.py` runs a real server with one run slot
  and two schedules ticking every 10 seconds (steps of 0.4 to 3 seconds), edits a node's
  code three times and compares each estimate, when first given, with when the thing
  happened. `expected_fresh_at` was 1.8 to 3.9 seconds later than the time the node became
  fresh (12 estimates a run), and `estimated_start_at` 4.0 seconds later than the start: the
  rounding up to 5 seconds and whole-second typical times, with a run's startup (not
  modelled) pulling the other way. The test asserts 8 seconds and 6 seconds. On longer
  steps the unmodelled parts (startup, retries, capacity weights, other runs sharing the
  server's capacity) are not bounded by this measurement.

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

As implemented (#963; `causes::gap_seconds`): `gap_seconds` is on every node, a whole number
of seconds, read at the start of `as_of`'s minute on every surface, so an answer does not
change with every second of the clock (an unchanged poll within a minute is still a `304`).
A term whose time is `null` counts 0, so a node with no recorded `stale_since` and no
estimate has gap 0 and is counted by `unbounded`.

### Distance from fresh (#1028)

One served measure of how far a scope is from a fresh DAG, in weighted nodes, computed in
`crates/barca-core/src/rollup.rs` (`Cause::weight`, `FRESH_WEIGHT`, `Weights`) and read by the
root list, the health map and `barca status --rollup` alike.

| A node that is | Weighs | Why |
| --- | --- | --- |
| fresh (`ok`, `suspect`: no cause) | 1 | every node counts, so a fully fresh scope still has a size: its `nodes` |
| `queued`, `catching_up`, `undecided` | 2 | it will be fresh without anyone acting, or barca cannot say |
| `failing`, `overdue`, `no_refresh_path` | 6 | someone has to act |

- A rollup entry carries `weight`, the sum over the nodes underneath, and `weights`, the same
  by part (`failing`, `overdue`, `no_refresh_path`, `queued`, `undecided`, `catching_up`,
  `fresh`), which add up to it. Both are linear in the entry's `nodes` and `causes`, so a
  level's is the exact sum of its children's (INV-HEALTH-03).
- A root and a `held_by` entry carry `weight`: the weight of the nodes held (never a fresh
  node). In a scope, the `held_by` weights and one per fresh node add up to the entry's
  `weight`.
- **Ranking.** A root list (`roots`, and every entry's `held_by` over what is held there) is
  ordered by `weight`, the most first; among equals a failure root first, then the most
  `nodes`, the largest `gap_seconds`, then `id`. So a root holding nodes that need someone
  outranks a larger one a tick will clear (152 with no refresh path, 912, before 180 catching
  up or undecided, 360), and a failing node with what it blocks is ranked by all it holds.
- It is not `gap_seconds`: one node out of date for a year would outweigh everything, and a
  node with no recorded `stale_since` and no estimate (gap 0, counted by `unbounded`) would
  weigh nothing. The gap is stated beside it.
- `barca status --rollup` prints `Distance from fresh: 1728 (weighted nodes); 698 when every
  node is fresh` under the gap line and `weight N` on each root.

The three weights are chosen, not measured: 6 to 2 makes one node that needs someone count as
three that do not. Conformance: `roots_rank_by_weight_then_failures_first` and INV-HEALTH-03
with the weight on every fixture (`crates/barca-core/src/rollup.rs`);
`roots_rank_by_weight_and_a_failure_with_blocked_nodes_is_among_the_first_three` and the audit
shape (`crates/barca-core/src/causes.rs`).

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
| `no_estimate` | string enum or `null` | yes | `no_estimate` |
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
`no_refresh_path` with root kind `scheduler_off`, unless the tick record shows a scheduler
firing ("Cause"). As implemented (#963): `rollup` is `rollup::total`, the same sums as the
root entry of `GET /state/rollup` for the scope with `children` empty (the tree is the
rollup's own document), over every node in scope whatever a filter or a limit cuts from
`nodes`; `GET /nodes` carries both too.

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
- `also_in` lists the other groups that declare an entry, by id: it is counted under `parent`
  alone, and a surface listing a group's members lists the members counted under it, so a list
  and its count are the same nodes (#959).
- `files` (beside `entries`) sums the same nodes by source file, `{file, nodes, assets,
  running, health, asset_health, worst}` per file in scope: the tree has a `file` entry only
  for the nodes in no group, and a surface that lists files reads these instead of adding
  entries up (#959).
- `roots` is the root list above, scoped to the nodes in the response.

As implemented (#955), in `crates/barca-core/src/rollup.rs` (`build`), which the route, the
CLI and the web UI all read:

- Served on an entry: `id`, `kind`, `label`, `parent` (`null` on the root), `children`,
  `nodes`, `assets`, `running`, `health`, `asset_health`, and `keys` on a partitioned node.
  An entry also carries `data_coverage` (`{"assets", "measured", "suspect"}` for the assets
  underneath), which this section did not name: it is what lets a surface state the coverage
  of a group without adding it up, and it sums like every other count. `unbounded` and
  `fresh_by` (#962), `causes`, `asset_causes` and `gap_seconds` (#963) are read off each row
  and summed in the one place a level adds up its children (`Sums::add`).
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
- `root` on a node (`NodeStatus.root`) and `roots` are those of "Root": every node that is
  not fresh carries a root and is under exactly one entry of `roots` (INV-HEALTH-04), so the
  roots' `nodes`, `gap_seconds` and `unbounded` add up to the root entry's not-fresh nodes,
  gap and unbounded nodes. A failure root's `detail` is the error headline as recorded (its
  first line, or the last line of a bare traceback; `N of M keys failed` for a node whose
  keys failed with no error of its own).
- The scope is that of `GET /state`: targets stand for their upstream cones, so the rollup of
  one group or file also counts what it reads from outside it.
- The server sums the status result `GET /state` is built from (no status is computed for the
  rollup) and classifies per answer. Its `as_of` is the start of the current minute, the
  instant lateness is read at, so an unchanged poll within a minute answers `304`; the CLI's
  is the instant of the command.
- `barca status --json` carries `as_of` and `rollup`, the root entry's sums ("Node fields and
  where they appear"); the tree and the roots are the rollup's own document.
- `barca status --rollup` prints, under the tree, the gap line and the ranked roots, each
  with what it holds, why, and the action with its command and compute time.
- **A scope's roots (#967).** An entry carries `not_fresh` (the nodes underneath with a
  cause: `causes` as one number) and `held_by`: the roots that hold those nodes, each as
  `{root, nodes, assets, causes, gap_seconds, unbounded, weight, cumulative_nodes}` over the nodes
  underneath that entry alone. It is the root list cut to the scope: a root whose own node
  is in another group is listed where it holds a node, with what it holds there. The order
  is the root list's rule applied to what is held in the scope (`rollup::rank`: the most
  weight first, a failure first among equals; then nodes, gap, id), so the root entry's
  `held_by` is `roots` in order. `cumulative_nodes` is the running total of `nodes` down the list:
  against `not_fresh` it says how much of the scope the first roots explain, and `barca
  status --rollup` prints it for the first three ("The first 3 hold 392 of them."). A node
  entry's `held_by` is empty (its root is on its row); a level's `held_by` holds exactly
  what its children's do (INV-HEALTH-03 covers it), and its `nodes`, `gap_seconds` and
  `unbounded` add up to the entry's `not_fresh`, `gap_seconds` and `unbounded`
  (INV-HEALTH-04, per scope). `barca status --json` carries both on `rollup`.

### The web UI's overview (#966, #967)

Presentation over the rollup, stated here because its sizes and colours are the one place a
surface turns served counts into geometry. Implemented in `ui/src/lib/causeDisplay.ts` (the
display table), `ui/src/lib/rollupView.ts` (the layout) and `ui/src/lib/causesView.ts` (the
list's words).

- **Unit.** The map, its centre, the line beside it and the list count nodes, and say so;
  assets are stated beside them from the same entry (`assets`, a root's `assets`).
- **Size.** A segment's angle is its entry's served `weight` ("Distance from fresh", #1028),
  so it is exactly the sum of the children's, grows with every node that falls behind and
  with the severity of its cause, and leaves a fully fresh group on the map at one unit a
  node. The UI holds no weight table: it divides one served number by another to get an
  angle. The gap is stated in words beside the map and on each root.
- **Colour.** A segment is drawn in the colours of what is inside it, each as wide as its
  part of the entry's served `weights`, in the served order of the causes, then fresh: never in the colour of its worst member. Each
  cause has one colour; `failing` keeps its own, `overdue` and `no_refresh_path` are the two
  alarm colours, `queued` and `catching_up` are calm, `undecided` is the one grey, and a
  node with no cause (`ok` and `suspect`) is fresh. `suspect` is a data warning and is
  listed under data signals, not coloured on the map.
- **Centre.** The scope's `causes` in the served order, in nodes; under the map, `nodes`
  and `not_fresh`, "fresh within about 5m" from `fresh_by` against `as_of` (rounded up to
  the unit shown) and "N with no estimate" from `unbounded`.
- **The to-do list** is the open scope's `held_by` joined with `roots` by id, in the served
  order: the root's kind and node, `detail`, what it holds there (`nodes`, `assets`,
  `causes`), its `gap_seconds` or how many have no estimate, and `action` with its command
  to copy and its `estimated_seconds`. Failure roots sharing an `error_group` are one entry
  that opens to the others. "N more" opens the whole list in place. Picking a root lights
  the entries whose `held_by` names it and the rows whose `root.id` is its id, on the map
  and in the table; its node's name opens the node. Data signals (`empty`, `rows_changed`,
  `behind_input`, a late schedule) are a section of their own under the list.

### The web UI's estimates (#968)

Presentation over the served times, stated here because "fresh" and "rechecked" are different
promises and a blank is neither. Implemented in `ui/src/lib/estimates.ts` (the words),
`ui/src/lib/causeDisplay.ts` (the display tables) and `ui/src/lib/time.ts` (the rounding).

- **A node** (a table row, the panel): `fresh ~15:05` from `expected_fresh_at`, to the minute
  and rounded up; else, where a tick's look can settle it (`no_estimate` is `not_promised` or
  `undecided`), `rechecked 15:00` from `recheck_at`, exact; else `no estimate:` and the word
  of `no_estimate`, with the recheck in the tooltip. A fresh node says when it is next
  rechecked, or when it was last built. The panel also shows `stale_since`, `refresh_path`
  with `missed_ticks`, `last_check`, the `cause` in the map's word and colour, and the `root`
  with a link to its node, its `detail` and its `action` from the root list.
- **A group** (its row): `fresh` when `not_fresh` is 0; `all fresh by ~15:02` from `fresh_by`
  when `unbounded` is 0; `152 with no estimate` when `fresh_by` is `null`; both otherwise.
  The State column's bar is the entry's `weights` in the map's colours, its words the entry's
  `causes` in the served order; rows are ordered by `weight`.
- **A schedule**: late only by `late`; `queue-bound` by `queue_bound`, with what to do;
  `typical_queue_wait_seconds` and `typical_run_seconds` as `~15s`; late and queue-bound
  rows first.
- **A queue entry**: `starts in ~40s` from `estimated_start_at`; `no estimate` when `null`.
- An estimated length is rounded up to 5 seconds, whole minutes, hours or days (`<5s` below
  5 seconds) and marked `~`; no surface shows a second it does not know.

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

As implemented (#965), in `crates/barca-core/src/sql/tables.rs` (the columns and the row
builders, which `barca sql` on a machine and the server's query route both call):

- **`barca.schedules`** is the `schedule` block of each scheduled node's status
  (`ScheduleStatus`, what `barca status --json` and `GET /nodes` print), with the node's `id`,
  `name` and `kind`; `interval_seconds` is `schedule_standing::interval_seconds`, the distance
  between the slots around now that `standing` uses; `targets` is the scheduler's own target
  list, the scheduled node first (`ScheduledJob::targets` on a machine, the walk kept per
  graph on a server). On a machine without a server the block is what the metadata database
  records and `live` is false; on a server it is the scheduler's. It is loaded again with the
  graph and at least every 30 seconds, like `barca.nodes`.
- **`barca.ticks`** is `schedule_ticks` read once (`db::recent_ticks`), every schedule's
  ticks, newest slot first; a schedule no longer in the graph keeps its rows until they are
  trimmed. `scheduled_at` is the slot (RFC 3339, UTC), `started_at` and `finished_at` the
  run's times with milliseconds, the counts `null` on a tick that did not fire or has not
  ended. A machine no server scheduled has none.
- **`barca.queue`** is the entries of `GET /queue`, started runs first, then the waiting
  ones in order, with `state` as `run_queue` records it and `scheduled_at` the slot of a
  tick's run. It is the server's state, so it is handed over for every query that names it.
  A `--read-only` server lists the recorded queue, with `started_at`, `estimated_start_at`
  and `wait_seconds` `null`, as its `GET /queue` does. `barca sql` without a server has no
  queue: the table is there, empty, and stderr says why.
- **Times are text**, as the rule above says, in every table: a query casts one to compute
  with it (`now() - stale_since::timestamptz`).
- **With a server**, a query that names only `barca.*` tables is answered by the route; one
  that also names a result view runs on the client, which reads these three tables through
  the route (`select * from barca.<table>`, at most 10,000 rows) and leaves them empty, with
  a note, when the route does not answer.
- **The manual** (`barca docs sql`, "Health queries") ships eight queries, each run as
  written by `python/tests/test_docs_examples.py` on a machine and against a `--read-only`
  server: the gap as one number, the gap by source file, the top roots, the nodes with no
  refresh path, the overdue nodes with their missed ticks, the queue-bound schedules with
  their typical wait, the tick outcomes of one node over time, and the runs per hour by
  trigger. The rollup by group is not a query: `barca.nodes` has no group column, and the
  group tree is `barca status --rollup` (`GET /state/rollup`).

### Server query route (#972)

Implemented ([route and helper owner](../../crates/barca-server/src/sql_query.rs),
[helper](../../python/barca/_sql_server.py), [wire shape](../interfaces/server-api.openapi.yaml)).

`POST /sql/query`, body `{"query": "...", "limit": 1000}`, answers the document `barca sql
--json` prints (`columns`, `rows`, `total`, `truncated`, `hint`), with `notes` when a table was
cut at the row limit below.

- Only `barca.*` tables exist in the helper's database. A query naming a result view, any
  other relation, or more than one statement, or one that is not a `SELECT` (or `WITH ...
  SELECT`), is refused with `400` and the usage error envelope. So is a SQL error.
- `limit` defaults to 1,000 and is at most 10,000 (a larger one is lowered to it); `truncated`
  and `total` say what was cut.
- An answer holds at most 4MB of encoded rows. The helper fetches rows a few at a time and
  refuses (`400`) when the first `limit` rows pass that, before reading the rest; the server
  reads no line of the helper longer than 16MB and replaces a helper that sends one. DuckDB's
  memory limit bounds its own buffers only: this bounds what Python and the server hold.
- Four queries are admitted at once: one running, three waiting inside their own 5 seconds.
  One more is answered `429` with `Retry-After` at once, so no client can make every other
  wait.
- A query has 5 seconds, waiting for the helper and loading tables included; past it the answer
  is `408` (usage envelope) and the query is cancelled: the helper is told to interrupt it and,
  when it has not stopped a second later, is killed. A table whose load used the time up stays
  loaded, so the same query sent again has its whole time.
- The server's auth applies as on every route (project tokens when enabled). It is a read: a
  `--read-only` server serves it.
- One long-lived DuckDB helper per server, started on first use, started again when it is found
  dead, gone with the server (its stdin is the server's end of a pipe). Queries run one at a
  time. Never a process per request.
- The boundary, each part holding without the others: the helper's in-memory database contains
  the `barca` schema only; external access is disabled (no file, network or extension access)
  and the configuration locked, so no statement enables it; the statement must start as a
  query does (`SELECT`, `WITH`, `FROM`, `VALUES`, `TABLE`, `SHOW`, `DESCRIBE`, `SUMMARIZE`,
  `UNPIVOT`) and is then checked with DuckDB's parser to be one `SELECT` (the first check is
  what refuses `PRAGMA database_list` on DuckDB 1.x, whose parser reads a table-function
  pragma as a `SELECT`; the route behaves the same on every supported DuckDB, 1.4 to 2.x); it runs in a read-only transaction that is rolled back,
  which refuses a write a `SELECT` carries (DuckDB 2 accepts `WITH x AS (DELETE ... RETURNING
  *) SELECT ...`); the statement runs as it was sent, never inside another; memory is limited
  to 1GB, with nothing spilled to disk, and threads to two, and a query that needs more is a
  `400` that leaves the helper and its tables in place; the helper's working directory and
  `HOME` are an empty directory of its own (the project is not importable, DuckDB's secret and
  extension directories are unset) and the server's paths are removed from the error messages
  it answers; the helper inherits none of the
  server's environment but what an interpreter needs. Rows reach it as query parameters, never
  as files.
- Rust hands the helper the rows of a table when a query names the table and it is not loaded
  for the current state: the metadata database on disk, and for a table computed from the
  graph (`barca.nodes`) the inspected graph, the same evidence the read cache keeps status by.
  `barca.nodes` is the kept status of `GET /nodes`, handed over again at least every 30 seconds
  (health moves with the clock). A query on an unchanged server reads no database and computes
  no status. At most 100,000 rows of a table are loaded, newest first; `notes` says when a
  table has more. A query that calls `query(...)` or `query_table(...)`, which reach a table
  by a name the text does not show, gets every table. The helper has 60 seconds to take a
  table; one that has not is replaced.
- The route exposes the rows of `barca.*` tables to whoever may call it, the error text of
  failed steps and artifact paths among them, as the other read routes do.
- A table is one entry in the route's table list (`SOURCES`): its name, the columns `barca sql`
  gives it, whether it depends on the graph, and a function from the metadata snapshot (and
  graph) to rows. #965's tables are added there.
- `barca sql --server` sends a query that names only `barca.*` tables to this route. A query
  naming result views runs on the client as today, and so does one with files typed after it,
  with `--all` or a `--limit` above 10,000, with a configured server one naming `barca.nodes`
  (its `local` and `unsynced` are the client's), and any query when the route answers `404`,
  `405` or `503`. This narrows the manual's "the server never runs SQL" to: the server never
  runs SQL over results or files.
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
  add up to the not-fresh nodes in scope; so do their `gap_seconds` and `unbounded` to the
  scope's.

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
  `next_tick` is `null`. For the cause it reads whether a scheduler is firing from the same
  ticks ("Cause"): a schedule that fired its newest slot proves one is, so its nodes read
  `catching_up` or `queued`, not `no_refresh_path`; when the scheduling server stops, its
  schedules fall behind their slots within their grace and the cause turns to
  `no_refresh_path` with root `scheduler_off`.
- When ticks stop (the scheduler is down, the schedule paused or removed), a standing verdict
  expires within one interval plus the last tick's duration and grace, and the node reads
  `undecided` again; `last_check` keeps the record.
- The query helper is restarted when it dies, sends a line longer than 16MB or does not take a
  table within 60 seconds; the request in flight answers `503`. A query that exceeds DuckDB's
  memory limit is a `400` and the helper stays. Without DuckDB installed the route answers `503` naming the extra,
  on every request until it is installed; every other route is unaffected.

## Acceptance and evidence

Each issue lands its scenario with the code and replaces the matching source link. Passing
today (with the #961, #962, #963, #966, #967, #968, #973 and #1028 rows): #954 ([health rules](../../crates/barca-core/src/health.rs): the twelve rows, the walk
through `unknown` nodes and past pins, the signal rules), #957 (the same file's coverage
test, the [CLI contract snapshot](../../python/tests/snapshots/cli_contract/status.txt), and
the web UI's [coverage line](../../ui/src/lib/dataSignals.test.ts)), the #959 row, the #960
row, the #964 row, the #965 row, the #972 row and the #985 row. The rest do not pass yet.

| Scenario | Issue |
| --- | --- |
| The twelve health rows, `blocked_by` through `unknown` nodes and past pins, as Rust tests ported from the TypeScript ones | #954 |
| The map, the bars and `barca status --rollup` print the same counts from one response; asset and node units both present | #955: [rollup rules](../../crates/barca-core/src/rollup.rs) (INV-HEALTH-03 on every fixture, one parent per node, both units, the failure roots), [the route against `GET /state`](../../crates/barca-server/tests/openapi.rs), [no second status computation](../../crates/barca-server/src/read_cache.rs), [the CLI contract snapshot](../../python/tests/snapshots/cli_contract/status_rollup.txt), [the map's layout over the response](../../ui/src/lib/rollupView.test.ts), [the bar, the map and a group row against one response of a served project](../../ui/e2e-groups/groups.spec.ts) |
| A project with 7 of 556 assets counted reads "7 of 556 measured" in status JSON and the UI | #957 |
| A fixture with a queued tick: `GET /schedule`, `GET /state` and `barca status` agree on last tick, last run and late (passes: `a_queued_tick_reads_the_same_on_every_surface`) | #958 |
| Re-adding a derived word in TypeScript fails CI | #959: [the check](../../scripts/check-ui-layering.py) (a prek hook, so every tier; it seeds violations into a small tree on each run and fails if one passes), [its table](../../crates/barca-cli/ui-layering.json) (every route the UI calls with its CLI command, every module of `ui/src/lib` with its kind, the named exemptions), [the table against the CLI and the router](../../crates/barca-cli/src/ui_layering_tests.rs), [the fixtures' health words from `classify`](../../crates/barca-core/src/ui_fixture_tests.rs) |
| A fired, paused, skipped and failing tick each leave one row; the perf gate is unchanged | #960: [owner tests](../../crates/barca-db/src/db/ticks.rs), [scheduler tests](../../crates/barca-server/src/scheduler/tests.rs) ("the tick record"), [outcomes](../../crates/barca-core/src/serve/ticks.rs), [a served pipeline](../../python/tests/test_serve_schedule.py) |
| A scheduled sensor with `Always` assets below, an unscheduled chain and a `Manual` node: refresh paths equal the tick's targets | #952, #961: [the shapes, a paused schedule, no live scheduler, paused and pinned nodes, missed ticks](../../crates/barca-core/src/refresh_path.rs) ("membership is the scheduler's own target list"), [the batched misses](../../crates/barca-db/src/db/ticks.rs), [the walk once per graph](../../crates/barca-server/src/read_cache.rs), [CLI, `GET /state`, `GET /nodes`, `barca.nodes`, a scheduling and a read-only server](../../python/tests/test_refresh_path.py) |
| Estimates within the stated error on a real server (one run slot, two schedules); a node below a pending cutoff has `recheck_at` only | #962: [the rules](../../crates/barca-core/src/estimates.rs) (the start, the workers, what is only rechecked, `stale_since` by cause, the queue), [the tick timings in one read](../../crates/barca-db/src/db/ticks.rs), [measured on a server; CLI, `GET /state`, `GET /nodes`, `GET /state/rollup`, `barca.nodes`, a read-only server](../../python/tests/test_estimates.py), [the node panel's lines](../../ui/src/components/assets/EstimateLine.test.tsx) |
| On the audit fixture the top three roots hold at least 90% of not-fresh nodes; every not-fresh node is under one root | #963: [the cause rows, the walk, the roots where nothing refreshes, the audit shape (698 nodes, 422 not fresh, five roots, the top three hold 392)](../../crates/barca-core/src/causes.rs), [the root list, its actions and INV-HEALTH-03 with causes and the gap](../../crates/barca-core/src/rollup.rs), [the routes against each other](../../crates/barca-server/tests/openapi.rs), [CLI, `GET /state`, `GET /nodes`, `GET /state/rollup`, `barca.nodes`, the commands of two actions run](../../python/tests/test_causes.py), [the causes list over the served roots](../../ui/src/lib/causesView.test.ts) |
| A group with 13% of its nodes overdue is drawn 13 parts overdue by weight, not solid; grey has one word; the map and the bars state nodes | #966: [the weight, the stacked colours, a fresh group kept, the keys ring](../../ui/src/lib/rollupView.test.ts), [the centre in the terms of the gap and the one word for grey](../../ui/src/components/CausesList.test.tsx), [the map against one response of a served project](../../ui/e2e-groups/groups.spec.ts) |
| With a failing node that blocks 20 added to the audit shape, the first three roots hold at least 75% of the not-fresh nodes and the failure is among them; `barca status --rollup --json` carries the weight; no weight table in TypeScript | #1028: [the order and the weights, and the weight in INV-HEALTH-03 on every fixture](../../crates/barca-core/src/rollup.rs), [the audit shape with a failure (719 nodes, 443 not fresh: 152, 180 and the failure's 21 are 353), and the #963 shape under the new order (392 of 422 still)](../../crates/barca-core/src/causes.rs), [the CLI contract snapshot](../../python/tests/snapshots/cli_contract/status_rollup.txt), [the map's angles from the served weight](../../ui/src/lib/rollupView.test.ts), [the guard's table: `rollupView` lays out the served weight, `causeDisplay` holds none](../../crates/barca-cli/ui-layering.json) |
| A node with no estimate says why on every surface; a group row says when it is fresh from its entry; a schedule is late only by the server's word | #968: [the reason, and an estimate or a reason on every not-fresh node](../../crates/barca-core/src/causes.rs), [the words and the rounding](../../ui/src/lib/estimates.test.ts), [the panel's lines](../../ui/src/components/assets/EstimateLine.test.tsx), [the bar in the map's vocabulary](../../ui/src/components/HealthBar.test.tsx), [late, queue-bound and the typical wait](../../ui/src/lib/schedules.test.ts), [a queue entry's start](../../ui/src/lib/queue.test.ts), [a served project's panel, rows and bar](../../ui/e2e-groups/groups.spec.ts), [the Schedules page over the server's words](../../ui/e2e/estimates.spec.ts), [CLI, `GET /state`, `barca.nodes`](../../python/tests/test_estimates.py) |
| Every not-fresh node of a scope is under one root of its list; "more" holds the rest; a root's command can be copied | #967: [a scope's roots, their order and running total, and their sums on every fixture](../../crates/barca-core/src/rollup.rs), [the audit shape's list and the line the CLI prints](../../crates/barca-core/src/causes.rs), [the list's join and words](../../ui/src/lib/causesView.test.ts), [the list rendered](../../ui/src/components/CausesList.test.tsx), [a served project: every root listed once, more in place, a picked root lit, its node opened](../../ui/e2e-groups/groups.spec.ts) |
| An asset a tick checks every 5 minutes and finds up to date reads "checked 3m ago, up to date" on a read-only server | #964: [the rule](../../crates/barca-core/src/health.rs) ("a node the read path cannot decide takes the verdict of the tick that checked it"), [the batched read](../../crates/barca-db/src/db/ticks.rs), [CLI, `GET /state`, `GET /nodes`, `barca.nodes`, a read-only server and a live tick](../../python/tests/test_last_check.py), [the node panel's line](../../ui/src/components/assets/LastCheckLine.test.tsx) |
| A stale `Manual` node above `Always` assets, with and without a cutoff asset in between, and a pinned asset: `barca status`, `barca get --dry-run` and a tick's recorded outcome say the same of every node below it | #985: [status against the dry run, a run and live ticks](../../python/tests/test_status_run_rules.py) |
| `python/tests/test_docs_examples.py` runs the gap, rollup and top-roots queries | #965: [the columns against the JSON keys](../../crates/barca-core/src/sql/tables.rs), [every tick in one read](../../crates/barca-db/src/db/ticks.rs), [the manual's eight queries on a machine and a read-only server](../../python/tests/test_docs_examples.py) (the rollup as the gap by file), [a live scheduler's schedules and ticks, the run queue, and a query run on the client](../../python/tests/test_sql_query_route.py) |
| A query naming a result view is refused with a usage error; a 6-second query answers `408` | #972: [the refusals (INV-HEALTH-09), the time limit, a killed helper, no DuckDB, an unchanged server loading nothing](../../crates/barca-server/src/sql_query.rs), [a served project, read-only and behind a token, and the CLI](../../python/tests/test_sql_query_route.py), [the wire shape](../../crates/barca-server/tests/openapi.rs) |
| One run slot and overlapping schedules: status names the queue as the cause, with the wait | #973: [the rule](../../crates/barca-core/src/causes.rs) ("ticks that wait longer than they are apart name the queue"), [a real server with one slot, and a read-only server beside it](../../python/tests/test_causes.py) |

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
  data coverage: it reads `root` and the rollup. What it still derives: a file's one state in the
  sidebar and the asset list's filter counts, which read rows, not the rollup; and a node
  panel's count of nodes downstream, a graph walk for navigation. The health map's angles
  are the served `weight` (#1028). A node declared in several
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
- No record holds when a code or environment change was first seen (#962, "Times"): a code
  change is dated by its file's modification time, a lower bound on how long the node has
  been out of date, and an environment change has `stale_since: null` and contributes no
  backward term. Recording the first sighting of a definition hash would need a write on a
  read path; whether a run or a server's graph load should record it is open.
- A polled sensor that is not a tick's target is not counted as work in an estimate, and a
  node whose result a cutoff asset above may leave unchanged is covered only through its own
  `unknown` state: a node the read path decides `stale` below such an asset is promised fresh.
- Assets paused with `barca pause` and pinned assets keep the health their cache state gives
  and have no refresh path (#961). #963 roots a paused node at itself (`schedule_paused`) and
  a pinned asset at itself (`no_schedule`), without a kind of their own ("Root").
- `refresh_path.live` on a `--read-only` server is that process's scheduler, which it does
  not have; the cause reads the tick record instead ("Cause"). That proof is per project, not
  per schedule: a scheduling server that serves only some of the project's files leaves the
  other schedules reading as fired until their first slot passes unfired and they turn late;
  a schedule that never fired is not late, so it reads as fired for as long as any other
  schedule proves a scheduler.
- The undecided rule comes before the running one, so an undecided node a running run is
  about to decide reads `undecided`, not `catching_up`, until the run records it.
- An `input` root is read from the components a result recorded (0.29.0 on); for an older
  result the root is the node itself. Two inputs that changed name the first in the order of
  `cache.changed`.
- A `run` action's command lists every held node nothing else held reads; a wide fan-out
  makes it long. A selector for "everything below a node" does not exist on `barca get`.
- The gap of a node whose staleness has no recorded time (an environment change, a new key)
  is 0 until it has an estimate; such nodes are counted by `unbounded`, not by the gap.
- The estimate models the worker pool but not a run's startup, capacity weights, retries or
  the capacity runs share; its error is measured on short steps only ("Times").
- A node declared in several groups is placed by group id (#955 keeps the rule: it is the one
  that keeps every level an exact sum). Whether overlapping groups should instead be refused
  at parse time is open.
- `barca.nodes` has no group column, so the gap by group is the rollup tree, not a query
  (#965). A node sits under one group there (the first by group id); a column would have to
  carry that placement, not the declared groups, to add up the same.
- The numbers fixed here (20 ticks for typical values, 1,000 ticks and 14 days of retention, a
  5-second query, 1,000 and 10,000 rows) are defaults chosen without measurement; an issue that
  measures otherwise revises this document in its PR.
