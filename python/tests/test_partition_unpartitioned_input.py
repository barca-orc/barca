"""A partitioned asset can take an unpartitioned input (#170).

Every key's call receives the partition key *and* the upstream value, the upstream's run hash is
part of every key's run hash (so changing the upstream re-runs every key), and `--refresh` of the
upstream cascades into the partitioned asset and past it.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

# `multiplier`'s return value is spliced in so a test can change its code (and so its run hash)
# without touching any other function's source.
PIPELINE = """
import time
from barca import asset, collect, partitions, partitions_from, task


@asset()
def multiplier() -> int:
    return {m}


@asset()
def offset() -> int:
    return 100


@asset()
def stamp() -> float:
    return time.time()  # different every time it re-runs


@asset(inputs={{"m": multiplier}}, partitions={{"k": partitions(["a", "b"])}})
def leaf(k: str, m: int) -> dict:
    return {{"k": k, "v": m}}


@asset(inputs={{"leaves": collect(leaf)}})
def summary(leaves: list) -> dict:
    return {{"v": sorted((x["k"], x["v"]) for x in leaves)}}


# Several unpartitioned inputs.
@asset(inputs={{"m": multiplier, "o": offset}}, partitions={{"k": partitions(["a", "b"])}})
def two_inputs(k: str, m: int, o: int) -> dict:
    return {{"k": k, "v": m + o}}


@asset(inputs={{"xs": collect(two_inputs)}})
def two_inputs_sum(xs: list) -> dict:
    return {{"v": sorted((x["k"], x["v"]) for x in xs)}}


# An unpartitioned input plus a collect() input.
@asset(partitions={{"j": partitions(["x", "y", "z"])}})
def other(j: str) -> str:
    return j


@asset(inputs={{"m": multiplier, "others": collect(other)}}, partitions={{"k": partitions(["a", "b"])}})
def with_collect(k: str, m: int, others: list) -> dict:
    return {{"k": k, "v": m, "others": sorted(others)}}


@asset(inputs={{"xs": collect(with_collect)}})
def with_collect_sum(xs: list) -> list:
    return sorted((x["k"], x["v"], x["others"]) for x in xs)


# A partitioned asset downstream of another partitioned asset with the same key, plus an
# unpartitioned input.
@asset(partitions={{"k": partitions(["a", "b"])}})
def fetch(k: str) -> str:
    return k.upper()


@asset(inputs={{"f": fetch, "m": multiplier}}, partitions={{"k": partitions(["a", "b"])}})
def transform(k: str, f: str, m: int) -> dict:
    return {{"k": k, "f": f, "v": m}}


@asset(inputs={{"xs": collect(transform)}})
def transform_sum(xs: list) -> list:
    return sorted((x["k"], x["f"], x["v"]) for x in xs)


# Dynamic partitions with an unpartitioned input.
@asset()
def keys() -> list:
    return ["p", "q", "r"]


@asset(inputs={{"m": multiplier}}, partitions={{"k": partitions_from(keys)}})
def dyn(k: str, m: int) -> dict:
    return {{"k": k, "v": m}}


@asset(inputs={{"xs": collect(dyn)}})
def dyn_sum(xs: list) -> list:
    return sorted((x["k"], x["v"]) for x in xs)


# Cascade: a task downstream of a partitioned asset whose unpartitioned input is refreshed.
@asset(inputs={{"s": stamp}}, partitions={{"k": partitions(["a", "b"])}})
def stamped(k: str, s: float) -> dict:
    return {{"k": k, "s": s}}


@task(inputs={{"xs": collect(stamped)}})
def report(xs: list) -> list:
    return sorted((x["k"], x["s"]) for x in xs)
"""


# The exact shape from #170: `multiplier` has a single consumer, so the planner fuses the two
# into one chain.
ISSUE = """
from barca import asset, partitions


@asset()
def multiplier() -> int:
    return {m}


@asset(inputs={{"m": multiplier}}, partitions={{"k": partitions(["a", "b"])}})
def leaf(k: str, m: int) -> dict:
    return {{"k": k, "v": m}}
"""


def write(project: Path, m: int = 3) -> None:
    (project / "p.py").write_text(PIPELINE.format(m=m))


@pytest.fixture()
def project(tmp_path) -> Path:
    write(tmp_path)
    return tmp_path


def barca(project: Path, *args: str) -> dict:
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env=os.environ,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def output(result: dict):
    return result["final_output"]


def test_issue_shape_materializes_both_keys(tmp_path):
    (tmp_path / "issue.py").write_text(ISSUE.format(m=3))
    res = barca(tmp_path, "get", "leaf", "issue.py")
    assert res["status"] == "success", res
    assert res["steps_executed"] == 3  # multiplier + both keys
    for key in ("a", "b"):
        (art,) = (tmp_path / ".barca" / "artifacts" / f"issue.py--leaf_k_{key}").glob("*.json")
        assert json.loads(art.read_text()) == {"k": key, "v": 3}

    assert barca(tmp_path, "get", "leaf", "issue.py")["steps_executed"] == 0

    # Changing multiplier re-runs every key.
    (tmp_path / "issue.py").write_text(ISSUE.format(m=5))
    changed = barca(tmp_path, "get", "leaf", "issue.py")
    assert changed["steps_executed"] == 3
    for key in ("a", "b"):
        arts = (tmp_path / ".barca" / "artifacts" / f"issue.py--leaf_k_{key}").glob("*.json")
        assert sorted(json.loads(a.read_text())["v"] for a in arts) == [3, 5]


def test_partitioned_target_receives_unpartitioned_input(project):
    res = barca(project, "get", "leaf", "p.py")
    assert res["status"] == "success", res
    assert output(barca(project, "get", "summary", "p.py")) == {"v": [["a", 3], ["b", 3]]}


def test_several_unpartitioned_inputs(project):
    assert output(barca(project, "get", "two_inputs_sum", "p.py")) == {
        "v": [["a", 103], ["b", 103]]
    }


def test_unpartitioned_input_plus_collect_input(project):
    assert output(barca(project, "get", "with_collect_sum", "p.py")) == [
        ["a", 3, ["x", "y", "z"]],
        ["b", 3, ["x", "y", "z"]],
    ]


def test_partitioned_chain_plus_unpartitioned_input(project):
    assert output(barca(project, "get", "transform_sum", "p.py")) == [
        ["a", "A", 3],
        ["b", "B", 3],
    ]


def test_dynamic_partitions_with_unpartitioned_input(project):
    assert output(barca(project, "get", "dyn_sum", "p.py")) == [["p", 3], ["q", 3], ["r", 3]]


def test_changing_the_unpartitioned_input_reruns_every_key(project):
    assert output(barca(project, "get", "summary", "p.py")) == {"v": [["a", 3], ["b", 3]]}
    assert barca(project, "get", "summary", "p.py")["steps_executed"] == 0

    write(project, m=5)
    changed = barca(project, "get", "summary", "p.py")
    assert output(changed) == {"v": [["a", 5], ["b", 5]]}
    # multiplier, both leaf keys, and summary.
    assert changed["steps_executed"] == 4


EXPECTED = {
    "two_inputs_sum": lambda m: {"v": [["a", m + 100], ["b", m + 100]]},
    "transform_sum": lambda m: [["a", "A", m], ["b", "B", m]],
    "dyn_sum": lambda m: [["p", m], ["q", m], ["r", m]],
}


@pytest.mark.parametrize("target", sorted(EXPECTED))
def test_changing_the_unpartitioned_input_reruns_every_key_in_each_shape(project, target):
    assert output(barca(project, "get", target, "p.py")) == EXPECTED[target](3)
    write(project, m=7)
    assert output(barca(project, "get", target, "p.py")) == EXPECTED[target](7)


def test_refresh_cascades_into_partitioned_asset(project):
    first = output(barca(project, "run", "report", "p.py"))
    assert [k for k, _ in first] == ["a", "b"]
    assert first[0][1] == first[1][1]  # both keys saw the same stamp

    # Without a refresh, the task re-runs but the asset chain is cached.
    assert output(barca(project, "run", "report", "p.py")) == first

    refreshed = barca(project, "run", "report", "p.py", "--refresh", "stamp")
    out = output(refreshed)
    assert [k for k, _ in out] == ["a", "b"]
    assert out[0][1] == out[1][1]
    assert out[0][1] != first[0][1], "stamped keys did not pick up the refreshed stamp"
    # stamp, both stamped keys, and the task.
    assert refreshed["steps_executed"] == 4

    # The refreshed values are now the cached ones.
    assert output(barca(project, "run", "report", "p.py")) == out


def test_issue_shape_with_upstream_already_cached(tmp_path):
    # With multiplier cached, leaf's keys used to be hashed before multiplier's run hash was
    # known, so their run hash left multiplier out and a changed multiplier served stale keys.
    (tmp_path / "issue.py").write_text(ISSUE.format(m=3))
    barca(tmp_path, "get", "multiplier", "issue.py")
    first = barca(tmp_path, "get", "leaf", "issue.py")
    assert first["final_output"]["v"] == 3

    (tmp_path / "issue.py").write_text(ISSUE.format(m=5))
    changed = barca(tmp_path, "get", "leaf", "issue.py")
    assert changed["steps_executed"] == 3
    assert changed["final_output"]["v"] == 5
