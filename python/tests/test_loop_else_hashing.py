"""#295: helper references in for/while else blocks invalidate cached results."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


@pytest.mark.parametrize("loop", ["for item in []", "while False"])
@pytest.mark.parametrize("location", ["asset", "project_helper"])
def test_helper_edit_in_loop_else_recomputes_only_affected_asset(tmp_path, loop, location):
    body = f"    {loop}:\n        pass\n    else:\n        return compute()\n"
    if location == "asset":
        definition = "from settings import compute\n\n@asset()\ndef value():\n" + body
    else:
        (tmp_path / "helpers.py").write_text(
            "from settings import compute\n\ndef result():\n" + body
        )
        definition = "from helpers import result\n\n@asset()\ndef value():\n    return result()\n"
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n" + definition + "\n@asset()\ndef unrelated(): return 7\n"
    )
    helper = tmp_path / "settings.py"
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
    assert cold["targets"]["value"]["final_output"] == 1
    assert run()["steps_executed"] == 0
    helper.write_text("def compute(): return 222\n")
    changed = run()
    assert changed["steps_executed"] == 1, "loop else helper edit served stale result"
    assert changed["targets"]["value"]["final_output"] == 222
    assert changed["targets"]["unrelated"]["final_output"] == 7
