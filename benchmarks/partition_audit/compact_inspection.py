"""Seed and measure a real aligned chain, including inspection during a held sensor poll.

BARCA_BIN=.venv/bin/barca KEYS=5000 PROJECT_DIR=$PWD/target-local/inspect \
  INSPECTION_MAX_KIB=97656 INSPECTION_MAX_SECONDS=10 python compact_inspection.py

Run each size separately (5000, 50000, 200000). Seed cost is excluded from inspection;
all selected artifacts are real files. PROJECT_DIR must be an empty owned directory. The original 100 MB acceptance
budget remains intentionally red on some served-run samples; records are printed first.
"""

import hashlib
import importlib.util
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

import recheck_inspection


def main():
    root = Path(os.environ["PROJECT_DIR"]).resolve()
    root.mkdir(parents=True, exist_ok=False)
    binary = str(Path(os.environ["BARCA_BIN"]).resolve())
    count = int(os.environ.get("KEYS", "5000"))
    spec = importlib.util.spec_from_file_location(
        "fixture", Path(__file__).parents[1] / "batched_partitions/bench.py"
    )
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    source = fixture.pipeline("1000").replace(
        "    return True, json.loads",
        '    if Path("delay").exists():\n'
        "        import time\n"
        '        Path("polling").touch()\n'
        '        time.sleep(float(Path("delay").read_text()))\n'
        "    return True, json.loads",
    )
    (root / "pipeline.py").write_text(source)
    (root / "barca.toml").write_text("")
    (root / "versions.json").write_text(json.dumps({f"r{i}": 1 for i in range(count)}))
    print(
        json.dumps(
            {
                "keys": count,
                "binary_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                "python": sys.version,
                "machine": platform.platform(),
                "load": os.getloadavg(),
                "pool": os.environ.get("BARCA_POOL_SIZE", "default"),
            }
        ),
        flush=True,
    )
    for case in ("seed", "proof"):
        result = subprocess.run(
            [binary, "get", "scored", "pipeline.py", "--json"],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=600,
        )
        assert result.returncode == 0, result.stderr
        document = json.loads(result.stdout)
        assert document["steps_executed"] == (1 + 2 * count if case == "seed" else 1)
        print(json.dumps({"case": case, "steps": document["steps_executed"]}), flush=True)
    os.environ.update(BARCA_BIN=binary, EXPECT_KEYS=str(count), ACTIVE_CHECK="1", POST_RUN_NODES="1")
    for key, value in {
        "INSPECTION_MAX_KIB": "97656",
        "INSPECTION_RETAINED_MAX_KIB": "97656",
        "INSPECTION_COLD_MAX_KIB": "97656",
        "INSPECTION_MAX_SECONDS": "10",
    }.items():
        os.environ.setdefault(key, value)
    recheck_inspection.main()


if __name__ == "__main__":
    main()
