"""Equivalent table partitions keep their bytes and do not invalidate cutoff consumers."""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary


def get(project: Path, *flags: str) -> dict:
    env = {**os.environ, "BARCA_REMOTE": "off", "BARCA_POOL_SIZE": "1"}
    proc = subprocess.run(
        [_find_binary(), "get", "leaf", "pipeline.py", "--json", *flags],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def leaf_keys(result: dict) -> int:
    return next(s for s in result["steps"] if s["id"] == "pipeline.py:leaf")["partitions"][
        "will_run"
    ]


@pytest.mark.parametrize("kind", ["duckdb", "arrow", "polars"])
@pytest.mark.parametrize("serializer", ["parquet", "csv"])
def test_table_row_order_does_not_invalidate_unchanged_partitions(tmp_path, kind, serializer):
    returns = {
        "duckdb": "return rows",
        "arrow": "return rows.to_arrow_table()",
        "polars": "return pl.from_arrow(rows.to_arrow_table())",
    }
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "reverse.txt").write_text("false")
    (tmp_path / "edit.txt").write_text("0")
    (tmp_path / "pipeline.py").write_text(f'''
from pathlib import Path
import duckdb
import polars as pl
from barca import asset, partitions, partitions_from

HERE = Path(__file__).parent
duckdb.sql("SET threads=4")

@asset(partitions={{"account": partitions(["a", "b", "c"])}}, batch="all", cutoff=True, serializer={serializer!r})
def source(account: list[str]):
    direction = "DESC" if (HERE / "reverse.txt").read_text() == "true" else "ASC"
    rows = duckdb.sql("""
        SELECT account, i AS item_id, CASE WHEN i % 7 = 0 THEN NULL ELSE i % 5 END AS value
        FROM (VALUES ('a'), ('b'), ('c')) accounts(account), range(1000) items(i)
        WHERE account IN (SELECT unnest($accounts))
        ORDER BY account, item_id """ + direction, params={{"accounts": account}})
    edit = int((HERE / "edit.txt").read_text())
    rows = rows.project("account, item_id, CASE WHEN account = 'a' AND item_id = 1 THEN value + " + str(edit) + " ELSE value END AS value")
    {returns[kind]}

@asset(partitions={{"account": partitions_from(source)}}, batch="all")
def leaf(account: list[str], source: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    assert duckdb.sql("SELECT current_setting('threads')").fetchall()[0][0] == 4
    return source
''')
    assert leaf_keys(get(tmp_path)) == 3
    (tmp_path / "reverse.txt").write_text("true")
    result = get(tmp_path, "--refresh", "source")
    assert leaf_keys(result) == 0, result
    before = {
        key: (tmp_path / value["_barca_artifact"]["path"]).read_bytes()
        for key, value in result["final_output"].items()
    }
    (tmp_path / "edit.txt").write_text("10")
    changed = get(tmp_path, "--refresh", "source")
    assert leaf_keys(changed) == 1, changed
    assert leaf_keys(get(tmp_path, "--refresh", "source", "--batch", "none")) == 0
    for key in ["account=b", "account=c"]:
        assert (
            tmp_path / changed["final_output"][key]["_barca_artifact"]["path"]
        ).read_bytes() == before[key]


def test_parallel_partition_refresh_keeps_unchanged_consumers_cached(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text('''
import duckdb
from barca import asset, partitions, partitions_from

duckdb.sql("SET threads=4")

@asset(partitions={"account": partitions([str(i) for i in range(32)])}, batch="all", cutoff=True)
def source(account: list[str]) -> duckdb.DuckDBPyRelation:
    return duckdb.sql("""
        SELECT CAST(i // 32768 AS VARCHAR) AS account, i % 32768 AS item_id, i AS value
        FROM range(1048576) items(i)
        ORDER BY account, item_id
    """)

@asset(partitions={"account": partitions_from(source)}, batch="all")
def leaf(account: list[str], source: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    assert duckdb.sql("SELECT current_setting('threads')").fetchall()[0][0] == 4
    return source
''')
    assert leaf_keys(get(tmp_path)) == 32
    for _ in range(3):
        result = get(tmp_path, "--refresh", "source")
        assert leaf_keys(result) == 0, result


@pytest.mark.parametrize(
    "expression", ["'101'::BIT", "i::VARINT", "['101'::BIT]", "{'nested': ['101'::BIT]}"]
)
def test_ordered_partitions_support_bit_and_bignum_columns(tmp_path, expression):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "reverse.txt").write_text("false")
    (tmp_path / "pipeline.py").write_text(f'''
from pathlib import Path
import duckdb
from barca import asset, partitions, partitions_from

HERE = Path(__file__).parent

@asset(partitions={{"account": partitions(["a", "b"])}}, batch="all", cutoff=True)
def source(account: list[str]) -> duckdb.DuckDBPyRelation:
    direction = "DESC" if (HERE / "reverse.txt").read_text() == "true" else "ASC"
    return duckdb.sql("""
        SELECT account, i AS item_id, {expression} AS value
        FROM (VALUES ('a'), ('b')) accounts(account), range(1000) items(i)
        ORDER BY account, item_id """ + direction)

@asset(partitions={{"account": partitions_from(source)}}, batch="all")
def leaf(account: list[str], source: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return source
''')
    assert leaf_keys(get(tmp_path)) == 2
    (tmp_path / "reverse.txt").write_text("true")
    result = get(tmp_path, "--refresh", "source")
    assert leaf_keys(result) == 0, result


def test_multifile_partitions_do_not_invalidate_unchanged_consumers(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text('''
import duckdb
from barca import asset, partitions, partitions_from

duckdb.sql("SET threads=4")
duckdb.sql("SET partitioned_write_flush_threshold=2048")

@asset(partitions={"account": partitions(["a", "b"])}, batch="all", cutoff=True)
def source(account: list[str]) -> duckdb.DuckDBPyRelation:
    return duckdb.sql("""
        SELECT CASE WHEN i < 131072 THEN 'a' ELSE 'b' END AS account,
               i AS item_id, hash(i) % 97 AS value
        FROM range(132096) items(i)
    """)

@asset(partitions={"account": partitions_from(source)}, batch="all")
def leaf(account: list[str], source: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return source
''')
    assert leaf_keys(get(tmp_path)) == 2
    for _ in range(3):
        result = get(tmp_path, "--refresh", "source")
        assert leaf_keys(result) == 0, result


def test_partition_bytes_do_not_depend_on_writer_file_count(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "threshold.txt").write_text("1000000000")
    (tmp_path / "pipeline.py").write_text('''
from pathlib import Path
import duckdb
from barca import asset, partitions, partitions_from

HERE = Path(__file__).parent
duckdb.sql("SET threads=4")
duckdb.sql("SET disabled_optimizers='filter_pushdown'")

@asset(partitions={"account": partitions(["a", "b"])}, batch="all", cutoff=True)
def source(account: list[str]) -> duckdb.DuckDBPyRelation:
    threshold = int((HERE / "threshold.txt").read_text())
    duckdb.sql("SET partitioned_write_flush_threshold=" + str(threshold))
    return duckdb.sql("""
        SELECT CASE WHEN i % 2 = 0 THEN 'a' ELSE 'b' END AS account,
               i AS item_id, CASE WHEN i % 7 = 0 THEN NULL ELSE i % 97 END AS value
        FROM range(400000) items(i)
    """)

@asset(partitions={"account": partitions_from(source)}, batch="all")
def leaf(account: list[str], source: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    assert duckdb.sql("SELECT current_setting('disabled_optimizers')").fetchall()[0][0] == 'filter_pushdown'
    return source
''')
    assert leaf_keys(get(tmp_path)) == 2
    for threshold in [2048, 1000000000, 2048]:
        (tmp_path / "threshold.txt").write_text(str(threshold))
        result = get(tmp_path, "--refresh", "source")
        assert leaf_keys(result) == 0, result
