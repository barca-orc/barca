"""`--partition` selects keys across the whole planned cone (`barca docs partitions`, "Selecting
keys"): every partitioned node with a selected dimension computes only those keys, a fan-in over
them is skipped, and a selected key is computed exactly as a full run computes it."""

import json
import os
import subprocess
from pathlib import Path

import barca
import pytest
from barca.api import _find_binary

from .test_serve_robustness import Server

PIPELINE = """
from barca import asset, collect, partitions, partitions_from, task

RETAILERS = ["r1", "r2", "r3", "r4", "r5"]


@asset()
def rate() -> int:
    return 3


@asset(partitions={"currency": partitions(["eur", "usd"])})
def fx(currency: str) -> dict:
    return {"currency": currency, "rate": 2 if currency == "eur" else 1}


@asset(inputs={"rate": rate}, partitions={"retailer": partitions(RETAILERS)})
def orders(retailer: str, rate: int) -> dict:
    return {"retailer": retailer, "orders": int(retailer[1:]) * rate}


@asset(partitions={"retailer": partitions_from(orders)})
def revenue(retailer: str, orders: dict) -> dict:
    return {"retailer": retailer, "revenue": orders["orders"] * 10}


@asset(inputs={"fx": fx}, partitions={"retailer": partitions_from(revenue)})
def margin(retailer: str, revenue: dict, fx: list) -> dict:
    return {"retailer": retailer, "margin": revenue["revenue"] * len(fx)}


@asset(inputs={"orders": orders}, partitions={"retailer": partitions(["r1", "r2", "r3", "r4", "r5"])})
def audit(retailer: str, orders: dict) -> dict:
    return {"retailer": retailer, "ok": orders["retailer"] == retailer}


@asset(partitions={"retailer": partitions(["r1", "r9"])})
def returns(retailer: str) -> dict:
    return {"retailer": retailer}


@asset(inputs={"all": collect(margin)})
def total(all: list) -> int:
    return sum(m["margin"] for m in all)


@asset(inputs={"total": total})
def board(total: int) -> str:
    return f"total {total}"


@asset()
def names() -> list:
    return ["r1", "r2", "r7"]


@asset(partitions={"retailer": partitions_from(names)})
def late(retailer: str) -> str:
    return retailer.upper()


@task(partitions={"retailer": partitions_from(orders)})
def notify(retailer: str, orders: dict) -> None:
    print("notify", retailer)
"""


def _env() -> dict[str, str]:
    env = {**os.environ, "BARCA_REMOTE": "off", "BARCA_POOL_SIZE": "2"}
    for key in ("BARCA_OUTPUT", "BARCA_SERVER"):
        env.pop(key, None)
    return env


def cli(project: Path, *args: str, code: int = 0) -> subprocess.CompletedProcess:
    proc = subprocess.run(
        [_find_binary(), *args],
        check=False,
        cwd=project,
        capture_output=True,
        text=True,
        env=_env(),
        timeout=120,
    )
    assert proc.returncode == code, proc.stderr
    return proc


def doc(proc: subprocess.CompletedProcess) -> dict:
    return json.loads(proc.stdout.strip().splitlines()[-1])


def steps(d: dict) -> dict[str, dict]:
    return {s["id"].split(":", 1)[1]: s for s in d["steps"]}


def counts(d: dict) -> dict[str, tuple]:
    return {
        n["id"].split(":", 1)[1]: (n["selected"], n["skipped"])
        for n in d["partition_selection"]["nodes"]
    }


def keys_of(d: dict, node: str) -> list[str]:
    s = steps(d)[node]
    p = s.get("partitions")
    return sorted(p["will_run_keys"]) if p else []


@pytest.fixture()
def project(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


def test_a_chain_computes_only_the_selected_keys_end_to_end(project, monkeypatch):
    d = doc(cli(project, "get", "margin", "pipeline.py", "--partition", "retailer=r1,r2", "--json"))
    assert sorted(d["final_output"]) == ["retailer=r1", "retailer=r2"]
    s = steps(d)
    for node in ("orders", "revenue", "margin"):
        assert s[node]["partitions"]["total"] == 2, node
        assert keys_of(d, node) == ["retailer=r1", "retailer=r2"], node
    assert counts(d)["orders"] == (2, 3)
    assert counts(d)["margin"] == (2, 3)
    assert d["partition_selection"]["selection"] == {"retailer": ["r1", "r2"]}
    assert d["partition_selection"]["skipped_nodes"] == []
    # The upstream on another dimension keeps all its keys; the whole input runs once.
    assert s["fx"]["partitions"]["total"] == 2
    assert s["rate"]["status"] == "ran"
    monkeypatch.chdir(project)
    monkeypatch.setenv("BARCA_REMOTE", "off")
    values = barca.get("margin", "pipeline.py", partition="retailer=r1,r2")
    assert values == {
        "retailer=r1": {"retailer": "r1", "margin": 60},
        "retailer=r2": {"retailer": "r2", "margin": 120},
    }


def test_explicit_partitions_sharing_the_dimension_are_narrowed(project):
    d = doc(cli(project, "get", "audit", "pipeline.py", "--partition", "retailer=r3", "--json"))
    assert sorted(d["final_output"]) == ["retailer=r3"]
    assert keys_of(d, "orders") == ["retailer=r3"]
    assert counts(d)["audit"] == (1, 4)
    # `returns` declares r9, which no other node has: still a key of this run.
    d = doc(cli(project, "get", "returns", "pipeline.py", "--partition", "retailer=r9", "--json"))
    assert sorted(d["final_output"]) == ["retailer=r9"]


def test_a_node_on_another_dimension_is_planned_as_without_the_flag(project):
    cli(project, "get", "fx", "pipeline.py", "--json")
    d = doc(cli(project, "get", "margin", "pipeline.py", "--partition", "retailer=r4", "--json"))
    fx = steps(d)["fx"]
    assert fx["status"] == "cached" and fx["partitions"]["cached"] == 2
    assert "pipeline.py:fx" not in {n["id"] for n in d["partition_selection"]["nodes"]}


def test_a_fan_in_is_skipped_with_one_note_and_the_run_exits_0(project):
    proc = cli(project, "get", "pipeline.py", "--partition", "retailer=r1", "--json")
    d = doc(proc)
    s = steps(d)
    assert s["total"]["status"] == "skipped"
    assert s["total"]["reason"] == "partition_selection"
    assert "reads every key of 'margin'" in s["total"]["detail"]
    assert "run without --partition" in s["total"]["detail"]
    assert s["board"]["reason"] == "partition_selection"
    assert "depends on 'total'" in s["board"]["detail"]
    assert d["partition_selection"]["skipped_nodes"] == ["pipeline.py:total", "pipeline.py:board"]
    notes = [line for line in proc.stderr.splitlines() if "--partition skips" in line]
    assert notes == [
        (
            "[barca] note: --partition skips a fan-in that reads every key: total, and 1 node"
            " below it. Run without --partition to update them."
        )
    ]
    # A skipped fan-in is a usage error as a target, and under --refresh.
    err = cli(project, "get", "total", "pipeline.py", "--partition", "retailer=r1", code=2).stderr
    assert "'total' reads every key of 'margin'" in err
    err = cli(
        project, "get", "pipeline.py", "--partition", "retailer=r1", "--refresh", "total", code=2
    ).stderr
    assert "--partition skips 'total'" in err


def test_refresh_with_a_selection_refreshes_only_the_selected_keys(project):
    cli(project, "get", "margin", "pipeline.py", "--json")
    d = doc(
        cli(
            project,
            "get",
            "margin",
            "pipeline.py",
            "--partition",
            "retailer=r2",
            "--refresh",
            "orders",
            "--json",
        )
    )
    s = steps(d)
    for node in ("orders", "revenue", "margin"):
        assert s[node]["partitions"]["total"] == 1, node
        assert keys_of(d, node) == ["retailer=r2"], node
    # The other keys are untouched: a full run serves them, and r2, from cache.
    full = doc(cli(project, "get", "margin", "pipeline.py", "--json"))
    for node in ("orders", "revenue", "margin"):
        assert steps(full)[node]["status"] == "cached", node


def test_no_target_means_every_asset_and_sensor_for_those_keys(project):
    d = doc(cli(project, "get", "pipeline.py", "--partition", "retailer=r1,r2", "--json"))
    nodes = counts(d)
    assert nodes["orders"] == (2, 3)
    assert nodes["audit"] == (2, 3)
    assert nodes["returns"] == (1, 1)
    assert nodes["late"] == (2, 1)  # known once `names` ran
    assert "pipeline.py:notify" not in {n["id"] for n in d["partition_selection"]["nodes"]}
    # `run` narrows a task's cone the same way.
    d = doc(cli(project, "run", "notify", "pipeline.py", "--partition", "retailer=r5", "--json"))
    assert counts(d)["notify"] == (1, 4)
    assert keys_of(d, "notify") == ["retailer=r5"]


def test_an_unknown_value_or_dimension_is_a_usage_error_before_anything_runs(project):
    err = json.loads(
        cli(
            project, "get", "margin", "pipeline.py", "--partition", "retailer=r8", "--json", code=2
        ).stderr.splitlines()[-1]
    )
    assert err["kind"] == "usage"
    assert "retailer=r8" in err["error"] and "r1, r2, r3, r4, r5" in err["error"]
    err = cli(project, "get", "margin", "pipeline.py", "--partition", "region=emea", code=2).stderr
    assert "no node in the cone of 'margin' has dimension 'region'" in err
    assert "currency, retailer" in err
    err = cli(project, "get", "rate", "pipeline.py", "--partition", "retailer=r1", code=2).stderr
    assert "nothing in the cone of 'rate' is partitioned" in err
    assert not (project / ".barca" / "artifacts").exists()


def test_keys_known_late_are_filtered_once_the_source_ran(project):
    d = doc(cli(project, "get", "late", "pipeline.py", "--partition", "retailer=r7", "--json"))
    assert sorted(d["final_output"]) == ["retailer=r7"]
    assert counts(d)["late"] == (1, 2)
    proc = cli(project, "get", "late", "pipeline.py", "--partition", "retailer=r4", "--json")
    assert "'late' has none of the selected keys" in proc.stderr
    assert doc(proc)["final_output"] == {}
    assert counts(doc(proc))["late"] == (0, 3)


def test_a_selected_key_is_the_full_runs_key_and_is_served_from_cache(project):
    sel = doc(
        cli(project, "get", "margin", "pipeline.py", "--partition", "retailer=r1,r3", "--json")
    )
    full = doc(cli(project, "get", "margin", "pipeline.py", "--json"))
    for key in ("retailer=r1", "retailer=r3"):
        # The artifact path holds the run hash: the same key, the same hash and file.
        assert full["final_output"][key] == sel["final_output"][key], key
    for node in ("orders", "revenue", "margin"):
        p = steps(full)[node]["partitions"]
        assert (p["cached"], p["will_run"]) == (2, 3), node
        assert sorted(p["will_run_keys"]) == ["retailer=r2", "retailer=r4", "retailer=r5"]
    # And the other way round: after a full run a selection is all cache hits.
    again = doc(
        cli(project, "get", "margin", "pipeline.py", "--partition", "retailer=r5", "--json")
    )
    assert again["final_output"]["retailer=r5"] == full["final_output"]["retailer=r5"]
    assert all(steps(again)[n]["status"] == "cached" for n in ("orders", "revenue", "margin"))


def test_dry_run_and_plan_report_the_same_selection(project, monkeypatch):
    proc = cli(project, "get", "pipeline.py", "--partition", "retailer=r2", "--dry-run", "--json")
    d = doc(proc)
    assert d["partition_selection"]["skipped_nodes"] == ["pipeline.py:total", "pipeline.py:board"]
    s = steps(d)
    assert s["total"]["action"] == "skipped" and s["total"]["reason"] == "partition_selection"
    assert keys_of(d, "orders") == ["retailer=r2"]
    assert counts(d)["late"] == (None, None)  # `names` has not run yet
    assert "--partition skips a fan-in" in proc.stderr
    pretty = cli(
        project, "get", "pipeline.py", "--partition", "retailer=r2", "--dry-run", "--pretty"
    )
    assert "--partition retailer=r2: " in pretty.stdout
    assert "orders 1 of 5 keys" in pretty.stdout
    assert "Skipped (run without --partition to update): total, board" in pretty.stdout
    assert not (project / ".barca" / "artifacts").exists()

    plan = json.loads(cli(project, "plan", "pipeline.py", "--partition", "retailer=r2").stdout)
    assert plan["partition_selection"]["skipped_nodes"] == [
        "pipeline.py:total",
        "pipeline.py:board",
    ]
    planned = [s for p in plan["phases"] for st in p["streams"] for s in st["steps"]]
    assert "pipeline.py:total" not in planned and "pipeline.py:orders" in planned
    assert counts(plan)["orders"] == (1, 4)
    monkeypatch.chdir(project)
    assert (
        barca.plan("pipeline.py", partition={"retailer": "r2"})["partition_selection"]
        == plan["partition_selection"]
    )


def test_server_runs_carry_the_selection(project):
    server = Server(project, "--no-schedule")
    url = f"http://127.0.0.1:{server.port}"
    try:
        d = doc(
            cli(
                project,
                "get",
                "--partition",
                "retailer=r1",
                "--server",
                url,
                "--json",
            )
        )
        assert counts(d)["orders"] == (1, 4)
        assert d["partition_selection"]["skipped_nodes"] == [
            "pipeline.py:total",
            "pipeline.py:board",
        ]
        dry = doc(
            cli(
                project,
                "get",
                "margin",
                "--partition",
                "retailer=r2",
                "--dry-run",
                "--server",
                url,
                "--json",
            )
        )
        assert keys_of(dry, "margin") == ["retailer=r2"]
        d = doc(
            cli(project, "run", "notify", "--partition", "retailer=r3", "--server", url, "--json")
        )
        assert counts(d)["notify"] == (1, 4)
        err = cli(project, "get", "total", "--partition", "retailer=r1", "--server", url, code=2)
        assert "'total' reads every key of 'margin'" in err.stderr
        assert barca.get("margin", partition="retailer=r1", server=url) == {
            "retailer=r1": {"retailer": "r1", "margin": 60}
        }
    finally:
        server.stop()


def test_the_python_api_carries_the_selection(project, monkeypatch):
    monkeypatch.chdir(project)
    monkeypatch.setenv("BARCA_REMOTE", "off")
    assert barca.get("revenue", "pipeline.py", partition={"retailer": ["r4"]}) == {
        "retailer=r4": {"retailer": "r4", "revenue": 120}
    }
    # No target: the value of the last asset planned.
    barca.get("pipeline.py", partition="retailer=r4")
    assert barca.run("notify", "pipeline.py", partition="retailer=r4") == {"retailer=r4": None}
    with pytest.raises(barca.BarcaError, match="retailer=r8"):
        barca.get("revenue", "pipeline.py", partition="retailer=r8")
