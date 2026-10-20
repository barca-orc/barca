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
original data on failure, while existing shared-state replacement keeps the
previous local copy. A future destructive migration must separately define and
test a recoverable backup before it is accepted; this policy does not authorize
one. Unsupported versions are refused in place and are never automatically
restored, downgraded or reset. Upgrade to a compatible Barca release to open them.

## Shared history and older binaries

Downloaded state is checked before migration or replacement. Both the local and
downloaded database must be compatible before their rows are merged. A refused
download leaves the local history intact. Compatible unknown nullable/defaulted
columns remain untouched; incompatible required columns are still rejected.

Version checks cannot make an already-published older binary enforce a marker it
does not understand. Version 1 therefore remains additive and compatible with
the previous release's schema. Later format changes must account for older
shared-state writers and may need a deployment upgrade boundary.

## Verification and remaining scope

Regression tests cover legacy run/log/schedule/materialization preservation,
transaction rollback and reopening after an interrupted migration, negative and
future versions, refusal through inspection and cache readers, and refusal of
download/local merges without losing either history. Existing remote-state,
snapshot and older-schema migration tests remain part of the acceptance suite.

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
hash; legacy rows default to 0. Shared-history carry preserves both `output_hash` and
`bytes_changed` rather than recomputing the event. See [materialization-digests.md](materialization-digests.md).

Format 1 also gains nullable columns after its first release. `runs` carries `triggered_by`
(`manual`, `api`, `schedule`, `client`), `scheduled_at`, `schedule_id` and `outcome_json` (a
run `barca serve` started: its full outcome, so `GET /runs/{id}` answers after a restart);
`schedule_state` carries `paused_at` and `paused_until` for a paused schedule. They are listed
in `FORMAT_1_ADDITIONS` (`db_schema.rs`). A format-1 database that lacks one is brought up to
date by the same transactional additive migration as a legacy database and stays format 1, so
a previous release still opens it; a current one is still checked without being written. Rows
written before have `NULL` there (`triggered_by: null`). Shared-history carry copies the new
`runs` columns like the others.
