# Server mode: one ledger for a team

Sharing results between machines means one `barca serve` that everyone points at. **The server
is the ledger**: it holds the one record of which results exist (run hashes, artifact paths, run
history) and it is the only process that writes it. The artifact bytes stay in the store
(`[remote]`, `barca docs remote`); the server never moves them.

This topic covers local runs against a server, which is what ships today. Running a command *on*
the server (`--server`), `BARCA_SERVER`, `--local`, and `barca status` / `barca history`
answered by the server are still being built (RFC-0007, #394).

## Configure

Every machine (the server and each laptop) uses the same store; the laptops also name the
server:

```toml
# barca.toml on a laptop
[server]
url = "http://barca.internal:8274"

[remote]
uri = "s3://team-bucket/barca"     # the store the server uses
```

The server is an ordinary `barca serve` of the project with the same `[remote]` and no
`[server]` section (`barca docs scheduling`, "Calling the server"). Only `http://` URLs are
supported for now; put a TLS-terminating, authenticating proxy in front of the server and point
the URL at the proxy's plain-HTTP side, or reach it over a private network.

With `[server] url` set, `[remote] state` defaults to `off` (no shared database blob is pulled
or pushed: the server is the ledger), and `state = "optimistic"` is a usage error.

## What a run does

Nothing about the commands changes. `barca get` and `barca run` still run on this machine, with
this machine's code:

```bash
barca get daily_report                 # cache hits come from the server's ledger and the store
barca get daily_report --dry-run       # what would run, decided against the server's ledger
barca run deploy                       # a task, run here, recorded on the server
```

In order:

1. The command prints `[barca] server <url>` and asks `GET /health`. The server must speak this
   barca's API version and write to the same store as this project's `[remote]`.
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
   `client_run_id` (`barca history --json` on the server shows both).

The lookup is by run hash only. Two people with the same code share results; a person who edited
`report.py` gets misses for the nodes the edit changed and hits for the rest, and the rows they
post are keyed by their own hashes.

## When the post does not go through

The run is a success here whatever happens to its post: the exit code and the output do not
change. If the server refuses the post (`409`: an object is missing, outside its store, or does
not have the posted digest) or cannot be reached, the command prints

```
[barca] warning: run <id> was not recorded on the server <url>: <why>; it stays on this machine (unsynced) and the next command against the server sends it again
```

and keeps the post in this machine's database (the `ledger_posts` table, state `unsynced`). The
next `get` or `run` against the server posts it before its own lookup and prints
`[barca] recorded run <id> on the server (run <server id>)`, or
`[barca] warning: run <id> is still not recorded ...` if it is refused again. A result whose
object went missing is a miss on the ledger, so the next `get` that needs it computes and posts
it again, after which the refused post is accepted too.

Only successful runs are posted. A run that failed keeps the steps that succeeded on this
machine; they are computed again by the next machine that needs them.

## Two machines, same run hash, different bytes

When the ledger already holds a run hash with another digest (a function that is not
deterministic, computed twice), it keeps its row and tells the poster, which prints

```
[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic
```

once, and marks the step `bytes_changed: true` in its `--json` output. Artifacts are addressed
by run hash, so the second machine's upload has replaced the object in the store; a later fetch
of that result notes the mismatch and uses the store's copy.

## Errors

| Situation | Exit | Message names |
|---|---|---|
| the server cannot be reached, answers unexpectedly, or speaks another API version | 3 | the server URL and the request |
| the server writes to another store (or another environment) than this project's `[remote]` | 2 | both stores |
| `[server] url` is set and the project has no `[remote]` | 2 | the missing `[remote]` section |
| the post after the run is refused or does not arrive | 0 (the run's own code) | the run id and why, on stderr |

There is no silent fallback to this machine's ledger: two people who stop agreeing without
noticing is what the server exists to prevent. To run without the server, remove `[server] url`.

## The HTTP routes

| Route | Body | Answer |
|---|---|---|
| `POST /ledger/lookup` | `{"hashes": [{"node_id", "run_hash"}]}` | `{"results": [{"node_id", "run_hash", "exists", "path", "format", "size_bytes", "digest"}]}`, one per pair, in order; only `exists: false` for a miss |
| `POST /ledger/runs` | `{"run": {...}, "steps": [...]}`: the run row and its step rows | `201 {"run_id", "bytes_changed"}`, or `409` with the error envelope naming the node and nothing recorded; `400` on a server with no store, `403` on a `--read-only` one, `503` while it shuts down |

A repeated post of a run the server already recorded (same `run.run_id`) answers `201` with the
run it recorded. The full schemas are in `specs/server-api.openapi.yaml`.
