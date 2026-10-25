"""Compare warm DAG planning on an identical keyed chain with release binaries.

BARCA_BIN=/path/to/new/barca BASELINE_BIN=/path/to/old/barca python bench.py
Requires matching Python helpers and Linux /proc. JSONL goes to stdout.
BASELINE_PYTHONPATH selects older helpers when the baseline uses an older worker protocol.
PROJECTS_DIR optionally retains projects for repeated runs; otherwise they are removed.
COORDINATOR_MAX_KIB / COORDINATOR_MAX_CPU_MS optionally enforce warm-run budgets.
CHANGED_KEYS optionally changes several dispersed keys in each update (default 1).
"""

import importlib.util
import json
import os
import re
import subprocess
import sqlite3
from contextlib import closing
import tempfile
import threading
import time
from pathlib import Path

fixture = importlib.util.spec_from_file_location(
    "batched_fixture", Path(__file__).parents[1] / "batched_partitions/bench.py"
)
module = importlib.util.module_from_spec(fixture)
fixture.loader.exec_module(module)


def measure(binary, project, count, implementation, case, changed=1):
    env = os.environ.copy()
    env["BARCA_TRACE_TIMING"] = "1"
    for name in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(name, None)
    if implementation == "baseline" and "BASELINE_PYTHONPATH" in os.environ:
        env["PYTHONPATH"] = os.environ["BASELINE_PYTHONPATH"]
    start = time.perf_counter()
    peak_kib = 0
    done = threading.Event()
    process = subprocess.Popen(
        [binary, "get", "scored", "pipeline.py", "--json"],
        cwd=project, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )

    def sample():
        nonlocal peak_kib
        status = Path(f"/proc/{process.pid}/status")
        while not done.is_set():
            try:
                line = next(line for line in status.read_text().splitlines() if line.startswith("VmHWM:"))
                peak_kib = max(peak_kib, int(line.split()[1]))
            except (OSError, StopIteration):
                pass
            done.wait(0.001)

    sampler = threading.Thread(target=sample, daemon=True)
    sampler.start()
    try:
        stdout, stderr = process.communicate(timeout=600)
    except BaseException:
        process.kill()
        process.wait()
        raise
    finally:
        done.set()
        sampler.join()
    result = subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
    assert result.returncode == 0, result.stdout + result.stderr
    elapsed = time.perf_counter() - start
    document = json.loads(result.stdout)
    match = re.search(r"incremental identities (\d+)", result.stderr)
    resources = re.search(r"coordinator resources peak_kib=(\d+) cpu_ms=([\d.]+)", result.stderr)
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as db:
        worker = db.execute(
            "SELECT MAX(max_rss_bytes), SUM(cpu_seconds), SUM(elapsed_seconds) "
            "FROM materializations INDEXED BY idx_mat_run WHERE run_id = ?",
            (document["run_id"],),
        ).fetchone()
    row = {
        "keys": count,
        "implementation": implementation,
        "case": case,
        "seconds": round(elapsed, 3),
        "coordinator_peak_kib": max(int(resources[1]), peak_kib) if resources else peak_kib,
        "coordinator_cpu_ms": float(resources[2]) if resources else None,
        "worker_peak_kib": round((worker[0] or 0) / 1024),
        "worker_cpu_ms": round((worker[1] or 0) * 1000, 3),
        "worker_elapsed_ms": round((worker[2] or 0) * 1000, 3),
        "steps": document["steps_executed"],
        "identities": int(match[1]) if match else None,
    }
    print(json.dumps(row), flush=True)
    if case in ("one_key", "batch"):
        assert row["steps"] == 1 + 2 * changed, row
        if implementation == "incremental":
            # Small targets return every artifact reference, so those references
            # must enter coordinator state too. Large targets return a summary.
            assert row["identities"] == (count + 1 + changed if count <= 1000 else 1 + 2 * changed), row
    if case == "unchanged":
        assert row["steps"] == 1, row
        if implementation == "incremental":
            assert row["identities"] == (count + 1 if count <= 1000 else 1), row
    if implementation == "incremental" and case in ("one_key", "batch", "unchanged"):
        for setting, metric in (
            ("COORDINATOR_MAX_KIB", "coordinator_peak_kib"),
            ("COORDINATOR_MAX_CPU_MS", "coordinator_cpu_ms"),
        ):
            if limit := os.environ.get(setting):
                assert row[metric] is not None and row[metric] < float(limit), row
    return row


def benchmark(root):
    binary = str(Path(os.environ["BARCA_BIN"]).resolve())
    baseline = os.environ.get("BASELINE_BIN")
    if baseline:
        baseline = str(Path(baseline).resolve())
    for count in map(int, os.environ.get("KEYS", "1000,50000").split(",")):
        project = root / f"keys-{count}"
        project.mkdir(parents=True, exist_ok=True)
        (project / "barca.toml").write_text("")
        (project / "pipeline.py").write_text(module.pipeline("1000"))
        versions_file = project / "versions.json"
        versions = (
            json.loads(versions_file.read_text())
            if versions_file.exists()
            else {f"r{i}": 1 for i in range(count)}
        )
        assert len(versions) == count
        versions_file.write_text(json.dumps(versions))
        measure(
            baseline or binary, project, count, "baseline" if baseline else "incremental", "seed"
        )
        for label, executable in (("baseline", baseline), ("incremental", binary)):
            if executable is None:
                continue
            if label == "incremental":
                measure(executable, project, count, label, "proof")
            changed = int(os.environ.get("CHANGED_KEYS", "1"))
            assert 1 <= changed <= count
            # Dispersed keys, rather than one contiguous slice of a producer's order.
            keys = [f"r{(i * 7919) % count}" for i in range(changed)]
            assert len(set(keys)) == changed
            for _ in range(int(os.environ.get("REPEATS", "3"))):
                for key in keys:
                    versions[key] += 1
                versions_file.write_text(json.dumps(versions))
                measure(executable, project, count, label,
                        "one_key" if changed == 1 else "batch", changed)
            measure(executable, project, count, label, "unchanged")


if __name__ == "__main__":
    if directory := os.environ.get("PROJECTS_DIR"):
        benchmark(Path(directory).resolve())
    else:
        with tempfile.TemporaryDirectory(prefix="barca-incremental-") as directory:
            benchmark(Path(directory))
