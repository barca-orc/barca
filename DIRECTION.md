# Direction

Last reset: 2026-10-10, at v0.22.0; re-triaged 2026-10-20 at v0.24.1, and re-planned the same day at v0.24.2 for three new requests. The pinned [Direction issue](https://github.com/barca-orc/barca/issues/395) mirrors this file; it replaced the roadmap issue (#344) and the adoption tracker (#108).

## What barca is

An embedded orchestrator. You mark Python functions with `@asset`, `@sensor`, `@task` and `@sink`, barca reads the source without importing it, runs what is stale in worker processes on this machine, and caches results under `.barca/`. It is a binary and a directory, like DuckDB or SQLite, not a service you deploy.

`barca serve` is the same binary kept running: cron schedules, an HTTP API and the web UI, and the deployed copy of the pipeline code. **Server mode** means the server owns the ledger, the one record of which results exist, so that several people get the same data. A laptop runs its own work, uploads the artifacts to the store and tells the server; a `--server` run happens on the server itself. Either way every run is one machine, and the server only arbitrates what exists. One ledger, many single-node runs.

## Standing non-goals

- Distributing one run across machines. A remote store holds artifacts and a backup; the server holds the ledger.
- Sharing state without a server. Two machines writing one bucket is not supported; a store without a server is one machine's backup and cache.
- Running code sent by a client on the server (experiments, overlays, namespaces).
- Built-in authentication. The server sits behind a proxy that authenticates.
- More than one project root per server.
- A worker pool or engine shared across runs; warm workers across runs.
- Approval gates or workflows that wait on external events.

## Now, in order

An issue's milestone is the release it ships in; the items below are the order.

1. **Correctness and recovery.** Shipped at 0.24.2: the last two hash holes (#295), a worker killed by the kernel named by its signal (#433), failed steps' real duration in telemetry (#437), a tick that duplicated an API-started run (#432). Left: superlinear cold partition scaling (#345, fixed in #456), a partitioned target that returns one key (#287), and a disk that `.barca/` fills because nothing is ever deleted (#457). The corrupt shared-history blob (#243) closes when the pull is removed (#403).
2. **Everyday use.** Left: paths by default in `barca get` output (#404), a remote store that fails soft after the work is done (#390), a metadata backup with a restore command (RFC-0009, #403), signed links to results in a bucket (RFC-0011, #452), a Slack message when a job starts failing or recovers (RFC-0010, #450), logs you can join to a trace under `serve` (#434), current numbers on the comparison pages (#65). Later: a limit on how many heavy workers run at once (#436); UI work once it is defined.
3. **Server mode.** The server is the ledger (RFC-0007): drain on shutdown (#190), one run contract over HTTP (#393) with pause and resume for a schedule (#435), `--server` on the CLI with a parity suite (#394), and local runs that read the server's ledger and write back to it (#402).
4. **Freshness under serve.** `Always` and `Manual` get their run-time meaning (RFC-0008, #253): a tick reconciles the `Always` nodes below it, `Manual` pins itself, failures retry up to `max_failures`.

### Release sequence

Each release is a GitHub milestone with a due date; the milestone page is the schedule. Each minor carries its breaking changes and is followed by a cleanup release (below). Dates are targets, not promises.

| Milestone | Due | Theme | Issues, in order |
|---|---|---|---|
| 0.24.2 | shipped 2026-10-20 | Patch: the three production reports | #437, #432, #433, #295, #439 |
| 0.24.3 cleanup | 2026-10-30 | Linear partition scaling, current comparison numbers, last Dependabot alert | #345 (#456), #65, #445, #448 |
| 0.25.0 | 2026-11-06 | Breaking: output shape, and a size limit for `.barca` | #287 with #404 (#447), #457 |
| 0.25.1 cleanup | 2026-11-13 | | |
| 0.26.0 | 2026-11-27 | Breaking: the store is a backup, `optimistic` is gone; signed URLs; Slack | #390 (#444), #403 (closes #243), #452, #450 |
| 0.26.1 cleanup | 2026-12-04 | Delete the state-sync modules and optimistic paths | |
| 0.27.0 | 2026-12-30 | The server is the ledger | #190, #393 with #435, #394, #402; #250 small fixture and #434 alongside |
| 0.27.1 cleanup | 2027-01-07 | Drop the route aliases from #393 | |
| 0.28.0 | 2027-01-28 | Breaking: freshness propagation | #253 |

**Why this order for the new requests.** The disk limit (#457) is a production outage, so it joins the next minor. Signed URLs (#452) read the store states #444 and #403 define, so they ship with them. Slack (#450) moves ahead of server mode into 0.26.0: it is what tells a team a deployed job is broken, and 0.27.0 is already the largest release. Whichever of #450 and #394 lands first brings the HTTPS client; the other reuses it.

An open issue with no milestone is unscheduled on purpose (#436); it is scheduled when it becomes the next most important thing.

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

- **Drain defaults** (#190): the default grace, the config key (`[server] drain_timeout` is proposed), and the exit code after a forced stop. By 0.27.0.
- **Pause persistence** (#435): whether a paused schedule stays paused across a server restart. Proposed yes. By 0.27.0.
- **Route aliases and resume** (#393): how long the old routes stay as aliases, and when `Last-Event-ID` resume replaces the drop-is-infra-error rule. By 0.27.0.
- **Fixture home** (#250): `examples/planning_pipeline/` or `benchmarks/planning_pipeline/`. By 0.27.0.
- **Heavy-worker limit** (#436): a per-node weight, a concurrency group, or a memory budget from recorded RSS. Unscheduled; decided when scheduled.

## Decided, October 2026

The design decisions behind items 2 to 4, settled on 2026-10-14. The RFCs carry the detail; this is the list to check a proposal against.

**Freshness.** A scheduled tick brings its node and every `Always` node below it up to date in one run. `Manual` means refreshed by hand only: it is never recomputed by a tick or by a downstream `get`, and it does not block what is below it, which reads its current result. A `Manual` node that has never been computed makes a downstream request a usage error with a remediation line. A failed node retries at every tick up to `Schedule(..., max_failures=N)`. A new task fires at the first tick that reaches it. Propagation is on by default, with a Breaking line.

**Output.** `barca get` prints artifact paths by default; `-o value` prints the value and `-o path` the path, with a small human preview and no size flag. Agents are expected to compose `barca sql` and the paths. Partitions are selected with `--partition key=value,value`, or a JSON object for compound keys; a partitioned target returns every selected key. A step's prints go to stderr prefixed with the node id, never to stdout under `--json`; `barca logs <run_id>` shows the captured lines. Partial failure of a partitioned target has no new surface yet: failed keys have no artifact and are recomputed by the next request.

**Store and backup.** A store without a server is this machine's backup and read-through cache. The metadata database is backed up on the minute checkpoint and at run end, from `serve` too, five copies rotating in the store and in `.barca/backups/`. `barca state restore [--from N]` is explicit, never automatic, and moves the current database aside first. `optimistic` mode and pull-before-run are removed with a Breaking line. `cached` with a store means local exists, the store object exists and the content hashes agree; anything else is `unsynced` with a reason, and `get` repairs it from the owner's copy without recomputing.

**Logs under serve.** Structured logs are a `serve`-only opt-in, `BARCA_LOG=json`: one JSON object per line with `level`, `run_id`, `node` and the trace and span ids when telemetry is on, on stdout. The CLI contract for `get` and `run`, progress and errors on stderr, is unchanged. Settled 2026-10-20 (#434).

**Cache size.** `.barca/` has a budget, `[cache] max_size` or `BARCA_CACHE_MAX_SIZE`, defaulting to 10% of its filesystem; `"off"` removes it. Past it barca evicts leftovers, superseded results, local copies the store holds, then the least recently read results, down to 80% of the limit (a fixed low watermark, not configurable), and never the command's own outputs, in-flight inputs, a storeless `Manual` result or a pending upload. No free-space floor. Settled 2026-10-20 (RFC-0012).

**Slack.** `BARCA_SLACK_WEBHOOK_URL` turns on posts to a Slack incoming webhook; the default mode is `changes` (a job starts failing, or recovers); a job that keeps failing is reminded every 24 hours, fixed; no post on an unclean `serve` restart; no job filter in v1. Shipped in 0.26.0. Settled 2026-10-20 (RFC-0010).

**Signed URLs.** `barca.signed_url(uri)` returns a link that expires after 1 hour by default and at most 7 days, with `Content-Disposition: attachment; filename=<node>.<ext>`; `barca.get` keeps returning values. Shipped in 0.26.0. Settled 2026-10-20 (RFC-0011).

**Store reasons and partial keys.** A result only on this machine is `unsynced` with `cache.reason: "local_only"` in status JSON (RFC-0009's `why` wording is amended to match). A partitioned target with failed keys exits 1 and returns only the keys that succeeded, experimentally. Settled 2026-10-20.

**Server mode.** Sharing state means a server. The server owns the ledger (run hashes, artifact paths, run history, schedules), holds the deployed code and runs schedules from it, and is the only writer of metadata. `barca get` runs locally by default: it asks the server's ledger, fetches hits from the store, computes misses locally, uploads them to the store and then posts the rows; the server checks the object and its checksum before accepting a row. `--server` runs the job on the server. A configured server that cannot be reached is exit 3 naming it; `--local` runs against the local ledger with nothing pushed. A connection dropped mid-run is an infra error naming the run id. Otherwise local and server runs print the same thing.

## Heard, not planned

Named refresh groups; a schema contract on sinks; OTLP export; email notifications; user-recorded metrics; TTL and code-insensitive freshness; filtered `collect()`; tag and resume-from scopes; a per-project setup hook; real-cloud CI. Each has a closed issue labelled `parked` with the original write-up. Reopen one when it becomes the next most important thing, not before.

## How work is tracked

- An issue states one observable problem and what done looks like. No process vocabulary, no ledger of CI runs.
- Labels: `bug` or `enhancement`, one `area:*`, `parked` for closed ideas. Nothing else.
- Milestones are releases, one per row of the release sequence, with a due date. The four items above are the order within and between them, not milestones. A decision in "Decided" is the answer to a design question; reopen it with a reason, not a re-ask. A question nobody has answered is listed under "Open questions" with the release it must be decided by.
- Design that touches the public surface is an RFC on the site. A design PR is decided within a week: merged as Accepted, or closed with a sentence saying why.
- `specs/` holds the four boundary specifications and their index. Implementation notes live in the PR that made them.
