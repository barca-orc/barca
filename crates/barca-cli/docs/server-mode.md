# Server mode: one ledger for a team, and running commands on it

To execute a deployed project remotely, start `barca serve` there and use an explicit URL:

```bash
barca get daily_report --server http://barca.internal:8274
barca run deploy --server http://barca.internal:8274
barca history --server http://barca.internal:8274
```

This runs the server's code and keeps history there; it does not upload local edits.
No shared artifact store is needed for this path. Keep the server on a private network
or behind an authenticating proxy (https://barca.sh/deploying/).

## Local computation with shared results

Sharing results between machines means one `barca serve` that everyone points at. **The server
is the ledger**: it holds the one record of which results exist (run hashes, artifact paths, run
history) and it is the only process that writes it. The artifact bytes stay in the store
(`[remote]`, `barca docs remote`); the server never moves them. Server mode is experimental
(`barca docs contract`). To set it up in three steps, see
https://barca.sh/team-server/.

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
`[server]` section (`barca docs scheduling`, "Calling the server").

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
  `sql` and `doctor`. `serve`, `plan`, `docs` and `version` always run on this machine;
  `--server` or `--local` on them is a usage error. `barca list` without `--server` reads this
  machine's files. `barca sql` always runs its query on this machine: a server only says which
  results its views read (`barca docs sql`, "With a server"); the server never runs SQL.
  `doctor` runs its checks here and, with a server (configured, or `--server`), checks the
  server too; `--local` leaves it out (`barca docs doctor`, "Checking a server").

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
results could not be uploaded to the store (their upload is pending, `barca docs remote`) is
held: it prints `[barca] warning: run <id> is not recorded on the server <url> yet: some of its
results are not in the artifact store yet ...`, and its post is kept `unsynced` (`barca status`
shows its results `not_in_ledger`) without being sent, since the server would refuse it. The
next command that opens the store uploads the pending results first and then posts the run
(`[barca] recorded run <id> on the server ...`), before it decides its own steps, which are
then hits; other machines get hits from then on. A pending result whose local file is gone is
dropped (`barca docs remote`) and taken out of the post, and the rest of the run is posted.

A server that is draining before it stops (SIGTERM, `barca docs scheduling`) refuses posts with
`503`; the run stays `unsynced` here and the next command against the server sends it again.

### Backups

The server backs up its database to the store (`<uri>/<env>/state/backups`, `barca docs
remote`); the machines that use it take no backups, with or without `--local`, and print no
`backed up the metadata DB` line. A client's history that matters is on the server, and a
client's copies in the server's backup set would rotate the server's own out of it and be
candidates for a restore of the server. Backups barca 0.29.1 and older took on a client stay
in the store (pruning keeps every machine's newest; `barca state backups` lists them with their
`host` and `project`), and you can delete them there. `barca state restore` without `--from`
on the server takes the server's own newest and names any newer ones a client took; on a server
with no backup of its own (a new host) it takes the newest and names who took it, so check
`barca state backups` and pass `--from` with the server's.

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
with `--local` (`barca docs logs`, "Debugging a live pipeline"). Nothing falls back to this
machine, but `barca logs RUN_ID` for a run only this machine has fails (exit 2) with the
remediation `this run is on this machine: barca logs RUN_ID --local`, and `barca logs` without a
run id adds `[barca] note: this machine has a newer run <id> (...); barca logs --local reads it`
on stderr when this machine's newest run is newer than the server's.

`barca history`'s filters (`--status`, `--target`, `--triggered-by`, `--since`) are applied by
the server (`GET /history?status=...&target=...&triggered_by=...&since=...`; a relative
`--since` counts back from the server's clock), and `barca history RUN_ID` reads
`GET /runs/{run_id}`: the run, its steps with their attempts, run hashes, artifacts, errors and
tracebacks (at most 1000 steps unless `--limit N` or `--all` asks for more; `steps_page` says how
many there are). A run only this machine has (a failed run against the ledger is never posted) fails
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
tables (`barca.runs`, `barca.materializations`, `barca.nodes`) from its records, then runs the
query here over the store; the server never runs SQL (`barca docs sql`, "With a server"). An
unsynced result is not mixed into a view: the error or note says so and names `--local`.

`barca status --problems` (or `--state <states>`) keeps the nodes that need attention, the
server's failures, stale results and failing or paused schedules among them, plus this
machine's `not_in_ledger` results; each scheduled node carries the server scheduler's view in
`schedule` (`barca docs status`, "Schedules"). `barca doctor` checks the server itself.

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

`run -o value` on a parquet or pickle result prints the pointer, as locally. `barca.get(...,
server=True)` reads the value from `GET /runs/{run_id}/output` (an unpartitioned target only).
With `partition=` it asks for the values instead (`-o value`): the selected keys' values read on
the server, as JSON (a table as a list of row objects).

A draining server (one that is restarting) accepts a new `--server` run into its run queue;
the client keeps following it: when the stream breaks it waits up to 60 seconds
(`BARCA_SERVER_RECONNECT`) for the server to come back and follows the same run id there, and
only then exits 3 (`barca docs scheduling`, "Restarting without losing work"). `barca logs -f
--server` does the same. Nothing runs here either way.

Ctrl-C (or SIGTERM) asks the server to cancel the run (`DELETE /runs/<run_id>`) and keeps
printing until the run has stopped; it then exits 130. A second Ctrl-C exits 130 at once, and
the server still stops the run.

## Errors

| Situation | Exit | Message names |
|---|---|---|
| the server cannot be reached, answers with something that is not barca, or speaks another API version | 3 | the server URL (and the request); remediation `--local` for a configured server |
| the server writes to another store (or another environment) than this project's `[remote]` (a local run) | 2 | both stores |
| a server is configured and the project has no `[remote]` (a local run) | 2 | the missing `[remote]` section |
| the post after a local run is refused or does not arrive | 0 (the run's own code) | the run id and why, on stderr |
| an error the server answers with (an unknown target, a task given to `get`) | as locally | as the local command prints it |
| the connection drops while a `--server` run streams | 3 | the run id |

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
constructing one warns, and it will be removed in a later minor release.

## Deploying and the HTTP routes

Run `barca serve --host 0.0.0.0` behind a private network or an authenticating, TLS-terminating
proxy; authentication defaults off (https://barca.sh/deploying/). Point clients at the
proxy's URL, prefix included; `https://` works for every request, ledger ones included.
Server URL userinfo is omitted from printed addresses in errors, announcements, ledger
warnings, stream-drop messages, SQL provenance and doctor output, including JSON. For example,
`https://user:password@host/barca` prints as `https://host/barca`. Requests retain the configured
credentials; the display change does not change authentication or pending-post matching.

For a bearer-authenticated proxy, set `BARCA_SERVER_TOKEN` in the client environment.
The CLI and deprecated Python `Client` send it as `Authorization: Bearer` on every server
request, including ledger requests, event streams and cancellation. URL userinfo instead
sends Basic credentials. Combining userinfo with `BARCA_SERVER_TOKEN` is a usage error
(exit 2); a present token must be nonempty ASCII without whitespace or control characters.
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
and Barca bearer mode cannot share one Authorization header; a layered proxy must forward
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
| `POST /ledger/runs` | `{"run": {...}, "steps": [...]}`: the run row and its step rows | `201 {"run_id", "bytes_changed"}`, or `409` with the error envelope naming the node and nothing recorded; `400` on a server with no store, `403` on a `--read-only` one, `503` while it shuts down |

A repeated post of a run the server already recorded (same `run.run_id`) answers `201` with the
run it recorded.

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
the whole graph. A status error leaves source metadata visible with the error above it.
On a project with groups, select a member node in either List or Graph to run it from the
topbar Run button or its details panel. Assets and sensors use cache-aware `get`; tasks use
`run`. Groups organize nodes and are not run targets. Read-only servers disable the actions.
The selected node shows as running as soon as the server accepts it; View run opens its
durable status, steps and logs. Ungrouped list panels offer the same action.
The full schemas are in `specs/interfaces/server-api.openapi.yaml`; every route is
described at https://barca.sh/reference/server-api/.

See also: `barca docs scheduling`, `barca docs remote`, `barca docs contract`.
