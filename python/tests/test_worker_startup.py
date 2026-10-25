"""Start-up overhead (#560), checked by counting rather than by timing.

Workers used to start one at a time: the coordinator started one, waited 30-40 ms for it to
connect, and only then started the next. A wave of work now starts every worker it can use at
once and takes each connection as it arrives. A warm run (every step cached) used to open the
metadata DB eleven times; it opens it once to start, once per phase, and once to record.
"""

import os
import re
import subprocess
from pathlib import Path

from barca.api import _find_binary


def traced(project: Path, *args: str, **env: str) -> str:
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, "BARCA_TRACE_TIMING": "1", **env},
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stderr


def worker_lines(trace: str) -> list[tuple[str, int]]:
    """("spawned" | "connected", worker id) in the order the trace printed them."""
    return [
        (m.group(2), int(m.group(1)))
        for m in re.finditer(r"worker (\d+) (?:process )?(spawned|connected)", trace)
    ]


def test_a_wave_of_workers_starts_before_any_is_waited_for(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n"
        + "".join(f"\n@asset()\ndef a{i}() -> int:\n    return {i}\n" for i in range(4))
    )
    # A negligible pull cost makes every step a batch of its own: four batches, four workers.
    trace = traced(
        tmp_path,
        "get",
        "pipeline.py",
        "--json",
        BARCA_POOL_SIZE="4",
        BARCA_COMM_COST_SECONDS="1e-9",
    )
    lines = worker_lines(trace)
    spawned = [w for kind, w in lines if kind == "spawned"]
    # Not every worker need connect: the first ones can finish all four steps before the last
    # has started, and the run then stops it.
    assert sorted(spawned) == [0, 1, 2, 3], trace
    # The count of serial waits: a worker started only after another one connected. Started
    # one at a time, it was three.
    first_connect = next(i for i, (kind, _) in enumerate(lines) if kind == "connected")
    serial_waits = sum(1 for kind, _ in lines[first_connect:] if kind == "spawned")
    assert serial_waits == 0, trace
    assert "starting 4 worker(s)" in trace


def test_a_one_step_run_starts_one_worker(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n\n@asset()\ndef only() -> int:\n    return 1\n"
    )
    trace = traced(tmp_path, "get", "pipeline.py", "--json", BARCA_POOL_SIZE="8")
    assert [kind for kind, _ in worker_lines(trace)] == ["spawned", "connected"], trace


def test_a_warm_run_starts_no_worker_and_bounds_metadata_connections(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n\n"
        "@asset()\ndef a() -> int:\n    return 1\n\n"
        "@asset(inputs={'a': a})\ndef b(a: int) -> int:\n    return a + 1\n\n"
        "@asset(inputs={'b': b})\ndef c(b: int) -> int:\n    return b + 1\n"
    )
    traced(tmp_path, "get", "pipeline.py", "--json")
    warm = traced(tmp_path, "get", "pipeline.py", "--json")
    assert worker_lines(warm) == [], warm
    phases = len(re.findall(r"phase\d+_cache_check_done", warm))
    opens = int(re.search(r"metadata db opens (\d+)", warm).group(1))
    # Start + one cache visit per phase + the ledger. Each visit checks the format
    # read-only before opening a writer: SQLite must not checkpoint an unsupported DB.
    assert phases >= 1
    assert opens == 2 * (phases + 2), warm
