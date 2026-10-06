"""`partitions_from(upstream)` on a partitioned upstream (#189).

The consumer gets the upstream's keys, and each key is called with the key and that key's
upstream output, passed under the upstream's name. It used to be called with no arguments at all
(`TypeError: margin() missing 2 required positional arguments`).

`partitions_from(<unpartitioned asset returning a list>)` (keys from the list's values) is covered
by test_partitions.sh, test_reliability.py and test_partition_unpartitioned_input.py.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
import os
from barca import asset, collect, partitions, partitions_from

REGIONS = os.environ.get("BARCA_TEST_REGIONS", "emea,amer,apac").split(",")


# Static upstream.
@asset(partitions={"region": partitions(["emea", "amer", "apac"])})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 100}


@asset(partitions={"region": partitions_from(sales)})
def margin(region: str, sales: dict) -> dict:
    return {"region": region, "margin": sales["revenue"] * 0.2}


# Chained: partitions_from over partitions_from.
@asset(partitions={"region": partitions_from(margin)})
def margin_pct(region: str, margin: dict) -> dict:
    return {"region": region, "pct": margin["margin"] / 100}


# An extra unpartitioned input (#175's shape) next to the partitioned upstream.
@asset()
def multiplier() -> int:
    return 3


@asset(inputs={"m": multiplier}, partitions={"region": partitions_from(sales)})
def scaled(region: str, sales: dict, m: int) -> dict:
    return {"region": region, "v": sales["revenue"] * m}


# Naming the upstream in inputs= passes it under that parameter name instead.
@asset(inputs={"s": sales}, partitions={"region": partitions_from(sales)})
def renamed(region: str, s: dict) -> dict:
    return {"region": region, "revenue": s["revenue"]}


# Dynamic upstream: partitions(<expression>), evaluated at plan time.
@asset(partitions={"region": partitions([r for r in REGIONS])})
def dyn_sales(region: str) -> dict:
    return {"region": region, "revenue": len(region)}


@asset(partitions={"region": partitions_from(dyn_sales)})
def dyn_margin(region: str, dyn_sales: dict) -> dict:
    return {"region": region, "revenue": dyn_sales["revenue"]}


# Dynamic upstream: partitions_from(<asset returning a list>), resolved at run time.
@asset()
def keys() -> list:
    return ["p", "q"]


@asset(partitions={"k": partitions_from(keys)})
def part(k: str) -> dict:
    return {"k": k, "v": k.upper()}


@asset(partitions={"k": partitions_from(part)})
def part_next(k: str, part: dict) -> dict:
    return {"k": k, "v": part["v"] + "!"}


# Fan-in names are chosen so no node id ends with another's name: `barca get margin_all` would
# also match `dyn_margin_all` (target names match by suffix).
def _rows(xs, field):
    return sorted([x["region"], x[field]] for x in xs)


@asset(inputs={"xs": collect(margin)})
def all_margin(xs: list) -> list:
    return _rows(xs, "margin")


@asset(inputs={"xs": collect(margin_pct)})
def all_pct(xs: list) -> list:
    return _rows(xs, "pct")


@asset(inputs={"xs": collect(scaled)})
def all_scaled(xs: list) -> list:
    return _rows(xs, "v")


@asset(inputs={"xs": collect(renamed)})
def all_renamed(xs: list) -> list:
    return _rows(xs, "revenue")


@asset(inputs={"xs": collect(dyn_margin)})
def all_dyn(xs: list) -> list:
    return _rows(xs, "revenue")


@asset(inputs={"xs": collect(part_next)})
def all_part_next(xs: list) -> list:
    return sorted([x["k"], x["v"]] for x in xs)
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "p.py").write_text(PIPELINE)
    return tmp_path


def run(project: Path, *args: str, regions: str = "emea,amer,apac"):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, "BARCA_TEST_REGIONS": regions},
        capture_output=True,
        text=True,
    )


def get(project: Path, target: str, *flags: str, regions: str = "emea,amer,apac") -> dict:
    proc = run(project, "get", target, "p.py", *flags, regions=regions)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def test_static_upstream(project):
    res = get(project, "all_margin")
    assert res["final_output"] == [["amer", 80.0], ["apac", 80.0], ["emea", 80.0]]
    assert res["steps_executed"] == 7  # three sales keys, three margin keys, the fan-in


def test_plan_lists_one_step_per_upstream_key(project):
    proc = run(project, "plan", "p.py")
    assert proc.returncode == 0, proc.stderr
    plan = json.loads(proc.stdout)
    steps = [s for p in plan["phases"] for st in p["streams"] for s in st["steps"]]
    assert steps.count("p.py:margin") == 3
    assert steps.count("p.py:margin_pct") == 3


def test_chained_partitions_from(project):
    assert get(project, "all_pct")["final_output"] == [
        ["amer", 0.8],
        ["apac", 0.8],
        ["emea", 0.8],
    ]


def test_extra_unpartitioned_input(project):
    assert get(project, "all_scaled")["final_output"] == [
        ["amer", 1200],
        ["apac", 1200],
        ["emea", 1200],
    ]


def test_inputs_entry_renames_the_upstream_parameter(project):
    assert get(project, "all_renamed")["final_output"] == [
        ["amer", 400],
        ["apac", 400],
        ["emea", 400],
    ]


def test_dynamic_expression_upstream(project):
    assert get(project, "all_dyn", regions="eu,us")["final_output"] == [
        ["eu", 2],
        ["us", 2],
    ]


def test_runtime_list_upstream(project):
    assert get(project, "all_part_next")["final_output"] == [["p", "P!"], ["q", "Q!"]]
    assert get(project, "all_part_next")["steps_executed"] == 0


def test_cached_per_key(project):
    first = get(project, "all_dyn", regions="eu,us")
    assert first["steps_executed"] == 5  # two dyn_sales keys, two dyn_margin keys, the fan-in
    assert get(project, "all_dyn", regions="eu,us")["steps_executed"] == 0

    # A new upstream key runs that key of the upstream and of the consumer; eu and us of both
    # are served from cache. Only the fan-in re-runs, because its set of inputs changed.
    grown = get(project, "all_dyn", regions="eu,us,apac")
    assert grown["final_output"] == [["apac", 4], ["eu", 2], ["us", 2]]
    assert grown["steps_executed"] == 3
    by = {s["id"]: s for s in grown["steps"]}
    for node in ("p.py:dyn_sales", "p.py:dyn_margin"):
        assert by[node]["partitions"]["cached"] == 2, by[node]
        assert by[node]["partitions"]["will_run_keys"] == ["region=apac"], by[node]
    assert by["p.py:all_dyn"]["status"] == "ran"


def test_changing_the_consumer_reruns_only_the_consumer(project):
    get(project, "all_margin")
    (project / "p.py").write_text(PIPELINE.replace("* 0.2}", "* 0.5}"))
    changed = get(project, "all_margin")
    assert changed["final_output"] == [["amer", 200.0], ["apac", 200.0], ["emea", 200.0]]
    assert changed["steps_executed"] == 4  # three margin keys and the fan-in; sales is cached


# ─── Usage errors ─────────────────────────────────────────────────────────────


IMPLICIT_COLLECT = """
from barca import asset, partitions


@asset(partitions={"region": partitions(["emea", "amer"])})
def sales(region: str) -> dict:
    return {"region": region}


@asset(inputs={"all_sales": sales})
def summary(all_sales: list) -> int:
    return len(all_sales)
"""


def test_partitioned_input_to_unpartitioned_consumer_is_a_usage_error(tmp_path):
    (tmp_path / "p.py").write_text(IMPLICIT_COLLECT)
    for cmd in (["get", "summary", "p.py"], ["plan", "p.py"], ["list", "p.py"]):
        proc = run(tmp_path, *cmd)
        assert proc.returncode == 2, (cmd, proc.stderr)
        assert "collect(sales)" in proc.stderr, proc.stderr
        assert "partitions_from(sales)" in proc.stderr, proc.stderr
    proc = run(tmp_path, "get", "summary", "p.py", "--json")
    err = json.loads(proc.stderr.strip().splitlines()[-1])
    assert err["kind"] == "usage" and err["code"] == 2
    assert "collect(sales)" in err["remediation"]
    assert "partitions_from(sales)" in err["remediation"]


DIMENSION_MISMATCH = """
from barca import asset, partitions, partitions_from


@asset(partitions={"region": partitions(["emea", "amer"])})
def sales(region: str) -> dict:
    return {"region": region}


@asset(partitions={"r": partitions_from(sales)})
def margin(r: str, sales: dict) -> dict:
    return sales
"""


def test_partitions_from_must_reuse_the_upstream_dimension_name(tmp_path):
    (tmp_path / "p.py").write_text(DIMENSION_MISMATCH)
    proc = run(tmp_path, "get", "margin", "p.py")
    assert proc.returncode == 2, proc.stderr
    assert "region" in proc.stderr and "partitions_from(sales)" in proc.stderr, proc.stderr
