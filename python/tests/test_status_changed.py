"""`barca status` and `--dry-run` say what changed in a stale node's run hash (#558).

Each successful materialization records a digest of each run-hash component; status and the dry
run name the components that differ from the node's last success. A row recorded without them
(before 0.29.0) keeps the plain `changed`.
"""

import json
import os
import sqlite3
import subprocess
from contextlib import closing
from pathlib import Path

import pytest
from barca.api import _find_binary

COMMON = """
def bucket_ltv(total):
    return "high" if total > 100 else "low"


def unused():
    return 0
"""

SOURCES = """
from pathlib import Path
from barca import asset, sensor


@sensor()
def orders_version() -> str:
    return Path("orders.txt").read_text()


@asset(inputs={"version": orders_version}, env=["BARCA_TEST_REGION"])
def orders(version: str) -> list:
    return [{"customer": "a", "total": 120}, {"customer": "b", "total": 10}]
"""

TRANSFORMS = """
from barca import asset

from pipelines.common import bucket_ltv
from pipelines.sources import orders


@asset(inputs={"rows": orders}, serializer="json")
def customer_ltv(rows: list) -> dict:
    return {r["customer"]: bucket_ltv(r["total"]) for r in rows}


@asset(inputs={"ltv": customer_ltv})
def ltv_buckets(ltv: dict) -> dict:
    out = {}
    for bucket in ltv.values():
        out[bucket] = out.get(bucket, 0) + 1
    return out
"""

ENV = {"BARCA_TEST_REGION": "eu"}


@pytest.fixture()
def project(tmp_path) -> Path:
    pkg = tmp_path / "pipelines"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("")
    (pkg / "common.py").write_text(COMMON)
    (pkg / "sources.py").write_text(SOURCES)
    (pkg / "transforms.py").write_text(TRANSFORMS)
    (tmp_path / "orders.txt").write_text("v1")
    return tmp_path


def barca(project: Path, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, **ENV, **(env or {})},
        capture_output=True,
        text=True,
        check=False,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def status(project: Path, env: dict | None = None) -> dict:
    result = ok(barca(project, "status", "--json", env=env))
    return {n["name"]: n["cache"] for n in result["nodes"]}


def dry_run(project: Path, env: dict | None = None) -> dict:
    result = ok(barca(project, "get", "ltv_buckets", "--dry-run", "--json", env=env))
    return {s["id"].rsplit(":", 1)[-1]: s for s in result["steps"]}


def edit(path: Path, old: str, new: str) -> None:
    text = path.read_text()
    assert old in text
    path.write_text(text.replace(old, new))


def test_a_helper_edit_names_the_helper_in_status_the_dry_run_and_the_table(project):
    ok(barca(project, "get", "ltv_buckets"))
    edit(project / "pipelines/common.py", "> 100", "> 50")

    cache = status(project)
    ltv = cache["customer_ltv"]
    assert (ltv["state"], ltv["reason"]) == ("stale", "changed")
    assert ltv["changed"] == ["helper pipelines/common.py:bucket_ltv"]
    assert ltv["detail"] == "changed: helper pipelines/common.py:bucket_ltv"
    # The node below names its stale upstream, and what it would see change.
    below = cache["ltv_buckets"]
    assert below["reason"] == "upstream_stale" and "customer_ltv" in below["detail"]
    assert below["changed"] == ["input pipelines/transforms.py:customer_ltv"]
    # Nodes that did not change carry no list.
    assert cache["orders"]["state"] == "cached" and "changed" not in cache["orders"]

    step = dry_run(project)["customer_ltv"]
    assert step["reason"] == "not_materialized"
    assert step["changed"] == ["helper pipelines/common.py:bucket_ltv"]
    assert step["detail"] == "changed: helper pipelines/common.py:bucket_ltv"

    table = barca(project, "status", "--pretty")
    assert table.returncode == 0, table.stderr
    row = next(line for line in table.stdout.splitlines() if line.startswith("customer_ltv"))
    assert "changed: helper pipelines/common.py:bucket_ltv" in row

    # An edit to a function the asset does not use changes nothing.
    ok(barca(project, "get", "ltv_buckets"))
    edit(project / "pipelines/common.py", "return 0", "return 1")
    assert status(project)["customer_ltv"]["state"] == "cached"


def test_code_decorator_arguments_and_input_changes_are_named(project):
    ok(barca(project, "get", "ltv_buckets"))
    transforms = project / "pipelines/transforms.py"
    edit(transforms, 'serializer="json"', 'serializer="pickle"')
    edit(transforms, "out = {}", "out = dict()")
    cache = status(project)
    assert cache["customer_ltv"]["changed"] == ["decorator argument serializer"]
    assert cache["customer_ltv"]["detail"] == "changed: decorator argument serializer"

    # Recompute only customer_ltv: ltv_buckets is then stale through its input and its code.
    ok(barca(project, "get", "customer_ltv"))
    below = status(project)["ltv_buckets"]
    assert below["reason"] == "changed"
    assert below["changed"] == ["code", "input pipelines/transforms.py:customer_ltv"]
    assert below["detail"] == "changed: code (+1 more)"


def test_a_declared_env_var_and_a_sensor_value_are_named(project):
    ok(barca(project, "get", "ltv_buckets"))
    cache = status(project, env={"BARCA_TEST_REGION": "us"})
    assert cache["orders"]["changed"] == ["env BARCA_TEST_REGION"]

    # The sensor returns a new value: what reads it is stale because of it.
    (project / "orders.txt").write_text("v2")
    ok(barca(project, "get", "orders_version"))
    cache = status(project)
    assert cache["orders"]["reason"] == "changed"
    assert cache["orders"]["changed"] == ["sensor pipelines/sources.py:orders_version"]


def test_a_result_recorded_without_components_says_changed_as_before(project):
    ok(barca(project, "get", "ltv_buckets"))
    # A row written by barca before 0.29.0 has no components.
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as conn:
        conn.execute("UPDATE materializations SET components = NULL")
        conn.commit()
    edit(project / "pipelines/common.py", "> 100", "> 50")
    ltv = status(project)["customer_ltv"]
    assert (ltv["state"], ltv["reason"]) == ("stale", "changed")
    assert "changed" not in ltv
    assert ltv["detail"].startswith("code or upstream outputs changed")
    step = dry_run(project)["customer_ltv"]
    assert "changed" not in step
    assert step["detail"] == "no cached result for this code and these inputs"


def test_a_definition_is_stored_once_and_rows_name_it(project):
    ok(barca(project, "get", "ltv_buckets"))
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as conn:
        rows = [json.loads(r[0]) for r in conn.execute("SELECT components FROM materializations")]
        stored = dict(conn.execute("SELECT * FROM definition_components"))
    assert rows
    assert all(row["@definition"] in stored for row in rows)
    assert not any(key == "code" or key.startswith("helper ") for row in rows for key in row)
    assert any("code" in json.loads(parts) for parts in stored.values())
    # The named digests still say what changed.
    edit(project / "pipelines/common.py", "> 100", "> 50")
    assert status(project)["customer_ltv"]["changed"] == ["helper pipelines/common.py:bucket_ltv"]
