"""A repair targets one partition and is reversible without changing its neighbors."""

import json

import pytest

from .helpers import barca

PIPELINE = """
from barca import asset, collect, partitions, partitions_from

@asset(partitions={"account": partitions(["bad", "good"])})
def raw(account: str) -> dict:
    if account == "bad":
        raise ValueError("broken feed")
    return {"v": 3}

@asset(partitions={"account": partitions_from(raw)})
def doubled(account: str, raw: dict) -> dict:
    return {"v": raw["v"] * 2}

@asset(partitions={"region": partitions(["us", "eu"])}, inputs={"rows": collect(raw)})
def unrelated(region: str, rows: dict) -> int:
    return len(rows)
"""


@pytest.fixture(params=[False, True], ids=["static", "runtime"])
def repair_project(project, request):
    if request.param:
        path = project / "pipeline.py"
        path.write_text(
            path.read_text().replace(
                '@asset(partitions={"account": partitions(["bad", "good"])})',
                '@asset()\ndef accounts() -> list:\n    return ["bad", "good"]\n\n'
                '@asset(partitions={"account": partitions_from(accounts)})',
            )
        )
    return project


def test_repair_one_failed_key_and_unpin(repair_project):
    project = repair_project
    first = barca(project, "get", "doubled", "--json")
    assert first.returncode == 1, first.stderr
    replacement = project / "repair.json"
    replacement.write_text('{"v": 21}')
    repair = barca(project, "pin", "raw[account=bad]", "--from-file", str(replacement), "--json")
    assert repair.returncode == 0, repair.stderr
    predicted = barca(
        project, "get", "doubled", "--partition", "account=bad", "--dry-run", "--json", timeout=10
    )
    assert predicted.returncode == 0, predicted.stderr
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


def test_repair_downstream_key_bypasses_its_broken_input(repair_project):
    project = repair_project
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


def test_unknown_key_upload_changes_nothing(repair_project):
    project = repair_project
    barca(project, "get", "doubled", "--json")
    replacement = project / "repair.json"
    replacement.write_text('{"v": 21}')
    result = barca(project, "pin", "raw[account=typo]", "--from-file", str(replacement), "--json")
    assert result.returncode == 2, result.stdout
    assert "unknown partition" in result.stderr
    listed = barca(project, "pin", "--json")
    assert json.loads(listed.stdout)["pins"] == []


def test_http_repair_and_rollback_show_the_held_version(repair_project):
    from urllib.parse import quote

    from .served import Serve

    project = repair_project
    barca(project, "get", "doubled", "--json")
    path = "/assets/" + quote("raw[account=bad]", safe="")
    with Serve(project, "--no-schedule", wait="health") as served:
        status, first = served.request(
            "POST", path + "/pin/upload?file_name=repair.json&reason=feed", {"v": 21}
        )
        assert status == 200, first
        status, second = served.request(
            "POST", path + "/pin/upload?file_name=newer.json", {"v": 99}
        )
        assert status == 200, second
        status, pinned = served.request(
            "POST", path + "/pin", {"run_hash": first["pin"]["run_hash"]}
        )
        assert status == 200, pinned
        status, grid = served.request("GET", "/assets/raw/partitions/grid")
        assert status == 200, grid
        assert grid["counts"]["pinned"] == 1
        assert "p" in grid["states"]
        status, detail = served.request("GET", "/assets/raw/partitions/key?key=account%3Dbad")
        assert status == 200, detail
        assert detail["state"] == "p"
        assert detail["output"]["artifact_path"] == first["version"]["artifact"]
        status, versions = served.request("GET", path + "/versions")
        assert status == 200, versions
        assert versions["pin"]["run_hash"] == first["pin"]["run_hash"]


def test_missing_pinned_file_never_recomputes_the_key(repair_project):
    from pathlib import Path

    project = repair_project
    barca(project, "get", "doubled", "--json")
    replacement = project / "repair.json"
    replacement.write_text('{"v": 21}')
    result = barca(project, "pin", "raw[account=bad]", "--from-file", str(replacement), "--json")
    assert result.returncode == 0, result.stderr
    artifact = Path(json.loads(result.stdout)["version"]["artifact"])
    (artifact if artifact.is_absolute() else project / artifact).unlink()
    run = barca(project, "get", "doubled", "--partition", "account=bad", "--json")
    assert run.returncode == 2, run.stderr
    assert "pinned" in run.stderr
    assert "broken feed" not in run.stderr
    prediction = barca(
        project, "get", "doubled", "--partition", "account=bad", "--dry-run", "--json"
    )
    assert prediction.returncode == 2, prediction.stderr
    assert "pinned" in prediction.stderr
