"""`barca sql`: query cached artifacts with DuckDB, without writing a probe step (#202).

Every asset (and task) with a cached result is a view named after its function. The query runs
in an in-memory DuckDB over the artifact files: user code is never imported, nothing is
recorded, and nothing is written under .barca/.
"""

import json
import subprocess
import textwrap
from pathlib import Path

import pytest

from barca.api import _find_binary

pytest.importorskip("duckdb")
pytest.importorskip("pandas")

PIPELINE = """
import pandas as pd
from barca import asset, partitions, task


@asset()
def orders() -> pd.DataFrame:
    return pd.DataFrame(
        {"id": [1, 2, 3, 4, 5], "region": ["emea", "amer", "emea", "apac", "amer"],
         "amount": [10.0, 20.0, 30.0, 40.0, 50.5]}
    )


@asset(inputs={"o": orders})
def revenue(o: pd.DataFrame) -> pd.DataFrame:
    return o.groupby("region", as_index=False)["amount"].sum()


@asset()
def config() -> dict:
    return {"tolerance": 0.01, "owner": "planning"}


@asset()
def keys() -> list:
    return [{"ca": "CA3913", "ppg": "PPG2840", "residual": 0.0100001}]


@asset()
def blob() -> set:
    return {1, 2}


@asset(partitions={"week": partitions(["w1", "w2"])})
def weekly(week: str) -> pd.DataFrame:
    return pd.DataFrame({"week": [week], "units": [1 if week == "w1" else 2]})


@asset()
def never() -> pd.DataFrame:
    return pd.DataFrame({"x": [1]})


@task(inputs={"r": revenue})
def validate(r: pd.DataFrame) -> dict:
    return {"status": "PASS", "regions": len(r)}
"""


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=cwd, capture_output=True, text=True, check=False
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def envelope(proc: subprocess.CompletedProcess) -> dict:
    return json.loads(proc.stderr.strip().splitlines()[-1])


@pytest.fixture
def proj(tmp_path: Path) -> Path:
    root = tmp_path / "proj"
    (root / "sub").mkdir(parents=True)
    (root / "barca.toml").write_text("")
    (root / "pipeline.py").write_text(textwrap.dedent(PIPELINE))
    for target in ("revenue", "config", "keys", "blob", "weekly"):
        p = barca(root, "get", target, "--json")
        assert p.returncode == 0, p.stderr
    p = barca(root, "run", "validate", "--json")
    assert p.returncode == 0, p.stderr
    return root


def sql(cwd: Path, query: str, *args: str) -> subprocess.CompletedProcess:
    return barca(cwd, "sql", query, "--json", *args)


def test_query_a_parquet_asset(proj):
    out = ok(sql(proj, "select region, amount from revenue order by region"))
    assert out["columns"] == ["region", "amount"]
    assert out["rows"] == [
        {"region": "amer", "amount": 70.5},
        {"region": "apac", "amount": 40.0},
        {"region": "emea", "amount": 40.0},
    ]
    assert out["total"] == 3 and out["truncated"] is False


def test_join_two_assets(proj):
    q = """
        select o.region, count(*) as n, r.amount as total
        from orders o join revenue r using (region)
        group by o.region, r.amount order by o.region
    """
    rows = ok(sql(proj, q))["rows"]
    assert rows[0] == {"region": "amer", "n": 2, "total": 70.5}


def test_json_assets_are_views_too(proj):
    assert ok(sql(proj, "select owner from config"))["rows"] == [{"owner": "planning"}]
    rows = ok(sql(proj, "select ca, ppg, residual > 0.01 as over from keys"))["rows"]
    assert rows == [{"ca": "CA3913", "ppg": "PPG2840", "over": True}]


def test_task_results_are_views(proj):
    assert ok(sql(proj, "select status from validate"))["rows"] == [{"status": "PASS"}]


def test_a_partitioned_asset_is_one_view_with_a_partition_column(proj):
    rows = ok(sql(proj, "select partition, week, units from weekly order by week"))["rows"]
    assert rows == [
        {"partition": "week=w1", "week": "w1", "units": 1},
        {"partition": "week=w2", "week": "w2", "units": 2},
    ]


def test_limit_truncates_and_reports_the_total(proj):
    out = ok(sql(proj, "select * from orders order by id", "--limit", "2"))
    assert [r["id"] for r in out["rows"]] == [1, 2]
    assert out["truncated"] is True and out["total"] == 5
    assert ok(sql(proj, "select * from orders", "--all"))["total"] == 5


def test_an_asset_without_a_cached_result_says_how_to_get_one(proj):
    proc = sql(proj, "select * from never")
    assert proc.returncode == 2
    err = envelope(proc)
    assert err["kind"] == "usage"
    assert "barca get never" in err["remediation"]


def test_a_pickle_asset_is_not_queryable_and_says_so(proj):
    proc = sql(proj, "select * from blob")
    assert proc.returncode == 2
    assert "pickle" in envelope(proc)["error"]


def test_an_unknown_table_lists_the_views(proj):
    proc = sql(proj, "select * from revenu")
    assert proc.returncode == 2
    err = envelope(proc)
    assert "revenu" in err["error"]
    assert "revenue" in err["remediation"] and "orders" in err["remediation"]


def test_a_sql_error_is_a_usage_error_with_duckdbs_message(proj):
    proc = sql(proj, "selec 1")
    assert proc.returncode == 2
    assert "syntax error" in envelope(proc)["error"].lower()


def test_sql_records_and_writes_nothing(proj):
    before = ok(barca(proj, "history", "--json"))
    files = sorted((p, p.stat().st_mtime_ns) for p in (proj / ".barca").rglob("*") if p.is_file())
    ok(sql(proj, "select count(*) as n from orders"))
    assert ok(barca(proj, "history", "--json")) == before
    after = sorted((p, p.stat().st_mtime_ns) for p in (proj / ".barca").rglob("*") if p.is_file())
    assert after == files


def test_values_are_plain_json(proj):
    q = "select date '2026-10-04' as d, 'nan'::double as x, 1.5::decimal(4,2) as m, [1, 2] as l"
    assert ok(sql(proj, q))["rows"] == [{"d": "2026-10-04", "x": None, "m": 1.5, "l": [1, 2]}]


def test_a_stale_asset_is_queryable_with_a_note(proj):
    p = proj / "pipeline.py"
    p.write_text(p.read_text().replace('"owner": "planning"', '"owner": "ops"'))
    proc = sql(proj, "select owner from config")
    assert ok(proc)["rows"] == [{"owner": "planning"}]
    assert "stale" in proc.stderr and "config" in proc.stderr


def test_runs_from_a_subdirectory_and_scopes_to_files(proj):
    assert ok(sql(proj / "sub", "select count(*) as n from orders"))["rows"] == [{"n": 5}]
    (proj / "other.py").write_text(
        "from barca import asset\n\n@asset()\ndef orders() -> list:\n    return [{'x': 1}]\n"
    )
    # Two `orders` now: views are named by full id, and stderr says so.
    proc = sql(proj, 'select count(*) as n from "pipeline.py:orders"')
    assert ok(proc)["rows"] == [{"n": 5}]
    assert "pipeline.py:orders" in proc.stderr
    # A file argument scopes the views back to one `orders`.
    assert ok(sql(proj, "select count(*) as n from orders", "pipeline.py"))["rows"] == [{"n": 5}]


def test_pretty_output_is_a_table(proj):
    proc = barca(proj, "sql", "select region, amount from revenue order by region", "--pretty")
    assert proc.returncode == 0, proc.stderr
    lines = proc.stdout.splitlines()
    assert lines[0].split() == ["region", "amount"]
    assert lines[-1].split() == ["emea", "40.0"]
