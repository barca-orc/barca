---
title: 'RFC-0007: Server Mode — the server is the ledger'
description: 'One deployed barca serve owns the record of what exists. Laptops run their own work against it, or send a run to it. Still one node per run.'
---

- **Status:** Accepted
- **Date:** 2026-10-03 (revised 2026-10-10, accepted 2026-10-14)
- **Touches:** barca-cli | HTTP server | python/barca | barca-core
- **Prerequisites:** [#306](https://github.com/barca-orc/barca/pull/306) (SIGTERM, trigger errors, interrupted runs in containers) and [#272](https://github.com/barca-orc/barca/pull/272) (`serve --host`), both merged. [RFC-0009](/rfcs/0009-store-backup-and-restore/) (the store as backup, content digests) ships first or together. Draining on shutdown ([#190](https://github.com/barca-orc/barca/issues/190)) is not a prerequisite; a refusal while shutting down is in scope (§4.5).
- **Supersedes / Related:** extends [RFC-0004](/rfcs/0004-http-server-api/) (lifts its v1 cuts: localhost-only, no streaming, memory-only run state, server-specific JSON); supersedes the sharing half of [RFC-0006](/rfcs/0006-configuration-and-remote-state/) (optimistic mode); preserves the output contract of [RFC-0002](/rfcs/0002-cli-surface/) and `barca docs contract`. Implementation: [#393](https://github.com/barca-orc/barca/issues/393) (the run contract), [#394](https://github.com/barca-orc/barca/issues/394) (the CLI client and parity suite) and the ledger issue filed with this revision. Decisions recorded in [DIRECTION.md](https://github.com/barca-orc/barca/blob/main/DIRECTION.md#decided-october-2026).

---

## 1. Summary

A team deploys one `barca serve` and points every client at it with one setting: `[server] url`
in `barca.toml`, `BARCA_SERVER`, or `--server <url>`. **The server is the ledger.** It owns the
one record of which results exist (run hashes, artifact paths, run history, schedules), it holds
the deployed copy of the pipeline code, and it is the only process that writes that record.
Everyone who points at it gets the same answer to "is this computed?".

Work can happen in two places, and both write to the same ledger:

- **Locally, by default.** `barca get` on a laptop asks the server which of the run hashes it
  needs already exist, fetches those artifacts from the store, computes the rest on the laptop,
  uploads the new artifacts to the store, and then posts the rows to the server. The server
  verifies each object before it accepts the row.
- **On the server, with `--server`.** The command runs on the server's machine with its deployed
  code and workers, and prints here exactly as a local run would: the same stdout JSON, the same
  stderr lines, the same exit codes, and Ctrl-C cancels.

Every run is still one machine. The server never splits a run, never runs code it is sent, and
never proxies artifact bytes. It arbitrates what exists. One ledger, many single-node runs.

### Non-goals

Standing non-goals, recorded in the [Direction issue](https://github.com/barca-orc/barca/issues/395):

- **Sharing state without a server.** Two machines writing one bucket is not supported. A store
  with no server is one machine's backup and read-through cache ([RFC-0009](/rfcs/0009-store-backup-and-restore/)).
- **Distributing one run across machines.** The server does not schedule laptops, and laptops do
  not take work from a queue.
- **Experiments.** Uploading local `.py` files to run on the server. A local edit runs locally, or
  is deployed.
- **Built-in authentication.** The server sits behind a proxy that authenticates
  ([Deploying](/deploying/#authentication)).
- **Several project roots per server.** A worker pool shared across runs, or warm workers.

## 2. Motivation

Sharing today means [RFC-0006](/rfcs/0006-configuration-and-remote-state/)'s optimistic mode:
every machine pulls the whole metadata DB from object storage, runs, and pushes it back with a
conditional upload. It does not scale to a team, and it has no owner:

- **No single home for truth.** Two machines that both think they own the blob disagree, and the
  conditional push turns that into a conflict storm or, with a corrupt blob ([#243](https://github.com/barca-orc/barca/issues/243)),
  into every machine losing its history.
- **Size.** About 380 bytes per materialization, never pruned. S3 and R2 single-request uploads
  cap the blob at 48 MiB, and before then every run, even a fully cached one, uploads the whole
  file.
- **No single home for schedules.** Two machines running `barca serve` with one schedule both
  fire it.

`barca serve` already exposes the core commands over HTTP ([RFC-0004](/rfcs/0004-http-server-api/)),
and deployments exist ([Deploying](/deploying/)). The two things people asked for are "let me run
barca on the server from my laptop" and "let two of us agree on what is computed". One process
that owns the ledger answers both.

## 3. Guide-Level Explanation

### 3.1 CLI

Configure once:

```toml
# barca.toml
[server]
url = "https://barca.internal.example.com"

[remote]
uri = "s3://team-bucket/barca"      # the same store the server uses
```

…or per shell (`export BARCA_SERVER=https://barca.internal.example.com`). Precedence is flag >
environment > `barca.toml`; an empty `BARCA_SERVER=` counts as unset, as every barca variable does.

A laptop needs both: the server URL for the ledger and the store credentials for the bytes. The
server never proxies artifacts, so a client without store access gets a usage error naming the
missing `[remote]` section before anything runs.

With a server configured, nothing about the commands changes. What changes is where the answer
comes from:

```bash
barca get daily_report                 # runs here; cache hits come from the ledger and the store
barca get daily_report -o value        # prints the value
barca get daily_report --dry-run       # what would run, decided against the server's ledger
barca get daily_report --server        # runs on the server, streams progress to stderr here
barca run deploy --refresh fetch       # a task, run here, recorded on the server
barca status daily_report              # ledger state, with this machine's local copy noted
barca list                             # the deployed nodes, from the server
barca history -l 20                    # everyone's runs
barca stats daily_report               # timings, from the ledger
barca get daily_report --local         # this machine's ledger only; nothing is read from or
                                       # written to the server
```

`--server` takes an optional URL; bare `--server` uses the configured one and is a usage error
when none is configured. `--local` ignores the server entirely, including for cache lookups, and
records nothing on it.

The first time a command touches the configured server it prints one stderr line,
`[barca] server https://...`, so a shared run is never silent.

**Local runs and the deployed code.** A laptop runs its own files. Its run hashes cover its own
code, so a laptop editing `report.py` gets cache misses for the nodes it changed and hits for the
rest, and the rows it posts are keyed by its hashes. Two people with the same code agree
automatically; two people with different code have different entries. The ledger holds no code.

**`--server` runs and the deployed code.** The server runs its deployed files. When a file named
on the command line differs from the deployed one, the client prints one note and continues:

```
[barca] note: pipelines/report.py differs from the deployment; the server runs the deployed version
```

File arguments are optional for a `--server` run (the server has its project). One that does not
name a deployed file is a usage error listing the deployed files.

`serve`, `plan`, `sql`, `docs` and `version` always run on this machine. `--server` on them is a
usage error. `barca sql` reads this machine's artifacts; with a server it reads what has been
fetched or computed here, which `barca status` shows.

### 3.2 Python API

`barca.api` runs the binary, so it follows the same configuration with no change:

```python
import barca
barca.get("daily_report")                      # local run against the server's ledger
barca.get("daily_report", server=True)         # run on the server
```

`barca.Client` is deprecated in favour of `barca.api` (one canonical way): it warns on
construction for one minor release, then is removed.

### 3.4 HTTP API

`barca serve --host 0.0.0.0` (#272) makes the server reachable. With no authentication, every
start on a non-loopback address prints a warning; deploy behind a private network or an
authenticating, TLS-terminating proxy ([Deploying](/deploying/)).

All endpoints below are new or changed. Bodies are JSON; errors are the CLI envelope (§4.1).

**The ledger.** What a local run talks to.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/ledger/lookup` | Body `{"hashes": [{"node_id", "run_hash"}]}`. Answer: for each pair, `{"exists", "path", "format", "size_bytes", "digest"}` or `exists: false`. One round trip per command. |
| `POST` | `/ledger/runs` | Body: one finished local run, the `runs` row plus its step rows, as the CLI's `--json` body carries them, with each artifact's `path` in the store and its content `digest`. The server checks that every object exists at its path with that digest, then writes the rows and answers `201 {"run_id"}`. A missing or mismatched object is `409` with an envelope naming the node; nothing from that run is recorded. |

The lookup is by run hash only. The server does not care which code produced the hash, which is
why laptops with different edits coexist in one ledger.

**Runs.** What a `--server` run talks to.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/runs` | Start a run on the server. Body: `RunRequest` (below). `202 {"run_id"}`, or `200` with the `--dry-run` JSON when `dry_run` is set (nothing starts). |
| `GET` | `/runs/{run_id}` | The run: `{run_id, status, triggered_by, started_at, finished_at, outcome}`. Backed by the DB, so it survives a restart. |
| `GET` | `/runs/{run_id}/events` | Server-sent events with an `id:` on each. |
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
| `log` | `node_id`, `line` | A line your step printed, which the client prints prefixed with the node id as a local run does. |
| `step_finished` | `node_id`, `ok`, `elapsed_seconds`, `error` | As today; now also for cached steps (`cached: true`). |
| `run_finished` | `run_id`, `outcome` | Terminal. `outcome` is the run's full result (below). |

`outcome` is tagged by `status`:

- `success`: the exact `barca get --json` / `barca run --json` body. Artifact paths are the
  store paths, which the client can fetch with the same reader `-o value` uses locally.
- `failed`: the CLI's failed-run stdout body (`failed_node`, `error`, `steps`, ...) plus
  `envelope`, the stderr error envelope.
- `cancelled`, `error`: `envelope` only.

**Inspection.** The body is the CLI command's `--json` output, byte for byte, built by the same
function:

| Method | Path | Same as |
|---|---|---|
| `GET` | `/list` | `barca list --json --all` (the client applies `--limit` and `--fields`) |
| `GET` | `/nodes?targets=a,b&sample=N` | `barca status --json` for the ledger; the client adds this machine's local state |
| `GET` | `/history?limit=N&all=1` | `barca history --json` |
| `GET` | `/stats/{name}` | `barca stats --json` |
| `GET` | `/project` | The deployed tree: `{api_version, root, env, deployed_at, commit, files: [{path, sha256}]}` |
| `GET` | `/health` | Adds `api_version`, `env` and the store `uri` the server writes to, so a client can refuse a mismatched `[remote]` |

**Existing routes.** `POST /run`, `POST /run/{target}`, `POST /get/{target}`,
`GET /status/{id}`, `GET /events/{id}`, `GET /assets`, `GET /assets/{name}` stay for one minor
release as aliases implemented on the new handlers, then are removed. `GET /assets` moves to the
CLI's freshness shape and `GET /assets/{name}` to the CLI's `id` key in the same release (the
divergence recorded in `barca docs contract`). The web UI moves to the new routes in the same
release.

### 3.5 Dev server / `--watch` / UI

`--watch` re-hashes the deployed tree for `/project` on every reload. The UI shows
`triggered_by` in run lists, including `client` for rows a laptop posted.

---

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Config:** `[server] url` in `barca.toml`; `BARCA_SERVER`; `--server [<url>]` and `--local`
  on `get`, `run`, `status`, `list`, `history`, `stats`. All enter the contract as experimental.
- **Resolution of the ledger:** `BARCA_SERVER` (non-empty) > `[server] url`. `--server <url>`
  overrides it for one command. `--local` disables it for one command.
- **Where a run happens:** on this machine unless `--server` is given. `--local` and `--server`
  together are a usage error.
- **What a local run does with a configured server**, in order:
  1. `GET /health`; check `api_version` and that the server's store `uri` equals this machine's
     `[remote] uri`. A mismatch is a usage error naming both.
  2. Plan as today. `POST /ledger/lookup` with every run hash in the cone. A hit whose artifact
     this machine does not have is fetched from the store by the transfer helper before the step
     that reads it, as cache hits from other machines are fetched today. A hit this machine has
     is compared by digest; the server's copy wins on a mismatch ([RFC-0009](/rfcs/0009-store-backup-and-restore/)).
  3. Run the misses here. Upload their artifacts as today, and wait for upload confirmation at
     run end as today.
  4. `POST /ledger/runs` with the run and its rows. On `201`, the local database records the run
     as shared. On `409` or a transport failure, the run is still a success here: it exits as it
     would have, prints a warning naming what was not recorded and why, and the rows stay local
     as `unsynced`. The next command against the server retries them before its own lookup
     ([#390](https://github.com/barca-orc/barca/issues/390) is this fail-soft rule).
- **The local database with a server** is this machine's record: its runs, its copies, and the
  rows it has not yet posted. `barca history` and `barca stats` with a server configured answer
  from the server. `barca status` answers from the server and annotates each node with whether a
  copy is here (`local: true`), so `barca sql` users can see what they can query.
- **Output parity is the contract** for `--server` runs. For every command, a server run
  produces the same stdout JSON key for key, the same stderr lines in the same order, and the
  same exit codes (0, 1 step failed, 2 usage, 3 infra, 130 cancelled) as a local run against the
  same project and ledger. Declared differences, which the parity tests normalize and the manual
  states:
  - `root`, artifact `path` and `artifact_dir` are the server's paths.
  - The "no .py files" usage error does not occur; the server has the files.
  - A local terminal run shows a progress bar; a client shows the lines a bar would leave behind
    (warnings, notes, the end-of-run line) and, with `--agent`, the agent lines.
- **Transport failures.** A configured server that cannot be reached, answers unexpectedly, or
  reports a different `api_version` major is `kind: infra`, exit 3, with a message naming the
  server URL and which of the two endpoints (server or store) failed, and a remediation of
  `--local`. There is no silent fallback to a local-only run: two people stopping to agree without
  noticing is the failure the server exists to prevent.
- **A stream dropped mid `--server` run** is `kind: infra`, exit 3, naming the run id, with a
  remediation of `barca history <run_id>`. The run continues on the server. Resuming the stream
  with `Last-Event-ID` is §11.
- **Error envelope over HTTP.** Every error body is the stderr envelope
  `{error, code, kind, remediation, node?, traceback?, artifact_dir?}`. HTTP status follows the
  kind: `usage` 400 (404 for an unknown node, 409 for an ambiguous one or a rejected ledger
  post), `infra` 500, refused while shutting down 503. A failed step is never an HTTP error: it is
  a run whose `outcome.status` is `failed`.
- **Versioning:** `api_version: 1` in `/health` and `/project`. The client refuses a different
  major. Additive fields do not bump it.
- **DB:**
  - `runs` gains `triggered_by` (`cli`, `api`, `schedule`, `client`) and `outcome_json` (the
    serialized `outcome`, so a finished run's result survives a restart). A row posted by a
    laptop carries `triggered_by: client` and the laptop's run id in `client_run_id`.
  - `materializations` rows carry the content `digest` from RFC-0009; the ledger post requires it.
  - `barca history --json` rows gain `triggered_by` (additive).
- **Retired:** the contract's exception that `barca serve` JSON is the engine's own
  serialization; `[remote] state = "optimistic"` and `BARCA_STATE` (RFC-0009 has the Breaking
  line).

### 4.2 Implementation Details

**One rendering path.** The code that turns a run's result into stdout JSON and the error
envelope already live in `barca-core::report` and `barca-core::envelope`
([#339](https://github.com/barca-orc/barca/pull/339)). The server builds response bodies with
these functions, and the client renders a received `outcome` with them.

**The ledger is the existing cache lookup behind HTTP.** `cache.rs` already answers "does a
result exist for this run hash". `/ledger/lookup` calls it; the client's planner calls the
lookup where it would have called `cache.rs` directly. The ledger post reuses `persist.rs`'s
final write with the verification step in front of it.

**Verification before acceptance.** For each row the server does one `HEAD` (or equivalent)
against the store for the object's size and checksum where the backend offers one, and compares
it with the posted `digest`; a backend without checksums falls back to size and the digest in the
receipt sidecar the upload writes (#381). Verification is per object and bounded, so a large
run's post is a few hundred metadata requests, never a download.

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

**Client.** `--server` and the ledger URL resolve in `main()` before `enter_project_root`. A new
`barca-cli/src/client.rs` holds the HTTP flow on `reqwest` (rustls, no default features). The
binary grows by about 2 to 3 MB; the PR states the measured number.

`get`/`run` with `--server`:

1. `GET /health`; check `api_version`.
2. `POST /runs`.
3. Stream `/runs/{id}/events`: print `line`s filtered by mode, `log` lines as the local
   coordinator prints them.
4. On `run_finished`, render `outcome` with the shared report functions and exit with its code.

Ctrl-C sends `DELETE /runs/{id}` and keeps streaming until `cancelled`; a second Ctrl-C exits
130 at once, as a local run does.

### 4.3 Rust ↔ Python Boundary

The worker protocol does not change. A `--server` run's workers run on the server over the
existing UDS; a local run's workers run here. The transfer helper gains nothing: it already
uploads this machine's artifacts and fetches other machines' hits by path.

### 4.4 Node-Kind Semantics

Unchanged. Sensors and tasks are never cache hits, so a laptop's sensor is polled on the laptop,
and a `--server` run's sensor on the server.

### 4.5 Edge Cases

- **Local edits.** A local run uses local code and gets local hashes. A `--server` run uses the
  deployed code; a named file that differs prints the note in §3.1 and nothing is refused.
- **Two laptops post the same hash.** The second post finds the row present. If the digests
  agree it is a no-op; if they differ the server keeps its row, answers `201` with
  `bytes_changed: true` for that node, and the client prints the non-determinism warning from
  [#392](https://github.com/barca-orc/barca/issues/392). The server's copy stays the truth.
- **One server per DB.** `barca serve` holds a lifetime lock on `.barca/serve.lock`; a second
  `serve` on the same DB exits with `kind: infra` naming the holder. It does not hold the DB
  itself (that would bring back the bug #136 fixed); DB access keeps #136's short per-operation
  lock. A `barca --local ...` run on the server host is safe, and invisible to the server.
- **The server's own store is its backup.** The server backs up its database on the RFC-0009
  cadence like any other owner. `barca state restore` on the server is the recovery path for
  the ledger.
- **Shutdown.** While shutting down the server answers new runs and ledger posts with `503`;
  the client exits 3 naming the server, and a local run keeps its rows as `unsynced`. Streaming
  clients get the run's terminal `cancelled` outcome and exit 130. Draining in-flight runs before
  stopping is #190.
- **`--env`** must equal the server's environment; another is a usage error naming the server's.
- **Large outputs.** `-o value` on a `--server` run's pointer downloads the artifact from the
  store with the same reader `barca.api` uses.

## 5. Determinism, Caching & Testing

No cache-key change. Run hashes are computed exactly as today, on whichever machine runs.

- **Parity suite:** the contract test's case table runs once locally and once through a served
  copy of the same project, in the same order. Normalized stdout JSON, the stderr line sequence
  and exit codes must be equal. This is the test that keeps "nothing changes" true.
- **Ledger suite:** two project copies against one server. Copy A computes, copy B gets a hit
  and fetches; B edits one node and gets exactly one miss; A and B post the same hash with
  different bytes and the warning fires once; a post with a missing object is `409` and the row
  stays `unsynced` locally and is retried by the next command; a server down mid-post leaves
  the run successful and the rows `unsynced`; `--local` touches nothing on the server.
- **Resilience:** dropped stream mid-run (exit 3 naming the run id), server restart mid-run
  (`/runs/{id}` from the DB), Ctrl-C, `503` during shutdown.
- **HTTP shapes:** every route in `crates/barca-server/tests/api.rs` with the in-process router,
  and the OpenAPI conformance test in `crates/barca-server/tests/openapi.rs`.
- `benchmarks/trivial`: local runs must not regress (the `Reporter` sits on the hot path).

## 6. Performance

A local run with a server configured gains one `/health` and one `/ledger/lookup` round trip
and, at the end, one post. A fully cached `barca get` against a server on the same host should be
within about 20 ms of a local run, measured with `benchmarks/trivial` once client mode exists.
The lookup is one request for the whole cone, never one per node.

## 7. Drawbacks

- Every command gains a second path. The parity suite and the ledger suite are required, not
  optional.
- A laptop needs two credentials, the server URL and the store. That is one more thing a new
  team member must get right; the usage errors in §3.1 and §4.1 name exactly which is missing.
- Changes reach the server through whatever deploys it (git pull with `--watch`, a container
  rebuild). There is no way to run a local edit on the server; there is every way to run it
  locally against the server's ledger, which is the common case.
- The server is a single point of failure for shared runs. `--local` still works, and says so.

## 8. Rationale & Alternatives

- **Every run on the server** (this RFC's third draft). Simple, but it made the server the only
  place work could happen, and the pipeline author's edit loop went through a deployment. Local
  by default with a shared ledger keeps the fast loop and still gives everyone the same answer.
- **Proxy artifact bytes through the server** so a laptop needs only the URL. Rejected: it puts
  gigabytes through a small axum process that otherwise handles metadata, and object stores
  already do this job with credentials people have.
- **Let the server discover artifacts by hash path** instead of taking a post. Rejected: the
  ledger would then be "whatever is in the bucket", with no run history, no verification, and no
  way to tell an interrupted upload from a result.
- **A bucket-native ledger without a server** (create-only index objects per `(node, run_hash)`).
  Keeps zero-ops, but needs batching, merge-on-read history and GC, and still leaves schedules
  homeless. Sharing means a server; there is no other clean way, and a store on its own is a
  backup (RFC-0009).
- **Experiments: upload local code and run it in an isolated namespace** (second draft). Dropped.
  A single node that runs its deployed code is the whole product.
- **`--remote` as the flag name.** `[remote]` already means the store; one word would mean two
  unrelated things in one file.

## 9. Prior Art

Prefect points its CLI and SDK at a server with `PREFECT_API_URL`: one setting, then business as
usual, and the server is the record of flow runs while the work happens wherever the agent is.
Dagster's webserver works against a deployed code location. Nix and Bazel remote caches are the
closer analogy for the ledger: a content-addressed store plus one index of what exists, with
clients that build locally and push results.

## 10. Unresolved Questions

None at acceptance. The questions the earlier revisions carried are answered in the Direction
file's "Decided" section.

## 11. Future Possibilities

- Resuming a dropped `--server` stream with `Last-Event-ID` instead of exiting 3.
- `barca status` showing, per node, which machine posted the current row.
- A garbage-collection command for store objects no ledger row references (`barca gc`, parked).
