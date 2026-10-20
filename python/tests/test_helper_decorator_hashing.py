"""#295: helper-only decorators participate in selective project dependency hashing."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


@pytest.mark.parametrize(
    "kind", ["function", "nested_function", "factory_argument", "class", "nested_class"]
)
def test_edit_to_helper_only_decorator_invalidates_affected_asset(tmp_path, kind):
    if kind == "function":
        code = "@decorate\ndef inner(): return 0\n\n@asset()\ndef value(): return inner()\n"
    elif kind == "nested_function":
        code = "@asset()\ndef value():\n    from helpers import decorate\n    @decorate\n    def inner(): return 0\n    return inner()\n"
    elif kind == "factory_argument":
        code = "def wrap(v):\n    return lambda f: lambda: v\n\n@wrap(compute())\ndef inner(): return 0\n\n@asset()\ndef value(): return inner()\n"
    elif kind == "class":
        code = "@decorate_class\nclass Inner:\n    value = 0\n\n@asset()\ndef value(): return Inner.value\n"
    else:
        code = "@asset()\ndef value():\n    from helpers import decorate_class\n    @decorate_class\n    class Inner:\n        value = 0\n    return Inner.value\n"
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\nfrom helpers import decorate, decorate_class, compute\n\n"
        + code
        + "\n@asset()\ndef unrelated(): return 7\n"
    )
    helper = tmp_path / "helpers.py"
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["PYTHONDONTWRITEBYTECODE"] = "1"

    def edit(v):
        helper.write_text(
            f"def compute(): return {v}\n\ndef decorate(fn):\n    return lambda: compute()\n\ndef decorate_class(cls):\n    cls.value = compute()\n    return cls\n"
        )

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

    edit(1)
    cold = run()
    assert cold["steps_executed"] == 2
    assert cold["targets"]["value"]["final_output"] == 1
    assert run()["steps_executed"] == 0
    edit(222)
    changed = run()
    assert changed["steps_executed"] == 1, "helper-only decorator edit served stale result"
    assert changed["targets"]["value"]["final_output"] == 222
    assert changed["targets"]["unrelated"]["final_output"] == 7
