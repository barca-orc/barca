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

## Approved creation interval repair (before implementation)

The same lifecycle PR will also cover the demonstrated creation interruption.
During main-thread creation only, a private context temporarily defers SIGTERM,
restores the exact previous handler on every exit, then replays a pending signal
through that handler. Default termination is re-sent after restoring SIG_DFL;
SIG_IGN stays ignored. Non-main-thread creation retains the existing lock policy.

An outer try/finally begins before creation, with no owned path initially. The
creation, exact registration and fd close occur under the existing lock and the
private deferral. Replay occurs afterward within the outer cleanup scope: the
transfer handler discovers the registered path, and the state handler's SystemExit
unwinds through concrete-path cleanup. Exceptions also restore the handler and
remove only the path actually acquired by this call. No other handlers or callers
change. Cross-thread lifeline cleanup continues to wait for the existing lock.

Acceptance adds real transfer/state SIGTERM subprocesses at mkstemp return, plus
normal/error handler restoration and SIG_DFL/SIG_IGN cases. Each signal regression
must fail the old lifecycle and pass the repaired one. Repeat storage/transfer,
state and actual cancellation acceptance, strict checks, independent final review,
and both required fresh CI on the consolidated current-main head.

The actual state helper also raises SystemExit during finalization, so registry
visibility alone cannot complete an interrupted unlink. A forced actual-state
SIGTERM at unlink entry proves this remaining interval. Apply the same private
deferral to just the owned unlink/unregister critical section, replaying afterward;
keep the raw owned remover available for SIG_DFL cleanup. This adds no caller or
lifecycle policy and avoids deferring any user work or network operations.

Review also found a deferred SIG_DFL arriving at handler-restoration entry could
be lost if pending state was checked only beforehand. The real restoration-entry
signal regression proves exit 0 instead of default termination. Recheck pending
state after restoring, perform any still-needed owned cleanup, and re-send the
signal; preserve restoration and termination even if cleanup fails.
