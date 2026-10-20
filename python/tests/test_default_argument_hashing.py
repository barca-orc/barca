"""Default expressions participate in the dependency cone (#295)."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary

CASES = [
    """@asset()
def value(result=helpers.compute()):
    return result
""",
    """def helper(result=helpers.compute()):
    return result
@asset()
def value():
    return helper()
""",
    """def helper(*, result=helpers.compute()):
    return result
@asset()
def value():
    return helper()
""",
    # The default is evaluated before the new function's parameter binding exists.
    """@asset()
def value():
    def nested(helpers=helpers.compute()):
        return helpers
    return nested()
""",
    """@asset()
def value():
    return (lambda helpers=helpers.compute(): helpers)()
""",
]


def cli(root, *args):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env={**os.environ, "BARCA_REMOTE": "off", "PYTHONDONTWRITEBYTECODE": "1"},
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


@pytest.mark.parametrize("body", CASES)
def test_default_helper_edit_invalidates_only_its_consumers(tmp_path, body):
    (tmp_path / "helpers.py").write_text("def compute():\n    return 1\n")
    (tmp_path / "p.py").write_text(
        "import helpers\nfrom barca import asset\n"
        + body
        + "\n@asset()\ndef unrelated():\n    return 7\n"
    )
    first = cli(tmp_path, "get", "p.py", "-o", "value", "--json")
    assert first["steps_executed"] == 2
    assert cli(tmp_path, "get", "p.py", "-o", "value", "--json")["steps_executed"] == 0
    (tmp_path / "helpers.py").write_text("def compute():\n    return 222\n")
    changed = cli(tmp_path, "get", "p.py", "-o", "value", "--json")
    assert changed["steps_executed"] == 1
    assert {s["id"]: s["status"] for s in changed["steps"]} == {
        "p.py:value": "ran",
        "p.py:unrelated": "cached",
    }
    assert cli(tmp_path, "get", "value", "p.py", "-o", "value", "--json")["final_output"] == 222
    assert cli(tmp_path, "get", "p.py", "-o", "value", "--json")["steps_executed"] == 0
