"""Lazy inputs read a remote artifact in place: only what the step's query uses moves (#234).

A step that annotates an input `duckdb.DuckDBPyRelation` or `pl.LazyFrame` is asking for the
subset its query touches. With a remote store the worker used to download the whole object
first. These tests run against a fake object store that behaves like adlfs/s3fs (buffered
files that fetch byte ranges, with adlfs's 50 MB default read-ahead block) and count every byte
fetched.
"""

import textwrap
from pathlib import Path

import pytest

from barca import _artifacts, _duckdb, _runtime, _storage, _worker

duckdb = pytest.importorskip("duckdb")
pl = pytest.importorskip("polars")
pytest.importorskip("pyarrow")
pytest.importorskip("fsspec")

from fsspec.implementations.memory import MemoryFileSystem  # noqa: E402
from fsspec.spec import AbstractBufferedFile  # noqa: E402

SRC = "memory://arts/orders/h1.parquet"


class _RangeFile(AbstractBufferedFile):
    def _fetch_range(self, start, end):
        data = self.fs.store[self.path].getvalue()[start:end]
        self.fs.fetched += len(data)
        return data


class CountingStore(MemoryFileSystem):
    """memory:// whose read handles fetch byte ranges like a real object store and count them."""

    protocol = ("memory",)
    cachable = False  # a fresh instance per test, not fsspec's shared one

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.fetched = 0
        self.downloads = 0

    def _open(self, path, mode="rb", block_size=None, cache_type="readahead", **kwargs):
        if "r" not in mode:
            return super()._open(path, mode, **kwargs)
        path = self._strip_protocol(path)
        return _RangeFile(
            self,
            path,
            mode,
            block_size=block_size or 50 * 2**20,  # adlfs's default
            cache_type=cache_type,
            size=self.info(path)["size"],
        )

    def modified(self, path):
        # Like a real object store: every write gets a new mtime.
        return self.store[self._strip_protocol(path)].modified

    def get_file(self, rpath, lpath, **kwargs):
        self.downloads += 1
        return super().get_file(rpath, lpath, **kwargs)


@pytest.fixture
def store(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    fs = CountingStore()
    monkeypatch.setitem(_storage._fs_cache, "memory", fs)
    # An 8-column artifact in 20 row groups; column `a` is a small share of the file.
    duckdb.sql(
        """copy (select i::bigint as a, 'x' || (i % 1000) as b, random() as c, random() as d,
                        random() as e, random() as f, random() as g, random() as h
                 from range(200000) t(i))
           to 'src.parquet' (format parquet, row_group_size 10000)"""
    )
    fs.put_file("src.parquet", SRC)
    fs.fetched = 0
    yield fs
    fs.store.clear()


def _staged_files() -> list[Path]:
    root = Path(_artifacts._STAGING_DIR)
    return [p for p in root.rglob("*") if p.is_file()] if root.is_dir() else []


def _run_step(tmp_path, monkeypatch, body, function_name, inputs, param_types):
    source = tmp_path / "mod.py"
    source.write_text(textwrap.dedent(body))
    errors = []
    monkeypatch.setattr(_runtime, "emit_step_error", lambda **kw: errors.append(kw))
    step = {
        "node_id": f"mod.py:{function_name}",
        "function_name": function_name,
        "source_file": str(source),
        "kind": "task",
        "inputs": inputs,
        "param_types": param_types,
        "run_hash": "h2",
    }
    ok = _worker._run_daemon_step(step, {}, "memory://arts", _worker._ArtifactLRU())
    assert errors == []
    assert ok
    # What the step moved; reading its output back below is not part of it.
    store = _storage._fs_cache["memory"]
    store.step_fetched, store.step_downloads = store.fetched, store.downloads
    return _artifacts.artifact_path("memory://arts", step["node_id"], "json", "h2")


_DUCKDB_SUM = """
    import duckdb

    def total(orders: duckdb.DuckDBPyRelation):
        return orders.aggregate("sum(a)").fetchone()[0]
    """

_POLARS_SUM = """
    import polars as pl

    def total(orders: pl.LazyFrame):
        assert isinstance(orders, pl.LazyFrame), type(orders)
        return orders.select(pl.col("a").sum()).collect().item()
    """


@pytest.mark.parametrize("body,frame_type", [(_DUCKDB_SUM, "duckdb"), (_POLARS_SUM, "polars_lazy")])
def test_lazy_input_fetches_only_the_columns_it_reads(
    store, tmp_path, monkeypatch, body, frame_type
):
    size = store.info(SRC)["size"]
    out = _run_step(tmp_path, monkeypatch, body, "total", {"orders": SRC}, {"orders": frame_type})
    assert _artifacts.deserialize(out, "json") == sum(range(200000))
    assert store.step_downloads == 0, "a lazy input must not download the whole object"
    assert 0 < store.step_fetched < size * 0.25, f"fetched {store.step_fetched} of {size} bytes"
    assert _staged_files() == []


def test_lazy_filter_skips_row_groups(store, tmp_path, monkeypatch):
    size = store.info(SRC)["size"]
    body = """
        import duckdb

        def few(orders: duckdb.DuckDBPyRelation):
            return orders.filter("a < 10000").aggregate("count(*)").fetchone()[0]
        """
    out = _run_step(tmp_path, monkeypatch, body, "few", {"orders": SRC}, {"orders": "duckdb"})
    assert _artifacts.deserialize(out, "json") == 10000
    assert store.step_downloads == 0
    assert 0 < store.step_fetched < size * 0.1, f"fetched {store.step_fetched} of {size} bytes"


def test_relation_over_a_remote_input_is_materialized(store, tmp_path, monkeypatch):
    """The returned relation is still lazy when the function returns; writing it reads the
    remote object in place."""
    body = """
        import duckdb

        def small(orders: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
            return orders.filter("a < 3").project("a, b")
        """
    source = tmp_path / "mod.py"
    source.write_text(textwrap.dedent(body))
    errors = []
    monkeypatch.setattr(_runtime, "emit_step_error", lambda **kw: errors.append(kw))
    step = {
        "node_id": "mod.py:small",
        "function_name": "small",
        "source_file": str(source),
        "kind": "task",
        "inputs": {"orders": SRC},
        "param_types": {"orders": "duckdb"},
        "run_hash": "h2",
    }
    assert _worker._run_daemon_step(step, {}, "memory://arts", _worker._ArtifactLRU())
    assert errors == []
    assert store.downloads == 0
    out = _artifacts.artifact_path("memory://arts", "mod.py:small", "parquet", "h2")
    got = _artifacts.deserialize(out, "parquet", frame_type="polars").sort("a")
    assert got.to_dicts() == [{"a": 0, "b": "x0"}, {"a": 1, "b": "x1"}, {"a": 2, "b": "x2"}]


def test_fan_in_of_remote_lazy_inputs_reads_in_place(store, tmp_path, monkeypatch):
    store.copy(SRC, "memory://arts/orders/h2.parquet")
    body = """
        import polars as pl

        def total(parts: list[pl.LazyFrame]):
            return pl.concat(parts).select(pl.col("a").sum()).collect().item()
        """
    collected = {
        "_collected": True,
        "artifacts": [
            {"path": SRC, "format": "parquet"},
            {"path": "memory://arts/orders/h2.parquet", "format": "parquet"},
        ],
    }
    out = _run_step(
        tmp_path, monkeypatch, body, "total", {"parts": collected}, {"parts": "polars_lazy"}
    )
    assert _artifacts.deserialize(out, "json") == 2 * sum(range(200000))
    assert store.step_downloads == 0


def test_a_rewritten_artifact_is_read_again_not_served_from_duckdbs_cache(
    store, tmp_path, monkeypatch
):
    """duckdb caches remote reads per (path, mtime) in the worker. A refresh can rewrite the
    same artifact path, so the next read must see the new bytes."""
    body = """
        import duckdb

        def n(orders: duckdb.DuckDBPyRelation):
            return orders.aggregate("count(*)").fetchone()[0]
        """
    out = _run_step(tmp_path, monkeypatch, body, "n", {"orders": SRC}, {"orders": "duckdb"})
    assert _artifacts.deserialize(out, "json") == 200000
    duckdb.sql("copy (select 1::bigint as a) to 'one.parquet' (format parquet)")
    store.put_file("one.parquet", SRC)
    out = _run_step(tmp_path, monkeypatch, body, "n", {"orders": SRC}, {"orders": "duckdb"})
    assert _artifacts.deserialize(out, "json") == 1


def test_eager_input_still_downloads_the_whole_object(store, tmp_path, monkeypatch):
    body = """
        def total(orders):
            return int(orders["a"].sum())
        """
    out = _run_step(tmp_path, monkeypatch, body, "total", {"orders": SRC}, {})
    assert _artifacts.deserialize(out, "json") == sum(range(200000))
    assert store.step_downloads == 1
    assert _staged_files() == []


def test_the_users_own_remote_queries_keep_their_own_filesystem(store, tmp_path, monkeypatch):
    """Barca reads its artifacts under a private scheme, so registering its store on the shared
    connection never takes over `s3://`/`abfss://`/... URLs in the user's own SQL."""
    _run_step(tmp_path, monkeypatch, _DUCKDB_SUM, "total", {"orders": SRC}, {"orders": "duckdb"})
    registered = set(_duckdb.connection().list_filesystems())
    assert "memory" not in registered
    assert any(name.startswith("barca") for name in registered), registered
