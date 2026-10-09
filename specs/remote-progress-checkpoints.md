# Confirmed remote progress and one-minute checkpoints (#214, P12)

The accepted cadence is sixty seconds in healthy operation. Only completed
steps whose configured artifact uploads are confirmed may become reusable rows.
The DB remains authoritative history; no retention, deletion, snapshot archive,
new storage mode or new user configuration is introduced.

## Sequential implementation

1. Record confirmed remote artifacts during execution. The existing transfer
   socket owner invokes an optional private success notification for each upload
   reply. The notification queues the already prepared StepRow in the existing
   StepRecorder, replacing its local path with the configured store location and
   carrying the hash of the uploaded bytes. Transfer code never opens the DB and
   persistence does not infer success from an enqueued request, a local file, a
   timeout or an upload's position in the queue. A stalled earlier upload must
   not hide a later confirmed result. Preserve the final transfer report and
   cancellation drain so final outcome accounting still uses the same receipts.
2. Publish committed progress every sixty seconds through the existing recorder
   and shared-state push/conflict path. Give the recorder optional publication
   context and ownership of the state token until finalization, rather than add
   a second scheduler/ledger/DB owner. Its timer starts after startup succeeds;
   missed ticks coalesce, pushes never overlap, and unchanged progress does not
   upload metadata again. The dirty generation advances after successful DB
   commits, not after receipt delivery. Confirmed rows not yet committed must
   never be described as published. A long single phase must checkpoint too.
3. Stop/await the recorder before final persistence and return its latest known
   token for the existing terminal publication. Finalization retains the final
   flush, optimistic replay and two-signal cancellation semantics. An upload
   acknowledgement lost after publication is handled as an unknown token and
   recovered through the same conflict/carry path, not assumed absent.

These are separate bounded PRs. The first closes no whole issue: cross-machine
one-minute publication, conflict tests and traffic measurement remain in #214.

## Persistence and conflict discipline

Before the publication slice, replace the existing best-effort batch behavior
that discards failed inserts/counter updates. Require one validated transaction
for rows and counts, run/node deduplication, and retained retry batches. Only a
successful commit advances the publication generation; snapshot captures that
exact generation before network awaits. The first receipt-only slice retains
the current best-effort recorder and final-ledger fallback and does not claim
these retry or minute-publication guarantees.

Rows are identified by run ID and node ID, matching current ledger/carry logic.
Retrying a batch or replay must not add duplicates or increment executed count
again. Existing history is preserved. Recorder connection lifetimes remain short
and end before state snapshots checkpoint WAL. Completed uploads retain their
actual byte hashes and store paths; incomplete/failed uploads have no success
row. Final output/failure/attempt/sink/timing data continue to use the ledger.

Refactor the existing SharedPush conflict loop to accept recorded-progress replay
without finalizing a running run. A checkpoint keeps status running and leaves
finished_at unset. A pull carries already durable local rows and unrelated runs;
there is no replacement independent progress database. On each acknowledged
upload retain the returned token immediately, including if a later pull/replay
fails; don't manufacture a conflict with the token
from startup at every subsequent checkpoint. A lost acknowledgement retains
the last known token for conflict recovery: StateToken(None) means a confirmed
absent remote object and must never mean an unknown upload outcome.

A checkpoint gets the existing ten-second bounded publication budget and the
run/recorder cancellation token. A failed checkpoint is an actionable sanitized
note, leaves local rows and dirty progress intact, and retries at the next
coalesced minute. It does not fail successful user work or spin between ticks.
The terminal push keeps its existing outcome/error policy. The cadence is not a
one-minute loss guarantee while artifact uploads, DB writes or state storage fail.
No checkpoint holds a DB lock through network I/O.

## Required regression evidence

- Observe a real remote upload acknowledgement while a later step is blocked:
  local inspection sees the saved remote result before run end; SIGKILL then
  local restart reuses it. No row points to an unconfirmed upload.
- Hold the first upload and allow a second to finish: only the second is saved.
  Disconnect, failed uploads and cancellation preserve final partial outcomes.
- Retry/replay and final ledger persistence keep one outcome per run/node with
  matching hashes, format, sinks, attempts and timings; no double counters.
- Kill between real sixty-second checkpoints and resume on another machine using
  the saved confirmed results. Assert actual stored objects exist and unfinished
  uploads are absent from reusable metadata.
- Conflict with an unrelated concurrent run, including continued local writes
  during snapshot/upload and acknowledgement interruption: no history is lost.
- Slow/outage storage, first/second cancellation, many missed ticks and unchanged
  progress demonstrate bounded shutdown, one in-flight push and no busy retries.
- Measure real checkpoint bytes and upload count for a representative long run.
  Deterministic private timer tests may shorten the interval; no public test knob.

Update cache/remote manuals, site and contracts only for behavior exercised by
this slice. Retention/GC/recovery policies remain #243/#83, and immutable saved
partition-result references remain the separate #287/#57 design.
