# Cancellation-safe staged-file finalization

## Evidence and scope

Backend job v5dk2px0rp left one local artifact `.tmp` after a cancelled run;
exit 130, cancelled history, artifact integrity, and helper shutdown passed.
`staged_beside` currently unregisters its concrete temporary path before unlinking
it. A real SIGTERM at that unlink boundary calls the actual transfer stop handler,
which cannot discover the unregistered partial file and exits with it remaining.

This bounded change keeps the owned path registered through successful removal,
under the existing reentrant lock. It introduces no cleanup scan, public API,
configuration, persistence change, or cleanup of another owner's files.

## Implementation and acceptance

1. Add a subprocess regression using actual `staged_beside`, a nonempty partial
   artifact, and actual `_transfer._stop`. Intercept only that concrete stage's
   `Path.unlink` entry to send SIGTERM. Assert exit 143, no temporary file, and
   unchanged destination and unrelated sentinel. Prove failure before the fix.
2. Unlink before unregistering, inside the existing staging lock. Retain the
   registration if removal fails so shutdown can still retry that owned path.
3. Test removal failure retention and cross-thread cleanup ordering; run storage,
   transfer, artifact, and cancellation regressions with the current checkout.
4. Review the other create/unregister windows and report residual limitations;
   require independent review and fresh required CI before merging.

## Separate demonstrated residual

The creation lock protects cross-thread cleanup, but a same-thread SIGTERM can
reenter while `mkstemp` has created a file and has not returned its name for
registration. An actual-handler subprocess reproduction exits 143 with that
empty stage remaining. Solving that creation interval requires separate lifecycle
coordination; this finalization change must not claim that interval is repaired.
