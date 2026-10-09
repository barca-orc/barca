# Checkpoint cancellation and diagnostic repair (#214)

Prepared before implementation on minute-publisher head 36446fb. This bounded
follow-up keeps the existing public storage/configuration API and one recorder.

## Implementation

1. Reuse the internal cancellation/deadline context to limit admission to the
   process mutex and cross-process database lock, including snapshot creation,
   post-upload bookkeeping and pull replacement. Cancellation before admission
   changes no database. Once admitted atomic database work begins, finish the
   existing WAL/swap/migration protocol safely; never time out an arbitrary swap.
2. Copy upload snapshots in bounded 64 KiB chunks with cancellation/deadline
   checks and cooperative yields; remove incomplete copies. The publication
   budget covers admission, copy checkpoints and network waits. An admitted
   atomic database operation and an individual local filesystem syscall must
   finish safely and may exceed the deadline; no hard real-time bound is claimed.
3. Retain an acknowledged remote token before waiting for local post-upload
   bookkeeping. Keep the existing public push signature and add only a private
   tracked variant used by SharedPush. Skip redundant database initialization
   after progress pulls, whose admitted replacement already validates/migrates;
   terminal ledger replay remains unchanged.
4. Sanitize state-helper SDK exceptions with the existing storage diagnostic
   sanitizer, covering rewritten signed URLs and configured secrets in both
   error and conflict output.

## Evidence

Use real filesystem locks and actual helper processes to exercise cancellation
and deadlines before snapshot and swap admission, and after an upload has really
acknowledged. Prove the latest token survives stopped bookkeeping and that
cancelled copies/pulls leave no stage or changed local history. A child Python
state helper must emit useful error context without signed-query/authentication
or configured-option secrets. Reproduce failures against old behavior where
practical. Run focused Rust/state Python checks, formatting and strict Clippy.

Running-progress count reconciliation belongs to the separate state-carry fix.
No public test knobs, retention policy, new scheduler or alternate DB owner.

## Completed isolated verification

Implementation commit 472a1b3 retains confirmed upload tokens before admitted
post-upload bookkeeping. Both regular snapshots and recorder batches can stop
while waiting for the process mutex or filesystem lock. Snapshot copies check
between 64 KiB chunks and retain source permissions; their owned stage is
removed on failure/drop. A downloaded pull stops before swap admission, then
finishes the existing atomic swap protocol once admitted. Progress replay skips
redundant initialization; terminal ledger replay remains unchanged.

State-helper error/conflict output uses the existing storage sanitizer. Rust's
configured-URI diagnostic fragments use the existing diagnostic URI formatter.
Tests cover a rewritten signed SDK URL, basic-auth password and configured
storage-option secret, retaining useful operation context.

Verification used this checkout's dedicated target and newly built binary,
with PYTHONPATH pointing to its Python package and an isolated dependency venv:

- All 806 workspace Rust tests pass, including seven new admission, copy,
  recorder-fallback, actual-helper post-ack/pull and URI diagnostic regressions.
- All 83 Python state, actual CLI pull/carry and validation/recovery checks pass
  with no skips; two are the new actual child SDK diagnostic regressions.
- Strict workspace/all-target Clippy, Cargo formatting, Ruff 0.11.13 check and
  formatting, and whitespace checks pass.
- Both Python diagnostic regressions fail against original 36446fb `_state.py`.
  All seven Rust regressions fail after restoring original unbounded admission,
  whole-file copy, recorder admission and raw-URI formatting behavior. Fixed
  sources were restored before the complete successful verification above.

The post-ack regression uses the actual stdlib shared-state backend: a helper
publishes real DB bytes and their SHA token, then a held OS DB lock prevents
bookkeeping. Stopping retains that confirmed token and cleans the upload stage.
The pull regression downloads an actual different history before blocked
admission; stopping leaves the prior local DB byte-for-byte intact and removes
the downloaded stage. The recorder regression stops during held-mutex batch
admission and then verifies complete terminal ledger persistence. No cloud
provider acceptance or hard real-time bound on admitted atomic DB work/local
filesystem syscalls is claimed. Minute timer/count integration belongs to the
parent publisher branch and must be rerun there before merge.
