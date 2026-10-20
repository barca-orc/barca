"""#295: module-level control-flow bindings retain static helper provenance."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


@pytest.mark.parametrize(
    "bindings,expression",
    [
        ("for marker in [0]:\n    VALUE = compute()", "VALUE"),
        ("VALUE = 0\nwhile VALUE == 0:\n    VALUE = compute()", "VALUE"),
        ("with nullcontext(compute()) as VALUE:\n    pass", "VALUE"),
        ("match compute():\n    case VALUE:\n        pass", "VALUE"),
        ("for marker in [0]:\n    def result(): return compute()", "result()"),
        ("with nullcontext():\n    class Result:\n        value = compute()", "Result.value"),
        ("for marker in [0]:\n    VALUE = compute()\nCAPTURED = VALUE\nVALUE = 0", "CAPTURED"),
    ],
)
def test_helper_edit_in_control_flow_binding_invalidates_its_consumer(
    tmp_path, bindings, expression
):
    (tmp_path / "pipeline.py").write_text(
        "from contextlib import nullcontext\nfrom barca import asset\nfrom helpers import compute\n\n"
        + bindings
        + f"\n\n@asset()\ndef value(): return {expression}\n\n@asset()\ndef unrelated(): return 7\n"
    )
    helper = tmp_path / "helpers.py"
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["PYTHONDONTWRITEBYTECODE"] = "1"

    def run():
        proc = subprocess.run(
            [_find_binary(), "get", "value,unrelated", "pipeline.py", "-o", "value", "--json"],
            cwd=tmp_path,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    helper.write_text("def compute(): return 1\n")
    cold = run()
    assert cold["steps_executed"] == 2
    assert cold["targets"]["value"]["final_output"] == 1
    assert run()["steps_executed"] == 0
    helper.write_text("def compute(): return 222\n")
    changed = run()
    assert changed["steps_executed"] == 1, "control-flow binding helper edit served stale result"
    assert changed["targets"]["value"]["final_output"] == 222
    assert changed["targets"]["unrelated"]["final_output"] == 7
