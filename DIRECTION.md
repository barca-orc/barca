# Direction

Last reset: 2026-10-10, at v0.22.0. The pinned [Direction issue](https://github.com/barca-orc/barca/issues/395) mirrors this file; it replaced the roadmap issue (#344) and the adoption tracker (#108).

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

1. **Correctness and recovery.** Wrong cache results and lost history come first: upload receipts (#381), hash holes that serve stale results (#295), a corrupt shared-history blob destroying every machine's copy (#243), a killed run that stays `running` (#290), a partitioned target that returns one key (#287).
2. **Everyday use.** What the two real users asked for: per-step timings in the run output (#386), task output you can use (#387), `barca doctor` and one documented local profile (#388), a one-line failure summary (#389), a remote store that fails soft after the work is done (#390), sane `barca list` defaults (#391), a warning when a recompute produces different bytes (#392), paths by default in `barca get` output, and a metadata backup with a restore command in place of the shared-history blob (RFC-0009).
3. **Server mode.** The server is the ledger (RFC-0007): one run contract over HTTP (#393), `--server` on the CLI with a parity suite (#394), and local runs that read the server's ledger and write back to it.
4. **Freshness under serve.** `Always` and `Manual` get their run-time meaning (RFC-0008, #253): a tick reconciles the `Always` nodes below it, `Manual` pins itself, failures retry up to `max_failures`.

## Decided, October 2026

The design decisions behind items 2 to 4, settled on 2026-10-14. The RFCs carry the detail; this is the list to check a proposal against.

**Freshness.** A scheduled tick brings its node and every `Always` node below it up to date in one run. `Manual` means refreshed by hand only: it is never recomputed by a tick or by a downstream `get`, and it does not block what is below it, which reads its current result. A `Manual` node that has never been computed makes a downstream request a usage error with a remediation line. A failed node retries at every tick up to `Schedule(..., max_failures=N)`. A new task fires at the first tick that reaches it. Propagation is on by default, with a Breaking line.

**Output.** `barca get` prints artifact paths by default; `-o value` prints the value and `-o path` the path, with a small human preview and no size flag. Agents are expected to compose `barca sql` and the paths. Partitions are selected with `--partition key=value,value`, or a JSON object for compound keys; a partitioned target returns every selected key. A step's prints go to stderr prefixed with the node id, never to stdout under `--json`; `barca logs <run_id>` shows the captured lines. Partial failure of a partitioned target has no new surface yet: failed keys have no artifact and are recomputed by the next request.

**Store and backup.** A store without a server is this machine's backup and read-through cache. The metadata database is backed up on the minute checkpoint and at run end, from `serve` too, five copies rotating in the store and in `.barca/backups/`. `barca state restore [--from N]` is explicit, never automatic, and moves the current database aside first. `optimistic` mode and pull-before-run are removed with a Breaking line. `cached` with a store means local exists, the store object exists and the content hashes agree; anything else is `unsynced` with a reason, and `get` repairs it from the owner's copy without recomputing.

**Server mode.** Sharing state means a server. The server owns the ledger (run hashes, artifact paths, run history, schedules), holds the deployed code and runs schedules from it, and is the only writer of metadata. `barca get` runs locally by default: it asks the server's ledger, fetches hits from the store, computes misses locally, uploads them to the store and then posts the rows; the server checks the object and its checksum before accepting a row. `--server` runs the job on the server. A configured server that cannot be reached is exit 3 naming it; `--local` runs against the local ledger with nothing pushed. A connection dropped mid-run is an infra error naming the run id. Otherwise local and server runs print the same thing.

## Heard, not planned

Named refresh groups; a schema contract on sinks; OTLP export; Slack and email notifications; user-recorded metrics; TTL and code-insensitive freshness; filtered `collect()`; tag and resume-from scopes; a per-project setup hook; `barca gc`; real-cloud CI. Each has a closed issue labelled `parked` with the original write-up. Reopen one when it becomes the next most important thing, not before.

## How work is tracked

- An issue states one observable problem and what done looks like. No process vocabulary, no ledger of CI runs.
- Labels: `bug` or `enhancement`, one `area:*`, `parked` for closed ideas. Nothing else.
- Milestones match the four items above. A decision in "Decided" is the answer to a design question; reopen it with a reason, not a re-ask.
- Design that touches the public surface is an RFC on the site. A design PR is decided within a week: merged as Accepted, or closed with a sentence saying why.
- `specs/` holds the four boundary specifications and their index. Implementation notes live in the PR that made them.
