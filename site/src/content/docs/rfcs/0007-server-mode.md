---
title: 'RFC-0007: Server Mode — the CLI and Python API against a deployed barca'
description: 'Point barca at a deployed server with one setting; every command behaves as it does locally.'
---

- **Status:** Draft
- **Date:** 2026-10-03
- **Touches:** barca-cli | HTTP server | python/barca | barca-core
- **Prerequisite:** [#190](https://github.com/barca-orc/barca/issues/190) — server lifecycle (graceful drain, forced stop, crash recovery). Server mode is only safe to deploy on top of it.
- **Supersedes / Related:** extends [RFC-0004](/rfcs/0004-http-server-api/) (lifts its v1 cuts: localhost-only, no streaming, memory-only run state — authentication stays out of scope, [#187](https://github.com/barca-orc/barca/issues/187)); revises the sharing story of [RFC-0006](/rfcs/0006-configuration-and-remote-state/); builds on [RFC-0002](/rfcs/0002-cli-surface/) (the output contract server mode must preserve)

---

## 1. Summary

A team shares barca by deploying one `barca serve` instance and pointing every client at
it with a single setting — `[server] url` in `barca.toml`, `BARCA_SERVER`, or
`--server <url>`. In server mode the CLI and `barca.api` send each command to the server
instead of executing locally, and present the result exactly as a local run would: same
stdout JSON, same stderr progress, same exit codes, Ctrl-C cancels. The server runs **its
deployed project** (not the caller's local files), owns the metadata DB on its own disk,
and keeps artifacts in the configured store. Shared state stops being a synchronized file
and becomes "the server's state".

## 2. Motivation

Sharing today means [RFC-0006](/rfcs/0006-configuration-and-remote-state/)'s optimistic
mode: every machine pulls the whole metadata DB from object storage, runs, and pushes it
back with a conditional upload. It does not scale to a team:

- **Size.** Measured: ~380 bytes per materialization, never pruned. S3/R2 single-request
  uploads cap the blob at 48 MiB ≈ 130k materializations — a daily 1,000-partition job
  reaches it in ~4 months, after which every push fails. Before that, every run (even a
  fully cached one) uploads the whole file.
- **Contention.** The conditional push is a global lock; each conflict re-downloads the
  whole DB and replays. Fine for two or three machines, a conflict storm for many.
- **No single home for schedules.** Two people running `barca serve` with the same
  schedule both execute it.

Meanwhile `barca serve` already exposes the core commands over HTTP
([RFC-0004](/rfcs/0004-http-server-api/)) but can't be used by a team: it binds to
`127.0.0.1` only, has no authentication, reports outputs as paths on the server's disk,
keeps run status in memory, and has no CLI client — only `barca.Client`, a second Python
surface that overlaps `barca.api`.

A deployed server with one writer to the DB removes the sync problem entirely, and a CLI
client mode makes using it "business as usual".

## 3. Guide-Level Explanation

### 3.1 CLI

Configure once:

```toml
# barca.toml
[server]
url = "https://barca.example.com"
```

…or per shell (`export BARCA_SERVER=https://barca.example.com`) or per command
(`barca --server https://barca.example.com get daily_report`). Precedence is the usual
flag > env > `barca.toml`. There is no authentication in this RFC (see §3.4 and
[#187](https://github.com/barca-orc/barca/issues/187)).

Then nothing changes:

```bash
barca get daily_report                 # runs on the server, streams progress here
barca get daily_report -o value        # prints the value (downloaded from the server)
barca run deploy --refresh fetch       # task run on the server
barca status daily_report              # cache state of nodes, from the server's DB
barca get daily_report --dry-run       # what would run, decided by the server
barca list                             # the server's deployed definitions
barca history -l 20                    # the server's run history (everyone's runs)
barca stats daily_report               # timing/cache stats from the server's DB
```

File arguments are optional in server mode — the server already has its project. If given,
they must name files in the deployed project. Because the server runs the deployed code,
a command **refuses** to run remotely when your local copy of the project differs from
what's deployed (see §4.5) — it would otherwise silently run code you aren't looking at:

```
$ barca get daily_report
error: local files differ from the version deployed on https://barca.example.com
       (deployed 2026-10-02 14:03, commit 2dcfb19):
         pipeline.py
       The server runs the deployed code. Deploy your change, or run locally with --local.
$ echo $?
2
```

`--server` is ignored by commands that are inherently local: `serve`, `docs`, `version`.
To run locally while a server is configured: `barca --local get ...` (or
`BARCA_SERVER=` empty).

### 3.2 Python API

`barca.api` shells out to the binary, so it follows the same configuration with no API
change:

```python
import barca
barca.get("daily_report")     # local or remote, depending on [server] / BARCA_SERVER
```

`barca.Client` is deprecated in favor of `barca.api` (one canonical way): it stays for one
minor release, warning on construction, then is removed.

### 3.4 HTTP API

Server side, `barca serve` gains a bind address:

```bash
barca serve pipeline.py --host 0.0.0.0
```

**No authentication in v1** — tracked in [#187](https://github.com/barca-orc/barca/issues/187).
Anyone who can reach the server can trigger and cancel runs and read outputs and history.
Binding beyond loopback is allowed but prints a warning on every start:

```
[barca] warning: serving on 0.0.0.0:8274 with no authentication — anyone who can reach
        this address can trigger and cancel runs. Keep it on a trusted network.
```

Deploy it on a private network/VPN, or behind a reverse proxy that authenticates (and
terminates TLS — barca does not ship certificates). The default bind stays `127.0.0.1`.

New and changed endpoints:

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/runs/{id}/events` | Server-sent events: the same progress/transfer/step lines the CLI prints, then a terminal event with the run's JSON result |
| `GET` | `/runs/{id}/output` | The final output's bytes (or `307` to a short-lived signed store URL when the store supports it) |
| `GET` | `/history?limit=N` | `barca history --json`, from the DB |
| `GET` | `/stats/{name}` | `barca stats --json` |
| `GET` | `/list` | `barca list --json` |
| `GET` | `/status-nodes?targets=a,b` | `barca status --json` (node cache state) |
| `GET` | `/project` | Deployed files with content hashes, deploy time, git commit if known — for drift detection |
| `POST` | `/get/{target}`, `/run/{target}`, `/run` | Unchanged trigger contract; accept `?dry_run=1` and the CLI's refresh flags (`refresh`, `refresh_all`, `no_cascade`) |
| `GET` | `/status/{id}` | Now backed by the DB: survives restarts; unknown ids still `404` |

`GET /health` reports an `api_version`; a client refuses a server with a different major
version rather than misreading responses.

---

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Config:** `[server] url`; env `BARCA_SERVER`; flags `--server <url>`, `--local`.
  Server: `serve --host <addr>` (default `127.0.0.1`; non-loopback warns, §3.4).
- **The CLI contract governs server mode.** `barca docs contract`
  ([#181](https://github.com/barca-orc/barca/pull/181), [#186](https://github.com/barca-orc/barca/pull/186))
  defines stdout JSON schemas, the stderr error envelope, `--agent` lines and exit codes.
  Server mode must satisfy it unchanged; the new surface (`[server]`, `BARCA_SERVER`,
  `--server`, `--local`) enters the contract as **experimental**. This also retires the
  contract's current `barca serve` exception ("its JSON is the engine's own serialization,
  not the CLI's"): the endpoints mirroring CLI commands return exactly the CLI's JSON.
- **Output parity (the core contract):** for every command, server mode produces the same
  stdout JSON (field for field, modulo run ids and timings), the same stderr lines, and the
  same exit codes as local mode (0 ok, 1 step failed, 2 usage error, 3 barca/infra
  failure, 130 cancelled). Transport failures — server unreachable, rejected request,
  version mismatch — are `kind: infra` (exit 3), with a message naming the server.
- **stderr includes the steps' own output.** The contract defines stderr as carrying "your
  steps' own `print` output", so the server captures each run's worker stdout/stderr and
  sends it through `/runs/{id}/events`; the client writes it to stderr exactly as a local
  run would.
- **HTTP:** the endpoints in §3.4; response bodies for `/history`, `/stats`, `/list` are
  exactly the corresponding `--json` CLI output.
- **DB:** `runs` gains `triggered_by` (`api` or `schedule`; a caller identity comes with auth, #187).

### 4.2 Implementation Details

- `barca-cli` gets a client mode: each command resolves a backend (local executor or HTTP
  client) and hands the backend's result to the **same** rendering code, so formatting can't
  drift between modes. The JSON result types already live in `barca-core`; the server and
  the CLI serialize the same structs.
- The SSE stream is fed by the same callback that drives the local progress bar today;
  events carry a sequence number so a dropped connection resumes with `Last-Event-ID`.
- Run status moves from the in-memory `DashMap` to the `runs` table (the map remains a
  cache for live runs).
- Server-side artifact storage is unchanged: with `[remote]` storage configured the server
  writes locally and transfers in the background (as on the CLI); `serve` keeps
  RFC-0004's restriction that blob-synced state is off — the server *is* the shared state.

### 4.3 Rust ↔ Python Boundary

No change. Workers run only on the server, over the existing UDS protocol. The client
never imports or executes user code — in server mode it doesn't parse Python at all.

### 4.5 Edge Cases

- **Local edits.** The server runs deployed code, so a mismatch is refused before anything
  is sent. The client fetches `GET /project` and compares content hashes for (1) every file
  named on the command line and (2) every deployed file that also exists locally at the same
  path relative to the local project root (the directory containing `barca.toml`, else the
  cwd). Deployed files absent locally are not a mismatch — a client needn't have the project
  checked out. Any difference → error `kind: usage` (exit 2) in the contract's error
  envelope, listing the files, with remediation "deploy your change, or run with `--local`";
  nothing is triggered. Remedies:
  deploy, or `--local`. There is deliberately no "run the deployed version anyway" override
  yet (§10).
- **Ctrl-C** sends `DELETE /run/{id}`, waits briefly for `cancelled`, then exits 130 as a
  local cancelled run does.
- **Dropped stream** reconnects with `Last-Event-ID`; if the run already finished, the
  client fetches the terminal result from `/status/{id}`.
- **Server restart mid-run.** Governed by [#190](https://github.com/barca-orc/barca/issues/190): a
  deploy drains (in-flight runs finish, new ones get `503`), then force-cancels after the
  drain deadline; after a crash, startup marks leftover `running` runs `failed`. Clients
  streaming a run receive the corresponding terminal event (or `server draining`) and
  exit with the matching code.
- **One server per metadata DB (decided).** `barca serve` holds a lifetime lock on
  `.barca/serve.lock`; a second `serve` on the same DB exits with `kind: infra` naming the
  holder. It deliberately does **not** hold the metadata DB itself — that would bring back
  the bug [#136](https://github.com/barca-orc/barca/pull/136) fixed. Database access stays
  on #136's short per-operation cross-process lock, so:

  | Caller | Path | Concurrency |
  |---|---|---|
  | One-off commands from any machine, server mode | HTTP | unlimited — they never open the DB |
  | `barca --local …` on the server host | the DB, via #136's short locks | safe; queues briefly |
  | A second `barca serve` on the same DB | blocked by `serve.lock` | error |

  A `--local` run on the server host is invisible to the server: it isn't drained on
  deploy (#190), doesn't join in-flight duplicates, and doesn't get per-run code pinning.
  Its cache results are still correct (as today); documented, not forbidden — operators
  need `--local` on the box.
- **`--env`** selects an environment that must exist on the server; unknown env → error
  naming the server's environments.
- **Large outputs.** `-o value` streams from `/runs/{id}/output`; with a signed-URL
  capable store the client downloads straight from the store.
- **Schedules** are unaffected and remain server-only; their runs appear in everyone's
  `history` with `triggered_by = schedule`.

## 5. Determinism, Caching & Testing

No cache-key change: server mode moves *where* commands run, not what they compute.

- **Parity suite:** run each command locally and through an in-process server against the
  same project; diff normalized stdout JSON, stderr lines and exit codes. This is the test
  that keeps "business as usual" true.
- Bind: default stays loopback; a non-loopback `--host` starts and prints the
  no-authentication warning.
- Resilience: dropped SSE connection mid-run, server restart mid-run, Ctrl-C.
- `tests/integration/test_server_mode.sh`: start `barca serve`, run the CLI and `barca.api`
  against it.

## 6. Performance

Local mode is untouched (one extra config lookup). In server mode the client skips parsing
and planning; its latency is the HTTP round trip plus the server's own run time. Target: a
fully cached `barca get` through a server on the same host within ~20 ms of the local
equivalent, measured with `benchmarks/trivial` once client mode exists.

## 7. Drawbacks

- Every command gains a second code path; the parity suite is mandatory, not optional.
- A deploy step: changes reach the server through whatever deploys it (git pull +
  `--watch`, a container rebuild). "Try my branch against the shared cache" is not
  possible until §11's local-code mode.
- Refusing on drift means uncommitted local edits block *all* remote use of that project —
  including just fetching the deployed value with `-o value` — until they're deployed,
  stashed, or the command is run from a directory without the project.
- The server is a single point of failure for shared runs (local mode still works).
- No authentication: a server reachable by untrusted clients can be driven by anyone.
  Safe deployment relies on network placement or an authenticating proxy until #187.

## 8. Rationale & Alternatives

- **Blob-synced DB (status quo, RFC-0006).** Zero-ops, but measured size and contention
  limits make it a small-team mode at best. Kept as-is for teams without a server (§10).
- **Bucket-native immutable cache index** (create-only index objects per
  `(node, run_hash)`, per-run history objects). Keeps zero-ops and scales writers, but
  needs batching to keep cache checks fast, merge-on-read history, GC, and still leaves no
  home for schedules. A reasonable future option for serverless sharing; not the primary
  path.
- **DuckLake / a Postgres catalog.** DuckLake's multi-writer mode requires a catalog
  database server; a catalog file in object storage is read-only. If a server is required
  anyway, barca's own server is the simpler one to run and already executes the work.
- **Ship local code to the server (option b).** Closest to "business as usual" but needs
  code transport, dependency handling and isolation between users' code. Deferred (§11).

## 9. Prior Art

Prefect points its CLI and SDK at a server or Prefect Cloud via `PREFECT_API_URL` and an
API key — the same "one setting, then business as usual" model. Dagster's CLI and
webserver operate against a deployed code location; Airflow exposes a REST API alongside
its CLI. See [Framework Comparison](/comparisons/framework-comparison/).

## 10. Unresolved Questions

- **Naming:** `--server`/`[server]` (proposed) vs. `--remote`. `[remote]` already means
  artifact/state *storage* in `barca.toml`, so `--remote` would mean two unrelated things.
- **Drift — decided for v1: refuse** (exit 2, §4.5). Revisit later: an explicit override
  (e.g. `--deployed`) for read-only uses such as fetching a deployed value, or a
  warn-only mode for read commands (`list`, `history`, `stats`).
- **Authentication** — deferred to [#187](https://github.com/barca-orc/barca/issues/187) (tokens, roles, identity in `triggered_by`).
- **RFC-0006 optimistic mode:** keep it for server-less teams, or deprecate it in favor of
  server mode (one canonical way to share)?

## 11. Future Possibilities

- **Local code against the shared server** (option b): the client uploads the project
  source for planning and execution in an isolated environment.
- **Read-through cache for local runs:** local `barca get` consults the server's cache
  index and fetches hits, then runs only what's missing locally.
- Authentication and roles ([#187](https://github.com/barca-orc/barca/issues/187)), webhooks, a web UI on the same API, remote workers.
