# Boundary specifications

A boundary specification defines what callers can rely on. Update the specification,
its executable conformance checks and explanatory documentation in the same PR when
changing that boundary. Specification versions identify contract revisions; they are
independent of Barca package versions. A design plan or RFC does not promise shipped
behavior.

| Boundary | Native specification | Status / version | Executable conformance |
| --- | --- | --- | --- |
| HTTP routes and wire bodies | [server-api.openapi.yaml](server-api.openapi.yaml) (OpenAPI 3.1 / JSON Schema 2020-12) | Current implemented surface, including the RFC-0007 §3.4 run contract and inspection routes and the ledger routes; specification 7 | [Rust router conformance](../crates/barca-server/tests/openapi.rs), [real Python Client conformance](../python/tests/test_http_openapi_client.py) |
| Python decorator grammar and execution model | [user-api.allium](user-api.allium), [decisions](user-api-decisions.md) | Mixed implemented grammar and design rules; specification 1. Ignored grammar cases and unresolved decisions are aspirations. | [grammar_spec.rs](../crates/barca-core/tests/grammar_spec.rs), [project star-import provenance](../python/tests/test_static_barca_bindings.py); runtime acceptance remains in core and Python integration suites |
| Materialization digests and recompute checks | [materialization-digests.md](materialization-digests.md) | Implemented producer/recompute scope and store mismatch repair (RFC-0009); specification 1. Server acceptance remains proposed. | [CLI digest conformance](../python/tests/test_materialization_digests.py), persistence Rust tests |
| Artifact formats and CSV outputs | [artifact-formats.md](artifact-formats.md) | Implemented; specification 1 | [CSV conformance](../python/tests/test_csv_outputs.py), [decorator grammar](../crates/barca-core/tests/grammar_spec.rs) |
| Artifact upload ownership and receipts | [artifact-upload-receipts.md](artifact-upload-receipts.md) | Implemented; specification 1 | [controlled overlaps](../python/tests/test_upload_snapshot_receipts.py), [cancellation/crash lifecycle](../python/tests/test_remote_cancel.py), [fail-soft pending uploads](../python/tests/test_fail_soft_uploads.py), transfer and persistence Rust tests |
| Cache decisions, recorded progress and interrupted runs | [cache-decisions.md](cache-decisions.md) | Implemented; specification 1 | [cache.rs](../crates/barca-core/src/cache.rs) decision tests, [conditional](../python/tests/test_conditional_helper_hashing.py) and [star-import](../python/tests/test_helper_star_hashing.py) hashing, [progress](../python/tests/test_incremental_persist.py), [terminal outcomes](../python/tests/test_terminal_persist.py), [run_owner.rs](../crates/barca-core/src/run_owner.rs), [reconciliation](../python/tests/test_history_reconcile.py) |
| Metadata format compatibility | [metadata-schema.md](metadata-schema.md) | Implemented compatibility policy; database format 1 (with additive nullable columns and tables, e.g. the cache-limit columns `evicted_at` and `last_read_at` from v0.25.0), legacy 0 | [db_schema.rs](../crates/barca-core/src/db_schema.rs), [state_validate.rs](../crates/barca-core/src/state_validate.rs), restore tests in [backup.rs](../crates/barca-core/src/backup.rs), snapshot tests in [db.rs](../crates/barca-core/src/db.rs), [pending_uploads.rs](../crates/barca-core/src/pending_uploads.rs), snapshot/carry tests in [db.rs](../crates/barca-core/src/db.rs), cache-limit schema tests in [gc/tests.rs](../crates/barca-core/src/gc/tests.rs) |
| Metadata backups in the store and `barca state restore` | No native specification: [RFC-0009](../site/src/content/docs/rfcs/0009-store-backup-and-restore.md) (accepted) for the design; the layout, names, sidecar and restore checks in [`barca docs remote`](../crates/barca-cli/docs/remote.md) and the `state backups` / `state restore` JSON in [the CLI contract](../crates/barca-cli/docs/contract.md); the `python -m barca._state` verbs in [_state.py](../python/barca/_state.py) | Implemented in 0.26.0; backup names carry milliseconds and a suffix from 0.26.1 (#487), to-the-second names still read | [backup.rs](../crates/barca-core/src/backup.rs) tests, [store backends](../python/tests/test_state_backends.py), [backup and restore CLI](../python/tests/test_store_backup_restore.py), [two-directory e2e](../tests/integration/test_remote_state.sh) |
| Signed URLs | No native specification: [RFC-0011](../site/src/content/docs/rfcs/0011-signed-urls.md) (accepted); `barca.signed_url` and `barca get -o url` in [`barca docs remote`](../crates/barca-cli/docs/remote.md) and [the CLI contract](../crates/barca-cli/docs/contract.md); the `sign` op of `python -m barca._value` in [_value.py](../python/barca/_value.py) | Implemented in 0.26.0 | [signed URL tests](../python/tests/test_signed_urls.py), CLI snapshots |
| Slack notifications | No native specification: [RFC-0010](../site/src/content/docs/rfcs/0010-slack-notifications.md) (accepted); variables and messages in [`barca docs telemetry`](../crates/barca-cli/docs/telemetry.md) | Implemented in 0.26.0; the message text is not a stable surface | [slack.rs](../crates/barca-core/src/telemetry/slack.rs) tests, [webhook tests](../python/tests/test_telemetry_slack.py) |
| Task results and recorded stdout | [task-output.md](task-output.md) | Implemented; specification 2 | [Task/stdout conformance](../python/tests/test_task_logs.py), CLI snapshots and Rust log persistence tests |
| Project prerequisites and credential diagnostics | [doctor.md](doctor.md) | Implemented; specification 1, experimental check data | [no-execution controls](../python/tests/test_doctor.py), [identity adapters](../python/tests/test_identity.py), CLI snapshots |
| CLI flags, messages and JSON envelopes | [existing CLI contract](../crates/barca-cli/docs/contract.md) and native help/JSON snapshots | Current implemented contract, existing ownership retained; no separate specification version. Includes the v0.25.0 output shape (`get` paths by default, `-o path\|value`, `--partition`; stable, except a partitioned target's partial failure, experimental) and the cache size limit (`barca gc`, `[cache] max_size`; experimental) | [CLI integration tests](../python/tests/test_cli_contract.py), Rust CLI snapshot tests |
| Worker/transfer socket protocol | [worker-protocol.md](worker-protocol.md) | Implemented in v0.24.0; specification 3 / protocol 3 | [protocol tests](../crates/barca-core/src/protocol.rs), [Python/CLI negotiation](../python/tests/test_protocol_handshake.py), [batched runtime/crash controls](../python/tests/test_compact_worker_protocol.py) |

The existing CLI normative document and snapshots remain in their established locations.
This inventory does not duplicate or migrate them. Future boundary specifications should
live here in their native format (OpenAPI, grammar, SQL/DDL or Markdown as appropriate);
there is no requirement to rewrite a grammar or a reviewed proposal as YAML.

Each new normative specification needs a short scope, implemented/proposed status,
independent specification version and links to the tests enforcing it. Explanatory manuals
and site pages should link to that owner and defer on wire details. Tests should link back
to the specification. Test changes should include controls that detect missing fields,
wrong statuses and additional routes, rather than only repeating happy-path examples.

## Implementation notes

[notes/](notes/) holds the implementation notes individual PRs left behind (a plan, the
invariant, the regression evidence). They are not boundaries, they are not indexed here, and
nothing in them is a promise: the PR that made each one is the record. A new boundary goes in
this directory and in the table above; anything else goes in `notes/` or stays in its PR.
Each note opens with one line naming the PR that made it and whether it still matches the code
as of the date given. A note whose subject is removed is deleted, not kept as history.

[DIRECTION.md](../DIRECTION.md) states what is being worked on and why.
