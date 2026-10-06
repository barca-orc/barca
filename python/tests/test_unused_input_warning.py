"""A declared data input the step never uses is flagged at plan time (#231).

Every data input is loaded in full before the step runs. Barca reads the function body
statically and warns on stderr, plus a `warnings` entry in the `plan` / `get` JSON.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

from barca.api import _find_binary

PIPELINE = """
from barca import asset


@asset()
def raw() -> dict:
    return {"n": 1}


@asset(inputs={"data": raw})
def dropped(data: dict) -> int:
    del data
    return 1


@asset(inputs={"data": raw})
def ignored(data: dict) -> int:
    return 2


@asset(inputs={"data": raw})
def used(data: dict) -> int:
    return helper(data)


@asset(inputs={"_data": raw})
def ordering_only(_data) -> int:
    return 3


def helper(d):
    return d["n"]
"""


def barca(tmp: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=tmp,
        env={**os.environ, "BARCA_OUTPUT": "json"},
        capture_output=True,
        text=True,
    )


def test_plan_warns_on_stderr_and_in_json(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    proc = barca(tmp_path, "plan", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    warnings = json.loads(proc.stdout)["warnings"]
    assert {(w["node"], w["param"]) for w in warnings} == {
        ("dropped", "data"),
        ("ignored", "data"),
    }
    assert all(w["kind"] == "unused_input" for w in warnings)
    assert "rename the parameter `_data`" in warnings[0]["message"]
    assert proc.stderr.count("[barca] warning:") == 2
    assert "step `used`" not in proc.stderr
    assert "ordering_only" not in proc.stderr


def test_get_json_carries_warnings_and_still_succeeds(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    proc = barca(tmp_path, "get", "ignored", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    out = json.loads(proc.stdout.strip().splitlines()[-1])
    assert out["status"] == "success"
    assert {w["node"] for w in out["warnings"]} == {"dropped", "ignored"}
    assert proc.stderr.count("[barca] warning:") == 2


def test_no_warnings_key_when_clean(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n\n@asset()\ndef a() -> int:\n    return 1\n"
    )
    proc = barca(tmp_path, "plan", "pipeline.py")
    assert "warnings" not in json.loads(proc.stdout)
    assert "warning" not in proc.stderr
