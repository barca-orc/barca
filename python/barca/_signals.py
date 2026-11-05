"""What a worker does with the signals it gets: Ctrl-C (SIGINT) and the coordinator's SIGTERM.

The terminal sends Ctrl-C to every process of the job, and the coordinator, which gets it too,
decides what it means for the run and stops the workers. A worker acts on it only while a
step's function runs:

- While the interpreter starts and barca is imported: it does not get the signal at all.
  The coordinator starts a worker in a process group of its own
  (`helper_proc::start_outside_the_job`), since nothing written here can keep an interrupt
  that arrives that early from ending the process with a traceback. `join_the_job` moves
  the worker into the coordinator's group once the handler below is installed.
- While a step's module is imported, between steps, and while a result or an error is
  reported: nothing happens (`ctrl_c_does_nothing`). An interrupt there used to end the
  worker with a `KeyboardInterrupt` traceback of barca's own frames or of the module's
  import, which tells the user nothing.
- While a step runs, from loading its inputs to writing its result: `KeyboardInterrupt` is
  raised in the step (`ctrl_c_interrupts_the_step`), as before. The worker reports the step
  as interrupted and the coordinator leaves it out of the cancelled run. The coordinator's
  SIGTERM follows the interrupt and ends the worker at once (`exit_on_sigterm`), so a step
  cannot count on its `except` or `finally` code running.

One exception to all of it: a barca that was itself started with SIGINT ignored (`nohup`, a
background job of a non-interactive shell, cron, some supervisors). Its workers inherit that,
and keep it: SIGINT stays ignored at every stage, a step included, which is what Python does
for any program started that way and what a step saw before these stages existed. The
coordinator still acts on a SIGINT sent to it and stops the workers.
"""

from __future__ import annotations

import os
import sys
from types import FrameType

# Whether this process was started with SIGINT ignored. Read before anything changes it.
_ctrl_c_inherited_ignored: bool | None = None


def _started_with_ctrl_c_ignored() -> bool:
    global _ctrl_c_inherited_ignored
    if _ctrl_c_inherited_ignored is None:
        import signal

        _ctrl_c_inherited_ignored = signal.getsignal(signal.SIGINT) is signal.SIG_IGN
    return _ctrl_c_inherited_ignored


def _no_interrupt(_signum: int, _frame: FrameType | None) -> None:
    pass


def ctrl_c_does_nothing() -> None:
    """Ctrl-C has no effect on this process until a step runs.

    A handler that does nothing, not `SIG_IGN`: an ignored signal is inherited as ignored by
    every process the user's code starts, a handled one is not.
    """
    import signal

    if _started_with_ctrl_c_ignored():
        return
    try:
        signal.signal(signal.SIGINT, _no_interrupt)
    except ValueError:
        pass  # not the main thread: nothing to change


def ctrl_c_interrupts_the_step() -> None:
    """Ctrl-C raises `KeyboardInterrupt` in the main thread, as in any Python program."""
    import signal

    if _started_with_ctrl_c_ignored():
        return
    try:
        signal.signal(signal.SIGINT, signal.default_int_handler)
    except ValueError:
        pass  # not the main thread: nothing to change


def join_the_job() -> None:
    """Move this worker into the coordinator's process group, where the terminal's Ctrl-C is
    delivered. Called once `ctrl_c_does_nothing` is in place; until then the worker is in a
    group of its own, out of the terminal's reach."""
    job = os.environ.get("BARCA_JOB_PGID")
    if not job:
        return  # started by hand, or by a coordinator that leaves workers in its group
    try:
        os.setpgid(0, int(job))
    except (OSError, ValueError):
        pass  # the coordinator's group is gone; so is the coordinator, and the socket says so


def exit_on_sigterm() -> None:
    """Install the SIGTERM handler: flush buffered progress output, then leave at once.

    The coordinator's `graceful_kill` sends SIGTERM. Exit via `os._exit`, not `sys.exit(0)`: a
    `SystemExit` raised from the handler while the interpreter is already shutting down is
    uncatchable, and Python prints it as a noisy "Exception ignored in: _on_sigterm ...
    SystemExit: 0" on stderr, which then leaks into surfaced worker errors (reliably on Linux).
    `os._exit` cannot raise, so that noise is impossible.
    """
    import signal

    def _on_sigterm(_signum: int, _frame: FrameType | None) -> None:
        try:
            sys.stdout.flush()
            sys.stderr.flush()
        except Exception:
            pass
        os._exit(0)

    signal.signal(signal.SIGTERM, _on_sigterm)
