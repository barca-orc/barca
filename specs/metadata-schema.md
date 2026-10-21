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
(`MATERIALIZATION_COLUMNS`, `RUN_COLUMNS`, `SCHEDULE_STATE_COLUMNS`, `INDEXES`, `ADDED_TABLES`), which both `init_db` and the
additive migration (`additions_missing` / `add_to_current_schema`) read:

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
  (`triggered_by: null`).
- `runs.client_run_id` (TEXT, nullable, 0.27.0, RFC-0007 §4.1): on a server, the run id a laptop
  gave a run it posted to the ledger (`triggered_by: client`); a repeated post of that run is
  recognised by it.
- `ledger_posts` (0.27.0, RFC-0007): on a machine that runs against a server's ledger, one row per
  run it posted or has yet to post (`client_run_id` primary key, `server`, the post `body` as
  JSON, `state` `unsynced` or `shared`, `server_run_id`, the last `error`, `attempts`,
  `created_at`, `updated_at`). Unlike the tables above it is created the first time a run is
  posted, never by a reader or by `init_db`; a database without it has posted nothing, and
  readers treat that as empty.
- `schedule_state.paused_at` and `schedule_state.paused_until` (INTEGER unix seconds, nullable,
  0.27.0, #435): a schedule paused with `POST /schedule/{id}/pause`; `paused_until` NULL means
  until it is resumed.

Conformance: `a_format_1_database_from_before_bytes_changed_gains_it_in_place` and
`a_format_1_database_from_0_24_3_gains_everything_added_since` and
`a_format_1_database_from_before_an_added_column_gets_it_and_keeps_its_rows` in `db_schema.rs`,
`the_table_list_is_exactly_the_schema_init_db_creates` in `state_validate.rs`, the job history
tests in `crates/barca-core/src/telemetry/history.rs`, and
`python/tests/test_upgrade_format1_columns.py`.

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
