# Specifications

Start here for what Barca promises and what is still design. Terms are defined in the
[glossary](../GLOSSARY.md); product direction lives in [DIRECTION.md](../DIRECTION.md).
This cleanup describes the implemented 0.33.0 baseline at `fa396a5d` and explicitly identified
gaps. [PR #658](https://github.com/barca-orc/barca/pull/658) is a pending historical-selection
fix, not assumed merged. A specification describes its stated scope; a passing grammar
check or benchmark is not evidence for unrelated runtime behavior.

## Product intent

[Product experience](experience.md) records the approved first-use, success, progress, logging,
failure and noninteractive-command bounds. It specifies observable outcomes while leaving
presentation and internal mechanisms open. [Project resource bounds](proposals/resources.md)
and [agent interaction](proposals/agents.md) record the ongoing requirements discussion,
including an anonymized synthetic navigation case. Their status is approved intent with partial
implementation coverage, not an assertion that all defaults have shipped.
[Operations](proposals/operations.md) records freshness requests, retry/snapshot semantics,
drain/recovery and artifact retention. Resume the discussion at the
[remaining questions](proposals/requirements-handoff.md), which separates unanswered proposals
from approved decisions.

## Behavioral contracts

| Owner | Scope | Status / evidence |
| --- | --- | --- |
| [Freshness](freshness.md) | Sensor meanings, partition membership, current selection and freshness policy | Implemented rules; known historical-selection defect #657; named invariants and real integration checks |
| [Execution](execution.md) | Runs, logical keys vs calls/processes, batches, partial failure, cancellation and recovery | Implemented rules; overlapping asset sharing and bounded full execution remain gaps |
| [Inspection](inspection.md) | Validity, execution, availability and current counts | Implemented behavior; explicit checking and independent status tracking are approved design only |
| [Performance](performance.md) | Workload-specific resource budgets and measurement scope | Measured warm budgets; cold/inspection bounds remain targets |
| [Python runtime](python-runtime.md) | Cached modules, process-local DuckDB state and temporary input views | Implemented; real worker connection controls |

Each owner links its exact conformance sources and records known limitations. Existing wire
schemas stay authoritative for field names/types/status enums; prose does not silently add
an interface. New behavior changes its owner, explanatory manual/site pages and conformance
in the same PR. A known bug is identified as a deviation, not rewritten as intended behavior.

## Interface contracts

| Interface | Owner | Status / revision | Executable conformance |
| --- | --- | --- | --- |
| HTTP routes and bodies | [OpenAPI 3.1 / JSON Schema](server-api.openapi.yaml) | Implemented, specification 13 | [Router](../crates/barca-server/tests/openapi.rs), [Python client](../python/tests/test_http_openapi_client.py) |
| Python declarations | [Allium overview](user-api.allium), [rationale](user-api-decisions.md) | Implemented declaration concepts, specification 2; hand-written grammar checks, no Allium compiler gate | [Grammar](../crates/barca-core/tests/grammar_spec.rs), [argument owner](../crates/barca-core/src/decorator_args.rs), [bindings](../python/tests/test_static_barca_bindings.py) |
| CLI flags, envelopes and output | [CLI contract](../crates/barca-cli/docs/contract.md), existing native help/JSON snapshots | Implemented, stable/experimental surfaces identified there | [CLI integration](../python/tests/test_cli_contract.py), [Rust snapshots](../crates/barca-cli/src/contract.rs) |
| Worker/transfer sockets | [Protocol](worker-protocol.md) | Specification 5 / protocol 5; sensor signals/compact observations in 0.33.0, batching in 0.32.0 | [Wire](../crates/barca-core/src/protocol.rs), [negotiation](../python/tests/test_protocol_handshake.py), [batch/crash](../python/tests/test_compact_worker_protocol.py) |
| Batched partitions | [Execution](execution.md), [RFC-0013](../site/src/content/docs/rfcs/0013-batched-partitions.md), existing wire owners | Implemented in 0.32.0; grouping/observability experimental | [Real worker/server](../python/tests/test_batched_partitions.py), [grouping](../crates/barca-core/src/batch.rs) |
| Materialization digests | [Digests](materialization-digests.md) | Implemented producer, repair and ledger acceptance, specification 2 | [CLI](../python/tests/test_materialization_digests.py), [ledger](../python/tests/test_server_ledger.py) |
| Artifact formats and dataset sinks | [Formats](artifact-formats.md) | Implemented, specification 2 | [CSV](../python/tests/test_csv_outputs.py), [dataset sinks](../python/tests/test_dataset_sinks.py) |
| Upload ownership/receipts | [Receipts](artifact-upload-receipts.md) | Implemented, specification 1; concurrent input read isolation is explicitly not guaranteed | [Controlled overlaps](../python/tests/test_upload_snapshot_receipts.py), [cancellation](../python/tests/test_remote_cancel.py), [pending uploads](../python/tests/test_fail_soft_uploads.py) |
| Cache decisions and durability | [Cache decisions](cache-decisions.md) | Implemented, specification 2; selection caveat in freshness owner | [Core decisions](../crates/barca-core/src/cache.rs), [progress](../python/tests/test_incremental_persist.py), [terminal writes](../python/tests/test_terminal_persist.py), [reconciliation](../python/tests/test_history_reconcile.py) |
| Metadata compatibility | [Metadata](metadata-schema.md) | Implemented policy specification 1; database format 1, legacy 0 | [Admission](../crates/barca-core/src/db_schema.rs), [validation](../crates/barca-core/src/state_validate.rs), [compatibility](../python/tests/test_metadata_compat.py) |
| Task results and recorded output | [Task output](task-output.md) | Implemented, specification 4 | [Task/stdout](../python/tests/test_task_logs.py), [live logs](../python/tests/test_live_logs.py) |
| Project/credential diagnostics | [Doctor](doctor.md) | Implemented, specification 1; check data experimental | [No-execution](../python/tests/test_doctor.py), [identity](../python/tests/test_identity.py), [server](../python/tests/test_debugging_problems.py) |

Backup/restore layout and signed URLs remain owned by the [remote manual](../crates/barca-cli/docs/remote.md)
and CLI contract, with [backup tests](../python/tests/test_store_backup_restore.py),
[backend checks](../python/tests/test_state_backends.py) and [signed URL checks](../python/tests/test_signed_urls.py).
Their accepted designs are [RFC-0009](../site/src/content/docs/rfcs/0009-store-backup-and-restore.md)
and [RFC-0011](../site/src/content/docs/rfcs/0011-signed-urls.md).
Slack configuration/behavior remains in the [telemetry manual](../crates/barca-cli/docs/telemetry.md),
with [webhook controls](../python/tests/test_telemetry_slack.py); message prose is not a stable surface.
These owners have not been relocated or copied into new schemas.

## Approved design and unapproved ideas

[Current state and execution ownership](proposals/current-state-and-execution.md) records
approved behavior for ordered publication, shared work, cancellation, pending reconciliation,
checking and bounded execution. Its acceptance scenarios are obligations for implementation,
not claims of passing conformance. [The TLA+ models](models/README.md) check only a finite
abstraction of the approved ownership/order rules. Model checking is not a proof of Rust
conformance or performance.

[API ideas](proposals/api-ideas.md) are unscheduled, unapproved alternatives. Keep them separate
from both implemented contracts and accepted design. [Historical implementation notes](notes/README.md)
retain selected rationale/reproductions; stale superseded plans were removed and remain in
linked PR history. They are not current contract owners.

## Maintaining a specification

Use the native format appropriate to the subject: OpenAPI/JSON Schema for HTTP, the existing
Allium declaration overview, Markdown transition tables and named invariants for behavior,
and a small model for concurrency. Specification revisions are independent of package versions.
Do not migrate formats just to make everything look uniform.

A behavioral owner states scope, status, defined terms, transitions, named invariants,
failure/restart behavior, examples/counterexamples, conformance and gaps. A narrower owner
can delegate those topics through explicit links. Avoid writing a second copy of wire fields.
New `INV-*` definitions have one owner and may be referenced from tests and models.

Checks:

```bash
python3 scripts/check-specs.py
python3 specs/models/check.py --jar /path/to/tla2tools.jar
```

The first verifies local links/anchors, owner inventory and conformance references; it cannot
prove prose agrees with code. The second checks finite models and negative controls using the
pinned tool described in [models](models/README.md). Both run in CI. Runtime guarantees still
need the linked real implementation tests; missing acceptance coverage stays explicitly missing.
