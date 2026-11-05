"""#295: evaluated assignment receivers/indices participate in helper hashing."""

import json

import pytest
from .helpers import QUICK, barca, clean_env

CASES = [
    "values[index()] = 42",
    "values[index()]: int = 42",
    "values[index()] = other = 42",
    "values[index()], other = (42, 7)",
    "[values[index()], other] = (42, 7)",
    "for values[index()] in [42]:\n        pass",
    "with nullcontext(42) as values[index()]:\n        pass",
    "unused = [None for values[index()] in [42]]",
    "receiver(values).value = 42",
]


@pytest.mark.parametrize("statement", CASES)
def test_edit_to_helper_in_target_invalidates_only_affected_asset(tmp_path, statement):
    attribute = statement.startswith("receiver")
    setup = "[SimpleNamespace(value=0), SimpleNamespace(value=0)]" if attribute else "[0, 0]"
    result = "[item.value for item in values]" if attribute else "values"
    (tmp_path / "pipeline.py").write_text(f"""
from contextlib import nullcontext
from types import SimpleNamespace
from barca import asset
from helpers import index, receiver

@asset()
def value():
    values = {setup}
    {statement}
    return {result}

@asset()
def unrelated():
    return 7
""")
    helper = tmp_path / "helpers.py"
    env = clean_env(scrub=("BARCA_",), PYTHONDONTWRITEBYTECODE="1")

    def run():
        proc = barca(
            tmp_path,
            "get",
            "value,unrelated",
            "pipeline.py",
            "-o",
            "value",
            "--json",
            env=env,
            timeout=QUICK,
        )
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    def edit(index):
        helper.write_text(
            f"def index():\n    return {index}\n\ndef receiver(items):\n    return items[index()]\n"
        )

    edit(0)
    cold = run()
    assert cold["steps_executed"] == 2
    assert cold["targets"]["value"]["final_output"] == [42, 0]
    assert run()["steps_executed"] == 0
    edit(1)
    changed = run()
    assert changed["steps_executed"] == 1, "helper edit served stale assignment result"
    assert changed["targets"]["value"]["final_output"] == [0, 42]
    assert changed["targets"]["unrelated"]["final_output"] == 7
