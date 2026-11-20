"""Row counts and data-health signals (#925): specs/behavior/inspection.md, "Row counts and
data-health signals", and specs/interfaces/worker-protocol.md, "Result row counts".

A real worker counts the rows of what each step returned and the coordinator records them;
`barca status`, `barca history RUN_ID`, `barca stats` and `GET /state` read them back, and
health, signals and data coverage (#954, #957) are classified from them on every surface.
"""

import json
import sqlite3
from pathlib import Path

import pytest

from .helpers import barca
from .served import get

pytest.importorskip("pandas")
pytest.importorskip("polars")
pytest.importorskip("pyarrow")
pytest.importorskip("duckdb")

PIPELINE = """
from pathlib import Path

import duckdb
import pandas as pd
import polars as pl

from barca import asset, collect, partitions, sensor

HERE = Path(__file__).parent


@sensor()
def version() -> str:
    return (HERE / "version.txt").read_text().strip()


@asset(inputs={"version": version}, partitions={"region": partitions(["a", "b", "c"])})
def orders(region: str, version: str) -> pd.DataFrame:
    v = int(version)
    n = {"a": 10 * v, "b": 5, "c": 0 if v >= 2 else 4}[region]
    return pd.DataFrame({"region": [region] * n, "amount": list(range(n))})


@asset(inputs={"orders": collect(orders)})
def orders_all(orders: list) -> pl.DataFrame:
    return pl.concat([pl.from_pandas(f) for f in orders])


@asset(inputs={"version": version})
def lazy(version: str) -> pl.LazyFrame:
    return pl.LazyFrame({"x": list(range(3 * int(version)))})


@asset(inputs={"version": version})
def relation(version: str) -> duckdb.DuckDBPyRelation:
    return duckdb.sql(f"select * from range({2 * int(version)})")


@asset(inputs={"version": version}, serializer="csv")
def lazy_csv(version: str) -> pl.LazyFrame:
    return pl.LazyFrame({"x": [1, 2]})


@asset(inputs={"version": version}, serializer="pickle")
def pickled(version: str) -> pd.DataFrame:
    return pd.DataFrame({"x": [1, 2, 3, 4]})


@asset(inputs={"version": version})
def not_a_frame(version: str) -> dict:
    return {"rows": 3}
"""


def recorded(root: Path) -> dict[str, list[int | None]]:
    """Each node id's recorded `rows`, oldest first."""
    con = sqlite3.connect(root / ".barca" / "metadata.db")
    try:
        out: dict[str, list[int | None]] = {}
        for node, rows in con.execute(
            "SELECT node_id, rows FROM materializations WHERE status = 'success' ORDER BY id"
        ):
            out.setdefault(node.split(":", 1)[1], []).append(rows)
        return out
    finally:
        con.close()


def get_all(root: Path, *extra: str):
    targets = "orders_all,lazy,relation,lazy_csv,pickled,not_a_frame"
    result = barca(root, "get", targets, "pipeline.py", "--json", *extra)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def test_rows_are_counted_for_frames_and_parquet_footers_and_nothing_else(project):
    (project / "version.txt").write_text("1")
    run = get_all(project)
    rows = recorded(project)
    assert rows["orders[region=a]"] == [10]
    assert rows["orders[region=c]"] == [4]
    assert rows["orders_all"] == [19], "a polars DataFrame's length"
    assert rows["lazy"] == [3], "a LazyFrame: the parquet footer"
    assert rows["relation"] == [2], "a DuckDB relation: the parquet footer"
    assert rows["pickled"] == [4], "a frame is counted in any format"
    assert rows["lazy_csv"] == [None], "a CSV of a lazy frame would need the whole file read"
    assert rows["not_a_frame"] == [None]
    assert rows["version"] == [None]

    status = json.loads(barca(project, "status", "pipeline.py", "--json").stdout)
    nodes = {n["name"]: n for n in status["nodes"]}
    assert nodes["lazy"]["last_materialization"]["rows"] == 3
    assert nodes["not_a_frame"]["last_materialization"]["rows"] is None
    assert "empty" not in nodes["orders"]["partitions"]

    history = json.loads(barca(project, "history", run["run_id"], "--json").stdout)
    steps = {s["node_id"].split(":", 1)[1]: s for s in history["steps"]}
    assert steps["relation"]["rows"] == 2
    assert steps["orders[region=b]"]["rows"] == 5
    assert steps["not_a_frame"]["rows"] is None

    # A partitioned node's stats entry is one run: its keys' rows added up.
    stats = json.loads(barca(project, "stats", "orders", "pipeline.py", "--json").stdout)
    assert stats["recent_runs"][0]["rows"] == 19
    assert stats["recent_runs"][0]["empty_keys"] == 0


def test_state_signals_empty_keys_changed_counts_and_a_fan_in_behind_its_input(project, serve):
    (project / "version.txt").write_text("1")
    get_all(project)
    # Version 2: key a doubles, key c is empty; only `orders` and `lazy` run again.
    (project / "version.txt").write_text("2")
    result = barca(project, "get", "orders,lazy", "pipeline.py", "--json")
    assert result.returncode == 0, result.stderr

    status = json.loads(barca(project, "status", "pipeline.py", "--json").stdout)
    nodes = {n["name"]: n for n in status["nodes"]}
    assert nodes["orders"]["partitions"]["empty"] == 1

    server = serve("--no-schedule")
    state = {row["name"]: row for row in get(server.port, "/state")}
    assert state["orders"]["signals"] == [
        {"kind": "empty", "empty_keys": 1, "keys": 3, "previous_empty_keys": 0},
        # a: 10 -> 20 and c: 4 -> 0 both moved by more than half; equal relative moves name
        # the first key.
        {
            "kind": "rows_changed",
            "changed_keys": 2,
            "key": "region=a",
            "rows": 20,
            "previous_rows": 10,
        },
    ]
    assert state["lazy"]["signals"] == [
        {"kind": "rows_changed", "changed_keys": 1, "key": None, "rows": 6, "previous_rows": 3}
    ]
    assert state["pickled"]["signals"] == []
    assert state["not_a_frame"]["signals"] == []
    # The fan-in is `stale`: its state already says it runs, so it is not `behind_input`.
    assert state["orders_all"]["cache"]["state"] == "stale"
    assert state["orders_all"]["signals"] == []
    # Unscheduled nodes have no slots.
    assert (state["orders"]["prev_run"], state["orders"]["late_since"]) == (None, None)

    # Health, signals and coverage are classified once, in barca-service (specs/behavior/health.md,
    # INV-HEALTH-02): the CLI, `GET /nodes`, `GET /state` and `barca.nodes` say the same.
    classified = ("health", "health_reason", "signals", "measured")
    status = json.loads(barca(project, "status", "pipeline.py", "--json").stdout)
    cli = {n["name"]: {k: n[k] for k in classified} for n in status["nodes"]}
    assert cli == {name: {k: row[k] for k in classified} for name, row in state.items()}
    served = get(server.port, "/nodes")
    assert cli == {n["name"]: {k: n[k] for k in classified} for n in served["nodes"]}
    assert (cli["orders"]["health"], cli["orders"]["health_reason"]) == ("suspect", "empty")
    assert (cli["lazy"]["health"], cli["lazy"]["health_reason"]) == ("suspect", "rows_changed")
    assert (cli["orders_all"]["health"], cli["orders_all"]["health_reason"]) == ("behind", "stale")
    assert (cli["version"]["health"], cli["version"]["measured"]) == ("ok", None)
    # A result no worker counted is not measured, and says nothing about its data.
    assert cli["not_a_frame"]["measured"] is False and cli["lazy_csv"]["measured"] is False
    assert cli["pickled"]["measured"] is True
    coverage = {"assets": 7, "measured": 5, "suspect": 2}
    assert status["data_coverage"] == coverage == served["data_coverage"]
    table = barca(project, "status", "pipeline.py", "--pretty").stdout
    assert "Data: 5 of 7 assets measured, 2 suspect" in table
    assert "Health: 2 suspect data, 5 behind, 1 ok" in table
    rows = json.loads(
        barca(
            project,
            "sql",
            "select name, health, health_reason, signals, measured from barca.nodes",
            "pipeline.py",
            "--json",
        ).stdout
    )["rows"]
    assert {r["name"]: r["health"] for r in rows} == {n: v["health"] for n, v in cli.items()}
    by_name = {r["name"]: r for r in rows}
    assert by_name["orders"]["signals"] == ["empty", "rows_changed"]
    assert by_name["version"]["measured"] is None

    grid = get(server.port, "/assets/orders/partitions/grid")
    assert grid["counts"]["empty"] == 1
    assert sorted(r for r in grid["rows"] if r is not None) == [0, 5, 20]
    keys = get(server.port, "/assets/orders/partitions")
    assert {k["key"]: k["rows"] for k in keys["keys"]} == {
        "region=a": 20,
        "region=b": 5,
        "region=c": 0,
    }


def test_an_unknown_fan_in_older_than_its_inputs_newest_result_is_behind_it(project, serve):
    # `orders` with cutoff=True: once its code changes, what reads it is `unknown` until it runs.
    pipeline = PIPELINE.replace(
        'partitions={"region": partitions(["a", "b", "c"])})',
        'partitions={"region": partitions(["a", "b", "c"])}, cutoff=True)',
    )
    (project / "pipeline.py").write_text(pipeline)
    (project / "version.txt").write_text("1")
    get_all(project)
    (project / "version.txt").write_text("2")
    assert barca(project, "get", "orders", "pipeline.py", "--json").returncode == 0
    (project / "pipeline.py").write_text(
        pipeline.replace("v = int(version)", "v = int(version) + 0")
    )

    server = serve("--no-schedule")
    state = {row["name"]: row for row in get(server.port, "/state")}
    fan_in = state["orders_all"]
    assert (fan_in["cache"]["state"], fan_in["cache"]["reason"]) == ("unknown", "cutoff_pending")
    [behind] = fan_in["signals"]
    assert behind["kind"] == "behind_input"
    assert behind["input"] == "pipeline.py:orders"
    assert behind["last_success_at"] == fan_in["last_success_at"]
    assert behind["input_success_at"] == state["orders"]["last_success_at"]
    assert behind["input_success_at"] >= behind["last_success_at"]
