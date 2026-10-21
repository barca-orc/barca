# Server mode: one ledger for a team, and running commands on it

Sharing results between machines means one `barca serve` that everyone points at. **The server
is the ledger**: it holds the one record of which results exist (run hashes, artifact paths, run
history) and it is the only process that writes it. The artifact bytes stay in the store
(`[remote]`, `barca docs remote`); the server never moves them. Server mode is experimental
(`barca docs contract`).

With a server configured, a command does one of three things:

| You type | `get` / `run` | `status` / `history` / `stats` |
|---|---|---|
| nothing extra | run on this machine, against the server's ledger | answered by the server |
| `--server [<url>]` | run on the server, printed here | answered by the server |
| `--local` | run on this machine against its own ledger; nothing is read from or sent to the server | answered from this machine's records |

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
- `--server` and `--local` apply to `get`, `run`, `status`, `list`, `history` and `stats`.
  `serve`, `plan`, `sql`, `doctor`, `logs`, `docs` and `version` always run on this machine;
  `--server` or `--local` on them is a usage error. `barca list` without `--server` reads this
  machine's files, and `barca sql` reads this machine's database and artifacts.

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
machine; they are computed again by the next machine that needs them. So does a run some of
whose results could not be uploaded to the store (their upload is pending, `barca docs remote`):
it prints `[barca] warning: run <id> is not recorded on the server <url>: some of its results
are not in the artifact store yet ...` and is not posted, now or later.

A server that is draining before it stops (SIGTERM, `barca docs scheduling`) refuses posts with
`503`; the run stays `unsynced` here and the next command against the server sends it again.

### Two machines, same run hash, different bytes

When the ledger already holds a run hash with another digest (a function that is not
deterministic, computed twice), it keeps its row and tells the poster, which prints

```
[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic
```

once, and marks the step `bytes_changed: true` in its `--json` output. Artifacts are addressed
by run hash, so the second machine's upload has replaced the object in the store; a later fetch
of that result notes the mismatch and uses the store's copy.

## status, history and stats with a server

With a server configured, `barca status`, `barca history` and `barca stats` answer from the
server, as `--server` does, after printing `[barca] server <url> (<source>)`. The server answers
for its deployed code.

`barca status` (without `--server`) also says what this machine holds. Each node in `--json`
gets `local`: `true` when this machine has a copy of the result the server would serve (its
mirror of the store object exists under this machine's artifact directory), else `false`. That
is what `barca sql` can query here. A node the server does not have a result for, whose result
this machine computed and could not post yet, is `unsynced` with `cache.reason:
"not_in_ledger"` and `local: true`; the table prints `unsynced  not in ledger`, and
`summary.unsynced` counts it. A result is matched by node and run hash, so one computed here
with code the server does not run keeps the server's state. A partitioned node has no single
artifact, so its `local` is `false`.

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
`--partition` selects keys as locally (`RunRequest.partition`). `get -o url` is a usage error
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

The parity suite (`python/tests/test_server_mode_parity.py`) runs the contract's case table
once locally and once through `barca serve` on a copy of the same project and compares the
normalized output with exactly these differences.

`run -o value` on a parquet or pickle result prints the pointer, as locally. `barca.get(...,
server=True)` reads the value from `GET /runs/{run_id}/output` (an unpartitioned target only;
`partition=` with `server=` is an error).

A draining server answers a new `--server` run with `503`: the client exits 3 naming the
server, and runs nothing here.

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
proxy; there is no built-in authentication (https://barca.sh/deploying/). Point clients at the
proxy's URL, prefix included; `https://` works for every request, ledger ones included.

| Route | Body | Answer |
|---|---|---|
| `POST /ledger/lookup` | `{"hashes": [{"node_id", "run_hash"}]}` | `{"results": [{"node_id", "run_hash", "exists", "path", "format", "size_bytes", "digest"}]}`, one per pair, in order; only `exists: false` for a miss |
| `POST /ledger/runs` | `{"run": {...}, "steps": [...]}`: the run row and its step rows | `201 {"run_id", "bytes_changed"}`, or `409` with the error envelope naming the node and nothing recorded; `400` on a server with no store, `403` on a `--read-only` one, `503` while it shuts down |

A repeated post of a run the server already recorded (same `run.run_id`) answers `201` with the
run it recorded. The full schemas are in `specs/server-api.openapi.yaml`; every route is
described at https://barca.sh/reference/server-api/.

More: barca docs scheduling, barca docs remote, barca docs contract.
