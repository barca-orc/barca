"""References in exception handler types participate in dependency hashing (#295)."""

import json
import os

import pytest
from .helpers import barca


@pytest.mark.parametrize(
    "handler", ["helpers.handled()", "(KeyError, helpers.handled())", "handled()"]
)
@pytest.mark.parametrize("group", [False, True])
def test_exception_type_edit_invalidates_its_consumer(tmp_path, handler, group):
    (tmp_path / "helpers.py").write_text("def handled():\n    return ValueError\n")
    star = "*" if group else ""
    raised = 'ExceptionGroup("batch", [ValueError("bad")])' if group else 'ValueError("bad")'
    (tmp_path / "p.py").write_text(f"""
import helpers
from helpers import handled
from barca import asset

def helper():
    result = 0
    try:
        raise {raised}
    except{star} {handler}:
        result = 1
    except{star} ValueError:
        result = 222
    return result

@asset()
def value():
    return helper()

@asset()
def unrelated():
    return 7
""")

    def cli(*args):
        proc = barca(
            tmp_path,
            *args,
            env={**os.environ, "BARCA_REMOTE": "off", "PYTHONDONTWRITEBYTECODE": "1"},
        )
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    assert cli("get", "p.py", "-o", "value", "--json")["steps_executed"] == 2
    assert cli("get", "value", "p.py", "-o", "value", "--json")["final_output"] == 1
    assert cli("get", "p.py", "-o", "value", "--json")["steps_executed"] == 0
    (tmp_path / "helpers.py").write_text("def handled():\n    return TypeError\n")
    changed = cli("get", "p.py", "-o", "value", "--json")
    assert changed["steps_executed"] == 1
    assert {s["id"]: s["status"] for s in changed["steps"]} == {
        "p.py:value": "ran",
        "p.py:unrelated": "cached",
    }
    assert cli("get", "value", "p.py", "-o", "value", "--json")["final_output"] == 222
    assert cli("get", "p.py", "-o", "value", "--json")["steps_executed"] == 0
