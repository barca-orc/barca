---
title: "CLI Contract"
description: "Every barca command, flag, environment variable, exit code and JSON schema, marked stable or experimental, and the policy for changing them."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/contract.md
---

<!-- Generated from crates/barca-cli/docs/contract.md (barca docs contract). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca site_pages_tests:: (or scripts/update-cli-snapshots.sh). -->

This page is the written-down CLI surface of this version of barca: every command, argument,
environment variable, exit code, JSON output schema, the stderr error envelope and the
`--agent` line formats. Scripts and AI agents can rely on everything marked `stable`.

Barca is pre-1.0, so the surface is **not frozen yet**. What this page guarantees is that no
part of it changes by accident: tests compare it with the real CLI, and a change to the surface
fails CI until this page and the snapshots are updated in the same pull request (see "Changing
the surface" below).

Most of this page is generated. The command and argument tables and the exit codes come from
the CLI definition (`cargo test -p barca`); every JSON schema table and the `--agent` lines come
from running the real binary on a fixture pipeline (`python/tests/test_cli_contract.py`). Text
outside the `GENERATED` blocks is written by hand.

## Stability

- **stable**: kept as documented. Pre-1.0 a breaking change to it needs a minor version bump and
  a "Breaking" line in the release notes. From 1.0 it only changes additively.
- **experimental**: may change in any release, with a note in the release notes. Each one says
  why it is not settled.

### Policy

- **Additive changes are always allowed**: a new command, a new flag, a new JSON key, a new
  value of an enum-like string (a new step `reason`, a new error `kind`), a new stderr line. Parse
  JSON by key and ignore keys you do not know; treat an unknown enum value as "other".
- **Breaking changes**: removing or renaming a command, flag, JSON key or environment variable;
  changing a key's type; changing what an exit code means; changing a stable `--agent` line;
  changing a default (for example what stdout prints when piped).
- **Before 1.0**: a breaking change is allowed. It ships in a minor release (`0.x.0`) whose
  release notes have a "Breaking" line naming it and the replacement, and the same pull request
  updates the snapshots and this page.
- **From 1.0**: changes are additive only. A flag that is replaced is deprecated, not removed: it
  keeps working for at least one minor release and prints a warning on stderr naming the
  replacement.
- **Not part of the contract**: human output (tables, `--pretty` summaries, the progress bar),
  the wording of error messages and remediations, the order of keys in a JSON object, whether
  JSON is printed on one line or indented, and the text of `barca docs` topics.

### Experimental items

The HTTP scheduler diagnostics are experimental as the loop operations and timing
bounds evolve. `GET /health` adds the status value `degraded`; `barca doctor`
reports its reason. `scheduler_enabled` can return to true after a successful
scheduler restart. `scheduler_liveness` on health and the opt-in
`GET /schedule?include_liveness=true` envelope contains `heartbeat_at_ms`,
`operation_started_at_ms`, `operation`, `last_due_slot`, `last_enqueue`,
`restart_count`, `last_exit_reason` and `reason`.

Commands and arguments, from `EXPERIMENTAL` in `crates/barca-cli/src/contract.rs` (the
`Stability` column of the tables below comes from the same list; an argument of an experimental
command is experimental with it):

<!-- BEGIN GENERATED experimental -->
| Command or argument | Why it is experimental |
|---|---|
| `barca get --cached-only` | new in 0.47: completed-result reads and per-key outcome shapes may evolve |
| `barca tokens` | new in 0.36 (RFC-0016): local bootstrap for opt-in project token access |
| `barca tokens bootstrap` | prints a one-use bootstrap credential; token formats may evolve |
| `barca pin` | new in 0.41 (barca docs pins): the JSON shapes, the version list and what a pin covers may change after field use |
| `barca unpin` | new in 0.41 (barca docs pins): the JSON shape may change after field use |
| `barca doctor` | new prerequisite report; check names and provider-specific data may evolve |
| `barca gc` | new in 0.25 (RFC-0012): the eviction order, the low watermark and the JSON shape may change after field use |
| `barca history <RUN_ID>` | new in 0.29.3: the run detail's step keys and human layout may change after field use |
| `barca history --reconcile` | explicit operator recovery for stopped runs whose owner cannot be verified |
| `barca history --status` | new in 0.29.3: the history filters' values and matching may change after field use |
| `barca history --target` | new in 0.29.3: the history filters' values and matching may change after field use |
| `barca history --triggered-by` | new in 0.29.3: the history filters' values and matching may change after field use |
| `barca history --activity` | new in 0.44 (#991): the activity's keys, its window and its human table may change after field use |
| `barca history --since` | new in 0.29.3: the history filters' values and matching may change after field use |
| `barca history --not-triggered-by` | new in 0.44 (#969): the history filters' values and matching may change after field use |
| `barca history --until` | new in 0.44 (#969): the history filters' values and matching may change after field use |
| `barca history --search` | new in 0.44 (#969): what the text is looked for in, and how, may change after field use |
| `barca list --groups` | Organizational group hierarchy and metadata shape are experimental |
| `barca plan` | prints the planner's internal phase/stream layout, which changes with scheduling work |
| `barca sql` | new in 0.13: the view naming and the JSON result shape may change after field use |
| `barca cancel` | Cancel a queued or running server run; the response follows DELETE /runs/{id} |
| `barca queue` | new in 0.29.4: the run queue's JSON shape, its table and the pause's scope may change after field use |
| `barca pause` | new in 0.41: pausing assets for scheduled runs; the JSON shape, the table and what a pause covers may change after field use |
| `barca resume` | new in 0.41: resuming paused assets; the JSON shape and the table may change after field use |
| `barca get --allow-paused` | new in 0.41: runs a paused target by hand; which requests need it may change after field use |
| `barca run --allow-paused` | new in 0.41: runs a paused target by hand; which requests need it may change after field use |
| `barca get --queue` | new in 0.46.0: a place in a server's run queue; the values, what `last` is ordered with and where it shows may change after field use |
| `barca run --queue` | new in 0.46.0: a place in a server's run queue; the values, what `last` is ordered with and where it shows may change after field use |
| `barca serve` | the HTTP API and scheduler are young: opt-in project tokens, routes may change; the older trigger routes remain deprecated aliases for Python Client compatibility, with no scheduled removal; a removal migrates Client first and lands in a minor release; runs share identical in-flight asset steps (new in 0.39.4, #654), with cancellation survival, a durable handoff and a decide-to-lease race still open |
| `barca serve --capacity` | new in 0.31.0 (#628): the admission order across runs and the shape of capacity in /queue and /health may change after field use |
| `barca state reset` | new in 0.28 (#508): the JSON shape and the name of the moved-aside file may change after field use |
| `barca get --expires` | new with -o url (signed links to results in a cloud store); the link format is the cloud's |
| `barca status --sample` | sample rows come from a Python helper whose output may grow |
| `barca status --problems` | orders problems by distance from fresh; work already in hand is omitted unless its cause is requested |
| `barca status --health` | experimental health inspection (#1043) |
| `barca status --root` | experimental health inspection (#1043) |
| `barca status --cause` | experimental health inspection (#1043) |
| `barca status --now` | experimental health inspection (#1043) |
| `barca status --rollup` | new in 0.44.0 (specs/behavior/health.md): counts by health and by cause, the freshness gap and the ranked roots with their actions; the wording of a root's detail and of the printed list may change after field use |
| `barca status --keys` | new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use |
| `barca status --grid` | new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use |
| `barca status --key` | new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use |
| `barca status --needs-run` | new in 0.44.1 (#1036), with --keys: the keys a run computes; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use |
| `barca status --value` | new in 0.44.1 (#1036), with --keys: a dimension's values; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use |
| `barca status --range` | new in 0.44.1 (#1036), with --keys: a stretch of a dimension; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use |
| `barca status --error` | new in 0.44.1 (#1036), with --keys: one error group's failed keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use |
| `barca status --match` | new in 0.44.1 (#1036), with --keys: text over the key; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use |
| `barca status --sort` | new in 0.44.1 (#1036), with --keys: the order of the keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use |
| `barca status --offset` | new in 0.44.1 (#1036), with --keys: the page of keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use |
| `barca status --overview` | new in 0.44.1 (#1036): a partitioned node's keys in one picture (counts, issues and the grid's rows); the JSON shape, the wording of an issue and the bound on the rows may change after field use |
| `barca get --batch` | new in 0.32.0 (RFC-0013): batched partitions; the override's forms and the dry run's batch object may change after field use |
| `barca run --batch` | new in 0.32.0 (RFC-0013): batched partitions; the override's forms may change after field use |
| `barca get --server` | server mode is new (RFC-0007): how the server is chosen and the client's messages may change |
| `barca get --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca run --server` | server mode is new (RFC-0007): how the server is chosen and the client's messages may change |
| `barca run --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca status --server` | server mode is new (RFC-0007): how the server is chosen and the client's messages may change |
| `barca status --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca list --server` | server mode is new (RFC-0007): how the server is chosen and the client's messages may change |
| `barca list --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca history --server` | server mode is new (RFC-0007): how the server is chosen and the client's messages may change |
| `barca history --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca stats --server` | server mode is new (RFC-0007): how the server is chosen and the client's messages may change |
| `barca stats --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca logs --server` | server mode is new (RFC-0007): how the server is chosen and the client's messages may change |
| `barca logs --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca sql --server` | new in 0.29.3: the views and barca's tables resolved from a server (GET /sql/views, GET /materializations) may change after field use; new in 0.44.0: a query naming only barca's tables is answered by the server (POST /sql/query), and which queries are sent there, its limits (5 seconds, 10000 rows and 4MB an answer, four queries at once) and its errors may change |
| `barca sql --local` | server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) |
| `barca logs --follow` | new in 0.29.2: how the end of a followed run is reported may change after field use |
<!-- END GENERATED experimental -->

What the command tables cannot express: JSON keys and shapes, HTTP routes, environment
variables, decorator arguments and output lines that are still experimental, written by hand.
Where a row below names a command, the generated list above is the owner of that command's
stability; the row adds the keys and routes that go with it.

| Item | Why |
|---|---|
| `barca doctor` and its JSON | prerequisite check names and provider-specific identity data may evolve; `project_version` checks the running CLI against the active `pyproject.toml` barca dependency; failed checks exit 3 with the report on stdout and the standard error envelope on stderr. With a server (`--server [<URL>]`, or a configured one without `--local`) it adds the `server`, `server_version`, `server_store`, `server_health`, `server_sources`, `server_scheduler` and `server_posts` checks (new in 0.29.3) |
| `barca plan` and its JSON | prints the planner's internal phase/stream layout, which changes with scheduling work (`reason` is an object, `{"type": "initial"}` or `{"type": "fan_in", "node_id": ...}`) |
| `barca serve` and all its flags | the HTTP API and scheduler are young: opt-in project tokens, routes may change. Its run outcomes (`GET /runs/{run_id}`, `run_finished`) and inspection routes (`/list`, `/nodes`, `/history`, `/stats/{name}`, `/assets`) are the CLI's `--json` bodies, built by the same functions, and its errors are this envelope. The older trigger routes (`POST /run`, `/run/{target}`, `/get/{target}`, `DELETE /run/{run_id}`, `GET /status/{run_id}`, `/events/{run_id}`) are deprecated aliases with no scheduled removal; a removal migrates the Python `Client` first and lands as a breaking change in a minor release |
| `get -o url`, `get --expires`, and `url` / `url_expires_at` in a `_barca_artifact` pointer | new (RFC-0011): signed HTTPS links to results in a cloud store. The URL's form is the cloud's own (SigV4, GCS V4, Azure SAS); `url_expires_at` is RFC 3339 UTC. `get -o url` with no cloud store, or an `--expires` over `7d`, is exit 2; a result missing from the store or credentials that cannot sign is exit 3 |
| `--server [<URL>]` and `--local` on `get`, `run`, `status`, `list`, `history`, `stats`, `logs`, `sql`, `queue`, `pause`, `resume`, `cancel`, `pin`, `unpin`, `gc`, `doctor`; `BARCA_SERVER` and `[server] url` | server mode is new (RFC-0007). A `--server` command prints what the local command prints, with the differences [Server Mode: a Shared Ledger and --server](/reference/server-mode/) lists (the server's paths, `triggered_by: api`, lines the Python workers print themselves stay on the server). Without `--server`, a configured server is the ledger of a local `get`/`run` (`POST /ledger/lookup`, `POST /ledger/runs`) and answers `status` (with `nodes[].local` and the `unsynced`/`not_in_ledger` state), `history`, `stats` and `logs`; `sql` resolves its views and barca's tables from it and runs the query here; `--local` ignores it. How the server is chosen and the client's own messages may change |
| The project socket: a `barca serve` running in the project is used by the commands typed there (`.barca/serve.sock`, `POST /gc`, `barca gc --server` / `--local`) | new in 0.46.0 (#1056). With a server running in the project and neither `--local` nor `--server`: `get` and `run` join its queue, `status`, `history`, `stats`, `logs`, `queue`, `pause`, `resume`, `pin`, `unpin` and `cancel` are answered by it, `sql` resolves from it, `gc` is run by it, and stderr says once `[barca] server .barca/serve.sock (this project's barca serve; --local runs on this machine)`. `--local` ignores it. No token is asked on the socket. `state restore` and `state reset` refuse while the server is up (exit 3). Which commands are routed, the socket's path, the announcement's wording, the 5-second answer limit and the `POST /gc` body may change after field use ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)) |
| `barca sql` with a server (`GET /sql/views`, `GET /materializations`) and the `barca` schema (`barca.runs`, `barca.materializations`, `barca.nodes`, `barca.schedules`, `barca.ticks`, `barca.queue`) | new in 0.29.3 (the last three in 0.44.1): the columns of barca's tables, the limit of rows read from a server and the wording of the notes may change after field use; the result JSON is `sql`'s |
| `barca sql` with a server, for a query that names only barca's tables (`POST /sql/query`) | new in 0.44.0: the server answers it instead of this machine. Which queries are sent to the server, its limits (5 seconds, 10000 rows and 4MB an answer, 100000 rows a table, four queries at once), the `notes` and the wording of its refusals may change; the result JSON is `sql`'s, a refusal is exit 2 with the standard envelope |
| `logs -f/--follow` | new in 0.29.2: the end-of-run line on stderr (`barca: run <run_id> ended: <status>`) and the choice of the run to follow without a run id may change after field use; the exit codes (0 succeeded, 130 cancelled, 1 otherwise) and the JSON Lines on stdout are stable |
| `status --problems`, `nodes[].schedule` and `filter` in `status --json` (and `GET /nodes?state=&problems=`) | new in 0.29.3: what counts as needing attention may grow (ordered by cause weight; work already in hand is omitted unless the cause is requested); `schedule.state` and `refresh_path.state` identify paused, held, late, overrunning or on-time schedules. `schedule.state_detail` explains the standing. These objects may gain keys. `--state` and the summary counting every node are stable |
| `status --sample` and `nodes[].shape` | read by a Python helper (`barca._inspect`) whose output may grow per format; the wording of `shape.note` (an unreadable, too large or unreachable artifact) may change |
| `BARCA_PROGRESS_SECS`, `BARCA_POOL_SIZE`, `BARCA_COMM_COST_SECONDS`, `BARCA_TRACE_TIMING` | tuning and benchmarking knobs |
| `BARCA_TELEMETRY` and the spans and metrics it sends | new: span names, resources, tags and metrics may change |
| `BARCA_SLACK_WEBHOOK_URL`, `BARCA_SLACK_NOTIFY`, `BARCA_UI_URL` and the Slack posts | new: the message text and layout are not a contract and may change in any release |
| `BARCA_LOG` and the JSON lines of `barca serve` | new: the fields and how a line's `level` is chosen may change |
| `barca queue` and its JSON, `barca serve --max-runs`, `[server] max_runs`, `BARCA_SERVER_RECONNECT` | new in 0.29.4: the run queue (`GET /queue`, `POST /queue/pause`, `POST /queue/resume`), its JSON and table, and how long and how often a `--server` client waits for a restarting server may change after field use |
| `[server] admission`, `[server] scheduled_max_wait_seconds`, and `capacity.admission`, `capacity.scheduled_max_wait_seconds` and `capacity.waiting[].origin` in `GET /queue` | new in 0.45.0 (#1003): which runs count as requested, the default bound and how the order is shown may change after field use; `admission = "first_come"` keeps the order of earlier releases |
| `control` of `GET /runs/{run_id}`, run-list and queue entries, and node `running` badges | experimental: cancellation authority of the answering server, with `cancellable`, descriptive `owner`, and nullable refusal `reason`. A CLI-owned or interrupted run cannot be cancelled by the server; write permissions are separate. |
| `queue` of `POST /runs` (`--queue next\|last` of `get` and `run`), and `queue` in the answer of `POST /runs`, on each run of `GET /queue` (`barca queue --json`) and in `GET /runs/{run_id}` | new in 0.46.0: `"last"` queues a requested run behind every waiting run and orders it with scheduled work ([Operating barca serve: Queue a run last](/reference/serve-ops/#queue-a-run-last---queue-last)); the values, the rule and where it shows may change after field use. `--queue` is a usage error (exit 2) on a command that runs on this machine |
| `undeclared_env`: the plan-warning kind, `steps[].undeclared_env` in `get`/`run` (and `--dry-run`) JSON, `nodes[].undeclared_env` in `status --json` | new in 0.29.1 (#574): which environment reads are found statically may grow |
| the `[barca] <n> more: ...` line | collapsing repeated library warnings is new; which output counts as a warning may change |
| `BARCA_ARTIFACT_URI` | 0.4.0 back-compat override, superseded by `BARCA_REMOTE_URI` / `[remote].artifacts_uri` |
| `barca state reset` and its JSON, `POST /state/reset` | new in 0.28 (#508): the JSON shape and the name of the moved-aside file may change after field use |
| `weight=` on `@asset`, `@task` and `@sensor`, `[execution] capacity`, `BARCA_CAPACITY`, `capacity` / `capacity_source` / `pool_size` / `weights` in `barca plan`, `capacity` and `steps[].weight` in `--dry-run`, the `capacity` doctor check | new in 0.29 (#436): how steps are admitted under the capacity, and whether a step waiting in `parallel()` counts, may change after field use |
| `cutoff=` on `@asset`, the dry-run and status reason `cutoff_pending` | new in 0.31.0 (#628): what is hashed (the serialized bytes), where a `--refresh` cascade stops and how a dry run predicts the consumers may change after field use |
| `barca gc` and its JSON, `[cache] max_size`, `BARCA_CACHE_MAX_SIZE`, the `cache` key of `get`/`run` results, the step and status reason `evicted`, the `cache_size` doctor check | new in 0.25 (RFC-0012): the low watermark, the eviction order and the JSON shapes may change after field use |
| dataset sinks: `@sink("prefix/")`, its `partition_by=` and `rows_per_file=`, the `_barca_sink.json` marker, the deletion of removed keys and the size warnings | new in 0.30.0 (#621): the marker's keys, the warning thresholds and how a key is swapped in on an object store may change after field use |
| `require_env` in `barca.toml`, `BARCA_REQUIRE_ENV` and the `[barca] note: no --env given, ...` line | new in 0.31 (#627): which commands count as touching an environment, and when the note is printed, may change after field use |
| `@asset(deterministic=False)` | new in 0.31 (#627): may be extended to other decorators |
| `max_rss_bytes` and `median_max_rss_bytes` in `barca stats`, `steps[].max_rss_bytes` and `steps[].output_tail` in `barca history RUN_ID` | new in 0.31 (#627): what the peak covers (the worker's baseline is included) and how many lines the tail keeps may change |
| `partitions.failed` and `partitions.failed_keys` in `status --json`, `steps_page.nodes` in `history RUN_ID --json`, `recent_runs[].keys` and `recent_runs[].failed_keys` in `stats --json` | new in 0.38.0: what counts as a failed key (its latest attempt failed, with no later success) may change after field use |
| `steps[].batch_execution` and `metadata_seconds` in `get`/`run` JSON, the `[barca] batch:<node> ...` and `[barca] metadata ...` lines | new in 0.38.0 (RFC-0013): which phases are reported and what each one covers may change after field use |
| `steps_page.nodes[].status` (`ran`, `cached`, `skipped`) and `steps_page.nodes[].blocked_by` in `history RUN_ID --json` with a server | the server lists every planned node of a finished run whose plan it can rebuild; the local `history RUN_ID` lists only nodes with recorded steps and never has these keys ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)) |
| `barca pin`, `barca unpin`, their JSON, the step reason `pinned`, `GET /pins`, `GET /assets/{name}/versions`, `POST` and `DELETE /assets/{name}/pin`, `barca pin --from-file` and `POST /assets/{name}/pin/upload` (a version's `upload`) | new in 0.41 ([barca pin and unpin](/reference/cli/pin/)): the version list, the JSON shapes and what a pin covers (only unpartitioned assets) may change after field use |
| `GET /assets/{name}/partitions/grid` and `GET /assets/{name}/partitions/key`, and the JSON of `barca status NODE --grid` and `--key KEY`, which print them | new in 0.42 (the commands in 0.44, #990): every key of a partitioned node in one body, and one key across its chain; the encoding of the per-key columns and which states are told apart may change after field use |
| `GET /assets/{name}/partitions/keys` and `GET /assets/{name}/partitions/overview`, and the JSON of `barca status NODE --keys` and `--overview`, which print them; `needs_run` in `GET /assets/{name}/partitions/key`; `causes` and `fresh` of each entry of `files` in `GET /state/rollup` and `barca status --rollup --json` | new in 0.44.1 (#1036): the keys of a partitioned node filtered, ordered and paged by the server, and its keys in one picture; the route's parameters, the page size (at most 1000 keys), the bound on a served `selection` (5,000 keys or values) and on the overview's rows (200,000 cells) may change after field use |
| `barca pause --schedule`, `barca pause --until`, `barca resume --schedule`, their JSON (the schedule's `GET /schedule` entry), `schedules` in `barca pause --json` and `GET /pauses`, and a duration as `?until=` of `POST /schedule/{id}/pause` | new in 0.44 (#989, [Scheduling With Cron](/scheduling/), "Pausing one schedule"): the entry's keys follow `GET /schedule` |
| `nodes[].last_attempt` and `nodes[].typical_run_seconds` in `status --json` (and `GET /state`, `GET /nodes`), `barca pause --dry-run` and `covers` in its JSON (`GET /pauses?target=`) | new in 0.46.0 (#1056, [barca status: Fields and JSON: Last attempt](/reference/cli/status-fields/#last-attempt) and "Typical run time"; [barca pause and resume: What a pause would cover](/reference/cli/pause/#what-a-pause-would-cover)): the words of `last_attempt` and of `covers.schedules[].relation` may grow, and what `typical_run_seconds` counts for a batched node may change after field use |
| Row counts: `last_materialization.rows` and `partitions.empty` in `status --json`, `steps[].rows` in `history RUN_ID --json`, `recent_runs[].rows` and `recent_runs[].empty_keys` in `stats --json` (and `--fields`), `rows` in `barca sql`'s `barca.materializations`, `last_rows` and `partitions_empty` in `barca.nodes` | new in 0.43 (#925, [barca status: Fields and JSON: Row counts](/reference/cli/status-fields/#row-counts)): which results are counted (frames, parquet footers) may widen after field use |
| `run.plan_reasons`, `run.retries`, `principal`, `trigger_reason` and `cancellation` in `barca history --json` (and `GET /runs`, `GET /runs/{run_id}`), `reason` of `POST /runs`, `?offset=` and `steps_page.next_offset` of `GET /runs/{run_id}`, `explanation` of `barca history RUN_ID --json` and `GET /runs/{run_id}` (#956), the `why:`, `target:`, `reason:` and `cancelled: ...` lines of `barca history RUN_ID` | new in #925: which reasons the engine tells apart, the bounds (64 reasons, 10,000 examined keys per node and phase) and the wording of the lines may change after field use |
| Health: `health`, `health_reason`, `cause`, `root`, `gap_seconds`, `signals`, `measured`, `last_check`, `refresh_path`, `stale_since`, `expected_fresh_at`, `recheck_at` and `no_estimate` of each node, and `data_coverage`, `as_of` and `rollup` beside `summary`, in `status --json` (and `--fields`, `GET /nodes`, `GET /state`), `typical_queue_wait_seconds`, `typical_run_seconds` and `queue_bound` of a node's `schedule` (and of `GET /schedule`), `started_at`, `estimated_start_at` and `wait_seconds` of a run in `barca queue --json`, the `health`, `health_reason`, `cause`, `root_id`, `root_kind`, `root_node`, `gap_seconds`, `signals`, `measured`, `last_check_at`, `last_check_outcome`, `refresh_schedule`, `next_tick`, `missed_ticks`, `stale_since`, `expected_fresh_at`, `recheck_at` and `no_estimate` columns of `barca.nodes`, the `Health:`, `Gap:` and `Data:` lines of the `barca status` table, the `checked 3m ago, up to date` it adds to the `WHY` of an `unknown` node and the `next tick 15:05 UTC by inbox`, `no refresh path`, `fresh ~15:05 UTC` or `overdue 16h` it adds to the `WHY` of a node that is not fresh | new in 0.44 (#954, #957, #961, #962, #963, #964, #968, #973, [barca status: Fields and JSON: Health](/reference/cli/status-fields/#health), "Cause", "Root", "Last check", "Refresh path" and "Times"): how an estimate is computed and rounded, which fact a root names and the order of the cause rules may change after field use; `schedule_late` takes its final definition of late from #958, and when a tick's check decides health (`checked_up_to_date`) may change after field use |
| The rollup: the JSON of `barca status --rollup` and `GET /state/rollup` (`as_of`, `entries`, `roots`, `data_coverage`, `files`), the table and the ranked list `barca status --rollup` prints, and `root` on each status node | new in 0.44 (#955, #962, #963, #967, #973, #1028, [barca status: Fields and JSON: The rollup](/reference/cli/status-fields/#the-rollup)): an entry has `causes`, `asset_causes`, `not_fresh`, `weight`, `weights`, `held_by`, `gap_seconds`, `unbounded` and `fresh_by`, and `keys.cause` on a partitioned node; the document and each file have `kinds` (#992); a root has `causes`, `gap_seconds`, `unbounded`, `weight` and `action`. The project-wide `scheduler_off` root has an empty `node` and ranks first. Other roots are ranked by `weight` (#1028). The wording of `detail`, which command an action names, the weights and how roots are ranked may change after field use |
| `line` of each run in `barca history --json` and `GET /runs` (`steps`, `steps_detail`, `failure`, `failure_detail`), and `now` of `GET /runs?status=running` | new in 0.44 (#959, [barca status](/reference/cli/status/)): the words a run list prints, computed once for the CLI and the web UI; their wording may change after field use |
| `now` of each run in progress in `barca history --status running --json` (and `GET /history?status=running`; `running`, `queued`, `keys`, `groups`, `line`), the `now:` line under its row, and the output of `barca history --activity` (`activity`: `since`, `until`, `triggered_by`, `hours`, `total`, `summary`; also `activity` and `?triggered_by=` of `GET /runs`) | new in 0.44 (#991, [barca status: Runs in progress and the last day's activity](/reference/cli/status/#runs-in-progress-and-the-last-days-activity)): what the web UI's Runs page shows, computed once for the CLI and the UI; the keys, the window and the wording may change after field use |
| `--agent` lines other than `step:`, the end-of-run line and `run failed:` | progress notes (`still running`, skipped tasks, the text of warnings, dataset sink deletions) whose wording may change |

HTTP-only keys: `failing_since`, `consecutive_failures`, and (experimental, new in 0.43, #925) `prev_run` and `late_since` (rows of `GET /state`, [Server Mode: a Shared Ledger and --server: Data health](/reference/server-mode/#data-health); `signals` moved from here onto every status node in 0.44), and `steps_failed` and `first_failure` (entries of `GET /runs`) are in the server's HTTP responses only, specified in `specs/interfaces/server-api.openapi.yaml`; no CLI command prints them.

### Accepted exceptions

- `barca docs` prints markdown by default, even when piped, and has `--json` but no `--pretty`:
  its output is a manual meant to be read or pasted into a model's context, so the terminal rule
  does not apply. `--json` and `--fields` give the JSON index or topic.

### Deprecated compat flags

These older spellings keep working and are not experimental: their behavior does not change.
Each is removed only in a minor release, and only at least one minor release after barca starts
printing a stderr notice naming its replacement; no notice is printed yet, so none is scheduled
for removal.

- `get -o json` and `get -o pretty`: the older spellings of `--json` and `--pretty`.
  `get -o path`, `get -o value` and `get -o url` are not deprecated.
- `run -o json` and `run -o pretty` (`run --output`): the older spellings of `--json` and
  `--pretty`. `run -o value` has no other spelling yet and keeps working.

### Experimental surface: what graduates when

Each whole-command experimental entry, decided for 0.40.0 (#800, item 11). Graduation is
proposed only where the output contract has not changed for at least three minor releases;
nothing has graduated yet. An experimental flag or key inside a graduating command keeps its
own row above.

- `barca sql`: graduates in 0.40.0. Shipped since 0.13.0; the `sql.txt` JSON snapshot is
  unchanged since 0.13.0 and the help since 0.35.2. `sql --server` and the `barca` schema stay
  experimental.
- `barca history <RUN_ID>`: graduates in 0.40.0. Shipped since 0.29.3; the `history_run.txt`
  JSON snapshot is unchanged since 0.34.0. `steps_page.nodes`, `max_rss_bytes` and
  `output_tail` stay experimental.
- `barca doctor`: graduates in 0.40.0. Shipped since 0.24.0; the `doctor.txt` JSON snapshot last
  changed additively in 0.36.0 (`budget_bytes`, `backup_bytes`) and not at all in 0.37 to 0.39.
  New checks stay additive; the server checks stay experimental with `--server`.
- `barca state reset`: graduates in 0.40.0. Shipped since 0.28.0; its JSON
  `{database, moved_to, moved, undo}` is unchanged since 0.28.0 and pinned by
  `python/tests/test_state_reset.py`. `POST /state/reset` stays with `serve`.
- `barca plan`: stays experimental. Its JSON (unchanged since 0.31.0) is the planner's internal
  phase and stream layout, which the bounded-scheduling work in #655 is expected to change.
- `barca gc`: stays experimental. Its JSON changed in 0.36.0, and the eviction order is
  size-based where `specs/behavior/operations.md` approves a three-version retention policy;
  a killed worker's `.barca/.batch-*` directory and a killed run's `live-logs/` file count
  toward the budget but are never cleaned up.
- `barca serve`: stays experimental. Its HTTP API changed in every minor from 0.36.0 to 0.39.0,
  shared in-flight steps have three open gaps (#654), and bounded cold planning and inspection
  are unmet (#655, #656).
- `barca queue`: stays experimental. It reads `serve`'s run queue, which stays experimental,
  and its JSON has no schema snapshot.
- `barca pin` and `barca unpin`: stay experimental. New in 0.41.
- `barca tokens` and `barca tokens bootstrap`: stay experimental. New in 0.36.0 (RFC-0016); the
  token format may evolve, and the cost of token access in a cloud deployment is not
  established.

## Commands

`barca <command> --help` (or `-h`) prints concise flags, common examples and manual pointers.
`barca <command> --examples` prints the full runnable example collection. Both exit 0 on
stdout without loading a project or executing work; they also work on nested commands such
as `barca state restore --examples`. Detailed semantics live in `barca docs <topic>`. Shorthand:
`barca file.py [flags]` is `barca get file.py [flags]` (stable).

<!-- BEGIN GENERATED commands -->
| Command | Arguments | Stability | Purpose |
|---|---|---|---|
| `barca tokens` | - | experimental: new in 0.36 (RFC-0016): local bootstrap for opt-in project token access | Manage project authentication on the deployed host |
| `barca tokens bootstrap` | - | experimental: prints a one-use bootstrap credential; token formats may evolve | Print a one-use 15-minute bootstrap code; replaces any unconsumed grant |
| `barca get` | `[<ARGS>...]` | stable | Get assets; reuse unchanged results and print their artifact paths |
| `barca run` | `<ARGS>...` | stable | Run a task and its dependencies; the task runs every time |
| `barca plan` | `[<FILES>...]` | experimental: prints the planner's internal phase/stream layout, which changes with scheduling work | Parse source files and emit the execution plan as JSON |
| `barca doctor` | `[<FILES>...]` | experimental: new prerequisite report; check names and provider-specific data may evolve | Diagnose project prerequisites without running pipeline steps |
| `barca logs` | `[<RUN_ID>]` | stable | Read a run's recorded step output (stdout and stderr), without progress messages |
| `barca history` | `[<RUN_ID>]` | stable | Show recent run history, or one run's steps and errors |
| `barca gc` | - | experimental: new in 0.25 (RFC-0012): the eviction order, the low watermark and the JSON shape may change after field use | Evict cached results to keep .barca/ under its size limit |
| `barca stats` | `<TARGET> [<FILES>...]` | stable | Show execution statistics for an asset |
| `barca pin` | `[<TARGET>] [<FILES>...]` | experimental: new in 0.41 (barca docs pins): the JSON shapes, the version list and what a pin covers may change after field use | Pin an asset to an earlier materialization, or list pins and versions |
| `barca unpin` | `<TARGET> [<FILES>...]` | experimental: new in 0.41 (barca docs pins): the JSON shape may change after field use | Remove an asset's pin; the next run decides it as before |
| `barca serve` | `[<FILES>...]` | experimental: the HTTP API and scheduler are young: opt-in project tokens, routes may change; the older trigger routes remain deprecated aliases for Python Client compatibility, with no scheduled removal; a removal migrates Client first and lands in a minor release; runs share identical in-flight asset steps (new in 0.39.4, #654), with cancellation survival, a durable handoff and a decide-to-lease race still open | Manage runs, schedules and results on a server |
| `barca cancel` | `<RUN_ID>` | experimental: Cancel a queued or running server run; the response follows DELETE /runs/{id} | Cancel a queued or running server run |
| `barca queue` | `[<ACTION>]` | experimental: new in 0.29.4: the run queue's JSON shape, its table and the pause's scope may change after field use | Show a barca server's run queue, or pause or resume it (experimental) |
| `barca pause` | `[<TARGET>]` | experimental: new in 0.41: pausing assets for scheduled runs; the JSON shape, the table and what a pause covers may change after field use | Pause assets on a barca server: scheduled runs skip them until resumed (experimental) |
| `barca resume` | `[<TARGET>]` | experimental: new in 0.41: resuming paused assets; the JSON shape and the table may change after field use | Resume paused assets on a barca server (experimental) |
| `barca list` | `[<FILES>...]` | stable | List all discovered definitions (assets, tasks, sensors) with their deps |
| `barca status` | `[<ARGS>...]` | stable | Show every node's cache state, last materialization and artifact shape (read-only) |
| `barca sql` | `<QUERY> [<FILES>...]` | experimental: new in 0.13: the view naming and the JSON result shape may change after field use | Query cached results with SQL (DuckDB) — each asset is a named view |
| `barca state` | - | stable | Metadata backups: list them, put one back in place of the local database, or start over |
| `barca state backups` | - | stable | List the metadata backups, local and in the store, newest first |
| `barca state restore` | - | stable | Put a metadata backup in place of the local database (the current one is kept aside) |
| `barca state reset` | - | experimental: new in 0.28 (#508): the JSON shape and the name of the moved-aside file may change after field use | Start the metadata database over: move it aside and create an empty one (experimental) |
| `barca docs` | `[<TOPIC>]` | stable | Show the built-in manual: concepts, output formats, examples, agent conventions |
| `barca version` | - | stable | Print version information |
| `barca help` | - | stable | Print this message or the help of the given subcommand(s) |
<!-- END GENERATED commands -->

Positional rules (stable), for `get`, `run` and `status`:

- The target comes before the files. If the first positional ends in `.py`, every positional is
  a file and there is no target (`run` then exits 2: it needs one). Otherwise the first
  positional is the target and the rest are files.
- A target is one name or several, comma-separated without spaces (`a,b`), on all three. A name
  is the function name or the full id `file.py:name`, and selects exactly that node (`deploy`
  never selects `prod_deploy`). A function name defined in more than one file, or an empty name
  (`a,,b`), is a usage error.
- An unknown target is a usage error (exit 2) on every command, with one remediation:
  ``Run `barca list <files>` to see available assets and tasks.``, preceded by a suggestion when
  a node name is close (below). So is a name that several nodes have, and `barca run` without a
  target. No other usage error carries that remediation: each states its own fix (the corrected
  command for positionals in the wrong order, the valid names for an unknown `--refresh` name,
  what to set for `-o url` without a cloud store, what to remove for a removed setting) or, with
  nothing more to say, `See barca <command> --help`.
- A mistyped name gets one "did you mean" rule: a flag, a subcommand, a target, a `--refresh`
  name, a `barca sql` view, a `barca docs` topic and a decorator argument (`input=` for
  `inputs=`). barca suggests the closest names within one edit of what was written (two for a
  name longer than four characters): the best match only, or two when they tie, and nothing
  when none is that close or more than two tie. It reads `Did you mean 'amounts'?` (or
  `Did you mean 'stats' or 'status'?`) and starts the envelope's `remediation`, or follows the
  error line in human mode; `error` is unchanged. A suggestion never changes the exit code (a
  usage error stays 2) and barca never acts on one. clap's own tips are not used: a mistyped
  flag gets no "to pass '--x' as a value, use '-- --x'" tip.
- `--refresh` and `--fields` take one comma-separated list (`--refresh a,b`).

## Arguments

Every command also takes `-h, --help`. `Notes` lists whether an argument is required, its
default, and any aliases.

<!-- BEGIN GENERATED flags -->
#### barca

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--examples` | - | - | stable | Show all command examples without running anything |
| `-V, --version` | - | - | stable | Print version |

#### barca tokens

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca tokens bootstrap

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca get

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--cached-only` | `store\|local` | - | experimental: new in 0.47: completed-result reads and per-key outcome shapes may evolve | Read completed results without execution; local excludes every store call |
| `<ARGS>...` | - | - | stable | Target(s), then optional files/directories; omit files to discover the project |
| `-o, --output` | `path\|value\|url\|json\|pretty` | - | stable | Choose the result output mode (see possible values) |
| `--expires` | `DURATION` | - | experimental: new with -o url (signed links to results in a cloud store); the link format is the cloud's | Signed URL lifetime: 30m, 12h, 7d or seconds (default: 1h; maximum: 7d) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--partition` | `KEY=VALUE[,VALUE]\|JSON` | - | stable | Select keys: dimension values or JSON exact-key list; collect() fan-ins are skipped |
| `--batch` | `[NODE=]none\|all\|N` | - | experimental: new in 0.32.0 (RFC-0013): batched partitions; the override's forms and the dry run's batch object may change after field use | Override declared batching: none, all, N or NODE=N (experimental) |
| `--refresh` | comma-separated names | - | stable | Recompute assets or selected keys and their dependents: --refresh a,b |
| `--no-cascade` | - | default `false` | stable | Refresh only named assets; cached downstream results may be outdated |
| `--refresh-all` | - | default `false` | stable | Recompute every asset needed by the target |
| `--dry-run` | - | default `false` | stable | Preview cached and planned work; execute nothing and record no run |
| `--agent` | - | default `false` | stable | Print plain structured progress on stderr |
| `--fields` | comma-separated: `id`, `kind`, `action`, `status`, `reason`, `detail`, `changed`, `run_hash`, `artifact`, `warning`, `artifact_mismatch`, `bytes_changed`, `elapsed_seconds`, `attempts`, `import_seconds`, `sink_seconds`, `partitions`, `env`, `undeclared_env`, `weight`, `batch`, `batch_execution` | - | stable | Select JSON fields as a comma-separated list; implies --json |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--allow-paused` | - | default `false` | experimental: new in 0.41: runs a paused target by hand; which requests need it may change after field use | Run a target even though it is paused (experimental) |
| `--queue` | `next\|last` | - | experimental: new in 0.46.0: a place in a server's run queue; the values, what `last` is ordered with and where it shows may change after field use | On a server, join its run queue `next` (default) or `last`, behind scheduled runs (experimental) |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca run

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ARGS>...` | - | required | stable | Target(s), then optional files/directories; omit files to discover the project |
| `--refresh` | comma-separated names | - | stable | Recompute assets or selected keys and their dependents: --refresh a,b |
| `--no-cascade` | - | default `false` | stable | Refresh only named assets; cached downstream results may be outdated |
| `--refresh-all` | - | default `false` | stable | Recompute every asset needed by the target |
| `--partition` | `KEY=VALUE[,VALUE]\|JSON` | - | stable | Select keys: dimension values or JSON exact-key list; collect() fan-ins are skipped |
| `--batch` | `[NODE=]none\|all\|N` | - | experimental: new in 0.32.0 (RFC-0013): batched partitions; the override's forms may change after field use | Override declared batching: none, all, N or NODE=N (experimental) |
| `--dry-run` | - | default `false` | stable | Preview cached and planned work; execute nothing and record no run |
| `-o, --output` | `json\|value\|pretty` | - | deprecated: -o json\|pretty are the older spellings of --json / --pretty; -o value has no replacement yet and keeps working | Choose the result output mode (see possible values) |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--agent` | - | default `false` | stable | Print plain structured progress on stderr |
| `--fields` | comma-separated: `id`, `kind`, `action`, `status`, `reason`, `detail`, `changed`, `run_hash`, `artifact`, `warning`, `artifact_mismatch`, `bytes_changed`, `elapsed_seconds`, `attempts`, `import_seconds`, `sink_seconds`, `partitions`, `env`, `undeclared_env`, `weight`, `batch`, `batch_execution` | - | stable | Select JSON fields as a comma-separated list; implies --json |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--allow-paused` | - | default `false` | experimental: new in 0.41: runs a paused target by hand; which requests need it may change after field use | Run a target even though it is paused (experimental) |
| `--queue` | `next\|last` | - | experimental: new in 0.46.0: a place in a server's run queue; the values, what `last` is ordered with and where it shows may change after field use | On a server, join its run queue `next` (default) or `last`, behind scheduled runs (experimental) |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca plan

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories; default: discover the project |
| `--partition` | `KEY=VALUE[,VALUE]\|JSON` | - | experimental (with the command) | Select keys: dimension values or JSON exact-key list; collect() fan-ins are skipped |
| `--json` | - | default `false` | experimental (with the command) | Accepted for symmetry with the other commands: the plan is always pretty-printed JSON, so this changes nothing |
| `--pretty` | - | default `false` | experimental (with the command) | Accepted for symmetry with the other commands: the plan is always pretty-printed JSON, so this changes nothing (there is no table form) |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca doctor

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories; default: discover the project |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Use independent local state; ignore the configured server |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca logs

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<RUN_ID>` | - | - | stable | Exact run id from get/run JSON or history (default: the most recent run; with --follow, the newest running run) |
| `--step` | `STEP` | - | stable | Function name or exact node id (including a partition key when desired) |
| `--stream` | `stdout\|stderr` | - | stable | Only the lines of this stream (default: both) |
| `-f, --follow` | - | default `false` | experimental: new in 0.29.2: how the end of a followed run is reported may change after field use | Print a running run's lines as they arrive and exit when it ends (exit 0 when it succeeded, 130 when it was cancelled, 1 otherwise). With --json, one JSON object per line |
| `--timestamps` | - | default `false` | stable | Text output: put each line's arrival time (UTC, milliseconds) before it. JSON always has it as `ts` |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca history

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<RUN_ID>` | - | - | experimental: new in 0.29.3: the run detail's step keys and human layout may change after field use | One run in detail: its fields, the steps it recorded (the first 1000, failed steps always; --limit and --all change it), and each failed step's error, traceback and how its worker ended |
| `--reconcile` | `RUN_ID` | - | experimental: explicit operator recovery for stopped runs whose owner cannot be verified | After verifying a run stopped, record it as interrupted; refuses a proven live owner |
| `--status` | `STATUS[,...]` | - | experimental: new in 0.29.3: the history filters' values and matching may change after field use | Only runs with one of these statuses (comma-separated): pending, running, success, skipped, failed, cancelled, interrupted; not_successful means failed, cancelled or interrupted |
| `--target` | `NAME` | - | experimental: new in 0.29.3: the history filters' values and matching may change after field use | Only runs whose target or job is this node (a function name or node id); runs of every node (`all`) are left out |
| `--triggered-by` | `TRIGGER[,...]` | - | experimental: new in 0.29.3: the history filters' values and matching may change after field use | Only runs started this way (comma-separated): manual, api, schedule, client |
| `--not-triggered-by` | `T[,...]` | - | experimental: new in 0.44 (#969): the history filters' values and matching may change after field use | Without the runs started this way (comma-separated): schedule hides the cron ticks and keeps the runs that recorded no trigger |
| `--search` | `TEXT` | - | experimental: new in 0.44 (#969): what the text is looked for in, and how, may change after field use | Only runs whose run id, target or job contains this text, whatever its case |
| `--activity` | - | default `false` | experimental: new in 0.44 (#991): the activity's keys, its window and its human table may change after field use | Instead of the runs: how many started in each of the last 24 hours (UTC), by status and by what started them, with each hour's median time and leading failure, counted over every recorded run; takes --status, --triggered-by, --not-triggered-by, --search |
| `--since` | `WHEN` | - | experimental: new in 0.29.3: the history filters' values and matching may change after field use | Only runs started at or after this: a duration back from now (30m, 24h, 7d, 2w), an RFC 3339 time or a UTC date (2026-10-22) |
| `--until` | `WHEN` | - | experimental: new in 0.44 (#969): the history filters' values and matching may change after field use | Only runs started before this, written as --since is: with it, a window |
| `-l, --limit` | `LIMIT` | - | stable | Maximum recent runs (default: every run in JSON, 10 in a human table); with a RUN_ID, the most steps listed (default 1000) |
| `--all` | - | default `false` | stable | Show every recorded run (no limit); with a RUN_ID, every step it recorded |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--fields` | comma-separated: `run_id`, `command`, `files`, `target`, `job`, `status`, `steps_total`, `steps_executed`, `steps_cached`, `attempts`, `started_at`, `finished_at`, `elapsed_seconds`, `triggered_by`, `scheduled_at`, `schedule_id`, `client_run_id`, `failure`, `failed_node`, `error`, `principal`, `trigger_reason`, `cancellation`, `line`, `now` | - | stable | Select JSON fields as a comma-separated list; implies --json |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca gc

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--max-size` | `SIZE` | - | experimental (with the command) | A limit for this run in place of the configured one: 20GB, 500MB, 20GiB, bytes, 10% (of the filesystem) or off (only leftovers and superseded results) |
| `--dry-run` | - | default `false` | experimental (with the command) | Preview cached and planned work; execute nothing and record no run |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca stats

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TARGET>` | - | required | stable | Target asset function name |
| `<FILES>...` | - | - | stable | Python files or directories; default: discover the project |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--fields` | comma-separated: `elapsed_seconds`, `status`, `created_at`, `error_message`, `attempts`, `run_id`, `cpu_seconds`, `max_rss_bytes`, `rows`, `empty_keys`, `keys`, `failed_keys` | - | stable | Select JSON fields as a comma-separated list; implies --json |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca pin

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TARGET>` | - | - | experimental (with the command) | The asset or complete key (e.g. orders[region=emea]); omit to list every pin |
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories; default: discover the project |
| `--run-hash` | `HASH` | - | experimental (with the command) | Pin the successful materialization with this run hash (or a unique prefix of at least 6 characters), from `--versions` |
| `--previous` | - | default `false` | experimental (with the command) | Pin the newest successful materialization before the latest one (roll back one version) |
| `--from-file` | `PATH` | - | experimental (with the command) | Pin it to data you supply: a .json, .parquet, .csv or .pkl file, checked by loading it and recorded as an uploaded (override) materialization |
| `--versions` | - | default `false` | experimental (with the command) | List the asset's recent successful materializations (one per run hash) instead of pinning |
| `-l, --limit` | `LIMIT` | - | experimental (with the command) | With --versions, the most listed (default 20) |
| `--reason` | `TEXT` | - | experimental (with the command) | Why it is pinned, recorded with the pin and shown in the UI |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca unpin

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TARGET>` | - | required | experimental (with the command) | The pinned asset or complete key (e.g. orders[region=emea]) |
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories; default: discover the project |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca serve

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories; default: discover the project |
| `--auth` | `AUTH` | - | experimental (with the command) | Require project bearer tokens; overrides BARCA_SERVER_AUTH and [server] auth |
| `-p, --port` | `PORT` | default `8274` | experimental (with the command) | Port to bind on |
| `--host` | `HOST` | default `127.0.0.1` | experimental (with the command) | Bind address; 0.0.0.0 exposes the unauthenticated API |
| `--watch` | - | default `false` | experimental (with the command) | Reload changed source files; restart to discover new files |
| `--no-schedule` | - | default `false` | experimental (with the command) | Disable scheduled runs |
| `--timezone` | `TIMEZONE` | default `local` | experimental (with the command) | Cron timezone: local, utc or an IANA name such as America/New_York |
| `--read-only` | - | default `false` | experimental (with the command) | Inspect only: refuse runs, never schedule, read compatible WAL metadata in place |
| `--drain-timeout` | `SECONDS` | - | experimental (with the command) | Shutdown grace period in seconds; unfinished runs requeue (default: config or 30) |
| `--max-runs` | `N` | - | experimental (with the command) | Maximum concurrent runs; others queue (default: config or CPU count) |
| `--capacity` | `N` | - | experimental: new in 0.31.0 (#628): the admission order across runs and the shape of capacity in /queue and /health may change after field use | Limit total running step weight across server runs (experimental) |
| `--run-timeout` | `SECONDS` | - | experimental (with the command) | Run time limit in seconds; 0 disables it (default: config or unlimited) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca cancel

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<RUN_ID>` | - | required | experimental (with the command) | Run id returned by the server |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Check the server's environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca queue

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ACTION>` | - | - | experimental (with the command) | pause: queued runs wait until resumed (runs in flight go on); resume: they start again. Omit to show the queue |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca pause

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TARGET>` | - | - | experimental (with the command) | Node or file.py to pause; omit to list paused nodes and schedules |
| `--downstream` | - | default `false` | experimental (with the command) | Also pause every node downstream of the target |
| `--dry-run` | - | default `false` | experimental (with the command) | Pause nothing; print what the pause would cover |
| `--schedule` | `NODE` | - | experimental (with the command) | Pause the schedule of this scheduled node instead of nodes |
| `--until` | `WHEN` | - | experimental (with the command) | With --schedule: pause until a duration from now (2h) or an RFC 3339 time |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca resume

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TARGET>` | - | - | experimental (with the command) | Node or file.py to resume |
| `--schedule` | `NODE` | - | experimental (with the command) | Resume the schedule of this scheduled node instead of nodes |
| `--downstream` | - | default `false` | experimental (with the command) | Also resume the nodes below the target that its pause holds |
| `--all` | - | default `false` | experimental (with the command) | Resume every paused node |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental (with the command) | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental (with the command) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca list

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<FILES>...` | - | - | stable | Python files or directories; default: discover the project |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--groups` | - | default `false` | experimental: Organizational group hierarchy and metadata shape are experimental | Show the organizational hierarchy (groups are never executable nodes) |
| `-l, --limit` | `LIMIT` | - | stable | Maximum nodes, in topological order (default: every node in JSON, 100 in a human table) |
| `--all` | - | default `false` | stable | Show every node (no limit) |
| `--fields` | comma-separated: `id`, `kind`, `freshness`, `schedule`, `inputs`, `env`, `next_fire`, `file`, `line` | - | stable | Select JSON fields as a comma-separated list; implies --json |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca status

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<ARGS>...` | - | - | stable | Target(s), then optional files/directories; omit files to discover the project |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `-l, --limit` | `LIMIT` | - | stable | Maximum nodes, in topological order (default: every node in JSON, 100 in a human table) |
| `--all` | - | default `false` | stable | Show every node (no limit) |
| `--fields` | comma-separated: `id`, `name`, `kind`, `inputs`, `partitioned`, `cache`, `health`, `health_reason`, `signals`, `measured`, `cause`, `root`, `gap_seconds`, `last_check`, `refresh_path`, `stale_since`, `expected_fresh_at`, `recheck_at`, `no_estimate`, `last_attempt`, `typical_run_seconds`, `partitions`, `last_materialization`, `last_bytes_change`, `shape`, `env`, `undeclared_env`, `local`, `schedule` | - | stable | Select JSON fields as a comma-separated list; implies --json |
| `--sample` | `N` | - | experimental: sample rows come from a Python helper whose output may grow | Include up to N sample rows from each json/parquet/CSV artifact (off by default; pickles are never sampled) |
| `--state` | comma-separated names | - | stable | Show only nodes in these cache states (comma-separated): cached, unsynced, stale, never_run, partial, blocked, unknown, always_runs. The summary still counts every node. With --keys: only keys in these key states: failed, blocked, stale, never_run, pinned, fresh |
| `--cause` | `failing\|overdue\|no_refresh_path\|queued\|undecided\|catching_up` | - | experimental: experimental health inspection (#1043) | Keep nodes with this cause |
| `--health` | `failing\|blocked\|suspect\|behind\|undecided\|never_run\|ok` | - | experimental: experimental health inspection (#1043) | Keep nodes with this health |
| `--root` | `ROOT` | - | experimental: experimental health inspection (#1043) | Keep nodes with this root id |
| `--problems` | - | default `false` | experimental: orders problems by distance from fresh; work already in hand is omitted unless its cause is requested | List problems by distance from fresh; omit work already in hand unless selected by cause |
| `--rollup` | - | default `false` | experimental: new in 0.44.0 (specs/behavior/health.md): counts by health and by cause, the freshness gap and the ranked roots with their actions; the wording of a root's detail and of the printed list may change after field use | Experimental: print the rollup instead of the nodes: counts by cause per group and file, in nodes and in assets, then what holds the nodes that are not fresh (the ranked roots) and the action that clears each |
| `--now` | - | default `false` | experimental: experimental health inspection (#1043) | Evaluate the rollup at the current second (server polls default to the minute) |
| `--keys` | - | default `false` | experimental: new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use | Experimental: list the keys of one partitioned target, each with its state |
| `--needs-run` | - | default `false` | experimental: new in 0.44.1 (#1036), with --keys: the keys a run computes; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use | Experimental, with --keys: only keys a run computes |
| `--value` | `DIM=VALUE[,VALUE]` | - | experimental: new in 0.44.1 (#1036), with --keys: a dimension's values; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use | Experimental, with --keys: only keys with these values of a dimension |
| `--range` | `DIM=FROM..TO` | - | experimental: new in 0.44.1 (#1036), with --keys: a stretch of a dimension; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use | Experimental, with --keys: only keys in a stretch of a dimension |
| `--error` | `N` | - | experimental: new in 0.44.1 (#1036), with --keys: one error group's failed keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use | Experimental, with --keys: only the failed keys of error group N |
| `--match` | `TEXT` | - | experimental: new in 0.44.1 (#1036), with --keys: text over the key; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use | Experimental, with --keys: only keys that contain TEXT |
| `--sort` | `key\|state\|attempted\|elapsed\|size\|rows` | - | experimental: new in 0.44.1 (#1036), with --keys: the order of the keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use | Experimental, with --keys: order by key, state, attempted, elapsed, size or rows |
| `--offset` | `N` | - | experimental: new in 0.44.1 (#1036), with --keys: the page of keys; the filters, the orders and the JSON's `offset`, `needs_run` and `selection` may change after field use | Experimental, with --keys: skip the first N matching keys |
| `--overview` | - | default `false` | experimental: new in 0.44.1 (#1036): a partitioned node's keys in one picture (counts, issues and the grid's rows); the JSON shape, the wording of an issue and the bound on the rows may change after field use | Experimental: print one partitioned target's keys in one picture: counts, issues, rows |
| `--grid` | - | default `false` | experimental: new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use | Experimental: print one partitioned target's keys as a grid of states |
| `--key` | `KEY` | - | experimental: new in 0.44.0 (#990): a partitioned node's keys; the JSON shape, the table, the key states told apart and the grid's per-key columns may change after field use | Experimental: show one key (day=2026-01-01,region=apac) of one partitioned target |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental: server mode is new (RFC-0007): how the server is chosen and the client's messages may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca sql

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<QUERY>` | - | required | experimental (with the command) | The SQL query (DuckDB dialect); quote it as one argument |
| `<FILES>...` | - | - | experimental (with the command) | Python files or directories; default: discover the project |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `-l, --limit` | `LIMIT` | default `100` | experimental (with the command) | Maximum number of rows to return |
| `--all` | - | default `false` | experimental (with the command) | Return every row (no limit) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--server` | `URL` | - | experimental: new in 0.29.3: the views and barca's tables resolved from a server (GET /sql/views, GET /materializations) may change after field use; new in 0.44.0: a query naming only barca's tables is answered by the server (POST /sql/query), and which queries are sent there, its limits (5 seconds, 10000 rows and 4MB an answer, four queries at once) and its errors may change | Use a server (URL or configured); get/run execute its deployed code |
| `--local` | - | default `false` | experimental: server mode is new (RFC-0007): ignores the configured server (its ledger, the answers of status, history, stats and logs, and the views of sql) and, new in 0.46.0, a barca serve running in this project (its socket, .barca/serve.sock) | Run on this machine; ignore a configured server and this project's barca serve |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca state

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca state backups

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca state restore

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--from` | `N\|NAME\|PATH` | - | stable | Backup index (1 = newest), name or database path; default: newest backup |
| `--json` | - | default `false` | stable | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | stable | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | stable | Use a separate cache and history for this environment |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca state reset

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `-y, --yes` | - | default `false` | experimental (with the command) | Do not ask: required when stdin is not a terminal |
| `--json` | - | default `false` | experimental (with the command) | Emit JSON on stdout (the default when stdout is not a terminal) |
| `--pretty` | - | default `false` | experimental (with the command) | Emit human-readable output (the default when stdout is a terminal) |
| `--env` | `ENV` | - | experimental (with the command) | Use a separate cache and history for this environment |
| `--examples` | - | - | experimental (with the command) | Show all command examples without running anything |

#### barca docs

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `<TOPIC>` | - | - | stable | Topic to show (omit for the index), e.g. types, cache, examples/duckdb |
| `--all` | - | default `false` | stable | Print every topic in one stream |
| `--json` | - | default `false` | stable | Emit JSON instead of markdown |
| `--pretty` | - | default `false` | stable | Print markdown, the default even when piped (accepted like every read command's --pretty) |
| `--fields` | comma-separated: `name`, `summary`, `content` | - | stable | Select JSON fields as a comma-separated list; implies --json |
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca version

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| `--examples` | - | - | stable | Show all command examples without running anything |

#### barca help

| Argument | Value | Notes | Stability | Description |
|---|---|---|---|---|
| - | - | - | - | no arguments besides `-h, --help` |
<!-- END GENERATED flags -->

## Output selection (stable)

`get`, `run`, `logs`, `list`, `status`, `history` and `stats` choose JSON or human output on stdout by
one rule, first match wins:

1. a flag: `--json` or `--pretty` (on `get`/`run` also `-o json|pretty`, which cannot be
   combined with them); `--fields` implies JSON and is a usage error with `--pretty` or
   `-o pretty` (on `run` also with `-o value`);
2. `BARCA_OUTPUT=json|pretty`;
3. the terminal: stdout is a TTY, human output; anything else, JSON.

`plan` always prints JSON. `docs` prints markdown unless `--json` or `--fields` is given. Errors
follow the same rule: in JSON mode they are the envelope on stderr (see Errors).

### What `get` returns (stable)

`barca get` returns artifact paths: one per target, one per partition key. `-o path` (the
default) and `-o value` choose what is returned. Alone, `-o path` prints only the paths, one per
line, and `-o value` only the value, pretty-printed; with `--json` (or `--fields`) they choose
what `final_output` holds, and with `--pretty` they add the human summary. Without `-o`, the
rule above chooses between the JSON result and the summary, and paths are returned. In a
terminal the summary previews a small json value (at most 10 lines) under its path; the preview
is human output, not contract. `run` keeps `-o value`, the task's value, and prints a task's
value in `final_output`. From 0.32.0, a partitioned task returns an object of partition key
to value (tables as JSON rows), including a single selected key; unpartitioned task output
is unchanged. The bounded partition summary below applies to tasks too.

| Invocation | stdout |
|---|---|
| `get` (piped), `get --json`, `get -o path --json` | the JSON result, `final_output` a pointer `{"_barca_artifact": {"path", "format", "size_bytes"}}` for every format |
| `get -o value --json` | the JSON result, `final_output` the value |
| `get -o path` | the paths, one per line |
| `get -o value` | the value, pretty-printed |

A partitioned target returns every key, or the keys `--partition` selects
(`DIMENSION=VALUE[,VALUE...]`, or a JSON object of dimension to value or list of values). Its
`final_output` is an object keyed by partition key (`dim=value`, dimensions sorted and joined
with `,`), sorted, each a pointer or (under `-o value`) a value; `-o path` prints one line per
key in the same order. Past 1000 keys with no `--partition` (0.30.2, #623), `final_output` is
`{"_barca_partitions": {keys, computed, cached, outputs, truncated, hint}}` instead: `outputs` is
that object for the keys the run computed only (at most 1000, `truncated` when there are
more), `keys` and `cached` count the rest, and `-o path` prints the computed keys with a note on
stderr ([Partitions: Many keys](/reference/partitions/#many-keys)). `--partition` returns every key it selects. `--partition` narrows every partitioned node of the run with a named
dimension, with or without a target, on `get`, `run` and `plan`; the result then carries
`partition_selection` (`selection`, `nodes[]` with `id`, `selected`, `skipped`, and
`skipped_nodes`), and a node it does not run is a step with status (dry run: action) `skipped`
and reason `partition_selection`, without changing the exit code. A dimension no node in the
cone has, a value no node with that dimension has, or a target the selection would skip, is a
usage error (exit 2) before anything runs. Up to 0.29.2 it narrowed only the target.

Up to 0.24, `final_output` held the value of a json artifact and a terminal printed the value;
`-o value` (with `--json` for the JSON line) prints it now. A partitioned target returned the
value of its first key only.

## Environment variables

| Variable | Effect | Stability |
|---|---|---|
| `BARCA_OUTPUT` | `json` or `pretty`: output format when no flag is given; any other value is a usage error (exit 2) | stable |
| `BARCA_ENV` | environment name when `--env` is not given (beats `default_env` in `barca.toml`) | stable |
| `BARCA_REQUIRE_ENV` | `1` makes `--env` or `BARCA_ENV` required for every command that reads or writes an environment (exit 2 without it); `0` turns `require_env = true` in `barca.toml` off; anything else exits 2 | experimental |
| `BARCA_REMOTE` | `off` disables remote artifacts and history backups for this process; unset/empty retains normal config; other values are usage errors | stable |
| `BARCA_REMOTE_URI` | remote root for artifacts and metadata backups (`[remote].uri`) | stable |
| `BARCA_BACKUP_INTERVAL` | overrides `[remote].backup_interval`: positive whole duration in seconds or with s/m/h/d suffix, or off; default 1m; routine requests share the interval before copy/hash/upload, final backups remain; invalid values exit 2 | experimental |
| `BARCA_TRANSFER_CONCURRENCY` | positive integer: concurrent artifact transfers (`[remote].transfer_concurrency`, default 4); invalid values exit 3 | stable |
| `BARCA_TRANSFER_TIMEOUT` | positive integer seconds per transfer attempt (`[remote].transfer_timeout`, default 600); invalid values exit 3 | stable |
| `BARCA_TRANSFER_RETRIES` | whole number: how many times the transfer helper retries a failed artifact upload or download after its first attempt (default 3), waiting 0.5 s, then 1 s, 2 s, ... between attempts; a missing object or an authentication or configuration error is not retried. Read by the Python helper as written, not checked by barca | experimental |
| `BARCA_STORAGE_OPTIONS` | JSON object keyed by protocol, merged over `[remote.storage_options.*]` | stable |
| `BARCA_ARTIFACT_URI` | literal artifact root, bypassing the environment prefix (warns with a non-default env) | experimental |
| `BARCA_PROGRESS_SECS` | seconds between `still running` lines (default 15, `0` turns them off) | experimental |
| `BARCA_POOL_SIZE` | the most Python workers a run starts (else `[execution] pool_size`, else the available cores), capped at the capacity when that is smaller ([Assets, Sensors and Inputs: Heavy steps](/reference/assets/#heavy-steps-weight)) | experimental |
| `BARCA_COMM_COST_SECONDS` | the scheduler's per-dispatch cost estimate | experimental |
| `BARCA_TRACE_TIMING` | when set, prints a timing waterfall on stderr | experimental |
| `BARCA_CACHE_MAX_SIZE` | size limit for `.barca/` (`[cache] max_size`): `20GB`, `500MB`, `20GiB`, bytes, `10%` of the filesystem or `off`; default 10% of the filesystem; an invalid value is a usage error (exit 2) | experimental |
| `BARCA_CAPACITY` | the most the `weight=` of the steps running at once may add up to (beats `[execution] capacity` in `barca.toml`; default: the pool size); a whole number of at least 1, anything else is a usage error (exit 2) | experimental |
| `BARCA_SERVER` | the configured barca server (beats `[server] url` in `barca.toml`): the ledger of a local `get`/`run`, what `status`/`history`/`stats`/`logs` answer from, and what a bare `--server` uses; empty is unset; an invalid URL is a usage error ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)) | experimental |
| `BARCA_TELEMETRY` | comma-separated telemetry integrations to send finished runs to; `datadog` is the only one ([Telemetry (Datadog)](/reference/telemetry/)). Its settings are Datadog's own variables (`DD_SERVICE`, `DD_ENV`, `DD_VERSION`, `DD_TAGS`, `DD_TRACE_AGENT_URL`, `DD_AGENT_HOST`, `DD_TRACE_AGENT_PORT`, `DD_TRACE_ENABLED`, and for `barca serve` metrics `DD_DOGSTATSD_URL`, `DD_DOGSTATSD_SOCKET`, `DD_DOGSTATSD_PORT`) | experimental |
| `BARCA_SLACK_WEBHOOK_URL` | a Slack incoming webhook (`https://`); set and non-empty, barca posts when a job starts failing, recovers, or is still failing a day later ([Slack Alerts](/reference/slack/)). Never printed. `SLACK_WEBHOOK_URL` is ignored | experimental |
| `BARCA_SLACK_NOTIFY` | `changes` (default), `failures` or `all`: which finished runs post to Slack; any other value is a warning and `changes` applies | experimental |
| `BARCA_UI_URL` | base URL of the web UI; Slack posts link the run id to it | experimental |
| `BARCA_TRACE_LIBRARIES` | `1` or `true`: with `BARCA_TELEMETRY=datadog` and `ddtrace` installed, workers patch every library `ddtrace` supports. Unset: only those `DD_PATCH_MODULES` or `DD_TRACE_<LIB>_ENABLED=true` name ([Telemetry (Datadog)](/reference/telemetry/)) | experimental |
| `BARCA_SERVER_AUTH` | server authentication mode, `off` (default) or `token`; overrides `[server] auth`, overridden by `serve --auth`; invalid values are exit 2 ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)) | experimental |
| `BARCA_SERVER_TOKEN` | server-only bearer credential for every CLI and deprecated Python Client request; empty counts as unset; otherwise ASCII without whitespace or control characters; cannot be combined with URL userinfo; invalid credentials are exit 2; removed from Python helper environments ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)) | experimental |
| `BARCA_SERVER_RECONNECT` | seconds a `--server` client following a run (`get`/`run --server`, `logs -f --server`) waits for a server whose stream broke to come back, and follows the same run id there (default 60; `0`: give up at once, exit 3); not a number of seconds is a usage error (exit 2) | experimental |
| `DD_SITE` | the Datadog site (`datadoghq.com`, `us3.datadoghq.com`) whose web app the UI's trace links open (beats `[telemetry] datadog_site` in `barca.toml`); one that is not a host name stops `barca serve` with exit 2 ([Telemetry (Datadog)](/reference/telemetry/)) | experimental |
| `BARCA_LOG` | `json`: `barca serve` writes every line as one JSON object on stdout instead of text on stderr ([Telemetry (Datadog): Logs under barca serve](/reference/telemetry/#logs-under-barca-serve)); `text` or unset: the default. Under `serve` any other value is a usage error (exit 2). On every other command it changes nothing and prints `[barca] warning: BARCA_LOG applies to barca serve only; this command writes its usual lines` | experimental |

On a loopback TCP listener, every HTTP route first checks Host (or HTTP/2 authority).
Foreign hosts receive HTTP 403 in the standard error envelope (`kind: usage`, `code: 2`).
Loopback names and exact DNS names in `[server] allowed_hosts` are accepted; the project
socket is exempt. See [Server Mode: a Shared Ledger and --server: Loopback Host protection](/reference/server-mode/#loopback-host-protection).

The experimental HTTP query `GET /state?targets=a,b` computes only the named nodes and their
upstream dependencies, including durations. A target may be a group id (`group:FILE:NAME`, as
`GET /groups` lists it), which stands for every node in the group and its nested groups, and
`files=a.py,b.py` adds every node those source files define, so a view of thousands of nodes is
one short URL. Absent or empty `targets` and `files` mean all nodes; unknown targets, groups or
files answer `404`, ambiguous names `409`. It preserves the `NodeState` array shape (OpenAPI owns
it).

`[server] url` in `barca.toml` or `BARCA_SERVER` (experimental) points `get` and `run` at a
server's ledger ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)): they still run here, read cache hits from the server
and post what they compute, over `http://` or `https://`. `status`, `history`, `stats` and `logs` answer
from the server, and `doctor` checks it beside this machine. `--local` ignores it for one command. A server that cannot be reached, answers
unexpectedly or speaks another API version is exit 3 naming its URL, with `--local` as the
remediation; a server whose store is not this project's `[remote]` (or a project with no
`[remote]`) is exit 2 naming both.

Printed server URLs omit authority userinfo (`https://user:password@host/prefix` prints as
`https://host/prefix`), including error/remediation text, announcements, ledger warnings,
stream-drop messages, SQL provenance and doctor JSON. Network authentication and pending-post
identity still use the configured URL; response schemas are unchanged.

barca sets `BARCA_SOCKET`, `BARCA_WORKER`, `BARCA_WORKER_ID`, `BARCA_WORKER_AGENT` (and passes `BARCA_ARTIFACT_URI`,
`BARCA_STORAGE_OPTIONS`) for its own worker processes, and `BARCA_LIFELINE`, `BARCA_JOB_PGID`,
`BARCA_UPLOAD_SNAPSHOT_PREFIX` and `BARCA_DOCTOR_REQUEST` for its Python helpers; these are
internal, not part of the contract. `BARCA_TEST_*` variables are hooks for barca's own test suite, also not part of the
contract. Variables your nodes declare with `@asset(env=[...])` are yours ([Assets, Sensors and Inputs](/reference/assets/)).

Removed in 0.26.0 (breaking, RFC-0009): `BARCA_STATE`, `BARCA_STATE_URI`, `BARCA_PUSH_RETRIES`
and the `barca.toml` keys `[remote] state`, `[remote] state_uri`, `[remote] push_retries`. Any of
them set is a usage error (exit 2) naming it and the release, even under `BARCA_REMOTE=off`.

## Exit codes

Each error `kind` has exactly one exit code (stable):

<!-- BEGIN GENERATED exit-codes -->
| Code | `kind` | Stability |
|---|---|---|
| 0 | (success) | stable |
| 1 | `step_failed` | stable |
| 1 | `cache_miss` | stable |
| 2 | `usage` | stable |
| 3 | `infra` | stable |
| 130 | `cancelled` | stable |
<!-- END GENERATED exit-codes -->

`step_failed`: one of your steps raised (fix the code), or one of its `@sink`s could not be written (from 0.30.0; the error is a `SinkError` naming the sink). `usage`: bad arguments, unknown target,
task/asset misuse, a `.py` file that does not parse, a decorator called with an argument it does
not define (`@asset(after=x)`, `input=` for `inputs=`, `@asset(**options)`; [Assets, Sensors and Inputs](/reference/assets/),
"Accepted arguments"), or with a value barca cannot read from the source (`tags=TAGS`,
`inputs=make_inputs()`; "Values barca reads"), a DAG that cannot be built (an input that
names no definition, a cycle, a partitioned asset in an unpartitioned asset's `inputs=` without
`collect()`, a `partitions_from()` the asset cannot mirror), invalid `--env` or `barca.toml`, a
`get`/`run` that reads (or a `get` that targets) a `Manual` node that has never been computed,
without naming it in `--refresh` (the remediation is the command that computes it;
[The Cache](/reference/cache/)). `infra`:
barca or its environment failed (metadata DB, workers, the store, I/O); retrying may help.
`cancelled`: stopped by Ctrl-C (SIGINT) or by SIGTERM (what a supervisor, a CI timeout or
`docker stop` sends), at any point of a `get` or `run`: while steps run, and
with an artifact store also while artifacts upload or download or the history is backed up.
Workers keep private process groups. The coordinator handles terminal Ctrl-C and stops the
workers and their inherited subprocesses; a step cannot rely on receiving KeyboardInterrupt
or completing its exception/finally handlers. Stopped callers are resumed before TERM;
after 200 ms, remaining group members receive KILL. Deliberately daemonized children that
leave the group need cleanup by their launcher.
The exit code is 130 whatever the store does: a cancelled run spends at most 10 seconds backing
its record up, a second Ctrl-C or SIGTERM ends that at once, and a backup that fails then is a
line on stderr, not exit 3 ([Remote Storage: Ctrl-C](/reference/remote-storage/#ctrl-c)).

An artifact upload or history-backup upload that fails after every step succeeded is not exit 3
either: the run exits 0 with a warning and an `uploads` object, and the results are uploaded
by the next command that opens the store (see "get and run: results the store did not
receive").

The exit code is 130 for SIGTERM as well as for SIGINT. Barca handles both signals the same way
(it stops the workers, records the run as `cancelled` and exits), and the exit code follows the
error `kind`, which has one code. 143, which a shell reports for a process that SIGTERM killed,
is never barca's own exit code. SIGKILL cannot be handled: the shell reports 137 and the run is
later reported as `interrupted` ([The Cache](/reference/cache/)).

`barca serve` is not a run. Every run it accepts is recorded in its run queue before it answers,
and a restart resumes the queued runs in order under their ids. Stopped by SIGINT or SIGTERM it
drains: the queue pauses (new runs are still accepted, `202` with `status: pending` and
`waiting_for: "restart"`, and wait for the next start) and it waits for the runs in flight for up to `--drain-timeout` seconds
(`[server] drain_timeout` in `barca.toml`, default 30). It exits 0 when they all finished, and 0
when the deadline passed and it put the runs left back in the queue, to start again from the
beginning after the restart (at least once: [Operating barca serve: At least once](/reference/serve-ops/#at-least-once)). A second
signal cancels the runs in flight instead (recorded as `cancelled`, reason `server shutdown`) and
it exits 1 with a `step_failed` error naming how many were cancelled, so deploy tooling can tell
a forced stop from a clean one ([Operating barca serve: Stopping the server](/reference/serve-ops/#stopping-the-server)). Exit 1 from
`serve` means a forced stop, not a failed step of yours.

A writer `barca serve` (not `--read-only`) sets its metadata DB up before it binds the port. When
it cannot (the file is not SQLite, in a newer format or has a malformed schema, or cannot be
opened) it exits 3 with the error naming the file and the remediation, and serves nothing. A
`--read-only` server keeps serving and reports the error in `GET /health` `metadata_error`
([Scheduling With Cron](/scheduling/), "The metadata DB at startup"). An empty file (0 bytes) is not an
error: it holds no history, and a writer initializes it as a new database with one note line on
stderr naming it.

With `BARCA_LOG=json`, `barca serve` writes its lines, an error that stops it included, as JSON
objects on stdout instead of text on stderr. Exit codes do not change. `get`, `run` and every
other command keep progress and errors on stderr whatever `BARCA_LOG` says.

An error in a pipeline file (a syntax error, a decorator argument barca does not define, an
`inputs=` key or partition dimension that is not a parameter, a parameter nothing fills, an
invalid `env=`, cron or `max_failures=`) fails every command that reads that file, whatever the target: `list`,
`plan`, `status`, `get`, `run`, `stats` and `sql` exit 2 and run nothing. With no file arguments
that is every pipeline file of the project. The envelope's `error` starts with `Parse error:
<file>:<function> (line <n>):` for a decorator argument or a parameter mismatch, and its `remediation` says what to
change; the wording is not contract.

A syntax error in a project module that a pipeline file imports (a helper) fails the commands
that plan a step of that file (`plan`, `status`, `get`, `run`, `--dry-run`) with exit 2 before
anything runs; a command whose steps are all in other files, and `list`, are not affected. The
envelope's `error` starts with `Parse error: syntax error in <module file>: line <n>, column
<c>:` and names the step; its `remediation` shows the source line with a caret.

### A closed stdout or stderr (stable)

When the reader of barca's output goes away (`barca list | head -1`, a pager that quits, a
caller that stops reading), barca prints nothing about it and does not stop what it is doing:

- Output for the closed stream is dropped. There is no panic, no traceback and no message on
  the other stream.
- The command finishes. A `get` or `run` executes every step and records the run as it would
  with a reader, whether stdout, stderr or both are closed. A closed pipe is not a
  cancellation: to stop a run, interrupt it (Ctrl-C, exit 130).
- A step is not affected, whatever it does. Its `print` output, the output of a process it
  starts, a write to file descriptor 1 or 2 and the output of a C library are dropped like
  barca's own; none of them raises `BrokenPipeError` in the step or ends a child process with
  SIGPIPE. When barca's stderr is a pipe or a socket, the workers write to a pipe barca itself
  reads and copies to its stderr, so they never hold a pipe that can lose its reader. With a
  reader attached the output is the same as before, in the same order, on the same stream
  (stderr); when stderr is a terminal or a file the workers hold it directly, as before.
  SIGPIPE keeps its default for the processes a step starts, so `yes | head -1` inside a step
  ends as usual.
- The exit code does not change. It is the one the command would have had with a reader: 0
  when it succeeded, and 1, 2, 3 or 130 when it has an error of its own (the envelope is still
  written to stderr if stderr is open). A reader that stops early is not an error, so
  `barca list | head -1` and `barca list | grep -q name` exit 0, also under `set -o pipefail`.
  barca does not exit 141 and is not ended by SIGPIPE.

This holds for every command, `--help`, `--version` and `barca docs` included. Up to 0.18.1,
`docs` and `--help` behaved this way, every other command ended in a Rust panic (exit 101), and
a closed stderr abandoned a run in the middle (it stayed `interrupted` in `barca history`).

Limits: barca learns that a stream is closed only when it writes to it, so a run whose reader
has left goes on to its end; interrupt it to stop it.

**New in this release: a process a step leaves running.** If a step starts a process that
outlives the run (a server, a watcher) and lets it inherit the step's stdout and stderr, then,
when barca's stderr is a pipe or a socket, that process holds barca's pipe on both, not the
caller's. Once barca has exited nobody reads that pipe: the process's next write to its stdout
or its stderr ends it (SIGPIPE), or fails with `BrokenPipeError` if it ignores the signal. Up to
0.18.1 it held the caller's pipe instead, went on writing to it, and the caller's pipeline
(`barca run start_server 2>&1 | tee log`) did not finish until that process did. Nothing
changes when barca's stderr is a terminal or a file: the process holds that, as before. The
remedy is to give such a process its own output when starting it, for example
`subprocess.Popen(cmd, stdout=log, stderr=log)` or `stdout=subprocess.DEVNULL,
stderr=subprocess.DEVNULL`.

A caller's pipe in non-blocking mode is supported: while its reader is behind, barca waits for
it (no output is dropped, and the wait reaches the step that is printing), on stdout and on
stderr. A caller cannot tell from the exit code
that part of the result was not read: it is the caller that stopped reading. A write to stdout
that fails for any other reason (a full disk behind `> result.json`) is exit 3, with one line
on stderr.

Breaking in 0.24.0: the deprecated `get`/`run --no-cache` flag is removed (usage error,
exit 2). Use `--refresh-all`. Python `barca.get` also removes the deprecated keyword;
use `refresh_all=True`.

`list`, `status` and `history` return every item in JSON by default, including piped
stdout, `--fields` and `BARCA_OUTPUT=json`. An explicit `--limit N` applies in either
format; `--all` cannot be combined with it. Human tables default to 100 nodes for
`list`/`status` and 10 runs for `history`. Truncation still reports `total`, `truncated`
and a `hint` naming `--all`; the status summary still counts every node. So does a filter:
`status --state <states>` and `--problems` keep only the matching nodes (`total` counts them),
name the filter in `filter`, and leave `summary` counting every node in scope.

## JSON output schemas

How to read the tables: a key path is dotted, `[]` is "each item of the array", `<name>` stands
for keys you chose (target names, environment variable names) and `<user value>` is the value
your function returned. Types are JSON types; `a | b` means either. `sometimes` means the key is
present on some objects only (see each section for when). Values in the tables come from one
fixture run, so a nullable key can show only the type it had there; the notes say which keys
can be `null`.

Every schema below is stable unless its section says otherwise.

### Plan warnings

`plan`, `get`, `run` and `get|run --dry-run` carry a top-level `warnings` key in every JSON
document they print on stdout: success, failed (`status: "failed"`), several targets and dry run
alike. It is **always present and always an array**, `[]` when there is nothing to report, like
every other array in these schemas (an empty array is never dropped, and never `null`). In the
tables below a section whose fixture run had no warning shows only the `warnings` row; one that
had a warning also shows the item rows.

Each item is an object with exactly these keys, all strings and always present:

| Key | Value |
|---|---|
| `kind` | what was found: `unused_input` or `undeclared_env`; treat an unknown value as "other" |
| `node` | the step's node id, `file.py:name` (never a partition key: a partitioned step is reported once) |
| `param` | the parameter concerned (`unused_input`), or the environment variable (`undeclared_env`) |
| `message` | the warning in words, with what to do about it. The wording is not contract |

The list covers the steps the command planned (the targets' cones; the whole project for `plan`
or `get` with no target), one item per step and parameter, sorted by node, parameter and kind. It is a function of the
source files and the targets only: cache state, `--refresh*`, `--env` and the output mode do not
change it, so a dry run reports exactly what the real run will. Each item is also printed once on
stderr as `[barca] warning: <message>` before any step runs, in every output mode (except
`undeclared_env`, summarized in one line, below). Warnings never
change the exit code, and the error envelope on stderr never contains them. `list`, `status`,
`history`, `stats` and `sql` have no `warnings` key. `barca serve` carries the same array in
`GET /plan` and in the run outcome of `GET /runs/{run_id}` (experimental with the rest of that
API).

`unused_input`: a step declares an input its function never uses ([Assets, Sensors and Inputs: Unused inputs](/reference/assets/#unused-inputs), has the exact rule and what is never reported). Stability: the key, its presence and the
item keys are stable on `get` and `run`, and experimental with the rest of the output on `plan`;
the set of `kind` values grows additively.

`undeclared_env`: a step's code reads an environment variable it does not declare in `env=[...]`
([The Cache: Environment variables](/reference/cache/#environment-variables)), one item per step and variable; the items of one
step carry the same `message`. These items are not printed one by one: stderr gets a single
summary line for the command, `[barca] warning: <n> step(s) read(s) environment variables not
declared in env=: <at most 5 names, sorted> (+<k> more); barca status lists them`, printed only
when there is at least one. A variable name built at run
time is a pattern with `*` (`*_REGISTRY_CSV`). The kind is experimental: which reads are found
may grow.

### get and run: one target

`barca get <asset> files --json` and `barca run <task> files --json` print one JSON line.

<!-- BEGIN GENERATED schema get -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get -->

- `status` is `success` (see the failed result line below for `failed`).
- `steps_executed` counts the steps that executed, failed ones included (a failed result's
  `steps[]` says which, by `status`); it is 0 when everything came from cache. On `get`, `final_output` is the
  pointer to the target's artifact (with no target, the last asset's); on `run` it is the
  task's value, or `null` for a task that returned nothing.
- `steps[]`: `status` is `ran`, `cached`, `partial` or `failed`; `reason` (why it ran) is one of
  `task`, `sensor`, `refresh`, `refresh_cascade`, `refresh_all`, `not_materialized`,
  `artifact_missing`, `evicted`, `partitions_unknown`, `sensor_output_unknown`, with `detail` in
  words. A `cached` step served from a pin ([barca pin and unpin](/reference/cli/pin/)) has `reason` `pinned`. A cached refresh satisfied by a current-hash success recorded at or after
  original acceptance has `reason` `refreshed_by_this_run`. Under `--partition`, a node the selection does not run (a fan-in over the selected keys,
  or what is below one) has `status` `skipped` (in a dry run, `action` `skipped`) and `reason`
  `partition_selection`, and the result has `partition_selection` (see "Partitioned targets").
  After a partition key fails, a key of a node that reads it key for key is a step with `status`
  `skipped`, `reason` `upstream_key_failed` and a `detail` naming the failed key
  ([Partitions: Failed keys block their own keys](/reference/partitions/#failed-keys-block-their-own-keys)); a node that reads every key
  (a `collect()` fan-in, an unpartitioned node) is `skipped` with reason `upstream_failed`,
  as before. `barca status` reports such a node `blocked`. In a scheduled tick of `barca serve`,
  a paused target (`barca pause`, [barca pause and resume](/reference/cli/pause/)) is `skipped` with reason `paused` and a
  `detail` naming `barca resume`. A `cached` step of a `Manual` node read as it is (not recomputed, though it may be
  stale; [The Cache](/reference/cache/)) carries a `detail` starting `Manual`. Breaking in this release: a
  stale `Manual` upstream used to be recomputed; it is now read as it is, with a note on stderr.
  `artifact_missing` is a step that has a cached result whose artifact file is gone
  and is needed ([The Cache](/reference/cache/)); `evicted` is the same when the cache size limit deleted
  the file (experimental, [Disk Use](/reference/disk/)).
- `cache` (experimental) appears only when the check after the run found `.barca/` over its
  size limit: `{size_bytes, limit_bytes, evicted_files, evicted_bytes}`, sizes after eviction
  ([Disk Use](/reference/disk/)). It is on single- and multi-target results alike.
- `get` and `run` share one refresh vocabulary: `--refresh a,b` (cascading downstream),
  `--no-cascade`, `--refresh-all`. `artifact` appears on cached steps, `run_hash`
  on unpartitioned steps, `warning` on a cached step whose upstream was refreshed without
  cascading and on a step with `artifact_mismatch` (see "A store copy that differs from its
  recorded hash" below), `env` on nodes that declare `env=[...]` (the value in barca's environment, else in the
  project's `.env`; `null` when unset in both, `"<redacted>"` for secret-looking names).

Executed steps also carry `attempts` and `elapsed_seconds`, including failures.
Elapsed seconds sum active reported time across attempts; backoff is excluded; a step's
sinks are excluded too (`sink_seconds`, below). For
partition summaries, elapsed time sums executed keys while attempts is the maximum
per key. Cached steps and dry runs omit these keys. Worker disconnects use the
coordinator's observed in-flight time. Slow module imports (> 1 second) add
`import_seconds`, included in elapsed time and summed across attempts/keys. Reused
modules add no import time. The conditional import field is snapshotted with a
controlled slow-import/retry fixture below; ordinary schema fixtures exclude it to
avoid depending on CI machine speed. [barca status](/reference/cli/status/) explains the timing scopes.

<!-- BEGIN GENERATED schema get_timing_retry -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].import_seconds` | number | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_timing_retry -->

A step whose asset declares `@sink`s adds `sink_seconds` (stable, 0.45.0): the seconds its sinks
took to write, summed over the keys that ran. They are written after the step's own work and are
not part of its `elapsed_seconds`, which stays the time of the step itself; a step's time with its
sinks is the sum of the two. Cached steps, dry runs and steps without sinks omit it. `barca history
RUN_ID --json` has it on each recorded step (`null` without sinks, on a failed step and on rows from
before 0.45.0), and each sink's own time is `seconds` in the step's record. [Sinks](/reference/sinks/)
says what a sink writes.

<!-- BEGIN GENERATED schema get_sink -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].sink_seconds` | number | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_sink -->

<!-- BEGIN GENERATED schema history_sink -->
| Key | Type | Present |
|---|---|---|
| `explanation` | object | always |
| `explanation.attempts` | string | always |
| `explanation.cancellation` | null | always |
| `explanation.counts` | object | always |
| `explanation.counts.attempts` | integer | always |
| `explanation.counts.attempts_line` | string | always |
| `explanation.counts.cached` | integer | always |
| `explanation.counts.failed` | integer | always |
| `explanation.counts.nodes` | integer | always |
| `explanation.counts.not_reached` | integer | always |
| `explanation.counts.not_reached_line` | null | always |
| `explanation.counts.not_reached_nodes` | integer | always |
| `explanation.counts.ok` | integer | always |
| `explanation.counts.outcome` | string | always |
| `explanation.counts.planned` | string | always |
| `explanation.counts.ran` | integer | always |
| `explanation.counts.retries` | integer | always |
| `explanation.counts.steps` | integer | always |
| `explanation.counts.units` | string | always |
| `explanation.outputs` | array | always |
| `explanation.outputs[]` | object | always |
| `explanation.outputs[].format` | string | always |
| `explanation.outputs[].keys` | null | always |
| `explanation.outputs[].line` | string | always |
| `explanation.outputs[].node_id` | string | always |
| `explanation.outputs[].path` | string | always |
| `explanation.outputs[].rows` | null | always |
| `explanation.outputs[].size_bytes` | integer | always |
| `explanation.progress` | null | always |
| `explanation.requested_by` | string | always |
| `explanation.slowest` | array | always |
| `explanation.slowest[]` | object | always |
| `explanation.slowest[].keys` | integer | always |
| `explanation.slowest[].node_id` | string | always |
| `explanation.slowest[].seconds` | number | always |
| `explanation.steps` | string | always |
| `explanation.target_line` | string | always |
| `explanation.target_outcomes` | array | always |
| `explanation.target_outcomes[]` | string | always |
| `explanation.targets` | array | always |
| `explanation.targets[]` | string | always |
| `explanation.why` | object | always |
| `explanation.why.groups` | array | always |
| `explanation.why.groups[]` | object | always |
| `explanation.why.groups[].category` | string | always |
| `explanation.why.groups[].label` | string | always |
| `explanation.why.groups[].rows` | array | always |
| `explanation.why.groups[].rows[]` | object | always |
| `explanation.why.groups[].rows[].counts` | string | always |
| `explanation.why.groups[].rows[].example` | string | always |
| `explanation.why.groups[].rows[].example_text` | string | always |
| `explanation.why.groups[].rows[].line` | string | always |
| `explanation.why.groups[].rows[].nodes` | integer | always |
| `explanation.why.groups[].rows[].share` | number | always |
| `explanation.why.groups[].rows[].steps` | integer | always |
| `explanation.why.groups[].rows[].subject` | null | always |
| `explanation.why.groups[].steps` | integer | always |
| `explanation.why.headline` | string | always |
| `explanation.why.omitted` | integer | always |
| `explanation.why.partial` | boolean | always |
| `run` | object | always |
| `run.attempts` | integer | always |
| `run.cancellation` | null | always |
| `run.client_run_id` | null | always |
| `run.command` | string | always |
| `run.elapsed_seconds` | number | always |
| `run.failure` | null | always |
| `run.files` | array | always |
| `run.files[]` | string | always |
| `run.finished_at` | string | always |
| `run.job` | string | always |
| `run.plan_reasons` | object | always |
| `run.plan_reasons.nodes` | integer | always |
| `run.plan_reasons.omitted` | integer | always |
| `run.plan_reasons.reasons` | array | always |
| `run.plan_reasons.reasons[]` | object | always |
| `run.plan_reasons.reasons[].category` | string | always |
| `run.plan_reasons.reasons[].example` | string | always |
| `run.plan_reasons.reasons[].nodes` | integer | always |
| `run.plan_reasons.reasons[].steps` | integer | always |
| `run.plan_reasons.reasons[].subject` | null | always |
| `run.plan_reasons.steps` | integer | always |
| `run.principal` | null | always |
| `run.retries` | integer | always |
| `run.run_id` | string | always |
| `run.schedule_id` | null | always |
| `run.scheduled_at` | null | always |
| `run.started_at` | string | always |
| `run.status` | string | always |
| `run.steps_cached` | integer | always |
| `run.steps_executed` | integer | always |
| `run.steps_total` | integer | always |
| `run.target` | string | always |
| `run.trigger_reason` | null | always |
| `run.triggered_by` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact_format` | string | always |
| `steps[].artifact_path` | string | always |
| `steps[].artifact_size_bytes` | integer | always |
| `steps[].attempts` | integer | always |
| `steps[].batch` | null | always |
| `steps[].cpu_seconds` | number | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].error` | null | always |
| `steps[].error_message` | null | always |
| `steps[].error_phase` | null | always |
| `steps[].error_traceback` | null | always |
| `steps[].error_type` | null | always |
| `steps[].max_rss_bytes` | integer | always |
| `steps[].node_id` | string | always |
| `steps[].output_tail` | null | always |
| `steps[].partition_key` | null | always |
| `steps[].rows` | null | always |
| `steps[].run_hash` | string | always |
| `steps[].sink_seconds` | number | always |
| `steps[].status` | string | always |
| `steps[].worker_exit` | null | always |
| `steps_page` | object | always |
| `steps_page.nodes` | array | always |
| `steps_page.nodes[]` | object | always |
| `steps_page.nodes[].calls` | integer | always |
| `steps_page.nodes[].elapsed_seconds` | number | always |
| `steps_page.nodes[].failed` | integer | always |
| `steps_page.nodes[].keys` | integer | always |
| `steps_page.nodes[].node_id` | string | always |
| `steps_page.nodes[].status` | string | always |
| `steps_page.total` | integer | always |
| `steps_page.truncated` | boolean | always |
<!-- END GENERATED schema history_sink -->

Under `-o value --json`, `final_output` is the value instead (a table is a list of row
objects):

<!-- BEGIN GENERATED schema get_value -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_value -->

A parquet or pickle artifact has the same pointer as a json one:

<!-- BEGIN GENERATED schema get_artifact_pointer -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_artifact_pointer -->

A partitioned step carries `partitions` (counted in keys; `will_run_keys` lists the first 20 keys in lexical order):

<!-- BEGIN GENERATED schema get_partitioned -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | sometimes |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].partitions.will_run_keys[]` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | sometimes |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partitioned -->

A partitioned target (`get per_key`) returns every key: `final_output` is an object keyed by
partition key, each a pointer (past 1000 keys, the `_barca_partitions` summary above).

<!-- BEGIN GENERATED schema get_partition_keys -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output.<key>` | object | always |
| `final_output.<key>._barca_artifact` | object | always |
| `final_output.<key>._barca_artifact.format` | string | always |
| `final_output.<key>._barca_artifact.path` | string | always |
| `final_output.<key>._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | always |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_keys -->

With `--partition k=a -o value --json`, only the selected keys, each its value:

<!-- BEGIN GENERATED schema get_partition_values -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output.<key>` | `<user value>` | always |
| `partition_selection` | object | always |
| `partition_selection.nodes` | array | always |
| `partition_selection.nodes[]` | object | always |
| `partition_selection.nodes[].id` | string | always |
| `partition_selection.nodes[].selected` | integer | always |
| `partition_selection.nodes[].skipped` | integer | always |
| `partition_selection.selection` | object | always |
| `partition_selection.selection.k` | array | always |
| `partition_selection.selection.k[]` | string | always |
| `partition_selection.skipped_nodes` | array | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | always |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_values -->

With `--partition k=a` and no target, every narrowed node's key counts in `partition_selection`
and the skipped fan-in in `steps` and `skipped_nodes` (the dry run has the same key, with
`action` `skipped`):

<!-- BEGIN GENERATED schema get_partition_selection -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `partition_selection` | object | always |
| `partition_selection.nodes` | array | always |
| `partition_selection.nodes[]` | object | always |
| `partition_selection.nodes[].id` | string | always |
| `partition_selection.nodes[].selected` | integer | always |
| `partition_selection.nodes[].skipped` | integer | always |
| `partition_selection.selection` | object | always |
| `partition_selection.selection.k` | array | always |
| `partition_selection.selection.k[]` | string | always |
| `partition_selection.skipped_nodes` | array | always |
| `partition_selection.skipped_nodes[]` | string | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | sometimes |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | sometimes |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_selection -->

<!-- BEGIN GENERATED schema get_partition_selection_dry_run -->
| Key | Type | Present |
|---|---|---|
| `capacity` | integer | always |
| `command` | string | always |
| `dry_run` | boolean | always |
| `partition_selection` | object | always |
| `partition_selection.nodes` | array | always |
| `partition_selection.nodes[]` | object | always |
| `partition_selection.nodes[].id` | string | always |
| `partition_selection.nodes[].selected` | integer | always |
| `partition_selection.nodes[].skipped` | integer | always |
| `partition_selection.selection` | object | always |
| `partition_selection.selection.k` | array | always |
| `partition_selection.selection.k[]` | string | always |
| `partition_selection.skipped_nodes` | array | always |
| `partition_selection.skipped_nodes[]` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].artifact` | string | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | sometimes |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | sometimes |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `target` | null | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_selection_dry_run -->

`run` prints the same shape:

<!-- BEGIN GENERATED schema run -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps[].undeclared_env` | array | sometimes |
| `steps[].undeclared_env[]` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
<!-- END GENERATED schema run -->

A partitioned task returns keyed values:

<!-- BEGIN GENERATED schema run_partitioned -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output.<key>` | `<user value>` | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | always |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].partitions.will_run_keys[]` | string | always |
| `steps[].reason` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema run_partitioned -->

### get and run: a failed step

When a step raises in JSON mode, stdout still gets one result line (exit 1), and the error
envelope goes to stderr:

<!-- BEGIN GENERATED schema run_failed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `error` | string | always |
| `failed_node` | string | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema run_failed -->

A partitioned target of `get` with a failed key is a failed step as above (exit 1, `failed_node`
names the key's step, e.g. `pipeline.py:flaky[k=bad]`). The line also has `final_output`: the
keys that succeeded, keyed as above; the failed keys are absent and have no artifact, so the next
`get` computes them again. Stability: experimental. Whether a partial failure gets its own exit
code, and whether the result lists the failed keys, is not decided yet.

<!-- BEGIN GENERATED schema get_partition_failed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `error` | string | always |
| `failed_node` | string | always |
| `final_output` | object | always |
| `final_output.<key>` | object | always |
| `final_output.<key>._barca_artifact` | object | always |
| `final_output.<key>._barca_artifact.format` | string | always |
| `final_output.<key>._barca_artifact.path` | string | always |
| `final_output.<key>._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].partitions` | object | always |
| `steps[].partitions.cached` | integer | always |
| `steps[].partitions.total` | integer | always |
| `steps[].partitions.will_run` | integer | always |
| `steps[].partitions.will_run_keys` | array | always |
| `steps[].partitions.will_run_keys[]` | string | always |
| `steps[].reason` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_partition_failed -->

### get and run: a store copy that differs from its recorded hash

With an artifact store, an artifact fetched from the store is checked against the SHA-256
recorded when it was written ([Remote Storage: Checking a local copy against the store](/reference/remote-storage/#checking-a-local-copy-against-the-store)). A copy with
other bytes is still used, the exit code does not change, and the result says so on the step
entries:

<!-- BEGIN GENERATED schema get_artifact_mismatch -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].artifact_mismatch` | boolean | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].bytes_changed` | boolean | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps[].warning` | string | sometimes |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_artifact_mismatch -->

- `steps[].artifact_mismatch` is the boolean `true`, or the key is absent. It is never `false`
  and never `null`: test for the key. It is on the step the artifact belongs to, whatever that
  step's `status`, and on every step that read the artifact as an input in this run (`status`
  `ran`, `partial` or `failed`). A step that only depends on such a reader does not have it.
- `steps[].warning` on the same entries is the finding in words: the store location, and the
  `--refresh` that recomputes the step. The wording is not contract; `artifact_mismatch` is.
- Without a server, the run records the store copy's digest as the result's, so the finding is
  reported by the run that made it and not by later ones ([Remote Storage](/reference/remote-storage/)).
- The key appears on `get` and `run`, with one target or several, in a failed run's result
  line, with `--agent`, and in the `steps` of a `barca serve` run's outcome (`GET /runs/{run_id}`).
  `--fields` accepts it (`--fields id,status,artifact_mismatch`). It never appears with
  `--dry-run`, which does not contact the store, nor in `status`, `history` or `stats`, which
  describe recorded results and do not fetch.
- It is not an item of the top-level `warnings` array. That array is the plan warnings (see
  "Plan warnings"): a function of the source files and the targets, the same in a dry run and
  whatever is cached. A mismatch is found only when a run fetches an artifact, so it is
  reported where the other facts about what happened to a step are.
- stderr has the same finding in every output mode, `--agent` included, as
  `[barca] warning: <id>: the copy at <location> is not the one this result was recorded
  with ...`. Like every `[barca] warning:` line its wording is experimental: read the key.

In the fixture run above, the store's copy of one asset was overwritten after it was recorded;
that asset and the step recomputed from it carry the key, and the two other steps do not.
Stability: stable.

### get and run: results the store did not receive

With an artifact store, a run whose steps all succeeded and whose only failure is an upload of
an artifact (or of the end-of-run history backup) is a success: exit 0, the usual
result, one `[barca] warning: <n> result(s) not uploaded to <store> (<ids>): <why>; ...` line
on stderr, and an `uploads` object (#390). The results are recorded and kept on this machine;
the next `get` or `run` that opens the store uploads them before its own work and says so on
stderr (`[barca] uploading <n> pending results ...`). There is no flag to give up on them.
A store that fails before any step runs (the startup check, a cached artifact that cannot be
fetched) is still exit 3.

<!-- BEGIN GENERATED schema get_uploads_pending -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `uploads` | object | always |
| `uploads.error` | string | always |
| `uploads.nodes` | array | always |
| `uploads.nodes[]` | string | always |
| `uploads.pending` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_uploads_pending -->

- `uploads.pending` counts this machine's results not yet in the store after the run,
  including earlier runs' that failed again; `uploads.nodes` names them (step ids, partition
  keys included, sorted); `uploads.error` is why, in words (`null` only when nothing says).
  A failed backup upload alone gives `pending: 0` and its error (`backup to <store> failed: ...`). The key is absent when nothing
  is pending: test for the key. It is on `get` and `run`, with one target or several, and on
  `result` of `barca serve`'s run status.
- `barca status` shows each such node with `cache.state` `unsynced` and `cache.reason`
  `local_only` (the table prints `unsynced  local only`), and `summary.unsynced` counts them:

<!-- BEGIN GENERATED schema status_unsynced -->
| Key | Type | Present |
|---|---|---|
| `as_of` | string | always |
| `data_coverage` | object | always |
| `data_coverage.assets` | integer | always |
| `data_coverage.measured` | integer | always |
| `data_coverage.suspect` | integer | always |
| `disk_usage` | object | always |
| `disk_usage.backup_bytes` | integer | always |
| `disk_usage.budget_bytes` | integer | always |
| `disk_usage.limit_bytes` | integer | always |
| `disk_usage.size_bytes` | integer | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.artifact` | string | always |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | always |
| `nodes[].cache.state` | string | always |
| `nodes[].cause` | string | always |
| `nodes[].env` | array | always |
| `nodes[].expected_fresh_at` | null | always |
| `nodes[].gap_seconds` | integer | always |
| `nodes[].health` | string | always |
| `nodes[].health_reason` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `nodes[].last_attempt` | string | always |
| `nodes[].last_check` | null | always |
| `nodes[].last_materialization` | object | always |
| `nodes[].last_materialization.artifact` | string | always |
| `nodes[].last_materialization.content_hash` | string | always |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | number | always |
| `nodes[].last_materialization.format` | string | always |
| `nodes[].last_materialization.rows` | null | always |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | integer | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].measured` | boolean | always |
| `nodes[].name` | string | always |
| `nodes[].no_estimate` | string | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].recheck_at` | null | always |
| `nodes[].refresh_path` | null | always |
| `nodes[].root` | object | always |
| `nodes[].root.id` | string | always |
| `nodes[].root.kind` | string | always |
| `nodes[].root.node` | string | always |
| `nodes[].shape` | object | always |
| `nodes[].shape.keys` | array | sometimes |
| `nodes[].shape.keys[]` | string | always |
| `nodes[].shape.rows` | integer | sometimes |
| `nodes[].shape.type` | string | always |
| `nodes[].signals` | array | always |
| `nodes[].stale_since` | null | always |
| `nodes[].typical_run_seconds` | number | always |
| `rollup` | object | always |
| `rollup.also_in` | array | always |
| `rollup.asset_causes` | object | always |
| `rollup.asset_causes.catching_up` | integer | always |
| `rollup.asset_causes.failing` | integer | always |
| `rollup.asset_causes.no_refresh_path` | integer | always |
| `rollup.asset_causes.overdue` | integer | always |
| `rollup.asset_causes.queued` | integer | always |
| `rollup.asset_causes.undecided` | integer | always |
| `rollup.asset_health` | object | always |
| `rollup.asset_health.behind` | integer | always |
| `rollup.asset_health.blocked` | integer | always |
| `rollup.asset_health.failing` | integer | always |
| `rollup.asset_health.never_run` | integer | always |
| `rollup.asset_health.ok` | integer | always |
| `rollup.asset_health.suspect` | integer | always |
| `rollup.asset_health.undecided` | integer | always |
| `rollup.assets` | integer | always |
| `rollup.causes` | object | always |
| `rollup.causes.catching_up` | integer | always |
| `rollup.causes.failing` | integer | always |
| `rollup.causes.no_refresh_path` | integer | always |
| `rollup.causes.overdue` | integer | always |
| `rollup.causes.queued` | integer | always |
| `rollup.causes.undecided` | integer | always |
| `rollup.children` | array | always |
| `rollup.data_coverage` | object | always |
| `rollup.data_coverage.assets` | integer | always |
| `rollup.data_coverage.measured` | integer | always |
| `rollup.data_coverage.suspect` | integer | always |
| `rollup.fresh_by` | null | always |
| `rollup.gap_seconds` | integer | always |
| `rollup.health` | object | always |
| `rollup.health.behind` | integer | always |
| `rollup.health.blocked` | integer | always |
| `rollup.health.failing` | integer | always |
| `rollup.health.never_run` | integer | always |
| `rollup.health.ok` | integer | always |
| `rollup.health.suspect` | integer | always |
| `rollup.health.undecided` | integer | always |
| `rollup.held_by` | array | always |
| `rollup.held_by[]` | object | always |
| `rollup.held_by[].action_kind` | string | always |
| `rollup.held_by[].assets` | integer | always |
| `rollup.held_by[].causes` | object | always |
| `rollup.held_by[].causes.catching_up` | integer | always |
| `rollup.held_by[].causes.failing` | integer | always |
| `rollup.held_by[].causes.no_refresh_path` | integer | always |
| `rollup.held_by[].causes.overdue` | integer | always |
| `rollup.held_by[].causes.queued` | integer | always |
| `rollup.held_by[].causes.undecided` | integer | always |
| `rollup.held_by[].cumulative_nodes` | integer | always |
| `rollup.held_by[].fresh_by` | null | always |
| `rollup.held_by[].gap_seconds` | integer | always |
| `rollup.held_by[].nodes` | integer | always |
| `rollup.held_by[].root` | string | always |
| `rollup.held_by[].stale_since` | null | always |
| `rollup.held_by[].unbounded` | integer | always |
| `rollup.held_by[].weight` | integer | always |
| `rollup.id` | string | always |
| `rollup.kind` | string | always |
| `rollup.label` | string | always |
| `rollup.nodes` | integer | always |
| `rollup.not_fresh` | integer | always |
| `rollup.parent` | null | always |
| `rollup.running` | integer | always |
| `rollup.split` | object | always |
| `rollup.split.catching_up` | integer | always |
| `rollup.split.in_hand_nodes` | integer | always |
| `rollup.split.in_hand_weight` | integer | always |
| `rollup.split.need_someone` | integer | always |
| `rollup.split.need_someone_nodes` | integer | always |
| `rollup.split.need_someone_weight` | integer | always |
| `rollup.split.run_by_hand` | integer | always |
| `rollup.stale_since` | null | always |
| `rollup.unbounded` | integer | always |
| `rollup.weight` | integer | always |
| `rollup.weights` | object | always |
| `rollup.weights.catching_up` | integer | always |
| `rollup.weights.failing` | integer | always |
| `rollup.weights.fresh` | integer | always |
| `rollup.weights.no_refresh_path` | integer | always |
| `rollup.weights.overdue` | integer | always |
| `rollup.weights.queued` | integer | always |
| `rollup.weights.undecided` | integer | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.blocked` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_unsynced -->

- `--dry-run` has the same `uploads` object when results are pending, and predicts them
  `cached`; the human dry run adds a line naming them:

<!-- BEGIN GENERATED schema get_dry_run_uploads_pending -->
| Key | Type | Present |
|---|---|---|
| `capacity` | integer | always |
| `command` | string | always |
| `dry_run` | boolean | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].artifact` | string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].run_hash` | string | always |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `target` | string | always |
| `uploads` | object | always |
| `uploads.error` | string | always |
| `uploads.nodes` | array | always |
| `uploads.nodes[]` | string | always |
| `uploads.pending` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_dry_run_uploads_pending -->

With an artifact store, `barca status` also reports `unsynced` with `cache.reason` `store_only`
(in the store, not on this machine) and `mismatch` (the two copies differ in size, or in bytes
for a plain-directory store, or this machine's copy is not the recorded one) for unpartitioned
cached nodes; a result in neither place is `stale` / `artifact_missing` (RFC-0009 section 4.1,
reasons spelled as `cache.reason` snake_case values). `barca get` on such a target repairs it
without running a step: it uploads a `local_only` copy, uploads a `mismatch` copy that is the
recorded one (and prints the changed-bytes warning and sets `steps[].bytes_changed: true` on
that step), and fetches a `store_only` one, or the store's copy for a `mismatch` whose local
copy is not the recorded one (`steps[].artifact_mismatch`; without a server its digest is then
recorded, so the next `get` reports nothing); the exit code does not change. Stability: stable.

### get and run: several targets

With `a,b`, `final_output` is replaced by `targets`, keyed by target name in the order given.
A successful target has `final_output` (on `get` a pointer, or with `-o value --json` its value);
a failed partitioned target of `get` also has `final_output` with the keys that succeeded; a
failed one has `failed_node` (the step that raised:
the target or something upstream; the same key as a failed single-target run) and `error`. Top-level `status` is `failed` if any target
failed, and the exit code is then 1. Steps skipped because an upstream failed have `status`
`skipped` and `reason` `upstream_failed`.

<!-- BEGIN GENERATED schema get_multi_target -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `targets` | object | always |
| `targets.<name>` | object | always |
| `targets.<name>.final_output` | object | always |
| `targets.<name>.final_output._barca_artifact` | object | always |
| `targets.<name>.final_output._barca_artifact.format` | string | always |
| `targets.<name>.final_output._barca_artifact.path` | string | always |
| `targets.<name>.final_output._barca_artifact.size_bytes` | integer | always |
| `targets.<name>.status` | string | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_multi_target -->

<!-- BEGIN GENERATED schema run_multi_target_failed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact` | string | sometimes |
| `steps[].attempts` | integer | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].elapsed_seconds` | number | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps[].undeclared_env` | array | sometimes |
| `steps[].undeclared_env[]` | string | always |
| `steps_executed` | integer | always |
| `targets` | object | always |
| `targets.<name>` | object | always |
| `targets.<name>.error` | string | sometimes |
| `targets.<name>.failed_node` | string | sometimes |
| `targets.<name>.final_output` | `<user value>` | sometimes |
| `targets.<name>.status` | string | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
<!-- END GENERATED schema run_multi_target_failed -->

### Dry run

`--dry-run` on `get` or `run` executes no steps and records no new run; it prints what
would happen. `partitions(<expression>)` may evaluate Python while resolving keys. `steps[].action` is
`cached`, `run`, `partial` or `unknown` (in place of `status`). An `unknown` step has `reason`
`partitions_unknown` (dynamic partitions whose source has not run), `sensor_output_unknown` (it,
or a step upstream of it, reads a sensor with no recorded output) or `cutoff_pending` (it, or a
step upstream of it, reads an `@asset(cutoff=True)` that would run, so whether it runs depends on
the bytes that asset writes; experimental). A step that reads a sensor is
predicted from the sensor's last recorded output, and `detail` says so. `capacity` is the most the
`weight=` of the steps running at once may add up to, and a step whose node has a `weight=` above
1 carries `weight` (both experimental, #436). With one target the
document has `target` (a name, or `null` for a whole file):

<!-- BEGIN GENERATED schema get_dry_run -->
| Key | Type | Present |
|---|---|---|
| `capacity` | integer | always |
| `command` | string | always |
| `dry_run` | boolean | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].detail` | string | always |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `target` | string | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_dry_run -->

With several targets, `targets` replaces `target`: an object keyed by target name in the order
given (like a real multi-target run), each `{"summary": {...}}` counted over that target's cone.
The top-level `summary` counts the union once.

<!-- BEGIN GENERATED schema run_dry_run_multi_target -->
| Key | Type | Present |
|---|---|---|
| `capacity` | integer | always |
| `command` | string | always |
| `dry_run` | boolean | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].artifact` | string | sometimes |
| `steps[].detail` | string | sometimes |
| `steps[].env` | object | sometimes |
| `steps[].env.<name>` | null \| string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | sometimes |
| `steps[].run_hash` | string | always |
| `steps[].undeclared_env` | array | sometimes |
| `steps[].undeclared_env[]` | string | always |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `targets` | object | always |
| `targets.<name>` | object | always |
| `targets.<name>.summary` | object | always |
| `targets.<name>.summary.cached` | integer | always |
| `targets.<name>.summary.unknown` | integer | always |
| `targets.<name>.summary.will_run` | integer | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
<!-- END GENERATED schema run_dry_run_multi_target -->

### list

List-shaped output is an envelope: the items, `total` (how many exist) and `truncated`. `hint`
appears only when `truncated` is true. `--fields` keeps only the named keys on each item.

<!-- BEGIN GENERATED schema list -->
| Key | Type | Present |
|---|---|---|
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].env` | array | always |
| `nodes[].env[]` | string | always |
| `nodes[].file` | string | always |
| `nodes[].freshness` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `nodes[].line` | integer | always |
| `root` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema list -->

### `list --groups`

<!-- BEGIN GENERATED schema list_groups -->
| Key | Type | Present |
|---|---|---|
| `groups` | array | always |
| `groups[]` | object | always |
| `groups[].description` | string | always |
| `groups[].id` | string | always |
| `groups[].members` | array | always |
| `groups[].members[]` | string | always |
| `groups[].name` | string | always |
| `groups[].output` | string | always |
<!-- END GENERATED schema list_groups -->


Truncated (`--limit 1`):

<!-- BEGIN GENERATED schema list_truncated -->
| Key | Type | Present |
|---|---|---|
| `hint` | string | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].env` | array | always |
| `nodes[].file` | string | always |
| `nodes[].freshness` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].kind` | string | always |
| `nodes[].line` | integer | always |
| `root` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema list_truncated -->

`nodes[].kind` is `asset`, `task` or `sensor`; `nodes[].freshness` is `always`, `manual` or
`schedule`, lowercase like `kind`. A scheduled node also has `schedule` (the cron expression) and
`next_fire` (string, `YYYY-MM-DD HH:MM:SS`): the next match of the cron expression in the local
time of the machine `list` runs on. `list` talks to no server, so the value does not follow a
server's `--timezone`; `GET /schedule` on the running server does ([Scheduling With Cron](/scheduling/)).
`list` reads no state, so it takes no `--env`.

### sql (experimental)

The query result: `columns` in order and one object per row in `rows`, keyed by column name, with
the same bounded-output envelope as `list` (`total` counts every row the query returns; `hint`
appears when `truncated`). Row values are JSON scalars, arrays or objects; dates and times are ISO
8601 strings, NaN and infinities are `null`.

With remote storage, the artifacts of the views a query names are downloaded into
`.barca/sql-cache/`; a download is reported on stderr (`barca: fetched 1 remote artifact ...`) and
the JSON is the same. A remote artifact that cannot be downloaded exits 3 (`infra`); a missing
storage driver exits 2 (`usage`).

barca's own records are tables in the `barca` schema, loaded when a query names them:
`barca.runs` (the keys of `barca history --json` runs), `barca.materializations` (one row per step
result: `node_id`, `partition`, `run_id`, `run_hash`, `status`, `created_at`, `elapsed_seconds`,
`cpu_seconds`, `max_rss_bytes`, `attempts`, `format`, `size_bytes`, `artifact`, `content_hash`,
`error_type`, `error_message`), `barca.nodes` (`barca status --json` nodes, flattened),
`barca.schedules` (one row per scheduled node: its `schedule` block), `barca.ticks` (one row per
recorded tick of a schedule) and `barca.queue` (one row per run in a server's run queue; empty
without a server, with a note on stderr). [barca sql](/reference/cli/sql/) lists their columns. `show
tables` lists the views and those tables (`barca.runs`). Their rows are the query's result like any
other; the JSON is the same.

With a server configured (or `--server <url>`), `[barca] server <url> (<source>)` is printed on
stderr, the views and barca's tables come from the server (`GET /sql/views`, `GET /history`, `GET
/materializations`, `GET /nodes`) and the query runs on this machine; the result JSON is the same.
An unreachable server exits 3 (`infra`, remediation `--local`); a project whose `[remote]` is not
the server's store exits 2. A view with no result on the server exits 2 and, when this machine
holds an unsynced result for it, says so and names `--local`. At most 10000 rows of each of
barca's tables are read from a server, newest first, with a note on stderr when there are more.

A query that names only barca's tables is answered by the server instead (`POST /sql/query`,
[barca sql: Queries the server answers](/reference/cli/sql/#queries-the-server-answers)): the result JSON is the same, this machine needs
no DuckDB, and the project's `[remote]` is not checked. The server refuses anything but one
`SELECT` over those tables, rows past 4MB, and a query that takes more than 5 seconds: exit 2
(`usage`); a fifth query while four are admitted is exit 3 (`infra`, `429`). The
query runs on this machine, as above, when it also names a result view, when files are typed
after it, with `--all` or a `--limit` above 10000, with a configured server when it names
`barca.nodes`, and when the server has no such route or no DuckDB.

Partitioned views include only current identities from full cache-aware prediction,
including cached keys across worker chunks. Removed-key history is preserved. A
current key may show its stale last successful result; an unknown derived key set
requires its source to materialize first, and zero current keys have no result
view. Excluded historical remote artifacts are not fetched for that view.

<!-- BEGIN GENERATED schema sql -->
| Key | Type | Present |
|---|---|---|
| `columns` | array | always |
| `columns[]` | string | always |
| `rows` | `<user value>` | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema sql -->

### status

Experimental `disk_usage` reports project-wide `size_bytes`, `budget_bytes`, `backup_bytes`
and `limit_bytes`. Physical size includes backups; budget excludes recognized local metadata
backup files. Budgeted bytes use maintained GC accounting, and backup measurements inspect
at most 4096 directory entries. Unknown counts are `null`. No artifact walk is added to
status, and node selection does not narrow this storage scope.
Experimental `disk_usage.metadata_trim`, present once a metadata database went over its share,
lists each environment's last trim pass and its `reason` (`busy`, `needs_gc`, `live_run`, `nothing_eligible`,
`vacuum_deferred` or null).

The status document: the same envelope keys as `list` (`total`, `truncated`, `hint`) beside
`target`, `targets`, `nodes` and `summary`. `target` is the one target given (`null` for the whole
file or several targets); `targets` lists every target given (`[]` for the whole file). `summary`
counts every node even when `nodes` is truncated.

<!-- BEGIN GENERATED schema status -->
| Key | Type | Present |
|---|---|---|
| `as_of` | string | always |
| `data_coverage` | object | always |
| `data_coverage.assets` | integer | always |
| `data_coverage.measured` | integer | always |
| `data_coverage.suspect` | integer | always |
| `disk_usage` | object | always |
| `disk_usage.backup_bytes` | integer | always |
| `disk_usage.budget_bytes` | integer | always |
| `disk_usage.limit_bytes` | integer | always |
| `disk_usage.size_bytes` | integer | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.artifact` | string | sometimes |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | sometimes |
| `nodes[].cache.state` | string | always |
| `nodes[].cause` | null \| string | always |
| `nodes[].env` | array | always |
| `nodes[].env[]` | string | always |
| `nodes[].expected_fresh_at` | null | always |
| `nodes[].gap_seconds` | integer | always |
| `nodes[].health` | string | always |
| `nodes[].health_reason` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `nodes[].last_attempt` | string | always |
| `nodes[].last_check` | null | always |
| `nodes[].last_materialization` | null \| object | always |
| `nodes[].last_materialization.artifact` | null \| string | always |
| `nodes[].last_materialization.content_hash` | string | sometimes |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | null \| number | always |
| `nodes[].last_materialization.error` | string | sometimes |
| `nodes[].last_materialization.format` | null \| string | always |
| `nodes[].last_materialization.partition` | string | sometimes |
| `nodes[].last_materialization.rows` | integer \| null | always |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | integer \| null | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].measured` | boolean \| null | always |
| `nodes[].name` | string | always |
| `nodes[].no_estimate` | null \| string | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].partitions` | object | sometimes |
| `nodes[].partitions.behind` | integer | always |
| `nodes[].partitions.cached` | integer | always |
| `nodes[].partitions.missing` | integer | always |
| `nodes[].partitions.missing_keys` | array | always |
| `nodes[].partitions.missing_keys[]` | string | always |
| `nodes[].partitions.total` | integer | always |
| `nodes[].recheck_at` | null | always |
| `nodes[].refresh_path` | null | always |
| `nodes[].root` | null \| object | always |
| `nodes[].root.id` | string | always |
| `nodes[].root.kind` | string | always |
| `nodes[].root.node` | string | always |
| `nodes[].shape` | null \| object | always |
| `nodes[].shape.columns` | array | sometimes |
| `nodes[].shape.columns[]` | object | always |
| `nodes[].shape.columns[].name` | string | always |
| `nodes[].shape.columns[].type` | string | always |
| `nodes[].shape.keys` | array | sometimes |
| `nodes[].shape.keys[]` | string | always |
| `nodes[].shape.rows` | integer | sometimes |
| `nodes[].shape.sample` | `<user value>` | always |
| `nodes[].shape.type` | string | always |
| `nodes[].signals` | array | always |
| `nodes[].stale_since` | null \| string | always |
| `nodes[].typical_run_seconds` | null \| number | always |
| `nodes[].undeclared_env` | array | sometimes |
| `nodes[].undeclared_env[]` | string | always |
| `rollup` | object | always |
| `rollup.also_in` | array | always |
| `rollup.asset_causes` | object | always |
| `rollup.asset_causes.catching_up` | integer | always |
| `rollup.asset_causes.failing` | integer | always |
| `rollup.asset_causes.no_refresh_path` | integer | always |
| `rollup.asset_causes.overdue` | integer | always |
| `rollup.asset_causes.queued` | integer | always |
| `rollup.asset_causes.undecided` | integer | always |
| `rollup.asset_health` | object | always |
| `rollup.asset_health.behind` | integer | always |
| `rollup.asset_health.blocked` | integer | always |
| `rollup.asset_health.failing` | integer | always |
| `rollup.asset_health.never_run` | integer | always |
| `rollup.asset_health.ok` | integer | always |
| `rollup.asset_health.suspect` | integer | always |
| `rollup.asset_health.undecided` | integer | always |
| `rollup.assets` | integer | always |
| `rollup.causes` | object | always |
| `rollup.causes.catching_up` | integer | always |
| `rollup.causes.failing` | integer | always |
| `rollup.causes.no_refresh_path` | integer | always |
| `rollup.causes.overdue` | integer | always |
| `rollup.causes.queued` | integer | always |
| `rollup.causes.undecided` | integer | always |
| `rollup.children` | array | always |
| `rollup.data_coverage` | object | always |
| `rollup.data_coverage.assets` | integer | always |
| `rollup.data_coverage.measured` | integer | always |
| `rollup.data_coverage.suspect` | integer | always |
| `rollup.fresh_by` | null | always |
| `rollup.gap_seconds` | integer | always |
| `rollup.health` | object | always |
| `rollup.health.behind` | integer | always |
| `rollup.health.blocked` | integer | always |
| `rollup.health.failing` | integer | always |
| `rollup.health.never_run` | integer | always |
| `rollup.health.ok` | integer | always |
| `rollup.health.suspect` | integer | always |
| `rollup.health.undecided` | integer | always |
| `rollup.held_by` | array | always |
| `rollup.held_by[]` | object | always |
| `rollup.held_by[].action_kind` | string | always |
| `rollup.held_by[].assets` | integer | always |
| `rollup.held_by[].causes` | object | always |
| `rollup.held_by[].causes.catching_up` | integer | always |
| `rollup.held_by[].causes.failing` | integer | always |
| `rollup.held_by[].causes.no_refresh_path` | integer | always |
| `rollup.held_by[].causes.overdue` | integer | always |
| `rollup.held_by[].causes.queued` | integer | always |
| `rollup.held_by[].causes.undecided` | integer | always |
| `rollup.held_by[].cumulative_nodes` | integer | always |
| `rollup.held_by[].fresh_by` | null | always |
| `rollup.held_by[].gap_seconds` | integer | always |
| `rollup.held_by[].nodes` | integer | always |
| `rollup.held_by[].root` | string | always |
| `rollup.held_by[].stale_since` | string | always |
| `rollup.held_by[].unbounded` | integer | always |
| `rollup.held_by[].weight` | integer | always |
| `rollup.id` | string | always |
| `rollup.kind` | string | always |
| `rollup.label` | string | always |
| `rollup.nodes` | integer | always |
| `rollup.not_fresh` | integer | always |
| `rollup.parent` | null | always |
| `rollup.running` | integer | always |
| `rollup.split` | object | always |
| `rollup.split.catching_up` | integer | always |
| `rollup.split.in_hand_nodes` | integer | always |
| `rollup.split.in_hand_weight` | integer | always |
| `rollup.split.need_someone` | integer | always |
| `rollup.split.need_someone_nodes` | integer | always |
| `rollup.split.need_someone_weight` | integer | always |
| `rollup.split.run_by_hand` | integer | always |
| `rollup.stale_since` | string | always |
| `rollup.unbounded` | integer | always |
| `rollup.weight` | integer | always |
| `rollup.weights` | object | always |
| `rollup.weights.catching_up` | integer | always |
| `rollup.weights.failing` | integer | always |
| `rollup.weights.fresh` | integer | always |
| `rollup.weights.no_refresh_path` | integer | always |
| `rollup.weights.overdue` | integer | always |
| `rollup.weights.queued` | integer | always |
| `rollup.weights.undecided` | integer | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.blocked` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status -->

- `cache.state` is `cached`, `unsynced`, `stale`, `never_run`, `partial`, `blocked`, `unknown`
  or `always_runs`: the same snake_case spelling as the `summary` keys (the human table prints
  `never-run`; `summary.unsynced` is always present);
  `cache.reason` is `materialized`, `local_only`, `store_only`, `mismatch`, `not_in_ledger`, `changed`, `upstream_stale`, `upstream_failed`, `failed`, `artifact_missing`,
  `evicted` (experimental), `no_record`, `partitions_missing`, `partitions_unknown`,
  `sensor_output_unknown`, `cutoff_pending` (experimental), `not_planned`, `task` or `sensor`.
- `cache.changed` (experimental, 0.29.0) appears on a `stale` node (reason `changed` or
  `upstream_stale`) whose last successful materialization recorded its run-hash components: an
  array of strings naming what differs, `code`, `decorator argument NAME`, `decorator NAME`,
  `helper FILE:NAME` (or `helper FILE`), `input ID`, `sensor ID`, `value ID`, `env NAME` or
  `kind`, in that order (`value ID`: a dict `partitions_from` source's value for the key, 0.29.6).
  With reason `changed`, `detail` is then `changed: <first>` plus ` (+N more)`. Absent for
  a result recorded before 0.29.0. A dry run's step (`steps[].changed`) carries the same list
  when its `reason` is `not_materialized` and the step succeeded before (for a partitioned
  step, its first key that ran before).
- `partitions.changed` (experimental, 0.29.6) on a partitioned node of `barca status` and on a
  partitioned step of `--dry-run`: an object from each previewed key that will run
  (`missing_keys` / `will_run_keys`) and ran before to the same list for that key. A new key is
  absent; the object is absent when no key has an entry.
- With a configured server (experimental, [Server Mode: a Shared Ledger and --server](/reference/server-mode/)), `barca status` without
  `--server` or `--local` is answered by the server and each node gets `local` (boolean: this
  machine has a copy of the result). A node the server has no result for whose result this
  machine computed and has not posted yet is `unsynced` with reason `not_in_ledger` (the table
  prints `not in ledger`); `summary.unsynced` counts `unsynced` nodes and is `0` otherwise.
  `cache.run_hash` and `cache.artifact` appear when known (`artifact` only when cached).
- `partitions` appears only on partitioned nodes. `last_materialization` is `null` when the node
  never ran; in it `partition` appears for a partitioned node and `error` for a failed run.
- `shape` is `null` unless the last materialization succeeded, and is experimental: `type`, and
  by format `rows`, `columns[]`, `keys`, `key_count`, `sample` (with `--sample`), or only `note`
  when it cannot be read.
- `schedule` (experimental, 0.29.3) appears on a node with `freshness=Schedule(...)`: `cron`,
  `next_fire`, `last_fired`, `late_since`, `paused_until` (RFC 3339 UTC or `null`), `last_status`,
  `last_run_id`, `skip_reason` (string or `null`), `failures`, `max_failures` (integers),
  `failing`, `paused`, `late` and `live` (booleans; `live` is `true` when a running scheduler
  reported it). `last_run_id`, `late` and `late_since` are new in 0.44 (#958). Its types are in
  `specs/interfaces/server-api.openapi.yaml` (`NodeSchedule`).

#### Nodes selected by cause

`barca status --cause failing --fields id,cause,root` reports matching nodes and
whole-scope totals. The health and root filters intersect with the cause filter.

<!-- BEGIN GENERATED schema status_by_cause -->
| Key | Type | Present |
|---|---|---|
| `as_of` | string | always |
| `data_coverage` | object | always |
| `data_coverage.assets` | integer | always |
| `data_coverage.measured` | integer | always |
| `data_coverage.suspect` | integer | always |
| `disk_usage` | object | always |
| `disk_usage.backup_bytes` | integer | always |
| `disk_usage.budget_bytes` | integer | always |
| `disk_usage.limit_bytes` | integer | always |
| `disk_usage.size_bytes` | integer | always |
| `filter` | object | always |
| `filter.cause` | string | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cause` | string | always |
| `nodes[].id` | string | always |
| `nodes[].root` | object | always |
| `nodes[].root.id` | string | always |
| `nodes[].root.kind` | string | always |
| `nodes[].root.node` | string | always |
| `rollup` | object | always |
| `rollup.also_in` | array | always |
| `rollup.asset_causes` | object | always |
| `rollup.asset_causes.catching_up` | integer | always |
| `rollup.asset_causes.failing` | integer | always |
| `rollup.asset_causes.no_refresh_path` | integer | always |
| `rollup.asset_causes.overdue` | integer | always |
| `rollup.asset_causes.queued` | integer | always |
| `rollup.asset_causes.undecided` | integer | always |
| `rollup.asset_health` | object | always |
| `rollup.asset_health.behind` | integer | always |
| `rollup.asset_health.blocked` | integer | always |
| `rollup.asset_health.failing` | integer | always |
| `rollup.asset_health.never_run` | integer | always |
| `rollup.asset_health.ok` | integer | always |
| `rollup.asset_health.suspect` | integer | always |
| `rollup.asset_health.undecided` | integer | always |
| `rollup.assets` | integer | always |
| `rollup.causes` | object | always |
| `rollup.causes.catching_up` | integer | always |
| `rollup.causes.failing` | integer | always |
| `rollup.causes.no_refresh_path` | integer | always |
| `rollup.causes.overdue` | integer | always |
| `rollup.causes.queued` | integer | always |
| `rollup.causes.undecided` | integer | always |
| `rollup.children` | array | always |
| `rollup.data_coverage` | object | always |
| `rollup.data_coverage.assets` | integer | always |
| `rollup.data_coverage.measured` | integer | always |
| `rollup.data_coverage.suspect` | integer | always |
| `rollup.fresh_by` | null | always |
| `rollup.gap_seconds` | integer | always |
| `rollup.health` | object | always |
| `rollup.health.behind` | integer | always |
| `rollup.health.blocked` | integer | always |
| `rollup.health.failing` | integer | always |
| `rollup.health.never_run` | integer | always |
| `rollup.health.ok` | integer | always |
| `rollup.health.suspect` | integer | always |
| `rollup.health.undecided` | integer | always |
| `rollup.held_by` | array | always |
| `rollup.held_by[]` | object | always |
| `rollup.held_by[].action_kind` | string | always |
| `rollup.held_by[].assets` | integer | always |
| `rollup.held_by[].causes` | object | always |
| `rollup.held_by[].causes.catching_up` | integer | always |
| `rollup.held_by[].causes.failing` | integer | always |
| `rollup.held_by[].causes.no_refresh_path` | integer | always |
| `rollup.held_by[].causes.overdue` | integer | always |
| `rollup.held_by[].causes.queued` | integer | always |
| `rollup.held_by[].causes.undecided` | integer | always |
| `rollup.held_by[].cumulative_nodes` | integer | always |
| `rollup.held_by[].fresh_by` | null | always |
| `rollup.held_by[].gap_seconds` | integer | always |
| `rollup.held_by[].nodes` | integer | always |
| `rollup.held_by[].root` | string | always |
| `rollup.held_by[].stale_since` | string | always |
| `rollup.held_by[].unbounded` | integer | always |
| `rollup.held_by[].weight` | integer | always |
| `rollup.id` | string | always |
| `rollup.kind` | string | always |
| `rollup.label` | string | always |
| `rollup.nodes` | integer | always |
| `rollup.not_fresh` | integer | always |
| `rollup.parent` | null | always |
| `rollup.running` | integer | always |
| `rollup.split` | object | always |
| `rollup.split.catching_up` | integer | always |
| `rollup.split.in_hand_nodes` | integer | always |
| `rollup.split.in_hand_weight` | integer | always |
| `rollup.split.need_someone` | integer | always |
| `rollup.split.need_someone_nodes` | integer | always |
| `rollup.split.need_someone_weight` | integer | always |
| `rollup.split.run_by_hand` | integer | always |
| `rollup.stale_since` | string | always |
| `rollup.unbounded` | integer | always |
| `rollup.weight` | integer | always |
| `rollup.weights` | object | always |
| `rollup.weights.catching_up` | integer | always |
| `rollup.weights.failing` | integer | always |
| `rollup.weights.fresh` | integer | always |
| `rollup.weights.no_refresh_path` | integer | always |
| `rollup.weights.overdue` | integer | always |
| `rollup.weights.queued` | integer | always |
| `rollup.weights.undecided` | integer | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.blocked` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_by_cause -->

#### Problems only

`barca status pipeline.py --problems --json` (after `broken` failed) keeps the nodes that need
attention, adds `filter`, and leaves `summary` counting every node; `--state <states>` gives
`filter.states` instead of `filter.problems`:

<!-- BEGIN GENERATED schema status_problems -->
| Key | Type | Present |
|---|---|---|
| `as_of` | string | always |
| `data_coverage` | object | always |
| `data_coverage.assets` | integer | always |
| `data_coverage.measured` | integer | always |
| `data_coverage.suspect` | integer | always |
| `disk_usage` | object | always |
| `disk_usage.backup_bytes` | integer | always |
| `disk_usage.budget_bytes` | integer | always |
| `disk_usage.limit_bytes` | integer | always |
| `disk_usage.size_bytes` | integer | always |
| `filter` | object | always |
| `filter.problems` | boolean | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | sometimes |
| `nodes[].cache.state` | string | always |
| `nodes[].cause` | string | always |
| `nodes[].env` | array | always |
| `nodes[].expected_fresh_at` | null | always |
| `nodes[].gap_seconds` | integer | always |
| `nodes[].health` | string | always |
| `nodes[].health_reason` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `nodes[].last_attempt` | string | always |
| `nodes[].last_check` | null | always |
| `nodes[].last_materialization` | null \| object | always |
| `nodes[].last_materialization.artifact` | null | always |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | null | always |
| `nodes[].last_materialization.error` | string | always |
| `nodes[].last_materialization.format` | null | always |
| `nodes[].last_materialization.rows` | null | always |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | null | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].measured` | boolean \| null | always |
| `nodes[].name` | string | always |
| `nodes[].no_estimate` | string | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].partitions` | object | sometimes |
| `nodes[].partitions.behind` | integer | always |
| `nodes[].partitions.cached` | integer | always |
| `nodes[].partitions.missing` | integer | always |
| `nodes[].partitions.missing_keys` | array | always |
| `nodes[].partitions.missing_keys[]` | string | always |
| `nodes[].partitions.total` | integer | always |
| `nodes[].recheck_at` | null | always |
| `nodes[].refresh_path` | null | always |
| `nodes[].root` | object | always |
| `nodes[].root.id` | string | always |
| `nodes[].root.kind` | string | always |
| `nodes[].root.node` | string | always |
| `nodes[].shape` | null | always |
| `nodes[].signals` | array | always |
| `nodes[].stale_since` | string | always |
| `nodes[].typical_run_seconds` | null | always |
| `rollup` | object | always |
| `rollup.also_in` | array | always |
| `rollup.asset_causes` | object | always |
| `rollup.asset_causes.catching_up` | integer | always |
| `rollup.asset_causes.failing` | integer | always |
| `rollup.asset_causes.no_refresh_path` | integer | always |
| `rollup.asset_causes.overdue` | integer | always |
| `rollup.asset_causes.queued` | integer | always |
| `rollup.asset_causes.undecided` | integer | always |
| `rollup.asset_health` | object | always |
| `rollup.asset_health.behind` | integer | always |
| `rollup.asset_health.blocked` | integer | always |
| `rollup.asset_health.failing` | integer | always |
| `rollup.asset_health.never_run` | integer | always |
| `rollup.asset_health.ok` | integer | always |
| `rollup.asset_health.suspect` | integer | always |
| `rollup.asset_health.undecided` | integer | always |
| `rollup.assets` | integer | always |
| `rollup.causes` | object | always |
| `rollup.causes.catching_up` | integer | always |
| `rollup.causes.failing` | integer | always |
| `rollup.causes.no_refresh_path` | integer | always |
| `rollup.causes.overdue` | integer | always |
| `rollup.causes.queued` | integer | always |
| `rollup.causes.undecided` | integer | always |
| `rollup.children` | array | always |
| `rollup.data_coverage` | object | always |
| `rollup.data_coverage.assets` | integer | always |
| `rollup.data_coverage.measured` | integer | always |
| `rollup.data_coverage.suspect` | integer | always |
| `rollup.fresh_by` | null | always |
| `rollup.gap_seconds` | integer | always |
| `rollup.health` | object | always |
| `rollup.health.behind` | integer | always |
| `rollup.health.blocked` | integer | always |
| `rollup.health.failing` | integer | always |
| `rollup.health.never_run` | integer | always |
| `rollup.health.ok` | integer | always |
| `rollup.health.suspect` | integer | always |
| `rollup.health.undecided` | integer | always |
| `rollup.held_by` | array | always |
| `rollup.held_by[]` | object | always |
| `rollup.held_by[].action_kind` | string | always |
| `rollup.held_by[].assets` | integer | always |
| `rollup.held_by[].causes` | object | always |
| `rollup.held_by[].causes.catching_up` | integer | always |
| `rollup.held_by[].causes.failing` | integer | always |
| `rollup.held_by[].causes.no_refresh_path` | integer | always |
| `rollup.held_by[].causes.overdue` | integer | always |
| `rollup.held_by[].causes.queued` | integer | always |
| `rollup.held_by[].causes.undecided` | integer | always |
| `rollup.held_by[].cumulative_nodes` | integer | always |
| `rollup.held_by[].fresh_by` | null | always |
| `rollup.held_by[].gap_seconds` | integer | always |
| `rollup.held_by[].nodes` | integer | always |
| `rollup.held_by[].root` | string | always |
| `rollup.held_by[].stale_since` | string | always |
| `rollup.held_by[].unbounded` | integer | always |
| `rollup.held_by[].weight` | integer | always |
| `rollup.id` | string | always |
| `rollup.kind` | string | always |
| `rollup.label` | string | always |
| `rollup.nodes` | integer | always |
| `rollup.not_fresh` | integer | always |
| `rollup.parent` | null | always |
| `rollup.running` | integer | always |
| `rollup.split` | object | always |
| `rollup.split.catching_up` | integer | always |
| `rollup.split.in_hand_nodes` | integer | always |
| `rollup.split.in_hand_weight` | integer | always |
| `rollup.split.need_someone` | integer | always |
| `rollup.split.need_someone_nodes` | integer | always |
| `rollup.split.need_someone_weight` | integer | always |
| `rollup.split.run_by_hand` | integer | always |
| `rollup.stale_since` | string | always |
| `rollup.unbounded` | integer | always |
| `rollup.weight` | integer | always |
| `rollup.weights` | object | always |
| `rollup.weights.catching_up` | integer | always |
| `rollup.weights.failing` | integer | always |
| `rollup.weights.fresh` | integer | always |
| `rollup.weights.no_refresh_path` | integer | always |
| `rollup.weights.overdue` | integer | always |
| `rollup.weights.queued` | integer | always |
| `rollup.weights.undecided` | integer | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.blocked` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_problems -->

#### The rollup

`barca status pipeline.py --rollup --json` (after `broken` failed) prints the rollup instead of
the nodes (experimental; [barca status: Fields and JSON: The rollup](/reference/cli/status-fields/#the-rollup)). `entries[].keys` is on a
partitioned node's entry only; `roots[].action.estimated_seconds` is an integer when a node
the root holds has a recorded duration, and `roots[].action.command` a string when a command
clears the root:

<!-- BEGIN GENERATED schema status_rollup -->
| Key | Type | Present |
|---|---|---|
| `as_of` | string | always |
| `data_coverage` | object | always |
| `data_coverage.assets` | integer | always |
| `data_coverage.measured` | integer | always |
| `data_coverage.suspect` | integer | always |
| `entries` | array | always |
| `entries[]` | object | always |
| `entries[].also_in` | array | always |
| `entries[].asset_causes` | object | always |
| `entries[].asset_causes.catching_up` | integer | always |
| `entries[].asset_causes.failing` | integer | always |
| `entries[].asset_causes.no_refresh_path` | integer | always |
| `entries[].asset_causes.overdue` | integer | always |
| `entries[].asset_causes.queued` | integer | always |
| `entries[].asset_causes.undecided` | integer | always |
| `entries[].asset_health` | object | always |
| `entries[].asset_health.behind` | integer | always |
| `entries[].asset_health.blocked` | integer | always |
| `entries[].asset_health.failing` | integer | always |
| `entries[].asset_health.never_run` | integer | always |
| `entries[].asset_health.ok` | integer | always |
| `entries[].asset_health.suspect` | integer | always |
| `entries[].asset_health.undecided` | integer | always |
| `entries[].assets` | integer | always |
| `entries[].causes` | object | always |
| `entries[].causes.catching_up` | integer | always |
| `entries[].causes.failing` | integer | always |
| `entries[].causes.no_refresh_path` | integer | always |
| `entries[].causes.overdue` | integer | always |
| `entries[].causes.queued` | integer | always |
| `entries[].causes.undecided` | integer | always |
| `entries[].children` | array | always |
| `entries[].children[]` | string | always |
| `entries[].data_coverage` | object | always |
| `entries[].data_coverage.assets` | integer | always |
| `entries[].data_coverage.measured` | integer | always |
| `entries[].data_coverage.suspect` | integer | always |
| `entries[].fresh_by` | null | always |
| `entries[].gap_seconds` | integer | always |
| `entries[].health` | object | always |
| `entries[].health.behind` | integer | always |
| `entries[].health.blocked` | integer | always |
| `entries[].health.failing` | integer | always |
| `entries[].health.never_run` | integer | always |
| `entries[].health.ok` | integer | always |
| `entries[].health.suspect` | integer | always |
| `entries[].health.undecided` | integer | always |
| `entries[].held_by` | array | always |
| `entries[].held_by[]` | object | always |
| `entries[].held_by[].action_kind` | string | always |
| `entries[].held_by[].assets` | integer | always |
| `entries[].held_by[].causes` | object | always |
| `entries[].held_by[].causes.catching_up` | integer | always |
| `entries[].held_by[].causes.failing` | integer | always |
| `entries[].held_by[].causes.no_refresh_path` | integer | always |
| `entries[].held_by[].causes.overdue` | integer | always |
| `entries[].held_by[].causes.queued` | integer | always |
| `entries[].held_by[].causes.undecided` | integer | always |
| `entries[].held_by[].cumulative_nodes` | integer | always |
| `entries[].held_by[].fresh_by` | null | always |
| `entries[].held_by[].gap_seconds` | integer | always |
| `entries[].held_by[].nodes` | integer | always |
| `entries[].held_by[].root` | string | always |
| `entries[].held_by[].stale_since` | string | always |
| `entries[].held_by[].unbounded` | integer | always |
| `entries[].held_by[].weight` | integer | always |
| `entries[].id` | string | always |
| `entries[].keys` | object | sometimes |
| `entries[].keys.behind` | integer | always |
| `entries[].keys.cause` | null \| string | always |
| `entries[].keys.failed` | integer | always |
| `entries[].keys.ok` | integer | always |
| `entries[].kind` | string | always |
| `entries[].label` | string | always |
| `entries[].nodes` | integer | always |
| `entries[].not_fresh` | integer | always |
| `entries[].parent` | null \| string | always |
| `entries[].running` | integer | always |
| `entries[].split` | object | always |
| `entries[].split.catching_up` | integer | always |
| `entries[].split.in_hand_nodes` | integer | always |
| `entries[].split.in_hand_weight` | integer | always |
| `entries[].split.need_someone` | integer | always |
| `entries[].split.need_someone_nodes` | integer | always |
| `entries[].split.need_someone_weight` | integer | always |
| `entries[].split.run_by_hand` | integer | always |
| `entries[].stale_since` | null \| string | always |
| `entries[].unbounded` | integer | always |
| `entries[].weight` | integer | always |
| `entries[].weights` | object | always |
| `entries[].weights.catching_up` | integer | always |
| `entries[].weights.failing` | integer | always |
| `entries[].weights.fresh` | integer | always |
| `entries[].weights.no_refresh_path` | integer | always |
| `entries[].weights.overdue` | integer | always |
| `entries[].weights.queued` | integer | always |
| `entries[].weights.undecided` | integer | always |
| `files` | array | always |
| `files[]` | object | always |
| `files[].asset_health` | object | always |
| `files[].asset_health.behind` | integer | always |
| `files[].asset_health.blocked` | integer | always |
| `files[].asset_health.failing` | integer | always |
| `files[].asset_health.never_run` | integer | always |
| `files[].asset_health.ok` | integer | always |
| `files[].asset_health.suspect` | integer | always |
| `files[].asset_health.undecided` | integer | always |
| `files[].assets` | integer | always |
| `files[].causes` | object | always |
| `files[].causes.catching_up` | integer | always |
| `files[].causes.failing` | integer | always |
| `files[].causes.no_refresh_path` | integer | always |
| `files[].causes.overdue` | integer | always |
| `files[].causes.queued` | integer | always |
| `files[].causes.undecided` | integer | always |
| `files[].file` | string | always |
| `files[].fresh` | integer | always |
| `files[].health` | object | always |
| `files[].health.behind` | integer | always |
| `files[].health.blocked` | integer | always |
| `files[].health.failing` | integer | always |
| `files[].health.never_run` | integer | always |
| `files[].health.ok` | integer | always |
| `files[].health.suspect` | integer | always |
| `files[].health.undecided` | integer | always |
| `files[].kinds` | object | always |
| `files[].kinds.asset` | object | always |
| `files[].kinds.asset.causes` | object | always |
| `files[].kinds.asset.causes.catching_up` | integer | always |
| `files[].kinds.asset.causes.failing` | integer | always |
| `files[].kinds.asset.causes.no_refresh_path` | integer | always |
| `files[].kinds.asset.causes.overdue` | integer | always |
| `files[].kinds.asset.causes.queued` | integer | always |
| `files[].kinds.asset.causes.undecided` | integer | always |
| `files[].kinds.asset.fresh` | integer | always |
| `files[].kinds.asset.nodes` | integer | always |
| `files[].kinds.sensor` | object | always |
| `files[].kinds.sensor.causes` | object | always |
| `files[].kinds.sensor.causes.catching_up` | integer | always |
| `files[].kinds.sensor.causes.failing` | integer | always |
| `files[].kinds.sensor.causes.no_refresh_path` | integer | always |
| `files[].kinds.sensor.causes.overdue` | integer | always |
| `files[].kinds.sensor.causes.queued` | integer | always |
| `files[].kinds.sensor.causes.undecided` | integer | always |
| `files[].kinds.sensor.fresh` | integer | always |
| `files[].kinds.sensor.nodes` | integer | always |
| `files[].kinds.task` | object | always |
| `files[].kinds.task.causes` | object | always |
| `files[].kinds.task.causes.catching_up` | integer | always |
| `files[].kinds.task.causes.failing` | integer | always |
| `files[].kinds.task.causes.no_refresh_path` | integer | always |
| `files[].kinds.task.causes.overdue` | integer | always |
| `files[].kinds.task.causes.queued` | integer | always |
| `files[].kinds.task.causes.undecided` | integer | always |
| `files[].kinds.task.fresh` | integer | always |
| `files[].kinds.task.nodes` | integer | always |
| `files[].nodes` | integer | always |
| `files[].running` | integer | always |
| `files[].worst` | string | always |
| `kinds` | object | always |
| `kinds.asset` | object | always |
| `kinds.asset.causes` | object | always |
| `kinds.asset.causes.catching_up` | integer | always |
| `kinds.asset.causes.failing` | integer | always |
| `kinds.asset.causes.no_refresh_path` | integer | always |
| `kinds.asset.causes.overdue` | integer | always |
| `kinds.asset.causes.queued` | integer | always |
| `kinds.asset.causes.undecided` | integer | always |
| `kinds.asset.fresh` | integer | always |
| `kinds.asset.nodes` | integer | always |
| `kinds.sensor` | object | always |
| `kinds.sensor.causes` | object | always |
| `kinds.sensor.causes.catching_up` | integer | always |
| `kinds.sensor.causes.failing` | integer | always |
| `kinds.sensor.causes.no_refresh_path` | integer | always |
| `kinds.sensor.causes.overdue` | integer | always |
| `kinds.sensor.causes.queued` | integer | always |
| `kinds.sensor.causes.undecided` | integer | always |
| `kinds.sensor.fresh` | integer | always |
| `kinds.sensor.nodes` | integer | always |
| `kinds.task` | object | always |
| `kinds.task.causes` | object | always |
| `kinds.task.causes.catching_up` | integer | always |
| `kinds.task.causes.failing` | integer | always |
| `kinds.task.causes.no_refresh_path` | integer | always |
| `kinds.task.causes.overdue` | integer | always |
| `kinds.task.causes.queued` | integer | always |
| `kinds.task.causes.undecided` | integer | always |
| `kinds.task.fresh` | integer | always |
| `kinds.task.nodes` | integer | always |
| `roots` | array | always |
| `roots[]` | object | always |
| `roots[].action` | object | always |
| `roots[].action.command` | null \| string | always |
| `roots[].action.estimated_seconds` | null | always |
| `roots[].action.kind` | string | always |
| `roots[].action.run_once` | null \| string | always |
| `roots[].assets` | integer | always |
| `roots[].causes` | object | always |
| `roots[].causes.catching_up` | integer | always |
| `roots[].causes.failing` | integer | always |
| `roots[].causes.no_refresh_path` | integer | always |
| `roots[].causes.overdue` | integer | always |
| `roots[].causes.queued` | integer | always |
| `roots[].causes.undecided` | integer | always |
| `roots[].detail` | string | always |
| `roots[].error_group` | null \| string | always |
| `roots[].fresh_by` | null | always |
| `roots[].gap_seconds` | integer | always |
| `roots[].id` | string | always |
| `roots[].kind` | string | always |
| `roots[].needs_someone` | boolean | always |
| `roots[].node` | string | always |
| `roots[].nodes` | integer | always |
| `roots[].stale_since` | string | always |
| `roots[].unbounded` | integer | always |
| `roots[].weight` | integer | always |
<!-- END GENERATED schema status_rollup -->

#### One node's keys

After one key of the partitioned `flaky` failed (experimental; [Partitions](/reference/partitions/),
"Listing keys"). `barca status flaky pipeline.py --keys --json` lists its keys, each with its
state and latest attempt, what `GET /assets/{name}/partitions/keys` answers; `hint` appears
when `truncated` is true:

<!-- BEGIN GENERATED schema status_keys -->
| Key | Type | Present |
|---|---|---|
| `counts` | object | always |
| `counts.blocked` | integer | always |
| `counts.empty` | integer | always |
| `counts.failed` | integer | always |
| `counts.fresh` | integer | always |
| `counts.never_run` | integer | always |
| `counts.pinned` | integer | always |
| `counts.stale` | integer | always |
| `dimensions` | array | always |
| `dimensions[]` | string | always |
| `errors` | array | always |
| `errors[]` | object | always |
| `errors[].error` | string | always |
| `errors[].keys` | integer | always |
| `id` | string | always |
| `keys` | array | always |
| `keys[]` | object | always |
| `keys[].attempted_at` | string | always |
| `keys[].elapsed_seconds` | null \| number | always |
| `keys[].error` | null \| string | always |
| `keys[].key` | string | always |
| `keys[].rows` | null | always |
| `keys[].run_id` | string | always |
| `keys[].size_bytes` | integer \| null | always |
| `keys[].state` | string | always |
| `keys[].values` | array | always |
| `keys[].values[]` | string | always |
| `known` | boolean | always |
| `needs_run` | integer | always |
| `offset` | integer | always |
| `selection` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_keys -->

`barca status flaky pipeline.py --keys --state failed --value k=bad --error 0 --match BAD
--sort attempted --limit 1 --json` is the same document for the keys the filters keep (every
filter must hold): `total` and `needs_run` count what matches, `counts` every key, and
`selection` names what matches as `--partition` takes it:

<!-- BEGIN GENERATED schema status_keys_filtered -->
| Key | Type | Present |
|---|---|---|
| `counts` | object | always |
| `counts.blocked` | integer | always |
| `counts.empty` | integer | always |
| `counts.failed` | integer | always |
| `counts.fresh` | integer | always |
| `counts.never_run` | integer | always |
| `counts.pinned` | integer | always |
| `counts.stale` | integer | always |
| `dimensions` | array | always |
| `dimensions[]` | string | always |
| `errors` | array | always |
| `errors[]` | object | always |
| `errors[].error` | string | always |
| `errors[].keys` | integer | always |
| `id` | string | always |
| `keys` | array | always |
| `keys[]` | object | always |
| `keys[].attempted_at` | string | always |
| `keys[].elapsed_seconds` | null | always |
| `keys[].error` | string | always |
| `keys[].key` | string | always |
| `keys[].rows` | null | always |
| `keys[].run_id` | string | always |
| `keys[].size_bytes` | null | always |
| `keys[].state` | string | always |
| `keys[].values` | array | always |
| `keys[].values[]` | string | always |
| `known` | boolean | always |
| `needs_run` | integer | always |
| `offset` | integer | always |
| `selection` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_keys_filtered -->

`barca status flaky pipeline.py --overview --json` is the node's keys in one picture, what
`GET /assets/{name}/partitions/overview` answers: the counts, what is wrong and where
(`issues`) and the grid's rows along its longest dimension (`rows`); `hint` appears when
`truncated` is true:

<!-- BEGIN GENERATED schema status_overview -->
| Key | Type | Present |
|---|---|---|
| `along` | string | always |
| `chain` | array | always |
| `chain[]` | string | always |
| `counts` | object | always |
| `counts.blocked` | integer | always |
| `counts.empty` | integer | always |
| `counts.failed` | integer | always |
| `counts.fresh` | integer | always |
| `counts.never_run` | integer | always |
| `counts.pinned` | integer | always |
| `counts.stale` | integer | always |
| `dimensions` | array | always |
| `dimensions[]` | object | always |
| `dimensions[].name` | string | always |
| `dimensions[].values` | array | always |
| `dimensions[].values[]` | string | always |
| `errors` | array | always |
| `errors[]` | object | always |
| `errors[].error` | string | always |
| `errors[].keys` | integer | always |
| `id` | string | always |
| `issues` | array | always |
| `issues[]` | object | always |
| `issues[].error` | integer | always |
| `issues[].keys` | integer | always |
| `issues[].places` | array | always |
| `issues[].places[]` | object | always |
| `issues[].places[].dimension` | string | always |
| `issues[].places[].words` | string | always |
| `issues[].selection` | string | always |
| `issues[].state` | string | always |
| `issues[].title` | string | always |
| `known` | boolean | always |
| `last_attempt_at` | string | always |
| `needs_run` | integer | always |
| `rows` | `<user value>` | always |
| `rows_total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_overview -->

`barca status flaky pipeline.py --grid --json` is every key in columns, what
`GET /assets/{name}/partitions/grid` answers. `error` maps a failed cell's index to an index
into `errors`:

<!-- BEGIN GENERATED schema status_grid -->
| Key | Type | Present |
|---|---|---|
| `attempted_at` | array | always |
| `attempted_at[]` | integer | always |
| `attempted_base` | integer | always |
| `chain` | array | always |
| `chain[]` | string | always |
| `counts` | object | always |
| `counts.blocked` | integer | always |
| `counts.empty` | integer | always |
| `counts.failed` | integer | always |
| `counts.fresh` | integer | always |
| `counts.never_run` | integer | always |
| `counts.pinned` | integer | always |
| `counts.stale` | integer | always |
| `dimensions` | array | always |
| `dimensions[]` | object | always |
| `dimensions[].name` | string | always |
| `dimensions[].values` | array | always |
| `dimensions[].values[]` | string | always |
| `elapsed_ms` | array | always |
| `elapsed_ms[]` | integer \| null | always |
| `error` | object | always |
| `error.<name>` | integer | always |
| `errors` | array | always |
| `errors[]` | object | always |
| `errors[].error` | string | always |
| `errors[].keys` | integer | always |
| `id` | string | always |
| `known` | boolean | always |
| `rows` | `<user value>` | always |
| `run` | array | always |
| `run[]` | integer | always |
| `runs` | array | always |
| `runs[]` | string | always |
| `size_bytes` | array | always |
| `size_bytes[]` | integer \| null | always |
| `states` | string | always |
<!-- END GENERATED schema status_grid -->

`barca status flaky pipeline.py --key k=ok --json` is one key, what
`GET /assets/{name}/partitions/key` answers. `output` is `null` for a key with no successful
result, `inputs[]` has the keys of `output`, and `shape` is the artifact's shape as in
`barca status --json`:

<!-- BEGIN GENERATED schema status_key -->
| Key | Type | Present |
|---|---|---|
| `attempts` | array | always |
| `attempts[]` | object | always |
| `attempts[].artifact_path` | string | always |
| `attempts[].created_at` | string | always |
| `attempts[].elapsed_seconds` | number | always |
| `attempts[].error` | null | always |
| `attempts[].format` | string | always |
| `attempts[].rows` | null | always |
| `attempts[].run_id` | string | always |
| `attempts[].size_bytes` | integer | always |
| `attempts[].status` | string | always |
| `chain` | array | always |
| `chain[]` | object | always |
| `chain[].id` | string | always |
| `chain[].state` | string | always |
| `id` | string | always |
| `inputs` | array | always |
| `key` | string | always |
| `needs_run` | boolean | always |
| `output` | object | always |
| `output.artifact_path` | string | always |
| `output.format` | string | always |
| `output.id` | string | always |
| `output.shape` | object | always |
| `output.shape.columns` | array | always |
| `output.shape.columns[]` | object | always |
| `output.shape.columns[].name` | string | always |
| `output.shape.columns[].type` | string | always |
| `output.shape.keys` | array | always |
| `output.shape.keys[]` | string | always |
| `output.shape.type` | string | always |
| `output.size_bytes` | integer | always |
| `output.step` | string | always |
| `state` | string | always |
<!-- END GENERATED schema status_key -->

#### What changed

After `total`'s input `numbers` changed its code, `barca status pipeline.py --json`:

<!-- BEGIN GENERATED schema status_changed -->
| Key | Type | Present |
|---|---|---|
| `as_of` | string | always |
| `data_coverage` | object | always |
| `data_coverage.assets` | integer | always |
| `data_coverage.measured` | integer | always |
| `data_coverage.suspect` | integer | always |
| `disk_usage` | object | always |
| `disk_usage.backup_bytes` | integer | always |
| `disk_usage.budget_bytes` | integer | always |
| `disk_usage.limit_bytes` | integer | always |
| `disk_usage.size_bytes` | integer | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.change_causes` | array | sometimes |
| `nodes[].cache.change_causes[]` | string | always |
| `nodes[].cache.changed` | array | sometimes |
| `nodes[].cache.changed[]` | string | always |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | always |
| `nodes[].cache.state` | string | always |
| `nodes[].cause` | string | always |
| `nodes[].env` | array | always |
| `nodes[].expected_fresh_at` | null | always |
| `nodes[].gap_seconds` | integer | always |
| `nodes[].health` | string | always |
| `nodes[].health_reason` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].inputs[]` | string | always |
| `nodes[].kind` | string | always |
| `nodes[].last_attempt` | string | always |
| `nodes[].last_check` | null | always |
| `nodes[].last_materialization` | null \| object | always |
| `nodes[].last_materialization.artifact` | string | always |
| `nodes[].last_materialization.content_hash` | string | always |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | number | always |
| `nodes[].last_materialization.format` | string | always |
| `nodes[].last_materialization.rows` | null | always |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | integer | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].measured` | boolean | always |
| `nodes[].name` | string | always |
| `nodes[].no_estimate` | string | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].recheck_at` | null | always |
| `nodes[].refresh_path` | null | always |
| `nodes[].root` | object | always |
| `nodes[].root.id` | string | always |
| `nodes[].root.kind` | string | always |
| `nodes[].root.node` | string | always |
| `nodes[].shape` | null \| object | always |
| `nodes[].shape.keys` | array | sometimes |
| `nodes[].shape.keys[]` | string | always |
| `nodes[].shape.rows` | integer | sometimes |
| `nodes[].shape.type` | string | always |
| `nodes[].signals` | array | always |
| `nodes[].stale_since` | string | always |
| `nodes[].typical_run_seconds` | null \| number | always |
| `rollup` | object | always |
| `rollup.also_in` | array | always |
| `rollup.asset_causes` | object | always |
| `rollup.asset_causes.catching_up` | integer | always |
| `rollup.asset_causes.failing` | integer | always |
| `rollup.asset_causes.no_refresh_path` | integer | always |
| `rollup.asset_causes.overdue` | integer | always |
| `rollup.asset_causes.queued` | integer | always |
| `rollup.asset_causes.undecided` | integer | always |
| `rollup.asset_health` | object | always |
| `rollup.asset_health.behind` | integer | always |
| `rollup.asset_health.blocked` | integer | always |
| `rollup.asset_health.failing` | integer | always |
| `rollup.asset_health.never_run` | integer | always |
| `rollup.asset_health.ok` | integer | always |
| `rollup.asset_health.suspect` | integer | always |
| `rollup.asset_health.undecided` | integer | always |
| `rollup.assets` | integer | always |
| `rollup.causes` | object | always |
| `rollup.causes.catching_up` | integer | always |
| `rollup.causes.failing` | integer | always |
| `rollup.causes.no_refresh_path` | integer | always |
| `rollup.causes.overdue` | integer | always |
| `rollup.causes.queued` | integer | always |
| `rollup.causes.undecided` | integer | always |
| `rollup.children` | array | always |
| `rollup.data_coverage` | object | always |
| `rollup.data_coverage.assets` | integer | always |
| `rollup.data_coverage.measured` | integer | always |
| `rollup.data_coverage.suspect` | integer | always |
| `rollup.fresh_by` | null | always |
| `rollup.gap_seconds` | integer | always |
| `rollup.health` | object | always |
| `rollup.health.behind` | integer | always |
| `rollup.health.blocked` | integer | always |
| `rollup.health.failing` | integer | always |
| `rollup.health.never_run` | integer | always |
| `rollup.health.ok` | integer | always |
| `rollup.health.suspect` | integer | always |
| `rollup.health.undecided` | integer | always |
| `rollup.held_by` | array | always |
| `rollup.held_by[]` | object | always |
| `rollup.held_by[].action_kind` | string | always |
| `rollup.held_by[].assets` | integer | always |
| `rollup.held_by[].causes` | object | always |
| `rollup.held_by[].causes.catching_up` | integer | always |
| `rollup.held_by[].causes.failing` | integer | always |
| `rollup.held_by[].causes.no_refresh_path` | integer | always |
| `rollup.held_by[].causes.overdue` | integer | always |
| `rollup.held_by[].causes.queued` | integer | always |
| `rollup.held_by[].causes.undecided` | integer | always |
| `rollup.held_by[].cumulative_nodes` | integer | always |
| `rollup.held_by[].fresh_by` | null | always |
| `rollup.held_by[].gap_seconds` | integer | always |
| `rollup.held_by[].nodes` | integer | always |
| `rollup.held_by[].root` | string | always |
| `rollup.held_by[].stale_since` | string | always |
| `rollup.held_by[].unbounded` | integer | always |
| `rollup.held_by[].weight` | integer | always |
| `rollup.id` | string | always |
| `rollup.kind` | string | always |
| `rollup.label` | string | always |
| `rollup.nodes` | integer | always |
| `rollup.not_fresh` | integer | always |
| `rollup.parent` | null | always |
| `rollup.running` | integer | always |
| `rollup.split` | object | always |
| `rollup.split.catching_up` | integer | always |
| `rollup.split.in_hand_nodes` | integer | always |
| `rollup.split.in_hand_weight` | integer | always |
| `rollup.split.need_someone` | integer | always |
| `rollup.split.need_someone_nodes` | integer | always |
| `rollup.split.need_someone_weight` | integer | always |
| `rollup.split.run_by_hand` | integer | always |
| `rollup.stale_since` | string | always |
| `rollup.unbounded` | integer | always |
| `rollup.weight` | integer | always |
| `rollup.weights` | object | always |
| `rollup.weights.catching_up` | integer | always |
| `rollup.weights.failing` | integer | always |
| `rollup.weights.fresh` | integer | always |
| `rollup.weights.no_refresh_path` | integer | always |
| `rollup.weights.overdue` | integer | always |
| `rollup.weights.queued` | integer | always |
| `rollup.weights.undecided` | integer | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.blocked` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_changed -->

and `barca get total pipeline.py --dry-run --json`:

<!-- BEGIN GENERATED schema get_dry_run_changed -->
| Key | Type | Present |
|---|---|---|
| `capacity` | integer | always |
| `command` | string | always |
| `dry_run` | boolean | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].action` | string | always |
| `steps[].changed` | array | always |
| `steps[].changed[]` | string | always |
| `steps[].detail` | string | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `summary` | object | always |
| `summary.cached` | integer | always |
| `summary.unknown` | integer | always |
| `summary.will_run` | integer | always |
| `target` | string | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_dry_run_changed -->

### state backups, state restore and state reset

`barca state backups` prints `{backups, local_dir, store}` and, when the store could not be
listed, `store_error` (a string; the local backups are still listed and stderr has a warning).
`store` is the store's backup prefix with credentials redacted, or `null` without a store.
Each entry of `backups`, newest first: `index` (integer, 1 = newest), `name`
(`2026-10-14T18-02-11.482Z-3f9a1c2e.db`; `2026-10-14T18-02-11Z.db` from 0.26.0), `where`
(`local`, `store` or `both`), `size_bytes` (integer),
`run_id` (string or `null`), `rows` and `db_format` (integers or `null`), `digest` and
`barca_version` (strings or `null`; from the backup's sidecar), `agree` (boolean when the
backup is in both places, else `null`), and, additive in 0.29, `host` and `project` (strings,
the host and absolute project directory that took it) and `mine` (boolean: this host and
project directory), all `null` for a backup whose sidecar predates them. No backups is an empty
array, exit 0. A backup another host or project directory took after this one's newest adds a
`[barca] warning: ...` line on stderr (not contract); the exit code is unchanged.

`barca state restore [--from N|NAME|PATH]` puts a backup (or, with a path, any barca database
file, such as one `barca state reset` moved aside) in place of the metadata DB and moves the
current one to `metadata.db.pre-restore`. Without `--from` it takes this host and project
directory's newest backup (`mine`), or the newest when there is none (from 0.29; before, always
the newest), with a `[barca] restoring ...` line on stderr when that is not the newest. One
`[barca] restored ...` line on stderr (not contract); with `--json` (or piped), stdout has
`{restored, from, source, pre_restore, rows, run_id, host, project}`: `from` is `local`,
`store` or `file` (`--from PATH`; `rows`, `run_id`, `host` and `project` are then `null`),
`source` the copy's path or redacted URI,
`pre_restore` the path of the replaced database or `null`. Exit 2 (`usage`): no backup, an
unknown `--from`, a copy that fails validation (the message names the next newest), a copy in a
format this barca cannot use. Exit 3 (`infra`): a `barca serve` holds the environment, or no
local backup exists and the store cannot be listed. Nothing is changed on any error.
Stability: stable.

`barca state reset [--yes]` moves the metadata DB and its `-wal` and `-shm` aside to
`metadata.db.reset-<UTC>` (and `<that>-wal`, `<that>-shm`) in the same directory and creates an
empty one; nothing is deleted. Without `--yes` it asks on stderr when stdin is a terminal (an
answer other than `y` changes nothing, exit 0). One `[barca] reset the metadata database ...`
line on stderr (not contract); with `--json` (or piped), stdout has `{database, moved_to, moved,
undo}`: `database` the path of the new empty DB, `moved_to` the moved file's path or `null` when
there was no DB, `moved` an array of every moved path (the DB first), `undo` the
`barca state restore --from ...` command line or `null`. Exit 2 (`usage`): no `--yes` and stdin
is not a terminal. Exit 3 (`infra`): a run is in progress on this machine (the message names it),
or a `barca serve` holds the environment (use `POST /state/reset`). Nothing is changed on any
error. Stability: experimental.

### history

`runs[].attempts` is the sum of attempts in its recorded materialization rows, including
executed partition keys; an all-cached run has zero. Upload-failure rows recorded by
earlier versions (a failed upload no longer fails its step) retain their transfer-attempt counts. `stats.recent_runs[].attempts` continues to report
attempts per materialization, with the stored final-attempt duration.

Experimental `--reconcile RUN_ID` is an explicit write after the operator verifies the run
stopped. It returns just that run in the existing envelope (`total: 1`, `truncated: false`),
recorded as `interrupted`. It refuses a proven live owner, a non-running row or unknown id
(exit 2); database errors are exit 3. `--limit` and `--all` conflict with it; `--fields`,
output selection and `--env` still apply. Successful step rows/artifacts and progress are
preserved, and an unknown end time/duration is not invented. Ordinary history stays read-only.

`runs[].triggered_by` says what started a run: `manual` (a command typed on a machine), `api`
(an HTTP request to `barca serve`), `schedule` (a cron tick) or `client` (a run a laptop posted
to a server's ledger); `null` for runs recorded before barca recorded it. A scheduled run also
has `scheduled_at` (the tick, RFC 3339 in UTC) and `schedule_id` (the scheduled node); both are
`null` on every other run. The three keys are additive. `runs[].client_run_id` is the run id
a `client` run had on the laptop that posted it ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)), `null` on every other
run; additive and experimental.

`runs[].line` (experimental, #959) is the run in the words of one row of a list, the same the
web UI's run list prints: `steps` (the steps by outcome, `7 ok · 1 failed · 0 cached`),
`steps_detail` (what that counts, the steps planned and where the run stopped), `failure`
(what went wrong in one line, `null` when nothing did) and `failure_detail` (the same in full).

`runs[].principal` and `runs[].trigger_reason` (experimental, #925) say who asked a server for
the run and why: the project token its `POST /runs` was authenticated with, as `token:<name>`
(`null` without authentication, and for every run not started over HTTP), and the free-text
`reason` the request gave (`null` when it gave none). `runs[].cancellation` (experimental,
#925) is `null` except on a run a server cancelled: `{by, principal, started, waiting_behind,
same_as}`, where `by` is `request` (`DELETE /runs/{run_id}`: the web UI's Cancel, or the first
Ctrl-C/SIGTERM of a `--server` client) or `shutdown` (the server stopped), `principal` the
token of the cancel request, `started` is `false` for a run cancelled while it waited in the
queue (it executed nothing: 0 steps, 0 seconds), `waiting_behind` the runs executing then (at
most 10) and `same_as` the one of them with the same command, targets and files, if any. barca
does not deduplicate requests: two identical requests are two runs, and the second one waits
or shares in-flight steps with the first ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)).

`runs[].job` (additive, 0.29.0) is what the run was for: `all` for a run without targets,
otherwise its resolved node ids, sorted and comma-separated (`barca.job` in telemetry);
`null` for runs recorded before barca stored it. `target` stays the target as typed. The human
table's TARGET column shows the job by function name, cut at 24 characters with `…`.

`runs[].failure` (additive, 0.29.3) is `{node, error}` for a run with a failed step: the first
failed step's node id and the first line of its error (`ZeroDivisionError: division by zero`);
`null` for every other run. The human table ends a failed run's row with
`<function>: <error>`, cut at 72 characters with `…`.

Experimental filters (0.29.3): `--status` (comma-separated: `success`, `failed`, `cancelled`,
`error`, `running`, `interrupted`; `not_successful`, 0.44, is every one but `success` and
`running`), `--target NAME` (the run's target or job names the node), `--triggered-by`
(comma-separated: `manual`, `api`, `schedule`, `client`) and `--since` (a duration
`30m`/`24h`/`7d`/`2w`, an RFC 3339 time or a UTC date). Added in 0.44 (#969):
`--not-triggered-by` (the same values, left out; a run that recorded no trigger is kept),
`--until` (started before a time, written as `--since` is) and `--search TEXT` (the run id,
target or job contains the text, whatever its case). They combine with AND; `total` and
`truncated` count the runs they keep and `--limit` applies after them. An unknown value is a
usage error (exit 2) naming the valid ones. Against a server they are the query parameters
`status`, `target`, `triggered_by`, `not_triggered_by`, `since`, `until` and `search` of
`GET /history` (a 400 for an unknown value), and all but `target` are also parameters of
`GET /runs`.

Experimental (0.44, #991): when `--status` names `running`, a listed run in progress has
`now`, what it is working on: `running[]` (`node_id`, `keys`, `share`), `queued`, `keys`
(`{done, total}` or `null`), `groups[]` (`id`, `group`, `running`, `queued`) and `line`; the
table prints `  now: <line>` under its row. It is absent on every other run and without
`--status running`, and `--fields now` selects it. `barca history --activity` prints
`{activity}` instead of `{runs, total, truncated}`: `since`, `until`, `triggered_by`,
`not_triggered_by`, `statuses`, `search`, `hours[]` (24 entries: `at`, `until`, `runs`, `ok`,
`not_successful`, `in_progress`, `by_status`, `by_trigger`, `median_seconds`, `failure`
`{text, runs}` or `null`), `total` (one such entry for the window), `excluded` (one such entry
for the runs `--not-triggered-by` left out; `null` without it) and `summary`. It takes
`--status`, `--triggered-by`, `--not-triggered-by` and `--search`, and no other filter, limit
or `--fields` (exit 2). Semantics and limits: [barca status](/reference/cli/status/).

Experimental `barca history RUN_ID` (0.29.3) prints one run as `{run, steps, steps_page,
hint?}`. `run` is one `runs[]` entry. `steps[]` are the steps the run recorded, in the order it
recorded them, at most 1000 of them (`--limit N` sets the number, `--all` lists every one;
past the limit failed steps are kept first; 0.30.2, #623):
`node_id`, `partition_key` (`k=v` pairs joined by `,`, or `null`), `status` (`success` or
`failed`; `cached` only for a step a live server run has reported), `attempts`,
`elapsed_seconds`, `run_hash`, `artifact_path` (this machine's path or the store location),
`artifact_format`, `artifact_size_bytes`, `error` (the error as recorded: for a Python
exception `Type: message` and the frames), `error_type`, `error_message` (the message alone),
`error_traceback`, `error_phase` (where the step failed, as in the error envelope's `phase`;
`null` for a step that did not fail, a dead worker or a row from before #664) and
`worker_exit` (how a dead worker ended: `SIGKILL (possibly the OOM killer)`, another signal
name, `code N` or `disconnected`; `null` for a step that raised). A step that ran or failed in a run traced with
`BARCA_TELEMETRY=datadog` also has `trace_id` (the Datadog trace it was sent as, 128 bits in 32
hex digits) and `span_id` (its root `barca.step` span, decimal); the two keys are absent for
every other step (experimental, additive, 0.41.0; [Telemetry (Datadog)](/reference/telemetry/)).
Cached steps are not listed: `run.steps_cached` counts them. `steps_page` is `{total,
truncated}`: how many steps the run recorded and whether `steps` lists fewer (the table
prints a note on stderr). When `--limit` cut the list short it also has `next_offset`, the
position of the first step not listed, as `GET /runs/{run_id}` gives it (experimental; a run
read from this machine's database once lacked it); `barca history` has no flag that reads from an
offset, `--all` lists every step. `run.retries` (experimental, #925) counts the attempts that retried a
step (`attempts` minus one per recorded step), so `attempts` = steps recorded + `retries`.
`run.plan_reasons` (experimental, #925), absent for a run that executed nothing and in the
`barca history` list, says why the run executed the steps it did instead of serving them from
cache: `{nodes, steps, reasons, omitted}`, recorded as each phase of the run is decided (a run
in progress has the phases decided so far; a phase it never reached is not in it). `reasons[]`
are `{category, subject, nodes, steps, example}`, most steps first, at most 64 (`omitted` counts
the rest); a step with several changed components counts under each. `category` is one the
engine tells apart: `never_run` (no success recorded, for a partitioned node of none of its
keys), `new_keys` (keys with no success while other keys of the node have one), `failed` (the
last attempt at this run hash failed), `code_changed`, `inputs_changed` and `env_changed` (what
changed since the last success; `subject` is the component as `barca status` words it, `code`,
`helper pipelines/common.py:bucket_ltv`, `input pipeline.py:orders`, `env TOKEN`), `changed`
(the run hash differs and the last success recorded no components), `refresh` and
`refresh_cascade` (`subject`: the node named in `--refresh`), `refresh_all`, `artifact_missing`
and `evicted` (a valid result whose file is gone), `task` and `sensor` (they always run) and
`not_examined` (no cache hit, not looked up: a server's ledger answered the lookup, or more than
10,000 keys of one node ran in one phase). `example` is the first node executed for the reason.

Cached counts update as phases are decided, before their remaining steps execute. History,
server run lists/details, and the web UI include those counts while a run is still running.
Progress counts steps (one key of a partitioned node), not nodes or worker calls. Dynamic
keys can increase the total; recovery of a missing artifact removes that step from the cached
count before computing it. Repeated phases and restarted attempts do not add old counts.

`explanation` (experimental, #956) is the run in words, derived from those recorded facts by
the same code that answers `GET /runs/{run_id}` (its `explanation`), so the table, the JSON
and the web UI say the same. Its `counts` separates planned steps and nodes, executed steps, cache hits, unreached steps, attempts and retries. `outputs` describes returned artifacts, and `target_line` summarizes the requested targets. Other fields include `targets` (a list: the job's node ids, else the targets as given;
empty for a run of every asset; `run.target` stays the string as given), `requested_by`
(`api · token:ci-bot`), `attempts` (`29 attempts: one per step, none retried`, `19,461 attempts: 19,399 steps, 62 retries`:
retries are counted apart from each step's first attempt; `null` before an attempt is
recorded), `steps` (`7 ok · 1 failed · 0 cached · 2 not reached`), `target_outcomes` (one line per
target when the server rebuilt the run's plan, otherwise empty), `cancellation` (`{headline,
detail, run}` or `null`), `why`, `progress` (`{done, total, percent, executed, cached,
eta_seconds, eta}` for a run in progress, otherwise `null`) and `slowest` (at most five
`{node_id, seconds, keys}`). `why` is `{headline, groups, omitted, partial}`: the run's steps
grouped by the reason they ran, most steps first. `headline` names the largest three
(`29 steps ran: 27 sensors (always run), 2 never run`), or says what there is to say when
there are no groups (`Every step was served from cache.`). Each of `groups[]` is `{category,
label, steps, rows}`; each of `rows[]` is `{subject, counts, nodes, steps, share, example,
example_text, line}`, where `counts` names what was counted (`27 nodes` when each node ran one
step, else `5,000 steps of 1 node`) and `line` is the row in one line (`27 nodes, e.g.
poll_inbox`). A step with several changed components counts under each, so a group's `steps`
is capped at the run's and groups can add up to more. The words of `explanation` may change;
`run.plan_reasons`, `run.attempts`, `run.retries` and `run.cancellation` are the facts to
parse. The table prints `explanation.attempts` on its first line, and after the `steps:` line
a `target:` line per target outcome and `why:` with the headline, then each group's label and
under it its rows (the first eight); `reason:` and `cancelled: ...` lines follow
`triggered by`. `hint`, present when a step
failed, is the `barca logs RUN_ID --step STEP --stream stderr` command for the first failed
step, with the `--server`, `--local` and `--env` this command was given. An unknown run id is
exit 2; against a server, one only this machine has adds the remediation `this run is on this
machine: barca history RUN_ID --local`. RUN_ID conflicts with `--reconcile`, the filters and
`--fields`.


<!-- BEGIN GENERATED schema history -->
| Key | Type | Present |
|---|---|---|
| `hint` | string | always |
| `runs` | array | always |
| `runs[]` | object | always |
| `runs[].attempts` | integer | always |
| `runs[].cancellation` | null | always |
| `runs[].client_run_id` | null | always |
| `runs[].command` | string | always |
| `runs[].elapsed_seconds` | number | always |
| `runs[].error` | string | always |
| `runs[].failed_node` | string | always |
| `runs[].failure` | object | always |
| `runs[].failure.error` | string | always |
| `runs[].failure.node` | string | always |
| `runs[].files` | array | always |
| `runs[].files[]` | string | always |
| `runs[].finished_at` | string | always |
| `runs[].job` | string | always |
| `runs[].line` | object | always |
| `runs[].line.failure` | string | always |
| `runs[].line.failure_detail` | string | always |
| `runs[].line.steps` | string | always |
| `runs[].line.steps_detail` | string | always |
| `runs[].line.target` | string | always |
| `runs[].line.target_count` | integer | always |
| `runs[].principal` | null | always |
| `runs[].run_id` | string | always |
| `runs[].schedule_id` | null | always |
| `runs[].scheduled_at` | null | always |
| `runs[].started_at` | string | always |
| `runs[].status` | string | always |
| `runs[].steps_cached` | integer | always |
| `runs[].steps_executed` | integer | always |
| `runs[].steps_total` | integer | always |
| `runs[].target` | string | always |
| `runs[].trigger_reason` | null | always |
| `runs[].triggered_by` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema history -->

`barca history --status failed --target broken --since 1d --json` has the same shape:

<!-- BEGIN GENERATED schema history_filtered -->
| Key | Type | Present |
|---|---|---|
| `runs` | array | always |
| `runs[]` | object | always |
| `runs[].attempts` | integer | always |
| `runs[].cancellation` | null | always |
| `runs[].client_run_id` | null | always |
| `runs[].command` | string | always |
| `runs[].elapsed_seconds` | number | always |
| `runs[].error` | string | always |
| `runs[].failed_node` | string | always |
| `runs[].failure` | object | always |
| `runs[].failure.error` | string | always |
| `runs[].failure.node` | string | always |
| `runs[].files` | array | always |
| `runs[].files[]` | string | always |
| `runs[].finished_at` | string | always |
| `runs[].job` | string | always |
| `runs[].line` | object | always |
| `runs[].line.failure` | string | always |
| `runs[].line.failure_detail` | string | always |
| `runs[].line.steps` | string | always |
| `runs[].line.steps_detail` | string | always |
| `runs[].line.target` | string | always |
| `runs[].line.target_count` | integer | always |
| `runs[].principal` | null | always |
| `runs[].run_id` | string | always |
| `runs[].schedule_id` | null | always |
| `runs[].scheduled_at` | null | always |
| `runs[].started_at` | string | always |
| `runs[].status` | string | always |
| `runs[].steps_cached` | integer | always |
| `runs[].steps_executed` | integer | always |
| `runs[].steps_total` | integer | always |
| `runs[].target` | string | always |
| `runs[].trigger_reason` | null | always |
| `runs[].triggered_by` | string | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema history_filtered -->

`barca history --activity --json` (experimental):

<!-- BEGIN GENERATED schema history_activity -->
| Key | Type | Present |
|---|---|---|
| `activity` | object | always |
| `activity.excluded` | object | always |
| `activity.excluded.at` | string | always |
| `activity.excluded.by_status` | object | always |
| `activity.excluded.by_status.cancelled` | integer | always |
| `activity.excluded.by_status.failed` | integer | always |
| `activity.excluded.by_status.interrupted` | integer | always |
| `activity.excluded.by_status.pending` | integer | always |
| `activity.excluded.by_status.running` | integer | always |
| `activity.excluded.by_status.success` | integer | always |
| `activity.excluded.by_trigger` | object | always |
| `activity.excluded.by_trigger.api` | integer | always |
| `activity.excluded.by_trigger.client` | integer | always |
| `activity.excluded.by_trigger.manual` | integer | always |
| `activity.excluded.by_trigger.schedule` | integer | always |
| `activity.excluded.by_trigger.unknown` | integer | always |
| `activity.excluded.failure` | null | always |
| `activity.excluded.in_progress` | integer | always |
| `activity.excluded.median_seconds` | null | always |
| `activity.excluded.not_successful` | integer | always |
| `activity.excluded.ok` | integer | always |
| `activity.excluded.runs` | integer | always |
| `activity.excluded.until` | string | always |
| `activity.hours` | array | always |
| `activity.hours[]` | object | always |
| `activity.hours[].at` | string | always |
| `activity.hours[].by_status` | object | always |
| `activity.hours[].by_status.cancelled` | integer | always |
| `activity.hours[].by_status.failed` | integer | always |
| `activity.hours[].by_status.interrupted` | integer | always |
| `activity.hours[].by_status.pending` | integer | always |
| `activity.hours[].by_status.running` | integer | always |
| `activity.hours[].by_status.success` | integer | always |
| `activity.hours[].by_trigger` | object | always |
| `activity.hours[].by_trigger.api` | integer | always |
| `activity.hours[].by_trigger.client` | integer | always |
| `activity.hours[].by_trigger.manual` | integer | always |
| `activity.hours[].by_trigger.schedule` | integer | always |
| `activity.hours[].by_trigger.unknown` | integer | always |
| `activity.hours[].failure` | null \| object | always |
| `activity.hours[].failure.runs` | integer | always |
| `activity.hours[].failure.text` | string | always |
| `activity.hours[].in_progress` | integer | always |
| `activity.hours[].median_seconds` | null \| number | always |
| `activity.hours[].not_successful` | integer | always |
| `activity.hours[].ok` | integer | always |
| `activity.hours[].runs` | integer | always |
| `activity.hours[].until` | string | always |
| `activity.not_triggered_by` | array | always |
| `activity.not_triggered_by[]` | string | always |
| `activity.search` | null | always |
| `activity.since` | string | always |
| `activity.statuses` | array | always |
| `activity.summary` | string | always |
| `activity.total` | object | always |
| `activity.total.at` | string | always |
| `activity.total.by_status` | object | always |
| `activity.total.by_status.cancelled` | integer | always |
| `activity.total.by_status.failed` | integer | always |
| `activity.total.by_status.interrupted` | integer | always |
| `activity.total.by_status.pending` | integer | always |
| `activity.total.by_status.running` | integer | always |
| `activity.total.by_status.success` | integer | always |
| `activity.total.by_trigger` | object | always |
| `activity.total.by_trigger.api` | integer | always |
| `activity.total.by_trigger.client` | integer | always |
| `activity.total.by_trigger.manual` | integer | always |
| `activity.total.by_trigger.schedule` | integer | always |
| `activity.total.by_trigger.unknown` | integer | always |
| `activity.total.failure` | object | always |
| `activity.total.failure.runs` | integer | always |
| `activity.total.failure.text` | string | always |
| `activity.total.in_progress` | integer | always |
| `activity.total.median_seconds` | number | always |
| `activity.total.not_successful` | integer | always |
| `activity.total.ok` | integer | always |
| `activity.total.runs` | integer | always |
| `activity.total.until` | string | always |
| `activity.triggered_by` | array | always |
| `activity.until` | string | always |
<!-- END GENERATED schema history_activity -->

`barca history RUN_ID --json` (a failed run):

<!-- BEGIN GENERATED schema history_run -->
| Key | Type | Present |
|---|---|---|
| `explanation` | object | always |
| `explanation.attempts` | string | always |
| `explanation.cancellation` | null | always |
| `explanation.counts` | object | always |
| `explanation.counts.attempts` | integer | always |
| `explanation.counts.attempts_line` | string | always |
| `explanation.counts.cached` | integer | always |
| `explanation.counts.failed` | integer | always |
| `explanation.counts.nodes` | integer | always |
| `explanation.counts.not_reached` | integer | always |
| `explanation.counts.not_reached_line` | null | always |
| `explanation.counts.not_reached_nodes` | integer | always |
| `explanation.counts.ok` | integer | always |
| `explanation.counts.outcome` | string | always |
| `explanation.counts.planned` | string | always |
| `explanation.counts.ran` | integer | always |
| `explanation.counts.retries` | integer | always |
| `explanation.counts.steps` | integer | always |
| `explanation.counts.units` | string | always |
| `explanation.outputs` | array | always |
| `explanation.progress` | null | always |
| `explanation.requested_by` | string | always |
| `explanation.slowest` | array | always |
| `explanation.slowest[]` | object | always |
| `explanation.slowest[].keys` | integer | always |
| `explanation.slowest[].node_id` | string | always |
| `explanation.slowest[].seconds` | number | always |
| `explanation.steps` | string | always |
| `explanation.target_line` | string | always |
| `explanation.target_outcomes` | array | always |
| `explanation.target_outcomes[]` | string | always |
| `explanation.targets` | array | always |
| `explanation.targets[]` | string | always |
| `explanation.why` | object | always |
| `explanation.why.groups` | array | always |
| `explanation.why.groups[]` | object | always |
| `explanation.why.groups[].category` | string | always |
| `explanation.why.groups[].label` | string | always |
| `explanation.why.groups[].rows` | array | always |
| `explanation.why.groups[].rows[]` | object | always |
| `explanation.why.groups[].rows[].counts` | string | always |
| `explanation.why.groups[].rows[].example` | string | always |
| `explanation.why.groups[].rows[].example_text` | string | always |
| `explanation.why.groups[].rows[].line` | string | always |
| `explanation.why.groups[].rows[].nodes` | integer | always |
| `explanation.why.groups[].rows[].share` | number | always |
| `explanation.why.groups[].rows[].steps` | integer | always |
| `explanation.why.groups[].rows[].subject` | null | always |
| `explanation.why.groups[].steps` | integer | always |
| `explanation.why.headline` | string | always |
| `explanation.why.omitted` | integer | always |
| `explanation.why.partial` | boolean | always |
| `hint` | string | always |
| `run` | object | always |
| `run.attempts` | integer | always |
| `run.cancellation` | null | always |
| `run.client_run_id` | null | always |
| `run.command` | string | always |
| `run.elapsed_seconds` | number | always |
| `run.failure` | object | always |
| `run.failure.error` | string | always |
| `run.failure.node` | string | always |
| `run.files` | array | always |
| `run.files[]` | string | always |
| `run.finished_at` | string | always |
| `run.job` | string | always |
| `run.plan_reasons` | object | always |
| `run.plan_reasons.nodes` | integer | always |
| `run.plan_reasons.omitted` | integer | always |
| `run.plan_reasons.reasons` | array | always |
| `run.plan_reasons.reasons[]` | object | always |
| `run.plan_reasons.reasons[].category` | string | always |
| `run.plan_reasons.reasons[].example` | string | always |
| `run.plan_reasons.reasons[].nodes` | integer | always |
| `run.plan_reasons.reasons[].steps` | integer | always |
| `run.plan_reasons.reasons[].subject` | null | always |
| `run.plan_reasons.steps` | integer | always |
| `run.principal` | null | always |
| `run.retries` | integer | always |
| `run.run_id` | string | always |
| `run.schedule_id` | null | always |
| `run.scheduled_at` | null | always |
| `run.started_at` | string | always |
| `run.status` | string | always |
| `run.steps_cached` | integer | always |
| `run.steps_executed` | integer | always |
| `run.steps_total` | integer | always |
| `run.target` | string | always |
| `run.trigger_reason` | null | always |
| `run.triggered_by` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact_format` | null \| string | always |
| `steps[].artifact_path` | null \| string | always |
| `steps[].artifact_size_bytes` | integer \| null | always |
| `steps[].attempts` | integer | always |
| `steps[].batch` | null | always |
| `steps[].cpu_seconds` | null \| number | always |
| `steps[].elapsed_seconds` | null \| number | always |
| `steps[].error` | null \| string | always |
| `steps[].error_message` | null \| string | always |
| `steps[].error_phase` | null \| string | always |
| `steps[].error_traceback` | null \| string | always |
| `steps[].error_type` | null \| string | always |
| `steps[].max_rss_bytes` | integer \| null | always |
| `steps[].node_id` | string | always |
| `steps[].output_tail` | array \| null | always |
| `steps[].partition_key` | null | always |
| `steps[].rows` | null | always |
| `steps[].run_hash` | string | always |
| `steps[].sink_seconds` | null | always |
| `steps[].status` | string | always |
| `steps[].worker_exit` | null | always |
| `steps_page` | object | always |
| `steps_page.nodes` | array | always |
| `steps_page.nodes[]` | object | always |
| `steps_page.nodes[].calls` | integer | always |
| `steps_page.nodes[].elapsed_seconds` | number | always |
| `steps_page.nodes[].failed` | integer | always |
| `steps_page.nodes[].keys` | integer | always |
| `steps_page.nodes[].node_id` | string | always |
| `steps_page.nodes[].status` | string | always |
| `steps_page.total` | integer | always |
| `steps_page.truncated` | boolean | always |
<!-- END GENERATED schema history_run -->

Newest first. `files` is an array of the `.py` files the run was given. `target`,
`steps_total`, `finished_at` and `elapsed_seconds` can be `null` (no target; a run still in
progress or interrupted). `status` is `running`, `success`, `failed`, `cancelled` or
`interrupted`: local history records completed steps during execution, after confirmed upload
for remote results. New committed shared progress is published every minute in healthy
operation, followed by the final outcome at run end. A `running` run counts the steps
it has recorded so far in `steps_executed`, and
`interrupted` is a run whose process died without recording an outcome ([Where Things Live](/reference/layout/),
"While a run is going, and after one is killed").

Terminal status/counts and successful/failed step rows commit in one transaction. A required
write failure reports the existing infrastructure error (exit 3) and preserves previous
history/progress and run state; captured output is written separately afterward.

Reconciled history (same envelope; unknown end timestamp and duration remain null):

<!-- BEGIN GENERATED schema history_reconciled -->
| Key | Type | Present |
|---|---|---|
| `runs` | array | always |
| `runs[]` | object | always |
| `runs[].attempts` | integer | always |
| `runs[].cancellation` | null | always |
| `runs[].client_run_id` | null | always |
| `runs[].command` | string | always |
| `runs[].elapsed_seconds` | null | always |
| `runs[].error` | null | always |
| `runs[].failed_node` | null | always |
| `runs[].failure` | null | always |
| `runs[].files` | array | always |
| `runs[].finished_at` | null | always |
| `runs[].job` | null | always |
| `runs[].line` | object | always |
| `runs[].line.failure` | null | always |
| `runs[].line.failure_detail` | null | always |
| `runs[].line.steps` | string | always |
| `runs[].line.steps_detail` | string | always |
| `runs[].line.target` | string | always |
| `runs[].line.target_count` | integer | always |
| `runs[].principal` | null | always |
| `runs[].retries` | integer | always |
| `runs[].run_id` | string | always |
| `runs[].schedule_id` | null | always |
| `runs[].scheduled_at` | null | always |
| `runs[].started_at` | string | always |
| `runs[].status` | string | always |
| `runs[].steps_cached` | integer | always |
| `runs[].steps_executed` | integer | always |
| `runs[].steps_total` | integer | always |
| `runs[].target` | string | always |
| `runs[].trigger_reason` | null | always |
| `runs[].triggered_by` | null | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema history_reconciled -->

### stats

<!-- BEGIN GENERATED schema stats -->
| Key | Type | Present |
|---|---|---|
| `avg_elapsed_seconds` | number | always |
| `cache_hit_rate` | number | always |
| `id` | string | always |
| `max_elapsed_seconds` | number | always |
| `max_rss_bytes` | integer | always |
| `median_elapsed_seconds` | number | always |
| `median_max_rss_bytes` | integer | always |
| `p95_elapsed_seconds` | number | always |
| `recent_runs` | array | always |
| `recent_runs[]` | object | always |
| `recent_runs[].attempts` | integer | always |
| `recent_runs[].cpu_seconds` | number | always |
| `recent_runs[].created_at` | string | always |
| `recent_runs[].elapsed_seconds` | number | always |
| `recent_runs[].error_message` | null | always |
| `recent_runs[].max_rss_bytes` | integer | always |
| `recent_runs[].rows` | null | always |
| `recent_runs[].run_id` | string | always |
| `recent_runs[].status` | string | always |
| `total_runs` | integer | always |
<!-- END GENERATED schema stats -->

`id` is the node id (`file.py:name`), the same key every other command uses. The timing fields
are `null` when the asset never ran; `recent_runs[].error_message` is a string for failed runs.
`total_runs` counts executions (recorded materializations). Despite its name, `cache_hit_rate` is
not a cache hit rate: it is the share of those executions that repeated an earlier execution's
run hash, i.e. ran again with unchanged code and inputs (`--refresh`, a retry after a failure, an
evicted result). A cache hit executes nothing and is not recorded per asset; `barca history`
counts cache hits per run (`steps_cached`). The human report labels it "Re-executed with
unchanged inputs". Until 0.27.0 it counted every execution whose run hash appeared more than once,
the first included.

Since 0.31, each `recent_runs` entry has `run_id` (the run that recorded it: `barca history
RUN_ID` lists every step of that run), `cpu_seconds` and `max_rss_bytes` (the worker process's
peak resident memory while the step ran; `null` for a failed step, a platform that cannot tell,
or a row from before barca recorded it), and the report has `max_rss_bytes` and
`median_max_rss_bytes` over the node's recorded executions (`null` when none has one). Rows
recorded before 0.31 hold the worker's peak over every step it had run, not the step's own.

Since 0.43 (#925) each `recent_runs` entry has `rows`, the rows of a tabular result as its worker
counted them (`null` when not counted); a partitioned node's entry sums its successful keys'
rows (`null` unless every one was counted) and adds `empty_keys`.

### gc (experimental)

<!-- BEGIN GENERATED schema gc_dry_run -->
| Key | Type | Present |
|---|---|---|
| `after_bytes` | integer | always |
| `backup_bytes` | integer | always |
| `budget_after_bytes` | integer | always |
| `budget_before_bytes` | integer | always |
| `dry_run` | boolean | always |
| `evicted` | array | always |
| `evicted[]` | object | always |
| `evicted[].bytes` | integer | always |
| `evicted[].node` | string | sometimes |
| `evicted[].path` | string | always |
| `evicted[].tier` | string | always |
| `evicted_bytes` | integer | always |
| `evicted_files` | integer | always |
| `failed` | array | always |
| `held` | array | always |
| `held[]` | object | always |
| `held[].bytes` | integer | always |
| `held[].evictable` | boolean | always |
| `held[].what` | string | always |
| `limit_bytes` | integer | always |
| `restricted` | null | always |
| `size_bytes` | integer | always |
| `target_bytes` | integer | always |
| `tiers` | array | always |
| `tiers[]` | object | always |
| `tiers[].bytes` | integer | always |
| `tiers[].files` | integer | always |
| `tiers[].tier` | string | always |
<!-- END GENERATED schema gc_dry_run -->

- `size_bytes` is `.barca/` before, `after_bytes` after (predicted, with `--dry-run`), including
  backups. `budget_before_bytes` and `budget_after_bytes` exclude recognized local metadata
  `backup_bytes`. Limits and targets apply to budgeted bytes; backups remain retained.
  `limit_bytes` and `target_bytes` (80% of the limit) are `null` with the limit `off`.
- `evicted[]` lists every entry in eviction order: `tier` is `leftovers`, `superseded`,
  `store_held` or `current`; `path` is relative to the project root; `node` is set for a result.
  `tiers[]` sums them per tier, in the same order. `failed[]` lists what could not be deleted
  (`path`, `error`); those are skipped, and the exit code is still 0.
- `restricted` is `null`, or why only leftovers and superseded results were considered (a run in
  progress on this `.barca/`).
- `dry_run` says whether anything was deleted. `--dry-run` lists exactly what `barca gc` then
  deletes, unless something changes in between.
- `held[]` says what holds `after_bytes`, largest first: `what` is `results`, `unreferenced`
  (artifact files no result names, kept for now), `metadata`, `db_copies` (databases `state
  reset` and `state restore` moved aside), `backups` or `other`; `bytes`; and `evictable`
  (`false` for the databases, their copies and the backups). Leftover entries in `evicted[]`
  include artifact files no result names.

### get and run: over the cache size limit (experimental)

<!-- BEGIN GENERATED schema get_cache_evicted -->
| Key | Type | Present |
|---|---|---|
| `cache` | object | always |
| `cache.evicted_bytes` | integer | always |
| `cache.evicted_files` | integer | always |
| `cache.limit_bytes` | integer | always |
| `cache.size_bytes` | integer | always |
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_cache_evicted -->

`cache` appears only when the check after the run found `.barca/` over its size limit; its sizes
are after eviction ([Disk Use](/reference/disk/)).


### Project star-import discovery

A later project star import preserves imported barca bindings absent from its single literal
`__all__` list/tuple with no other references to `__all__`. Colliding local names lose recognition; unknown, computed, mutated and
reflective export sets keep the conservative fallback. Source inspection does not import
user modules. This changes discovery and argument validation, with no new flags or JSON keys.

### plan (experimental)

Every plan includes `plan_version: 1`, independent of the package version, worker protocol,
and metadata database schema. Consumers of persisted plans must reject unsupported plan
versions; a missing field identifies a legacy, unversioned plan.

`phases[].reason` is `{"type": "initial"}` for the first phase or `{"type": "fan_in", "node_id":
"<id>"}` for a phase that waits on a node gathering several upstream results. `plan` reads no
state and takes no `--env`.

The phase/stream layout uses the same pool as execution: available cores, or a positive
`BARCA_POOL_SIZE`.

`warnings` is the plan-warnings array described under "Plan warnings" above.

`pool_size` is that pool. `capacity` is the most the `weight=` of the steps running at once may
add up to, and `capacity_source` where it came from: `BARCA_CAPACITY`, `[execution] capacity`
or `pool size`. `weights` maps each planned node whose `weight=` is above 1 to its weight (`{}`
when there are none). A node heavier than the capacity runs alone; `plan` warns about it on
stderr. These keys are experimental (0.29, #436).

<!-- BEGIN GENERATED schema plan -->
| Key | Type | Present |
|---|---|---|
| `capacity` | integer | always |
| `capacity_source` | string | always |
| `phases` | array | always |
| `phases[]` | object | always |
| `phases[].reason` | object | always |
| `phases[].reason.node_id` | string | sometimes |
| `phases[].reason.type` | string | always |
| `phases[].streams` | array | always |
| `phases[].streams[]` | object | always |
| `phases[].streams[].steps` | array | always |
| `phases[].streams[].steps[]` | string | always |
| `phases[].streams[].stream_id` | string | always |
| `plan_version` | integer | always |
| `pool_size` | integer | always |
| `pool_source` | string | always |
| `total_steps` | integer | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
| `weights` | object | always |
<!-- END GENERATED schema plan -->

`barca plan --partition k=a` adds `partition_selection`:

<!-- BEGIN GENERATED schema plan_partition_selection -->
| Key | Type | Present |
|---|---|---|
| `capacity` | integer | always |
| `capacity_source` | string | always |
| `partition_selection` | object | always |
| `partition_selection.nodes` | array | always |
| `partition_selection.nodes[]` | object | always |
| `partition_selection.nodes[].id` | string | always |
| `partition_selection.nodes[].selected` | integer | always |
| `partition_selection.nodes[].skipped` | integer | always |
| `partition_selection.selection` | object | always |
| `partition_selection.selection.k` | array | always |
| `partition_selection.selection.k[]` | string | always |
| `partition_selection.skipped_nodes` | array | always |
| `partition_selection.skipped_nodes[]` | string | always |
| `phases` | array | always |
| `phases[]` | object | always |
| `phases[].reason` | object | always |
| `phases[].reason.node_id` | string | sometimes |
| `phases[].reason.type` | string | always |
| `phases[].streams` | array | always |
| `phases[].streams[]` | object | always |
| `phases[].streams[].steps` | array | always |
| `phases[].streams[].steps[]` | string | always |
| `phases[].streams[].stream_id` | string | always |
| `plan_version` | integer | always |
| `pool_size` | integer | always |
| `pool_source` | string | always |
| `total_steps` | integer | always |
| `warnings` | array | always |
| `warnings[]` | object | always |
| `warnings[].kind` | string | always |
| `warnings[].message` | string | always |
| `warnings[].node` | string | always |
| `warnings[].param` | string | always |
| `weights` | object | always |
<!-- END GENERATED schema plan_partition_selection -->

### docs

`barca docs --json` (and `--all --json`, where each topic also has `content`):

<!-- BEGIN GENERATED schema docs_index -->
| Key | Type | Present |
|---|---|---|
| `topics` | array | always |
| `topics[]` | object | always |
| `topics[].name` | string | always |
| `topics[].summary` | string | always |
<!-- END GENERATED schema docs_index -->

`barca docs <topic> --json`:

<!-- BEGIN GENERATED schema docs_topic -->
| Key | Type | Present |
|---|---|---|
| `content` | string | always |
| `name` | string | always |
| `summary` | string | always |
<!-- END GENERATED schema docs_topic -->

Topic names and contents are documentation, not contract.

## Project root (stable)

Every command except `docs` and `version` runs from the project root: the nearest directory at or
above the cwd holding `barca.toml`, else the cwd. File arguments are read relative to the cwd
they were typed in and rewritten relative to the root, so node ids, `.barca/` and the working
directory of steps do not depend on where barca was invoked. The stderr line
`barca: project root: <path> ...`, printed when the root is not the cwd, is informational and not
contract.

## stderr

stderr carries progress, warnings, your steps' own `print` output and errors. Only the error
envelope and the `--agent` lines below are contract. Other `[barca] ...` lines, such as
`[barca] backed up the metadata DB ...` and `[barca] restored ...` ([Remote Storage](/reference/remote-storage/)), are
informational and not contract. The same goes for the wording of an error: a backup that fails
its check in `barca state restore` is exit 2 with `kind` `usage`, and the text that names it and
the next newest backup is for people.

### Error envelope (stable)

In JSON mode every error is one JSON object on stderr. A failed run follows it with one
summary line; read the last JSON object with `kind` and `error`. The same envelope comes
from argument errors, engine errors and failed steps:

<!-- BEGIN GENERATED schema error_usage -->
| Key | Type | Present |
|---|---|---|
| `code` | integer | always |
| `error` | string | always |
| `kind` | string | always |
| `remediation` | string | always |
<!-- END GENERATED schema error_usage -->

An argument the parser rejects (here `--jsn`) gives the same shape:

<!-- BEGIN GENERATED schema error_usage_parse -->
| Key | Type | Present |
|---|---|---|
| `code` | integer | always |
| `error` | string | always |
| `kind` | string | always |
| `remediation` | string | always |
<!-- END GENERATED schema error_usage_parse -->

`step_failed` adds `node`, `traceback`, `artifact_dir` and `phase`:

<!-- BEGIN GENERATED schema error_step_failed -->
| Key | Type | Present |
|---|---|---|
| `artifact_dir` | string | always |
| `code` | integer | always |
| `error` | string | always |
| `kind` | string | always |
| `node` | string | always |
| `phase` | string | always |
| `remediation` | string | always |
| `traceback` | string | always |
<!-- END GENERATED schema error_step_failed -->

`code` equals the exit code. `remediation` can be `null`. In human mode the same error is prose
with the remediation before the final run summary.

`phase` (#664) says where the step failed, as its worker reported it. The values are a closed
set, in the order a step goes through them:

| `phase` | Where | `traceback` |
|---|---|---|
| `load_inputs` | barca reading the step's input artifacts (a `collect()` fan-in included) | every frame, barca's included |
| `user_code` | the step's own code: importing its module, calling its function, checking what it returned (a sensor's change signal, a `partitions_from` source's keys) | your code's frames (barca's removed), or `null` |
| `split_output` | barca dividing a batched call's result into its keys (a key with no result included) | every frame |
| `write_output` | barca writing the result to its artifact (an unpicklable return value included) | every frame |
| `sink` | barca writing the step's `@sink` files | every frame |

`phase` is `null` when no worker reported one: a worker that died (`worker for ... exited with
...`), a step barca failed without running it (its `partitions_from` source could not be read).
For any phase but `user_code` the failure is in barca's own code around the step, the human
prose says so (`Step 'p.py:clean' failed in barca while loading its inputs (phase
load_inputs): ...`, and `run failed: step 'p.py:clean' failed in barca (load_inputs): ...`),
and `error`, `kind` and the exit code are those of any failed step (`step_failed`, exit 1). A
future release may add a value; a client should treat an unknown one like a failure in barca.

`reason` (string) is present only on an error a client may handle on its own, and names it
stably; any other error has no `reason` key. Authentication failures use
`authentication_required` or `invalid_token` (`kind` `infra`, exit 3; HTTP 401 with
`WWW-Authenticate: Bearer`). Another value is `metadata_unusable`
(`kind` `infra`, exit 3): the metadata DB is there but this barca cannot use it (empty, not a
SQLite file, a newer format, a malformed schema). `error` names the file and `remediation` says
to upgrade barca, put a backup back with `barca state restore`, or start over with
`barca state reset` (the UI's Reset database button under `barca serve`). The HTTP
API returns the same envelope (`500`).

### --agent lines

With `--agent`, `get` and `run` print plain progress lines on stderr instead of a progress bar.
These are the lines the fixture run produced, with timings and counters replaced by
placeholders:

<!-- BEGIN GENERATED agent-lines -->
```
[barca] <n>/<total> steps | done in <secs>s
[barca] <n>/<total> steps | failed in <secs>s
[barca] checking artifact store <store>/default/artifacts (timeout <secs>s)
[barca] env default (built-in default); artifact store <store>/default/artifacts
[barca] importing <module>
[barca] run failed: step 'pipeline.py:broken' failed: ValueError: contract fixture failure; 0 downstream steps skipped (exit 1)
[barca] step:pipeline.py:broken failed: ValueError: contract fixture failure
[barca] step:pipeline.py:keys completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:numbers cached
[barca] step:pipeline.py:per_key[k=a] completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:per_key[k=b] completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:report completed <secs>s (<n>/<total>)
[barca] step:pipeline.py:total cached env CONTRACT_API_TOKEN=<unset> CONTRACT_REGION=eu
[barca] step:pipeline.py:value completed <secs>s (<n>/<total>) attempts=2
[barca] uploaded <n> pending results
[barca] uploading <n> artifacts to <store>/default/artifacts (timeout <secs>s per attempt); waiting for confirmation
[barca] uploading <n> pending results to <store>/default/artifacts from an earlier run
[barca] warning: 1 step reads environment variables not declared in env=: CONTRACT_REPORT_TITLE; barca status lists them
[barca] warning: pipeline.py:report never uses its input `rows`. It is still loaded in full each time the step runs, and it counts toward the step's cache key. Use it, remove it from inputs=, or rename the parameter `_rows` if it is there for ordering only (a `_` input is not loaded and never flagged)
```
<!-- END GENERATED agent-lines -->

| Line | When | Stability |
|---|---|---|
| `[barca] step:<id> completed <secs>s (<n>/<total>)[ env NAME=VALUE ...]` | a step finished; `<id>` includes `[key=value]` for a partition | stable |
| `[barca] step:<id> cached[ env NAME=VALUE ...]` | a step was served from cache. Printed when the step is decided, or, when its artifact is not on disk, at the end of the run once it is settled that nothing needed it (if something did, the step prints `completed` or `failed` instead, never both). Exception: a result in a remote artifact store whose object proves to be deleted when it is fetched has already printed `cached`; a `[barca] warning: <id>: the artifact of its cached result is missing ...` line and then `completed` or `failed` for the same step follow | stable |
| `[barca] step:<id> failed: <first line of the error>` | a step raised | stable |
| `[barca] run failed: step '<id>' failed: <first error line>; <n> downstream steps skipped (exit <code>)` | last stderr line of a failed run, after the error envelope (every mode) | stable |
| `[barca] <n>/<total> steps \| done in <secs>s` | end of a run that executed steps, with or without `--agent`; `failed in` when a step failed, `cancelled after` on a Ctrl-C that arrives while steps are running (never `done` then). The line is about the steps: with an artifact store, a Ctrl-C after the last step finished (while artifacts upload or the history is backed up) follows a `done` line, and the exit code (130) and the error envelope still say `cancelled` ([Remote Storage: Ctrl-C](/reference/remote-storage/#ctrl-c)) | stable |
| `[barca] batch:<node> <n> call(s), <n> key(s) \| import <secs>s, load <secs>s, compute <secs>s, split <secs>s, write <secs>s` | before the end-of-run line, one per node whose keys ran in batched calls, with `--agent` or a progress bar; also recorded in the run's logs ([Batched Partitions: Seeing calls](/reference/batched-partitions/#seeing-calls)) | experimental |
| `[barca] metadata <secs>s recording this run's results` | after the end-of-run line of a run with such a node, with `--agent` or a progress bar; also recorded in the run's logs | experimental |
| `[barca] env <env> (<source>); artifact store <store>` | once at startup of `get` and `run` (every mode) when the environment comes from `default_env` in `barca.toml` or a separate artifact store is configured. `<source>` is `--env`, `BARCA_ENV`, `default_env in barca.toml` or `built-in default`; `<store>` is the store URI with credentials redacted, or `local (<dir>)` | experimental |
| `[barca] env <env> (<source>); artifact store <store>; metadata <path> (db format <n>)` | once at startup of `serve`, always, after its metadata DB is set up: the line above plus the database and its format (`PRAGMA user_version`). `(unusable)` in place of `(db format <n>)` when it cannot be used (a writer then exits 3), `(not created yet)` when there is no database yet (the first run creates it) | experimental |
| `[barca] note: no --env given, so this uses env <env> (<source>); this project has <n> environments (<names>). Pass --env NAME or set BARCA_ENV; require_env = true in barca.toml (or BARCA_REQUIRE_ENV=1) makes it required` | once per command that reads or writes an environment (every mode), when neither `--env` nor `BARCA_ENV` names one and the project has more than one (`.barca/metadata.db` and the directories of `.barca/envs/`) | experimental |
| `[barca] checking artifact store <store> (timeout <secs>s)` | before workers start whenever a separate artifact store is configured, including state-off runs, except within 5 minutes of a successful check of the same store, environment and storage options when the run does not refresh, is not `get -o url` and has no pending uploads: such a run prints it before its first fetch, if it fetches ([Remote Storage: How often the store is checked](/reference/remote-storage/#how-often-the-store-is-checked)); requires bucket/container-root listing and backend existence access, verifies reachability rather than write permission | experimental |
| `[barca] uploading <n> artifacts to <store> (timeout <secs>s per attempt); waiting for confirmation` | before awaiting queued artifact uploads (every mode) | experimental |
| `[barca] uploading <n> pending results to <store> from an earlier run` | before workers start, when results an earlier run could not upload are retried (every mode); followed by `[barca] uploaded <n> pending results` or `[barca] <n> pending results still not uploaded (<k> uploaded): <why>` | experimental |
| `[barca] still waiting for artifact uploads to <store> (<secs>s)` | every 10 seconds while the upload drain waits (every mode); destinations omit URI passwords and signed query parameters | experimental |
| `[barca] progress checkpoint failed; recorded results remain local: <cause>` | a mid-run shared-state publication failed; committed local results are retained and retried at the next minute tick, without changing successful user work into a failed step | experimental |
| `[barca] still running (<n>s): <id>` | a step in flight for `BARCA_PROGRESS_SECS` (every mode) | experimental |
| `[barca] <n> more: <first line of a warning>` | before the end-of-run line (every mode): a `logging` WARNING printed because logging is unconfigured, or a `warnings` warning, from a step's process was printed once and suppressed `<n>` more times in this run; beyond ten texts, one `[barca] <n> more: <k> other repeated warnings` ([Scripts and AI Agents](/reference/agents/)) | experimental |
| `[barca] server <url> (<source>)` | once when a command uses the configured server (`BARCA_SERVER` or `[server] url in barca.toml`, every mode): a local `get`/`run` reads and posts to its ledger, `status`/`history`/`stats` and a bare `--server` are answered by it ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)) | experimental |
| `[barca] run <id> is queued behind a restart (position <n>): the server at <url> is draining and starts no run; it starts when the next server is up` | `get`/`run --server` (every mode), once after `POST /runs`, when the server is draining and the run waits for the next server (`waiting_for: "restart"`; [Server Mode: a Shared Ledger and --server](/reference/server-mode/)) | experimental |
| `[barca] recorded run <id> on the server (run <server id>)` | a run this machine could not post before was accepted by the server's ledger, before the command's own lookup | experimental |
| `[barca] warning: run <id> was not recorded on the server <url>: <why>; it stays on this machine (unsynced) and ...`, `[barca] warning: run <id> is still not recorded on the server <url>: <why>` | the server refused a run's post (`409`) or could not be reached after the run; the run's result and exit code are unchanged | experimental |
| `[barca] skipped N task(s) ...`, `[barca] nothing to get ...` | `get` with no target skipped tasks | experimental |
| `[barca] cache <size> over its <limit> limit; evicted <n> files (<size>): <n> <tier>, ...` | after `get`, `run` or a `serve` run, when `.barca/` was over `[cache] max_size` and something was evicted (every mode; [Disk Use](/reference/disk/)) | experimental |
| `[barca] <id>: its cached result was evicted to stay under the cache limit; computing it again` | a step whose cached result the size limit evicted is computed again because something reads it (every mode) | experimental |
| `[barca] dataset sink <prefix>: deleted <n> key(s) its asset no longer has` | after a run, the directories of keys a dataset sink's asset no longer has were deleted ([Sinks](/reference/sinks/)) | experimental |
| `[barca] warning: ...` | warnings (always this lowercase prefix; the text after it may change), including a dataset sink's size warning and a failed deletion of removed keys. Up to 0.29 a failed sink printed `[barca] SINK FAILED: ...`; from 0.30.0 it fails the step. A plan warning (see "Plan warnings") is printed before the first `step:` line, once per run, with or without `--agent`; read it from the JSON `warnings` array rather than from this line | experimental |

Failed runs keep the complete `steps` fan-out and `failed_node` in JSON. Human output
reports blocked downstream nodes as one count in the final line. This count is per node,
including a partitioned node once. A multi-target failure uses its first failed step for
the summary and counts all blocked downstream nodes; detailed outcomes remain in JSON.

`env` values: `<unset>` for an unset variable, `<redacted>` for secret-looking names (`*_TOKEN`,
`*_SECRET`, `*_KEY`, `*_PASSWORD`), double quotes around values with spaces. With several targets
the first failed step is summarized once; each target's error remains in JSON.

## Notes by feature

### Static decorator bindings

Qualified and imported aliases of barca decorators/helpers receive the same node
semantics and argument validation as direct imports. Explicit foreign or rebound
names do not define barca nodes. For a foreign wrapper intended to remain a node,
stack it with a genuine barca decorator; see [Assets, Sensors and Inputs](/reference/assets/).

`list --groups` is experimental: JSON returns `{groups: [{id, name, description, members, output}]}`. IDs and member/output references are strings; members is an array of strings. The default `list` schema is unchanged. Groups are organizational metadata and cannot be execution targets.

### Recompute byte checks

Successful materializations record SHA-256 digests. A different digest for the same node
and run hash emits `[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic`
and adds `steps[].bytes_changed: true` (otherwise absent). Partitioned reports aggregate
that flag, and stderr identifies each changed key. This warning does not change exit codes.
Status nodes optionally expose `last_bytes_change: {node_id, run_hash, content_hash, created_at}`,
retained after later unchanged recomputes; latest materializations expose `content_hash`
when known. These additive keys are stable. See [The Cache](/reference/cache/) and
`specs/interfaces/materialization-digests.md`. A `barca get` that re-uploads a `mismatch` target (no step
runs) prints the same warning and sets the same flag. Server-post semantics remain proposed.

#### Recomputed output with changed bytes

<!-- BEGIN GENERATED schema get_bytes_changed -->
| Key | Type | Present |
|---|---|---|
| `elapsed_seconds` | number | always |
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `phases` | integer | always |
| `run_id` | string | always |
| `status` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].attempts` | integer | always |
| `steps[].bytes_changed` | boolean | always |
| `steps[].detail` | string | always |
| `steps[].elapsed_seconds` | number | always |
| `steps[].id` | string | always |
| `steps[].kind` | string | always |
| `steps[].reason` | string | always |
| `steps[].run_hash` | string | always |
| `steps[].status` | string | always |
| `steps_executed` | integer | always |
| `warnings` | array | always |
<!-- END GENERATED schema get_bytes_changed -->

#### Status retaining the last byte change

<!-- BEGIN GENERATED schema status_bytes_changed -->
| Key | Type | Present |
|---|---|---|
| `as_of` | string | always |
| `data_coverage` | object | always |
| `data_coverage.assets` | integer | always |
| `data_coverage.measured` | integer | always |
| `data_coverage.suspect` | integer | always |
| `disk_usage` | object | always |
| `disk_usage.backup_bytes` | integer | always |
| `disk_usage.budget_bytes` | integer | always |
| `disk_usage.limit_bytes` | integer | always |
| `disk_usage.size_bytes` | integer | always |
| `nodes` | array | always |
| `nodes[]` | object | always |
| `nodes[].cache` | object | always |
| `nodes[].cache.artifact` | string | always |
| `nodes[].cache.detail` | string | always |
| `nodes[].cache.reason` | string | always |
| `nodes[].cache.run_hash` | string | always |
| `nodes[].cache.state` | string | always |
| `nodes[].cause` | null | always |
| `nodes[].env` | array | always |
| `nodes[].expected_fresh_at` | null | always |
| `nodes[].gap_seconds` | integer | always |
| `nodes[].health` | string | always |
| `nodes[].health_reason` | string | always |
| `nodes[].id` | string | always |
| `nodes[].inputs` | array | always |
| `nodes[].kind` | string | always |
| `nodes[].last_attempt` | string | always |
| `nodes[].last_bytes_change` | object | always |
| `nodes[].last_bytes_change.content_hash` | string | always |
| `nodes[].last_bytes_change.created_at` | string | always |
| `nodes[].last_bytes_change.node_id` | string | always |
| `nodes[].last_bytes_change.run_hash` | string | always |
| `nodes[].last_check` | null | always |
| `nodes[].last_materialization` | object | always |
| `nodes[].last_materialization.artifact` | string | always |
| `nodes[].last_materialization.content_hash` | string | always |
| `nodes[].last_materialization.created_at` | string | always |
| `nodes[].last_materialization.elapsed_seconds` | number | always |
| `nodes[].last_materialization.format` | string | always |
| `nodes[].last_materialization.rows` | null | always |
| `nodes[].last_materialization.run_hash` | string | always |
| `nodes[].last_materialization.size_bytes` | integer | always |
| `nodes[].last_materialization.status` | string | always |
| `nodes[].measured` | boolean | always |
| `nodes[].name` | string | always |
| `nodes[].no_estimate` | null | always |
| `nodes[].partitioned` | boolean | always |
| `nodes[].recheck_at` | null | always |
| `nodes[].refresh_path` | null | always |
| `nodes[].root` | null | always |
| `nodes[].shape` | object | always |
| `nodes[].shape.type` | string | always |
| `nodes[].signals` | array | always |
| `nodes[].stale_since` | null | always |
| `nodes[].typical_run_seconds` | number | always |
| `rollup` | object | always |
| `rollup.also_in` | array | always |
| `rollup.asset_causes` | object | always |
| `rollup.asset_causes.catching_up` | integer | always |
| `rollup.asset_causes.failing` | integer | always |
| `rollup.asset_causes.no_refresh_path` | integer | always |
| `rollup.asset_causes.overdue` | integer | always |
| `rollup.asset_causes.queued` | integer | always |
| `rollup.asset_causes.undecided` | integer | always |
| `rollup.asset_health` | object | always |
| `rollup.asset_health.behind` | integer | always |
| `rollup.asset_health.blocked` | integer | always |
| `rollup.asset_health.failing` | integer | always |
| `rollup.asset_health.never_run` | integer | always |
| `rollup.asset_health.ok` | integer | always |
| `rollup.asset_health.suspect` | integer | always |
| `rollup.asset_health.undecided` | integer | always |
| `rollup.assets` | integer | always |
| `rollup.causes` | object | always |
| `rollup.causes.catching_up` | integer | always |
| `rollup.causes.failing` | integer | always |
| `rollup.causes.no_refresh_path` | integer | always |
| `rollup.causes.overdue` | integer | always |
| `rollup.causes.queued` | integer | always |
| `rollup.causes.undecided` | integer | always |
| `rollup.children` | array | always |
| `rollup.data_coverage` | object | always |
| `rollup.data_coverage.assets` | integer | always |
| `rollup.data_coverage.measured` | integer | always |
| `rollup.data_coverage.suspect` | integer | always |
| `rollup.fresh_by` | null | always |
| `rollup.gap_seconds` | integer | always |
| `rollup.health` | object | always |
| `rollup.health.behind` | integer | always |
| `rollup.health.blocked` | integer | always |
| `rollup.health.failing` | integer | always |
| `rollup.health.never_run` | integer | always |
| `rollup.health.ok` | integer | always |
| `rollup.health.suspect` | integer | always |
| `rollup.health.undecided` | integer | always |
| `rollup.held_by` | array | always |
| `rollup.id` | string | always |
| `rollup.kind` | string | always |
| `rollup.label` | string | always |
| `rollup.nodes` | integer | always |
| `rollup.not_fresh` | integer | always |
| `rollup.parent` | null | always |
| `rollup.running` | integer | always |
| `rollup.split` | object | always |
| `rollup.split.catching_up` | integer | always |
| `rollup.split.in_hand_nodes` | integer | always |
| `rollup.split.in_hand_weight` | integer | always |
| `rollup.split.need_someone` | integer | always |
| `rollup.split.need_someone_nodes` | integer | always |
| `rollup.split.need_someone_weight` | integer | always |
| `rollup.split.run_by_hand` | integer | always |
| `rollup.stale_since` | null | always |
| `rollup.unbounded` | integer | always |
| `rollup.weight` | integer | always |
| `rollup.weights` | object | always |
| `rollup.weights.catching_up` | integer | always |
| `rollup.weights.failing` | integer | always |
| `rollup.weights.fresh` | integer | always |
| `rollup.weights.no_refresh_path` | integer | always |
| `rollup.weights.overdue` | integer | always |
| `rollup.weights.queued` | integer | always |
| `rollup.weights.undecided` | integer | always |
| `root` | string | always |
| `summary` | object | always |
| `summary.always_runs` | integer | always |
| `summary.blocked` | integer | always |
| `summary.cached` | integer | always |
| `summary.never_run` | integer | always |
| `summary.partial` | integer | always |
| `summary.stale` | integer | always |
| `summary.unknown` | integer | always |
| `summary.unsynced` | integer | always |
| `target` | null | always |
| `targets` | array | always |
| `total` | integer | always |
| `truncated` | boolean | always |
<!-- END GENERATED schema status_bytes_changed -->

### CSV artifact format

An artifact reference may have `format: "csv"` and a `.csv` path when explicitly selected
with `serializer="csv"`. CSV sink extensions select CSV automatically. Frames continue
to default to Parquet. CSV requires a supported tabular value; readers infer types.
Status samples represent CSV cells as strings, and SQL views support CSV artifacts.
See [Output Formats and Types](/reference/types/) and `specs/interfaces/artifact-formats.md`.

### Task stdout and logs

From 0.47.0, a multiline stderr write and each failed attempt's traceback occupy one
entry with embedded newlines in `line`; stdout remains line-oriented. Old records retain
their original boundaries. `--timestamps` prefixes each entry once. Use
`barca logs --pretty | less` for a readable pager. JSON keys and types are unchanged.

`logs RUN_ID` is stable. JSON returns an array of `{node_id: string, stream: "stdout" |
"stderr", ts: string | null, line: string}` objects in recorded order; text returns only the
lines (`--timestamps` puts each line's `ts` before it). `stream` (additive, 0.29.2) is
`stderr` for what a step wrote to Python's `sys.stderr` (`logging`, `warnings`) and for the
traceback of each failed attempt; `ts` (additive, 0.29.2) is when the worker wrote the line,
RFC 3339 UTC with milliseconds, and `null` on lines recorded by an older barca. `--stream
stdout|stderr` keeps one stream. `--step` selects an exact id/base id/function name; ambiguous
or absent steps and missing runs exit 2. Silent recorded steps return no lines. `--env` selects
the environment's history; logs does not import source or contact a store.
Without `RUN_ID` (additive, 0.29.0), logs reads the newest run in history, running or
finished, and names it on stderr (`barca: logs of the most recent run: <run_id> (<command>
<target>, <status>, started <time>)`; the wording is not stable); stdout is unchanged. With no
run recorded it exits 2. A run in progress returns its lines so far.
`-f`/`--follow` (experimental, 0.29.2) prints lines as they arrive, as JSON Lines with
`--json` (one object per line, the array's objects), and exits when the run ends: 0 when it
succeeded, 130 when it was cancelled, 1 otherwise. Without `RUN_ID` it follows the newest
running run, else reads the most recent one.
With a configured server (experimental), logs reads the server's runs (`GET
/runs/{run_id}/logs`; `-f` streams `GET /runs/{run_id}/events`); `--local` reads this
machine's, and nothing falls back. A run id the server does not have but this machine does is
exit 2 with the remediation `this run is on this machine: barca logs <run_id> --local`. Without
a run id, a configured server's answer is followed on stderr by `[barca] note: this machine has
a newer run <run_id> (<command> <target>, <status>); barca logs --local reads it` when this
machine's newest run started later; stdout is unchanged. The wording of both is not stable.
A run records complete user lines until their text reaches or exceeds 16 MiB. The whole
crossing line is retained, followed by `[barca] log cap reached: ...` (stream `stderr`);
later user lines are not recorded, but framework notes may follow. This is a soft threshold,
not a hard byte bound on one line or on retained memory.
Task return values remain `final_output` as for assets. Get/run JSON mode suppresses captured
stdout lines; non-JSON modes prefix them with `[node_id]` on stderr. A step's stderr is written
to stderr as it comes, in every mode, as before. Agent framework lines
remain `[barca]`-prefixed and are not recorded. Retrieve captured output with `barca logs`.
See [barca logs](/reference/cli/logs/) and `specs/interfaces/task-output.md` for flush, failure and ordering boundaries.

<!-- BEGIN GENERATED schema logs -->
| Key | Type | Present |
|---|---|---|
| `[]` | object | always |
| `[].line` | string | always |
| `[].node_id` | string | always |
| `[].stream` | string | always |
| `[].ts` | string | always |
<!-- END GENERATED schema logs -->

### Execution timing and imports

Under `--agent` (and in runs `barca serve` starts), import progress uses
`[barca] importing <module>` before a module is loaded; without `--agent` the line is not printed,
and `import_seconds` in the run JSON reports a slow import. Completion
and permanent failure step lines append `attempts=N` only when N > 1; their existing
prefixes and completion durations remain. Timeout messages identify the step, rather
than the internal callable wrapper.

Older read-only snapshots without attempt metadata report a null history attempt total.

### Doctor report (experimental)

<!-- BEGIN GENERATED schema doctor -->
| Key | Type | Present |
|---|---|---|
| `checks` | array | always |
| `checks[]` | object | always |
| `checks[].data` | object | always |
| `checks[].data.artifacts` | string | sometimes |
| `checks[].data.available_bytes` | integer | sometimes |
| `checks[].data.backup_bytes` | integer | sometimes |
| `checks[].data.barca_version` | string | sometimes |
| `checks[].data.budget_bytes` | integer | sometimes |
| `checks[].data.capacity` | integer | sometimes |
| `checks[].data.enabled` | boolean | sometimes |
| `checks[].data.executable` | string | sometimes |
| `checks[].data.file` | string | sometimes |
| `checks[].data.files` | array | sometimes |
| `checks[].data.files[]` | string | always |
| `checks[].data.filesystem_bytes` | integer | sometimes |
| `checks[].data.found` | boolean | sometimes |
| `checks[].data.limit_bytes` | integer | sometimes |
| `checks[].data.local_backups` | string | sometimes |
| `checks[].data.max_size` | string | sometimes |
| `checks[].data.metadata` | string | sometimes |
| `checks[].data.mode` | string | sometimes |
| `checks[].data.name` | string | sometimes |
| `checks[].data.nodes` | integer | sometimes |
| `checks[].data.path` | string | sometimes |
| `checks[].data.pool_size` | integer | sometimes |
| `checks[].data.pool_source` | string | sometimes |
| `checks[].data.size_bytes` | integer | sometimes |
| `checks[].data.source` | string | sometimes |
| `checks[].data.uri` | null | sometimes |
| `checks[].data.version` | string | sometimes |
| `checks[].detail` | string | always |
| `checks[].name` | string | always |
| `checks[].ok` | boolean | always |
| `ok` | boolean | always |
<!-- END GENERATED schema doctor -->

## Changing the surface

A pull request that changes a command, flag, environment variable, exit code, JSON output, the
error envelope or an `--agent` line updates, in the same PR:

1. the snapshots, with one command from the repository root (it rebuilds barca first):

   ```
   scripts/update-cli-snapshots.sh
   ```

   This runs `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca` (help snapshots in
   `crates/barca-cli/snapshots/help/`, the command, argument and exit-code tables here) and
   `BARCA_UPDATE_SNAPSHOTS=1 pytest python/tests/test_cli_contract.py` (JSON schemas in
   `python/tests/snapshots/cli_contract/` and the schema tables here), then copies this page to
   the site (`site/src/content/docs/reference/cli-contract.md`). Review the diff: it is the
   change to the contract.
2. this page's hand-written parts (stability, the experimental table, notes), and the
   classification in `EXPERIMENTAL` in `crates/barca-cli/src/contract.rs` for a new experimental
   flag or command;
3. pre-1.0, for a breaking change: plan a minor bump and add a "Breaking" line to the release
   notes. From 1.0: keep the old spelling working for at least one minor release, with a stderr
   warning.

Keyed refresh selectors (`--refresh 'source[account=A],source[account=C]'`) select a set
of refresh coordinates independently of the target domain. Each selector has one dimension.
The existing refresh list in CLI, Python and server requests shares these semantics;
`--partition` continues to narrow execution and skip affected fan-ins.


### Explicit task skips (#811)

`barca.Skip(reason)` raised by task code yields `steps[].status: skipped` with `skip_reason`.
The persisted run step carries the same fields and no output/artifact. A downstream blocked
step instead has reason `upstream_skipped` without `skip_reason`. An all-skipped run has
`status: skipped`, exit 0, and no final output; mixed successful work keeps `success`, and
failures dominate. Counts include `skipped`, with `ran = ok + failed + skipped`.
`GET /schedule` adds optional `last_task_skip_reason` alongside `last_status`; existing
scheduler `skip_reason` retains its enum vocabulary. Recorded tick outcomes include `skipped`.
### Completed-result reads

Experimental `get --cached-only[=store|local]` does not execute or enqueue work. JSON returns
`outcomes`, `final_output`, and `path_scope` (`local` or `server`); misses also carry `error`,
the `cache_miss` envelope. Every selected key is a hit or miss, and any miss exits 1 with
all outcomes in the stderr envelope. Executable pickle reconstruction is refused with infra
reason `cached_value_unsupported`; verified artifact path access remains available. Python raises `CacheMiss(BarcaError)` with `outcomes`
and `missing`. A store failure or invalid artifact exits 3. See [The Cache](/reference/cache/) for tiers,
freshness, discovery limits and lifetime of artifact references.


<!-- BEGIN GENERATED schema get_cached -->
| Key | Type | Present |
|---|---|---|
| `final_output` | object | always |
| `final_output._barca_artifact` | object | always |
| `final_output._barca_artifact.format` | string | always |
| `final_output._barca_artifact.path` | string | always |
| `final_output._barca_artifact.size_bytes` | integer | always |
| `outcomes` | array | always |
| `outcomes[]` | object | always |
| `outcomes[].format` | string | always |
| `outcomes[].fresh` | boolean | always |
| `outcomes[].node` | string | always |
| `outcomes[].partition` | null | always |
| `outcomes[].path` | string | always |
| `outcomes[].run_hash` | string | always |
| `outcomes[].size_bytes` | integer | always |
| `outcomes[].source` | string | always |
| `outcomes[].status` | string | always |
| `path_scope` | string | always |
<!-- END GENERATED schema get_cached -->

<!-- BEGIN GENERATED schema get_cached_miss -->
| Key | Type | Present |
|---|---|---|
| `error` | object | always |
| `error.<name>` | array \| integer \| null \| string | always |
| `error.<name>[]` | object | always |
| `error.<name>[].node` | string | always |
| `error.<name>[].partition` | string | always |
| `error.<name>[].reason` | string | always |
| `error.<name>[].status` | string | always |
| `final_output` | `<user value>` | always |
| `outcomes` | array | always |
| `outcomes[]` | object | always |
| `outcomes[].node` | string | always |
| `outcomes[].partition` | string | always |
| `outcomes[].reason` | string | always |
| `outcomes[].status` | string | always |
| `path_scope` | string | always |
<!-- END GENERATED schema get_cached_miss -->

<!-- BEGIN GENERATED schema get_cached_error -->
| Key | Type | Present |
|---|---|---|
| `code` | integer | always |
| `error` | string | always |
| `kind` | string | always |
| `outcomes` | array | always |
| `outcomes[]` | object | always |
| `outcomes[].node` | string | always |
| `outcomes[].partition` | string | always |
| `outcomes[].reason` | string | always |
| `outcomes[].status` | string | always |
| `remediation` | null | always |
<!-- END GENERATED schema get_cached_error -->


### Ordinary server cache hits

Ordinary server `get` requests whose selected plan is provably cached use the same
retained-artifact read operation, under request freshness rules. They do not wait for a run
slot or allocate a pipeline worker. Four artifact reads may proceed at once per server,
shared with explicit cached-only reads and independent of execution capacity. Store fetches
count as reads and refresh their retention stamps; active reads pin their selected artifacts
and retain completed inodes in process-owned staging through concurrent GC.

A miss, stale result, explicit refresh, or a plan requiring sensor execution or dynamic key
resolution follows normal execution admission. Executable pickle reconstruction also follows
normal admission; the read path never imports project code. Static selections, including whole-file requests, traverse artifact metadata in pages of
128 keys with one retained inode at a time. The existing output listing limit affects only
presentation: a larger unselected cache hit returns its usual partition counts, while an
explicit selection returns every selected key. Freshness and selection semantics are unchanged. Cached-only reads continue to return latest successful
results even when stale.

### Retained execution attempts (experimental)

Requeue keeps the original request ID and acceptance time. `history RUN_ID --json` adds
`run.previous_attempts` when earlier executions exist: entries contain `run_id`, a one-based
`attempt`, and `stop_reason`. Reading an archived execution adds `parent_run_id`,
`run_attempt` and `stop_reason`. Run-list counts exclude archived executions.
`logs RUN_ID` concatenates executions with stderr boundary notes; `logs -f --local` waits
while queue ownership shows the request will resume. Step/stream filters also apply to notes.

<!-- BEGIN GENERATED schema history_requeued -->
| Key | Type | Present |
|---|---|---|
| `explanation` | object | always |
| `explanation.attempts` | string | always |
| `explanation.cancellation` | null | always |
| `explanation.counts` | object | always |
| `explanation.counts.attempts` | integer | always |
| `explanation.counts.attempts_line` | string | always |
| `explanation.counts.cached` | integer | always |
| `explanation.counts.failed` | integer | always |
| `explanation.counts.nodes` | integer | always |
| `explanation.counts.not_reached` | integer | always |
| `explanation.counts.not_reached_line` | null | always |
| `explanation.counts.not_reached_nodes` | integer | always |
| `explanation.counts.ok` | integer | always |
| `explanation.counts.outcome` | string | always |
| `explanation.counts.planned` | string | always |
| `explanation.counts.ran` | integer | always |
| `explanation.counts.retries` | integer | always |
| `explanation.counts.steps` | integer | always |
| `explanation.counts.units` | string | always |
| `explanation.outputs` | array | always |
| `explanation.progress` | null | always |
| `explanation.requested_by` | string | always |
| `explanation.slowest` | array | always |
| `explanation.slowest[]` | object | always |
| `explanation.slowest[].keys` | integer | always |
| `explanation.slowest[].node_id` | string | always |
| `explanation.slowest[].seconds` | number | always |
| `explanation.steps` | string | always |
| `explanation.target_line` | string | always |
| `explanation.target_outcomes` | array | always |
| `explanation.target_outcomes[]` | string | always |
| `explanation.targets` | array | always |
| `explanation.targets[]` | string | always |
| `explanation.why` | object | always |
| `explanation.why.groups` | array | always |
| `explanation.why.groups[]` | object | always |
| `explanation.why.groups[].category` | string | always |
| `explanation.why.groups[].label` | string | always |
| `explanation.why.groups[].rows` | array | always |
| `explanation.why.groups[].rows[]` | object | always |
| `explanation.why.groups[].rows[].counts` | string | always |
| `explanation.why.groups[].rows[].example` | string | always |
| `explanation.why.groups[].rows[].example_text` | string | always |
| `explanation.why.groups[].rows[].line` | string | always |
| `explanation.why.groups[].rows[].nodes` | integer | always |
| `explanation.why.groups[].rows[].share` | number | always |
| `explanation.why.groups[].rows[].steps` | integer | always |
| `explanation.why.groups[].rows[].subject` | null | always |
| `explanation.why.groups[].steps` | integer | always |
| `explanation.why.headline` | string | always |
| `explanation.why.omitted` | integer | always |
| `explanation.why.partial` | boolean | always |
| `hint` | string | always |
| `run` | object | always |
| `run.attempts` | integer | always |
| `run.cancellation` | null | always |
| `run.client_run_id` | null | always |
| `run.command` | string | always |
| `run.elapsed_seconds` | number | always |
| `run.failure` | object | always |
| `run.failure.error` | string | always |
| `run.failure.node` | string | always |
| `run.files` | array | always |
| `run.files[]` | string | always |
| `run.finished_at` | string | always |
| `run.job` | string | always |
| `run.plan_reasons` | object | always |
| `run.plan_reasons.nodes` | integer | always |
| `run.plan_reasons.omitted` | integer | always |
| `run.plan_reasons.reasons` | array | always |
| `run.plan_reasons.reasons[]` | object | always |
| `run.plan_reasons.reasons[].category` | string | always |
| `run.plan_reasons.reasons[].example` | string | always |
| `run.plan_reasons.reasons[].nodes` | integer | always |
| `run.plan_reasons.reasons[].steps` | integer | always |
| `run.plan_reasons.reasons[].subject` | null | always |
| `run.plan_reasons.steps` | integer | always |
| `run.previous_attempts` | array | always |
| `run.previous_attempts[]` | object | always |
| `run.previous_attempts[].attempt` | integer | always |
| `run.previous_attempts[].run_id` | string | always |
| `run.previous_attempts[].stop_reason` | string | always |
| `run.principal` | null | always |
| `run.retries` | integer | always |
| `run.run_id` | string | always |
| `run.schedule_id` | null | always |
| `run.scheduled_at` | null | always |
| `run.started_at` | string | always |
| `run.status` | string | always |
| `run.steps_cached` | integer | always |
| `run.steps_executed` | integer | always |
| `run.steps_total` | integer | always |
| `run.target` | string | always |
| `run.trigger_reason` | null | always |
| `run.triggered_by` | string | always |
| `steps` | array | always |
| `steps[]` | object | always |
| `steps[].artifact_format` | null \| string | always |
| `steps[].artifact_path` | null \| string | always |
| `steps[].artifact_size_bytes` | integer \| null | always |
| `steps[].attempts` | integer | always |
| `steps[].batch` | null | always |
| `steps[].cpu_seconds` | null \| number | always |
| `steps[].elapsed_seconds` | null \| number | always |
| `steps[].error` | null \| string | always |
| `steps[].error_message` | null \| string | always |
| `steps[].error_phase` | null \| string | always |
| `steps[].error_traceback` | null \| string | always |
| `steps[].error_type` | null \| string | always |
| `steps[].max_rss_bytes` | integer \| null | always |
| `steps[].node_id` | string | always |
| `steps[].output_tail` | array \| null | always |
| `steps[].partition_key` | null | always |
| `steps[].rows` | null | always |
| `steps[].run_hash` | string | always |
| `steps[].sink_seconds` | null | always |
| `steps[].status` | string | always |
| `steps[].worker_exit` | null | always |
| `steps_page` | object | always |
| `steps_page.nodes` | array | always |
| `steps_page.nodes[]` | object | always |
| `steps_page.nodes[].calls` | integer | always |
| `steps_page.nodes[].elapsed_seconds` | number | always |
| `steps_page.nodes[].failed` | integer | always |
| `steps_page.nodes[].keys` | integer | always |
| `steps_page.nodes[].node_id` | string | always |
| `steps_page.nodes[].status` | string | always |
| `steps_page.total` | integer | always |
| `steps_page.truncated` | boolean | always |
<!-- END GENERATED schema history_requeued -->

<!-- BEGIN GENERATED schema history_attempt -->
| Key | Type | Present |
|---|---|---|
| `explanation` | object | always |
| `explanation.attempts` | null | always |
| `explanation.cancellation` | null | always |
| `explanation.counts` | object | always |
| `explanation.counts.attempts` | null | always |
| `explanation.counts.attempts_line` | null | always |
| `explanation.counts.cached` | integer | always |
| `explanation.counts.failed` | integer | always |
| `explanation.counts.nodes` | null | always |
| `explanation.counts.not_reached` | integer | always |
| `explanation.counts.not_reached_line` | null | always |
| `explanation.counts.not_reached_nodes` | null | always |
| `explanation.counts.ok` | integer | always |
| `explanation.counts.outcome` | string | always |
| `explanation.counts.planned` | string | always |
| `explanation.counts.ran` | integer | always |
| `explanation.counts.retries` | null | always |
| `explanation.counts.steps` | integer | always |
| `explanation.counts.units` | string | always |
| `explanation.outputs` | array | always |
| `explanation.progress` | null | always |
| `explanation.requested_by` | string | always |
| `explanation.slowest` | array | always |
| `explanation.steps` | string | always |
| `explanation.target_line` | string | always |
| `explanation.target_outcomes` | array | always |
| `explanation.targets` | array | always |
| `explanation.why` | object | always |
| `explanation.why.groups` | array | always |
| `explanation.why.headline` | string | always |
| `explanation.why.omitted` | integer | always |
| `explanation.why.partial` | boolean | always |
| `run` | object | always |
| `run.attempts` | integer | always |
| `run.cancellation` | null | always |
| `run.client_run_id` | null | always |
| `run.command` | string | always |
| `run.elapsed_seconds` | null | always |
| `run.failure` | null | always |
| `run.files` | array | always |
| `run.finished_at` | null | always |
| `run.job` | null | always |
| `run.parent_run_id` | string | always |
| `run.principal` | null | always |
| `run.retries` | integer | always |
| `run.run_attempt` | integer | always |
| `run.run_id` | string | always |
| `run.schedule_id` | null | always |
| `run.scheduled_at` | null | always |
| `run.started_at` | string | always |
| `run.status` | string | always |
| `run.steps_cached` | integer | always |
| `run.steps_executed` | integer | always |
| `run.steps_total` | null | always |
| `run.stop_reason` | string | always |
| `run.target` | null | always |
| `run.trigger_reason` | null | always |
| `run.triggered_by` | null | always |
| `steps` | array | always |
| `steps_page` | object | always |
| `steps_page.total` | integer | always |
| `steps_page.truncated` | boolean | always |
<!-- END GENERATED schema history_attempt -->

### Cancelling a queued run

`barca cancel RUN_ID --server URL --json` reports `status: "cancelled"` after removing a
queued run durably, including a run just requeued during drain. A running run reports
`status: "cancelling"` until its workers or partition/batch resolvers stop. Resolver
shutdown allows one second for SIGTERM, then kills and reaps the subprocess.

<!-- BEGIN GENERATED schema cancel_queued -->
| Key | Type | Present |
|---|---|---|
| `run_id` | string | always |
| `status` | string | always |
<!-- END GENERATED schema cancel_queued -->
### Worker-fatal run diagnostics

Experimental in 0.47.0: history detail may include `explanation.worker_failures`, an array
of `{node_id: string, first_error: string, retry_error: string | null, recovered: boolean}`.
Absent when no evidence was recorded. Original causes survive successful retries; a terminal
retry error belongs to the same step, and independent failures remain separate. CLI history
and the server run page use this recorded evidence, not an inference from retry counts.

Worker-fatal diagnostics retain at most 64 affected steps per run and 8 KiB per error,
with a `[barca] diagnostic truncated` marker when error text is shortened. The list is
not exhaustive at its limit; ordinary step outcomes still report independent failures.

### Server run options (experimental)

`POST /runs` accepts `include_manual: true` to bring Manual assets up to date and execute
Manual upstream tasks. Current assets still use the cache. Hard refresh (`refresh_all`)
remains unconditional for unheld nodes. Paused inputs remain held; `allow_paused` permits
paused targets, not their paused inputs. Prediction and queue recovery preserve these rules.

See also: [Scripts and AI Agents](/reference/agents/) (how to drive barca from scripts and agents), [Agent Skill](/reference/agent-skill/).

---

*The same text is in your terminal: `barca docs contract`.*
