"""A helper process must not outlive the coordinator that started it.

The coordinator starts its helpers (`barca._transfer`, `barca._state`) deaf to Ctrl-C: what an
interrupt means for a run is the coordinator's decision, and it stops the helpers itself
(crates/barca-core/src/helper_proc.rs). If the coordinator is killed outright, nobody is left
to stop them. So it hands each helper the read end of a pipe as stdin and keeps the write end.
When the coordinator is gone, however it went, the pipe reaches end-of-file: the helper removes
the temp files it was writing and exits, without a word.

Opt-in (`BARCA_LIFELINE=stdin`, set by the coordinator), so a helper run by hand or by a test
with a closed stdin is not affected.
"""

import os
import threading

# Set once the coordinator is known to be gone.
gone = threading.Event()

_PARENT = os.getppid()


def coordinator_gone(wait: float = 0.0) -> bool:
    """Whether the coordinator has exited: the lifeline closed, or this process was handed to
    another parent. `wait` gives the lifeline that long to say so."""
    return gone.wait(wait) or os.getppid() != _PARENT


def leave() -> None:
    """Exit now, quietly, leaving no half-written file behind."""
    from barca import _storage

    _storage.discard_staged()
    os._exit(0)


def watch(on_gone=leave) -> None:
    """Call `on_gone` (from a daemon thread) when the coordinator's end of stdin closes."""
    if os.environ.get("BARCA_LIFELINE") != "stdin":
        return

    def wait() -> None:
        try:
            while os.read(0, 4096):
                pass  # nothing is ever sent; only the end of the pipe matters
        except OSError:
            pass
        gone.set()
        on_gone()

    threading.Thread(target=wait, name="barca-lifeline", daemon=True).start()
