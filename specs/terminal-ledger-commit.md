# Atomic terminal ledger persistence (#319, #214)

A failed backend recovery check exposed a possible missing durable row despite
successful execution. Reader/WAL reproduction remains separate. Independently,
terminal persistence currently ignores successful/failed materialization insertion
errors and finalizes the run before those rows are written; uniqueness faults
can deterministically prove false completion. This is a correctness/data-loss fix
before extending checkpoint publication, not a new user API.

## Scope and invariants

- One transaction contains run creation, successful and failed step rows, and the
  terminal status/count/timestamp update. Any required query/insert/update/commit
  failure rolls back; the caller receives the existing typed database error.
- Read existing run/node identities through the shared checked helper. A query or
  row-decoding failure cannot become an empty set and duplicate existing history.
  State carry uses the same checked read and already has a typed error boundary.
- Release the process owner only after the terminal transaction commits. Preserve
  already durable mid-run rows, unrelated runs and artifacts on error. No history
  reset, synthetic success, new error envelope, flag or schema migration.
- Existing replay/run-node deduplication and terminal counts remain unchanged.
  Rebuildable cost estimates keep their existing best-effort semantics after the
  required ledger commit; they cannot make a partial outcome look complete.
- Cancellation outcome updates also propagate write failures and retain the owner
  until a confirmed write. No shutdown/deadline policy change.

This does not close #319: full durable provenance and atomic captured-output
integration remain separate work. Captured logs still use the existing subsequent
write; do not claim outcome+logs atomicity from this slice. The minute publisher
still requires #359/#360 and its recorded plan in #214.

## Evidence

Actual SQLite constraints force a successful materialization insert failure and
separately a failed-step insert failure. Old code returns success; corrected code
must return an error, retain the running row/owner, roll back new outcomes, preserve
previously committed progress and unrelated history, and permit complete replay
once the fault is removed. Existing ledger replay/carry/cancellation and actual CLI
history/run-detail tests verify normal behavior. Required workspace/Clippy/CI and
current-main integration precede merge; no release claim until package verification.
