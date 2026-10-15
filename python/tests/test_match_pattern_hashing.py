"""#295: helpers used only in match patterns must invalidate cached assets."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


@pytest.mark.parametrize(
    "pattern,subject",
    [
        ("helpers.MATCH", "1"),
        ("helpers.MATCH | helpers.OTHER", "1"),
        ("helpers.MATCH as captured", "1"),
        ("[helpers.MATCH, *rest]", "[1, 99]"),
        ("{helpers.MATCH: captured}", "{1: 99}"),
        ("helpers.Record(value=captured)", "SimpleNamespace(value=1)"),
    ],
)
def test_edit_to_pattern_helper_recomputes_only_affected_asset(tmp_path, pattern, subject):
    (tmp_path / "pipeline.py").write_text(f"""
from types import SimpleNamespace
import helpers
from barca import asset

@asset()
def value():
    match {subject}:
        case {pattern}:
            return 42
        case _:
            return 0

@asset()
def unrelated():
    return 7
""")
    helper = tmp_path / "helpers.py"
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["PYTHONDONTWRITEBYTECODE"] = "1"

    def run():
        proc = subprocess.run(
            [_find_binary(), "get", "value,unrelated", "pipeline.py", "--json"],
            cwd=tmp_path,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    def edit(changed):
        helper.write_text(
            "from types import SimpleNamespace\n"
            + f"MATCH = {222 if changed else 1}\nOTHER = 100\nRecord = {'dict' if changed else 'SimpleNamespace'}\n"
        )

    edit(False)
    cold = run()
    assert cold["steps_executed"] == 2
    assert cold["targets"]["value"]["final_output"] == 42
    assert run()["steps_executed"] == 0
    edit(True)
    changed = run()
    assert changed["steps_executed"] == 1, "pattern helper edit served stale result"
    assert changed["targets"]["value"]["final_output"] == 0
    assert changed["targets"]["unrelated"]["final_output"] == 7
