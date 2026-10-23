# Metadata schema compatibility

Scope: metadata format compatibility, not the full table/DDL contract.
Status: implemented. Specification version: 1 (database versions are described below).
Conformance: `crates/barca-core/src/db_schema.rs`, `state_validate.rs`, and snapshot/carry
tests in `db.rs`. See [boundary inventory](README.md).

The metadata database contains durable run history, logs and schedules alongside
rebuildable cache/cost data. A version mismatch never permits deleting or
recreating it. Rust owns this boundary; no new CLI or Python configuration is
introduced.

## Version 1 and legacy databases

SQLite's built-in `PRAGMA user_version` identifies the format. Its default value
0 is the legacy additive schema; the first versioned format is 1. Negative and
future versions are refused with an actionable error. Reading that field does
not create a table, change data or add a configuration knob. SQLite salvage
preserves it along with the database header, unlike a separate version row that
can be lost while history rows remain recoverable. The check precedes writes,
including inspection's private snapshots and shared-state validation/carry.

The version-0 to version-1 migration uses the existing additive table/column
definitions. It preserves every row, validates the resulting readable/writable
schema, and records the version last, all in one database transaction. Missing
columns are discovered explicitly; genuine DDL errors are not swallowed as if
they were duplicate-column errors. An error rolls the transaction back. A process
interrupted before commit leaves the old schema and its rows available; after
commit it leaves version 1. An already-current schema is checked without writing
to it. Inspection of an unversioned database does not migrate the source.

These additive changes need no new backup file: the transaction preserves the
original data on failure, and `barca state restore` keeps the database it
replaces as `metadata.db.pre-restore`. A future destructive migration must separately define and
test a recoverable backup before it is accepted; this policy does not authorize
one. Unsupported versions are refused in place and are never automatically
restored, downgraded or reset. Upgrade to a compatible Barca release to open them.

## Backups, restore and older binaries

The shared history (download, merge, replace) was removed in 0.26.0 (RFC-0009).
A backup restored by `barca state restore` is checked before migration or
replacement: a staged copy must pass the same validation, its schema version must
be supported, and only the staged copy is migrated. A refused copy leaves the
local history intact. Each backup's sidecar records its `db_format` (the
`PRAGMA user_version` of the copy). Compatible unknown nullable/defaulted columns
remain untouched; incompatible required columns are still rejected.

Version checks cannot make an already-published older binary enforce a marker it
does not understand. Version 1 therefore remains additive and compatible with
the previous release's schema. Later format changes must account for backups
written by older binaries and may need a deployment upgrade boundary.

## Verification and remaining scope

Regression tests cover legacy run/log/schedule/materialization preservation,
transaction rollback and reopening after an interrupted migration, negative and
future versions, refusal through inspection and cache readers, and refusal of an invalid backup
by `barca state restore` without touching the local history. Snapshot and
older-schema migration tests remain part of the acceptance suite.

Process protocol handshakes, plan JSON versioning and wire/batch conformance are
separate slices of issue #82. This change does not complete that entire issue or
merge the earlier destructive-reset proposal in PR #258.

An explicit `barca history --reconcile RUN_ID` may record a `running` row as `interrupted`
after the operator verifies the run stopped. It refuses positive live-owner evidence and
terminal rows, preserves materializations/progress, and records no invented end timestamp
or duration. Unknown-owner history reads remain conservative. CLI behavior belongs to
[the CLI contract](../crates/barca-cli/docs/contract.md); executable recovery controls live in
[test_history_reconcile.py](../python/tests/test_history_reconcile.py). This is a recovery
operation within format 1, with no schema change.

Materializations also carry `bytes_changed INTEGER DEFAULT 0`, an additive format-1
column. It records a byte mismatch when inserting a recompute with the same node/run
hash; legacy rows default to 0. See [materialization-digests.md](materialization-digests.md).

### Additions within format 1

Format 1 is additive: a release may add a nullable column or a table to it without a new
version number. A format-1 database written by an earlier release that lacks one gets the
same additive migration as a legacy database, in one transaction, keeping every row; an older
release keeps reading the result, because it leaves unknown nullable columns and unknown
tables alone. A backup being restored is accepted on the same terms before it is migrated.
`barca status`, which reads the newer columns, completes the schema first.

What format 1 gained since it first shipped, all listed in `db_schema.rs`
(`ADDED_COLUMNS`, which lists `MATERIALIZATION_COLUMNS`, `RUN_COLUMNS`, `SCHEDULE_STATE_COLUMNS`
and `LOG_COLUMNS` by table; `INDEXES`; `ADDED_TABLES`), which `init_db`, the check that runs
before the migration (`Schema::columns_fit`) and the additive migration (`additions_missing` /
`add_to_current_schema`) all read. 0.29.2 to 0.30.0 left `logs` out of that check, so they refused
every database from before 0.29.2 (#629):

- `materializations.bytes_changed` (0.24.0), the first such addition; 0.24.0 to 0.24.2 refused
  format-1 databases without it.
- `materializations.evicted_at`, `materializations.last_read_at` and the index `idx_mat_path`
  (0.25.0, the cache size limit; see below).
- `runs.job` (TEXT, nullable, 0.26.0): the canonical resolved job ids of a finished run, written
  with its outcome, and the index `idx_runs_job (job, id)`. Rows from earlier releases, and runs
  that never finished, keep `NULL`.
- `notifications (job, integration, posted_at, state, message_ref)` (0.26.0), primary key
  `(job, integration)`: the last post a notification integration (Slack, RFC-0010) made about
  a job. `posted_at` is UTC text in SQLite's `datetime('now')` format; `state` is `failing`,
  `still_failing`, `recovered` or `succeeded`; `message_ref` is empty for a webhook post and
  reserved for an integration whose posts have ids.
- `pending_uploads` (0.26.0, #390): one row per artifact-store object whose upload failed after
  its step succeeded (`store_path` primary key, `node_id`, `run_hash`, `run_id`, `local_path`,
  `host`, `error`, `attempts`, timestamps). A database without it (opened read-only, or a copy
  being validated) reads as having no pending uploads. Only rows of this host are read, retried
  or shown. Older binaries ignore the table.
- `runs.triggered_by` (`manual`, `api`, `schedule`, `client`), `runs.scheduled_at`,
  `runs.schedule_id` and `runs.outcome_json` (all TEXT, nullable, 0.27.0, #393): what started a
  run, the tick and schedule of a scheduled run, and the full outcome of a run `barca serve`
  started, so `GET /runs/{id}` answers after a restart. Rows written before have `NULL` there
  (`triggered_by: null`). From 0.30.2 (#623) a partitioned target of more than 1000 keys is
  stored as the `_barca_partitions` summary (the keys the run computed, counts of the rest), so
  the row's size follows what the run computed; rows written before keep every key and are
  read as they are. No migration.
- `runs.client_run_id` (TEXT, nullable, 0.27.0, RFC-0007 §4.1): on a server, the run id a laptop
  gave a run it posted to the ledger (`triggered_by: client`); a repeated post of that run is
  recognised by it.
- `ledger_posts` (0.27.0, RFC-0007): on a machine that runs against a server's ledger, one row per
  run it posted or has yet to post (`client_run_id` primary key, `server`, the post `body` as
  JSON, `state` `unsynced` or `shared`, `server_run_id`, the last `error`, `attempts`,
  `created_at`, `updated_at`). Since 0.29.0 (#570) the body also carries `bytes_unchecked`, the
  post's base node ids whose changed bytes are not a warning (a `Manual` asset), so a post sent
  later warns as one sent at the end of its run; it is absent when empty and in older rows, and
  never sent to the server. Unlike the tables above it is created the first time a run is
  posted, never by a reader or by `init_db`; a database without it has posted nothing, and
  readers treat that as empty.
- `schedule_state.paused_at` and `schedule_state.paused_until` (INTEGER unix seconds, nullable,
  0.27.0, #435): a schedule paused with `POST /schedule/{id}/pause`; `paused_until` NULL means
  until it is resumed.
- `materializations.components` (TEXT, nullable, 0.29.0, #558): a JSON object with a short digest
  (16 hex digits of SHA-256) of each component of a successful row's run hash (`code`,
  `decorator argument NAME`, `decorator NAME`, `helper FILE:NAME`, `input ID`, `sensor ID`,
  `env NAME`, `kind`), which `barca status` and `--dry-run` compare to say what changed. Never
  part of a cache key. Older rows, failed rows and rows a server's ledger receives from a client
  keep `NULL`, and their nodes report a plain `changed`.
- `schedule_failures (node_id, failures, failed_nodes, mark)` (0.28.0, RFC-0008 rule 9), primary
  key `node_id`: the consecutive failed ticks of one scheduled node, the nodes that failed in the
  last counted tick (a JSON array), and the highest `materializations.id` when it was counted. A
  scheduled node with no failures has no row. Older binaries ignore the table.
- Partial indexes on `materializations` (0.29.3), each holding only a few rows, so reads that
  used to scan the table are index lookups: `idx_mat_changed (node_id) WHERE bytes_changed = 1`
  (a node's last change in bytes for the same run hash, `barca status`), `idx_mat_attempts
  (run_id, attempts) WHERE attempts IS NULL OR attempts != 1` (a run's attempt count, `barca
  history`: the run's rows counted by `idx_mat_run`, corrected by these) and `idx_mat_evicted
  (artifact_path) WHERE evicted_at IS NOT NULL` (the results the cache size limit deleted, every
  dry run). Opening an older database builds them, a few seconds for 400k rows; older binaries
  ignore them. A node's partition-key rows (`<node>[...]`) are read as the range `node_id >=
  '<node>[' AND node_id < '<node>\'` of `idx_mat_node_run`.
- `run_queue (run_id, seq, state, spec, queued_at)` (0.29.4), primary key `run_id`: the runs
  `barca serve` accepted and has not finished recording, in the order `seq`. `state` is `queued`,
  `running`, or `draining` (0.31.0: in flight when the server was told to stop); `spec` is the server's description of the run (JSON: command, targets, nodes,
  files, cache policy, trigger, partition selection, freshness rules, accepted time);
  `queued_at` is RFC 3339 UTC. A run leaves the table when it has recorded its outcome. A server
  that starts resumes the `queued` rows in order and records a `running` row (its server was
  killed) as interrupted; a `draining` row (its server was killed during its drain) is put back
  in the queue once, counted in `spec.kill_requeues`, and interrupted the next time. Older
  binaries read `draining` as `queued`. Only `barca serve` writes it; readers of a database without it find the
  queue empty, and older binaries ignore it.
- `server_state (key, value)` (0.29.4), primary key `key`: settings `barca serve` keeps across
  restarts. `queue_paused_at` (unix seconds) is present while the run queue is paused.
- `logs.stream` (`stdout` / `stderr`) and `logs.ts` (RFC 3339 UTC, milliseconds), both TEXT,
  nullable (0.29.2): which stream a recorded line came from and when it arrived. Lines recorded
  before keep `NULL` in both and read as stdout with no time (`barca logs --stream stdout`
  includes them).

Conformance: `a_format_1_database_from_before_bytes_changed_gains_it_in_place` and
`a_format_1_database_from_0_24_3_gains_everything_added_since` and
`a_format_1_database_from_before_an_added_column_gets_it_and_keeps_its_rows` in `db_schema.rs`,
`the_table_list_is_exactly_the_schema_init_db_creates` in `state_validate.rs`, the job history
tests in `crates/barca-core/src/telemetry/history.rs`, and
`python/tests/test_upgrade_format1_columns.py`, and `python/tests/test_metadata_compat.py`, which
opens a database written by a release of every minor from 0.20 (legacy format 0) to the newest
(committed fixtures, rebuilt by `scripts/make-metadata-fixtures.py`) with `barca serve
--read-only`, `history`, `logs`, `status`, a cached `get` and `barca serve`.

## Cache size limit columns (RFC-0012)

Materializations carry `evicted_at TEXT` and `last_read_at TEXT`, additive format-1 columns, and
the index `idx_mat_path` on `artifact_path`. `evicted_at` records when this machine deleted the
artifact file to stay under `[cache] max_size` (NULL while the file is kept); `last_read_at`
records when a step, `barca get` or `barca sql` last read the result. Both describe this
machine's disk: they are not in `STEP_COLUMNS` (a backup need not have them), and
schema validation treats them as nullable columns an older barca may lack. Rows are never deleted
by eviction.

A version-1 database written by an earlier release that lacks an additive column or index of
this release (these, or `bytes_changed`) gains them when it is opened for writing
(`db::init_schema`): the missing columns and indexes are added and the schema validated in one
transaction, rolled back on error, with every row preserved and the version left at 1. A
database that already has them is only read. Conformance: the schema tests in
`crates/barca-core/src/gc/tests.rs` and `db_schema.rs`.
