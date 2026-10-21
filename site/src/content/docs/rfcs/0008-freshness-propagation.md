---
title: 'RFC-0008: Freshness Propagation in barca serve'
description: 'What Always and Manual do at run time: a scheduled tick brings the Always nodes below it up to date in one run; Manual is refreshed by hand only and pins itself.'
---

- **Status:** Accepted, implementation in 0.28.0. Nothing of it has shipped in 0.27.0: `Always` and `Manual` are still parsed and shown only, and `max_failures=` does not exist yet.
- **Date:** 2026-10-07 (accepted 2026-10-14)
- **Touches:** HTTP server | barca-core
- **Supersedes / Related:** [RFC-0001](/rfcs/0001-node-kinds-and-freshness/) (defines `Always`/`Manual`/`Schedule`; this RFC gives the first two their run-time meaning), [RFC-0004](/rfcs/0004-http-server-api/) (the scheduler and its per-job rules). Issues #253 and #80; closed attempt #261. Decisions recorded in [DIRECTION.md](https://github.com/barca-orc/barca/blob/main/DIRECTION.md#decided-october-2026).

---

## 1. Summary

Today only `Schedule` does anything at run time. `Always` and `Manual` are parsed, stored
and shown, and nothing acts on them. This RFC defines what they do in `barca serve`:

- When a scheduled node ticks, the server brings that node up to date and then, **in the
  same run**, every `Always` node below it.
- An `Always` asset is up to date when it has a result for its current code and inputs.
- An `Always` task is up to date when its last successful run used the current versions of
  its direct inputs. So a task fires only when something immediately before it changed.
- `Manual` means refreshed by hand only. A `Manual` node is never updated by a tick, and
  nothing below it is recomputed because of it; what is below reads its current result.
- A failed node is retried at every tick that reaches it, up to a per-node cap,
  `Schedule(..., max_failures=N)`.

`barca get` changes in one way: a stale `Manual` upstream is no longer recomputed on the way
to a target. It is served as it is, with a note. Only naming it in `--refresh`, or `barca get`
of the node itself with `--refresh`, recomputes it.

## 2. Motivation

**The documented meaning is not implemented.** The scheduling manual says `Always` is
"recomputed whenever stale and its upstreams are fresh" and that "a `Manual` upstream blocks
`Always` downstream nodes from auto-updating". [Core Constraints](/core-constraints/) and
RFC-0001 section 4.5 both say plainly that neither is true yet: nothing in the executor
branches on `Always` versus `Manual`.

**The workaround causes duplicate work (#253).** Because a sensor's tick only polls the
sensor, the only way to keep downstream nodes current is to put a schedule on each of them:

```python
@sensor(freshness=Schedule("*/5 * * * *"))
def inbox() -> tuple[bool, list[str]]: ...

@asset(inputs={"files": inbox})
def orders(files) -> pd.DataFrame: ...          # expensive

@asset(inputs={"orders": orders}, freshness=Schedule("0 * * * *"))
def hourly_summary(orders) -> dict: ...

@task(inputs={"orders": orders, "customers": customers}, freshness=Schedule("0 * * * *"))
def send_report(orders, customers) -> None: ...
```

At the top of the hour `hourly_summary` and `send_report` are each their own run. Each polls
`inbox` (a sensor is never cached), and when `orders` is stale both compute it, because the
two runs start together and neither has finished it when the other looks.

**Sharing a run after the fact does not work.** #261 tried to put jobs that are due together
into one run. Steps in one run are planned in phases that wait on each other, so jobs sharing
a run can delay each other; the rule had to be narrowed until it refused the shape above, and
it starved a fast job on a one-worker host. The problem is better removed at the source: the
pipeline above should have one schedule, on the sensor, and one run per tick.

## 3. Guide-Level Explanation

### 3.3 Decorator surface

No new decorators or arguments. The same pipeline, written the way this RFC intends:

```python
@sensor(freshness=Schedule("*/5 * * * *"))      # the only schedule
def inbox() -> tuple[bool, list[str]]: ...

@asset(inputs={"files": inbox})                  # Always (the default)
def orders(files) -> pd.DataFrame: ...

@asset(inputs={"orders": orders})                # Always
def hourly_summary(orders) -> dict: ...

@task(inputs={"orders": orders, "customers": customers})   # Always
def send_report(orders, customers) -> None: ...
```

Every five minutes `barca serve` polls `inbox`.

- If it returns the same value as before, nothing else runs: `orders` and `hourly_summary`
  already have results for these inputs, and `send_report` already ran with this version of
  `orders`.
- If it returns a new value, one run computes `orders` once, then `hourly_summary` and
  `send_report`. Both consumers see the same observation, and nothing is computed twice.

`Manual` pins a node, and only that node:

```python
@sensor(freshness=Schedule("*/5 * * * *"))
def prices() -> tuple[bool, str]: ...

@asset(inputs={"etag": prices}, freshness=Manual)
def baseline(etag) -> dict: ...                  # only an explicit request updates it

@asset(inputs={"baseline": baseline, "etag": prices})
def report(baseline, etag) -> dict: ...          # recomputed when prices changes, reading the pinned baseline

@task(inputs={"baseline": baseline})
def publish(baseline) -> None: ...               # does not fire when prices changes
```

`prices` changes. `baseline` is not recomputed: it is `Manual`. `report` is, because one of its
inputs changed, and it reads `baseline` as it is. `publish` does not fire, because nothing
immediately before it changed. After `barca get baseline pipeline.py --refresh baseline` (or the
equivalent API call) gives `baseline` a new result, the next tick of `prices` finds `report` and
`publish` out of date and runs them. This is the expensive-model case: the model is retrained by
hand, and everything that consumes it keeps moving on the version it has.

The same holds for `barca get`: `barca get report` serves the pinned `baseline` and prints
`[barca] note: baseline is Manual and stale; barca get baseline --refresh baseline to recompute`.
`barca get report` when `baseline` has never been computed is a usage error (exit 2) whose
remediation is that same command.

A failing node under a schedule is retried at every tick until it succeeds or hits its cap:

```python
@asset(inputs={"files": inbox}, freshness=Schedule("0 * * * *", max_failures=3))
def orders(files) -> pd.DataFrame: ...
```

After three consecutive failed ticks `orders` is skipped, `barca status` shows it as `failing`
with the count, and the next explicit `barca get orders` or a change in its inputs resets the
count. The default is 5.

### 3.4 HTTP API

No new endpoints. A tick's run has more than one target (the scheduled node and the `Always`
nodes below it) and is one run: one run id, one row in history with `triggered_by: schedule`,
and the existing multi-target result shape (`targets: [...]`, one entry per target with its
status) in `GET /runs/{id}` and the event stream. `GET /schedule` keeps `last_run` and
`last_status` per scheduled node, pointing at that shared run.

### 3.5 Dev server / `--watch` / UI

With `--watch`, an edit to an `Always` node's code changes its run hash. The next tick of a
scheduled node above it finds it out of date and recomputes it. Nothing happens between
ticks.

## 4. Reference-Level Explanation

### 4.1 Public API Surface

**Versions.** The rules below compare versions:

| Kind | Its version |
|---|---|
| sensor | the hash of the value it last returned (the `bool` in its return is not used, as today) |
| asset | its run hash, which already covers its code and its inputs' versions |
| task | its last successful run |

**The rules, for `barca serve`:**

1. **A tick reconciles.** On each tick of a scheduled node the server brings that node up to
   date exactly as it does today (a sensor is polled, a scheduled asset's cone is checked, a
   scheduled task runs). It then brings up to date every `Always` node that can be reached
   from the scheduled node going downstream through `Always` nodes only. All of this is one
   run.
2. **An `Always` asset is up to date** when a result exists for its current run hash. If one
   does not, it is computed. A result made earlier for the same run hash counts (a sensor
   that returns to an earlier value brings back the result made then, as today).
3. **An `Always` task is up to date** when its last successful run used the current version
   of every direct input. Otherwise it fires. A task whose input is another task fires when
   that task has run successfully since.
4. **`Manual` pins itself.** A `Manual` asset or task is never run by a tick, whether the
   tick is its own ancestor's or any other. It is not a wall: an `Always` node below it is
   still reached through it when some other input of that node changed, and reads the
   `Manual` node's current result. A `Manual` node that has never been computed blocks what
   reads it (section 4.5).
5. **`Schedule` stops propagation too.** A node with its own schedule is updated on its own
   ticks, not by a tick above it. Freshness answers one question, "when is this node
   updated", with one answer per node: `Always` follows what is above it, `Schedule` follows
   its cron, `Manual` follows explicit requests.
6. **Sensors are polled whenever a run needs their value.** That is the existing node-kind
   rule and it does not change. A sensor's freshness only decides whether it has ticks of
   its own. (Sensors default to `Manual` and reject `Always`, per RFC-0001.)
7. **A node with several inputs** is reached if any path to it from the ticking node passes
   only through `Always` nodes. It reads the current result of every input, including a
   `Manual` one, which stays pinned at whatever it last was. See section 10, question 1.
8. **Nodes with no scheduled node above them** are not touched by any tick. They are
   updated only by an explicit request, as today.

9. **A failed node retries at every tick, up to `max_failures`.** `Schedule(cron,
   max_failures=N)` caps consecutive failed ticks for that node; the default is 5. Past the
   cap the node is skipped and reported `failing`; an explicit request or a change in its
   inputs resets the count. The cap lives on the node and nowhere else.
10. **A node seen for the first time** is out of date and fires at the first tick that reaches
    it.

**What changes in `barca get` and `barca run`:** a stale `Manual` node on the way to the target
is served as it is, with a note, instead of being recomputed. `--refresh <node>` or
`--refresh-all` recomputes it as today. Everything else (`barca plan`, `POST /run`,
`POST /get/{target}`, `POST /run/{target}`) computes what it is asked for, as today.
The per-job rules of RFC-0004 section 4.5 still hold: a tick is skipped while the previous
run for the same scheduled node is still going, and one catch-up tick fires at startup.

### 4.2 Implementation Details

- The set of nodes a tick reconciles is a function of the DAG alone, so it is computed when
  the DAG is loaded (and again on `--watch` reload), not per tick.
- A tick's run is one multi-target run: the scheduled node plus the reachable `Always`
  nodes. Unchanged nodes cost a cache lookup each and run nothing.
- Rule 3 needs the server to know which input versions a task's last successful run used.
  Whether the existing run and step records carry enough, or a small addition is needed, is
  to be settled in implementation; it must survive a restart.
- Rule 9 needs a consecutive-failure count per scheduled node in `schedule_state`, reset on
  success, on an explicit request and on an input change.
- `max_failures` is a `Schedule` argument, so it is listed in `decorator_args.rs` and part of
  the definition hash like the cron expression.
- Rule 1 is level-triggered on purpose: each tick compares the present state with the
  desired state. A task that failed, a node edited under `--watch`, and a server that was
  down all converge at the next tick without separate machinery.

### 4.3 Rust ↔ Python Boundary

No change. Freshness is read from the AST as today; workers are not aware of it.

### 4.4 Node-Kind Semantics

Unchanged. Assets are cached, sensors and tasks are not, and a task may be an input only to
a task.

### 4.5 Edge Cases

- **A step fails.** Its downstream nodes in that run are skipped, as in any run. At the next
  tick the failed node is still out of date and is tried again, until `max_failures`.
- **A `Manual` asset that has never been computed.** Under a tick, a node that reads it cannot
  run; it is reported as blocked on that input and is not an error of the tick. Under
  `barca get`, it is a usage error (exit 2) naming the node and the `--refresh` command that
  computes it.
- **A tick arrives while its previous run is still going.** It is skipped (RFC-0004). Nothing
  is lost: versions are compared by value, so the next tick sees the latest state.
- **Two scheduled nodes above the same `Always` node.** Each one's tick reconciles it. If
  their ticks coincide, two runs may both compute a shared step. This is what remains of
  #253; see section 11.
- **A task with no recorded run.** It is out of date, so it fires at the first tick that
  reaches it (rule 10).

## 5. Determinism, Caching & Testing

Cache keys do not change. The plan for a tick is deterministic given the DAG and the
recorded versions.

Tests, all with an injected clock rather than real time:

- Unit: the reachable set for chains, diamonds, `Manual` and `Schedule` in the middle, a
  node under two scheduled nodes, and a node under none.
- Server: the two examples in section 3 (unchanged sensor runs nothing; changed sensor
  computes the shared asset once and fires each task once; `Manual` in the chain stops the
  task; an explicit refresh of the `Manual` node lets the next tick through; a node below a
  `Manual` node with another changed input is recomputed reading the pinned value); a failed
  task is retried at the next tick and not before, skipped after `max_failures`, and retried
  again after an explicit request; a restart does not refire a task that is up to date;
  `--watch` edit picked up at the next tick.
- CLI: `barca get` of a target behind a stale `Manual` node serves the pinned value and prints
  the note; behind a never-computed one exits 2; `--refresh` of the `Manual` node recomputes
  it. Contract snapshots and the `scheduling` and `cache` manual topics updated.

## 6. Performance

`barca get` and `barca run` are untouched. In the server, each tick plans a larger cone
than today. Planning is under a millisecond for typical graphs and each unchanged node is
one cache lookup; this needs a measurement on the 2,000-node benchmark file with a
one-second cron before it ships.

## 7. Drawbacks

- **It changes what existing deployments do.** A pipeline that today has a scheduled sensor
  and unscheduled `Always` nodes below it will start running those nodes, tasks included,
  when the sensor changes. That is the documented meaning, but it is new behavior with side
  effects, so it ships on by default in a minor version with a "Breaking" line.
- **`Manual` in a scheduled node's upstream cone is no longer recomputed at its tick,** and
  **`barca get` no longer recomputes a stale `Manual` upstream.** Today both are. This is the
  second Breaking line.
- Users who scheduled every consumer to work around the old behavior keep working, since
  `Schedule` nodes are left to their own ticks, but they keep the duplicate work until they
  move the schedule to the sensor.

## 8. Rationale & Alternatives

- **Put jobs due together into one run (#261, rejected).** Described in section 2.
- **De-duplicate steps across concurrent runs (#80).** A run that needs a step another run
  is already computing waits for it. This is the right answer for runs that are separate
  for good reason, and it is listed under future work. It is a larger change (one owner for
  the worker pool and in-flight steps), and it does not give `Always` and `Manual` a
  meaning, which the documentation already promises.
- **Edge-triggered propagation** (act only at the moment a sensor reports a change). It
  loses work when a step fails or the server restarts between the change and the reaction.
  Comparing state at every tick has no such gap and costs a cache lookup per node.
- **Use the sensor's `update_detected` flag as the trigger.** The cache already keys on the
  returned value and ignores the flag. One definition of "changed" is better than two.

## 9. Prior Art

Dagster's declarative automation ("eager" materialization of assets downstream of a change)
is the closest equivalent, configured per asset with a policy object. Barca keeps the
three-value freshness field from RFC-0001 and gives the default a meaning instead.

## 10. Unresolved Questions

None at acceptance. The six questions the draft carried were decided on 2026-10-14 and are in
the rules above: a changed input beside a stale `Manual` input updates the node (rule 4);
`Manual` pins in `barca get` too, as a Breaking change; a failed node retries every tick up to
`max_failures` (rule 9); a new task fires at the first tick (rule 10); propagation is on by
default with a Breaking line; a tick is one run with several targets in the existing
multi-target shape (section 3.4).

## 11. Future Possibilities

- Step-level de-duplication across concurrent runs (#80), for scheduled nodes whose cones
  overlap and whose ticks coincide.
- Showing, in `barca list` and the UI, which scheduled node each `Always` node follows, and
  which nodes are followed by nothing.
- A `Freshness::Reactive` kind, as floated in RFC-0001, is not needed if this RFC is
  accepted: `Always` below a scheduled sensor is that behavior.
