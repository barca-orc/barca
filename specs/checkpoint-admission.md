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
