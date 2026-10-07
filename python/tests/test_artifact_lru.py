"""The worker's tier-1 artifact cache: what it holds, and what a step can do to it (#232).

Two promises, each tested against the real libraries:

- Isolation. A value a step gets from the cache, and the object a step returned that the cache
  kept, are isolated from the cached value: no in-place edit of either changes what the cache
  hands to the next step.
- Bounds. An artifact is cached only when its serialized size is known and at most
  `_LRU_MAX_ARTIFACT_BYTES`, wherever it is stored, and the cache as a whole stays within a byte
  and an entry limit, so what a worker holds does not grow with the steps it runs.

pandas, polars and pyarrow come with the `test` extra, which is what CI installs; a test here
is skipped only where its library is missing.
"""

import gc
import textwrap
import tracemalloc

import pytest

from barca import _artifacts, _runtime, _storage, _worker
from barca._artifacts import serialize
from barca._worker import (
    _LRU_MAX_ARTIFACT_BYTES,
    _ArtifactLRU,
    _isolated_copy,
    _load_artifact,
    _load_collected_artifacts,
)

MB = 1024 * 1024


@pytest.fixture
def project(tmp_path, monkeypatch):
    """Run in an empty project directory with a clean memory:// store."""
    monkeypatch.chdir(tmp_path)
    yield tmp_path
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


def _run_step(project, monkeypatch, body, function_name, art_dir, lru, **step_fields) -> str:
    """Run one daemon-mode step against `lru` and return its artifact's path."""
    source = project / f"{function_name}.py"
    source.write_text(textwrap.dedent(body))
    errors = []
    monkeypatch.setattr(_runtime, "emit_step_error", lambda **kw: errors.append(kw))
    step = {
        "node_id": f"{source.name}:{function_name}",
        "function_name": function_name,
        "source_file": str(source),
        "kind": "asset",
        "inputs": {},
        "param_types": {},
        "run_hash": "h1",
        **step_fields,
    }
    ok = _worker._run_daemon_step(step, {}, art_dir, lru)
    assert errors == []
    assert ok
    for fmt in ("parquet", "json", "pickle"):
        path = str(_artifacts.artifact_path(art_dir, step["node_id"], fmt, step["run_hash"]))
        if _storage.exists(path):
            return path
    raise AssertionError(f"step {step['node_id']} wrote no artifact under {art_dir}")


# ─── Isolation: polars ────────────────────────────────────────────────────────


def _polars_frame():
    import polars as pl

    return pl.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})


def _pl_set_cell(df):
    df[0, "a"] = 99


def _pl_set_columns(df):
    import polars as pl

    df[["a"]] = pl.DataFrame({"a": [7, 8, 9]})


def _pl_insert_column(df):
    import polars as pl

    df.insert_column(0, pl.Series("n", [7, 8, 9]))


def _pl_replace_column(df):
    import polars as pl

    df.replace_column(0, pl.Series("a", [7, 8, 9]))


def _pl_extend(df):
    import polars as pl

    df.extend(pl.DataFrame({"a": [4], "b": ["w"]}))


def _pl_drop_in_place(df):
    df.drop_in_place("a")


def _pl_rename_columns(df):
    df.columns = ["p", "q"]


def _pl_hstack_in_place(df):
    import polars as pl

    df.hstack([pl.Series("n", [7, 8, 9])], in_place=True)


def _pl_vstack_in_place(df):
    import polars as pl

    df.vstack(pl.DataFrame({"a": [4], "b": ["w"]}), in_place=True)


# Every one of these changes the frame it is called on (asserted below), so each would reach
# the cache if the cache and a step ever held the same DataFrame object.
POLARS_IN_PLACE = [
    _pl_set_cell,
    _pl_set_columns,
    _pl_insert_column,
    _pl_replace_column,
    _pl_extend,
    _pl_drop_in_place,
    _pl_rename_columns,
    _pl_hstack_in_place,
    _pl_vstack_in_place,
]


def _same_frame(left, right) -> bool:
    return left.columns == right.columns and left.equals(right)


@pytest.mark.parametrize("mutate", POLARS_IN_PLACE, ids=lambda f: f.__name__)
class TestPolarsIsolation:
    def test_the_mutation_is_in_place(self, mutate):
        # Guards the list itself: an entry that stopped mutating would prove nothing below.
        pytest.importorskip("polars")
        df = _polars_frame()
        mutate(df)
        assert not _same_frame(df, _polars_frame())

    def test_mutating_a_cache_hit_does_not_change_the_cache(self, mutate):
        pytest.importorskip("polars")
        lru = _ArtifactLRU()
        lru.put("/a.parquet", _polars_frame(), "polars")
        hit = lru.get("/a.parquet", "polars")
        mutate(hit)
        assert _same_frame(lru.get("/a.parquet", "polars"), _polars_frame())

    def test_mutating_the_producers_frame_after_put_does_not_change_the_cache(self, mutate):
        pytest.importorskip("polars")
        lru = _ArtifactLRU()
        produced = _polars_frame()
        lru.put("/a.parquet", produced, "polars")
        mutate(produced)
        assert _same_frame(lru.get("/a.parquet", "polars"), _polars_frame())

    def test_one_consumer_does_not_see_anothers_mutation(self, mutate):
        pytest.importorskip("polars")
        lru = _ArtifactLRU()
        lru.put("/a.parquet", _polars_frame(), "polars")
        first = lru.get("/a.parquet", "polars")
        second = lru.get("/a.parquet", "polars")
        mutate(first)
        assert _same_frame(second, _polars_frame())


def test_polars_copy_shares_the_column_buffers():
    """The polars copy is free: both frames read the same memory until one is written to."""
    pl = pytest.importorskip("polars")
    np = pytest.importorskip("numpy")
    df = pl.DataFrame({"a": np.arange(100_000, dtype=np.int64)})
    lru = _ArtifactLRU()
    lru.put("/a.parquet", df, "polars")
    hit = lru.get("/a.parquet", "polars")
    assert hit is not df
    assert np.shares_memory(hit["a"].to_numpy(), df["a"].to_numpy())


def test_polars_lazyframe_is_copied_without_being_collected():
    pl = pytest.importorskip("polars")
    lazy = pl.LazyFrame({"a": [1, 2, 3]})
    copied = _isolated_copy(lazy)
    assert isinstance(copied, pl.LazyFrame)
    assert copied is not lazy
    assert copied.collect()["a"].to_list() == [1, 2, 3]


# ─── Isolation: pandas ────────────────────────────────────────────────────────


def _pandas_frame():
    import numpy as np
    import pandas as pd

    return pd.DataFrame({"a": np.arange(3, dtype=np.int64), "b": [1.5, 2.5, 3.5]})


def _pd_set_cell(df):
    df.iloc[0, 0] = 99


def _pd_set_column(df):
    df["a"] = [7, 8, 9]


def _pd_write_through_numpy(df):
    # The escape hatch a shallow copy does not survive: the array behind a column.
    values = df["a"].to_numpy()
    values.flags.writeable = True
    values[0] = 99


def _pd_drop_in_place(df):
    df.drop(columns=["b"], inplace=True)


PANDAS_IN_PLACE = [_pd_set_cell, _pd_set_column, _pd_write_through_numpy, _pd_drop_in_place]


@pytest.mark.parametrize("mutate", PANDAS_IN_PLACE, ids=lambda f: f.__name__)
class TestPandasIsolation:
    def test_the_mutation_is_in_place(self, mutate):
        pytest.importorskip("pandas")
        df = _pandas_frame()
        mutate(df)
        assert not df.equals(_pandas_frame())

    def test_mutating_a_cache_hit_does_not_change_the_cache(self, mutate):
        pytest.importorskip("pandas")
        lru = _ArtifactLRU()
        lru.put("/a.parquet", _pandas_frame(), "pandas")
        mutate(lru.get("/a.parquet", "pandas"))
        assert lru.get("/a.parquet", "pandas").equals(_pandas_frame())

    def test_mutating_the_producers_frame_after_put_does_not_change_the_cache(self, mutate):
        pytest.importorskip("pandas")
        lru = _ArtifactLRU()
        produced = _pandas_frame()
        lru.put("/a.parquet", produced, "pandas")
        mutate(produced)
        assert lru.get("/a.parquet", "pandas").equals(_pandas_frame())


def test_pandas_copy_does_not_share_memory():
    """pandas stays a real copy: a shallow one shares the numpy blocks (see the numpy case)."""
    pytest.importorskip("pandas")
    np = pytest.importorskip("numpy")
    df = _pandas_frame()
    lru = _ArtifactLRU()
    lru.put("/a.parquet", df, "pandas")
    first = lru.get("/a.parquet", "pandas")
    second = lru.get("/a.parquet", "pandas")
    assert not np.shares_memory(first["a"].to_numpy(), df["a"].to_numpy())
    assert not np.shares_memory(first["a"].to_numpy(), second["a"].to_numpy())


# ─── Isolation: pyarrow ───────────────────────────────────────────────────────


def _arrow_table(tmp_path):
    """A Table as a step receives it: read back from a parquet file."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    path = tmp_path / "t.parquet"
    pq.write_table(pa.table({"a": list(range(100))}), path)
    return pq.read_table(path)


def _arrow_write_through_buffer(table):
    """Overwrite the first value of column `a` through the buffer protocol.

    A Table has no mutating methods, but pyarrow hands out its data buffers and they are
    writable. This is why a Table is copied, not shared.
    """
    import numpy as np

    data = table.column("a").chunk(0).buffers()[1]
    np.frombuffer(data, dtype=np.int64)[0] = 99


class TestArrowIsolation:
    def test_a_table_can_be_written_through_its_buffers(self, tmp_path):
        pytest.importorskip("pyarrow")
        table = _arrow_table(tmp_path)
        _arrow_write_through_buffer(table)
        assert table.column("a")[0].as_py() == 99

    def test_writing_through_a_cache_hit_does_not_change_the_cache(self, tmp_path):
        pytest.importorskip("pyarrow")
        lru = _ArtifactLRU()
        lru.put("/a.parquet", _arrow_table(tmp_path), "pyarrow")
        _arrow_write_through_buffer(lru.get("/a.parquet", "pyarrow"))
        assert lru.get("/a.parquet", "pyarrow").column("a")[0].as_py() == 0

    def test_writing_through_the_producers_table_after_put_does_not_change_the_cache(
        self, tmp_path
    ):
        pytest.importorskip("pyarrow")
        lru = _ArtifactLRU()
        produced = _arrow_table(tmp_path)
        lru.put("/a.parquet", produced, "pyarrow")
        _arrow_write_through_buffer(produced)
        assert lru.get("/a.parquet", "pyarrow").column("a")[0].as_py() == 0


# ─── Isolation: containers of frames ──────────────────────────────────────────


def test_frames_inside_containers_are_isolated(tmp_path):
    pytest.importorskip("polars")
    pytest.importorskip("pandas")
    pytest.importorskip("pyarrow")

    def bundle():
        return {
            "polars": [_polars_frame(), _polars_frame()],
            "pandas": {"inner": _pandas_frame()},
            "arrow": (_arrow_table(tmp_path),),
            "plain": [1, 2, 3],
        }

    def mutate(value):
        _pl_set_cell(value["polars"][1])
        _pd_write_through_numpy(value["pandas"]["inner"])
        _arrow_write_through_buffer(value["arrow"][0])
        value["plain"].append(4)

    def unchanged(value) -> bool:
        return (
            _same_frame(value["polars"][1], _polars_frame())
            and value["pandas"]["inner"].equals(_pandas_frame())
            and value["arrow"][0].column("a")[0].as_py() == 0
            and value["plain"] == [1, 2, 3]
        )

    lru = _ArtifactLRU()
    produced = bundle()
    lru.put("/a.pkl", produced)
    mutate(produced)
    assert unchanged(lru.get("/a.pkl"))
    mutate(lru.get("/a.pkl"))
    assert unchanged(lru.get("/a.pkl"))


# ─── Bounds: which artifacts are cached ───────────────────────────────────────


def _write_json(path: str, approx_bytes: int) -> str:
    """Write a json artifact of roughly `approx_bytes` to a local path or remote URI."""
    serialize({"blob": "x" * approx_bytes}, path, "json")
    return path


OVER = _LRU_MAX_ARTIFACT_BYTES + 1024
SMALL = 1024


class TestAdmission:
    """The rule is the serialized size, and it is the same for local and remote artifacts."""

    @pytest.mark.parametrize("root", ["memory://arts", "local"])
    def test_artifact_over_the_limit_is_read_but_not_held(self, project, root):
        root = str(project) if root == "local" else root
        path = _write_json(f"{root}/n/big.json", OVER)
        lru = _ArtifactLRU()
        value = _load_artifact(path, lru)
        assert len(value["blob"]) == OVER
        assert lru.get(path) is None
        assert len(lru._entries) == 0

    @pytest.mark.parametrize("root", ["memory://arts", "local"])
    def test_artifact_within_the_limit_is_held(self, project, root):
        root = str(project) if root == "local" else root
        path = _write_json(f"{root}/n/small.json", SMALL)
        lru = _ArtifactLRU()
        _load_artifact(path, lru)
        assert lru.get(path) == {"blob": "x" * SMALL}

    def test_artifact_exactly_at_the_limit_is_held(self):
        lru = _ArtifactLRU()
        assert lru.admit("/n/a.json", 1, size_bytes=_LRU_MAX_ARTIFACT_BYTES)
        assert not lru.admit("/n/b.json", 2, size_bytes=_LRU_MAX_ARTIFACT_BYTES + 1)
        assert lru.get("/n/a.json") == 1
        assert lru.get("/n/b.json") is None

    def test_artifact_of_unknown_size_is_not_held(self, project):
        lru = _ArtifactLRU()
        assert not lru.admit(str(project / "missing.json"), {"k": 1})
        assert not lru.admit("memory://arts/n/missing.json", {"k": 1})
        assert len(lru._entries) == 0

    @pytest.mark.parametrize("root", ["memory://arts", "local"])
    def test_fan_in_holds_only_the_artifacts_within_the_limit(self, project, root):
        root = str(project) if root == "local" else root
        big = _write_json(f"{root}/n/big.json", OVER)
        small = _write_json(f"{root}/n/small.json", SMALL)
        lru = _ArtifactLRU()
        values = _load_collected_artifacts(
            [{"path": big, "format": "json"}, {"path": small, "format": "json"}], lru
        )
        assert [len(v["blob"]) for v in values] == [OVER, SMALL]
        assert lru.get(big) is None
        assert lru.get(small) is not None

    @pytest.mark.parametrize("art_dir", ["memory://arts", "local"])
    @pytest.mark.parametrize("size, held", [(OVER, False), (SMALL, True)])
    def test_step_result_is_held_only_within_the_limit(
        self, project, monkeypatch, art_dir, size, held
    ):
        art_dir = str(project / "arts") if art_dir == "local" else art_dir
        lru = _ArtifactLRU()
        path = _run_step(
            project,
            monkeypatch,
            f"""
            def produce():
                return {{"blob": "x" * {size}}}
            """,
            "produce",
            art_dir,
            lru,
        )
        assert _storage.exists(path)
        assert (lru.get(path) is not None) is held


# ─── Bounds: the cache as a whole ─────────────────────────────────────────────


class TestByteBudget:
    def test_least_recent_entries_are_evicted_to_fit_the_byte_limit(self):
        lru = _ArtifactLRU(max_total_bytes=100)
        lru.put("/a", "a", size_bytes=40)
        lru.put("/b", "b", size_bytes=40)
        assert lru.get("/a") == "a"  # touch /a → /b is now least recent
        lru.put("/c", "c", size_bytes=40)
        assert lru.get("/b") is None
        assert lru.get("/a") == "a"
        assert lru.get("/c") == "c"
        assert lru._total_bytes == 80

    def test_one_large_entry_evicts_as_many_as_it_needs(self):
        lru = _ArtifactLRU(max_total_bytes=100)
        for name in ("/a", "/b", "/c"):
            lru.put(name, name, size_bytes=30)
        lru.put("/d", "d", size_bytes=90)
        assert [lru.get(n) for n in ("/a", "/b", "/c")] == [None, None, None]
        assert lru.get("/d") == "d"
        assert lru._total_bytes == 90

    def test_entry_larger_than_the_whole_budget_is_not_kept(self):
        lru = _ArtifactLRU(max_total_bytes=100)
        lru.put("/a", "a", size_bytes=101)
        assert lru.get("/a") is None
        assert lru._total_bytes == 0

    def test_replacing_an_entry_counts_its_size_once(self):
        lru = _ArtifactLRU(max_total_bytes=100)
        lru.put("/a", "old", size_bytes=60)
        lru.put("/a", "new", size_bytes=70)
        assert lru.get("/a") == "new"
        assert lru._total_bytes == 70

    def test_entry_dropped_on_a_failed_copy_frees_its_bytes(self):
        class CopiesOnce:
            copies = 0

            def __deepcopy__(self, memo):
                if type(self).copies:
                    raise RuntimeError("no more copies")
                type(self).copies += 1
                return self

        lru = _ArtifactLRU(max_total_bytes=100)
        lru.put("/a", CopiesOnce(), size_bytes=60)
        assert lru._total_bytes == 60
        assert lru.get("/a") is None
        assert lru._total_bytes == 0

    def test_default_limits_hold_eight_artifacts_of_the_largest_size(self):
        lru = _ArtifactLRU()
        for i in range(12):
            assert lru.admit(f"/n/{i}.json", i, size_bytes=_LRU_MAX_ARTIFACT_BYTES)
        held = [i for i in range(12) if lru.get(f"/n/{i}.json") is not None]
        assert held == [4, 5, 6, 7, 8, 9, 10, 11]
        assert lru._total_bytes == _worker._LRU_MAX_TOTAL_BYTES

    def test_entry_limit_still_applies_to_tiny_artifacts(self):
        lru = _ArtifactLRU()
        for i in range(40):
            lru.admit(f"/n/{i}.json", i, size_bytes=1)
        assert len(lru._entries) == _worker._LRU_MAX_ENTRIES


# ─── Memory: a chain of large steps under a remote store ──────────────────────


CHAIN = """
import numpy as np
import pandas as pd

ROWS = 1_500_000


def link(prev: pd.DataFrame = None) -> pd.DataFrame:
    # Incompressible, numpy-backed: ~12 MB in memory and on disk, over the 8 MB limit.
    rng = np.random.default_rng(0 if prev is None else int(prev["x"].iloc[0] * 1e6))
    return pd.DataFrame({"x": rng.random(ROWS)})
"""


def test_chain_of_large_remote_steps_holds_no_frames(project, monkeypatch, capfd):
    """#232 acceptance: memory for a chain of large steps under a remote store stays bounded.

    One worker runs an 8-step chain; every step reads the previous step's ~12 MB frame from
    memory:// and returns a new one. tracemalloc counts the numpy memory the worker holds, which
    is what the cache's copies of pandas frames are made of, and the bytes of the memory://
    store itself, which are subtracted. Before the fix every result was kept (remote artifacts
    were cached whatever their size) and the worker ended the chain holding a copy of each one.
    """
    pytest.importorskip("pandas")
    pytest.importorskip("pyarrow")
    frame_bytes = 1_500_000 * 8
    steps = 8
    lru = _ArtifactLRU()

    def link(i, prev):
        return _run_step(
            project,
            monkeypatch,
            CHAIN,
            "link",
            "memory://arts",
            lru,
            run_hash=f"h{i}",
            inputs={"prev": prev} if prev else {},
            param_types={"prev": "pandas"} if prev else {},
        )

    # Warm up outside the measurement: the first read and write import the parquet machinery.
    prev = link(1, link(0, None))
    gc.collect()
    tracemalloc.start()
    try:
        baseline, _ = tracemalloc.get_traced_memory()
        stored = 0
        for i in range(2, 2 + steps):
            prev = link(i, prev)
            assert _storage.size(prev) > _LRU_MAX_ARTIFACT_BYTES
            stored += _storage.size(prev)
        gc.collect()
        held, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    capfd.readouterr()  # the steps' protocol lines
    held_frames = (held - baseline - stored) / frame_bytes
    peak_frames = (peak - baseline - stored) / frame_bytes
    print(f"chain memory, in frames: held {held_frames:.2f}, peak {peak_frames:.2f}")

    assert len(lru._entries) == 0
    # Nothing of the chain is left once it has run.
    assert held_frames < 0.25, held_frames
    # At most the running step's input and result were alive at once.
    assert peak_frames < 3, peak_frames
