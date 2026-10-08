---
title: 'RFC-0007: Server Mode — the CLI and Python API against a deployed barca'
description: 'Point barca at a deployed server with one setting; every command behaves as it does locally, and local edits run as isolated experiments.'
---

- **Status:** Under Review
- **Date:** 2026-10-03 (revised 2026-10-08)
- **Touches:** barca-cli | HTTP server | python/barca | barca-core
- **Prerequisites:** [#289](https://github.com/barca-orc/barca/issues/289) / [#306](https://github.com/barca-orc/barca/pull/306) (SIGTERM, trigger errors, interrupted runs in containers) and [#272](https://github.com/barca-orc/barca/pull/272) (`serve --host`). The full lifecycle of [#190](https://github.com/barca-orc/barca/issues/190) (drain, forced stop) is not a prerequisite; a refusal while shutting down is in scope (§4.5).
- **Supersedes / Related:** extends [RFC-0004](/rfcs/0004-http-server-api/) (lifts its v1 cuts: localhost-only, no streaming, memory-only run state, server-specific JSON); revises the sharing story of [RFC-0006](/rfcs/0006-configuration-and-remote-state/); preserves the output contract of [RFC-0002](/rfcs/0002-cli-surface/) and `barca docs contract`. Authentication stays out of scope ([#187](https://github.com/barca-orc/barca/issues/187)).

---

## 1. Summary

A team deploys one `barca serve` and points every client at it with one setting: `[server] url`
in `barca.toml`, `BARCA_SERVER`, or `--server <url>`. In server mode the CLI (and `barca.api`,
which runs the CLI) sends each command to the server and prints the result exactly as a local
run would: the same stdout JSON, the same stderr lines, the same exit codes, and Ctrl-C cancels.

The server runs **its deployed code**. Local edits do not block anything and are not sent. To
try a local change against the deployment's data and cache, `barca get <asset> --experiment`
uploads the local source and runs it on the server in an **experiment namespace**: it reads the
deployed cache, so unchanged upstreams are cache hits, and it writes only to its own namespace,
so the deployment's results, history and schedules are never touched.

One HTTP surface serves the CLI, the web UI and any other client. Every endpoint that mirrors a
command returns that command's `--json` body, built by the same Rust code, and errors carry the
CLI's error envelope.

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
and deployments exist (`site/.../deploying.md`). A client deploying it reported that they want
to drive the deployment from a laptop with the CLI they already use. Today they cannot:

- The CLI has no notion of a server.
- The server binds to `127.0.0.1` only (fixed by #272).
- A run's result is a path on the server's disk.
- Run status lives in memory and is lost on restart. A failed run keeps only an error string;
  its steps, traceback and database run id are dropped.
- The progress lines a local run prints (`--agent` lines, plan warnings, cached and completed
  steps) are written to the server's own stderr and never reach a client.
- Errors are `{"error": "..."}` and lose the CLI envelope's `kind`, `code`, `remediation`, `node`
  and `traceback`.
- The only client is `barca.Client`, a second Python surface that overlaps `barca.api`.

There is also a gap the server alone does not fill: an engineer editing an asset wants to see
what the edit produces on the deployment's data, reusing the deployment's cached upstreams,
without deploying and without any risk to what the deployment serves. Running locally means
recomputing every upstream on a laptop; deploying a change to try it is the risk itself.

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

#### Experiments: local code on the deployment

```
$ barca get daily_report --experiment
[barca] experiment mukul-2026-10-08-3f9c1a: 1 file differs from the deployment: pipelines/report.py
[barca] step:pipelines/sources.py:orders cached
[barca] step:pipelines/report.py:daily_report completed 4.2s (1/1)
{"status":"success","run_id":"…","experiment":{"id":"mukul-2026-10-08-3f9c1a","files_changed":["pipelines/report.py"]},…}
```

What happens:

1. The client walks the project root the way discovery does (same skip list and `[discovery]`
   `exclude`), hashes every `.py` file, and asks the server which hashes it lacks.
2. It uploads only those files, then starts the run with the full manifest.
3. The server runs the deployed project with the local `.py` files laid over it. Unchanged
   upstreams have the same run hash as deployed and are cache hits; the edited asset and
   everything downstream of it recompute.
4. Results go to the experiment's namespace. `barca status daily_report` without
   `--experiment` still shows the deployed result. Schedules, `--refresh` runs and the web UI's
   default view are unaffected.

`--experiment` takes an optional name, written with `=` so it is never mistaken for a target
(`--experiment=try-new-join`); without one the id is
`<user>-<utc date>-<short hash of the manifest>`, so the same edit re-run is the same experiment
and hits its own cache. The source of every experiment is kept (§4.2), so a result can always be
traced to the code that made it.

Only `get` takes `--experiment` in this RFC. Tasks and `@sink`s exist for their side effects,
which is exactly what an experiment must not have: `run --experiment` is a usage error, and a
sink downstream of an experiment's target is skipped with a line saying so.

The server must opt in: `barca serve --allow-experiments`. Without it, `--experiment` is refused
with exit 2 naming the flag. An experiment is code from the client running on the server, so it
belongs only behind the authenticating proxy that `deploying.md` already requires for a
reachable server (§4.5, §7).

### 3.2 Python API

`barca.api` runs the binary, so it follows the same configuration with no change:

```python
import barca
barca.get("daily_report")                      # local or remote, by [server] / BARCA_SERVER
barca.get("daily_report", experiment=True)     # new keyword, maps to --experiment
```

`barca.Client` is deprecated in favour of `barca.api` (one canonical way): it warns on
construction for one minor release, then is removed.

### 3.4 HTTP API

`barca serve --host 0.0.0.0` (#272) makes the server reachable. With no authentication, every
start on a non-loopback address prints a warning; deploy behind a private network or an
authenticating, TLS-terminating proxy (`deploying.md`).

All endpoints below are new or changed. Bodies are JSON; errors are the CLI envelope (§4.1).

**Runs**

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/runs` | Start a run. Body: `RunRequest` (below). `202 {"run_id"}`, or `200` with the `--dry-run` JSON when `dry_run` is set (nothing starts). |
| `GET` | `/runs/{run_id}` | The run: `{run_id, status, triggered_by, namespace, started_at, finished_at, outcome}`. Backed by the DB, so it survives a restart. |
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
  "agent": true,                    // which stderr lines the client will print (§4.2)
  "experiment": null                // or {"name": null, "files": [{"path": "...", "sha256": "..."}]}
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
| `GET` | `/nodes?targets=a,b&sample=N&namespace=` | `barca status --json` |
| `GET` | `/history?limit=N&all=1&namespace=` | `barca history --json` |
| `GET` | `/stats/{name}?namespace=` | `barca stats --json` |
| `GET` | `/project` | The deployed tree: `{api_version, root, env, deployed_at, commit, files: [{path, sha256}]}` |
| `GET` | `/health` | Adds `api_version`, `env`, `experiments` (bool) |

**Experiments** (`403` with an envelope unless the server runs with `--allow-experiments`):

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/blobs/missing` | Body `{"sha256": [...]}`, answer `{"missing": [...]}`. |
| `PUT` | `/blobs/{sha256}` | Upload one file's bytes. The server checks the hash. Idempotent. |
| `GET` | `/experiments` | `{experiments: [{id, created_by, created_at, files_changed, runs}]}` |
| `GET` | `/experiments/{id}` | One experiment, with its manifest. |
| `DELETE` | `/experiments/{id}` | Remove its rows and artifacts. Its source is kept unless `?source=1`. |

**Existing routes.** `POST /run`, `POST /run/{target}`, `POST /get/{target}`,
`GET /status/{id}`, `GET /events/{id}`, `GET /assets`, `GET /assets/{name}` stay for one minor
release as aliases implemented on the new handlers, then are removed. The web UI moves to the new
routes in the same release.

### 3.5 Dev server / `--watch` / UI

`--watch` re-hashes the deployed tree for `/project` on every reload. The UI shows
`triggered_by` and the namespace in run lists, and has an experiment filter; deployed views
never include experiment results.

---

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Config:** `[server] url` in `barca.toml`; `BARCA_SERVER`; `--server <url>` and `--local` on
  `get`, `run`, `status`, `list`, `history`, `stats`; `--experiment[=name]` on `get`;
  `serve --allow-experiments`. All enter the contract as experimental.
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
  while shutting down 503, experiments disabled 403. A failed step is never an HTTP error: it is
  a run whose `outcome.status` is `failed`. Remediation text names files as the user typed them,
  because the request carries them that way.
- **Versioning:** `api_version: 1` in `/health` and `/project`. The client refuses a different
  major. Additive fields do not bump it.
- **DB:**
  - `runs` gains `triggered_by` (`cli`, `api`, `schedule`), `namespace` (NULL for deployed,
    `exp:<id>`), and `outcome_json` (the serialized `outcome`, so a finished run's result
    survives a restart).
  - `materializations` gains `namespace`.
  - New table `experiments (id, created_by, created_at, manifest_json)`.
  - `barca history --json` rows gain `triggered_by` and `namespace` (additive).
- **Retired:** the contract's exception that `barca serve` JSON is the engine's own
  serialization.

### 4.2 Implementation Details

**One rendering path.** The code that turns a run's result into stdout JSON moves from
`barca-cli/src/main.rs` into a `barca-core::report` module, and the error envelope
(`ErrorKind`, `CliError`, `Context`) from `barca-cli/src/error.rs` into `barca-core::envelope`.
The server builds response bodies with these functions, and the client renders a received
`outcome` with them. `FailedStep`, `PartialRun`, `MultiResult` and `ExplainResult` gain serde.

**Progress as events.** Today `commands::execute` writes progress with `eprintln!` at about a
dozen sites. They go through one `Reporter` that either prints (local, with or without the bar,
byte for byte as today) or emits `line` events (server). Each line carries `when`: `agent` for
lines a local run prints only with `--agent`, `always` for the rest; the client prints by its
own mode. Cached steps emit `step_finished` with `cached: true`. The server's event channel
numbers events, keeps the backlog for the run's lifetime, and re-syncs a lagging subscriber from
the backlog instead of dropping events.

**DB-backed runs.** `execute` takes an options struct (`run_id`, `triggered_by`, `namespace`)
instead of its growing positional list. The server allocates the run id up front, so the id it
returns is the database id. The in-memory map stays as a cache for live runs; `/runs/{id}` falls
through to the DB.

**Client.** `--server` resolution happens in `main()` before `enter_project_root`. In server
mode the client still finds the root and rebases file arguments (for `--experiment` and to send
files as typed) but skips discovery. A new `barca-cli/src/client.rs` holds the HTTP flow on
`reqwest` (rustls, no default features). The binary grows by about 2 to 3 MB; the PR states the
measured number.

`get`/`run` in server mode:

1. `GET /health`; check `api_version`.
2. With `--experiment`: walk, hash, `POST /blobs/missing`, `PUT` each missing blob.
3. `POST /runs`.
4. Stream `/runs/{id}/events`: print `line`s filtered by mode, `log` lines as the local
   coordinator prints them.
5. On `run_finished`, render `outcome` with the shared report functions and exit with its code.
6. A dropped stream reconnects with `Last-Event-ID`. If the stream is gone, `GET /runs/{id}`
   gives the `outcome`.

Ctrl-C sends `DELETE /runs/{id}` and keeps streaming until `cancelled`; a second Ctrl-C exits
130 at once, as a local run does.

**Experiments.**

- **Blobs** are stored at `.barca/blobs/<sha256>`.
- **Overlay.** An experiment run gets an overlay tree `.barca/experiments/<id>/root/`: a mirror
  of the deployed root in which every entry is a symlink to the deployed file, except the
  uploaded `.py` files, which are copies of their blobs. A `.py` file deployed but absent from
  the manifest is removed from the overlay, so a local delete is honoured.
- **Planning and execution.** The run plans from the overlay as its root, so node ids and helper
  resolution match the deployment. Workers start with the overlay as their working directory,
  so data files and `__file__`-relative paths resolve, through the symlinks, to the deployed
  files.
- **Cache lookup** (`commands::lookup_cached`, the single query on `(node_id, run_hash)`):
  - A deployed run matches `namespace IS NULL`.
  - An experiment run matches `namespace = ? OR namespace IS NULL`, preferring its own row.
  - Writes always carry the run's namespace.
- **Artifacts** go under `{artifacts}/experiments/<id>/{node}/{run_hash}{ext}`, locally and in a
  remote store, so they are never mixed with deployed ones.
- **Sinks** downstream of the target are planned as skipped.
- **Schedules** ignore experiment rows entirely.

### 4.3 Rust ↔ Python Boundary

The worker protocol does not change. Workers run only on the server, over the existing UDS. An
experiment changes only the working directory and module paths the coordinator gives a worker.
The client never imports or parses user code; the server parses the overlay statically, as it
parses the deployed tree.

### 4.4 Node-Kind Semantics

Unchanged. Experiments preserve the cache-poisoning guard and extend it: an experiment row is
never visible to a deployed lookup, and tasks cannot run in an experiment.

### 4.5 Edge Cases

- **Local edits without `--experiment`.** The server runs the deployed code. When the client has
  the project checked out and a file named on the command line differs from the deployed one, it
  prints one line (`[barca] note: pipelines/report.py differs from the deployment; the server
  runs the deployed version (try --experiment)`). It never refuses.
- **One server per DB.** `barca serve` holds a lifetime lock on `.barca/serve.lock`; a second
  `serve` on the same DB exits with `kind: infra` naming the holder. It does not hold the DB
  itself (that would bring back the bug #136 fixed); DB access keeps #136's short per-operation
  lock.
  - A `barca --local ...` run on the server host is safe and still works.
  - Such a run is invisible to the server: it does not join in-flight duplicates and is not
    cancelled at shutdown. This is documented, not forbidden.
- **Shared remote state.** `serve` keeps refusing `state = "optimistic"`: the server is the
  shared state. [#310](https://github.com/barca-orc/barca/issues/310) tracks a serve-time
  override so one `barca.toml` can serve laptops and servers.
- **Shutdown.** While shutting down the server answers new runs with `503` and the client exits 3
  naming the server. Streaming clients get the run's terminal `cancelled` outcome and exit 130.
  Draining in-flight runs before stopping is #190.
- **`--env`** must equal the server's environment in this RFC; another is a usage error naming
  the server's.
- **Large outputs.** `-o value` on a pointer downloads `/runs/{id}/output` and deserializes it
  with the same reader `barca.api` uses.
- **Experiment hygiene.** Nothing expires automatically in this RFC. `DELETE /experiments/{id}`
  removes one; `barca gc` (#83) is where retention belongs.
- **Experiment dependencies.** The server's Python environment is used. An import the deployment
  does not have fails the step, with the worker's own error.
- **Non-Python files** are not uploaded. A change to a data or config file is not part of an
  experiment; the manual says so.

## 5. Determinism, Caching & Testing

No cache-key change. Server mode moves where commands run; experiments add a namespace filter to
the one lookup and a prefix to artifact paths. Run hashes are computed exactly as today, which is
what makes the read-through to deployed results correct: an unchanged upstream has the deployed
run hash.

- **Parity suite:** the contract test's case table runs once locally and once through a served
  copy of the same project, in the same order. Normalized stdout JSON, the stderr line sequence
  and exit codes must be equal. This is the test that keeps "nothing changes" true.
- **Experiment isolation:**
  - An edited node recomputes and an unchanged upstream is a hit.
  - A deployed `status` and `history` are unchanged afterwards.
  - A deployed run after the experiment does not hit the experiment's row.
  - `run --experiment` is refused, and a sink is skipped.
  - Without `--allow-experiments` the request gets `403`.
- **Resilience:** dropped stream mid-run, server restart mid-run (`/runs/{id}` from the DB),
  Ctrl-C, `503` during shutdown.
- **HTTP shapes:** every route in `crates/barca-server/tests/api.rs` with the in-process router.
- `benchmarks/trivial`: local runs must not regress (the `Reporter` sits on the hot path).

## 6. Performance

Local mode gains one config lookup. In server mode the client does not parse or plan; a fully
cached `barca get` through a server on the same host should be within about 20 ms of the local
run, measured with `benchmarks/trivial` once client mode exists. An experiment adds one hashing
walk of the root and the upload of changed files only.

## 7. Drawbacks

- Every command gains a second path. The parity suite is required, not optional.
- Changes reach the server through whatever deploys it (git pull with `--watch`, a container
  rebuild). Experiments cover "try my change"; they do not replace deploying.
- `--allow-experiments` turns the server into a place that runs code it is sent. Without
  authentication (#187), the network or proxy is the only boundary. This is why it is off by
  default and why the manual's experiments section starts with this.
- The server is a single point of failure for shared runs; local mode still works.

## 8. Rationale & Alternatives

- **Refuse to run when local files differ** (this RFC's first draft). Safe, but uncommitted
  edits blocked every remote command, including fetching a deployed value. Running the deployed
  code and offering `--experiment` for the local version is both more normal and more useful.
- **Run local code with no isolation.** The obvious meaning of "run my code on the server", and
  the one that breaks the deployment: an edited asset would overwrite the result the schedule and
  every other client read. A namespace that reads through to deployed results keeps the speed
  without the risk.
- **Blob-synced DB (RFC-0006).** Zero-ops, but its size and contention limits make it a
  small-team mode. Kept for teams without a server (§10).
- **Bucket-native cache index** (create-only index objects per `(node, run_hash)`). Keeps
  zero-ops and scales writers, but needs batching, merge-on-read history and GC, and still leaves
  schedules homeless. A possible future for serverless sharing.
- **`--remote` as the flag name.** `[remote]` already means artifact and state storage; one word
  would mean two unrelated things in one file.

## 9. Prior Art

Prefect points its CLI and SDK at a server with `PREFECT_API_URL` and an API key: one setting,
then business as usual. Dagster's CLI and webserver work against a deployed code location, and
branch deployments give each pull request its own isolated copy that reads production assets,
the closest analogue to experiments. Airflow exposes a REST API beside its CLI.

## 10. Unresolved Questions

- Whether RFC-0006's optimistic mode stays for server-less teams or is deprecated in favour of
  server mode (one canonical way to share).
- Whether experiments should be able to include changed non-Python files.
- Whether `--experiment` on a sink-free `run` should be allowed later, and how a task opts in.
- Authentication and roles ([#187](https://github.com/barca-orc/barca/issues/187)), which decide
  who may start an experiment.

## 11. Future Possibilities

- Promoting an experiment: `barca experiment promote <id>` copies its rows into the deployed
  namespace after the code is deployed, so the deployment does not recompute what the experiment
  already did (same run hash once deployed).
- A read-through cache for local runs: a local `barca get` consults the server's cache index and
  fetches hits, then runs only what is missing.
- Warm workers across runs (#84), a global worker budget (#80), remote workers.
