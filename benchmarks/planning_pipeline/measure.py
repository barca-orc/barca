"""Peak memory of a command and every process it starts (Linux).

`run(argv, ...)` returns the exit code, the peak of the summed resident size of the process
tree (sampled from /proc every 50 ms), and the peak resident size of its largest single process
(from `wait4`, which covers every descendant the command reaped). Used by `bench.py` and by the
remote tests of the planning pipeline (`python/tests/planning_pipeline_store.py`).
"""

from __future__ import annotations

import os
import subprocess
import threading
import time


def tree_rss(root: int) -> int:
    """The summed resident size of `root` and its descendants, in bytes."""
    parent, rss = {}, {}
    page = os.sysconf("SC_PAGE_SIZE")
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            with open(f"/proc/{entry}/statm") as f:
                rss[int(entry)] = int(f.read().split()[1]) * page
        except (OSError, ValueError, IndexError):
            continue
        parent[int(entry)] = ppid
    children: dict[int, list[int]] = {}
    for pid, ppid in parent.items():
        children.setdefault(ppid, []).append(pid)
    total, frontier = 0, [root]
    while frontier:
        pid = frontier.pop()
        total += rss.get(pid, 0)
        frontier.extend(children.get(pid, []))
    return total


def run(argv: list[str], *, cwd, env, stdout, stderr, interval: float = 0.05):
    """Run `argv`; returns (exit code, tree peak bytes, largest process peak bytes)."""
    proc = subprocess.Popen(argv, cwd=cwd, env=env, stdout=stdout, stderr=stderr)
    peak = [0]
    done = threading.Event()

    def sample():
        while not done.is_set():
            peak[0] = max(peak[0], tree_rss(proc.pid))
            time.sleep(interval)

    sampler = threading.Thread(target=sample, daemon=True)
    sampler.start()
    _, status, usage = os.wait4(proc.pid, 0)
    proc.returncode = os.waitstatus_to_exitcode(status)
    done.set()
    sampler.join()
    return proc.returncode, peak[0], usage.ru_maxrss * 1024
