# Committed local progress before remote checkpoints (#214)

This is the next P12 foundation, independently based on current main. It adds no
public controls, schema version, wire fields or publication cadence. The existing
recorder remains the single owner; successful receipts still belong to PR #359.

## Invariants and implementation

- A batch commits successful step rows and its running-run counter together.
  Any insert or counter-update error aborts the entire transaction; errors are
  returned rather than converted to partial success.
- A run/node already present is a no-op, including duplicate receipts inside one
  batch and retried batches. Preserve existing rows and unrelated history.
- The recorder retains failed batches and combines subsequent rows with them.
  Retry at the existing 500 ms cadence even when no new worker finishes. No busy
  loop, second database owner, new user configuration or blocking worker callback.
- Only a successful commit discards pending rows. This is the future minute
  publisher's commit boundary; this slice introduces no speculative dirty counter.
- Stopping still cancels/awaits the recorder without waiting for another interval;
  existing terminal ledger persistence supplies its complete fallback. Keep short
  connection lifetimes and release DB guards before any future network work.

## Evidence before and after

1. Duplicate batch/retry: exactly one row per run/node and one count increment.
2. Force the second insert to fail using an actual SQLite uniqueness constraint:
   no first-row partial commit, unchanged counter, actionable returned error;
   removing the constraint allows the complete batch to commit once.
3. Force counter update to fail with a constraint on runs: no materialization
   rows commit; removing the constraint allows the same rows to be replayed.
4. Keep a transient DB write failure through recorder ticks, then remove it
   without sending another worker result: pending progress eventually appears.
5. Existing ledger/carry/replacement, incremental CLI, cancellation and confirmed
   remote progress regressions remain valid. No #214 closure until actual minute
   publication, conflict handling and operation-cost evidence are complete.

Resource bound: retained rows are at most the run's completed outcomes plus queued
receipts, already retained by its terminal ledger. Database work remains batched;
measure deduplication against realistic partition scale rather than add a schema
migration solely for this change.

## Verified deduplication bound

An actual initialized Barca/Turso 0.7.0-pre.5 database selects a multi-index
intersection for the unrestricted run/node lookup: 500/2k/5k inserts took
0.605/4.837/23.015 seconds in the debug probe. Restricting that lookup to the
existing `idx_mat_node_run` index searches the node's history instead of every
row of the growing run. The same 2k/5k/20k insert transactions took
1.885/4.892/19.824 seconds. This avoids a new schema migration and quadratic
cold-partition work; the operation remains proportional to the node's prior
history, so no claim of a history-independent constant bound is made.
