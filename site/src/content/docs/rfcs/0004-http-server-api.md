---
title: 'RFC-0004: HTTP Server API'
description: 'barca serve — endpoints, the async run/poll contract, cron scheduling, and the Python client.'
---

- **Status:** Accepted (retroactive baseline — documents behavior as of v0.6.1)
- **Current behavior:** this page is a historical baseline and some details have changed since v0.6.1. For what barca does now, read the [Server API reference](/reference/server-api/).
- **Date:** 2026-07-16
- **Touches:** HTTP server | dev server/UI | barca-cli | python/barca
- **Supersedes / Related:** [RFC-0001](/rfcs/0001-node-kinds-and-freshness/) (Schedule freshness), [RFC-0002](/rfcs/0002-cli-surface/) (`serve` flags, shared result shape)

---

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
server's 10-minute timeout are stopped the same way and reported as `failed`.

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

## 5. Determinism, Caching & Testing

Served runs use the same cache/provenance model as the CLI ([RFC-0001](/rfcs/0001-node-kinds-and-freshness/),
[RFC-0005](/rfcs/0005-artifact-serialization-and-storage/)) — the server adds no new
cache semantics, only a scheduling and polling layer on top. Independent scheduled runs
execute concurrently (bounded by a run pool sized to CPU count); their writes to the
shared `metadata.db` are serialized by a process-wide DB lock. Covered by
`barca-server`'s `cargo test` suite and the shell integration tests in
`tests/integration/`.

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
