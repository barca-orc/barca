# Direction

Last reset: 2026-10-10, at v0.22.0. The pinned [Direction issue](https://github.com/barca-orc/barca/issues/395) mirrors this file; it replaced the roadmap issue (#344) and the adoption tracker (#108).

## What barca is

An embedded orchestrator. You mark Python functions with `@asset`, `@sensor`, `@task` and `@sink`, barca reads the source without importing it, runs what is stale in worker processes on this machine, and caches results under `.barca/`. It is a binary and a directory, like DuckDB or SQLite, not a service you deploy.

`barca serve` is the same binary kept running: cron schedules, an HTTP API and the web UI. **Server mode** means the CLI can point at that one process (`--server <url>`) and every command runs there and prints here. It is still one node. The node happens to be a server.

## Standing non-goals

- Distributing one run across machines. A remote store shares results, nothing else.
- Running code sent by a client on the server (experiments, overlays, namespaces).
- Built-in authentication. The server sits behind a proxy that authenticates.
- More than one project root per server.
- A worker pool or engine shared across runs; warm workers across runs.
- Approval gates or workflows that wait on external events.

## Now, in order

1. **Correctness and recovery.** Wrong cache results and lost history come first: upload receipts (#381), hash holes that serve stale results (#295), a corrupt shared-history blob destroying every machine's copy (#243), a killed run that stays `running` (#290), a partitioned target that returns one key (#287).
2. **Everyday use.** What the two real users asked for: per-step timings in the run output (#386), task output you can use (#387), `barca doctor` and one documented local profile (#388), a one-line failure summary (#389), a remote store that fails soft after the work is done (#390), sane `barca list` defaults (#391), a warning when a recompute produces different bytes (#392).
3. **Server mode.** One run contract over HTTP (#393) (start a run with the CLI's flags, a run id that survives restart, an event stream the client can print), then `--server` on the CLI (#394), with a parity suite that proves local and remote print the same thing.
4. **Freshness under serve.** Decide what `Always` and `Manual` do when a schedule ticks (RFC-0008) (#253). Today only `Schedule` does anything, and the docs say so.

## Heard, not planned

Named refresh groups; a schema contract on sinks; OTLP export; Slack and email notifications; user-recorded metrics; TTL and code-insensitive freshness; filtered `collect()`; tag and resume-from scopes; a per-project setup hook; `barca gc`; real-cloud CI. Each has a closed issue labelled `parked` with the original write-up. Reopen one when it becomes the next most important thing, not before.

## How work is tracked

- An issue states one observable problem and what done looks like. No process vocabulary, no ledger of CI runs.
- Labels: `bug` or `enhancement`, one `area:*`, `parked` for closed ideas. Nothing else.
- Milestones match the four items above.
- Design that touches the public surface is an RFC on the site. A design PR is decided within a week: merged as Accepted, or closed with a sentence saying why.
- `specs/` holds the four boundary specifications and their index. Implementation notes live in the PR that made them.
