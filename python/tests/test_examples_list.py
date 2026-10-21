"""Every directory under examples/ loads: `barca list` exits 0 and finds at least one node.

Two examples went stale unnoticed (#497): their barca.toml kept a `[project]` table that barca
no longer accepts, so `barca list` exited 2. `test_example_basic_app.py` runs basic_app's nodes;
this test only checks that each example parses, so a new example is covered without editing it.
"""

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

EXAMPLES = Path(__file__).resolve().parents[2] / "examples"
NAMES = sorted(p.name for p in EXAMPLES.iterdir() if p.is_dir())


def test_examples_exist():
    assert NAMES, f"no example directories under {EXAMPLES}"


@pytest.mark.parametrize("name", NAMES)
def test_example_lists(name, tmp_path):
    dest = tmp_path / name
    shutil.copytree(EXAMPLES / name, dest, ignore=shutil.ignore_patterns(".barca", "tmp", ".venv"))
    proc = subprocess.run(
        [_find_binary(), "list", "--json", "--all"], cwd=dest, capture_output=True, text=True
    )
    assert proc.returncode == 0, (
        f"barca list in examples/{name}: exit {proc.returncode}\n{proc.stderr}"
    )
    assert json.loads(proc.stdout)["nodes"], f"examples/{name} defines no nodes"
