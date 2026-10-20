# Server mode: run a command on a barca server

`barca serve` on a shared machine holds the deployed code, its workers and the record of what
it computed. `--server` runs a command there and prints the answer here exactly as the command
prints it locally: the same stdout JSON, the same stderr lines, the same exit codes, and Ctrl-C
cancels. It is experimental (`barca docs contract`).

```bash
barca get daily_report --server https://barca.internal.example.com   # run it on that server
barca get daily_report --server               # the configured server (BARCA_SERVER, [server] url)
barca run deploy --server --agent             # a task, with the agent progress lines
barca status --server                         # cache state on the server
barca list --server --json                    # the deployed nodes
barca history --server -l 20                  # the server's runs
barca stats daily_report --server             # timings from the server
barca get daily_report --local                # this machine only, whatever is configured
```

## Which server

Configure it once in `barca.toml`:

```toml
[server]
url = "https://barca.internal.example.com"
```

or per shell with `export BARCA_SERVER=https://barca.internal.example.com`. `BARCA_SERVER`
beats `[server] url`; an empty `BARCA_SERVER=` counts as unset. `--server <url>` names a server
for one command. The URL is `http://` or `https://`, with any path prefix a proxy adds
(`https://host.example.com/barca`).

- `--server` with no URL uses the configured server and prints `[barca] server <url> (<where it
  came from>)` once on stderr. With nothing configured it is a usage error (exit 2).
- `--server` followed by a target or a flag is bare `--server`: `barca get --server total` gets
  `total` from the configured server. A word with `://` after it is its URL, which must start
  with `http://` or `https://`.
- Without `--server` a command runs on this machine, as it always has. `--local` says so
  explicitly and ignores the configured server; `--local` with `--server` is a usage error.
- `--server` and `--local` apply to `get`, `run`, `status`, `list`, `history` and `stats`.
  `serve`, `plan`, `sql`, `doctor`, `logs`, `docs` and `version` always run on this machine;
  `--server` on them is a usage error. `barca sql` reads this machine's artifacts.

A configured server does not yet change a command without `--server`: a local run that reads the
server's record of what exists and records its results there is the next step of server mode
(RFC-0007, #402).

## What runs where

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

## What prints

For every command the client prints what a local run against the same project and history
prints: the stdout JSON key for key (built by the same functions), the stderr lines in the same
order, the exit codes 0, 1, 2, 3 and 130. `--json`, `--pretty`, `-o value`, `--fields`,
`--limit` and `--agent` apply here as they do locally. A step's own output (`[node] line`) is
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

The parity suite (`python/tests/test_server_mode_parity.py`) runs the contract's case table
once locally and once through `barca serve` on a copy of the same project and compares the
normalized output with exactly these differences.

`-o value` on a parquet or pickle result prints the pointer, as locally. `barca.get(...,
server=True)` reads the value from `GET /runs/{run_id}/output`.

## When the server cannot answer

- A server that cannot be reached, answers with something that is not barca, or speaks
  another API version (`api_version` in `GET /health`) is `kind: infra`, exit 3. The message
  names the URL; nothing runs on this machine instead. To run here, leave out `--server`.
- An error the server answers with (an unknown target, a task given to `get`) is printed as the
  local command prints it, with the same exit code.
- A connection that drops while a run streams is `kind: infra`, exit 3, naming the run id. The
  run continues on the server: `barca history --server` shows how it ended, and
  `GET <url>/runs/<run_id>` has its full outcome.
- Ctrl-C (or SIGTERM) asks the server to cancel the run (`DELETE /runs/<run_id>`) and keeps
  printing until the run has stopped; it then exits 130. A second Ctrl-C exits 130 at once,
  and the server still stops the run.

## Python

```python
import barca

barca.get("daily_report", server=True)                      # the configured server
barca.get("daily_report", server="https://barca.example.com")
barca.run("deploy", server=True)
```

`barca.api` runs the binary, so it follows the same configuration. `barca.Client` is deprecated
in favour of it: constructing one warns, and it will be removed in a later minor release.

## Deploying

Run `barca serve --host 0.0.0.0` behind a private network or an authenticating, TLS-terminating
proxy; there is no built-in authentication (https://barca.sh/deploying/). Point clients at the
proxy's URL, prefix included. The HTTP routes are described at
https://barca.sh/reference/server-api/.

More: barca docs scheduling, barca docs contract.
