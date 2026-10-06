#!/usr/bin/env python3
"""Profile barca's internal timing breakdown. Run before pushing PRs.

Checks for performance regressions by measuring planning time, per-step
overhead, and total execution time against known thresholds.

Usage: python benchmarks/perf/profile.py
Exit code 0 = all checks pass, 1 = regression detected.
"""

import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

BARCA = str(Path(__file__).resolve().parent.parent.parent / ".venv" / "bin" / "barca")
REPO = str(Path(__file__).resolve().parent.parent.parent)

# Thresholds — if any measurement exceeds these, it's a regression.
THRESHOLDS = {
    "plan_2002_ms": 500,  # planning 2002 assets should be <500ms (includes cone hashing)
    "plan_100_ms": 50,  # planning 100 assets should be <50ms
    "per_step_ms": 1.0,  # per-step overhead should be <1ms
    "trivial_ms": 100,  # trivial (1 asset) total should be <100ms
}


def run_plan(asset_file, runs=3):
    """Measure planning time (no execution)."""
    times = []
    for _ in range(runs):
        t0 = time.perf_counter()
        subprocess.run([BARCA, "plan", asset_file], capture_output=True)
        times.append((time.perf_counter() - t0) * 1000)
    return min(times)  # best of N to reduce noise


def run_get(asset_file):
    """Measure full execution time."""
    shutil.rmtree(Path(REPO) / ".barca", ignore_errors=True)
    t0 = time.perf_counter()
    r = subprocess.run([BARCA, "get", asset_file, "--refresh-all"], capture_output=True, text=True)
    wall_ms = (time.perf_counter() - t0) * 1000
    try:
        last_json = [line for line in r.stdout.strip().splitlines() if line.startswith("{")][-1]
        data = json.loads(last_json)
        return {
            "wall_ms": wall_ms,
            "barca_ms": data["elapsed_seconds"] * 1000,
            "steps": data["steps_executed"],
        }
    except (IndexError, json.JSONDecodeError, KeyError):
        err = r.stderr[:200] if r.stderr else "(no stderr)"
        print(f"  WARNING: benchmark failed — {err}", file=sys.stderr)
        return {"wall_ms": wall_ms, "barca_ms": 0, "steps": 0, "error": err}


def main():
    passed = 0
    failed = 0

    def check(name, value, threshold, unit="ms"):
        nonlocal passed, failed
        ok = value <= threshold
        status = "✓" if ok else "✗ REGRESSION"
        print(f"  {status} {name}: {value:.1f}{unit} (threshold: {threshold}{unit})")
        if ok:
            passed += 1
        else:
            failed += 1

    print("=== Barca Performance Profile ===\n")

    # 1. Trivial (1 asset)
    trivial = Path(REPO) / "benchmarks" / "trivial" / "barca" / "assets.py"
    if trivial.exists():
        plan_trivial = run_plan(str(trivial))
        get_trivial = run_get(str(trivial))
        print(f"Trivial (1 asset): plan {plan_trivial:.0f}ms, total {get_trivial['wall_ms']:.0f}ms")
        check("trivial_total", get_trivial["wall_ms"], THRESHOLDS["trivial_ms"])

    # 2. Chain 100
    chain = Path(REPO) / "benchmarks" / "chain_100" / "barca" / "assets.py"
    if chain.exists():
        plan_chain = run_plan(str(chain))
        print(f"Chain 100: plan {plan_chain:.0f}ms")
        check("plan_100", plan_chain, THRESHOLDS["plan_100_ms"])

    # 3. Timeseries 1000 (2002 assets)
    ts = Path(REPO) / "benchmarks" / "timeseries_1000" / "barca" / "assets.py"
    if ts.exists():
        plan_ts = run_plan(str(ts))
        get_ts = run_get(str(ts))
        per_step = (get_ts["barca_ms"] - plan_ts) / get_ts["steps"] if get_ts["steps"] > 0 else 0
        print(
            f"Timeseries 1000 (2002 assets): plan {plan_ts:.0f}ms, "
            f"total {get_ts['wall_ms']:.0f}ms, per-step {per_step:.3f}ms"
        )
        check("plan_2002", plan_ts, THRESHOLDS["plan_2002_ms"])
        check("per_step", per_step, THRESHOLDS["per_step_ms"])

    print(f"\n{'=' * 40}")
    print(f"Results: {passed} passed, {failed} failed")

    if failed > 0:
        print("\nPerformance regression detected! Fix before merging.")
        sys.exit(1)
    else:
        print("\nAll performance checks passed.")
        sys.exit(0)


if __name__ == "__main__":
    main()
