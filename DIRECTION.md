# Direction

Last updated: 2026-11-04, preparing v0.39.5 (the 0.39 cleanup, #800) after v0.39.4. The pinned
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

An issue's milestone is the release it ships in; the items below are the order.

The 0.39 cleanup ([#800](https://github.com/barca-orc/barca/issues/800), milestone `0.39 cleanup`,
shipping as 0.39.5) sets this order. It adds no commands, flags, keys, routes, decorator arguments
or UI features; #800 has the detail of each item.

1. **Correctness:** #799, a refresh cascade that left stale partition caches, is fixed with a test, or its workaround is documented and the issue stays open (#804).
2. **Measure before deciding:** benchmark 0.39.4, attribute the trivial run's time, price the 0.35.3 partition sort, gate performance in the merge tier and put the measured number in the README (#810: a trivial run takes 31.7 ms on the 0.39.4 wheel, against 26.3 ms at 0.1.5 and 60.3 ms at 0.28.0).
3. **Make the documents true:** this file, the #796 decision, one alias lifetime, `contract.md` hygiene, the experimental graduation list, an environment-variable contract test, the static-read claim, `specs/README.md` and the milestones (#803).
4. **Structure, in dependency order:** one `CacheState` enum (#802), one metadata write boundary, serve out of the embedded hot loop, `db.rs` split by table family, one graph cache and one backup-cadence authority, the Python dedupe and `.barca/` layout spec (#805).
5. **Tests and CI:** per-test timeouts, Playwright and type drift in CI, the simulation's `--strict-reads` failure, optional imports that skip (#806), isolated backup tests (#809), Dependabot at zero critical and high (#808).

0.39.5 ships when items 1 to 3 are done and the `CacheState` enum at minimum; the rest of item 4
can roll into 0.39.6, and nothing new lands until it is done. The UI redesign comes after
(Decided, November 2026).

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
| 0.39 cleanup (shipping as 0.39.5) | planned | No features: #799, measured overhead and a performance gate, documents and milestones made true, structure, tests and CI | #800, #799, #807 |

An open issue with no milestone is unscheduled on purpose (for example the production feedback in #666 to #676); it is scheduled when it becomes the next most important thing.

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

None at 0.39.5.

Settled by what shipped, and documented in the manual: drain defaults (#190: `--drain-timeout` and `[server] drain_timeout`, 30 s, exit 1 after a forced stop; 0.27.0), pause persistence (#435: a pause survives a restart; 0.27.0) and the fixture home (#250: `benchmarks/planning_pipeline/`; 0.29.0).

## Decided, November 2026

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
does not meet this, so the redesign is scoped after 0.39.5; #807 tracks the stale Playwright
specs.

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

**Freshness.** A scheduled tick brings its node and every `Always` node below it up to date in one run. `Manual` means refreshed by hand only: it is never recomputed by a tick or by a downstream `get`, and it does not block what is below it, which reads its current result. A `Manual` node that has never been computed makes a downstream request a usage error with a remediation line. A failed node retries at every tick up to `Schedule(..., max_failures=N)`. A new task fires at the first tick that reaches it. Propagation is on by default, with a Breaking line.

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
