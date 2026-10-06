"""Helper edits the cone analysis used to miss now invalidate the step (#194).

Four patterns: a class body, an import inside the function body, a module used as a value,
and a module above the pipeline file's directory (under the project root). For each, a helper
edit re-runs the step; for all but the module-as-value case an edit to an unrelated definition
does not.
"""

import json
import os
import subprocess
from pathlib import Path

from barca.api import _find_binary


def barca(cwd: Path, *args: str) -> dict:
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def get(root: Path, file_arg: str) -> dict:
    return barca(root, "get", "val", file_arg)


def check(root: Path, file_arg: str, helper: Path, v1: str, v2: str, unrelated: str, out2):
    """Cold run, cached rerun, unrelated edit stays cached, relevant edit re-runs."""
    helper.write_text(v1)
    assert get(root, file_arg)["steps_executed"] == 1
    assert get(root, file_arg)["steps_executed"] == 0
    helper.write_text(unrelated)
    assert get(root, file_arg)["steps_executed"] == 0, "an unrelated helper edit must not re-run"
    helper.write_text(v2)
    after = get(root, file_arg)
    assert after["steps_executed"] == 1, "a helper edit must invalidate the cached result"
    assert after["final_output"] == out2


PIPE = """
from barca import asset
{imp}


@asset()
def val() -> int:
{body}
"""

FUNCS = "def compute():\n    return {a}\n\n\ndef unused():\n    return {b}\n"


def test_class_body_edit_reruns(tmp_path):
    (tmp_path / "p.py").write_text(
        PIPE.format(imp="from helpers import Model", body="    return Model().predict()")
    )
    cls = (
        "class Model:\n    def predict(self):\n        return {a}\n\n\n"
        "class Other:\n    def f(self):\n        return {b}\n"
    )
    check(
        tmp_path,
        "p.py",
        tmp_path / "helpers.py",
        cls.format(a=1, b=0),
        cls.format(a=22, b=0),
        cls.format(a=1, b=555),
        22,
    )


def test_import_inside_function_body_edit_reruns(tmp_path):
    (tmp_path / "p.py").write_text(
        PIPE.format(imp="", body="    from helpers import compute\n    return compute()")
    )
    check(
        tmp_path,
        "p.py",
        tmp_path / "helpers.py",
        FUNCS.format(a=1, b=0),
        FUNCS.format(a=22, b=0),
        FUNCS.format(a=1, b=555),
        22,
    )


def test_module_used_as_value_edit_reruns(tmp_path):
    (tmp_path / "p.py").write_text(
        PIPE.format(imp="import helpers", body='    return getattr(helpers, "compute")()')
    )
    helper = tmp_path / "helpers.py"
    helper.write_text(FUNCS.format(a=1, b=0))
    assert get(tmp_path, "p.py")["steps_executed"] == 1
    assert get(tmp_path, "p.py")["steps_executed"] == 0
    # The whole module is hashed when it is used as a value: any edit re-runs (documented).
    helper.write_text(FUNCS.format(a=1, b=555))
    assert get(tmp_path, "p.py")["steps_executed"] == 1
    helper.write_text(FUNCS.format(a=22, b=555))
    after = get(tmp_path, "p.py")
    assert after["steps_executed"] == 1
    assert after["final_output"] == 22


def test_module_above_the_pipeline_directory_edit_reruns(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipelines").mkdir()
    (tmp_path / "shared").mkdir()
    (tmp_path / "shared" / "__init__.py").write_text("")
    (tmp_path / "pipelines" / "p.py").write_text(
        PIPE.format(imp="from shared.utils import compute", body="    return compute()")
    )
    check(
        tmp_path,
        "pipelines/p.py",
        tmp_path / "shared" / "utils.py",
        FUNCS.format(a=1, b=0),
        FUNCS.format(a=22, b=0),
        FUNCS.format(a=1, b=555),
        22,
    )
