"""Input annotations must survive remote-only storage and batching (#1103, #814)."""

import json

import pytest

from barca import _artifacts, _batch, _storage

from .helpers import barca


@pytest.mark.parametrize("remote", ["local", "remote-only", "mixed"])
@pytest.mark.parametrize("batched", [False, True], ids=["single", "batch"])
@pytest.mark.parametrize("kind", ["duckdb", "polars_lazy"])
def test_reader_preserves_type_and_rows(tmp_path, monkeypatch, remote, batched, kind):
    import duckdb
    import polars as pl
    import pyarrow as pa
    import pyarrow.parquet as pq

    monkeypatch.chdir(tmp_path)
    paths = []
    for i in range(2):
        path = tmp_path / f"h{i}.parquet"
        pq.write_table(pa.table({"amount": [i + 1]}), path)
        if remote == "remote-only" or (remote == "mixed" and i == 0):
            uri = f"memory://input-types/{tmp_path.name}/h{i}.parquet"
            _storage.put_file(path, uri)
            path.unlink()
            paths.append(uri)
        else:
            paths.append(str(path))
    try:
        value = (
            _batch.lazy_table("rows", kind, ["k"], ["a", "b"], paths, ["k=a", "k=b"])
            if batched
            else _artifacts.deserialize(paths[0], "parquet", frame_type=kind)
        )
        expected_type = {
            "duckdb": duckdb.DuckDBPyRelation,
            "polars_lazy": pl.LazyFrame,
        }[kind]
        assert isinstance(value, expected_type), type(value)
        if kind == "duckdb":
            rows = value.arrow().read_all().to_pylist()
        else:
            rows = value.collect().to_dicts()
        expected = (
            [{"amount": 1, "k": "a"}, {"amount": 2, "k": "b"}] if batched else [{"amount": 1}]
        )
        assert sorted(rows, key=lambda row: row["amount"]) == expected
    finally:
        _batch.unbind()


@pytest.mark.parametrize(
    "annotation",
    [
        "list[ds.Dataset]",
        "list[ds.Dataset] | None",
        "None | list[ds.Dataset]",
        "Optional[list[ds.Dataset]]",
        "typing.Optional[list[ds.Dataset]]",
        "Union[list[ds.Dataset], None]",
        "typing.Union[None, list[ds.Dataset]]",
        "list[ds.Dataset | None]",
        "list[Optional[ds.Dataset]]",
        '"list[ds.Dataset] | None"',
    ],
)
def test_optional_collect_preserves_dataset(tmp_path, annotation):
    (tmp_path / "pipeline.py").write_text(f"""
import typing
from typing import Optional, Union
import pyarrow as pa
import pyarrow.dataset as ds
from barca import asset, collect, partitions

@asset(partitions={{"k": partitions(["a", "b"])}})
def rows(k: str) -> pa.Table:
    return pa.table({{"amount": [1]}})

@asset(inputs={{"parts": collect(rows)}})
def total(parts: {annotation} = None) -> int:
    assert all(isinstance(p, ds.Dataset) for p in parts), [type(p).__name__ for p in parts]
    return sum(p.count_rows() for p in parts)
""")
    result = barca(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == 2


@pytest.mark.parametrize("remote", [False, True], ids=["local", "remote-only"])
@pytest.mark.parametrize("batched", [False, True], ids=["single", "batch"])
@pytest.mark.parametrize("kind", ["duckdb", "polars_lazy", "pyarrow_dataset"])
@pytest.mark.parametrize("optional", [False, True], ids=["plain", "optional"])
def test_cli_input_types(tmp_path, remote, batched, kind, optional):
    from . import emulators
    from .helpers import clean_env
    from .test_state_backends import S3Backend

    env = clean_env(BARCA_POOL_SIZE="1")
    if remote:
        backend = S3Backend()
        emulators.require("s3", backend.available())
        env.update(backend.env())
        env["BARCA_REMOTE_URI"] = backend.make_uri(tmp_path).rsplit("/state/", 1)[0]
    annotation, read = {
        "duckdb": ("duckdb.DuckDBPyRelation", "rows.arrow().read_all().to_pylist()"),
        "polars_lazy": ("pl.LazyFrame", "rows.collect().to_dicts()"),
        "pyarrow_dataset": ("ds.Dataset", "rows.to_table().to_pylist()"),
    }[kind]
    (tmp_path / "pipeline.py").write_text(f"""
import duckdb
import polars as pl
import pyarrow as pa
import pyarrow.dataset as ds
from barca import asset, partitions

@asset(partitions={{"k": partitions(["a", "b"])}})
def rows(k: str) -> pa.Table:
    return pa.table({{"amount": [1 if k == "a" else 2]}})

@asset(inputs={{"rows": rows}}, partitions={{"k": partitions(["a", "b"])}}{", batch=3" if batched else ""})
def total(k: {"list[str]" if batched else "str"}, rows: {annotation}{" | None = None" if optional else ""}) -> dict:
    assert isinstance(rows, {annotation}), type(rows)
    if {batched!r}:
        assert len(k) == 2, k
    records = {read}
    return {{r["k"]: r["amount"] for r in records}} if {batched!r} else {{"amount": records[0]["amount"]}}
""")
    produced = barca(tmp_path, "get", "rows", "pipeline.py", env=env)
    assert produced.returncode == 0, produced.stderr
    copies = list((tmp_path / ".barca" / "artifacts").rglob("*.parquet"))
    assert len(copies) == 2
    if remote:
        for path in copies:
            path.unlink()
    result = barca(tmp_path, "get", "total", "pipeline.py", "-o", "value", env=env)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == (
        {"k=a": 1, "k=b": 2} if batched else {"k=a": {"amount": 1}, "k=b": {"amount": 2}}
    )
    if remote:
        assert all(p.exists() for p in copies) == (kind == "pyarrow_dataset")


@pytest.mark.parametrize(
    "annotation",
    [
        "dict[str, pa.Table]",
        "dict[str, pyarrow.Table] | None",
        "Optional[dict[str, pa.Table]]",
        "dict[str, Union[pa.Table, None]]",
    ],
)
def test_batched_optional_arrow_values(tmp_path, annotation):
    (tmp_path / "pipeline.py").write_text(f"""
import pyarrow
import pyarrow as pa
from typing import Optional, Union
from barca import asset, partitions

@asset(partitions={{"k": partitions(["a", "b"])}})
def rows(k: str) -> pa.Table:
    return pa.table({{"amount": [1]}})

@asset(inputs={{"rows": rows}}, partitions={{"k": partitions(["a", "b"])}}, batch=3)
def total(k: list[str], rows: {annotation} = None) -> dict:
    assert isinstance(rows, dict), type(rows)
    assert all(isinstance(v, pa.Table) for v in rows.values()), [type(v) for v in rows.values()]
    return {{key: rows[key].num_rows for key in k}}
""")
    result = barca(tmp_path, "get", "total", "pipeline.py", "-o", "value")
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {"k=a": 1, "k=b": 1}


def test_remote_dataset_cannot_silently_substitute_polars():
    with pytest.raises(ValueError, match="fetched to local disk"):
        _artifacts.deserialize("memory://missing.parquet", "parquet", frame_type="pyarrow_dataset")
