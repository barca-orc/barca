"""Inputs annotated `pl.LazyFrame` arrive as a LazyFrame, not a loaded DataFrame (#235).

A lazy annotation is how a step asks for only the data it uses: the query it builds decides
which columns and row groups are read. Handing it an eager DataFrame read the whole file.
"""

import textwrap
from pathlib import Path

import pytest

from barca import _artifacts, _runtime, _storage, _worker

pl = pytest.importorskip("polars")


@pytest.fixture
def project(tmp_path, monkeypatch):
    """Run in an empty project directory with a clean memory:// store."""
    monkeypatch.chdir(tmp_path)
    yield tmp_path
    _artifacts.release_fetched()
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


def _staged_files() -> list[Path]:
    root = Path(_artifacts._STAGING_DIR)
    return [p for p in root.rglob("*") if p.is_file()] if root.is_dir() else []


def _orders_parquet(path: str) -> str:
    """Write a small `orders` artifact (local path or remote URI) and return its path."""
    frame = pl.DataFrame({"k": [1, 2, 3], "v": ["a", "b", "c"], "amount": [1.0, 2.0, 3.0]})
    frame.write_parquet("src.parquet")
    if _storage.is_remote(path):
        _storage.put_file("src.parquet", path)
    else:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        Path("src.parquet").replace(path)
    return path


def _run_step(project, monkeypatch, body, function_name, inputs, param_types, art_dir, lru=None):
    source = project / "mod.py"
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
    ok = _worker._run_daemon_step(step, {}, art_dir, lru or _worker._ArtifactLRU())
    assert errors == []
    assert ok
    return step


# ─── deserialize ──────────────────────────────────────────────────────────────


def test_polars_lazy_reader_returns_a_lazyframe(project):
    path = _orders_parquet(str(project / "orders.parquet"))
    value = _artifacts.deserialize(path, "parquet", frame_type="polars_lazy")
    assert isinstance(value, pl.LazyFrame)
    assert value.select("k").collect()["k"].to_list() == [1, 2, 3]


def test_polars_reader_still_returns_a_dataframe(project):
    path = _orders_parquet(str(project / "orders.parquet"))
    assert isinstance(_artifacts.deserialize(path, "parquet", frame_type="polars"), pl.DataFrame)


# ─── daemon steps ─────────────────────────────────────────────────────────────

_RECEIVES_LAZY = """
    import polars as pl

    def big(orders: pl.LazyFrame):
        assert isinstance(orders, pl.LazyFrame), type(orders)
        return orders.filter(pl.col("k") > 1).select("k", "amount")
    """


@pytest.mark.parametrize("store", ["local", "remote"])
def test_lazy_input_is_a_lazyframe_and_its_query_is_materialized(project, monkeypatch, store):
    art_dir = str(project / "arts") if store == "local" else "memory://arts"
    src = _orders_parquet(_storage.join(art_dir, "orders/h1.parquet"))
    step = _run_step(
        project,
        monkeypatch,
        _RECEIVES_LAZY,
        "big",
        {"orders": str(src)},
        {"orders": "polars_lazy"},
        art_dir,
    )
    out = _artifacts.artifact_path(art_dir, step["node_id"], "parquet", "h2")
    got = _artifacts.deserialize(out, "parquet", frame_type="polars")
    assert got.to_dicts() == [{"k": 2, "amount": 2.0}, {"k": 3, "amount": 3.0}]
    assert _staged_files() == []


def test_lazy_remote_input_is_not_served_from_the_lru_after_its_file_is_gone(project, monkeypatch):
    """A LazyFrame scans its file when collected. A cached one would point at a deleted
    staging file on the next step, so lazy inputs are never kept in the in-memory LRU."""
    src = _orders_parquet("memory://arts/orders/h1.parquet")
    lru = _worker._ArtifactLRU()
    for _ in range(2):
        _run_step(
            project,
            monkeypatch,
            _RECEIVES_LAZY,
            "big",
            {"orders": src},
            {"orders": "polars_lazy"},
            "memory://arts",
            lru,
        )
    assert _staged_files() == []


def test_fan_in_of_lazy_inputs_is_a_list_of_lazyframes(project, monkeypatch):
    art_dir = str(project / "arts")
    parts = [_orders_parquet(str(project / "arts" / f"orders/p{i}.parquet")) for i in range(2)]
    body = """
        import polars as pl

        def total(parts: list[pl.LazyFrame]):
            assert all(isinstance(p, pl.LazyFrame) for p in parts)
            return pl.concat(parts).select(pl.col("amount").sum()).collect().item()
        """
    step = _run_step(
        project,
        monkeypatch,
        body,
        "total",
        {
            "parts": {
                "_collected": True,
                "artifacts": [{"path": p, "format": "parquet"} for p in parts],
            }
        },
        {"parts": "polars_lazy"},
        art_dir,
    )
    out = _artifacts.artifact_path(art_dir, step["node_id"], "json", "h2")
    assert _artifacts.deserialize(out, "json") == 12.0


# ─── the in-memory LRU keys a result by what it is ────────────────────────────


@pytest.mark.parametrize(
    "returns",
    [
        "pl.DataFrame({'k': [1, 2]})",
        "pl.LazyFrame({'k': [1, 2]})",
    ],
)
def test_a_cached_result_reaches_an_unannotated_consumer_as_pandas(project, monkeypatch, returns):
    """The producer's result is cached for consumers in the same worker. A polars result must
    not be served to a consumer that asked for the default (pandas) reader."""
    art_dir = str(project / "arts")
    lru = _worker._ArtifactLRU()
    producer = _run_step(
        project,
        monkeypatch,
        f"""
        import polars as pl

        def make():
            return {returns}
        """,
        "make",
        {},
        {},
        art_dir,
        lru,
    )
    produced = _artifacts.artifact_path(art_dir, producer["node_id"], "parquet", "h2")
    consumer = _run_step(
        project,
        monkeypatch,
        """
        def use(data):
            return type(data).__module__.split(".")[0]
        """,
        "use",
        {"data": str(produced)},
        {},
        art_dir,
        lru,
    )
    out = _artifacts.artifact_path(art_dir, consumer["node_id"], "json", "h2")
    assert _artifacts.deserialize(out, "json") == "pandas"
