"""Project references in set literals contribute dependencies (#295)."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


@pytest.mark.parametrize(
    "body",
    [
        "return sorted({helpers.compute(), 7})",
        "return sorted({compute(), 7})",
        "def inner():\n        return {helpers.compute(), 7}\n    return sorted(inner())",
    ],
)
def test_set_helper_edit_invalidates_only_its_consumer(tmp_path, body):
    (tmp_path / "helpers.py").write_text("def compute():\n    return 1\n")
    (tmp_path / "p.py").write_text(
        "import helpers\nfrom helpers import compute\nfrom barca import asset\n"
        "@asset()\ndef value():\n    " + body + "\n@asset()\ndef unrelated():\n    return 9\n"
    )

    def cli(*args):
        proc = subprocess.run(
            [_find_binary(), *args],
            cwd=tmp_path,
            env={**os.environ, "BARCA_REMOTE": "off", "PYTHONDONTWRITEBYTECODE": "1"},
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    assert cli("get", "p.py", "--json")["steps_executed"] == 2
    assert cli("get", "value", "p.py", "--json")["final_output"] == [1, 7]
    assert cli("get", "p.py", "--json")["steps_executed"] == 0
    (tmp_path / "helpers.py").write_text("def compute():\n    return 222\n")
    changed = cli("get", "p.py", "--json")
    assert changed["steps_executed"] == 1
    assert {s["id"]: s["status"] for s in changed["steps"]} == {
        "p.py:value": "ran",
        "p.py:unrelated": "cached",
    }
    assert cli("get", "value", "p.py", "--json")["final_output"] == [7, 222]
    assert cli("get", "p.py", "--json")["steps_executed"] == 0
