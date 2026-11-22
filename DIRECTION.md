# Direction

Last updated: 2026-11-22, after v0.46.2; release milestones reconciled through 0.49.0. The pinned
[Direction issue](https://github.com/barca-orc/barca/issues/395) mirrors this file; it replaced
the roadmap issue (#344) and the adoption tracker (#108).

## What barca is

An embedded orchestrator. You mark Python functions with `@asset`, `@sensor`, `@task` and `@sink`, barca reads the source without importing it, runs what is stale in worker processes on this machine, and caches results under `.barca/`. It is a binary and a directory, like DuckDB or SQLite, not a service you deploy.

`barca serve` is the same binary kept running: cron schedules, an HTTP API and the web UI, and the deployed copy of the pipeline code. **Server mode** means the server owns the ledger, the one record of which results exist, so that several people get the same data. A laptop runs its own work, uploads the artifacts to the store and tells the server; a `--server` run happens on the server itself. Either way every run is one machine, and the server only arbitrates what exists. One ledger, many single-node runs.

## Standing non-goals

- Distributing one run across machines. A remote store holds artifacts and a backup; the server holds the ledger.
- Sharing state without a server. Two machines writing one bucket is not supported; a store without a server is one machine's backup and cache.
- Running code sent by a client on the server (experiments, overlays, namespaces).
- Built-in user accounts and SSO/OIDC providers. Opt-in API tokens issued and validated by
  barca are the approved 0.36.0 exception (#187); existing proxy deployments remain supported.
- More than one project root per server.
- A worker pool or engine shared across runs, and warm workers across runs. A server may deduplicate identical in-flight asset steps between its own runs (experimental, 0.39.4, #654).
- Approval gates or workflows that wait on external events.

## Now, in order

An issue's milestone is the release it ships in; the milestones below are the order.

**0.46.3, correctness fixes.** Batched stream deadlocks (#1100), remote and optional input
types (#1103, #814), loaded-runner test timing (#1096), loopback Host validation (#998),
and partition status after pinning (#1069).

**0.46.4 cleanup.** Warn about older installed versions (#672) and sensor tuples that always
report a change (#673).

**0.47.0, the run lifecycle on a server ([#944](https://github.com/barca-orc/barca/issues/944)).**
Stop and drain, restart and requeue, cancel, rerun scope and live progress (#926 to #943),
including consistent status words and route shapes. Also cache-only reads (#1085), first-cause
failure summaries (#1106), worker errors (#671), and skipped scheduled ticks (#811).

**0.47.1 cleanup.** Response compression, polling and key-explorer gaps (#1044), and freshness-gap follow-up (#974).

**0.48.0, resource bounds.** Cache eviction while runs remain active (#1054), gc on a large
catalog (#750, #754), coordinator and inspection memory at partition scale (#655, #656),
and metadata trimming without starving the control API or scheduler (#1082).

**0.49.0, time outside the steps on a server.** Computed and published (#1006), measurements
(#951), shared in-flight steps (#654), scheduled-work capacity share (#1105), recovered-key
batching (#1097), artifact measurements and a format RFC (#1104), tracing (#845), and out-of-call batch keys (#668). A new shared artifact format is outside this release's scope.

RFC-0014 pipeline fuzzing is deferred, with no action on its working branch. It has no
scheduled release. DuckDB 1.x and 2.x remain supported (`duckdb>=1.4,<3`), with CI on both.

### Release sequence

Each release is a GitHub milestone with a due date; the milestone page is the schedule, and a shipped release's milestone is closed with its ship date. Each minor carries its breaking changes and is followed by a cleanup release (below).

| Milestone | Shipped | Theme | Issues |
|---|---|---|---|
| 0.24.2 | 2026-10-20 | Patch: the three production reports, the last hash holes, Dependabot | #437, #432, #433, #295, #439 |
| 0.24.3 cleanup | 2026-10-20 | Linear partition scaling, the last Dependabot alert, metadata DBs from 0.21 to 0.23 open | #345 (#456), #445, #448 |
| 0.25.0 | 2026-10-20 | Breaking: output shape, and a size limit for `.barca` | #287 with #404, #457 |
| 0.25.1 cleanup | 2026-10-20 | A stable last-result key in `status`, the release workflow's PyPI check | #475, #480 |
| 0.26.0 | 2026-10-20 | Breaking: the store is a backup, `optimistic` is gone; signed URLs; Slack | #390, #403 (closes #243), #452, #450 |
| 0.26.1 cleanup | 2026-10-20 | The state-sync modules and optimistic paths deleted | #487 |
| 0.27.0 | 2026-10-20 | The server is the ledger; drain; JSON logs; Datadog traces | #190, #393 with #435, #394, #402, #434, #482 |
| 0.27.1 cleanup | not released as 0.27.1 | Its fixes shipped in 0.28.3 | #492, #507 |
| 0.28.0 | 2026-10-21 | Breaking: freshness propagation; database reset; loud metadata failures; generated site reference pages | #253, #508, #513, #479, #500, #501, #505 |
| 0.28.1, 0.28.2 | 2026-10-21 | Patches: metadata DB lock and empty DB; duckdb below 2 | #537 |
| 0.28.1 cleanup (shipped as 0.28.3) | 2026-10-21 | Docs and site, new-user fixes, faster start-up, running state in the UI | #495 to #499, #502, #504, #529, #534, #560, #65 |
| 0.29.0 | 2026-10-21 | Breaking: parameters checked at planning, retries in the same worker; heavy-step capacity; what changed | #549, #523, #436, #558, #555, #553, #250 and others |
| 0.29.1 | 2026-10-21 | Store and env fixes from the realistic fixture | #574, #575, #576, #577, #593, #595 |
| 0.29.2 to 0.29.6 | 2026-10-21 to 2026-10-22 | Bookkeeping, inspection, keyed partition sources and selection | #503, #614, #617, #622 |
| 0.30.0 | 2026-10-23 | Partitioned dataset sinks; sink failure fails the step | #621 |
| 0.30.1 | 2026-10-23 | Older metadata DB compatibility | #629, #630 |
| 0.30.2 | 2026-10-23 | Bounded outcomes and memory over many partition keys | #623 |
| 0.31.0 | 2026-10-23 | Production diagnostics, shared capacity, timeout, drain recovery and cutoff | #627, #628, #616, #637 |
| 0.32.0 | 2026-10-24 | Batched partitions and remaining documentation | #643, #606 |
| 0.33.0 | 2026-10-25 | Bounded DAG coordination; sensor `(True, value)` always counts as changed | #652 |
| 0.33.1 | 2026-10-27 | Run result bookkeeping separated from execution; behavioral contracts consolidated | #659, #660 |
| 0.34.0 | 2026-10-27 | Breaking: unreadable decorator values exit 2; duckdb 2; sink republish on cache hits; failure phases | #567, #662 to #665 |
| 0.35.0 | 2026-10-28 | Batched cache lookups, shared computed partition domains, responsive server reads and large-graph UI | [Release notes](https://github.com/barca-orc/barca/releases/tag/v0.35.0) |
| 0.35.1 | 2026-10-28 | Cache-size accounting off request threads; transfer-helper handshake before planning | #702, [release notes](https://github.com/barca-orc/barca/releases/tag/v0.35.1) |
| 0.35.2 | 2026-10-29 | Read-only status, metadata-first UI, run actions, credential masking and first-use/help docs | #708, #710, #709, #706 part 3, #650, #651 |
| 0.35.3 | 2026-10-29 | Streamed batched partitions sort by all columns, so a refresh with the same rows in another order keeps cutoff consumers cached | [#721](https://github.com/barca-orc/barca/pull/721), [release notes](https://github.com/barca-orc/barca/releases/tag/v0.35.3) |
| 0.36.0 | 2026-10-30 | Breaking: attempt-scoped event IDs with bounded replay, served-run backups on the routine interval, store inspection evidence reused for five minutes; configurable backup cadence, opt-in project tokens and UI token management, credentials on every request | #706, #187 |
| 0.36.1 | 2026-10-30 | The UI says what is broken and why at thousands of nodes; `GET /state` scoped by group and file | [#740](https://github.com/barca-orc/barca/pull/740) |
| 0.37.0 | 2026-10-30 | UI: command palette, failures by cause, one name per state, Run names its target; `file`/`line` in `list`, `cache.change_causes` in `status` | #742 |
| 0.37.1 | 2026-10-30 | The metadata database keeps to its share of the size limit (a 41 GB production database came down to 848 MB) | [#751](https://github.com/barca-orc/barca/pull/751) |
| 0.38.0 | 2026-10-31 | Batched calls reported per node; the UI at partition scale (`GET /assets/{name}/partitions`, failed keys in `status`) | [#753](https://github.com/barca-orc/barca/pull/753) |
| 0.39.0 | 2026-10-31 | A failed partition key blocks only the keys that read it (`blocked`, `upstream_key_failed`); run page, actionable node panel, key search, one time zone, readable graph | #666 (#764), #757 to #767 |
| 0.39.1 | 2026-10-31 | `--refresh 'source[key]'` keeps the whole target DAG; the metadata trim no longer evicts artifacts that fit; a seeded simulation of long server use | #784, [#783](https://github.com/barca-orc/barca/pull/783) |
| 0.39.2 | 2026-11-02 | `barca.sql()` in the Python API; UI duration estimates scoped to each node's partition history | [#788](https://github.com/barca-orc/barca/pull/788), [#789](https://github.com/barca-orc/barca/pull/789) |
| 0.39.3 | 2026-11-02 | Failed partition batches split into smaller calls, down to single partitions | [#787](https://github.com/barca-orc/barca/pull/787) |
| 0.39.4 | 2026-11-03 | A server's runs share identical in-flight asset steps (experimental); a requeued run's re-executed steps are recorded under their new run hashes | #654 (part, [#796](https://github.com/barca-orc/barca/pull/796)), #794 |
| 0.39.5 (the 0.39 cleanup) | 2026-11-04 | No features: #799 documented (a refresh recomputes only its target's cone); 0.39.4 measured (31.7 ms trivial run) with a performance gate in the merge tier; the documents made true; one `CacheState` enum, one metadata write boundary, serve out of the embedded hot loop, the `.barca/` layout spec; per-test timeouts, Playwright and type drift in CI, isolated backup tests, Dependabot at zero critical and high | #800: #804 (#799), #810, #803, #802, #819, #817, #805, #806, #809, #808 |
| 0.39.6 to 0.45.0 | 2026-11-05 to 2026-11-19 | The second cleanup, pins, pauses, the health map and the freshness gap, server query route, run counts and queue state; see each version's notes | [CHANGELOG.md](CHANGELOG.md) |
| 0.46.0 | 2026-11-20 | Breaking: a command beside a running `barca serve` goes to it (`--local` runs here). The modularity series: `barca-core` dissolved into layer crates, one definition per view for the CLI and the server, the metadata lock (lock-free reads, turns, one kept connection), an IR that is not the parser's | #1056 ([#1062](https://github.com/barca-orc/barca/pull/1062)), #945, part of #1054 |
| 0.46.1 | 2026-11-22 | Scheduler, metadata trimming and worker-fatal recovery fixes | [Release notes](https://github.com/barca-orc/barca/releases/tag/v0.46.1) |
| 0.46.2 | 2026-11-22 | Batched calls preserve per-key results | [Release notes](https://github.com/barca-orc/barca/releases/tag/v0.46.2) |
| 0.46.3 | planned | Correctness fixes | #1100, #1103, #1096, #998, #1069, #814 |
| 0.46.4 cleanup | planned | Version and sensor compatibility warnings | #672, #673 |
| 0.47.0 | planned | Server run lifecycle and cache-only reads | #944: #926 to #943, #1085, #1106, #671, #811 |
| 0.47.1 cleanup | planned | Polling, key exploration and freshness follow-up | #1044, #974 |
| 0.48.0 | planned | Resource bounds | #1054, #750, #754, #655, #656, #1082 |
| 0.49.0 | planned | Time outside the steps on a server | #1006, #951, #654, #1105, #1097, #1104, #845, #668 |

The milestone is the authoritative assignment. Tickets that missed an earlier milestone have
been moved to the release where their remaining work is scheduled. #1087 remains a deployment
validation follow-up on shipped 0.46.1. Simple tokens remain the authentication scope; roles
and SSO (#187) are parked for the future.

### Cleanup releases

After every minor, a `0.x.1 cleanup` milestone: one week, no features. The checklist is the release; each item that finds something becomes an ordinary issue in that milestone.

- Re-read every surface the minor shipped against "What barca is" and the non-goals. What drifted is reverted or gets an issue.
- Delete what the previous minor deprecated, and the modules its Breaking lines made unreachable.
- Diff the manual, `contract.md`, the snapshots and the site pages against behaviour you can run.
- Dependabot: zero critical and high; lockfiles refreshed; what cannot be bumped gets an issue naming the blocker.
- Flaky or skipped tests are fixed or deleted, never kept.
- Every open question due by this release is decided and moved to "Decided", or deferred with a reason.
- `specs/README.md` says implemented or proposed truthfully for every boundary.

## Open questions

Design questions nobody has answered yet, the issue each blocks, and the release it must be decided by. Proposals in issue status lines are proposals until they appear under "Decided".

- Whether `GET /schedule` and the `schedule` block of `GET /state` settle on RFC 3339 times, and
  whether a run in a list takes `barca history`'s key names (`run_id`, `failure`). Proposed with
  #943; blocks 0.47.0.
- `[cache] max_size`: a hard cap or a target, and whether a run is refused or delayed when gc
  cannot make room (#1054). Blocks 0.48.0.
- What a run with an owed sink reports, and when a server's ledger tells other machines of a
  computed result (#1006). Blocks 0.49.0.
- Whether a "check" is a kind the server knows (the web UI infers it from a `check__` name).
  No release yet.

Settled by what shipped, and documented in the manual: drain defaults (#190: `--drain-timeout` and `[server] drain_timeout`, 30 s; the server exits 0 when the runs in flight finish and when the timeout requeues them, and 1 only when a second SIGTERM or SIGINT cancels them, `barca docs scheduling`, "Stopping the server"; 0.27.0), pause persistence (#435: a pause survives a restart; 0.27.0) and the fixture home (#250: `benchmarks/planning_pipeline/`; 0.29.0).

## Decided, November 2026

**Structure (2026-11-19, #1056).** The crates are the layers: `barca-model` (shared vocabulary,
the IR and the graph), `barca-python` (the Python frontend), `barca-compile` (planning),
`barca-db`, `barca-proc`, `barca-engine` (the worker loop, no database), `barca-store`,
`barca-decide`, `barca-analysis`, `barca-run` and `barca-service`, the one crate the CLI and
the server import. The scheduling engine is separate from the decision layer. The CLI and the
web UI have one seam, the typed views; text and pixels are two renderings of them. The CLI is
one host of the service and the server another: a command is checked once and one place decides
where it runs. Record: [crate-split-2.md](docs/engineering/crate-split-2.md).

**A graph from another language (2026-11-19).** Wanted eventually, not now. The IR is an
internal Rust type that a second frontend could produce and that serializes
([ir.md](docs/engineering/ir.md) lists what a second frontend could not supply today); there is
no external format, and executing a plan built elsewhere stays undecided.

**A command beside a server (2026-11-19, 0.46.0).** In a project a `barca serve` is running in,
commands go to that server over `.barca/serve.sock`: reads are answered by it, `get` and `run`
join its queue ahead of scheduled work, in the order asked (`--queue last` waits behind
scheduled work too), and `--local` runs in the command's own process. The project's own server
wins over a configured remote one. The socket needs no token: it is owner-only, and whoever can
open it can write the database.

**The metadata lock (2026-11-19, 0.46.0).** The contention was barca's own lock, not SQLite's,
so MVCC was never the remedy. Reads do not take the lock, a waiter gets a turn, and a process
keeps one write connection; the cross-process lock is still taken per operation. DuckDB stays
the query surface (`barca sql`, `POST /sql/query`) and is not used for analysis.

**Refresh (2026-11-19, #937).** A plain run brings the asset DAG up to date: fresh by default.
`refresh all` is the hard refresh: it discards the cache and re-runs everything, and keeps that
name and meaning in the CLI and the API. The web UI's Run does the default, offers the hard
refresh as its own action, and has "Including Manual", off by default.

**Run status words (2026-11-19, #943).** `failed`, not `error`; `pending`, not `queued`.

**The 0.39 cleanup (2026-11-04, #800).** The cleanup weeks after 0.36, 0.37, 0.38 and 0.39 were
skipped: four minors and six patches shipped in five days with no checklist pass. 0.39.5 is the
catch-up, one cleanup release for all four, following the checklist under "Cleanup releases".

**Shared in-flight steps (#796, 0.39.4).** Kept on under `serve` and experimental: two runs of
one server that need the same asset step under the same run hash compute it once. The non-goal
above is amended to allow it. Three gaps remain, tracked in #654: cancelling the owning run
makes the waiting run re-execute the step, the handoff happens before the owner's row is
durable, and a decide-to-lease race can execute a step twice. A setting to turn it off is new
surface and belongs to a minor.

**The UI.** The UI is a vocabulary-faithful view of the ledger. It uses the CLI's state words
(`cached`, `stale`, `unsynced`, `blocked`, `never_run`, `partial`, `always_runs`, `unknown`)
and no others, maps each state once from the shared `CacheState` enum via generated types, and
shows exactly what `barca status --json` shows, with client-side muting removed. The current UI
does not meet this, so the redesign is scoped after 0.39.6 (its prototype is PR #818,
`feat/ui-chains`); #807 tracks the stale Playwright specs, fixed in 0.39.6.

Amended 2026-11-17 (#953, for 0.44.0). The health words (`failing`, `blocked`, `suspect`,
`behind`, `undecided`, `never_run`, `ok`) and the cause buckets (`catching_up`, `queued`,
`no_refresh_path`, `overdue`, `failing`, `undecided`) are barca vocabulary too: defined once in
Rust (`barca-analysis`), carried by `barca status --json` and the `barca.*` tables, and shown by the
CLI and the UI alike, beside the cache state and never in place of it. The UI still adds no
word of its own: the seven health words it shipped in TypeScript in 0.43 move to Rust. Four
layers, each with one job: **records** (the scheduler writes what it decided and computes no
analytics), **classification** (`barca-analysis` derives health, cause, refresh path and
estimates), **aggregation** (`barca sql` tables in the `barca` schema, queried with DuckDB over
rows Rust hands it) and **presentation** (the CLI and the UI render; TypeScript derives
nothing). Health is judged by when a node will next be fresh: a stale node the next tick
refreshes is catching up, not a problem; one that ticks passed over (overdue) or that nothing
will refresh (no refresh path) is. Specification: [health.md](specs/behavior/health.md),
approved intent until the 0.44.0 issues implement it.

**Metadata durability (2026-11-04, #810).** The metadata database keeps SQLite's full
synchronous mode. The 0.39.4 benchmark attributes 3 to 4 ms of a 32 ms trivial run to WAL
fsyncs; `synchronous=NORMAL` would recover them at the cost of losing the last commits on power
loss. The database is the ledger, so the milliseconds are not worth a durability caveat. Revisit
only with a measured need and a persistence-spec change.

**Store inspection (0.36.0, #706).** Artifact integrity evidence is bounded and reused without
making `cached` mean unchecked: local digest and shape evidence is keyed by file identity and
dropped when the file changes, cloud availability and remote shape observations live five
minutes, and required reads, transfers and ledger acceptance always verify directly
(`specs/behavior/inspection.md`, "Artifact inspection evidence"). The cost of cold inspection
as the database grows is still unmeasured and stays with #656.

## Decided, October 2026

**After 0.35.1 (2026-10-29, #707).** The RFCs record the accepted 0.36.0 designs;
implementation is merged except the final store-inspection gate; release validation and
publication are still required:

- Backups: `[remote] backup_interval` / `BARCA_BACKUP_INTERVAL`, default `"1m"`, with `"off"`
  for external volume backups; retain the shutdown backup. Explain whole-DB copy/upload cost.
  RFC-0015 chooses separate reporting of recognized local backups outside the cache budget,
  retaining the five-copy rotation. Routine served-run backups share the cadence gate;
  standalone final backups remain. Publication as part of 0.36.0 is still required.
- Credentials: support URL userinfo and `BARCA_SERVER_TOKEN` bearer headers on every request,
  including health, streams, cancellation and ledger traffic. Tokens can be generated in the
  UI and validated by barca in an opt-in mode with no user-account system. #187 is reopened
  for this scope. RFC-0016 specifies local bootstrap, tab-scoped browser bearer access,
  token persistence and revocation, without account or cookie-session systems. Implementation and conformance are merged; publication remains required.
- Streams: `Last-Event-ID` resume replaces treating every dropped connection as an infra error.
  RFC-0016 binds cursors to an attempt and bounds replay. After expiration, execution clients
  follow the existing run's canonical outcome with a gap notice; they do not resubmit work.
- Partial failure: a failed key does not stop other keys' downstream work (#666).
- Fuzzing is deferred. Incremental fan-ins (#674) need more discussion before scheduling.

**HTTP aliases (0.32.0, #606).** Keep the deprecated `POST /run`, `POST /run/{target}`,
`POST /get/{target}`, `DELETE /run/{run_id}`, `GET /status/{run_id}` and
`GET /events/{run_id}` routes: the public Python `Client` still uses them. New integrations
use `/runs`. There is no scheduled removal; a future removal must migrate `Client` first
and announce the breaking change in a minor release.

The design decisions behind items 2 to 4, settled on 2026-10-14. The RFCs carry the detail; this is the list to check a proposal against.

**Freshness.** A scheduled tick brings its node and every `Always` node below it up to date in one run, and with them the `Always` nodes below every unscheduled sensor that run polls: a change barca has seen propagates (what is below a sensor with its own `Schedule` follows that sensor's ticks). An `Always` node that is neither below a scheduled node nor below an unscheduled sensor some tick polls has no refresh path. `Manual` means refreshed by hand only: it is never recomputed by a tick or by a downstream `get`, and it does not block what is below it, which reads its current result. A `Manual` node that has never been computed makes a downstream request a usage error with a remediation line. A failed node retries at every tick up to `Schedule(..., max_failures=N)`. A new task fires at the first tick that reaches it. Propagation is on by default, with a Breaking line.

**Output.** `barca get` prints artifact paths by default; `-o value` prints the value and `-o path` the path, with a small human preview and no size flag. Agents are expected to compose `barca sql` and the paths. Partitions are selected with `--partition key=value,value`, or a JSON object for compound keys; a partitioned target returns every selected key. A step's prints go to stderr prefixed with the node id, never to stdout under `--json`; `barca logs <run_id>` shows the captured lines. Partial failure of a partitioned target has no new surface yet: failed keys have no artifact and are recomputed by the next request.

**Store and backup.** A store without a server is this machine's backup and read-through cache. The metadata database is backed up on the minute checkpoint and at run end, from `serve` too, five copies rotating in the store and in `.barca/backups/`. `barca state restore [--from N]` is explicit, never automatic, and moves the current database aside first. `optimistic` mode and pull-before-run are removed with a Breaking line. `cached` with a store means local exists, the store object exists and the content hashes agree; anything else is `unsynced` with a reason, and `get` repairs it from the owner's copy without recomputing.

**Logs under serve.** Structured logs are a `serve`-only opt-in, `BARCA_LOG=json`: one JSON object per line with `level`, `run_id`, `node` and the trace and span ids when telemetry is on, on stdout. The CLI contract for `get` and `run`, progress and errors on stderr, is unchanged. Settled 2026-10-20 (#434).

**Cache size.** `.barca/` has a budget, `[cache] max_size` or `BARCA_CACHE_MAX_SIZE`, defaulting to 10% of its filesystem; `"off"` removes it. Past it barca evicts leftovers, superseded results, local copies the store holds, then the least recently read results, down to 80% of the limit (a fixed low watermark, not configurable), and never the command's own outputs, in-flight inputs, a storeless `Manual` result or a pending upload. No free-space floor. Settled 2026-10-20 (RFC-0012).

**Slack.** `BARCA_SLACK_WEBHOOK_URL` turns on posts to a Slack incoming webhook; the default mode is `changes` (a job starts failing, or recovers); a job that keeps failing is reminded every 24 hours, fixed; no post on an unclean `serve` restart; no job filter in v1. Shipped in 0.26.0. Settled 2026-10-20 (RFC-0010).

**Signed URLs.** `barca.signed_url(uri)` returns a link that expires after 1 hour by default and at most 7 days, with `Content-Disposition: attachment; filename=<node>.<ext>`; `barca.get` keeps returning values. Shipped in 0.26.0. Settled 2026-10-20 (RFC-0011).

**Store reasons and partial keys.** A result only on this machine is `unsynced` with `cache.reason: "local_only"` in status JSON (RFC-0009's `why` wording is amended to match). A partitioned target with failed keys exits 1 and returns only the keys that succeeded, experimentally. Settled 2026-10-20.

**Heavy steps.** A per-node `weight=N` (default 1) on `@asset`, `@task` and `@sensor`, against a capacity that is the pool size unless `[execution] capacity` or `BARCA_CAPACITY` sets it; not a concurrency group, not a memory budget from recorded RSS. A step heavier than the capacity runs alone, with a warning; the ready queue stays first come, first served, so a heavy step is not overtaken; partition keys and `parallel()` branches take their node's weight. Experimental. Settled 2026-10-21 (#436).

**Server mode.** Sharing state means a server. The server owns the ledger (run hashes, artifact paths, run history, schedules), holds the deployed code and runs schedules from it, and is the only writer of metadata. `barca get` runs locally by default: it asks the server's ledger, fetches hits from the store, computes misses locally, uploads them to the store and then posts the rows; the server checks the object and its checksum before accepting a row. `--server` runs the job on the server. A configured server that cannot be reached is exit 3 naming it; `--local` runs against the local ledger with nothing pushed. A connection dropped mid-run is an infra error naming the run id. Otherwise local and server runs print the same thing.

## Heard, not planned

Named refresh groups; a schema contract on sinks; OTLP export; email notifications; user-recorded metrics; TTL and code-insensitive freshness; filtered `collect()`; tag and resume-from scopes; a per-project setup hook; real-cloud CI. Each has a closed issue labelled `parked` with the original write-up. Reopen one when it becomes the next most important thing, not before.

## How work is tracked

- An issue states one observable problem and what done looks like. No process vocabulary, no ledger of CI runs.
- Labels: `bug` or `enhancement`, one `area:*`, `parked` for closed ideas. Nothing else.
- Milestones are releases, one per row of the release sequence, with a due date. The priorities above are the order within and between them, not milestones. A decision in "Decided" is the answer to a design question; reopen it with a reason, not a re-ask. A question nobody has answered is listed under "Open questions" with the release it must be decided by.
- Design that touches the public surface is an RFC on the site. A design PR is decided within a week: merged as Accepted, or closed with a sentence saying why.
- `specs/README.md` inventories the behavioral and interface specifications, proposed designs and engineering notes. Implementation evidence lives in the PR that made the change.
