"""Measure run resources and process count; assert actual logical work.

resources.py PROJECT -- [barca arguments]
Prints one JSON record. PROJECT is retained; subprocesses are reaped on failure.
"""

import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path


def descendants(root):
    pending, seen = [root], set()
    while pending:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        try:
            for task in Path(f"/proc/{pid}/task").iterdir():
                pending.extend(map(int, (task / "children").read_text().split()))
        except (OSError, ValueError):
            pass
    return seen


def measure(project, args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env.update(BARCA_TRACE_TIMING="1", BARCA_POOL_SIZE="4")
    started = time.perf_counter()
    process = subprocess.Popen(
        [os.environ["BARCA_BIN"], *args],
        cwd=project,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    done = threading.Event()
    peak_rss = peak_processes = 0

    def sample():
        nonlocal peak_rss, peak_processes
        while not done.is_set():
            rss, count = 0, 0
            for pid in descendants(process.pid):
                try:
                    status = Path(f"/proc/{pid}/status").read_text()
                    rss += int(re.search(r"VmRSS:\s+(\d+)", status)[1])
                    count += 1
                except (OSError, TypeError):
                    pass
            peak_rss = max(peak_rss, rss)
            peak_processes = max(peak_processes, count)
            done.wait(0.002)

    sampler = threading.Thread(target=sample)
    sampler.start()
    try:
        out, err = process.communicate(timeout=600)
    except BaseException:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        raise
    finally:
        done.set()
        sampler.join()
    assert process.returncode == 0, out + err
    result = json.loads(out)
    resources = re.search(r"coordinator resources peak_kib=(\d+) cpu_ms=([\d.]+)", err)
    record = {
        "args": args,
        "seconds": round(time.perf_counter() - started, 4),
        "process_tree_peak_rss_kib": peak_rss,
        "peak_processes": peak_processes,
        "native_peak_kib": int(resources[1]) if resources else None,
        "native_cpu_ms": float(resources[2]) if resources else None,
        "steps": result.get("steps_executed"),
    }
    print(json.dumps(record), flush=True)
    assert peak_processes <= 5, record  # coordinator + configured pool; no remote store
    return record


if __name__ == "__main__":
    measure(Path(sys.argv[1]).resolve(), sys.argv[3:])
