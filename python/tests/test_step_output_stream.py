"""A step's own print() output goes to stderr, never stdout.

stdout carries only barca's result (one JSON document in JSON mode), so
`barca run ... | jq` works even when steps print. Before this fix the worker
inherited barca's stdout and step prints landed ahead of the JSON result.
"""

import json
import subprocess
from pathlib import Path

from barca.api import _find_binary

PIPELINE = """
import sys
from barca import asset, task


@asset()
def noisy() -> dict:
    print("hello from an asset")
    return {"n": 1}


@task(inputs={"x": noisy})
def chatty(x: dict) -> dict:
    print("hello from a task")
    sys.stdout.write("raw write\\n")
    return {"n": x["n"] + 1}
"""


def _barca(tmp: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=tmp, capture_output=True, text=True, timeout=120
    )


def test_step_prints_go_to_stderr_and_stdout_is_pure_json(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(PIPELINE)
    r = _barca(tmp_path, "run", "chatty", "p.py", "--json")
    assert r.returncode == 0, r.stderr
    assert "hello" not in r.stdout and "raw write" not in r.stdout
    assert "hello from an asset" in r.stderr
    assert "hello from a task" in r.stderr
    assert "raw write" in r.stderr
    doc = json.loads(r.stdout)  # the whole of stdout is one JSON document
    assert doc["final_output"] == {"n": 2}


def test_get_stdout_is_pure_json_with_printing_asset(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(PIPELINE)
    r = _barca(tmp_path, "get", "noisy", "p.py", "--json")
    assert r.returncode == 0, r.stderr
    assert json.loads(r.stdout)["final_output"] == {"n": 1}
