"""A repair targets one partition and is reversible without changing its neighbors."""

import json

from .helpers import barca

PIPELINE = """
from barca import asset, partitions, partitions_from

@asset(partitions={"account": partitions(["bad", "good"])})
def raw(account: str) -> dict:
    if account == "bad":
        raise ValueError("broken feed")
    return {"v": 3}

@asset(partitions={"account": partitions_from(raw)})
def doubled(account: str, raw: dict) -> dict:
    return {"v": raw["v"] * 2}
"""


def test_repair_one_failed_key_and_unpin(project):
    first = barca(project, "get", "doubled", "--json")
    assert first.returncode == 1, first.stderr
    replacement = project / "repair.json"
    replacement.write_text('{"v": 21}')
    repair = barca(project, "pin", "raw[account=bad]", "--from-file", str(replacement), "--json")
    assert repair.returncode == 0, repair.stderr
    pin = json.loads(repair.stdout)
    assert pin["node_id"] == "pipeline.py:raw[account=bad]"
    for key, expected in [("bad", 42), ("good", 6)]:
        run = barca(project, "get", "doubled", "--partition", f"account={key}", "-o", "value")
        assert run.returncode == 0, run.stderr
        assert json.loads(run.stdout) == {f"account={key}": {"v": expected}}
    unpin = barca(project, "unpin", "raw[account=bad]", "--json")
    assert unpin.returncode == 0, unpin.stderr
    again = barca(project, "get", "doubled", "--partition", "account=bad", "--json")
    assert again.returncode == 1, again.stderr


def test_repair_downstream_key_bypasses_its_broken_input(project):
    first = barca(project, "get", "doubled", "--json")
    assert first.returncode == 1, first.stderr
    replacement = project / "repair.json"
    replacement.write_text('{"v": 42}')
    repair = barca(
        project, "pin", "doubled[account=bad]", "--from-file", str(replacement), "--json"
    )
    assert repair.returncode == 0, repair.stderr
    run = barca(project, "get", "doubled", "--partition", "account=bad", "-o", "value")
    assert run.returncode == 0, run.stderr
    assert json.loads(run.stdout) == {"account=bad": {"v": 42}}
    all_keys = barca(project, "get", "doubled", "-o", "value", "--refresh-all")
    assert all_keys.returncode == 0, all_keys.stderr
    assert json.loads(all_keys.stdout) == {"account=bad": {"v": 42}, "account=good": {"v": 6}}
