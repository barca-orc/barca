"""Peak memory of a command and every process it starts (Linux).

`run(argv, ...)` returns the exit code, the peak of the summed resident size of the process
tree, and the peak resident size of its largest single process. Both peaks come from the same
snapshots of /proc, taken every 50 ms: the tree peak is the largest per-snapshot sum and the
process peak the largest single entry of those snapshots, so the largest process never reads
above the tree (#595). A spike shorter than the interval can fall between two snapshots. Used
by `bench.py` and by the remote tests of the planning pipeline
(`python/tests/planning_pipeline_store.py`).
"""

from __future__ import annotations

import os
import subprocess
import threading
import time


def tree_snapshot(root: int) -> list[int]:
    """The resident sizes of `root` and its descendants at one moment, in bytes.

    A process that exits between the listing of /proc and the reading of its files is left
    out, as are its descendants, which the kernel reparents away from the tree."""
    parent, rss = {}, {}
    page = os.sysconf("SC_PAGE_SIZE")
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            with open(f"/proc/{entry}/statm") as f:
                resident = int(f.read().split()[1]) * page
        except (OSError, ValueError, IndexError):
            continue
        parent[int(entry)] = ppid
        rss[int(entry)] = resident
    children: dict[int, list[int]] = {}
    for pid, ppid in parent.items():
        children.setdefault(ppid, []).append(pid)
    sizes, frontier, seen = [], [root], set()
    while frontier:
        pid = frontier.pop()
        if pid in seen:
            continue
        seen.add(pid)
        if pid in rss:
            sizes.append(rss[pid])
        frontier.extend(children.get(pid, []))
    return sizes


def run(argv: list[str], *, cwd, env, stdout, stderr, interval: float = 0.05):
    """Run `argv`; returns (exit code, tree peak bytes, largest process peak bytes)."""
    proc = subprocess.Popen(argv, cwd=cwd, env=env, stdout=stdout, stderr=stderr)
    tree_peak, process_peak = 0, 0
    done = threading.Event()

    def sample():
        nonlocal tree_peak, process_peak
        while not done.is_set():
            sizes = tree_snapshot(proc.pid)
            if sizes:
                tree_peak = max(tree_peak, sum(sizes))
                process_peak = max(process_peak, max(sizes))
            time.sleep(interval)

    sampler = threading.Thread(target=sample, daemon=True)
    sampler.start()
    _, status, _ = os.wait4(proc.pid, 0)
    proc.returncode = os.waitstatus_to_exitcode(status)
    done.set()
    sampler.join()
    return proc.returncode, tree_peak, process_peak
