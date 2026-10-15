"""#295: unpacked and augmented module-level constants retain helper provenance."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


@pytest.mark.parametrize(
    "bindings",
    [
        "VALUE, OTHER = compute(), 7",
        "[VALUE, OTHER] = [compute(), 7]",
        "VALUE, *REST = [compute(), 7, 8]",
        "VALUE = 0\nVALUE += compute()",
        "from helpers import compute as VALUE\nVALUE = VALUE()\nVALUE += 1",
        "VALUE, OTHER = compute(), 7\nCAPTURED = VALUE\nVALUE = 0",
        "VALUE = 0\nVALUE += compute()\nCAPTURED = VALUE\nVALUE = 0",
    ],
)
def test_edit_to_helper_of_module_constant_invalidates_only_its_consumer(tmp_path, bindings):
    captured = "CAPTURED" if "CAPTURED" in bindings else "VALUE"
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\nfrom helpers import compute\n\n"
        + bindings
        + f"\n\n@asset()\ndef value(): return {captured}\n\n@asset()\ndef unrelated(): return 7\n"
    )
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

    helper.write_text("def compute(): return 1\n")
    cold = run()
    assert cold["steps_executed"] == 2
    extra = 1 if "VALUE += 1" in bindings else 0
    assert cold["targets"]["value"]["final_output"] == 1 + extra
    assert run()["steps_executed"] == 0
    helper.write_text("def compute(): return 222\n")
    changed = run()
    assert changed["steps_executed"] == 1, "module constant helper edit served stale result"
    assert changed["targets"]["value"]["final_output"] == 222 + extra
    assert changed["targets"]["unrelated"]["final_output"] == 7
