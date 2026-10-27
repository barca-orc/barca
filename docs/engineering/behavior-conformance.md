# Behavioral conformance implementation series

Status: implementation evidence ledger, not a new specification. The behavioral owners in
[the index](../../specs/README.md) remain authoritative. A test name below identifies a
scenario to execute; it is not a claim that all environments or all obligations pass.

## First series

1. Correct sequential historical partition selection. Preserve bounded historical exceptions
   in generation-scoped proofs; overflow drops the shortcut and validates normally.
2. Establish a result-history recording boundary. Progress, terminal fallback, legacy cache
   writes and verified server posts use the recording owner, retaining their distinct
   eligibility, deduplication and transaction rules. Receipt acceptance stays with transfer;
   server object verification stays with ledger admission. History is not current selection.
3. Isolate guarded freshness-proof publication. The existing `CacheReader` holds the
   in-process guard and cross-process lock across preparation and saving, so cooperating Barca
   writers already cannot interleave there. Put the generation/foreign-work checks at the
   transaction boundary too, with exact rejection/rollback/cache-only deletion controls. This
   protects the boundary during future ownership changes; it is not a newly demonstrated
   supported-writer race or general ordered observation publication.

Each PR changes one boundary. Review and merge the series bottom-up; a later layer's tests
include its dependencies. No public wire change is intended by these three layers.

## Obligation evidence

| Obligation | Current scope/status | Executable scenario |
| --- | --- | --- |
| [INV-SELECT-01](../../specs/behavior/freshness.md#invariants) | Sequential historical reversion implemented in the first layer; concurrent ordering remains pending | [partition audit](../../python/tests/test_partition_audit.py): `test_return_to_old_source_value_stays_correct_on_following_cache_hits` covers sensor/asset producers and partitioned/unpartitioned controls; `test_historical_selections_over_budget_fall_back_without_stale_values` covers overflow and cache-only generations |
| [INV-MEMBER-01](../../specs/behavior/freshness.md#invariants) | Sequential domain transitions covered | [partition audit](../../python/tests/test_partition_audit.py): `test_mutating_partition_domain_matches_oracle` checks removed/restored/empty domains and downstream results for three batching modes |
| [INV-CHAIN-01](../../specs/behavior/freshness.md#invariants) | Existing warm affected-chain behavior must survive | [incremental DAGs](../../python/tests/test_incremental_dag.py): `test_one_key_update_does_not_register_fresh_partition_chains` |
| [INV-COMMIT-01](../../specs/behavior/current-state-and-execution.md#invariants) | Partial evidence only: history/count/terminal rollback. Selection, membership and summaries as one ordered commit remain pending | [persistence](../../crates/barca-core/src/persist.rs): `progress_batch_replay_keeps_one_row_and_one_count_per_node`, `a_failed_counter_update_rolls_back_progress_rows`, `a_failed_failure_row_rolls_back_new_success_rows_and_terminal_status`; [terminal persistence](../../python/tests/test_terminal_persist.py): `test_terminal_insert_fault_is_an_infra_error_and_preserves_prior_history` |
| Confirmed upload before receipt-driven checkpoint | Existing receipt contract retained; fail-soft terminal behavior remains distinct | [upload receipts](../../python/tests/test_upload_snapshot_receipts.py): `test_confirmed_receipt_is_checkpointed_while_later_step_is_running`; [persistence](../../crates/barca-core/src/persist.rs): `upload_receipts_preserve_worker_hashes_and_fill_ordinary_outputs` |
| Sensor evidence through server-ledger recording | Existing digest/observation distinction retained | [ledger](../../crates/barca-core/src/ledger.rs): `equal_artifact_bytes_can_carry_a_new_sensor_revision` |

Proof-publication boundary controls in [proof publication](../../crates/barca-core/src/incremental/publication.rs):
`a_materialization_after_preparation_rejects_the_proof_without_deleting_prior_evidence`,
`foreign_work_is_rejected_even_if_it_precedes_the_generation_read`,
`own_progress_and_cache_only_discard_publish_at_the_same_generation`, and
`a_failed_replacement_rolls_back_proof_deletions`. The separate SQL connection deliberately
bypasses the caller's advisory lock to exercise the boundary, not a supported Barca execution
path. Existing locks stay in place. Cache-only writes still do not supply an ordered revision.

## Remaining behavior changes and gates

The [audit](spec-audit.md) retains immutable counterexamples. Resolved documentation defects
are not runtime fixes. Subsequent PRs must extend this ledger with exact scenarios and results.

| Next change | Required evidence / unresolved gate |
| --- | --- |
| Ordered selection and membership (V10) | Define reconciliation scope and multi-source validity; test newer-first completion, reusing old bytes, late removed keys and restart between byte finalization and metadata commit |
| Captured code/configuration and stable input bytes (V3/V10) | Define project/dependency snapshot boundary; test delayed helper import, replacement worker, overwritten input and GC during active reads |
| Owned process-tree cleanup (V4) | Test success/failure/cancellation/worker death with descendants and an unrelated-process control; preserve terminal interrupt behavior and frozen parallel callers |
| Drain and invocation recovery (V6/V7) | Define invocation-ID lifetime and physical retry authorization; reject admission during drain, separate asset recovery from explicit task retry and cover lost acknowledgements |
| Bounded execution and inspection (V5/V10) | Backpressure through discovery/persistence; bounded event/log buffers and pages; verify cold/full-refresh, slow consumers, changing membership and obsolete checks |
| Shared server-owned computation (V10) | Only after ordered publication and stable inputs; test overlapping identities, independent cancellation, owner failure, retry races and restart |
| Project resource enforcement (V5/U2) | Decide equivalent-root identity and conflicting override arbitration before cross-process accounting |
| Proactive freshness (V8) | Decide direct-check wait/stale alternatives, then test source outage, backoff and recovery |
| Retention (V9) | Decide recurring-version identity/shared-byte accounting/remote deletion scope, then test current and active protection |
| Multiple sinks / preview imports (U1/U3) | Specify partial-publication recovery and resolver/import side-effect boundaries before promising new guarantees |

This is deliberately an initial inventory, not coverage of all 47 named invariants. Public
behavior changes must update the existing manual, interface owners and CLI snapshots alongside
the implementation. The unanswered proposals remain unapproved.
