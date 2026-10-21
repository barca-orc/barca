"""The planning pipeline (#250) at a chosen size: wall time and peak memory, cold and warm.

    python benchmarks/planning_pipeline/bench.py                  # the large size (scale 67)
    python benchmarks/planning_pipeline/bench.py --scale 8 --pool 4
    BARCA_REMOTE_URI=s3://bucket/prefix python benchmarks/planning_pipeline/bench.py   # store

Scale 67 is #233's setting: about 22M forecast rows, the central reconciled table (34
consumers) at the same row count. Barca runs from PATH (or `$BARCA`), with the environment it
is given: set `BARCA_REMOTE_URI` and the store's credentials to put barca's artifacts in a
store, and `PLANNING_SOURCE_MODE` (see `project/settings.py`) to read the sources from one.

Three runs, each reported with its wall time, the peak resident size of barca's whole process
tree and of its largest single process (`measure.py`):

1. cold: `barca run <all 42 tasks>` on a fresh `.barca/` (runs 72 assets and 42 tasks);
2. the two leaf assets: `barca get assets.py`;
3. warm: the same `barca run` again (every asset cached, the 42 tasks run).

Not run in CI: the large size needs several GB of memory and minutes.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(f"planning_pipeline_{name}", HERE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--scale", type=int, default=67, help="row-count multiplier (67: #233)")
    parser.add_argument("--seed", type=int, default=250, help="random seed for the data")
    parser.add_argument("--pool", type=int, default=4, help="BARCA_POOL_SIZE")
    parser.add_argument("--dir", help="project directory (default: a temp dir, removed after)")
    args = parser.parse_args()

    generate, measure = _load("generate"), _load("measure")
    barca = os.environ.get("BARCA") or shutil.which("barca") or sys.exit("barca not found")
    work = Path(args.dir) if args.dir else Path(tempfile.mkdtemp(prefix="planning-bench-"))
    try:
        started = time.monotonic()
        project = generate.write_project(work, scale=args.scale, seed=args.seed)
        sizes = generate.Sizes.scaled(args.scale)
        forecast = next((project.root / "data" / "forecast").rglob("*.parquet"))
        rows = sizes.accounts * sizes.groups * sizes.items_per_group * sizes.weeks
        print(
            f"scale {args.scale}: forecast {rows:,} rows, {forecast.stat().st_size / 2**20:.1f} MB "
            f"parquet; generated in {time.monotonic() - started:.1f} s; pool {args.pool}"
        )
        env = {**os.environ, "BARCA_POOL_SIZE": str(args.pool), "PYTHONDONTWRITEBYTECODE": "1"}
        tasks = ",".join(project.names("task"))
        runs = [
            ("cold run, 42 tasks", ["run", tasks, "assets.py", "--json"]),
            ("get, 2 leaf assets", ["get", "assets.py", "--json"]),
            ("warm run, 42 tasks", ["run", tasks, "assets.py", "--json"]),
        ]
        for label, argv in runs:
            out, err = project.root / ".bench.out", project.root / ".bench.err"
            started = time.monotonic()
            with out.open("w") as o, err.open("w") as e:
                code, tree, largest = measure.run(
                    [barca, *argv], cwd=project.root, env=env, stdout=o, stderr=e
                )
            elapsed = time.monotonic() - started
            if code != 0:
                sys.exit(f"{label}: barca exited {code}\n{err.read_text()[-4000:]}")
            executed = json.loads(out.read_text())["steps_executed"]
            print(
                f"  {label:<20} {elapsed:8.1f} s  {executed:3d} steps  "
                f"peak RSS tree {tree / 2**20:7.0f} MB, largest process {largest / 2**20:6.0f} MB"
            )
        status = json.loads(
            _barca_stdout(
                barca, ["status", "forecast_reconciled", "assets.py", "--json"], project.root, env
            )
        )
        node = next(n for n in status["nodes"] if n["name"] == "forecast_reconciled")
        print(
            f"  forecast_reconciled: {node.get('shape', {}).get('rows', '?'):,} rows, "
            f"{node['last_materialization']['size_bytes'] / 2**20:.1f} MB artifact"
        )
    finally:
        if not args.dir:
            shutil.rmtree(work, ignore_errors=True)


def _barca_stdout(barca: str, argv: list[str], cwd: Path, env: dict) -> str:
    import subprocess

    return subprocess.run(
        [barca, *argv], cwd=cwd, env=env, capture_output=True, text=True, check=True
    ).stdout


if __name__ == "__main__":
    main()
