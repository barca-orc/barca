---
title: 'RFC-0007: Server Mode — the CLI against a deployed barca'
description: 'Point barca at a deployed barca serve with one setting; every command runs there and prints here. Still one node.'
---

- **Status:** Under Review
- **Date:** 2026-10-03 (revised 2026-10-10)
- **Touches:** barca-cli | HTTP server | python/barca | barca-core
- **Prerequisites:** [#306](https://github.com/barca-orc/barca/pull/306) (SIGTERM, trigger errors, interrupted runs in containers) and [#272](https://github.com/barca-orc/barca/pull/272) (`serve --host`), both merged. Draining on shutdown ([#190](https://github.com/barca-orc/barca/issues/190)) is not a prerequisite; a refusal while shutting down is in scope (§4.5).
- **Supersedes / Related:** extends [RFC-0004](/rfcs/0004-http-server-api/) (lifts its v1 cuts: localhost-only, no streaming, memory-only run state, server-specific JSON); preserves the output contract of [RFC-0002](/rfcs/0002-cli-surface/) and `barca docs contract`. Implementation: [#393](https://github.com/barca-orc/barca/issues/393) (the run contract) and [#394](https://github.com/barca-orc/barca/issues/394) (the CLI client and parity suite).

---

## 1. Summary

A team deploys one `barca serve` and points every client at it with one setting: `[server] url`
in `barca.toml`, `BARCA_SERVER`, or `--server <url>`. In server mode the CLI (and `barca.api`,
which runs the CLI) sends each command to the server and prints the result exactly as a local
run would: the same stdout JSON, the same stderr lines, the same exit codes, and Ctrl-C cancels.

This is still one node. The server runs **its deployed code** on its own machine, with its own
workers, cache and history. Nothing is distributed and nothing is uploaded. The CLI is a remote
control for one `barca serve`.

One HTTP surface serves the CLI, the web UI and any other client. Every endpoint that mirrors a
command returns that command's `--json` body, built by the same Rust code, and errors carry the
CLI's error envelope.

### Non-goals

These were in an earlier revision of this RFC and are now standing non-goals, recorded in the
[Direction issue](https://github.com/barca-orc/barca/issues/395):

- **Experiments.** Uploading local `.py` files to run on the server in an isolated namespace.
  Running code sent by a client is a different product with its own isolation and authorization
  problems. A local edit is tried locally, or deployed.
- **Built-in authentication.** The server sits behind a proxy that authenticates
  ([Deploying](/deploying/#authentication)).
- **Several project roots per server.**
- **A worker pool shared across runs, or warm workers across runs.**

## 2. Motivation

Sharing today means [RFC-0006](/rfcs/0006-configuration-and-remote-state/)'s optimistic mode:
every machine pulls the whole metadata DB from object storage, runs, and pushes it back with a
conditional upload. It does not scale to a team:

- **Size.** About 380 bytes per materialization, never pruned. S3 and R2 single-request uploads
  cap the blob at 48 MiB, about 130k materializations. A daily 1,000-partition job reaches that in
  about four months, and before then every run, even a fully cached one, uploads the whole file.
- **Contention.** The conditional push is a global lock. Each conflict re-downloads the DB and
  replays. Fine for two or three machines, a conflict storm for many.
- **No single home for schedules.** Two machines running `barca serve` with one schedule both
  fire it.

`barca serve` already exposes the core commands over HTTP ([RFC-0004](/rfcs/0004-http-server-api/)),
and deployments exist ([Deploying](/deploying/)). A client deploying it asked to drive the
deployment from a laptop with the CLI they already use. Today they cannot:

- The CLI has no notion of a server.
- A run's result is a path on the server's disk.
- Run status lives in memory and is lost on restart. A failed run keeps only an error string;
  its steps, traceback and database run id are dropped. (`GET /runs` and `GET /runs/{id}` from
  [#343](https://github.com/barca-orc/barca/pull/343) now read history, but a run started before
  a restart still has no outcome.)
- The progress lines a local run prints (`--agent` lines, plan warnings, cached and completed
  steps) are written to the server's own stderr and never reach a client.
- Errors are `{"error": "..."}` and lose the CLI envelope's `kind`, `code`, `remediation`, `node`
  and `traceback`.
- The only client is `barca.Client`, a second Python surface that overlaps `barca.api`.

## 3. Guide-Level Explanation

### 3.1 CLI

Configure once:

```toml
# barca.toml
[server]
url = "https://barca.internal.example.com"
```

…or per shell (`export BARCA_SERVER=https://barca.internal.example.com`) or per command
(`barca get daily_report --server https://...`). Precedence is the usual flag > environment >
`barca.toml`; an empty `BARCA_SERVER=` counts as unset, as every barca variable does. `--local`
runs locally whatever is configured.

Then nothing changes:

```bash
barca get daily_report                 # runs on the server, streams progress to stderr here
barca get daily_report -o value        # prints the value
barca get daily_report --dry-run       # what would run, decided by the server
barca run deploy --refresh fetch       # a task run on the server
barca status daily_report              # cache state, from the server's DB
barca list                             # the server's deployed nodes
barca history -l 20                    # the server's run history (everyone's runs)
barca stats daily_report               # timings, from the server's DB
```

File arguments are optional in server mode: the server has its project. A file argument must
name a deployed file (relative to the root, as locally); one that does not is a usage error
listing the deployed files. The client never parses Python.

`serve`, `plan`, `sql`, `docs` and `version` always run locally. `--server` on them is a usage
error, so the answer to "did this run on the server?" is never ambiguous.

The server runs its deployed code. When a file named on the command line differs from the
deployed one, the client prints one note and continues:

```
[barca] note: pipelines/report.py differs from the deployment; the server runs the deployed version
```

To run a local edit, run it locally (`--local`), or deploy it.

### 3.2 Python API

`barca.api` runs the binary, so it follows the same configuration with no change:

```python
import barca
barca.get("daily_report")                      # local or remote, by [server] / BARCA_SERVER
```

`barca.Client` is deprecated in favour of `barca.api` (one canonical way): it warns on
construction for one minor release, then is removed.

### 3.4 HTTP API

`barca serve --host 0.0.0.0` (#272) makes the server reachable. With no authentication, every
start on a non-loopback address prints a warning; deploy behind a private network or an
authenticating, TLS-terminating proxy ([Deploying](/deploying/)).

All endpoints below are new or changed. Bodies are JSON; errors are the CLI envelope (§4.1).

**Runs**

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/runs` | Start a run. Body: `RunRequest` (below). `202 {"run_id"}`, or `200` with the `--dry-run` JSON when `dry_run` is set (nothing starts). |
| `GET` | `/runs/{run_id}` | The run: `{run_id, status, triggered_by, started_at, finished_at, outcome}`. Backed by the DB, so it survives a restart. |
| `GET` | `/runs/{run_id}/events` | Server-sent events with an `id:` on each; `Last-Event-ID` resumes. |
| `DELETE` | `/runs/{run_id}` | Cancel. |
| `GET` | `/runs/{run_id}/output` | The final artifact's bytes. `Content-Type` by format (`application/json`, `application/vnd.apache.parquet`, `application/octet-stream` for pickle). |

```json
// POST /runs
{
  "command": "get",                 // "get" | "run"
  "targets": ["daily_report"],      // as given on the command line; [] means every asset
  "files": [],                      // as typed, relative to the root; [] means the deployed set
  "refresh": [], "refresh_all": false, "cascade": true,
  "dry_run": false,
  "agent": true                     // which stderr lines the client will print (§4.2)
}
```

The run id is the database run id, the one `barca history` shows. There is no separate polling
handle.

Events (`data:` is JSON with a `type`):

| `type` | Payload | Meaning |
|---|---|---|
| `run_started` | `run_id` | |
| `line` | `line`, `when: "agent" \| "always"` | A stderr line exactly as a local run prints it: `--agent` step lines, plan warnings, `[barca]` notes, the end-of-run line. |
| `log` | `node_id`, `line` | A line your step printed. |
| `step_finished` | `node_id`, `ok`, `elapsed_seconds`, `error` | As today; now also for cached steps (`cached: true`). |
| `run_finished` | `run_id`, `outcome` | Terminal. `outcome` is the run's full result (below). |

`outcome` is tagged by `status`:

- `success`: the exact `barca get --json` / `barca run --json` body, with `final_output` inlined
  as the CLI inlines it (json values inline, other formats as a pointer). A json value larger
  than 8 MiB is a pointer whose `url` is `/runs/{run_id}/output`.
- `failed`: the CLI's failed-run stdout body (`failed_node`, `error`, `steps`, ...) plus
  `envelope`, the stderr error envelope.
- `cancelled`, `error`: `envelope` only.

**Inspection.** The body is the CLI command's `--json` output, byte for byte, built by the same
function:

| Method | Path | Same as |
|---|---|---|
| `GET` | `/list` | `barca list --json --all` (the client applies `--limit` and `--fields`) |
| `GET` | `/nodes?targets=a,b&sample=N` | `barca status --json` |
| `GET` | `/history?limit=N&all=1` | `barca history --json` |
| `GET` | `/stats/{name}` | `barca stats --json` |
| `GET` | `/project` | The deployed tree: `{api_version, root, env, deployed_at, commit, files: [{path, sha256}]}` |
| `GET` | `/health` | Adds `api_version` and `env` |

**Existing routes.** `POST /run`, `POST /run/{target}`, `POST /get/{target}`,
`GET /status/{id}`, `GET /events/{id}`, `GET /assets`, `GET /assets/{name}` stay for one minor
release as aliases implemented on the new handlers, then are removed. `GET /assets` moves to the
CLI's freshness shape and `GET /assets/{name}` to the CLI's `id` key in the same release (the
divergence recorded in `barca docs contract`). The web UI moves to the new routes in the same
release.

### 3.5 Dev server / `--watch` / UI

`--watch` re-hashes the deployed tree for `/project` on every reload. The UI shows
`triggered_by` in run lists.

---

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Config:** `[server] url` in `barca.toml`; `BARCA_SERVER`; `--server <url>` and `--local` on
  `get`, `run`, `status`, `list`, `history`, `stats`. All enter the contract as experimental.
- **Resolution:** `--local` > `--server` > `BARCA_SERVER` (non-empty) > `[server] url`. The
  resolved URL is printed once per command on stderr when it comes from the config or the
  environment (`[barca] server https://...`), so a remote run is never silent.
- **Output parity is the contract.** For every command, server mode produces the same stdout JSON
  key for key, the same stderr lines in the same order, and the same exit codes (0, 1 step
  failed, 2 usage, 3 infra, 130 cancelled) as a local run against the same project and DB state.
  Declared differences, which the parity tests normalize and the manual states:
  - `root`, artifact `path` and `artifact_dir` are the server's paths.
  - The "no .py files" usage error does not occur; the server has the files.
  - A local terminal run shows a progress bar; a client shows the lines a bar would leave behind
    (warnings, notes, the end-of-run line) and, with `--agent`, the agent lines.
  - `history`, `stats` and `status` reflect the server's DB: everyone's runs.
- **Transport failures** (server unreachable, unexpected response, `api_version` major mismatch)
  are `kind: infra`, exit 3, with a message naming the server URL and a remediation of
  `--local`.
- **Error envelope over HTTP.** Every error body is the stderr envelope
  `{error, code, kind, remediation, node?, traceback?, artifact_dir?}`. HTTP status follows the
  kind: `usage` 400 (404 for an unknown node, 409 for an ambiguous one), `infra` 500, refused
  while shutting down 503. A failed step is never an HTTP error: it is a run whose
  `outcome.status` is `failed`. Remediation text names files as the user typed them, because the
  request carries them that way.
- **Versioning:** `api_version: 1` in `/health` and `/project`. The client refuses a different
  major. Additive fields do not bump it.
- **DB:**
  - `runs` gains `triggered_by` (`cli`, `api`, `schedule`) and `outcome_json` (the serialized
    `outcome`, so a finished run's result survives a restart).
  - `barca history --json` rows gain `triggered_by` (additive).
- **Retired:** the contract's exception that `barca serve` JSON is the engine's own
  serialization.

### 4.2 Implementation Details

**One rendering path.** The code that turns a run's result into stdout JSON and the error
envelope already live in `barca-core::report` and `barca-core::envelope`
([#339](https://github.com/barca-orc/barca/pull/339)). The server builds response bodies with
these functions, and the client renders a received `outcome` with them.

**Progress as events.** Today `execute` writes progress with `eprintln!` at about a dozen sites.
They go through one `Reporter` that either prints (local, with or without the bar, byte for byte
as today) or emits `line` events (server). Each line carries `when`: `agent` for lines a local
run prints only with `--agent`, `always` for the rest; the client prints by its own mode. Cached
steps emit `step_finished` with `cached: true`. The server's event channel numbers events, keeps
the backlog for the run's lifetime, and re-syncs a lagging subscriber from the backlog instead of
dropping events.

**DB-backed runs.** `execute` takes an options struct (`run_id`, `triggered_by`) instead of its
growing positional list. The server allocates the run id up front, so the id it returns is the
database id. The in-memory map stays as a cache for live runs; `/runs/{id}` falls through to the
DB.

**Client.** `--server` resolution happens in `main()` before `enter_project_root`. In server
mode the client still finds the root and rebases file arguments (to send files as typed) but
skips discovery. A new `barca-cli/src/client.rs` holds the HTTP flow on `reqwest` (rustls, no
default features). The binary grows by about 2 to 3 MB; the PR states the measured number.

`get`/`run` in server mode:

1. `GET /health`; check `api_version`.
2. `POST /runs`.
3. Stream `/runs/{id}/events`: print `line`s filtered by mode, `log` lines as the local
   coordinator prints them.
4. On `run_finished`, render `outcome` with the shared report functions and exit with its code.
5. A dropped stream reconnects with `Last-Event-ID`. If the stream is gone, `GET /runs/{id}`
   gives the `outcome`.

Ctrl-C sends `DELETE /runs/{id}` and keeps streaming until `cancelled`; a second Ctrl-C exits
130 at once, as a local run does.

### 4.3 Rust ↔ Python Boundary

The worker protocol does not change. Workers run only on the server, over the existing UDS. The
client never imports or parses user code.

### 4.4 Node-Kind Semantics

Unchanged.

### 4.5 Edge Cases

- **Local edits.** The server runs the deployed code. A named file that differs locally prints
  the note in §3.1. It never refuses.
- **One server per DB.** `barca serve` holds a lifetime lock on `.barca/serve.lock`; a second
  `serve` on the same DB exits with `kind: infra` naming the holder. It does not hold the DB
  itself (that would bring back the bug #136 fixed); DB access keeps #136's short per-operation
  lock.
  - A `barca --local ...` run on the server host is safe and still works.
  - Such a run is invisible to the server: it does not join in-flight duplicates and is not
    cancelled at shutdown. This is documented, not forbidden.
- **Shared remote state.** `serve` keeps refusing `state = "optimistic"`: the server is the
  shared state. `BARCA_STATE=off` ([#353](https://github.com/barca-orc/barca/pull/353)) lets one
  `barca.toml` serve laptops and servers.
- **Shutdown.** While shutting down the server answers new runs with `503` and the client exits 3
  naming the server. Streaming clients get the run's terminal `cancelled` outcome and exit 130.
  Draining in-flight runs before stopping is #190.
- **`--env`** must equal the server's environment in this RFC; another is a usage error naming
  the server's.
- **Large outputs.** `-o value` on a pointer downloads `/runs/{id}/output` and deserializes it
  with the same reader `barca.api` uses.

## 5. Determinism, Caching & Testing

No cache-key change. Server mode moves where commands run. Run hashes are computed exactly as
today.

- **Parity suite:** the contract test's case table runs once locally and once through a served
  copy of the same project, in the same order. Normalized stdout JSON, the stderr line sequence
  and exit codes must be equal. This is the test that keeps "nothing changes" true.
- **Resilience:** dropped stream mid-run, server restart mid-run (`/runs/{id}` from the DB),
  Ctrl-C, `503` during shutdown.
- **HTTP shapes:** every route in `crates/barca-server/tests/api.rs` with the in-process router,
  and the OpenAPI conformance test in `crates/barca-server/tests/openapi.rs`.
- `benchmarks/trivial`: local runs must not regress (the `Reporter` sits on the hot path).

## 6. Performance

Local mode gains one config lookup. In server mode the client does not parse or plan; a fully
cached `barca get` through a server on the same host should be within about 20 ms of the local
run, measured with `benchmarks/trivial` once client mode exists.

## 7. Drawbacks

- Every command gains a second path. The parity suite is required, not optional.
- Changes reach the server through whatever deploys it (git pull with `--watch`, a container
  rebuild). There is no way to try a local edit against the server's cache without deploying it.
  That is deliberate (§1, non-goals).
- The server is a single point of failure for shared runs; local mode still works.

## 8. Rationale & Alternatives

- **Refuse to run when local files differ** (this RFC's first draft). Safe, but uncommitted
  edits blocked every remote command, including fetching a deployed value. Running the deployed
  code and printing a note is both more normal and more useful.
- **Experiments: upload local code and run it in an isolated namespace** (this RFC's second
  draft). Dropped. It turns the server into a place that runs code it is sent, which needs
  isolation (overlays, symlinks, shared resources) and authorization that barca does not have
  and does not want to own. A single node that runs its deployed code is the whole product.
- **Blob-synced DB (RFC-0006).** Zero-ops, but its size and contention limits make it a
  small-team mode. Kept for teams without a server (§10).
- **Bucket-native cache index** (create-only index objects per `(node, run_hash)`). Keeps
  zero-ops and scales writers, but needs batching, merge-on-read history and GC, and still leaves
  schedules homeless. A possible future for serverless sharing.
- **`--remote` as the flag name.** `[remote]` already means artifact and state storage; one word
  would mean two unrelated things in one file.

## 9. Prior Art

Prefect points its CLI and SDK at a server with `PREFECT_API_URL` and an API key: one setting,
then business as usual. Dagster's CLI and webserver work against a deployed code location.
Airflow exposes a REST API beside its CLI.

## 10. Unresolved Questions

- Whether RFC-0006's optimistic mode stays for server-less teams or is deprecated in favour of
  server mode (one canonical way to share).

## 11. Future Possibilities

- A read-through cache for local runs: a local `barca get` consults the server's cache index and
  fetches hits, then runs only what is missing.
