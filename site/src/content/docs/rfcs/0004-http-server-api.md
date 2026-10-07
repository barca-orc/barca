---
title: 'RFC-0004: HTTP Server API'
description: 'barca serve — endpoints, the async run/poll contract, cron scheduling, and the Python client.'
---

- **Status:** Accepted (retroactive baseline — documents behavior as of v0.6.1)
- **Date:** 2026-07-16
- **Touches:** HTTP server | dev server/UI | barca-cli | python/barca
- **Supersedes / Related:** [RFC-0001](/rfcs/0001-node-kinds-and-freshness/) (Schedule freshness), [RFC-0002](/rfcs/0002-cli-surface/) (`serve` flags, shared result shape)

---

> **Amended (0.18, issue #253):** scheduled jobs that fire together (due at the same tick, or
> caught up together at startup) now share one run when they have a step in common and the
> shared run would delay none of them, so that step is computed once. The rule is narrow on
> purpose and §4.5 says which shapes share and which do not. The per-job guarantees of §4.5
> are unchanged and are stated there job by job; §4.1 gains the status payload of a run over
> several targets, the `target_finished` event and the time limit of a shared run. Before 0.18
> every due job started its own run.

## 1. Summary

`barca serve` starts an axum-based HTTP/JSON API (`barca-server`) that exposes the same
`barca-core` commands as the CLI, plus a cron scheduler that gives `Schedule`-freshness
nodes runtime teeth. Runs are asynchronous — trigger endpoints return a `run_id`
immediately and clients poll `/status/{run_id}`. `barca.Client` is the reference Python
consumer.

## 2. Motivation

The CLI's process-per-invocation model is wrong for programmatic triggering, scheduling,
and (eventually) a web UI — each of those needs a long-lived process that already has
the DAG parsed and can serve many requests without re-paying parse cost per call. Rather
than build a second implementation, `barca-server` reuses `barca_core::commands::*`
directly (no subprocess, no separate daemon), so the server and the CLI can never
diverge on what a run actually does.

## 3. Guide-Level Explanation

### 3.1 CLI

```bash
barca serve pipeline.py                   # serve a DAG, default port 8274
barca serve pipeline.py --port 8400       # custom port
barca serve pipeline.py --watch           # dev mode: re-parse DAG on file change
barca serve pipeline.py --no-schedule     # disable the cron scheduler
barca serve pipeline.py --timezone utc    # evaluate cron in UTC (default: local)
```

### 3.4 HTTP API

```bash
curl -XPOST localhost:8274/get/daily_report
# {"run_id": "1b942bb33182"}

curl localhost:8274/status/1b942bb33182
# {"handle": "1b942bb33182", "status": "complete", "result": {...}, ...}

curl localhost:8274/schedule
# [{"id": "pipeline.py:daily_report", "cron": "0 5 * * *", ...}]
```

```python
from barca import Client

c = Client("http://127.0.0.1:8274")
run = c.get("daily_report")       # POST /get/{target}
result = run.wait(timeout=30)     # poll /status until complete/failed
```

### 3.5 Dev server / `--watch` / UI

`--watch` is a local-development convenience, off by default: it re-parses the DAG when
a source file changes so `/assets` and `/plan` reflect edits without a restart, and
(with the scheduler on) re-reads the cron job set live within a minute. It has no effect
on the production serving path. There is no UI shipped yet — a future UI is a separate,
non-Rust package that consumes this API as its only contract (see
[Architecture](/architecture/)); it could later be served from the same process via a
static-file route.

## 4. Reference-Level Explanation

### 4.1 Public API Surface

**Endpoints (v1):**

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Liveness + version |
| `GET` | `/assets` | Every node: kind, freshness, inputs |
| `GET` | `/assets/{name}` | One asset's summary + timing/cache stats |
| `GET` | `/plan` | Execution plan (phases/streams) as JSON |
| `POST` | `/run` | Trigger a full run → `run_id` |
| `POST` | `/run/{target}` | Trigger a task run → `run_id` |
| `POST` | `/get/{target}` | Trigger a run scoped to one asset → `run_id` |
| `DELETE` | `/run/{run_id}` | Cancel an in-flight run |
| `GET` | `/status/{run_id}` | Poll a run's status/result |
| `GET` | `/schedule` | List scheduled jobs |

**Async run contract.** Trigger endpoints return `{"run_id": "..."}` immediately.
`status` on `/status/{run_id}` is one of `pending`/`running`/`complete`/`failed`/
`cancelled` (terminal: the last three). `handle` is the server's in-memory polling id;
`result.run_id` is the persisted database run id (same row `barca history` shows) — the
two are usually equal but are distinct concepts. In-flight state is memory-only and not
persisted across a server restart; a background sweep evicts finished runs from memory
after an hour (checked every 5 minutes), after which `/status/{run_id}` for that id
returns `404` even though the DB row remains.

**Cancellation.** `DELETE /run/{run_id}` terminates the run's Python workers, persists
partial results from already-completed steps, and transitions status to `cancelled`.
Response is `{"run_id": "...", "status": "cancelling"}` — poll `/status` to observe the
actual transition. Cancelling an already-finished run returns `409`. Runs exceeding the
server's 10-minute timeout are stopped the same way and reported as `failed`. The run's row
in `barca history` says `cancelled` for such a run, because the run itself only sees that it
was stopped: the two surfaces disagree on a timeout, and `/status` is the one that knows why.

**Runs over several targets.** A run the scheduler starts for several jobs that share it
(§4.5) is one run with one `run_id`, and reports like `barca get a,b`:

- *Status.* `complete` when every target succeeded. `failed` when any target failed, with
  `error` naming them (`"1 of 3 targets failed: pipeline.py:broken"`). A failure stops only
  the targets downstream of it; the others still run.
- *Result.* `result` has `targets` in place of `final_output`: an object keyed by node id, in
  the order the jobs were fired, each `{"status": "success", "final_output": {...}}` or
  `{"status": "failed", "error": "...", "failed_node": "..."}` (`failed_node` is the step that
  failed: the target itself or a step upstream of it). Unlike a single-target run, whose
  `result` is `null` when it fails, this result is present on `failed` too, because the other
  targets have outcomes worth reading. It is `null` when the run was cancelled or timed out.
- *Events.* `/events/{run_id}` carries one `target_finished` event (`{node_id, ok}`) per
  target, emitted when that target's own steps have all ended **and are recorded** in the
  metadata DB, which can be long before the run ends. Finished steps are recorded during the
  run at most every half second, so the event follows the step by up to that long; a target
  whose steps were not recorded during the run is announced when the run's results are
  written, just before `run_finished`. The order matters: whoever acts on the event (the
  scheduler firing the job's next tick) must find the job's results cached. `run_finished` has
  `ok: false` whenever the status is not `complete`. Every run with a named target emits
  `target_finished`, single-target runs included.
- *History and telemetry.* One row in `barca history` and one trace: `target` is the node ids
  comma-separated, as for `barca get a,b`; `status` is `failed` when any target failed;
  `command` is `get` when the targets are all assets and sensors, `run` when they are all
  tasks, and `serve` when they are both (no CLI command takes that mix).
- *Time limit.* The 10-minute limit is per target: a run over `n` targets is stopped after
  `n` × 10 minutes. Jobs that each had a run, a worker pool and 10 minutes now share one run
  and one pool, so the shared run gets the sum. No job is stopped earlier than it was before
  0.18. A job that hangs is stopped later: after `n` × 10 minutes instead of 10. Until then
  only that job's ticks are skipped; the others are not held (§4.5).
- *Cancellation.* `DELETE /run/{run_id}` cancels the whole run. Targets whose steps had
  already ended keep their recorded results; the rest are cancelled.

**Errors.** `{"error": "..."}` body with: `404` (unknown asset/run), `400`
(parse/DAG errors), `409` (ambiguous `{name}` match, or cancel-after-finish), `500`
(execution/DB failure).

**`barca.Client`** (`python/barca/client.py`, stdlib-only) — `health()`, `assets()`,
`asset(name)`, `plan()`, `schedules()`, `status(run_id)`, `cancel(run_id)` (also
`Run.cancel()`), plus trigger verbs mirroring the CLI: `get(target=None)` (target
optional — omit for a full-DAG run) and `run(target)`. Trigger methods return a `Run`
whose `.wait(timeout=600.0, poll=0.5)` blocks until terminal; it does not raise on run
*failure*, only on poll timeout — callers must inspect `["status"]`/`["error"]`
themselves. This complements, and is entirely independent from, `barca.api`
([RFC-0003](/rfcs/0003-decorator-and-python-api/)), which shells out to the binary
rather than talking to a server.

**Not in v1:** no authentication (binds to `127.0.0.1` only — do not expose to
untrusted networks), no WebSocket/SSE streaming (poll `/status`), no web UI, no
distributed execution, no persistence of the in-memory run queue across restarts.

### 4.2 Implementation Details

`barca-server`'s `routes.rs` is the single API boundary; `handlers.rs` awaits the async
`barca-core` commands directly on the CLI's single tokio runtime (built once in
`main()`, also driving axum). `state.rs` holds `AppState` (a `DashMap` of
`RunState`/`RunStatus`) and the DAG cache. See [Architecture](/architecture/) for the
crate layering (`barca-core` has no HTTP/UI awareness by design).

### 4.3 Rust ↔ Python Boundary

No new boundary — a served run dispatches to the exact same worker pool / UDS protocol
as a CLI-invoked run (see [Architecture Decisions](/architecture-decisions/)). The only
difference is the caller: an axum handler instead of `barca-cli`'s `main()`.

### 4.4 Node-Kind Semantics

`GET /assets` surfaces kind/freshness/inputs per node exactly as
[RFC-0001](/rfcs/0001-node-kinds-and-freshness/) defines them; `POST /run/{target}` vs.
`POST /get/{target}` mirrors the CLI's `run`-is-for-tasks / `get`-is-for-assets split
(§4.1 of [RFC-0002](/rfcs/0002-cli-surface/)).

### 4.5 Edge Cases

- `barca serve` does not support shared remote state — if `barca.toml` resolves to
  `state = "optimistic"`, `serve` refuses to start with an error directing you to
  `state = "off"` or `BARCA_STATE=off` (see
  [RFC-0006](/rfcs/0006-configuration-and-remote-state/)).
- A scheduled job never overlaps itself: if its previous run is still pending/running
  when the next cron tick arrives, that tick is skipped (not queued).
- On startup, a scheduled tick that elapsed while the server was down fires **once** to
  catch up; jobs never seen before are anchored to "now" (no first-launch stampede).
  Individual ticks missed during a longer outage are not replayed one-for-one.
- **Which jobs share a run** (0.18). Jobs that are due together (at one tick, or caught up
  together at startup) share one run when both of these hold:
  1. they have a step in common: their cones overlap, directly or through a third job;
  2. the shared run would plan nothing ahead of any of them except that job's own upstream.

  The run is over the union of their cones, so the common step runs once. Jobs are compared by
  the instant they are due, not by their cron text: `0 5 * * *` and `*/5 * * * *` are due
  together at 05:00 and not at 05:05.

  Condition 2 exists because a run executes in phases and a phase starts only when the one
  before it has ended: a job waits for every step of the phases before its own. It is read off
  the plan of the shared run, exactly; nothing is estimated. A job that fails it is taken out
  and runs alone, and the rest are judged again. The shapes this gives:
  - *Share:* jobs that read the same upstream side by side (two scheduled tasks reading one
    asset; two scheduled assets reading one sensor), and a job with the jobs downstream of it
    (a scheduled asset and the scheduled tasks that read it, the case #253 was filed for).
  - *Do not share:* jobs at different depths below the common step, and a job that also reads
    something the others do not. Example: `tracked` reads sensor `version`; `publish` reads
    `feed` (which reads `version`) and `model`. In one run `publish` would wait for `tracked`
    and `tracked` for `model`, so each gets its own run and `version` is polled by both, as
    before 0.18. Sharing these needs a run that does not gate phases on steps a target does
    not read (§11).
  - Jobs with nothing in common never share: one run would compute nothing fewer times, and
    would tie them to each other's failure status, cancellation and time limit.
  - A job that ends up alone runs as it always did (`get` for an asset or sensor, `run` for a
    task) and its `/status` payload is unchanged.

  The decision uses the DAG read when the job set was read (at startup, and on `--watch`
  reload), so a tick reads no source file. When condition 2 keeps a job out, the server says
  so once on stderr, naming the job, the jobs it would have run with and the step it would
  have waited for. That line is for people, not a contract.
- **Inside a shared run** each job's step waits only for its own inputs and a free worker. The
  run gives every chain a stream of its own (an ordinary run packs chains into as many streams
  as workers, in order) and leases a worker steps of one node at a time (an ordinary run may
  lease a quick step behind another node's). The jobs share one pool of workers, one per core.
- **Sharing a run does not tie jobs to each other.** The guarantees above hold for each job
  on its own:
  - *Overlap* is judged by the job's own step. A job's "previous run" is still going only
    until that job's step (every partition of it) has ended and is recorded: completed,
    failed for good, or skipped because a step upstream of it failed. From then on its next
    tick fires, even while a slower job keeps the shared run open, and finds what that run
    computed already cached. At a tick where some due jobs are still running and some are
    not, the ones still running are skipped and the rest are fired.
  - *Failure.* A job that fails stops only the jobs downstream of it in that run.
  - *Catch-up* is decided per job (did *this* job miss a tick?), then the jobs that did are
    fired as jobs due together are.
- **Not with a remote artifact store.** When artifacts go to a store other than the local
  artifact directory, a step is recorded only when its run ends, once its upload is confirmed
  (see [Remote storage](/reference/remote-storage/)). A job in a shared run could
  then not fire again before the whole run ended without recomputing what the run had just
  computed. Keeping each job's ticks independent comes first, so such a server keeps the
  pre-0.18 behavior: one run per due job, and a step upstream of two jobs due together may be
  computed by both. Sharing runs there needs steps recorded as they finish (§11).
- `GET /schedule` is per job. `last_run` is the handle of the run the job last fired into
  (jobs that shared a run report the same one). `last_status` is the job's own: `pending` or
  `running` until its step has ended, then `complete` or `failed`, whatever the run's other
  jobs go on to do; `cancelled` if the run was cancelled before the job's step ended, and
  `failed` if the run timed out or failed first. It stays available after the run itself is
  evicted from memory.

## 5. Determinism, Caching & Testing

Served runs use the same cache/provenance model as the CLI ([RFC-0001](/rfcs/0001-node-kinds-and-freshness/),
[RFC-0005](/rfcs/0005-artifact-serialization-and-storage/)) — the server adds no new
cache semantics, only a scheduling and polling layer on top. Independent scheduled runs
execute concurrently (bounded by a run pool sized to CPU count); their writes to the
shared `metadata.db` are serialized by a process-wide DB lock. Covered by
`barca-server`'s `cargo test` suite and the shell integration tests in
`tests/integration/`.

The shared-run semantics of §4.5 are tested without the wall clock. The planning functions
(`plan_tick`, `plan_catchup`, and `barca_core::share::shared_run_groups`, which decides who
shares a run) are pure and take the time or the DAG as an argument; the server-level
tests in `crates/barca-server/src/scheduler/batch_tests.rs` call the scheduler's `tick` and
`catch_up` with a fixed time against real pipelines, and hold a slow step open with a file
the test creates, so no assertion races a cron tick.

## 6. Performance

The server is not on barca's headline "invisible" hot path (that's CLI-invoked `get`),
but per-request handler latency and the cron scheduler's per-minute wake cost matter for
serving many scheduled jobs. No dedicated `benchmarks/` scenario exists yet for server
throughput — see §11.

## 7. Drawbacks

No auth in v1 means `barca serve` is unsafe to expose beyond localhost as-is; this is a
deliberate v1 scope cut (documented, not accidental), not an oversight.

## 8. Rationale & Alternatives

Reusing `barca_core::commands::*` directly (rejected alternative: spawn the CLI binary
as a subprocess per request, mirroring how `barca.api` calls the CLI) avoids doubling
process-spawn overhead per HTTP request and keeps the server and CLI provably identical
in behavior — there's only one implementation of `get`/`run`/`plan` to keep correct.

Polling over `/status` (rejected: WebSocket/SSE push) was chosen for v1 simplicity — no
persistent-connection state to manage across server restarts, at the cost of poll
latency for callers that want near-real-time updates.

## 9. Prior Art

Dagster's GraphQL API and run-launcher model, Prefect's orchestration API (both support
streaming/webhooks that barca's v1 doesn't) — see
[Framework Comparison](/comparisons/framework-comparison/).

## 10. Unresolved Questions

Should `/status` gain long-polling or SSE before a real UI is built on top of it, given
polling-only is explicitly called out as a v1 limitation?

## 11. Future Possibilities

- Authentication (even a simple bearer token) before any non-localhost deployment story.
- A `benchmarks/` scenario for server throughput under many concurrent scheduled jobs.
- Serving a future web UI's static assets from the same process (`barca-server`'s
  layering already anticipates this — see [Architecture](/architecture/)).
- Shared remote state support in `serve` (currently rejected at startup, §4.5).
- Shared runs with a remote artifact store (§4.5): record a step when its upload is confirmed
  rather than at the end of the run, so a target can be announced finished mid-run there too.
- Shared runs for jobs at different depths or with extra roots (§4.5): an executor that starts
  a step when its own inputs are ready instead of when the previous phase has ended.
