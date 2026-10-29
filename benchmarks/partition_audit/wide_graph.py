"""Synthetic coordinator workload: planning skeleton topology, tiny JSON values.

This is not the DuckDB planning fixture. Every node is an asset (including the
skeleton's tasks), repeated in independent copies and truncated to NODES. Each
has KEYS partition identities. Defaults: 1,000 nodes x 1,200 keys = 1.2M keys.
Run with matching helpers, BARCA_BIN and a fresh PROJECTS_DIR on Linux.
"""

import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import sys
import threading
import time


def generate(root, count, keys):
    path = Path(__file__).parents[1] / "planning_pipeline/generate.py"
    spec = importlib.util.spec_from_file_location("planning_shape", path)
    shape = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = shape
    spec.loader.exec_module(shape)
    skeleton = shape.parse_skeleton()
    lines = ["from barca import asset, partitions", f"KEYS = [str(i) for i in range({keys})]", ""]
    names = {}
    edges = 0
    for index in range(count):
        node = skeleton[index % len(skeleton)]
        if index % len(skeleton) == 0:
            names = {}
        name = f"n{index}_{node.name}"
        inputs = {parameter: names[upstream] for parameter, _, upstream in node.params}
        edges += len(inputs)
        binding = ", ".join(f"{parameter!r}: {upstream}" for parameter, upstream in inputs.items())
        lines += [
            f"@asset(inputs={{{binding}}}, partitions={{'key': partitions(KEYS)}}, batch=1000)",
            f"def {name}(key: list[str]{''.join(', ' + p for p in inputs)}):",
        ]
        value = " + ".join(f"{parameter}[k]" for parameter in inputs) or "1"
        lines += [f"    return {{k: ({value}) % 1000003 for k in key}}", ""]
        names[node.name] = name
    root.mkdir(parents=True, exist_ok=False)
    (root / "barca.toml").write_text("")
    (root / "pipeline.py").write_text("\n".join(lines))
    return edges


def measure(binary, root, case, expected):
    env = os.environ.copy()
    for key in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(key, None)
    env["BARCA_TRACE_TIMING"] = "1"
    argv = [binary, "get", "pipeline.py", "--json"]
    if case == "refresh":
        argv += ["--refresh-all"]
    peak = 0
    done = threading.Event()
    start = time.monotonic()
    with (root / f"{case}.out").open("w") as out, (root / f"{case}.err").open("w") as err:
        process = subprocess.Popen(
            argv, cwd=root, env=env, stdout=out, stderr=err, start_new_session=True
        )

        def sample():
            nonlocal peak
            while not done.is_set():
                try:
                    text = Path(f"/proc/{process.pid}/status").read_text()
                    peak = max(peak, int(re.search(r"VmHWM:\s+(\d+)", text)[1]))
                except (OSError, TypeError):
                    pass
                done.wait(0.01)

        sampler = threading.Thread(target=sample)
        sampler.start()
        try:
            code = process.wait(timeout=1800)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGCONT)
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            done.set()
            sampler.join()
    elapsed = time.monotonic() - start
    document = json.loads((root / f"{case}.out").read_text()) if code == 0 else None
    trace = re.search(
        r"coordinator resources peak_kib=(\d+) cpu_ms=([\d.]+)", (root / f"{case}.err").read_text()
    )
    result = {
        "case": case,
        "seconds": round(elapsed, 3),
        "exit_code": code,
        "coordinator_peak_kib": max(peak, int(trace[1]) if trace else 0),
        "coordinator_cpu_ms": float(trace[2]) if trace else None,
        "steps": document["steps_executed"] if document else None,
    }
    if document:
        with sqlite3.connect(root / ".barca/metadata.db") as db:
            result["worker_calls"] = db.execute(
                "SELECT COUNT(DISTINCT batch_id) + COALESCE(SUM(batch_id IS NULL),0) "
                "FROM materializations INDEXED BY idx_mat_run WHERE run_id=?",
                (document["run_id"],),
            ).fetchone()[0]
    print(json.dumps(result), flush=True)
    assert code == 0, (root / f"{case}.err").read_text()[-4000:]
    assert result["steps"] == expected, result


def main():
    root = Path(os.environ["PROJECTS_DIR"]).resolve()
    count, keys = int(os.environ.get("NODES", "1000")), int(os.environ.get("KEYS", "1200"))
    edges = generate(root, count, keys)
    print(
        json.dumps(
            {"nodes": count, "keys_per_node": keys, "partition_keys": count * keys, "edges": edges}
        ),
        flush=True,
    )
    binary = str(Path(os.environ["BARCA_BIN"]).resolve())
    for case in os.environ.get("CASES", "seed,unchanged,refresh").split(","):
        measure(binary, root, case, 0 if case == "unchanged" else count * keys)


if __name__ == "__main__":
    main()
