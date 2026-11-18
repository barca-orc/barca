#!/usr/bin/env python3
"""Performance budgets that fail CI's merge tier (specs/behavior/performance.md, "Gates").

    BARCA_BIN=.venv/bin/barca python3 scripts/perf-gate.py     # default: barca on PATH

Three checks, each a few seconds:

trivial   benchmarks/trivial/barca/assets.py (one asset, no work), `barca get --refresh-all`,
          TRIVIAL_RUNS runs after three warm-ups. The median wall time must be under
          TRIVIAL_MAX_MS, the median coordinator CPU under TRIVIAL_MAX_CPU_MS and every run's
          coordinator peak memory under COORDINATOR_MAX_KIB.
incremental
          benchmarks/incremental_dag/bench.py at INCREMENTAL_KEYS keys: warm one-key updates
          and an unchanged run, with its own work-count assertions and the native budgets
          COORDINATOR_MAX_KIB and INCREMENTAL_MAX_CPU_MS.
history   a long-lived database: HISTORY_KEYS partition keys of one asset, each key's history
          copied to HISTORY_MULT times its rows with sqlite3 (every seventh copy a failure), then
          HISTORY_RUNS runs that each change the asset's code, so every key misses and is
          explained. The median of the runs' plan-reasons lookup time (BARCA_TRACE_TIMING's
          `plan reasons so far`) must be under HISTORY_MAX_MS: it reads each key's newest rows,
          not its history (#1009).

A check that fails is measured once more from scratch and fails the gate only if it fails
again: a regression fails both times, a burst of load on a shared runner rarely does.
Exit 0 when every check passes, 1 otherwise.
"""

import json
import os
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Defaults (specs/behavior/performance.md, "Gates", has the measurements behind them):
# - memory: resources.md's <20 MB for really simple coordination, as performance.md's
#   conservative 19,531 KiB;
# - trivial CPU: resources.md's <50 ms target for Barca's own overhead on ordinary work;
# - trivial wall: about 3.7x the `fast` profile wheel's 39-41 ms median on the reference
#   machine, for slower and shared CI runners; it catches a regression of ~100 ms, not ~10;
# - incremental CPU: twice performance.md's 20 ms reference-machine budget, which the `fast`
#   wheel already reaches 14-18 ms of there; a CI runner's slower cores would flake at 20.
TRIVIAL_RUNS = int(os.environ.get("TRIVIAL_RUNS", "15"))
TRIVIAL_MAX_MS = float(os.environ.get("TRIVIAL_MAX_MS", "150"))
TRIVIAL_MAX_CPU_MS = float(os.environ.get("TRIVIAL_MAX_CPU_MS", "50"))
COORDINATOR_MAX_KIB = int(os.environ.get("COORDINATOR_MAX_KIB", "19531"))
INCREMENTAL_KEYS = os.environ.get("INCREMENTAL_KEYS", "5000")
INCREMENTAL_MAX_CPU_MS = os.environ.get("INCREMENTAL_MAX_CPU_MS", "40")
# - history: the reference machine's `fast` wheel takes 2-3 ms at 300 keys and 200x history,
#   and took 76-147 ms before #1009 read only the newest rows; 25 ms leaves a slow runner room
#   and still fails a lookup that reads the history again.
HISTORY_KEYS = int(os.environ.get("HISTORY_KEYS", "300"))
HISTORY_MULT = int(os.environ.get("HISTORY_MULT", "200"))
HISTORY_RUNS = int(os.environ.get("HISTORY_RUNS", "3"))
HISTORY_MAX_MS = float(os.environ.get("HISTORY_MAX_MS", "25"))


def barca_bin() -> str:
    found = os.environ.get("BARCA_BIN") or shutil.which("barca")
    if not found:
        sys.exit("perf-gate: no barca binary (set BARCA_BIN or put barca on PATH)")
    return str(Path(found).resolve())


def clean_env() -> dict:
    env = os.environ.copy()
    for name in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(name, None)
    env["BARCA_TRACE_TIMING"] = "1"
    return env


def resources(stderr: str) -> tuple[int, float]:
    for line in stderr.splitlines():
        if "coordinator resources" in line:
            fields = dict(part.split("=") for part in line.split()[3:])
            return int(fields["peak_kib"]), float(fields["cpu_ms"])
    raise AssertionError(f"no coordinator resources line in:\n{stderr[-2000:]}")


def trivial(binary: str) -> tuple[bool, dict]:
    with tempfile.TemporaryDirectory(prefix="barca-perf-trivial-") as directory:
        project = Path(directory)
        shutil.copy(ROOT / "benchmarks/trivial/barca/assets.py", project / "assets.py")
        (project / "barca.toml").write_text("")
        env = clean_env()
        walls, cpus, peaks = [], [], []
        for i in range(3 + TRIVIAL_RUNS):
            start = time.perf_counter()
            done = subprocess.run(
                [binary, "get", "assets.py", "--refresh-all", "--json"],
                cwd=project,
                env=env,
                capture_output=True,
                text=True,
            )
            wall = (time.perf_counter() - start) * 1000
            assert done.returncode == 0, done.stdout + done.stderr
            assert json.loads(done.stdout)["steps_executed"] == 1, done.stdout
            if i < 3:
                continue
            peak, cpu = resources(done.stderr)
            walls.append(wall)
            cpus.append(cpu)
            peaks.append(peak)
    row = {
        "check": "trivial",
        "runs": TRIVIAL_RUNS,
        "wall_ms_median": round(statistics.median(walls), 1),
        "wall_ms_range": [round(min(walls), 1), round(max(walls), 1)],
        "coordinator_cpu_ms_median": round(statistics.median(cpus), 1),
        "coordinator_peak_kib_max": max(peaks),
        "limits": {
            "wall_ms_median": TRIVIAL_MAX_MS,
            "coordinator_cpu_ms_median": TRIVIAL_MAX_CPU_MS,
            "coordinator_peak_kib_max": COORDINATOR_MAX_KIB,
        },
    }
    ok = (
        row["wall_ms_median"] < TRIVIAL_MAX_MS
        and row["coordinator_cpu_ms_median"] < TRIVIAL_MAX_CPU_MS
        and row["coordinator_peak_kib_max"] < COORDINATOR_MAX_KIB
    )
    return ok, row


def incremental(binary: str) -> tuple[bool, dict]:
    env = clean_env()
    env.update(
        BARCA_BIN=binary,
        KEYS=INCREMENTAL_KEYS,
        REPEATS="3",
        COORDINATOR_MAX_KIB=str(COORDINATOR_MAX_KIB),
        COORDINATOR_MAX_CPU_MS=INCREMENTAL_MAX_CPU_MS,
    )
    env.pop("BASELINE_BIN", None)
    env.pop("PROJECTS_DIR", None)
    # The workers run the Python next to the binary, as installed; the harness only needs
    # the standard library.
    start = time.perf_counter()
    done = subprocess.run(
        [sys.executable, str(ROOT / "benchmarks/incremental_dag/bench.py")],
        env=env,
        capture_output=True,
        text=True,
    )
    rows = [json.loads(line) for line in done.stdout.splitlines() if line.startswith("{")]
    warm = [r for r in rows if r["case"] in ("one_key", "unchanged")]
    row = {
        "check": "incremental",
        "keys": INCREMENTAL_KEYS,
        "seconds": round(time.perf_counter() - start, 1),
        "warm_runs": len(warm),
        "coordinator_cpu_ms_max": max((r["coordinator_cpu_ms"] or 0 for r in warm), default=None),
        "coordinator_peak_kib_max": max((r["coordinator_peak_kib"] for r in warm), default=None),
        "wall_s_max": max((r["seconds"] for r in warm), default=None),
        "limits": {
            "coordinator_cpu_ms": float(INCREMENTAL_MAX_CPU_MS),
            "coordinator_peak_kib": COORDINATOR_MAX_KIB,
        },
    }
    if done.returncode != 0:
        row["error"] = done.stderr.strip().splitlines()[-1:] or ["exit " + str(done.returncode)]
    return done.returncode == 0, row


HISTORY_SOURCE = """\
from barca import asset, partitions



@asset(partitions={{"key": partitions({keys})}})
def a(key: str) -> dict:
    return {{"key": key, "version": {version}}}
"""


def seed_history(db: Path, mult: int) -> int:
    """Copy every row of `db`'s materializations `mult - 1` times under new run hashes, newest
    last, every seventh copy a failure: a server that has run for weeks. Returns the rows."""
    conn = sqlite3.connect(db)
    try:
        top = conn.execute("SELECT max(id) FROM materializations").fetchone()[0]
        columns = [r[1] for r in conn.execute("PRAGMA table_info(materializations)")]
        copied = {
            "run_hash": "run_hash || '-' || f.n",
            "status": "CASE WHEN f.n % 7 = 3 THEN 'failed' ELSE status END",
            "artifact_path": "CASE WHEN f.n % 7 = 3 THEN NULL ELSE artifact_path END",
        }
        columns = [c for c in columns if c != "id"]
        conn.execute(
            f"WITH RECURSIVE f(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM f WHERE n < {mult - 1}) "
            f"INSERT INTO materializations ({', '.join(columns)}) "
            f"SELECT {', '.join(copied.get(c, c) for c in columns)} "
            f"FROM f, materializations WHERE id <= {top} ORDER BY f.n, id"
        )
        conn.commit()
        return conn.execute("SELECT count(*) FROM materializations").fetchone()[0]
    finally:
        conn.close()


def history(binary: str) -> tuple[bool, dict]:
    with tempfile.TemporaryDirectory(prefix="barca-perf-history-") as directory:
        project = Path(directory)
        (project / "barca.toml").write_text("")
        env = clean_env()

        def run(version: int) -> float | None:
            # Literal keys: planning reads them without importing the module.
            keys = [f"k{i:04d}" for i in range(HISTORY_KEYS)]
            source = HISTORY_SOURCE.format(keys=keys, version=version)
            (project / "assets.py").write_text(source)
            done = subprocess.run(
                [binary, "get", "assets.py", "--json"],
                cwd=project,
                env=env,
                capture_output=True,
                text=True,
            )
            assert done.returncode == 0, done.stdout + done.stderr[-2000:]
            assert json.loads(done.stdout)["steps_executed"] == HISTORY_KEYS, done.stdout
            found = re.findall(r"plan reasons so far ([0-9.]+)ms", done.stderr)
            return max(map(float, found)) if found else None

        run(0)
        rows = seed_history(project / ".barca/metadata.db", HISTORY_MULT)
        lookups = [run(version) for version in range(1, HISTORY_RUNS + 1)]
    row = {
        "check": "history",
        "keys": HISTORY_KEYS,
        "rows": rows,
        "plan_reasons_ms": lookups,
        "limits": {"plan_reasons_ms_median": HISTORY_MAX_MS},
    }
    if None in lookups:
        row["error"] = "no `plan reasons so far` in BARCA_TRACE_TIMING's output"
        return False, row
    row["plan_reasons_ms_median"] = statistics.median(lookups)
    return row["plan_reasons_ms_median"] < HISTORY_MAX_MS, row


def main() -> int:
    binary = barca_bin()
    version = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout
    print(f"perf-gate: {version.strip()} at {binary}", file=sys.stderr)
    failed = False
    lines = []
    for check in (trivial, incremental, history):
        ok, row = check(binary)
        print(json.dumps(row), flush=True)
        if not ok:
            print(f"perf-gate: {row['check']} over budget, measuring again", file=sys.stderr)
            ok, row = check(binary)
            row["second_attempt"] = True
            print(json.dumps(row), flush=True)
        failed |= not ok
        lines.append(f"{'pass' if ok else 'FAIL'} {json.dumps(row)}")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as out:
            out.write("### Performance budgets\n\n```\n" + "\n".join(lines) + "\n```\n")
    print("perf-gate: " + ("FAIL" if failed else "all budgets met"), file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
