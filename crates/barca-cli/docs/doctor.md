# Doctor: check a project before running it

```bash
barca doctor
barca doctor pipeline.py --json
barca doctor src/ --env local --pretty
```

`doctor` reports the project root and how it was found, selected environment, parsed
configuration, actual Python executable and package version, local cache writability,
metadata integrity, the capacity steps share (`capacity`: how much `weight=` may run at once,
the pool size unless `[execution] capacity` or `BARCA_CAPACITY` sets it, and the pool size with
where it came from, `pool_source`; with `[server] capacity`, a `server_capacity` check with what a
run under `barca serve` gets; `barca docs assets`, "Heavy steps"), discovered source files and parse errors. It checks configured artifact
and shared-state stores with read/list operations. It runs no steps, imports no pipeline
modules and does not evaluate dynamic partition expressions. Source checks are syntax and
decorator extraction checks, not a complete execution plan.

With `--pretty`, each check occupies one line. With `--json`, stdout is
`{"ok": true, "checks": [{"name": "...", "ok": true, "detail": "...", "data": {}}]}`.
The default follows the usual terminal/pipe output choice. Exit 0 means every check passed;
exit 3 means at least one prerequisite failed, with the normal error envelope on stderr.
Argument errors still exit 2. Check names and provider-specific `data` are experimental.

`get` and `run` also print the selected environment and artifact store once at startup, on
stderr, when the environment comes from `default_env` in `barca.toml` or a store is configured:
`[barca] env prod (default_env in barca.toml); artifact store <store>` (`barca docs cache`,
"Environments"). `serve` always prints it, with the metadata DB and its format added
(`barca docs scheduling`, "The metadata DB at startup").

The Python probe has a 30-second overall deadline. It lists stores and looks up the
identity behind the selected credentials where supported: AWS STS caller, Azure token
principal/tenant, Google credential email or token identity, and an access-key fingerprint
for R2 and other S3-compatible endpoints. An unavailable identity lookup is reported as
unresolved; a successful store listing still passes. Azure shared-key/SAS credentials use
fingerprints. Fingerprints identify a credential, not a person. Credentials and signed URI
queries are redacted. Authentication errors show configured account/tenant context and a
recovery command before the provider error; a failed login cannot establish the effective
principal.

No remote objects are created or changed. Checking the local cache can create its directory
and a temporary write probe, removed immediately. Existing metadata is validated from a
snapshot without migrating or changing the original; an absent database is reported and
left absent. Store reachability does not establish upload permission or validate user input
paths inside steps.

## Checking a server

With a server configured (`BARCA_SERVER`, or `[server] url` in `barca.toml`; `barca docs
server-mode`), `doctor` checks it too, after `[barca] server <url> (<source>)` on stderr, as
`status` answers from it. `--server <url>` checks that server instead (bare `--server` the
configured one, a usage error when none is), and `--local` checks this machine only. The
server's checks follow this machine's, in the same report:

```bash
barca doctor                                    # this machine, and the configured server
barca doctor --server http://127.0.0.1:8274 --json   # a server named here
barca doctor --local                            # this machine only
```

| check | fails when | data |
|---|---|---|
| `server` | the server cannot be reached or does not answer `GET /health` as barca; the other server checks are then skipped | `url`, `source`, `reachable` |
| `server_version` | it speaks another API version than this client (another barca version with the same API passes, and says so) | `version`, `api_version`, `client_version`, `client_api_version` |
| `server_store` | its artifact store is not this project's `[remote]` (or either has none): a result one records could not be read by the other | `server`, `project`, `server_env`, `project_env` |
| `server_health` | it is draining (shutting down), or cannot use its metadata DB (`metadata_error`) | `status`, `read_only`, `metadata_error` |
| `server_sources` | it failed to load a deployed file (`GET /health` `load_errors`): that file's nodes are missing from everything it answers | `load_errors` |
| `server_scheduler` | its scheduler stopped on an error, or a schedule is failing (`max_failures` failed ticks in a row, so its ticks are skipped). A paused schedule is reported and passes; a server started with `--no-schedule` or `--read-only` passes | `running`, `schedules`, `paused`, `failing` (node ids) |
| `server_posts` | runs this machine computed against the server's ledger are not recorded there yet (`barca status` shows their results `unsynced`, `not_in_ledger`) | `unsynced`, `runs`, `last_error` |

The failed check's `detail` ends with a line that says what to do (in the table, after `; `):

```
[error] server_scheduler: running; 1 schedule, 1 failing (pipeline.py:nightly_sync): pipeline.py:nightly_sync failed 2 ticks in a row (max_failures=2) and its ticks are skipped; Run `barca status --problems` for the error and fix it, then `barca run pipeline.py:nightly_sync --server`: a success resets the count and the ticks resume.
[error] server_posts: 1 run computed here is not recorded on the server yet: 83fdf4585f8a (last: the server could not be reached: could not connect (Connection refused (os error 111))); The next `barca get` or `barca run` against the server sends it first; `barca status --state unsynced` lists the results.
```

Doctor only reads: it changes nothing on the server and sends no pending post. The scheduler
counts are those of `GET /schedule`, the numbers the web UI shows. `barca status --problems`
lists the schedules that are paused or failing with their nodes' errors (`barca docs status`).

## Local-only debugging profile

Run this block from your project root. Use local input fixtures in your pipeline/config;
these overrides disable remote artifacts and history backups, not application-level I/O.
The `local` environment uses a separate cache namespace.

```bash
export BARCA_REMOTE=off
export BARCA_ENV=local
barca doctor --pretty
barca get --refresh-all
```

`--env` overrides `BARCA_ENV` for an individual command.

See also: `barca docs remote` (store configuration), `barca docs discovery` (project roots and
source selection), `barca docs server-mode`, `barca docs agents` ("Debugging pipelines").
