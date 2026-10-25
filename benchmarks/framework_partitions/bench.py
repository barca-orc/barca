"""Fresh local processes, warm data: detect a changed key and update its asset chain.

BARCA_BIN=... DAGSTER_PYTHON=... PREFECT_PYTHON=... python bench.py
KEYS=1000,50000 REPEATS=3; outputs JSONL and retains logs/state under PROJECTS_DIR.
"""

from contextlib import closing
import importlib.util
import json
import os
import re
from pathlib import Path
import signal
import sqlite3
import subprocess
import threading
import time

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "fixture", HERE.parent / "batched_partitions/bench.py"
)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
ROOT = Path(os.environ["PROJECTS_DIR"]).resolve()
CORES = sorted(os.sched_getaffinity(0))[: int(os.environ.get("BENCH_CORES", "4"))]


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


def measure(framework, project, count, case, sample):
    env = {
        k: v for k, v in os.environ.items() if not k.startswith(("BARCA_", "PREFECT_", "DAGSTER_"))
    }
    env.update(
        PYTHONPATH=str(HERE.parents[1] / "python"),
        DO_NOT_TRACK="1",
        DAGSTER_HOME=str(project / "dagster-home"),
        DAGSTER_TELEMETRY_ENABLED="false",
        PREFECT_HOME=str(project / "prefect-home"),
        PREFECT_SERVER_ANALYTICS_ENABLED="false",
        PREFECT_LOGGING_LEVEL="ERROR",
        PREFECT_SERVER_EPHEMERAL_ENABLED="true",
        BARCA_POOL_SIZE=str(len(CORES)),
        BARCA_TRACE_TIMING="1",
    )
    if framework == "barca":
        command = [os.environ["BARCA_BIN"], "get", "scored", "pipeline.py", "--json"]
        if case == "cold":
            command.extend(["--partition", "retailer=r0"])
        env["PATH"] = str(Path(os.environ["BARCA_PYTHON"]).parent) + os.pathsep + env["PATH"]
    else:
        command = [os.environ[f"{framework.upper()}_PYTHON"], str(HERE / "worker.py"), framework]
        if case == "cold_full":
            command.append("full")
    command = ["taskset", "-c", ",".join(map(str, CORES)), *command]
    done = threading.Event()
    peak = 0
    known = set()
    start = time.perf_counter()
    process = subprocess.Popen(
        command,
        cwd=project,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )

    def sample_memory():
        nonlocal peak
        while not done.is_set():
            total = 0
            for pid in descendants(process.pid):
                known.add(pid)
                try:
                    text = Path(f"/proc/{pid}/status").read_text()
                    total += int(
                        next(
                            line for line in text.splitlines() if line.startswith("VmRSS:")
                        ).split()[1]
                    )
                except (OSError, StopIteration):
                    pass
            peak = max(peak, total)
            done.wait(0.005)

    sampler = threading.Thread(target=sample_memory, daemon=True)
    sampler.start()
    try:
        stdout, stderr = process.communicate(timeout=600)
    except BaseException:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        raise
    finally:
        done.set()
        sampler.join()
    elapsed = time.perf_counter() - start
    (project / f"{case}-{sample}.stderr").write_text(stderr)
    (project / f"{case}-{sample}.stdout").write_text(stdout)
    assert process.returncode == 0, stderr[-8000:]
    result = json.loads(stdout)
    expected = {} if case == "unchanged" else {"r0": {"score": sample * 2}}
    if case == "cold_full":
        expected = {f"r{i}": {"score": sample * 2} for i in range(count)}
    if framework == "barca":
        expected_calls = (
            1 if case in ("unchanged", "proof") else (1 + 2 * count if case == "cold_full" else 3)
        )
        assert result["steps_executed"] == expected_calls, result
        with closing(sqlite3.connect(project / ".barca/metadata.db")) as db:
            path = db.execute(
                "SELECT artifact_path FROM materializations WHERE node_id='pipeline.py:scored[retailer=r0]' ORDER BY id DESC LIMIT 1"
            ).fetchone()[0]
        assert json.loads((project / path).read_text()) == {"score": sample * 2}
        if case == "cold_full":
            with closing(sqlite3.connect(project / ".barca/metadata.db")) as db:
                artifacts = db.execute(
                    "SELECT artifact_path FROM materializations WHERE node_id LIKE 'pipeline.py:scored[%'"
                ).fetchall()
            assert len(artifacts) == count, len(artifacts)
            for (path,) in artifacts:
                assert json.loads((project / path).read_text()) == {"score": sample * 2}
        calls = result["steps_executed"]
    else:
        assert result["output"] == expected, result
        calls = len(result["calls"])
        expected_calls = 1 if case == "unchanged" else (1 + 2 * count if case == "cold_full" else 3)
        assert calls == expected_calls, result
    row = dict(
        framework=framework,
        keys=count,
        case=case,
        sample=sample,
        seconds=round(elapsed, 4),
        process_tree_peak_rss_kib=peak,
        calls=calls,
    )
    native = re.search(r"coordinator resources peak_kib=(\d+) cpu_ms=([\d.]+)", stderr)
    if native:
        row.update(native_peak_kib=int(native[1]), native_cpu_ms=float(native[2]))
    print(json.dumps(row), flush=True)
    if native and os.environ.get("NATIVE_MAX_KIB"):
        assert row["native_peak_kib"] < int(os.environ["NATIVE_MAX_KIB"]), row


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    for count in map(int, os.environ.get("KEYS", "1000,50000").split(",")):
        for framework in os.environ.get("FRAMEWORKS", "barca,dagster,prefect").split(","):
            project = ROOT / f"{framework}-{count}"
            project.mkdir(parents=True, exist_ok=True)
            (project / "dagster-home").mkdir(exist_ok=True)
            (project / "dagster-home/dagster.yaml").write_text("telemetry:\n  enabled: false\n")
            (project / "pipeline.py").write_text(fixture.pipeline("1000"))
            (project / "barca.toml").write_text("")
            versions = {f"r{i}": 1 for i in range(count)}
            (project / "versions.json").write_text(json.dumps(versions))
            (project / "previous.json").write_text(json.dumps(versions))
            if framework == "barca":
                # Full seed excluded; Barca validates the complete cached DAG before warm updates.
                env = os.environ.copy()
                env["PATH"] = (
                    str(Path(os.environ["BARCA_PYTHON"]).parent) + os.pathsep + env["PATH"]
                )
                env["PYTHONPATH"] = str(HERE.parents[1] / "python")
                seed = subprocess.run(
                    [os.environ["BARCA_BIN"], "get", "scored", "pipeline.py", "--json"],
                    cwd=project,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=600,
                )
                assert seed.returncode == 0, seed.stderr
                measure(framework, project, count, "proof", 1)
            # One unmeasured update warms each framework's durable history/import caches.
            for sample in range(2, int(os.environ.get("REPEATS", "3")) + 3):
                versions["r0"] = sample
                (project / "versions.json").write_text(json.dumps(versions))
                measure(framework, project, count, "warmup" if sample == 2 else "one_key", sample)
            measure(framework, project, count, "unchanged", sample)


if __name__ == "__main__":
    main()
