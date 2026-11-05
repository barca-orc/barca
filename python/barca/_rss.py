"""Peak resident memory of one step (#627), reported on its completion message.

A worker runs many steps, so the process's lifetime peak (`ru_maxrss`) is the largest step it
has run so far, not this one. On Linux the peak (`VmHWM`) can be reset at the start of a step;
elsewhere only a step that raised the lifetime peak has a known peak of its own.
"""

from __future__ import annotations

import sys

# What `start_window` returns and `step_peak_bytes` reads: ("hwm", 0) when the Linux high-water
# mark was reset, ("lifetime", peak before the step) otherwise.
RssWindow = tuple[str, int]


def peak_rss_bytes() -> int:
    """Peak RSS of this process over its whole life, in bytes (0 if unavailable).

    `ru_maxrss` is kilobytes on Linux and bytes on macOS.
    """
    try:
        import resource

        peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        if sys.platform == "darwin":
            return int(peak)
        return int(peak) * 1024
    except Exception:
        return 0


def _vm_hwm_bytes() -> int | None:
    """`VmHWM` from /proc/self/status (Linux): the peak RSS since it was last reset."""
    try:
        with open("/proc/self/status", "rb") as status:
            for line in status:
                if line.startswith(b"VmHWM:"):
                    return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        return None
    return None


def start_window() -> RssWindow:
    """Start measuring one step's peak RSS (#627).

    On Linux, writing `5` to /proc/self/clear_refs resets the peak (`VmHWM`) to the current
    RSS, so the peak read when the step ends is this step's own: the most memory the worker
    held while it ran, the worker's baseline (Python, imports, a DuckDB connection) included.
    Elsewhere, the lifetime peak before the step is kept, and the step's peak is known only
    when it raised it. Returns what `step_peak_bytes` needs.
    """
    try:
        with open("/proc/self/clear_refs", "w") as refs:
            refs.write("5")
        if _vm_hwm_bytes() is not None:
            return ("hwm", 0)
    except OSError:
        pass
    return ("lifetime", peak_rss_bytes())


def step_peak_bytes(window: RssWindow) -> int | None:
    """The step's peak RSS in bytes, from `start_window`'s window; None when unknown."""
    kind, before = window
    if kind == "hwm":
        return _vm_hwm_bytes()
    after = peak_rss_bytes()
    # The lifetime peak grew during the step: that is the step's peak. Otherwise the step
    # stayed under an earlier step's peak, and how far under is not known.
    return after if after > before else None
