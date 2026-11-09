---
title: "Serve Operations"
description: "Operating barca serve: the run queue, stopping and restarting without losing work, the stop grace a supervisor needs, a shared capacity and the run timeout."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/serve-ops.md
---

<!-- Generated from crates/barca-cli/docs/serve-ops.md (barca docs serve-ops). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

A running server accepts runs into a durable queue, drains when told to stop and resumes the
queue when it starts again. This topic is for the person who deploys and restarts it: the run
queue and `barca queue`, what each signal does and the exit code it ends with, the stop grace a
supervisor must allow, memory limits in containers, a capacity shared by every run, and the run
timeout. What a schedule does is [`barca docs scheduling`](/scheduling/).

## The run queue

Every run the server accepts joins its run queue: `POST /runs` and the older trigger routes, a
`--server` client, the web UI, and each scheduled tick. The run is recorded in the metadata DB
(`run_queue`) under its run id before the server answers, so a restart does not lose it. Runs
start in the order they were accepted, at most `--max-runs` at once (`[server] max_runs` in
`barca.toml`, default one per core); the others wait with status `pending`. `POST /runs` answers
`202 {"run_id", "status", "position"}`: `running` when it started at once, or `pending` and its
place in the queue (1 = next).

```bash
barca serve pipeline.py --max-runs 2           # two runs at a time; the rest wait, in order
barca queue --server                           # the configured server's queue: running and queued runs
barca queue --server http://127.0.0.1:8274 --json   # {paused, paused_at, draining, max_runs, running, queued}
barca queue pause --server                     # nothing new starts; runs in flight go on
barca queue resume --server                    # queued runs start again, in order
```

`barca queue` reads the server named by `--server`, `BARCA_SERVER` or `[server] url`; without one
it is a usage error (exit 2). The same is `GET /queue`, `POST /queue/pause` and
`POST /queue/resume`, and the web UI's Runs page shows the queue with a Pause / Resume button.
`GET /health` has `queue: {paused, queued, running, max_runs}`.

While the queue is paused, runs in flight finish, new runs are still accepted and wait, and a
scheduled tick joins the queue too. The rule for a tick whose node already has a run pending or
running holds: one tick waits, the ticks after it are skipped (`skip_reason: "in_flight"`), so a
long pause leaves one run per scheduled node, not one per tick. The pause is kept in the metadata
DB (`server_state`), like a schedule's pause: a server restarted while paused stays paused and
prints `[barca] the run queue is paused (since ...)`. A `--read-only` server refuses pause and
resume with `403` (`barca queue pause` exits 2); its `GET /queue` shows what the metadata DB
records. `DELETE /runs/{id}` on a queued run takes it off the queue and records it `cancelled`.
`barca state reset` (and `POST /state/reset`, refused while a run is queued) clears the pause
with the rest of the database.

When the server starts it resumes the runs it finds queued, in their order and under their run
ids, before its scheduler catches up, and prints `[barca] resuming 3 queued runs`. A client that
holds a run id sees the run go `pending` → `running` → `success` across the restart. A run the
previous server had started and never finished recording is not started again when the server
died on its own (SIGKILL without a stop signal first, such as the OOM killer, or a crash): it is
reported once on startup and recorded as interrupted (`error` when it never wrote a row), because
a run that kills its server would otherwise kill every server after it.

A run that was in flight when the server was told to stop is different. The first SIGTERM or
SIGINT marks every run in flight `draining` in the queue before anything else; if whatever sent
the signal kills the server before the drain ends (a supervisor whose grace period is too short),
the next server finds the run still `draining`, prints `[barca] run RUN_ID was in flight when the
last server was killed during its drain (SIGKILL after SIGTERM: the stop grace period ran out);
it is put back in the queue and starts again`, and starts it again first, under the same id, as
if the drain timeout had passed. This happens once per run: a run that is in flight through two
killed drains in a row is recorded as interrupted, so a run that keeps its server from stopping
cannot loop.

### Runs share the steps they have in common

Experimental (new in 0.39.4). Known gaps: cancelling the run that computes a step makes the waiting run execute it again from the start, the result is handed to the waiting run before the computing run's row is durable, and a run that decided a step before another run finished it but leases it after that run's claim ended executes it a second time (#654).

Two runs of the server that need the same asset step at the same time compute it once: the same
node, the same partition key and the same run hash (code, inputs and everything else that goes
into it). The run that reaches the step second waits for the first and reports it `cached`, with
the detail `computed by another run of the server at the same time`; only the run that computed
it records a row, and its `steps_executed` counts it. Steps the runs do not have in common run
concurrently: `barca get part --partition k=1` and `barca get part` share key 1 only.

```bash
barca get report --server   # in one terminal
barca get report --server   # in another, meanwhile: waits for the first run's steps, all cached
```

Not shared: tasks (each `barca run` is its own invocation), sensors, `parallel()` branches, steps
under different run hashes, and runs outside this server (a CLI run in the same project, or a
local run against the server's ledger). When the run computing a step fails it, or is cancelled,
or its worker dies, one waiting run computes the step itself; a cancelled run's work so far is
lost and done again.

## Stopping the server

SIGINT (Ctrl-C) and SIGTERM (`kill`, `docker stop`, systemd) stop the server the same way: the
first one starts a drain, which loses nothing the server accepted.

```bash
barca serve pipeline.py                      # on SIGTERM, runs in flight get 30 seconds
barca serve pipeline.py --drain-timeout 300  # give them 5 minutes
barca serve pipeline.py --drain-timeout 0    # stop them at once and requeue them
```

```toml
# barca.toml (the flags win)
[server]
drain_timeout = 300
max_runs = 4
```

During the drain the server prints `[barca] SIGTERM received: draining — the queue is paused
and new runs are queued for the next start; waiting up to 30s for 1 run in flight` and:

- starts no queued run, and keeps accepting new ones: `POST /runs` (and the older routes)
  answer as usual, with `status: "pending"`, and the run waits in the queue for the next server
  (`draining: true` in `GET /queue`);
- stops the scheduler (`[barca] scheduler stopped: the server is draining`). A tick that falls
  in the drain starts nothing; the next server's startup catch-up fires the job once;
- reports `"status": "draining"` on `GET /health` (still `200`);
- keeps answering reads, `GET /runs/{id}`, its `/events` and `DELETE /runs/{id}` (and their
  older aliases); `POST /state/reset` and ledger posts answer `503` with `Retry-After`;
- waits for the runs in flight, their artifact uploads included, to finish and be recorded.

When they have all finished it prints `[barca] drained: every run finished; shutting down` (and
how many runs wait in the queue), ends the open `/events` streams and exits 0. With nothing in
flight that takes well under a second.

When the drain timeout passes before the runs in flight have finished, they are stopped and put
back at the head of the queue: `[barca] the drain timeout (30s) passed: stopping 1 run and
putting it back in the queue`. Their workers are stopped, the steps that finished are kept (and
are cache hits on the next attempt), and the stopped attempt is not recorded as a run that ended:
its row and lines are replaced by the next attempt's. The server exits 0, and the next server
starts them again from the beginning, first, under the same run ids.

A second SIGTERM or SIGINT during the drain cancels the runs in flight instead: each run is
recorded as `cancelled` in `barca history`, with `[barca] run cancelled: server shutdown` in its
logs (`barca logs RUN_ID`) and `barca.reason: server shutdown` on its `barca.run` telemetry span.
The server exits 1, after an error naming how many runs were cancelled, so a supervisor or
deploy script can tell a forced stop from a clean one. The queued runs are kept either way.
Stopped runs get 10 seconds to stop their workers and record themselves, and connections still
open after that get 2 more (shorter with a drain timeout under 10 seconds: below).

### At least once

A run the drain timeout requeued runs again from its start, planned on the code the restarted
server has. Assets that finished are cache hits then while their code and inputs are unchanged; one
a deploy changed runs again under its new run hash, as in a new run. A step that was interrupted
runs again, and a task runs again from its beginning
(including what it did before it was stopped, such as an email or an insert). So a run the server
accepted is executed at least once, not exactly once. A task with side effects should be safe to
repeat (an upsert, a key that makes a second send a no-op), or give the drain long enough for it
to finish (`--drain-timeout`), or cancel it with a second signal when a repeat would be worse
than no run.

SIGHUP and SIGQUIT are not handled. Outside a container they end the process at once, and a run
in flight is then reported as `interrupted` ([`barca docs cache`](/reference/cache/)); `nohup barca serve` keeps
ignoring SIGHUP. A process that is process 1 of a container never receives them. SIGKILL cannot
be handled by any program: the queued runs survive it, the runs in flight are interrupted.

## Restarting without losing work

A deploy, a reboot or a config change restarts the server. Nothing it accepted is lost:

```bash
barca queue pause --server       # optional: let nothing new start before the restart
systemctl restart barca          # or: docker restart barca
barca queue --server             # after: the queue resumed (still paused if you paused it)
barca queue resume --server      # if you paused it
```

1. Pausing first is optional. It lets the runs in flight finish while nothing new starts, so the
   stop that follows waits only for them.
2. The supervisor sends SIGTERM: the server drains (above), the runs in flight finish or, at the
   drain timeout, go back to the queue, and it exits 0. Clients following a run reconnect.
3. The new server prints `[barca] resuming N queued runs` and starts them in order.

Give the supervisor a stop grace period of at least the drain timeout plus what the server takes
after it. Otherwise it kills the server mid-drain; the runs in flight are then put back in the
queue by the next server, once (above), but they lose the time the drain gave them. After the
drain the server takes:

| Part | Drain timeout 10 s or more | Drain timeout under 10 s |
|---|---|---|
| Stopped runs terminate their workers and record themselves | 10 s | half the drain timeout, at least 2 s |
| Open connections finish | 2 s | 1 s |
| The last metadata backup (only with a store) | 31 s | the drain timeout, at least 5 s, plus 1 s |
| The telemetry flush (only with `BARCA_TELEMETRY`) | 3 s | 3 s |

So 42 seconds with the default drain timeout of 30 and no store, 73 with a store; 9 seconds with
`drain_timeout = 5`, which fits Docker's default of 10; 3 with `drain_timeout = 0`. Under a
supervisor, `barca serve` prints the number for its configuration when it starts: as process 1
of a container, under systemd (`INVOCATION_ID` is set) and under Kubernetes
(`KUBERNETES_SERVICE_HOST` is set):

```text
[barca] stop grace: the container runtime must allow at least 9s between SIGTERM and SIGKILL (drain timeout 5s + 4s for stopping runs and closing connections); set docker stop -t or stop_grace_period (Docker's default is 10s)
```

With the default 30-second drain timeout, 45 seconds or more without a store (42 plus a margin),
and 75 or more with one (73 plus a margin); the table gives the no-store numbers:

| Supervisor | Setting | Default | With `drain_timeout = 30` |
|---|---|---|---|
| systemd | `TimeoutStopSec=` | 90 s | 90 s is enough |
| Docker | `docker stop -t`, `stop_grace_period` in Compose | 10 s | `stop_grace_period: 45s` |
| Kubernetes | `terminationGracePeriodSeconds` | 30 s | `45` |

This also holds when barca is process 1 of a container, so a plain `docker stop` drains. Keep
`.barca/` on persistent storage: the queue is in its metadata DB.

### Clients ride through a restart

A `--server` client following a run (`barca get ... --server`, `barca run ... --server`) and
`barca logs -f --server` keep following it when the server goes away: they print `[barca] lost
the event stream of run RUN_ID (...); waiting up to 60s for the server to come back`, ask the
server again every half second, and once it answers follow the same run id there (`[barca] the
server restarted; following run RUN_ID there`). A run the server stopped and requeued is shown
again from its new start; a run that ended meanwhile is read from `GET /runs/{id}`. When the
server does not come back within the window, the command ends as before: exit 3, naming the run
and how to read it later. `BARCA_SERVER_RECONNECT` sets the window in seconds (0: do not wait).

```bash
BARCA_SERVER_RECONNECT=300 barca run publish --server   # wait up to 5 minutes for a restart
BARCA_SERVER_RECONNECT=0 barca logs -f --server         # give up at once, as before 0.29.4
```

## Containers with a memory limit

Each step runs in its own worker process, and `serve` runs independent steps at the same time,
up to the pool size. Under a hard memory limit (`mem_limit` in Compose, a pod's memory limit),
heavy steps that become ready together (a cold cache after a deploy, say) can add up past it:
the kernel kills one, and the step fails with
`worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)`. The same step
succeeds alone.

Give the heavy steps a `weight=` (experimental, [`barca docs assets`](/reference/assets/), "Heavy steps"): the steps
running at once add up to at most the capacity, the pool size unless `[execution] capacity` in
barca.toml or `BARCA_CAPACITY` sets it. With

```toml
# barca.toml
[execution]
capacity = 4
```

and `@asset(weight=2)` on the heavy assets, at most two of them run at once, and light steps
still use what is left. `barca doctor` reports the capacity in effect and the pool size. The
pool is the cores, or the capacity when it is smaller (`[execution] pool_size` or
`BARCA_POOL_SIZE` set it outright), so `capacity = 1` starts one worker, not one per core. That
capacity is per run: two schedules that fire together run with a capacity each. To limit the
server as a whole, give it a capacity of its own (below).

## A capacity shared by every run

Experimental. `barca serve --capacity N` (or `[server] capacity = N` in barca.toml; the flag wins)
admits the `weight=` of the steps of every run on the server against one budget. A step starts
only when it fits its own run's capacity and the server's. Use it instead of `--max-runs 1` to
keep a light, frequent job running while a long heavy rebuild holds the rest:

```toml
# barca.toml
[server]
capacity = 4      # the steps of every run together weigh at most 4
```

```python
from barca import Schedule, asset

@asset(freshness=Schedule("* * * * *"))   # the one-minute sync: weight 1
def sync() -> dict:
    return {"rows": 1}

@asset(weight=3)                          # the rebuild's heavy steps: 3 of the 4
def rebuild_customer() -> dict:
    return {"rows": 2}
```

```bash
barca serve pipeline.py --capacity 4           # the same, from the command line
barca queue --server --json                    # capacity: {total, in_use, runs, waiting}
```

While a rebuild step of weight 3 runs, the sync's weight-1 steps run beside it, one at a time;
a second weight-3 step waits. How a step is admitted:

- **First come, first served across runs, with the head reserved.** A run asks for the step at
  the head of its own ready queue once that step fits the run's own capacity. If nothing waits
  and it fits what is left, it starts. Otherwise the run waits in line, one place per run. Only
  the run at the front of the line can be admitted, when its step fits; nothing behind it goes
  first, even a lighter step that would fit. So a heavy step is never starved by a stream of
  light ones: the weight that comes free is kept for it until it fits, and the light steps that
  asked after it wait that long (in the example, at most one heavy step's length when a heavy
  step waits). Steps within one run keep the order of their run's queue, as before.
- **No deadlock.** Only running steps hold weight, and they finish, fail or are cancelled. A
  step heavier than the server's capacity counts as all of it: it runs alone on the server, with
  `[barca] warning: 'x' has weight=8, more than the capacity 4 (--capacity); it runs alone` when
  its run is planned. A step that calls `parallel()` gives its weight to its branches while it
  waits for them and takes it back when it resumes, even if that briefly goes over.
- **Per-run capacity.** `[execution] capacity` still limits each run. The pool of each run is
  the cores, bounded by the smaller of the two capacities. Runs keep their own worker pools; a
  run waiting for the server's capacity holds no worker for the step it waits with.
- **`--max-runs`** still limits how many runs execute at once; a run waiting for capacity holds
  its run slot. With a server capacity, `--max-runs` can be larger than you would otherwise set
  it: the capacity, not the number of runs, bounds the load.
- **The run queue** is unchanged: runs leave it as before, and the capacity is held in memory, so
  a restart starts with nothing in use.

`GET /queue` (and `barca queue --server --json`) has `capacity: {total, source, in_use, runs,
waiting}`: the weight in use, the weight each run holds, and the steps waiting in admission
order with their run, weight and `waiting_seconds`; `null` without a server capacity. `GET
/health` has `queue.capacity: {total, in_use, waiting}`. The web UI's Runs page shows `weight 3
of 4 in use · 1 step waiting` and the waiting steps.

## Run timeout

A run under `barca serve` has no time limit by default. `--run-timeout SECONDS` (or `[server]
run_timeout_seconds` in barca.toml; the flag wins) stops a run that takes longer, and
`Schedule(cron, timeout_seconds=N)` sets it for one schedule's ticks, over both. 0 means no
limit at any of them. The time counts from when the run leaves the queue.

```bash
barca serve pipeline.py --run-timeout 3600     # stop any run after an hour
barca serve pipeline.py --run-timeout 0        # no limit (the default)
```

```python
@asset(freshness=Schedule("*/5 * * * *", timeout_seconds=240))   # a tick's run may take 4 minutes
def sync() -> dict:
    return {"rows": 1}
```

A run that times out is stopped like a cancelled one (its workers are terminated; steps that
finished stay cached) and is recorded as `error` in `barca history`, with the reason in its
outcome: `run timed out after 240s (Schedule(timeout_seconds=) of sync)`, naming the setting
that applied. Before 0.31.0 every run under `barca serve` stopped at 600 seconds, a limit that was
not configurable; `--run-timeout 600` restores it.

See also: [`barca docs scheduling`](/scheduling/), [`barca docs server-mode`](/reference/server-mode/), [`barca docs telemetry`](/reference/telemetry/),
[`barca docs contract`](/reference/cli-contract/).

---

*The same text is in your terminal: `barca docs serve-ops`.*
