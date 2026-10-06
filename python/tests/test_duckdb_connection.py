"""barca owns the duckdb connection for duckdb-typed steps.

Inputs annotated `duckdb.DuckDBPyRelation` are loaded on one process-wide connection and bound
as views named after their parameters, so SQL by name works anywhere (helper modules included),
authors never write their own bind code, and nothing lingers after the step. The connection is
exposed as `barca.duckdb_connection()` so a module can configure it once per worker process.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

duckdb = pytest.importorskip("duckdb")

from barca import _duckdb  # noqa: E402
from barca.api import _find_binary  # noqa: E402

# ─── Unit: the connection and view binding ────────────────────────────────────


@pytest.fixture()
def parquet(tmp_path) -> str:
    path = str(tmp_path / "t.parquet")
    duckdb.sql("select 1 as k, 'a' as v").write_parquet(path)
    return path


def test_connection_is_the_one_inputs_load_on(parquet):
    import barca

    con = barca.duckdb_connection()
    assert con is barca.duckdb_connection()
    # Module-level duckdb functions and barca's connection are the same connection...
    duckdb.execute("create or replace table connection_probe as select 7 as x")
    try:
        assert con.sql("select x from connection_probe").fetchone() == (7,)
        # ...so a loaded input combines with relations made through either API.
        rel = duckdb.read_parquet(parquet)  # how barca loads a duckdb-typed input
        assert rel.join(con.sql("select 1 as k"), "k").fetchall() == [(1, "a")]
        assert rel.join(duckdb.sql("select 1 as k"), "k").fetchall() == [(1, "a")]
    finally:
        con.execute("drop table if exists connection_probe")


def test_bind_creates_views_named_after_duckdb_params_only(parquet):
    kwargs = {
        "orders": duckdb.read_parquet(parquet),
        "pdf": "not a relation",
        "n": 3,
    }
    param_types = {"orders": "duckdb", "pdf": "pandas"}
    bound = _duckdb.bind_inputs(kwargs, param_types)
    try:
        assert bound == ["orders"]
        assert duckdb.sql("select v from orders").fetchall() == [("a",)]
    finally:
        _duckdb.unbind_inputs(bound)


def test_unbind_drops_the_views(parquet):
    bound = _duckdb.bind_inputs({"orders": duckdb.read_parquet(parquet)}, {"orders": "duckdb"})
    _duckdb.unbind_inputs(bound)
    with pytest.raises(duckdb.Error):
        duckdb.sql("select * from orders").fetchall()
    _duckdb.unbind_inputs(bound)  # idempotent


def test_bind_never_clobbers_an_existing_table(parquet):
    con = _duckdb.connection()
    con.execute("create or replace table keepme as select 99 as x")
    try:
        bound = _duckdb.bind_inputs({"keepme": duckdb.read_parquet(parquet)}, {"keepme": "duckdb"})
        assert bound == []  # skipped, not an error
        assert con.sql("select x from keepme").fetchall() == [(99,)]
    finally:
        con.execute("drop table if exists keepme")


def test_standalone_use_outside_a_worker():
    import barca

    assert barca.duckdb_connection().sql("select 42").fetchone() == (42,)


# ─── Loud failure for connection conflicts ────────────────────────────────────


def _mixing_error(kind: str, parquet: str) -> Exception:
    """Provoke the real DuckDB errors a step gets when it opens its own connection."""
    inp = duckdb.read_parquet(parquet)
    own = duckdb.connect()
    try:
        if kind == "combine":
            inp.join(own.sql("select 1 as k"), "k").fetchall()
        elif kind == "register":
            own.register("x", inp)
        elif kind == "catalog":
            inp.create_view("bound_name", replace=True)
            own.sql("select * from bound_name").fetchall()
    except Exception as e:  # noqa: BLE001
        return e
    finally:
        own.close()
        duckdb.execute("drop view if exists bound_name")
    raise AssertionError(f"{kind} did not raise")


def test_explains_relations_from_different_connections(parquet):
    for kind in ("combine", "register"):
        note = _duckdb.explain_error(_mixing_error(kind, parquet), [])
        assert note and "two different connections" in note, kind
        assert "barca.duckdb_connection()" in note and "barca docs types" in note


def test_explains_querying_a_bound_view_from_another_connection(parquet):
    err = _mixing_error("catalog", parquet)
    note = _duckdb.explain_error(err, ["bound_name"])
    assert note and "`bound_name`" in note and "different connection" in note
    # The same catalog error for a name barca did not bind is just a normal error.
    assert _duckdb.explain_error(err, ["something_else"]) is None


def test_unrelated_errors_are_not_touched(parquet):
    assert _duckdb.explain_error(ValueError("kaboom"), ["orders"]) is None
    typo = None
    try:
        duckdb.sql("select * from no_such_table_anywhere").fetchall()
    except duckdb.Error as e:
        typo = e
    assert typo is not None and _duckdb.explain_error(typo, ["orders"]) is None
    # "replacement scan" for a non-relation object is a different problem.
    not_rel = duckdb.InvalidInputException(
        'Invalid Input Error: Python Object "d" of type "dict" not suitable for replacement scan.'
    )
    assert _duckdb.explain_error(not_rel, []) is None


# ─── End to end through the real worker ───────────────────────────────────────

PIPELINE = """
import duckdb
import barca
from barca import asset

# Configure the shared connection once per worker process.
barca.duckdb_connection().execute("create or replace macro twice(x) as x * 2")


def doubled_amounts():
    # A helper: `orders` is not a local here, so only a bound view can resolve it.
    return duckdb.sql("select order_id, twice(amount) as amount2 from orders order by order_id")


@asset()
def orders() -> duckdb.DuckDBPyRelation:
    return duckdb.sql(
        "select * from (values (1, 5.0::double), (2, 7.5::double)) t(order_id, amount)"
    )


@asset(inputs={"orders": orders})
def doubled(orders: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return doubled_amounts()


@asset(inputs={"d": doubled})
def as_pandas(d) -> dict:
    return {"type": type(d).__name__, "rows": d["amount2"].tolist()}


@asset(inputs={"_after": as_pandas})
def leak_check(_after) -> dict:
    try:
        duckdb.sql("select * from orders").fetchall()
        return {"leaked": True}
    except duckdb.Error:
        return {"leaked": False}
"""


@pytest.fixture(scope="module")
def binary() -> str:
    return _find_binary()


def run_get(binary: str, cwd: Path, target: str, *flags: str) -> dict:
    # One worker, so every step shares a process: this is where shared state would leak.
    env = {**os.environ, "BARCA_POOL_SIZE": "1"}
    proc = subprocess.run(
        [binary, "get", target, "pipeline.py", *flags],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"exit {proc.returncode}\n{proc.stderr}"
    return json.loads(proc.stdout.strip().splitlines()[-1])["final_output"]


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def test_sql_by_name_works_in_helpers_and_uses_the_configured_connection(binary, project):
    # `doubled` calls a helper that queries `orders` by name and uses a macro defined at import:
    # needs both the bound view and the shared (configured) connection.
    out = run_get(binary, project, "as_pandas", "--refresh-all")
    assert out == {"type": "DataFrame", "rows": [10.0, 15.0]}


def test_views_do_not_outlive_the_step(binary, project):
    # All four steps run in one worker; `leak_check` runs after `doubled` bound `orders`.
    assert run_get(binary, project, "leak_check", "--refresh-all") == {"leaked": False}


def test_unannotated_consumer_still_gets_a_pandas_dataframe(binary, project):
    # The relation a step returned must never be handed to the next step in its place.
    assert run_get(binary, project, "as_pandas", "--refresh-all")["type"] == "DataFrame"


MIXING_PIPELINE = """
import duckdb
from barca import asset


@asset()
def base() -> duckdb.DuckDBPyRelation:
    return duckdb.sql("select * from (values (1, 5.0::double), (2, 7.5::double)) t(order_id, amount)")


@asset(inputs={"orders": base})
def joined_on_own_connection(orders: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    own = duckdb.connect()
    return orders.join(own.sql("select 1 as order_id"), "order_id")


def query_on_own_connection():
    # No Python variable called `orders` exists here: only the view barca bound on ITS connection.
    return duckdb.connect().sql("select * from orders")


@asset(inputs={"orders": base})
def queried_via_helper(orders: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return query_on_own_connection()
"""


def run_failing(binary: str, cwd: Path, target: str) -> subprocess.CompletedProcess:
    proc = subprocess.run(
        [binary, "get", target, "pipeline.py", "--refresh-all"],
        cwd=cwd,
        env={**os.environ, "BARCA_POOL_SIZE": "1"},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 1, f"expected a failure, got exit {proc.returncode}"
    # stdout carries only the failed run's result line (#149), never the error text.
    assert json.loads(proc.stdout)["status"] == "failed"
    return proc


def test_mixing_connections_fails_loudly_with_what_to_do(binary, tmp_path):
    (tmp_path / "pipeline.py").write_text(MIXING_PIPELINE)
    err = run_failing(binary, tmp_path, "joined_on_own_connection").stderr
    assert "different connections" in err  # DuckDB's own message is kept
    assert "barca: this step used DuckDB relations from two different connections" in err
    assert "barca.duckdb_connection()" in err and "con.register" in err


def test_querying_a_bound_input_from_another_connection_fails_loudly(binary, tmp_path):
    (tmp_path / "pipeline.py").write_text(MIXING_PIPELINE)
    err = run_failing(binary, tmp_path, "queried_via_helper").stderr
    assert "barca: this step queried `orders` on a different connection" in err
