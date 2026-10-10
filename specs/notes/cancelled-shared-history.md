# Cancelled shared-history reconciliation (#303, P03)

## Problem and boundary

A history upload can publish `success` or `failed` before the coordinator hears its
acknowledgement. An interrupt then records `cancelled` locally. If the bounded
wrap-up expires or a second interrupt abandons it, the next pull currently drops
that correction because carry logic compares only unfinished remote runs.

Keep the existing cancellation outcome, helper termination, conditional blob
upload, DB lock/swap and conflict replay. No new user API, database columns,
background service or Engine prerequisite.

## Implementation sequence

1. Reproduce publication-before-acknowledgement with the existing `pushed` helper
   hold point. Interrupt, then hold and abandon the corrective upload before it
   publishes. Assert a remote success and local cancellation before recovery.
2. Extend carry selection through the existing status index: compare local
   `cancelled` runs with remote `success`/`failed` runs of the same ID and recorded
   owner (host, pid and owner identity, including matching legacy empty values).
   Cancellation is monotonic: a stale local success never replaces remote
   cancellation. Keep completed steps and captured logs using existing copy logic.
3. Track an outcome-only correction in `kept_runs` and `Carried::wrote`, so swap
   bookkeeping records the correction even when every step/log already exists.
   Repeat pulls are idempotent. Existing added-row counters retain their meaning.
4. Exercise both an abandoned corrective upload and one that publishes before its
   helper acknowledgement. Add another machine's unrelated run between interruption
   and recovery. Prove the next pull and push preserve all history; also exercise
   conditional-upload conflict replay and stale success after cancellation.
5. Update the remote manual/site description with eventual reconciliation and the
   remaining observation window. Run focused Rust carry/swap tests and actual CLI
   cancellation/shared-history integration tests before opening a bounded PR.

## Guarantees and limits

The first subsequent pull preserves the local cancellation; the next successful
push publishes it. Other machines may see the earlier outcome before that push.
An unavailable store or a machine that never synchronizes cannot guarantee instant
agreement. This deliberately fixes reconciliation rather than inventing a distributed
transaction. Matching run ownership guards against overwriting a different run's
settled outcome. Selection reads cancelled runs through the existing status index;
no full success-history scan or schema migration is required.

Checkpoint cadence (#214) and damaged-history recovery (#243) use this same carry
path and need no hard prerequisite. Schema safety remains independently owned.
