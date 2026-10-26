"""Known failure: concurrent identical assets must execute once, tasks twice.

Run from the repository root: .venv/bin/python benchmarks/partition_audit/check_overlap.py
Uses the installed barca package and exits nonzero when asset work is duplicated.
"""

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from python.tests.test_partition_audit import check_overlapping_requests

if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="overlap-", dir=Path(__file__).parent) as directory:
        root = Path(directory)
        for kind in ("task", "asset"):
            project = root / kind
            project.mkdir()
            check_overlapping_requests(project, kind)
            print(f"{kind}: passed")
