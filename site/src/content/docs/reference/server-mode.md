---
title: "Server Mode: a Shared Ledger and --server"
description: "One barca serve as the team's ledger: laptops read its cache hits and post their runs, and --server runs a command on it."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/server-mode.md
---

<!-- Generated from crates/barca-cli/docs/server-mode.md (barca docs server-mode). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca site_pages_tests:: (or scripts/update-cli-snapshots.sh). -->

One `barca serve` can do two things for a team. It runs commands on its own deployed copy of
the project (`--server`, first below), and it can be the ledger every laptop reads cache hits
from and records its runs in ("Local computation with shared results"). Opt-in project tokens
protect both ("Opt-in project tokens").

This is the reference. To set a team up in three steps: [Share Results With a Team](/team-server/). To run
the server as a service: [Deploying barca serve](/deploying/).

To execute a deployed project remotely, start `barca serve` there and use an explicit URL:

```bash
barca get daily_report --server http://barca.internal:8274
barca run deploy --server http://barca.internal:8274
barca history --server http://barca.internal:8274
```

This runs the server's code and keeps history there; it does not upload local edits.
No shared artifact store is needed for this path. Turn on project tokens ("Opt-in project
tokens" below), or keep the server on a private network or behind an authenticating proxy
([Deploying barca serve](/deploying/)).

## In the server's own project: the project socket

A `barca` command typed in the project directory a `barca serve` is running in goes to that
server. Nothing is configured: the server listens on a Unix socket beside its metadata
database, `.barca/serve.sock` (`.barca/envs/<env>/serve.sock` for `--env <env>`), and a
command finds it there. The server says so when it starts, and the command says so once on
stderr:

```
[barca] project socket .barca/serve.sock (barca commands in this project use this server; --local runs them here)
[barca] server .barca/serve.sock (this project's barca serve; --local runs on this machine)
```

Why: the server is the one process that writes this project's metadata database. A second
process beside it has to wait for the database's lock each time the server writes, and behind
a server that is recording runs that wait can end after 60 seconds in
`timed out ... waiting for .barca/metadata.db.lock` (the error names the process holding the
lock: its pid and command line, on Linux).

With a server running here, and neither `--local` nor `--server` on the command line:

| Command | What happens |
|---|---|
| `status`, `history`, `stats`, `logs`, `queue`, `pause`, `resume`, `pin`, `unpin`, `cancel` | answered by, or sent to, the server, as with `--server` |
| `sql` | a query over barca's own tables is answered by the server; a query over results runs here, over the artifacts in `.barca/` |
| `get`, `run` | join the server's run queue; the command follows the run and prints its result, as with `--server` |
| `gc` | run by the server (`POST /gc`), between its own writes |
| `state restore`, `state reset` | refused (exit 3), naming the server: stop it first, or for a reset use `POST /state/reset` or the web UI |
| `list`, `plan`, `doctor`, `docs`, `tokens`, `state backups`, `history --reconcile` | run here, as without a server |

```bash
barca get daily_report            # queued on this project's server; its result prints here
barca status                      # the server's answer
barca gc                          # the server evicts, between its own writes
barca get daily_report --local    # run in this process instead, beside the server
barca gc --local --dry-run        # this process measures and plans the eviction itself
```

What that means for `get` and `run`:

- **The server runs its deployed files**, the ones it loaded when it started (or reloaded with
  `--watch`), not an edit it has not loaded. A command that names no file uses all of them; a
  file the server did not deploy is a usage error listing the deployed files. To run an edit
  the server has not loaded, pass `--local`.
- The run waits for a run slot like any other (`barca queue`), and Ctrl-C cancels it on the
  server.
- `-o url` is not available on a server yet; pass `--local` for it.
- A command a step starts (`barca.get(...)` inside a task) does not go to the server: it runs
  in the step's own process, as it did before the socket, so it never waits for a run slot its
  caller holds.

`--local` runs any of these in the command's own process, as before the socket existed. It
still shares the database lock with the server, so behind a busy server it can wait.
`--server URL` names a server explicitly and is unchanged. The socket takes precedence over a
configured server (`BARCA_SERVER`, `[server] url`).

**No token.** The socket is owner-only (mode `0600`). A command sent over it needs no `BARCA_SERVER_TOKEN`, even when the server has project
tokens on, because a user who can open the socket can already write the database beside it.
The server's TCP address asks for a token as before.

**A read-only server** (`barca serve --read-only`) opens the socket too. Reads are answered by
it; a write (`get`, `run`, `gc`, `pause`, `pin`, ...) is refused with its read-only error and a
pointer to `--local`. Nothing falls back to this machine on its own.

**No socket, or a dead one.** With no server there is no socket file and the check costs one
or two `stat` calls. A server that was killed (SIGKILL, a crash, a power cut) leaves its
socket file behind; nothing listens on it, so a command ignores it and runs here, and the next
server on the project replaces it. A socket that accepts a connection but does not answer
within 5 seconds is ignored with a warning. A server stopped normally removes its socket.

Limits: only the server that holds the environment (the first one started on it) opens the
socket, so a second server on the same environment is reachable by its URL only. A project
path so long that the socket path exceeds what the system allows (107 bytes on Linux, 103 on
macOS) leaves the server without a socket; it warns at startup, and commands then behave as
before. Not run so far: a second user of the machine, another container sharing the project
directory, a network filesystem, and macOS. There is no route for taking a metadata backup on
request; the server takes its own ([Metadata Backups](/reference/backup/)).

## Local computation with shared results

Sharing results between machines means one `barca serve` that everyone points at. **The server
is the ledger**: it holds the one record of which results exist (run hashes, artifact paths, run
history) and it is the only process that writes it. The artifact bytes stay in the store
(`[remote]`, [Remote Storage](/reference/remote-storage/)); the server never moves them. Server mode is experimental
([CLI Contract](/reference/cli-contract/)). To set it up in three steps, see
[Share Results With a Team](/team-server/).

Server mode remains experimental after 0.36.0. Runs of one server compute a matching asset step
once, experimentally since 0.39.4 ([Operating barca serve: Runs share the steps they have in common](/reference/serve-ops/#runs-share-the-steps-they-have-in-common)): cancelling the run that computes it makes the waiting run execute it again, the result
reaches the waiting run before the computing run's row is durable, and a run that decided the step
before another run finished it can execute it a second time. A stable designation
also requires closing those gaps (#654), bounded cold/full-domain planning and
inspection (#655, #656), and sustained multi-host proxy/store validation at the supported
scale. Opt-in tokens, bounded event replay and configurable backups address deployment
capabilities; they do not establish those remaining properties. Tokens grant full project
access, without accounts, roles or tenant isolation.

With a server configured, a command does one of three things:

| You type | `get` / `run` | `status` / `history` / `stats` | `sql` |
|---|---|---|---|
| nothing extra | run on this machine, against the server's ledger | answered by the server | views from the server's ledger, query run here |
| `--server [<url>]` | run on the server, printed here | answered by the server | views from that server, query run here |
| `--local` | run on this machine against its own ledger; nothing is read from or sent to the server | answered from this machine's records | this machine's results |

```bash
barca get daily_report                        # here; cache hits come from the server's ledger
barca get daily_report --dry-run              # what would run, decided against the ledger
barca run deploy                              # a task, run here, recorded on the server
barca get daily_report --server               # run it on the configured server
barca get daily_report --server https://barca.internal.example.com
barca run deploy --server --agent             # a task on the server, with the agent lines
barca status                                  # the server's ledger, and what is here
barca history -l 20                           # the server's runs
barca stats daily_report                      # timings from the server
barca sql "select * from daily_report"        # the team's result, queried here
barca sql "select * from barca.runs" --local  # this machine's own runs
barca status --problems                       # what needs attention on the server
barca doctor                                  # this machine and the server: version, store, load errors, schedules
barca list --server --json                    # the deployed nodes
barca get daily_report --local                # this machine only; the server is not touched
```

## Which server

Configure it once in `barca.toml`, with the store the server uses:

```toml
# barca.toml on a laptop
[server]
url = "https://barca.internal.example.com"

[remote]
uri = "s3://team-bucket/barca"     # the store the server uses
```

or per shell with `export BARCA_SERVER=https://barca.internal.example.com`. `BARCA_SERVER`
beats `[server] url`; an empty `BARCA_SERVER=` counts as unset. `--server <url>` names a server
for one command. The URL is `http://` or `https://`, with any path prefix a proxy adds
(`https://host.example.com/barca`).

The server is an ordinary `barca serve` of the project with the same `[remote]` and no
`[server]` section ([Scheduling With Cron](/scheduling/), "Calling the server").

- A command that uses the configured server prints `[barca] server <url> (<where it came
  from>)` once on stderr. A URL typed after `--server` is not announced.
- `--server` with no URL uses the configured server. With nothing configured it is a usage
  error (exit 2).
- `--server` followed by a target or a flag is bare `--server`: `barca get --server total` gets
  `total` from the configured server. A word with `://` after it is its URL, which must start
  with `http://` or `https://`.
- `--local` ignores the configured server for one command; `--local` with `--server` is a usage
  error.
- `--server` and `--local` apply to `get`, `run`, `status`, `list`, `history`, `stats`, `logs`,
  `sql`, `queue`, `pause`, `resume`, `cancel`, `pin`, `unpin`, `gc` and `doctor`. `plan`,
  `state`, `serve`, `tokens`, `docs` and `version` always run on this machine;
  `--server` or `--local` on them is a usage error. `barca list` without `--server` reads this
  machine's files. `barca sql` runs a query over results on this machine: a server only says
  which results its views read ([barca sql: With a server](/reference/cli/sql/#with-a-server)). The server never runs SQL
  over results or files; a query over barca's own tables alone it answers itself.
  `doctor` runs its checks here and, with a server (configured, or `--server`), checks the
  server too; `--local` leaves it out ([barca doctor: Checking a server](/reference/cli/doctor/#checking-a-server)).

## A local run against the server's ledger

Without `--server` or `--local`, `barca get` and `barca run` run on this machine with this
machine's code. In order:

1. The command prints `[barca] server <url> (<source>)` and asks `GET /health`. The server must
   speak this barca's API version and write to the same store as this project's `[remote]`.
2. Runs this machine could not post before are posted again (below).
3. One `POST /ledger/lookup` asks about every run hash of the plan that is known before anything
   runs. A step downstream of a sensor (its output is part of the hash) is asked about when its
   phase is decided, one request per such phase.
4. A hit is a cache hit as usual: its artifact is fetched from the store when something reads
   it. A local copy whose SHA-256 differs from the digest the server recorded is replaced by the
   store's copy. A miss runs here.
5. After the run's uploads are confirmed, `POST /ledger/runs` sends the run and its step rows
   (store paths and digests). The server checks that every object exists in its store with that
   digest (an object store, which has no content checksum to compare, by size) before it records
   anything, and records the run with `triggered_by: client` and this machine's run id as
   `client_run_id` (`barca history --json` shows both).

`--dry-run` is decided against the ledger and posts nothing. Only assets are posted: tasks and
sensors run here and stay in this machine's history.

The lookup is by run hash only. Two people with the same code share results; a person who edited
`report.py` gets misses for the nodes the edit changed and hits for the rest, and the rows they
post are keyed by their own hashes.

### When the post does not go through

The run is a success here whatever happens to its post: the exit code and the output do not
change. If the server refuses the post (`409`: an object is missing, outside its store, or does
not have the posted digest) or cannot be reached, the command prints

```
[barca] warning: run <id> was not recorded on the server <url>: <why>; it stays on this machine (unsynced) and the next command against the server sends it again
```

and keeps the post in this machine's database (the `ledger_posts` table, state `unsynced`).
`barca status` shows such a result as `unsynced` with `cache.reason: "not_in_ledger"` (below).
The next `get` or `run` against the server posts it before its own lookup and prints
`[barca] recorded run <id> on the server (run <server id>)`, or
`[barca] warning: run <id> is still not recorded ...` if it is refused again. A result whose
object went missing is a miss on the ledger, so the next `get` that needs it computes and posts
it again, after which the refused post is accepted too.

Only successful runs are posted. A run that failed keeps the steps that succeeded on this
machine; they are computed again by the next machine that needs them. A run some of whose
results could not be uploaded to the store (their upload is pending, [Remote Storage](/reference/remote-storage/)) is
held: it prints `[barca] warning: run <id> is not recorded on the server <url> yet: some of its
results are not in the artifact store yet ...`, and its post is kept `unsynced` (`barca status`
shows its results `not_in_ledger`) without being sent, since the server would refuse it. The
next command that opens the store uploads the pending results first and then posts the run
(`[barca] recorded run <id> on the server ...`), before it decides its own steps, which are
then hits; other machines get hits from then on. A pending result whose local file is gone is
dropped ([Remote Storage](/reference/remote-storage/)) and taken out of the post, and the rest of the run is posted.

A server that is draining before it stops (SIGTERM, [Scheduling With Cron](/scheduling/)) refuses posts with
`503`; the run stays `unsynced` here and the next command against the server sends it again.

### Backups

The server backs up its database to the store; the machines that use it take no backups. What
a restore on the server takes, and the client backups older barca versions left in the store,
are in [Metadata Backups: With a server](/reference/backup/#with-a-server).

### Two machines, same run hash, different bytes

When the ledger already holds a run hash with another digest (a function that is not
deterministic, computed twice), it keeps its row and tells the poster, which prints

```
[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic
```

once, and marks the step `bytes_changed: true` in its `--json` output. Artifacts are addressed
by run hash, so the second machine's upload has replaced the object in the store; a later fetch
of that result notes the mismatch and uses the store's copy.

## status, history, stats, logs and sql with a server

With a server configured, `barca status`, `barca history`, `barca stats` and `barca logs` answer
from the server, as `--server` does, after printing `[barca] server <url> (<source>)`. The server
answers for its deployed code. `barca logs` reads the server's runs (`GET /runs/{run_id}/logs`,
and `-f` its event stream); the lines of a run this machine ran against the ledger stay here,
with `--local` ([barca logs: Debugging a live pipeline](/reference/cli/logs/#debugging-a-live-pipeline)). Nothing falls back to this
machine, but `barca logs RUN_ID` for a run only this machine has fails (exit 2) with the
remediation `this run is on this machine: barca logs RUN_ID --local`, and `barca logs` without a
run id adds `[barca] note: this machine has a newer run <id> (...); barca logs --local reads it`
on stderr when this machine's newest run is newer than the server's.

`barca history`'s filters (`--status`, `--target`, `--triggered-by`, `--not-triggered-by`,
`--since`, `--until`, `--search`) are applied by the server
(`GET /history?status=...&target=...&triggered_by=...&not_triggered_by=...&since=...&until=...&search=...`;
a relative `--since` or `--until` counts back from the server's clock). With `--status running`
the server adds `now` to each run in flight, the nodes it is executing and how many wait, the
same object as on `GET /runs?status=running` ([barca status: Runs in progress and the last day's activity](/reference/cli/status/#runs-in-progress-and-the-last-days-activity)). `barca history --activity` reads `activity` of `GET /runs`: the runs
started in the last 24 hours counted by hour, status and trigger over every run the server
recorded in the window, which the web UI's Runs page draws as its strip.
`GET /runs` takes the same filters as query parameters (all but `target`), and its `runs`,
`total` and `activity` are of the runs they keep; its runs in progress come first, whatever
`limit` lists. `--not-triggered-by schedule`
(`GET /runs?not_triggered_by=schedule`, the page's "Hide schedule runs") leaves a
schedule's runs out of the list, its `total` and the activity. `barca history RUN_ID` reads
`GET /runs/{run_id}`: the run, its steps with their attempts, run hashes, artifacts, errors and
tracebacks (at most 1000 steps unless `--limit N` or `--all` asks for more; `steps_page` says how
many there are). For a finished run whose plan the server can still rebuild from the sources,
`steps_page.nodes` lists every planned node in plan order with a `status`: `ran`, `cached`, or
`skipped` with `blocked_by` naming the failed node upstream of it (experimental; the local
`barca history RUN_ID` lists only the nodes with recorded steps). The run also says why it
executed what it did (`run.plan_reasons`: never run, new keys, a failed last attempt, which
code, input or env component changed, a refresh, a missing artifact), who asked for it
(`principal`, the token of its request) and why (`trigger_reason`, the `reason` its
`POST /runs` gave), and for a cancelled run who cancelled it and whether it had started
(`cancellation`; [CLI Contract: history](/reference/cli-contract/#history)). `explanation` says the same in words, as
`barca history RUN_ID` prints it: the steps grouped by the reason they ran (`why`), the
retries apart from the first attempts, the targets as a list and what became of each, and for
a run in progress how far it is and an ETA at the rate so far. `GET /runs/{run_id}?offset=N` pages through
the steps (`steps_page.next_offset`), and while a run goes it shows its newest 5,000 spooled
lines in `logs`.

Two identical requests are two runs: the server does not deduplicate them. With a free run slot
the second starts at once and waits on the steps the first is computing (it takes their
results as cache hits); otherwise it waits in the queue. A run cancelled while it waits
(`DELETE /runs/{run_id}`, which the first Ctrl-C of a `--server` client sends) ends `cancelled`
with no steps, and its `cancellation` says so: `started: false`, the runs it waited behind, and
`same_as`, the one doing the same work. A run only this machine has (a failed run against the ledger is never posted) fails
(exit 2) with the remediation `this run is on this machine: barca history RUN_ID --local`.

`barca status` (without `--server`) also says what this machine holds. Each node in `--json`
gets `local`: `true` when this machine has a copy of the result the server would serve (its
mirror of the store object exists under this machine's artifact directory), else `false`.
`barca sql` queries the server's result either way, read from the store. A node the server does
not have a result for, whose result
this machine computed and could not post yet, is `unsynced` with `cache.reason:
"not_in_ledger"` and `local: true`; the table prints `unsynced  not in ledger`, and
`summary.unsynced` counts it. A result is matched by node and run hash, so one computed here
with code the server does not run keeps the server's state. A partitioned node has no single
artifact, so its `local` is `false`.

`barca sql` asks the server which result each view reads (`GET /sql/views`) and reads barca's
tables (`barca.runs`, `barca.materializations`, `barca.nodes`, `barca.schedules`, `barca.ticks`,
`barca.queue`) from its records, then runs the
query here over the store; the server never runs SQL over results or files ([barca sql](/reference/cli/sql/),
"With a server"). A query that names only barca's tables is sent to the server instead (`POST
/sql/query`, below). An unsynced result is not mixed into a view: the error or note says so and
names `--local`.

`barca status --problems` (or `--state <states>`) keeps the nodes that need attention, the
server's failures, stale results and failing or paused schedules among them, plus this
machine's `not_in_ledger` results; each scheduled node carries the server scheduler's view in
`schedule` ([barca status: Schedules](/reference/cli/status/#schedules)). `barca doctor` checks the server itself.

`barca history --reconcile` writes this machine's history and runs here. Add `--local` to see
this machine's own runs, statuses and timings.

## Running a command on the server: `--server`

The server runs its deployed files. Files on the command line are optional: without them the
server uses every deployed file; with them it reads only those, as a local command does, and a
directory selects the deployed files under it. A file the deployment does not have is a usage
error listing the deployed files. When a file you named differs from the deployed one, barca
prints one note and continues:

```
[barca] note: pipeline.py differs from the deployment; the server runs the deployed version
```

Your edits run only on this machine (leave out `--server`), or after you deploy them.

`--env` must name the server's environment; another is a usage error naming the server's.

### What prints

For every command the client prints what a local run against the same project and history
prints: the stdout JSON key for key (built by the same functions), the stderr lines in the same
order, the exit codes 0, 1, 2, 3 and 130. `--json`, `--pretty`, `-o path`, `-o value` and `-o
json|pretty` on `run`, `--fields`, `--limit` and `--agent` apply here as they do locally. A
`get` prints the server's artifact paths (paths by default, as `barca get --help` says): its
`--json` `final_output` is the server's `_barca_artifact` pointer. With `-o value` the server
reads the values (`RunRequest.values`) and they print as locally, partitioned targets included.
`--partition` selects keys across the cone as locally, on `get` and `run` (`RunRequest.partition`).
`get -o url` is a usage error
(exit 2) with `--server`: run the command with `--local`. A step's own output (`[node] line`) is
printed on stderr as a local run prints it.

The declared differences:

- `root`, artifact paths, `artifact_dir` and traceback file names are the server's.
- `triggered_by` in `barca history --json` is `api` for a run started with `--server` (a
  command typed here records `manual`).
- The command of a root's action in `barca status --rollup` (`roots[].action.command`) ends
  in `--server`, so that the run it starts joins the server's queue.
- The "no .py files" usage error does not occur: the server has its files.
- A local run in a terminal shows a progress bar. The client prints the lines a bar leaves
  behind (warnings, notes, the end-of-run line) and, with `--agent`, the agent lines.
- Lines the Python workers write to stderr themselves (`[barca] importing <module>`, and
  anything a step writes to stderr directly) stay on the server's stderr. A step's printed
  output (stdout) is streamed and printed here as `[node] line`.
- The `[barca] env <env> (...); artifact store <store>` startup line describes this machine's
  configuration and is not printed for a `--server` command.
- In human mode an error's text is rebuilt from the envelope's fields; in JSON mode the
  envelope is the same.
- `barca status --server` has no `local` key: it reports the server alone.
- Status `disk_usage` describes the storage and cache budget of the machine serving the
  request, across all of its project environments. A local project copy can have different
  budgeted, backup and physical byte counts, limits or unmeasured values while its graph and
  cached results are equivalent. The accounting relationship and JSON fields stay the same.

The parity suite (`python/tests/test_server_mode_parity.py`) runs the contract's case table
once locally and once through `barca serve` on a copy of the same project and compares the
normalized output with exactly these differences.

`barca status NODE --keys --server` sends its filters, order and page to the server
(`GET /assets/{name}/partitions/keys?state=failed,blocked&needs_run=1&value=region=apac,emea&range=day=2026-01-01..2026-01-31&error=0&q=text&sort=attempted&limit=200&offset=0`;
`value` repeats, one per dimension), which filters, orders and counts the keys with the
function a local `--keys` uses, and prints the same text and JSON. The server lists at most
1000 keys a request: `--all`, or a `--limit` past that, reads page after page and prints one
list. A filter the node does not have (a dimension, a range value, an error number) is a `400`
with the usage error a local command prints. `--overview` reads
`GET /assets/{name}/partitions/overview?limit=N`. The suite compares each filter, order and
page, as text and as JSON, both ways ([Partitions: Listing keys](/reference/partitions/#listing-keys-barca-status-node---keys---overview---grid---key)).

`run -o value` on a parquet or pickle result prints the pointer, as locally. `barca.get(...,
server=True)` reads the value from `GET /runs/{run_id}/output` (an unpartitioned target only).
With `partition=` it asks for the values instead (`-o value`): the selected keys' values read on
the server, as JSON (a table as a list of row objects).

A draining server (one that is restarting) accepts a new `--server` run into its run queue
and says that the run waits for the next server, not for the runs ahead of it: `POST /runs`,
`GET /queue` and `GET /runs/{run_id}` carry `waiting_for: "restart"` (`null` otherwise;
experimental), and the client prints `[barca] run RUN_ID is queued behind a restart (position
N): the server at URL is draining and starts no run; it starts when the next server is up` on
stderr, in every mode. The wait lasts the whole drain (until the runs in flight finish, at most
`[server] drain_timeout`) plus the restart. With no next server the run stays queued in the
metadata DB and starts whenever a server next starts on it. The client keeps following it:
when the stream breaks it waits up to 60 seconds
(`BARCA_SERVER_RECONNECT`) for the server to come back and follows the same run id there, and
only then exits 3 ([Operating barca serve: Restarting without losing work](/reference/serve-ops/#restarting-without-losing-work)). On servers
advertising event resume in `/health`, reconnects send the last successfully processed
`Last-Event-ID`; older servers retain replay-count fallback. An attempt change resets live
presentation while following the same run ID, without submitting another run.

Replay is bounded to 65,536 events or 16 MiB per attempt and 64 MiB across the server. If
required events have expired, execution commands warn that their live transcript is incomplete
and poll the recorded outcome every two seconds. A healthy pending/running response can be
followed longer than `BARCA_SERVER_RECONNECT`; that setting limits transport outages, not run
duration. Ctrl-C still cancels the existing server run during polling. `barca logs -f --server`
requires a complete transcript and instead exits 3 on an unfillable replay gap, naming the
recorded-log command. The browser also marks incomplete replay and follows the outcome.
Nothing runs here either way.

`barca cancel RUN_ID --server` requests cancellation of a queued or running run.
Use `--json` for the server acknowledgement.

Ctrl-C (or SIGTERM) asks the server to cancel the run (`DELETE /runs/<run_id>`) and keeps
printing until the run has stopped; it then exits 130. A second Ctrl-C exits 130 at once, and
the server still stops the run.

## Errors

| Situation | Exit | Message names |
|---|---|---|
| the server cannot be reached, answers with something that is not barca, or speaks another API version | 3 | the server URL (and the request); remediation `--local` for a configured server |
| this project's own server (the project socket) stops answering mid-command, or is `--read-only` and the command writes | 3 (unreachable), 2 (read-only) | the socket path; remediation `--local` |
| a command waits 60 seconds for the metadata database's lock (`--local` beside a busy server) | 3 | the lock file and the process holding it (pid and command line, Linux only) |
| the server writes to another store (or another environment) than this project's `[remote]` (a local run) | 2 | both stores |
| a server is configured and the project has no `[remote]` (a local run) | 2 | the missing `[remote]` section |
| the post after a local run is refused or does not arrive | 0 (the run's own code) | the run id and why, on stderr |
| an error the server answers with (an unknown target, a task given to `get`) | as locally | as the local command prints it |
| a stream transport outage exceeds `BARCA_SERVER_RECONNECT` | 3 | the run id |
| `logs -f --server` cannot recover a complete replay | 3 | the run id and recorded-log command |

There is no silent fallback to this machine's ledger: two people who stop agreeing without
noticing is what the server exists to prevent. To run without the server, pass `--local`. A
`--server` run whose stream dropped continues on the server: `barca history --server` shows how
it ended, and `GET <url>/runs/<run_id>` has its full outcome.

## Python

```python
import barca

barca.get("daily_report", server=True)                      # the configured server
barca.get("daily_report", server="https://barca.example.com")
barca.run("deploy", server=True)
```

`barca.api` runs the binary, so it follows the same configuration: without `server=` a
configured server is the ledger of the local run. `barca.Client` is deprecated in favour of it:
constructing one warns. There is no scheduled removal; a removal lands as a breaking change
in a minor release.

## Deploying and the HTTP routes

Run `barca serve --host 0.0.0.0` behind a private network or an authenticating, TLS-terminating
proxy; authentication defaults off ([Deploying barca serve](/deploying/)). Point clients at the
proxy's URL, prefix included; `https://` works for every request, ledger ones included.
Server URL userinfo is omitted from printed addresses in errors, announcements, ledger
warnings, stream-drop messages, SQL provenance and doctor output, including JSON. For example,
`https://user:password@host/barca` prints as `https://host/barca`. Requests retain the configured
credentials; the display change does not change authentication or pending-post matching.

For a bearer-authenticated proxy, set `BARCA_SERVER_TOKEN` in the client environment.
The CLI and deprecated Python `Client` send it as `Authorization: Bearer` on every server
request, including ledger requests, event streams and cancellation. URL userinfo instead
sends Basic credentials. Combining userinfo with `BARCA_SERVER_TOKEN` is a usage error
(exit 2); an empty value counts as unset, and any other token must be ASCII without whitespace or control characters.
These rules apply to URLs from `--server`, `BARCA_SERVER` and `[server] url` alike.
Redirects must stay within the configured origin and proxy path. Known credentials reflected
in HTTP errors are redacted. The reserved `BARCA_SERVER_TOKEN` variable is removed from
Python worker and transfer-helper environments.

### Opt-in project tokens

Set `[server] auth = "token"` in `barca.toml`, `BARCA_SERVER_AUTH=token`, or
`barca serve --auth token` (flag beats environment, environment beats TOML). The default
is `off`; only `off` and `token` are valid. Use TLS at the proxy for remote connections.
In token mode every API request, including health probes and event streams, needs a bearer
credential. The exact static UI files and `GET /auth/status` remain public.

On the deployed host, inside the deployed project, run:

```bash
barca tokens bootstrap
```

This local-only command prints one secret code to stdout. It replaces the previous
unconsumed code and expires after 15 minutes. `POST /auth/bootstrap` with JSON
`{"code":"<bootstrap code>"}` consumes it once and returns a connection token expiring
in 12 hours. Use that token as a bearer credential to create a named API token with
`POST /auth/tokens`, JSON `{"name":"laptop"}`. An optional future UTC `expires_at` sets
an expiry; omitted or null means **no expiry**. Creation returns the secret once.
The UI Connect screen accepts either a bootstrap code or an existing API token. Open
**API tokens** in the sidebar to create named tokens, copy each secret once, list metadata,
and revoke credentials. Tokens have no expiry unless you choose one; the creation form
states this explicitly. A bootstrap connection expires after 12 hours.

The browser keeps its connection in memory and tab-scoped session storage, keyed by server
origin and project prefix. Reloading retains the connection; **Disconnect** clears it and
the UI data cache without revoking the token. Authentication failure returns to Connect.
Token-mode requests, including fetch-based event streams, omit cookies and reject redirects.
No credential goes into a URL or localStorage. A script running in the same origin can read
tab storage: host untrusted projects on separate origins, and use TLS for remote access.

Every token grants the server's full capabilities, including token management. There are
no accounts or roles. Read-only mode still prevents pipeline writes but allows token
administration. `GET /auth/tokens` lists metadata; `DELETE /auth/tokens/{id}` revokes
idempotently. Losing every token is recoverable with a new local bootstrap. A reserved
bootstrap connection slot remains available at the 1000 ordinary token limit.

Auth state lives in owner-only `.barca/auth/`, shared across project environments and
separate from metadata backups. Preserve it separately when migrating; restoring metadata
cannot resurrect a revoked token. Unreadable/corrupt auth state fails closed. Secrets never
belong in URLs, logs, committed configuration or shell arguments. URL Basic credentials
and barca bearer mode cannot share one Authorization header; a layered proxy must forward
bearer credentials and use another mechanism for its own outer authentication.

Missing, invalid, revoked or expired tokens return 401 with `WWW-Authenticate: Bearer` and
the infra/exit-3 envelope. Token responses use `Cache-Control: no-store`. Revocation closes
local event streams; external changes and expiry are checked within one second even when
streams are idle. Auth-off mode rejects token administration.

| Route | Body | Answer |
|---|---|---|
| `POST /ledger/lookup` | `{"hashes": [{"node_id", "run_hash"}]}` | `{"results": [{"node_id", "run_hash", "exists", "path", "format", "size_bytes", "digest"}]}`, one per pair, in order; only `exists: false` for a miss |
| `GET /sql/views?files=a.py` | - | `{"views": [{"name", "node", "kind", "partitioned", "format", "files": [{"path", "partition", "content_hash"}], "stale", "run_hash"}], "missing": [{"name", "node", "command", ...}], "duplicates": [{"name", "ids"}]}`: which result each `barca sql` view reads |
| `GET /materializations?limit=N&offset=M` | - | `{"materializations": [...], "total", "limit", "offset"}`: step results, newest first (`barca.materializations`); `limit` defaults to 1000, at most 10000 |
| `POST /sql/query` | `{"query": "select ...", "limit": 1000}` | `{"columns", "rows", "total", "truncated", "hint"?, "notes"?}`: the document `barca sql --json` prints, for one `SELECT` over barca's tables (`barca.runs`, `barca.materializations`, `barca.nodes`, `barca.schedules`, `barca.ticks`, `barca.queue`); `400` for anything else or rows past 4MB, `408` past 5 seconds, `429` beyond four queries at once, `503` without DuckDB |
| `POST /ledger/runs` | `{"run": {...}, "steps": [...]}`: the run row and its step rows | `201 {"run_id", "bytes_changed"}`, or `409` with the error envelope naming the node and nothing recorded; `400` on a server with no store, `403` on a `--read-only` one, `503` while it shuts down |

A repeated post of a run the server already recorded (same `run.run_id`) answers `201` with the
run it recorded.

`POST /sql/query` is the one route on which the server runs SQL, and never over results or
files. It is a read: a `--read-only` server serves it, and with `--auth token` it needs a token
like every other route.

- The query is one `SELECT` (DuckDB dialect) over `barca.runs`, `barca.materializations`,
  `barca.nodes`, `barca.schedules`, `barca.ticks` and `barca.queue` ([barca sql: barca's own tables](/reference/cli/sql/#barcas-own-tables)). `barca.queue` is the queue of `GET /queue` at the time of the query. `limit` defaults to 1000 and is at
  most 10000; `total` counts every row, `truncated` says rows were cut and `hint` what to send.
  `notes` appears when a table holds more than the 100000 rows the server loads of it.
  An answer holds at most 4MB of rows: a query whose first `limit` rows are more is refused
  (`400`) before they are read to the end, so neither the helper nor the server holds them.
- `400` with the usage envelope for anything else: a result view or another relation, a file or
  URL (`read_parquet`, `read_csv`, `glob`, a quoted path), a statement that is not a `SELECT`
  (`COPY`, `ATTACH`, `INSTALL`, `LOAD`, `PRAGMA`, `SET`, any write), a second statement, a write
  inside a `SELECT`, a SQL error, rows past 4MB, a query that needs more memory than the 1GB
  DuckDB is given (the helper stays). `408` when the query has not finished within 5 seconds,
  waiting for the helper and loading tables included: it is cancelled, not left running.
  `429` with `Retry-After: 1` when four queries are admitted already (one running, three
  waiting): a client sending slow queries holds four requests, never all of them.
  `503` when the server's Python environment has no DuckDB (`uv add 'barca[sql]'` there) or the
  helper stopped under the query; send it again.
- The boundary: the query runs in a helper process of the server (`python -m
  barca._sql_server`, one per server, started by the first query, started again if it dies,
  gone with the server). Its in-memory DuckDB holds barca's tables and nothing else, with
  external access off (no file, no network, no extension, and no statement turns it back on),
  a memory limit of 1GB, nothing spilled to disk, and two threads; each query runs in a
  read-only transaction, as it was sent (never pasted inside another statement). The helper
  runs in an empty directory of its own that is also its `HOME` (not the project, which it
  cannot import from, and not the server user's home), with DuckDB's secret and extension
  directories unset, and is given the server's `PATH`, locale, `PYTHONPATH` and `PYTHONHOME`
  but no other environment variable (no store credentials, no tokens). The server hands it
  rows: it never opens `metadata.db`, an artifact or the store. The server's own paths (project
  root, home, Python environment) are removed from DuckDB's error messages before they are
  answered.
- What it does expose: the rows of barca's tables, to whoever may call the route. Those hold
  the error text of failed steps and runs (whatever the pipeline's code put in an exception)
  and artifact paths, as `GET /history`, `GET /runs/{run_id}` and `GET /materializations`
  do; DuckDB's version and platform can be read too. Run the server with `--auth token` where
  that is not for everyone who can reach it.
- A table is handed over when a query names it and the metadata database, or for `barca.nodes`
  the graph, changed since it was (and `barca.nodes` at least every 30 seconds). A query on an
  unchanged server reads no database and computes no status. A query that calls `query(...)`
  or `query_table(...)` can reach a table without naming it, so every table is handed over for
  it. The helper has 60 seconds to take a table; one that has not is replaced.

The inspection routes (`GET /state`, `/nodes`, `/history`, `/stats/{name}` and the others the
CLI and the web UI read) keep what they computed until the sources or the metadata DB change,
and answer a request whose `If-None-Match` names their last `ETag` with `304` and no body. So
`barca status --server` against an unchanged project answers in milliseconds, whatever the
number of partition keys; a finished run or an edit is in the next answer. When deciding the
cache states takes longer than a second (a graph with a million partition keys), a request
after a change waits that second and is answered with the last result meanwhile; what is
running is always current. The routes read the metadata DB in place, beside the runs that
write it, including under `--read-only`. A current WAL database is opened through read-only
connections: its rows are never written, migrated or checkpointed, although SQLite can create
`-wal` and `-shm` beside it. A database needing migration (or not in WAL mode) is copied
on the blocking pool and migrated privately. A missing database stays missing.
Read-only servers warm status once at startup even while another process writes the DB,
then again after settled changes while clients poll.
`GET /state?targets=first,second` (experimental) computes only those targets and their upstream
dependencies, including durations; use full node ids when names are ambiguous. A target may be
a group id (`group:FILE:NAME`), meaning every node in the group, and `files=a.py` adds every
node of a source file: the UI scopes a group or pipeline that way, one short URL at any size.
An absent or empty `targets` computes every node. Unknown targets, groups or files answer
`404`; ambiguous names answer `409`.
The UI renders the graph, groups and sidebar counts from `/assets` and `/groups` first,
shows cache states as checking while `/state` runs, then fills them in without moving the
graph. A selected pipeline or group requests only its upstream cone; All pipelines requests
the whole graph, and so does the sidebar, once a minute, for its per-pipeline health dots (a
dot appears only when all of its file's state is known). A status error leaves source metadata visible with the error above it.
The sidebar is the UI's one navigation: Search (Ctrl/Cmd-K), then Assets, Runs, Schedules,
Tasks and Sensors with how many each lists, then the pipelines, then the docs, the connection,
the time zone and the theme; collapsed, it keeps their icons. A pipeline's count and dot are of
the open page's kind (assets, except on Tasks and Sensors), with the same state its header in
the table shows.
On a project with groups, select a member node in either List or Graph to run it from its
details panel (or the command palette, Ctrl/Cmd-K). Assets and sensors use cache-aware `get`;
tasks use `run`. Groups organize nodes and are not run targets. Outside a group, the Run at the
end of each page's toolbar runs what is open: `Run billing.py` gets that file's assets and
sensors, and `Run all` gets every one after a confirmation; its tooltip gives the count
(`Run all 8,070 assets, 1 sensor`, which leaves out tasks, as they run only by name).
Read-only servers disable the actions.
The selected node shows as running as soon as the server accepts it; View run opens its
durable status, steps and logs. Ungrouped list panels offer the same action.
The full schemas are in `specs/interfaces/server-api.openapi.yaml`; every route is
described at [Server API Endpoints](/reference/server-api-endpoints/).

### Data health

Each row of `GET /state` and each node of `GET /nodes` carries what `barca status --json` does:
`health` and `health_reason` (whether something needs doing about the node, and the rule that
decided it), `cause` (why a node that is not fresh is so: `catching_up`, `queued`,
`no_refresh_path`, `overdue`, `failing` or `undecided`), `root` (the upstream fact that
explains it: a failing node, a sensor's new value, changed code, a missing or paused
schedule, a run queue that is behind) and `gap_seconds` (its freshness gap), `signals` (what
its recorded results say about its data: `empty`, `rows_changed`, `behind_input`), `measured`
(whether its rows were counted at all; `null` for a task or sensor), `last_check` (what the
newest tick that checked the node found, and when; `null` when no retained tick did) and
`refresh_path` (the schedule whose tick will bring it up to date, its next tick, and the ticks
that already missed it; `null` when none will), and the times: `stale_since` (since when it is
out of date), `expected_fresh_at` (when a run is expected to leave it fresh, an estimate) and
`recheck_at` (when a tick next looks at it, where no fresh time can be promised). They are
computed in barca from facts the metadata DB holds, never by reading an
artifact, and are the same on every surface: [barca status: Fields and JSON: Health](/reference/cli/status-fields/#health), "Cause",
"Root" and "Data signals and coverage", has the tables. `GET /nodes` also has
`data_coverage`, `as_of` and `rollup` (the scope summed) beside `summary`; its `as_of` is the
start of the current minute on the server's clock (the instant `gap_seconds` is read at), so
an unchanged poll within a minute is a `304`.

The server classifies per answer, on its clock and in its `--timezone`, so a schedule that
misses its slot turns its node `behind` (`health_reason: "schedule_late"`) without a change to
the database, and a paused schedule is never late. A node whose state the server cannot
decide (`unknown`) and that the last tick found up to date is `ok` with `health_reason:
"checked_up_to_date"`, on a `--read-only` server too: it reads the ticks the scheduling server
recorded, and the web UI's node panel reads "checked 3m ago, up to date". The check expires
when the next tick is overdue ([barca status: Fields and JSON: Last check](/reference/cli/status-fields/#last-check)); the tick record is
read once per change of the metadata database, not per poll. A `--read-only` server runs no
scheduler, so its `refresh_path.live` is `false`; for the cause it reads the same tick record:
a schedule that fired its newest slot proves a scheduler is running, and the node reads
`catching_up` or `queued` as on the scheduling server instead of `no_refresh_path` with the
root `scheduler_off` ([barca status: Fields and JSON: Cause](/reference/cli/status-fields/#cause)). It gives no `expected_fresh_at`.
An empty `signals` on a row with `measured:
false` says nothing was counted, not that the data is fine; the web UI states the coverage of
the open scope ("7 of 556 measured") beside its causes.

`GET /state/rollup` (experimental) is those rows summed: one tree of everything in scope, its
groups, the source files of the nodes in no group, and the nodes, each level the exact sum of
its children, in nodes (`nodes`, `health`, `causes`) and in assets (`assets`, `asset_health`,
`asset_causes`), with `gap_seconds` and `unbounded`, the scope's `data_coverage`, and `roots`:
what holds the nodes that are not fresh, ranked, each with the nodes it holds by cause, its
gap and the `action` that clears it (`wait`, `run`, `add_schedule`, `resume_schedule`,
`start_server`, `fix_failure`, `raise_max_runs`; a `command` that names `--server`, and the
typical compute time of what it holds). Every node that is not fresh is under one root. It
takes the scope of `GET /state` (`?targets=`, `?files=`), is the document `barca status
--rollup --json` prints ([barca status: Fields and JSON: The rollup](/reference/cli/status-fields/#the-rollup)), and is what the web UI's
health map, health bars, group rows, to-do list and failures list render, so the map, the bars
and the CLI state the same numbers. An entry's `not_fresh` and `held_by` say how many of its
nodes are not fresh and which roots hold them there, so the page's list for an open group is
that group's entry, not a filter in the browser. It sums the status result `GET /state` is built from: no
status is computed for it. Its `as_of` is the start of the current minute on the server's
clock, so an unchanged poll within a minute is a `304`; `gap_seconds` is read at the start
of the minute on every route for the same reason. Each row's `root` names what holds it; the
web UI reads it instead of walking the graph.

```bash
curl -s localhost:8274/state/rollup | jq '.entries[0] | {nodes, assets, health}'
curl -s 'localhost:8274/state/rollup?targets=total' | jq -r '.roots[] | [.nodes, .node, .detail] | @tsv'
curl -s localhost:8274/state/rollup | jq -r '.roots[] | [.nodes, .kind, .node, .action.kind, .action.command // "-"] | @tsv'
curl -s localhost:8274/state | jq -r '.[] | select(.cause) | [.cause, .root.id, .gap_seconds, .id] | @tsv'
curl -s localhost:8274/state | jq '.[] | select(.signals != []) | {id, signals}'
curl -s localhost:8274/state | jq 'map(select(.kind == "asset")) | {assets: length, measured: map(select(.measured)) | length}'
curl -s localhost:8274/nodes | jq '.data_coverage'
```

A scheduled node's row also has `prev_run`, the cron's most recent fire time at or before now,
beside `next_run` (unix seconds, in the server's `--timezone`), `late_since`: the first slot
after its schedule's newest recorded decision, when that slot has passed, and a
`schedule` block, the one `barca status --json` prints, whose `late` says whether the schedule
is late ([Scheduling With Cron](/scheduling/), "A schedule that missed its slot"). `late_since` is
`null` while on time, while the schedule is paused or held and when no tick is recorded. The web
UI shows a schedule as `late by 12m` when `schedule.late` is true and compares no times itself.
A `--read-only` server reads the same runs and run queue from the shared metadata, so its
answer is the scheduling server's, with slots in its own `--timezone`.

See also: [Scheduling With Cron](/scheduling/), [Remote Storage](/reference/remote-storage/), [CLI Contract](/reference/cli-contract/).

---

*The same text is in your terminal: `barca docs server-mode`.*
