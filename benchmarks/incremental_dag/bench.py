"""Compare warm DAG planning on an identical keyed chain with release binaries.

BARCA_BIN=/path/to/new/barca BASELINE_BIN=/path/to/old/barca python bench.py
Requires matching Python helpers on PATH and Linux /usr/bin/time. JSONL goes to stdout.
PROJECTS_DIR optionally retains projects for repeated runs; otherwise they are removed.
"""

import importlib.util
import json
import os
import re
import subprocess
import tempfile
import time
from pathlib import Path

fixture = importlib.util.spec_from_file_location(
    "batched_fixture", Path(__file__).parents[1] / "batched_partitions/bench.py"
)
module = importlib.util.module_from_spec(fixture)
fixture.loader.exec_module(module)


def measure(binary, project, count, implementation, case):
    env = os.environ.copy()
    env["BARCA_TRACE_TIMING"] = "1"
    for name in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(name, None)
    start = time.perf_counter()
    result = subprocess.run(
        [
            "/usr/bin/time",
            "-f",
            "%M",
            "-o",
            str(project / "rss.txt"),
            binary,
            "get",
            "scored",
            "pipeline.py",
            "--json",
        ],
        cwd=project,
        check=False,
        env=env,
        capture_output=True,
        text=True,
        timeout=600,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    document = json.loads(result.stdout)
    match = re.search(r"incremental identities (\d+)", result.stderr)
    row = {
        "keys": count,
        "implementation": implementation,
        "case": case,
        "seconds": round(time.perf_counter() - start, 3),
        "coordinator_peak_kib": int((project / "rss.txt").read_text()),
        "steps": document["steps_executed"],
        "identities": int(match[1]) if match else None,
    }
    print(json.dumps(row), flush=True)
    if case == "one_key":
        assert row["steps"] == 3, row
        if implementation == "incremental":
            assert row["identities"] == 3, row
    if case == "unchanged":
        assert row["steps"] == 1, row
        if implementation == "incremental":
            assert row["identities"] == 1, row
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
            for _ in range(int(os.environ.get("REPEATS", "3"))):
                versions["r0"] += 1
                versions_file.write_text(json.dumps(versions))
                measure(executable, project, count, label, "one_key")
            measure(executable, project, count, label, "unchanged")


if __name__ == "__main__":
    if directory := os.environ.get("PROJECTS_DIR"):
        benchmark(Path(directory).resolve())
    else:
        with tempfile.TemporaryDirectory(prefix="barca-incremental-") as directory:
            benchmark(Path(directory))
