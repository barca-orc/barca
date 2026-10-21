# Changelog

Release notes for barca, newest first: each `## <version> (<date>)` section is published as that
version's [GitHub release](https://github.com/barca-orc/barca/releases) body, and changes not
released yet collect under `## Pending`. Releases 0.1.2 to 0.23.x have their notes only on
GitHub Releases, apart from the upgrade notes kept below for 0.10.0 to 0.20.0; the "0.1.2 to
0.8.0" summary below lists what the earlier of them shipped.

## Pending

### Removed

- Committed junk and two broken examples (#497): the three `benchmarks/*/airflow/.venv`
  symlinks to macOS paths, the 0.1.5 macOS wheel under `benchmarks/partitioned_10k/wheels/` and
  `examples/chain_100/stderr.txt`. The Airflow harnesses take their venv from
  `BARCA_BENCH_AIRFLOW_VENV` (`benchmarks/README.md`); `examples/nested_tasks` and
  `examples/pathological_assets` load again (their `barca.toml` dropped the unsupported
  `[project]` table), and a test runs `barca list` in every example.

### Documentation

- `CHANGELOG.md` is the source of release notes (#504). The release workflow publishes the
  version's `## <version> (<date>)` section as the GitHub release body, from a `release-notes`
  job that the GitHub release and the PyPI upload both need, so a version without a section
  publishes nothing (`scripts/changelog-section.py`). `cliff.toml` and git-cliff are gone. Every
  section now uses `## <version> (<date>)` and the Breaking, Features, Bug Fixes, Performance,
  Removed and Documentation subsections, and the GitHub release bodies of 0.24.2 to 0.26.1 were
  rewritten from their sections.

## 0.28.1 (2026-10-21)

### Bug Fixes

- Concurrent `barca get` / `barca run` processes in one project no longer fail now and then with
  `Failed locking file '.barca/metadata.db'. File is locked by another process` (exit 3) (#537).
  Since 0.28.0, setting up the metadata DB released barca's cross-process lock before closing
  the database, so another process could open it while it was still held.

- An empty metadata.db is initialized, not refused (#537). A 0-byte `.barca/metadata.db` holds no
  history: a `touch`, a backup tool or a script that opened the path before barca created the
  database leaves one. `barca serve`, `get` and `run` now initialize it as a new database, with
  one line on stderr naming it (`[barca] note: the metadata DB <path> was empty (0 bytes);
  initialized it as a new database`), where a writer `serve` exited 3. A `--read-only` server
  reads it as no history yet and leaves it alone. A file that is not SQLite, a newer format or a
  malformed schema is still refused, and an empty `.barca/metadata.db` beside a named env's
  database still gets the stray-file warning.

## 0.28.0 (2026-10-21)

### Breaking

- **Breaking:** unscheduled `Always` nodes below a scheduled node, tasks included, now run at its
  ticks under `barca serve` (RFC-0008, #253): a tick of a scheduled node is one run that brings it
  and every `Always` node below it up to date. To keep a node out of its ticks, make it `Manual` or
  give it its own `Schedule`.

- **Breaking:** `barca get`, `barca run`, `POST /runs` (so `--server` too) and ticks no longer
  recompute a stale `Manual` node, upstream or the target of `barca get`, unless `--refresh` names
  it (RFC-0008, #253): they read its latest result and print
  `[barca] note: <name> is Manual and stale; ...`. Name it in `--refresh` (or use
  `--refresh-all`) to recompute it. A `Manual` node that has never been computed is a usage error
  (exit 2) naming that command. A `Manual` task named as the target of `barca run` runs. The older
  `POST /get/{target}`, `POST /run/{target}` and `POST /run` aliases are unchanged.

- **Breaking:** a writer `barca serve` whose metadata DB cannot be set up exits 3 before it binds,
  naming the file and what to do (upgrade barca, `barca state restore`, or `barca state reset`),
  where it used to keep serving with the scheduler off and one `scheduler disabled` line (#513).
  A database barca cannot use (empty, not SQLite, a newer format, a malformed schema) is now one
  error, `reason: "metadata_unusable"` in the envelope (new optional key; CLI and HTTP), instead
  of a raw SQL error such as `Parse error: no such column`; `get` and `run` on a 0-byte
  `metadata.db` now exit 3 instead of starting a new history in it. A `--read-only` server keeps
  serving and reports the error. `GET /health` gains `scheduler_enabled` and `metadata_error`.
  `serve`'s startup line is always printed and ends with `; metadata <path> (db format <n>)`, and
  a stray empty or invalid `.barca/metadata.db` beside a named env's database gets a warning
  naming both. A writer now waits for another process's DB lock (up to 60 s) before it binds.

### Features

- `barca serve` gives `Always` and `Manual` their run-time meaning (RFC-0008, #253). A tick of a
  scheduled node is one run with several targets: an asset is computed when it has no result for
  its current code and inputs, a task fires when its last successful run used other inputs (or a
  task it reads ran since, or it never ran). The tick's run keeps `triggered_by: schedule` and
  `scheduled_at`, and its `barca.run` span names every target. A failing scheduled node retries at
  every tick up to the new `Schedule(cron, max_failures=N)` (default 5), then is skipped and
  reported `failing` (`failures`, `max_failures`, `failing` in `GET /schedule`) until an explicit
  request or an input change; ticks skipped while a job is paused or still in flight do not count.
  Consecutive failures are kept in a new `schedule_failures` table, added in place within metadata
  format 1.

- **Reset the metadata database** from the web UI and with `barca state reset` (#508). A
  **Reset database** button on the "Can't load pipeline" panel and in the sidebar, behind a dialog
  that says what happens and how to undo it, calls the new `POST /state/reset`. The database and
  its `-wal` and `-shm` move aside to `metadata.db.reset-<UTC>` in the same directory and an empty
  one takes its place; nothing is deleted, artifacts stay, and a database that cannot be opened
  or migrated moves as it is. Refused with `409` (exit 3 on the CLI) while a run is pending or
  running, `403` under `--read-only`, `503` while draining. `barca state reset` asks in a
  terminal (`--yes` otherwise) and refuses while `barca serve` holds the environment. Undo with
  `barca state restore --from <the moved file>`: `--from` now also takes a database file's path.
  Experimental.

### Bug Fixes

- `barca serve` skips a cron tick while a `barca get` or `barca run` started from a shell on the
  same project (the same `.barca/`) is running the same node, as it does for a run started over
  HTTP (#479): the skip names the CLI run's id on stderr, in `GET /schedule` (`skipped_for`) and
  on the skipped `barca.run` span. A `running` row whose process is gone (a killed run) does not
  block. A run now records its job (`runs.job`) when it starts, not only when it ends.

### Performance

- Recording a run in the metadata database prepares each per-row statement once instead of
  re-parsing its SQL for every row (#460): the step rows the recorder writes during the run, the
  terminal ledger's success and failure rows, the read stamps (`last_read_at`/`evicted_at`),
  pending uploads, captured logs and the cost estimates, which now commit in one transaction
  instead of one per partition key. A cold three-level partition chain at 20,000 keys goes from
  12.0 s to 7.1 s, and the warm run from 3.3 s to 2.5 s. The rows written are unchanged.

- Skipping the dependents of failed partition keys no longer scans the ready queue once per
  skipped key (#461). When half of N keys failed while the rest waited, the coordinator touched
  the queue about N²/4 times (25 million at 10,000 keys); it now touches it about 3.5 N times.
  The ready queue keeps a membership set, so a skipped item is taken out in O(1) and its stale
  entry is dropped when it reaches the front. The skipped set is unchanged.

### Documentation

- The site's reference pages that are manual topics (CLI Contract, Discovery, Doctor, Groups,
  Remote Storage, Server Mode, SQL, Telemetry) are generated from `crates/barca-cli/docs/*.md`
  with `barca docs <topic>` references turned into links, and `cargo test -p barca` fails when one
  differs from its topic (#501). This brings back in step the pages that had drifted (`barca sql`
  reads CSV and evicts its `.barca/sql-cache/` copies; `get --json` always returns a pointer), adds
  Server Mode and Groups to the sidebar, and lists the Reference pages alphabetically.

- `barca docs cache` reads concept, behaviour, reference: what a run hash covers, what is not
  followed, the cached/stale/evicted/unsynced states, forcing a recompute, the size limit. Its
  0.18/0.19 upgrade notes moved to this file; its implementation guarantees to
  `specs/cache-decisions.md` (#500).

## 0.27.0 (2026-10-20)

### Breaking

- **Breaking:** `barca serve` drains on SIGTERM or SIGINT instead of cancelling its runs at once
  (#190). New runs get `503` with `Retry-After`, the scheduler stops, `GET /health` reports
  `"status": "draining"`, and runs in flight finish, uploads included, for up to
  `--drain-timeout` seconds (new flag; `[server] drain_timeout` in `barca.toml`; default 30). The
  server then exits 0. When the deadline passes or a second signal arrives first, the runs left
  are cancelled with reason `server shutdown` (in their logs, in `/status` `error`, and as
  `barca.reason` on the `barca.run` span) and the server exits 1, where it used to exit 0.
  For the old immediate cancel use `--drain-timeout 0` (it exits 1 when it cancels a run). Give
  the supervisor a stop grace period longer than the drain timeout (Docker's default is 10 s).
- **Breaking (HTTP API):** every error body is the CLI error envelope (`error`, `code`, `kind`,
  `remediation`, ...) instead of `{"error"}` alone (`error` is still there). `GET /assets` and
  `GET /assets/{name}` return the `barca list --json` node shape (`freshness` a lowercase word,
  cron in `schedule`) and the `barca stats --json` body (`id` instead of `node_id`).
  `GET /runs/{id}` returns the flat run view with its `outcome` instead of `{run, steps, logs,
  result}`. Run ids returned by the trigger routes are the database run ids. `POST /run`,
  `POST /run/{target}`, `POST /get/{target}`, `DELETE /run/{run_id}`, `GET /status/{run_id}` and
  `GET /events/{run_id}` stay as aliases for one minor release; use `POST /runs`,
  `DELETE /runs/{run_id}`, `GET /runs/{run_id}` and `GET /runs/{run_id}/events`. Event streams
  now end after `run_finished` and carry an `id:` per event.
- **Breaking:** Datadog dashboards and monitors need updating. The `<DD_SERVICE>-python` service
  is gone: `barca.execute` and library spans are in `DD_SERVICE`, and libraries are no longer
  patched unless opted in (`DD_PATCH_MODULES=<lib>:true` or `BARCA_TRACE_LIBRARIES=1`).
  `barca.run` resources use short names (`run publish`, was `run pipeline.py:publish`);
  `barca.step` and `barca.execute` resources are the node's short name (was the node id, with
  the partition key, and the job for `barca.execute`): filter by `@barca.node_id` or
  `@barca.partition` instead. The step tag `barca.node` is `barca.node_id`. Partition keys are
  `barca.partition` child spans of one `barca.step` per node. The `barca.serve.start`,
  `barca.serve.heartbeat` and `barca.serve.stop` traces are replaced by the `barca.serve.up`
  gauge (1, and 0 on stop) and its siblings: move monitors on those traces to the metric.
  Span `type` is `worker` (was `custom`).

### Features

- `barca serve` runs the server half of the `--server` run contract (RFC-0007 §3.4, #393).
  `POST /runs` takes the flags of `barca get` / `barca run` as JSON and answers with the run's
  database id at once (`dry_run` answers with the `--dry-run` JSON); targets are checked first,
  in the CLI's words. `GET /runs/{run_id}/events` streams every stderr line a local run prints
  (`line`, tagged `when: agent | always`), step output (`log`), step results (`step_finished`,
  with `cached: true` for cache hits) and `run_finished` with the outcome, each event numbered
  (`id:`); a slow or reconnecting reader receives every event once, and the stream ends after
  `run_finished`. `GET /runs/{run_id}` answers with the outcome (the exact `--json` body, or the
  failed-run body and error envelope) from the metadata DB, so it survives a restart;
  `DELETE /runs/{run_id}` cancels; `GET /runs/{run_id}/output` serves the final artifact.
  `GET /list`, `/nodes`, `/history` and `/stats/{name}` return the CLI's `--json` bodies byte
  for byte, `GET /project` describes the deployed tree, and `GET /health` adds `api_version`,
  `env` and `store_uri`. Runs record `triggered_by` (`manual`, `api`, `schedule`, `client`) and,
  for scheduled runs, `scheduled_at` and `schedule_id`, in `barca history --json`, `GET /runs`
  and the `barca.run` span (`barca.trigger`, `barca.scheduled_at`, `barca.schedule_id`, #434).
  The web UI uses the new routes and shows what started each run.
- `POST /schedule/{id}/pause` (optionally `?until=<RFC 3339>`) and `POST /schedule/{id}/resume`
  hold one schedule back without `--no-schedule` (#435). `GET /schedule` reports `paused`,
  `paused_until` and `skip_reason`; a paused job's ticks are skipped with reason `paused` (also
  on the telemetry span) and are not caught up. Pauses are kept in `schedule_state`, so a restart
  does not resume a paused job.
- `--server [<url>]` runs `get`, `run`, `status`, `list`, `history` and `stats` on a
  `barca serve` and prints the answer as the local command prints it: the same stdout JSON, the
  same stderr lines, the same exit codes, rendered by the same functions (#394, experimental).
  With no URL it uses `BARCA_SERVER`, then `[server] url` in `barca.toml`, and says so once on
  stderr; nothing configured is a usage error. `--local` ignores a configured server (and is a
  usage error with `--server`); `--server` on `serve`, `plan`, `sql`, `doctor`, `logs`, `docs`
  and `version` is a usage error. A run streams its progress and step output, and Ctrl-C cancels
  it on the server (a second Ctrl-C exits 130 at once). A server that cannot be reached, is not
  barca or speaks another API version is exit 3 naming its URL; a stream that drops mid-run is
  exit 3 naming the run id. A named file that differs from the deployed one prints a note.
  `get --server` prints the server's artifact paths (paths by default); `-o value` and
  `--partition` work as locally (`RunRequest.values`, `RunRequest.partition`), and `-o url` is a
  usage error with `--server`.
  `barca.get(..., server=True)` and `barca.run(..., server=True)` do the same from Python.
  `barca.Client` is deprecated in favour of them and warns when constructed. The binary now
  carries an HTTPS client (`reqwest` with `rustls`); `POST /runs` with `files` now reads only
  those files, as the CLI does. See `barca docs server-mode`.
- Server mode's ledger (RFC-0007 §3.4 and §4.1, #402), experimental. With `[server] url` in
  `barca.toml` (and `[remote]` pointing at the server's store), `barca get` and `barca run`
  still run on this machine but ask the server's ledger which run hashes exist
  (`POST /ledger/lookup`, one request for the plan), fetch the hits from the store, compute the
  misses here and, once their uploads are confirmed, post the run (`POST /ledger/runs`). The
  server checks every object in its store (a directory store by SHA-256, an object store by
  size) before it records the run as `triggered_by: client` with the laptop's run id in the new
  `client_run_id` (also in `barca history --json`); a missing or mismatched object is `409`
  naming the node and records nothing. A refused or undelivered post leaves the run a success:
  a warning names it, it stays `unsynced` in the new `ledger_posts` table and the next command
  against the server sends it again. A run some of whose uploads are pending (#390) is not
  posted. A run hash the ledger holds with other bytes prints the
  non-determinism warning once. A server that cannot be reached is exit 3; a different or
  missing store is exit 2. The ledger is reached over `http://` or `https://` with the
  `--server` client, from `[server] url` or `BARCA_SERVER`; `--local` runs against this
  machine's ledger and touches nothing on the server (error remediations name it). With a
  server configured, `barca status`, `barca history` and `barca stats` answer from the server
  (`[barca] server <url> (<source>)` once); `barca status` adds `local` per node (this machine
  has a copy) and shows a result whose post is pending as `unsynced` with
  `cache.reason: "not_in_ledger"` (`summary.unsynced` counts them). New manual topic:
  `barca docs server-mode`.
- `barca serve`: `BARCA_LOG=json` writes every line as one JSON object on stdout, with `ts`, a
  real `level` (`info`, `warning`, `error`), `msg`, the `run_id` and `node` it is about, and
  `dd.trace_id`/`dd.span_id` when the Datadog integration is on, so a container's logs join the
  run's trace and are no longer all filed as errors. A step's printed output becomes `info` lines
  of its run. Experimental; without it nothing changes, and `get`/`run` keep progress and errors
  on stderr (`BARCA_LOG` on another command prints a warning). `barca.step` spans gain
  `barca.upstream` (direct upstream ids) and, on a step that ran, `barca.digest` (#434).
- Datadog traces show the pipeline (#482). Every span reports as one service, `DD_SERVICE`
  (default `barca`), `type: worker`, `span.kind: internal`, with `env`, `version`,
  `barca.version` and `barca.project`. `barca.run` is named by the command and the targets' short
  names (`get daily_revenue`) and, on a failure, names the first failed asset and its error
  (`barca.failed_asset`, `error.type`, `error.message`). `barca.step` is one span per node, named
  by the node, with `barca.asset`, `barca.node_id`, `barca.file`, `barca.group` (nested `group()`
  path), `barca.freshness`, `barca.schedule`, `barca.cached` and outcome `skipped` for a planned
  step that did not run. A partitioned node is one step with `barca.partitions.total/executed/
  cached/failed` and a `barca.partition` child per key that ran (up to 200 per node, 2,000 per
  run) and per failed key. Python `barca.execute` spans are named by the node too. Library spans
  are opt-in: `DD_PATCH_MODULES`, `DD_TRACE_<LIB>_ENABLED`, or `BARCA_TRACE_LIBRARIES=1`
  (experimental). Trace ids are 128-bit (`_dd.p.tid`); traces go to the Agent's `/v0.4/traces`
  as JSON and are kept with sampling priority 2. `barca serve` reports `barca.serve.up`,
  `barca.serve.schedules`, `barca.serve.files` and `barca.serve.nodes` as DogStatsD gauges
  (`DD_DOGSTATSD_URL`, `DD_DOGSTATSD_SOCKET`, or `DD_AGENT_HOST`:`DD_DOGSTATSD_PORT`), with a
  Datadog event on start and stop.

## 0.26.1 (2026-10-20)

### Bug Fixes

- Metadata backups no longer overwrite each other when several directories back up to one
  store location in the same second (#487). A backup is now named by the UTC time to the
  millisecond plus a short suffix from the host name, the database's path and the copy's digest
  (`2026-10-14T18-02-11.482Z-3f9a1c2e.db`). `barca state backups` stays newest first and
  `barca state restore --from <name>` takes both these names and the to-the-second ones 0.26.0
  wrote, which are still listed and pruned.

### Removed

- Leftovers of the shared history: barca-core's `state_sync` module (its deadline and
  helper-error code moved to `helper_proc`; the WAL-fold wrapper only its own test called is
  deleted), and the cache manual's limitation about eviction marks in a pushed history. Docs,
  specs and comments no longer describe a shared-history push. No user-visible surface changes.

## 0.26.0 (2026-10-20)

### Breaking

- **Breaking:** a remote store fails soft once the steps are done (#390). When every step
  succeeded and only an artifact upload (or a metadata backup) failed, `get` and `run`
  exit 0 (was 3) with one warning, add `uploads: {pending, error, nodes}` to the run JSON and
  `--dry-run`, and record the results; `barca status` shows them `unsynced` / `local_only`
  (`summary.unsynced`) until the next command that opens the store uploads them first. Read
  `uploads` instead of exit 3 to detect unpublished results.

- **Breaking:** the shared history (`optimistic` mode) is removed (RFC-0009, #403; closes
  #243): `[remote] state`, `[remote] state_uri`, `[remote] push_retries`, `BARCA_STATE`,
  `BARCA_STATE_URI`, `BARCA_PUSH_RETRIES`, the pull before each run, the conditional push after
  it and the `metadata.db.prev` copy. A `barca.toml` or environment that still sets one is a
  usage error (exit 2) naming it and barca 0.26.0, even under `BARCA_REMOTE=off`: remove it. A
  single machine gets backups of its history with no setting (below); teams that shared history
  through a bucket move to a server (RFC-0007), and a wiped machine gets its history back
  with `barca state restore`. The old `state/metadata.db` object is ignored.

### Features

- With a remote store (`[remote] uri`), the metadata DB is backed up on the minute checkpoint
  and at the end of every `get` and `run`, and every minute it changed under `barca serve`, to
  `.barca/backups/` and `{uri}/{env}/state/backups/` (five copies each, with a `<UTC>.json`
  sidecar). New `barca state backups` lists them and `barca state restore [--from N|NAME]` puts
  one back after validating it, keeping the replaced database as `metadata.db.pre-restore`.
  A backup that fails to upload is a warning and `uploads.error`, never a failed run (#403).

- `barca status` with a store reports `unsynced` with `cache.reason` `store_only` and
  `mismatch` besides `local_only`, by comparing this machine's copy, the store object and the
  recorded digest; `barca get` repairs an `unsynced` target without running a step (upload,
  fetch, or re-upload with the changed-bytes warning and `steps[].bytes_changed`) (#403).

- `barca serve` runs with a remote store (it used to require `BARCA_STATE=off`) and backs its
  history up.

- Signed URLs (experimental, RFC-0011, #452): `barca.signed_url(uri, expires=3600)` returns a
  time-limited, read-only HTTPS link to any object in S3 (also R2 and MinIO), GCS or Azure, signed
  with the credentials barca already uses: SigV4 on S3, V4 on GCS (IAM `signBlob` when there is no
  key), and on Azure a service SAS with an account key or a user delegation SAS from
  `DefaultAzureCredential`. `barca get <target> -o url [--expires 30m|12h|7d|seconds]` prints one
  link per result (default 1 hour, at most 7 days); with `--json` each `_barca_artifact` gains
  `url` and `url_expires_at`. Links download as `<node>.<ext>`. The URL goes to stdout only.

- Slack notifications (RFC-0010, #450): set `BARCA_SLACK_WEBHOOK_URL` to an incoming webhook and
  barca posts when a job starts failing, when it recovers, and once a day while it keeps failing,
  for `barca get`, `barca run` and every run `barca serve` starts. `BARCA_SLACK_NOTIFY`
  (`changes`, `failures`, `all`) and `BARCA_UI_URL` (run links) tune it. A delivery failure is
  one warning that never names the webhook URL, and never changes a run's outcome. Runs now
  record their canonical job (`runs.job`) and the last post per job is kept in a new
  `notifications` table, both added in place within metadata format 1. The binary now carries an
  HTTPS client (`reqwest` with `rustls`), shared with the coming `--server` client. See
  `barca docs telemetry`.

- A cached result whose upload is pending is never evicted by the cache size limit, and
  `.barca/backups/` counts toward it but is never evicted. `barca get -o url` uploads a
  `local only` result before signing it.

## 0.25.1 (2026-10-20)

### Bug Fixes

- `barca status` reports a partitioned node's last materialization by a fixed rule: the latest
  run (by row id), and within it the first key in sort order. Before, it was whichever key
  finished last, so two identical runs could report different keys (#475).

- The release workflow's `verify-published-linux` jobs wait for the PyPI simple index to serve a
  new version before installing it, retrying every 20 s for up to 12 minutes (PyPI's CDN caches
  the index with `max-age=600`), so a release no longer needs a manual rerun for index lag (#480).

## 0.25.0 (2026-10-20)

### Breaking

- **Breaking:** `barca get` prints artifact paths by default (#404). In `--json` (and whenever
  stdout is piped) `final_output` is always the pointer `{"_barca_artifact": {"path", "format",
  "size_bytes"}}`, for json artifacts too, and a terminal shows the path with a small json value
  previewed under it. The old default, the value, is `-o value` (`-o value --json` for the JSON
  line); `-o path` prints only the paths. `barca.get()` still returns the value.

- **Breaking:** a partitioned target returns every key, sorted, never one key (#287):
  `final_output` is an object keyed by partition key (`"region=emea"`), and `barca.get()`
  returns a dict of key to value.
  `--partition region=emea,amer` (or a JSON object for compound keys) selects keys, computes only
  those and scopes `--refresh` of the target; `barca.get(..., partition=...)` does the same.

- **Breaking:** `.barca/` now has a size limit, `[cache] max_size` in `barca.toml` or
  `BARCA_CACHE_MAX_SIZE` (`"20GB"`, `"500MB"`, `"20GiB"`, bytes, `"10%"` of the filesystem, or
  `"off"`), defaulting to 10% of the filesystem `.barca/` is on. After every `get` and `run`,
  after every `serve` run and once a minute under `serve`, a cache over its limit is evicted
  down to 80% of it: leftovers (dead processes' staging, `*.moved-aside*` directories,
  `sql-cache/` copies), then superseded results, then local copies the artifact store holds,
  then the least recently read results; never the command's own outputs, results a run in
  flight holds, a storeless `Manual` node's current result, or the metadata databases. An
  evicted result is fetched or computed again under the same run hash when something needs it
  (step and status reason `evicted`). A project whose cache was larger than the limit sees more
  fetches and recomputes after upgrading; `max_size = "off"` restores the old unbounded
  behaviour. New, experimental: `barca gc [--dry-run] [--max-size SIZE] [--json] [--env ENV]`,
  the `cache` key on `get`/`run` results, the `cache_size` check of `barca doctor`, and `cache`
  in `GET /health`. `materializations` gains `evicted_at` and `last_read_at` (additive, metadata
  format 1); rows are never deleted (RFC-0012, #457).

## 0.24.3 (2026-10-20)

### Bug Fixes

- Open a metadata database last written by barca 0.21 to 0.23. Since 0.24.0, every command
  refused one with exit 3 ("its `materializations` table has no `bytes_changed` column"), and
  `barca status` failed with "no such column: bytes_changed". barca now adds the missing column
  in place, in one transaction, keeping every row; the older release still reads the result.

### Performance

- Cold runs of large partitioned pipelines scale linearly with the number of keys again: a
  three-level chain of 20,000 keys runs cold in about 9.5 s instead of about 245 s, with a
  0.55 GB coordinator instead of 5.3 GB (#345). Each key of a step used to wait on every key of
  the previous step in its stream through its own edge; one shared gate per step now keeps the
  same ordering. Dispatch also finds an upstream's output by id instead of scanning every
  output, and the end-of-run check no longer scans every item on each event.

## 0.24.2 (2026-10-20)

### Bug Fixes

- `barca serve`: a cron tick is skipped while any run of the same node is pending or running,
  including one started by `POST /get`, `POST /run/{task}` or `POST /run`, not only one the
  scheduler started (#432). The skip is logged with the run in flight, reported as
  `last_skipped` and `skipped_for` in `GET /schedule`, and sent to telemetry as a `barca.run`
  span with status `skipped` and `barca.reason: in_flight`.

- Refresh dependency lockfiles (`uv.lock`, `ui/`, `site/`, the dagster benchmark) to clear the open critical and high Dependabot alerts: PyJWT 2.15.1, aiohttp 3.14.5, cryptography 50.0.2, urllib3 2.8.0, oauthlib 4.0.0, astro 7.4.1, wrangler 4.151.3, react-router 7.18.4 and vitest 4.1.12, plus patched transitives.

- Failed `barca.step` telemetry spans now report their duration, from the first attempt to the
  permanent failure (retries included; a timeout reports about its timeout), instead of a
  zero-length span at the end of the run.

- A worker that dies while running a step (#433) now names how it ended instead of a bare
  `worker disconnected`: `worker for <node> exited with SIGKILL (possibly the OOM killer)`,
  another signal by name, or `exited with code N`, in the step error, the run JSON and the
  failure summary. `get`, `run` and `serve` print `[barca] env <env> (<source>); artifact store
  <store>` once on stderr at startup when `default_env` from `barca.toml` or an artifact store
  is in effect.

- Follow a project module reached through another module's `import` (`from wrapper import
  helpers` or `wrapper.helpers.f()` where `wrapper.py` does `import helpers`), and follow
  import chains at any depth instead of stopping six project modules deep; modules that
  import each other terminate. Editing such a helper now invalidates its consumers. Existing
  affected consumers may recompute once after upgrading.

## 0.24.0 (2026-10-15)

### Breaking

- **Breaking:** agent completion and permanent failure lines append `attempts=N` after
  retries. Allow this optional suffix when parsing those lines, or read the structured
  `steps[].attempts` field instead.

- **Breaking:** removed `barca get|run --no-cache` and `barca.get(no_cache=...)`.
  Use `--refresh-all` and `refresh_all=True`, respectively. The CLI alias is now an
  unknown-flag usage error (exit 2); Python rejects the removed keyword.

### Features

- Add `barca doctor` prerequisite checks, credential identity diagnostics and a documented local-only debugging profile.

- Run JSON reports executed-step durations and attempts, with slow module imports
  reported separately. History JSON totals recorded materialization attempts; stats
  retains per-materialization attempts. Timeout errors identify the step id.

### Bug Fixes

- Follow project helper references in exception handler types, including exception tuples
  and `except*`. Editing those helpers invalidates their consumers. Existing affected
  consumers may recompute once after upgrading.

- Follow project helper references inside set literals. Editing those helpers now invalidates
  their cached consumers. Existing affected consumers may recompute once after upgrading.

- Follow project helper calls in assignment receivers and indices, including annotated, unpacked, loop, context-manager and comprehension targets. Editing such a helper now invalidates affected cached assets.

- Follow project helper references in `for`/`while` `else:` blocks so edits invalidate affected cached assets.

- Follow project helper values, classes and mapping keys in `match` patterns so edits invalidate affected cached assets.

- Follow project helper references in ordinary function/class decorators, including nested definitions and decorator-factory arguments.

- Retain project-helper provenance for unpacked and augmented module constants, including aliases captured before later overwrites. Consumers conservatively hash the module source and static references.

- Track project star imports in ordinary helper modules conservatively, including functions/classes, captured constants, forwarded exports, multiple/conditional imports and transitive stars.

- Follow project helper references in function and lambda default arguments, including
  nested functions and keyword-only defaults. Editing those helpers now invalidates their
  consumers. Existing affected consumers may recompute once after upgrading.

- Retain project-helper provenance for names, functions and classes bound inside module-level `for`, `while`, `with` and `match` blocks. Consumers conservatively hash module source and static references.

## 0.20.0 (2026-10-08)

### Bug Fixes

- A run records the kernel it ran on (boot id), its pid namespace, its start time and a marker
  in `.barca/run-owners/`, and `barca history` reports it `interrupted` only on an observation
  that its process is gone (`specs/cache-decisions.md`); otherwise it stays `running`. A run
  started by 0.19.0 or earlier is still judged by its process id and host name, so in a
  container, on another machine or once its process id is reused it can stay `running`. 0.18.1
  reported a run killed by a restart of a machine whose project is on a network filesystem as
  `interrupted`, from the host name and process id; from 0.20.0 such a run stays `running`.

## 0.19.0 (2026-10-08)

### Breaking

- **Breaking:** the definition part of the run hash covers the decorator arguments that count
  (`barca docs cache`, "Which decorator arguments count") instead of the decorators' text as
  written. Up to 0.18 any edit there (a new partition key, a description, a comment, a
  reformat) re-ran the step for every key, and everything downstream (#283). Every step's hash
  is computed differently, so **every step recomputes once** on the first run after upgrading,
  in every project and every environment (`--env`); the run after that is served from cache.
  Results written by 0.18 stay in `.barca/metadata.db` and on disk under their old run hashes;
  0.19 neither reads nor deletes them. With an artifact store, 0.19 writes every result under
  its new run hash and leaves 0.18 results in the store alone; the two versions do not reuse
  each other's results. Run hashes in the JSON of `get`, `run` (also `--dry-run`) and `status`
  change value; no command, flag or JSON key changed.

## 0.18.0 (2026-10-07)

### Bug Fixes

- The run hash follows classes, imports inside function bodies, aliased `from` imports, modules
  used as values, and root modules imported from a subdirectory (#194). A step recomputes once
  after upgrading, with its downstream steps, if its function or a helper it reaches uses: a
  class defined in the project; a project module imported inside a function or class body;
  `from module import name as alias` of a project module; a project module as a value
  (`getattr(helpers, name)`, passing `helpers` along); from a pipeline in a subdirectory, a
  module in the project root; a helper module named like a pipeline file in another directory
  (both are hashed now); or, for a pipeline file inside a package, project modules imported as
  `from .helpers import f` or `from pkg.helpers import f` (these now resolve from the root, as
  they do when the step runs). A step whose helpers import the same name from two different
  modules outside the project (`from numpy import array` and `from jax.numpy import array`) had
  two possible hashes, picked at random on every run, and missed its cache about half the time;
  it now has one, and recomputes once if its last result was recorded under the other. Every
  other step keeps its hash and its cached results.

## 0.12.0 (2026-10-03)

### Features

- A sensor's output is part of the run hash of every asset that reads it (#188). Previously an
  asset reading a sensor was served from cache whatever the sensor returned. Assets that read a
  sensor re-run once after upgrading; pipelines without sensors keep their run hashes and
  caches. `--dry-run` and `barca status` predict from the sensor's last recorded output, and a
  sensor recorded before this version has none, so its consumers are `unknown` until it runs.

## 0.10.0 (2026-10-02)

### Breaking

- **Breaking:** `--refresh` cascades downstream by default: it re-runs the named assets and
  every asset downstream of them in the target's cone (#168). Previously it re-ran only the
  named assets and left their downstream assets cached; `--no-cascade` keeps that behaviour.

## 0.1.2 to 0.8.0 (summary)

### Breaking

- `barca run` is now cache-aware for upstream assets by default (same as `barca get`);
  the task itself still always re-runs. Previously every upstream asset was force-rerun.
  `--burst` is renamed to `--refresh <a,b>`; add `--refresh-all` (alias `--no-cache`) to
  restore the old default. Python API: `barca.run(..., burst=[...])` is now
  `refresh=[...]` / `refresh_all=True`.

### Features

- `--dry-run` on `barca get` and `barca run`: reports, for the exact command and flags, which
  steps are served from cache and which will run and why (`cached` / `run` / `partial` /
  `unknown`), without executing or writing anything. Real runs now report the same per step in a
  `steps` array (and `[barca] step:<id> cached` in `--agent` mode). The dry run and the real run
  share one decision function, and tests check that the predicted `will_run` equals the real
  run's `steps_executed`.

- `barca docs`: a manual compiled into the binary (concepts, output formats, caching, tasks,
  partitions, scheduling, runnable examples, conventions for scripts and AI agents). Every
  command's `--help` now ends with runnable examples, and `list`, `history` and `stats` take
  `--json`. Docs, help examples and JSON output are now part of the feature workflow
  (see CLAUDE.md); tests execute the manual's examples.

- DuckDB: steps returning a `DuckDBPyRelation` or a pyarrow `Table` are written as parquet
  without `serializer="parquet"` (a relation used to crash with "cannot pickle", a Table was
  silently pickled). Barca now owns one DuckDB connection per worker process and binds every
  duckdb-typed input as a view named after its parameter for the step, so SQL by name works
  in helpers without bind code; `barca.duckdb_connection()` exposes the connection for
  one-time configuration (extensions, credentials, settings). Steps that mix relations from
  their own `duckdb.connect()` with inputs still fail, but the failure now carries a `barca:`
  note explaining the conflict and the fix instead of only DuckDB's cryptic message.

- Sub-minute cron scheduling: `Schedule(...)` now accepts a 6-field cron with a
  leading seconds field (e.g. `*/15 * * * * *` — every 15 seconds); the `barca serve`
  scheduler evaluates at 1-second resolution. 5-field crons are unchanged (seconds
  pinned to `0`).

- Built-in cron scheduler in `barca serve`: enforces `@asset`/`@task`/`@sensor`
  `freshness=Schedule(...)`, timezone-aware (`--timezone`), durable catch-up on
  restart, self-overlap skip, and live status via `GET /schedule`; toggle with
  `--no-schedule`.

- Shared remote state and content-addressed artifacts: `barca.toml` config, `--env`
  environment separation, and remote artifact storage (Azure/S3/GCS/R2 via fsspec)
  with the metadata DB pushed/pulled as a blob.

- Minimal standalone scheduler example under `examples/scheduler`.

- Async-native core: the async runtime is owned by the caller, with cancellable runs.

- Centralized cron parsing behind a single `CronExpr::parse` helper so validation,
  the scheduler, and the `/schedule` handler share one grammar.

### Bug Fixes

- `--refresh` is no longer easy to misuse: a name that is not an upstream asset is an error that
  lists the valid names (it used to silently do nothing), `--refresh a b` says to use a comma
  instead of failing with "No such file", and barca warns when a refresh leaves cached assets
  downstream of it stale (run hashes cover upstream hashes, not outputs, so they stay cached).

- A step that runs for 15 s or more is now reported on stderr (`[barca] still running (45s): ...`,
  `BARCA_PROGRESS_SECS` to tune) in every mode, so a slow step no longer looks hung.

- Partitioned assets are now cached per key. Previously every partition re-executed on each
  run (a `TODO` in the coordinator); now unchanged keys are served from cache and only new or
  changed keys run, with the fan-in re-running only when its inputs change.

- Concurrent barca processes in one project no longer fail with `Failed locking file
  '.barca/metadata.db'. File is locked by another process`. Turso opens the DB
  single-process (its multi-process mode is experimental), and `barca get`/`run` held the DB
  open for the whole run. Barca now holds a short cross-process lock (`.barca/metadata.db.lock`)
  only while it reads or writes the DB and releases it while your Python runs, so processes
  queue instead of failing.

### Documentation

- Surface the task-scheduler workflow in the README and docs splash; new
  "Barca vs cron / systemd timers" comparison; sub-minute cron documented across
  the Scheduling guide and reference.

- Real `freshness=` (and other) keyword parameters on the `@asset`/`@sensor`/`@task`
  Python stubs for IDE autocomplete and type checking.

## 0.1.1 (2026-06-04)

### Features

- Correct __main__.py entry-point description in CLAUDE.md
- Clean up stale files from pre-Rust rewrite
- Polish: Result errors, default subcommand, versions, test isolation, README
- V0.1.1: Engine refactor + clap CLI + Python API

### Bug Fixes

- Fix 3 PR review issues: artifact key collision, binary caching, deterministic partition get

## 0.1.0 (2026-06-04)

### Features

- File-based artifact persistence: replace JSON-over-stderr with format-aware artifacts
- Require Python >=3.12, build wheels for 3.12/3.13/3.14
- Restore full release workflow from prior working config
- Use PYPI_API_TOKEN secret for PyPI publish (already configured)
- Engine hardening: refactor, protocol, first-class partitions, P0 fixes, CI/CD
- Rewrite gap tests: cross-file, sensor, partition (drop ops concerns)
- Add gap tests documenting known cache/staleness limitations
- Add cache fuzz tests: 100 random DAGs × mutate × verify staleness
- Add dependency cone analysis for staleness detection
- Implement `barca get` with cache-hit detection and staleness tracking
- Add cache/staleness integration tests (TDD: all currently fail)
- Add dagster server-mode partition benchmarks
- Add dagster/prefect for partition benchmarks, run full comparison
- Add partition benchmarks with partition-aligned stream assignment
- Implement dynamic partition eval and partitions_from resolution
- Implement static partition expansion and dynamic partition parsing
- Clean up DagNode duplication, remove dead code, tighten types
- Comprehensive planner tests documenting DAG shape → execution plan mapping
- Make parser pure: return Result, add 10 edge case tests
- Add integration tests for CLI behavior
- Add Airflow benchmarks: trivial, chain_100, deep_diamond, fan_out_500_50ms
- Complete benchmark fairness: server-mode dagster, parallel prefect
- WIP: Add server-mode benchmark scaffolding for dagster
- Address benchmark fairness concerns (ExSidius/barca#35)
- Add ETL pipeline, wide join, and incremental backfill benchmarks
- Add large_payloads, map_reduce, and multi_file_discovery benchmarks
- Add deep_diamond, wide_layers, and mixed_io_cpu benchmarks
- Wire up multi-process dispatch: workers communicate via stdout, Rust owns DB
- Add execution planner: Dag → decompose → Topology → plan → ExecutionPlan
- Add spaceflights, fan_out_500, and fan_out_500_50ms benchmarks
- Add benchmark results README
- Use SmallVec for inputs/sinks/partition_keys (stack allocation for small collections)
- Add comprehensive grammar spec tests, fix int parsing
- Rewrite barca as Rust binary with Python execution runner
- Add AGENTS.md (#33)
- Replace Datastar/Jinja2 UI with React + shadcn/ui (#31)
- Add comprehensive tests for multicore asset execution (#30)
- Add learning path to README, show artifact path after refresh, fix broken examples (#29)
- Replace plain text CLI output with Rich-formatted tables and panels (#28)
- Consolidate three packages into one + GitHub install instructions (#25)
- Update documentation with GitHub installation and CLI options (#24)
- Switch from sqlite3 to libsql as primary DB driver for MVCC concurrency (#23)
- Update documentation: reflect notebook workflow, add MkDocs, improve code docs (#22)
- Add notebook workflow helpers: load_inputs, materialize, read_asset, list_versions (workflow 7) (#21)
- Make sensors first-class nodes with dedicated CLI, API, and display (workflow 6) (#20)
- Add spaceflights benchmark: 10-asset diamond DAG adapted from Kedro (#19)
- Add test coverage roadmap with detailed spec for 100+ new tests (#18)
- Improve dependency tracking with codebase merkle hash, snapshots, and batch worker (#17)
- Add benchmark suite: Barca vs Prefect vs Dagster orchestration performance (#16)
- Refactor to align with barca.allium: freshness, sinks, run/dev/prune (#32)
- Release polish: metadata, changelog, serializer= parsing, __version__

### Bug Fixes

- Fix PyPI license metadata: use SPDX identifier instead of text table
- Fix all 11 PR review issues: CI, correctness, latent bugs, nits
- Fix all 3 staleness gaps: cross-file imports, sensor bypass, partition cascade
- Fix chain caching (ordered persist) + add cached benchmarks
- Fix run_hash consistency: 13/13 cache tests pass
- Fix: user print() statements no longer corrupt worker protocol
- Fix all clippy warnings, tighten idiomatic Rust
- Fix thread-safety in MetadataStore for concurrent per-thread usage (#27)
- Fix: make UI reactive to asset state changes after reset/reindex (#14)
- Fix: support bare @asset decorator (no parentheses) (#12)

### Removed

- Drop linux-arm64 from release matrix (simsimd NEON cross-compile failure)
- Remove dead code: plan.rs, NodeState, classify_shape, stats
- Remove AssetStatusBadge web component; use data-persist for dark mode (#15)

### Documentation

- Document Airflow 3 LocalExecutor limitations
- Document Airflow LocalExecutor limitation: requires PostgreSQL for parallelism
- Docs: fix datastar-reference for RC.8 syntax (#13)
- Docs: fix Quick Start so new users can actually run barca (#11)

## 0.0.3 (2026-03-14)

### Features

- Chore: use cross for Linux CLI builds in just release

## 0.0.3rc1 (2026-03-14)

### Features

- Ci: replace release workflow with local just release recipe
- Ci: use macos-latest for x86_64 macOS wheel (macos-13 unavailable)
- Chore: bump all crate versions to 0.0.3
- Ci: fix manylinux glibc compatibility for bundled CLI binary (#10)
- Init (#1)
- Initial commit

### Bug Fixes

- Fix: use command -v to check for cargo-zigbuild
- Fix: use uv tool install ziglang + symlink to zig in setup recipe
- Fix: ignore untracked files in dirty check
- Fix: handle PEP 440 rc versions in just release (convert to semver for Cargo)
