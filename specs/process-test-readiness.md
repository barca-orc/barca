# Supported-interpreter and worker-shutdown tests (#334)

## Remaining scope

Start from current main `f881a6d` in an isolated worktree. Keep repaired inherited
SIGINT and state-helper timeout cases unchanged. This slice fixes the Python
3.12.0 closed-stdin fixture and replaces startup timing guesses in the worker
shutdown regression. No public API or production change unless an actual defect
is independently demonstrated.

## Bounded mechanics

1. Install and run actual CPython 3.12.0, not a version-number shim; run the
   existing transfer lifeline-mid-download test before modifying it. Retain
   explicit hold readiness, actual pipe EOF, successful helper exit and staged
   download removal.
2. A closed `Popen.stdin` still being referenced makes Python 3.12.0
   `communicate()` try to flush that closed stream. In the test helper, normalize
   an already-closed stdin reference to `None` before communicating. Keep the
   actual prior close; do not fake EOF or suppress the exception generically.
   Always reap a killed child during fixture finalization so assertion/timeout
   failures cannot leave a zombie. Verify process death and cleanup explicitly.
3. For Rust shutdown, replace the fixed 100ms startup sleep with a per-worker
   readiness file written by the same shell after it installs SIGTERM ignore,
   immediately before `exec sleep`. The exec retains the child PID and signal
   disposition, so no grandchild/descriptor leak is introduced. Bound readiness
   separately from measured shutdown and check liveness before signaling.
4. Increase the regression's pool size to 16 to make sequential grace periods
   unambiguously wrong (3.2 seconds). Require one shared ~200ms grace period,
   with a bound at one quarter of the sequential total (800ms) that tolerates
   scheduling on a loaded two-CPU runner. This changes one specific complexity
   assertion and adds event proof, not global timeout increases. Verify every
   child is reaped, the owned socket and branch directory are removed, and no
   readiness/resource leakage remains.
5. Stress the unchanged production shutdown repeatedly under CPU affinity to
   two CPUs, including competing CPU load; capture failures/timing rather than
   widening bounds on failure. Repeat Python coverage on supported 3.13 baseline.
   A real production failure requires a new concrete analysis before code.

## Evidence and checks

Record old/new actual CPython 3.12.0 lifeline result, actual 3.13 baseline result,
constrained shutdown repetitions and meaningful restoration of the sequential
implementation proving the regression still fails. Run transfer process tests,
relevant Rust tests and required formatting/lint/workspace checks. Preserve
clean import and SQL membership branches. No merge/release from this task.

## Recorded evidence

Actual CPython 3.12.0 failed the unchanged lifeline-mid-download test at
`subprocess.communicate()` flushing the closed stdin reference. The corrected
fixture passes all 69 transfer tests on both 3.12.0 and 3.13.16. A separate
communication-timeout regression proves finalization kills and reaps the actual
helper and closes its streams, socket and temporary socket directory; restoring
the prior finalizer makes this regression fail.

The final shutdown regression passed 100 repetitions pinned to two CPUs with
two competing CPU processes. Whole-test wall time was 0.240–0.278 seconds, and
each repetition checked every child was reaped and owned resources removed.
Temporarily restoring sequential per-worker grace periods makes the same test
fail at 3.209 seconds, proving the 800ms bound still detects the original defect.
Readiness validates live children after their ignore handler is installed.

The full Rust workspace passed 800 tests, and strict workspace/all-target Clippy
passed. Cargo formatting, pinned Ruff 0.11.13 checks and formatting, lockfile,
version consistency and diff whitespace checks passed. No production code,
public contract, inherited SIGINT case or state-helper timeout changed.
