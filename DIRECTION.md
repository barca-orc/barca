# Direction

Last updated: 2026-10-30, preparing v0.36.0 after v0.35.3. The pinned
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
- A worker pool or engine shared across runs; warm workers across runs.
- Approval gates or workflows that wait on external events.

## Now, in order

An issue's milestone is the release it ships in; the items below are the order.

The decisions in [#707](https://github.com/barca-orc/barca/issues/707) set this order:

1. **Shipped in 0.35.2: inspection and everyday server use.** Includes #708 (read-only WAL
   inspection, bounded history queries, work off request threads), #710 (metadata before
   scoped status), #709 (grouped-page and node-panel run actions), and #706's credential
   masking, first-use documentation (#650) and concise help (#651).
   Rechecked #654–656 against the newer implementation. The evidence in
   #716 still reproduces duplicate overlapping asset work, large bootstrap memory and
   expensive cold/post-run inspection; those issues stay open with current measurements.
   The real planning fixture has 116 nodes and scales table rows; its smoke run is separate
   from the synthetic 1,000-node/1.2M-key coordinator workload.
2. **Shipped as 0.35.3: stable streamed partition artifacts (#721).** A batched DuckDB, Arrow or Polars
   result with the same rows in another physical order produced different Parquet bytes, so an
   unchanged refresh reran its cutoff consumers (26 of 32 keys on a million-row fixture). Each
   streamed partition is now sorted by all columns before it is hashed. The first refresh after
   upgrading rewrites existing partition artifacts and reruns their consumers once.
3. **0.36.0: server mode behind a proxy (#706).** RFC-0015's configurable backup cadence
   and separate local-backup accounting are implemented, awaiting this release. Complete
   release validation of the implemented client credentials, UI token management and
   `Last-Event-ID` stream resume. Include the cost of
   hashing and remotely checking every artifact during status. Userinfo remains supported;
   bearer credentials reach every client request. The user chose barca-issued and
   barca-validated UI tokens in an opt-in mode with no user-account system (#187). RFC-0016
   specifies bootstrap access, browser security and revocation; implementation and conformance
   are merged. Publication remains required.
4. **Then the cleanup week.** Triage #666 into 0.36.1 or 0.37.0: a failed key must not stop
   independent keys' downstream work. Batch the small feedback in #668, #671, #672 and #673.
   Keep #674 (incremental fan-ins) parked pending discussion, alongside #667, #669, #670,
   #675 and #676. Final scheduling follows the 0.36.0 RFC decisions.

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
| 0.36.0 | planned | Configurable backups, proxy credentials and UI token management, resumable streams and store inspection costs | #706, #187 |

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

- **Store inspection, due by 0.36.0 (#706):** bound/reuse artifact
  integrity evidence without making `cached` mean unchecked. Record the cost as the DB grows.

Settled by what shipped, and documented in the manual: drain defaults (#190: `--drain-timeout` and `[server] drain_timeout`, 30 s, exit 1 after a forced stop; 0.27.0), pause persistence (#435: a pause survives a restart; 0.27.0) and the fixture home (#250: `benchmarks/planning_pipeline/`; 0.29.0).

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
