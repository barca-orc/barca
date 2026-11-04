"""Tests for barca._artifacts — format detection, round-trip serialization, path sanitization."""

import datetime

import pytest

from barca import _storage
from barca._artifacts import (
    artifact_path,
    deserialize,
    detect_format,
    resolve_format,
    safe_node_id,
    serialize,
)

pd = pytest.importorskip("pandas")
pl = pytest.importorskip("polars")


class _Point:
    """Module-level class for pickle round-trip tests (local classes can't be pickled)."""

    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __eq__(self, other):
        return isinstance(other, _Point) and self.x == other.x and self.y == other.y


# ─── Format detection ────────────────────────────────────────────────────────


class TestDetectFormat:
    """detect_format(value, explicit=None) → 'json' | 'pickle' | 'parquet'"""

    # --- JSON-serializable primitives ---

    def test_int(self):
        assert detect_format(42) == "json"

    def test_float(self):
        assert detect_format(3.14) == "json"

    def test_bool(self):
        assert detect_format(True) == "json"

    def test_none(self):
        assert detect_format(None) == "json"

    def test_str(self):
        assert detect_format("hello") == "json"

    # --- JSON-serializable containers ---

    def test_simple_dict(self):
        assert detect_format({"key": "value"}) == "json"

    def test_nested_dict(self):
        assert detect_format({"a": {"b": [1, 2, 3]}}) == "json"

    def test_list_of_primitives(self):
        assert detect_format([1, 2, 3]) == "json"

    def test_list_of_dicts(self):
        assert detect_format([{"x": 1}, {"x": 2}]) == "json"

    def test_empty_dict(self):
        assert detect_format({}) == "json"

    def test_empty_list(self):
        assert detect_format([]) == "json"

    # --- Non-JSON-serializable → pickle ---

    def test_set(self):
        assert detect_format({1, 2, 3}) == "pickle"

    def test_bytes(self):
        assert detect_format(b"binary data") == "pickle"

    def test_datetime(self):
        assert detect_format(datetime.datetime(2025, 1, 1)) == "pickle"

    def test_custom_class(self):
        class Foo:
            pass

        assert detect_format(Foo()) == "pickle"

    def test_tuple(self):
        # Tuples are JSON-serializable (as arrays) in json.dumps,
        # but round-trip as lists. If we detect json, that's fine.
        # If pickle, that's also fine. The key is it doesn't crash.
        fmt = detect_format((1, 2, 3))
        assert fmt in ("json", "pickle")

    def test_dict_with_non_string_keys(self):
        # json.dumps accepts int keys (converts to strings), so this is JSON-serializable.
        # However, round-trip is lossy (int keys become string keys).
        # detect_format checks json.dumps succeeds, so this is "json".
        assert detect_format({1: "a", 2: "b"}) == "json"

    def test_dict_with_tuple_keys(self):
        assert detect_format({(1, 2): "a"}) == "pickle"

    # --- DataFrames → parquet ---

    def test_pandas_dataframe(self):
        df = pd.DataFrame({"x": [1, 2, 3]})
        assert detect_format(df) == "parquet"

    def test_polars_dataframe(self):
        df = pl.DataFrame({"x": [1, 2, 3]})
        assert detect_format(df) == "parquet"

    def test_polars_lazyframe(self):
        lf = pl.LazyFrame({"x": [1, 2, 3]})
        assert detect_format(lf) == "parquet"

    def test_duckdb_relation(self):
        duckdb = pytest.importorskip("duckdb")
        assert detect_format(duckdb.sql("select 1 as x")) == "parquet"

    def test_pyarrow_table(self):
        pa = pytest.importorskip("pyarrow")
        assert detect_format(pa.table({"x": [1, 2, 3]})) == "parquet"

    def test_pandas_series(self):
        # Series are not DataFrames — should go to pickle
        s = pd.Series([1, 2, 3])
        assert detect_format(s) == "pickle"

    # --- Explicit override wins ---

    def test_override_pickle_on_dict(self):
        assert detect_format({"key": "value"}, explicit="pickle") == "pickle"

    def test_override_json_on_dataframe(self):
        df = pd.DataFrame({"x": [1, 2, 3]})
        assert detect_format(df, explicit="json") == "json"

    def test_override_parquet_on_dict(self):
        assert detect_format({"key": "value"}, explicit="parquet") == "parquet"

    def test_override_pickle_on_int(self):
        assert detect_format(42, explicit="pickle") == "pickle"


# ─── Round-trip serialization ─────────────────────────────────────────────────


class TestRoundTripJSON:
    """serialize → deserialize with JSON format."""

    def test_dict(self, tmp_path):
        value = {"key": "value", "nested": {"a": 1}}
        path = tmp_path / "out.json"
        size = serialize(value, path, "json")
        assert size > 0
        assert path.exists()
        assert deserialize(path, "json") == value

    def test_list_of_mixed_primitives(self, tmp_path):
        value = [1, "two", 3.0, True, None]
        path = tmp_path / "out.json"
        serialize(value, path, "json")
        assert deserialize(path, "json") == value

    def test_nested_dict_with_lists(self, tmp_path):
        value = {
            "users": [
                {"name": "alice", "scores": [10, 20]},
                {"name": "bob", "scores": [30]},
            ]
        }
        path = tmp_path / "out.json"
        serialize(value, path, "json")
        assert deserialize(path, "json") == value

    def test_primitive_int(self, tmp_path):
        path = tmp_path / "out.json"
        serialize(42, path, "json")
        assert deserialize(path, "json") == 42

    def test_primitive_string(self, tmp_path):
        path = tmp_path / "out.json"
        serialize("hello", path, "json")
        assert deserialize(path, "json") == "hello"

    def test_primitive_bool(self, tmp_path):
        path = tmp_path / "out.json"
        serialize(True, path, "json")
        assert deserialize(path, "json") is True

    def test_primitive_none(self, tmp_path):
        path = tmp_path / "out.json"
        serialize(None, path, "json")
        assert deserialize(path, "json") is None


class TestRoundTripPickle:
    """serialize → deserialize with pickle format."""

    def test_set(self, tmp_path):
        value = {1, 2, 3}
        path = tmp_path / "out.pkl"
        size = serialize(value, path, "pickle")
        assert size > 0
        assert deserialize(path, "pickle") == value

    def test_datetime(self, tmp_path):
        value = datetime.datetime(2025, 6, 15, 12, 30, 0)
        path = tmp_path / "out.pkl"
        serialize(value, path, "pickle")
        assert deserialize(path, "pickle") == value

    def test_custom_class(self, tmp_path):
        value = _Point(3, 4)
        path = tmp_path / "out.pkl"
        serialize(value, path, "pickle")
        result = deserialize(path, "pickle")
        assert result == value

    def test_bytes(self, tmp_path):
        value = b"\x00\x01\x02\xff"
        path = tmp_path / "out.pkl"
        serialize(value, path, "pickle")
        assert deserialize(path, "pickle") == value

    def test_dict_with_non_string_keys(self, tmp_path):
        value = {1: "a", (2, 3): "b"}
        path = tmp_path / "out.pkl"
        serialize(value, path, "pickle")
        assert deserialize(path, "pickle") == value

    def test_nested_complex_structure(self, tmp_path):
        value = {"data": {frozenset([1, 2]): [datetime.date(2025, 1, 1)]}}
        path = tmp_path / "out.pkl"
        serialize(value, path, "pickle")
        assert deserialize(path, "pickle") == value


class TestRoundTripParquet:
    """serialize → deserialize with parquet format."""

    def test_pandas_dataframe(self, tmp_path):
        df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
        path = tmp_path / "out.parquet"
        size = serialize(df, path, "parquet")
        assert size > 0
        result = deserialize(path, "parquet")
        pd.testing.assert_frame_equal(result, df)

    def test_polars_dataframe_default_reader_is_pandas(self, tmp_path):
        df = pl.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
        path = tmp_path / "out.parquet"
        serialize(df, path, "parquet")
        result = deserialize(path, "parquet")
        expected_pd = df.to_pandas()
        pd.testing.assert_frame_equal(result, expected_pd)

    def test_polars_dataframe_with_frame_type(self, tmp_path):
        df = pl.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
        path = tmp_path / "out.parquet"
        serialize(df, path, "parquet")
        result = deserialize(path, "parquet", frame_type="polars")
        assert isinstance(result, pl.DataFrame)
        assert result.equals(df)

    def test_pandas_multiple_dtypes(self, tmp_path):
        df = pd.DataFrame(
            {
                "int_col": [1, 2, 3],
                "float_col": [1.1, 2.2, 3.3],
                "str_col": ["a", "b", "c"],
                "bool_col": [True, False, True],
                "dt_col": pd.to_datetime(["2025-01-01", "2025-01-02", "2025-01-03"]),
            }
        )
        path = tmp_path / "out.parquet"
        serialize(df, path, "parquet")
        result = deserialize(path, "parquet")
        pd.testing.assert_frame_equal(result, df)

    def test_duckdb_relation(self, tmp_path):
        duckdb = pytest.importorskip("duckdb")
        rel = duckdb.sql("select * from (values (1, 'a'), (2, 'b')) t(n, s)")
        path = tmp_path / "out.parquet"
        serialize(rel, path, "parquet")
        # Default reader is pandas...
        df = deserialize(path, "parquet")
        assert df["n"].tolist() == [1, 2]
        # ...and frame_type="duckdb" hands back a relation over the parquet file.
        back = deserialize(path, "parquet", frame_type="duckdb")
        assert type(back).__name__ == "DuckDBPyRelation"
        assert back.fetchall() == [(1, "a"), (2, "b")]

    def test_pyarrow_table(self, tmp_path):
        pa = pytest.importorskip("pyarrow")
        table = pa.table({"a": [1, 2, 3], "b": ["x", "y", "z"]})
        path = tmp_path / "out.parquet"
        serialize(table, path, "parquet")
        back = deserialize(path, "parquet", frame_type="pyarrow")
        assert back.equals(table)

    def test_polars_lazyframe(self, tmp_path):
        lf = pl.LazyFrame({"a": [1, 2, 3], "b": [4.0, 5.0, 6.0]})
        path = tmp_path / "out.parquet"
        serialize(lf, path, "parquet")
        result = deserialize(path, "parquet")
        # LazyFrames must be collected before writing; result may come back as DataFrame
        assert result is not None

    def test_empty_pandas_dataframe(self, tmp_path):
        df = pd.DataFrame({"a": pd.Series([], dtype="int64"), "b": pd.Series([], dtype="str")})
        path = tmp_path / "out.parquet"
        serialize(df, path, "parquet")
        result = deserialize(path, "parquet")
        assert len(result) == 0
        assert list(result.columns) == ["a", "b"]

    def test_large_pandas_dataframe(self, tmp_path):
        df = pd.DataFrame({"value": range(10_000)})
        path = tmp_path / "out.parquet"
        serialize(df, path, "parquet")
        result = deserialize(path, "parquet")
        pd.testing.assert_frame_equal(result, df)


# ─── Path sanitization ───────────────────────────────────────────────────────


class TestSafeNodeId:
    """safe_node_id(node_id) → filesystem-safe string."""

    def test_simple(self):
        result = safe_node_id("file.py:func")
        assert "/" not in result
        assert ":" not in result
        assert len(result) > 0

    def test_with_partition(self):
        result = safe_node_id("file.py:func[ticker=AAPL]")
        assert "[" not in result
        assert "]" not in result
        assert "=" not in result

    def test_with_multi_dim_partition(self):
        result = safe_node_id("dir/file.py:func[a=1,b=2]")
        assert "/" not in result
        assert "[" not in result
        assert "," not in result

    def test_no_collisions(self):
        ids = [
            "file.py:func",
            "file.py:func2",
            "file.py:func[t=A]",
            "file.py:func[t=B]",
            "other.py:func",
        ]
        safe_ids = [safe_node_id(nid) for nid in ids]
        assert len(set(safe_ids)) == len(ids), f"Collision detected: {safe_ids}"

    def test_deterministic(self):
        nid = "file.py:func[ticker=AAPL]"
        assert safe_node_id(nid) == safe_node_id(nid)


class TestArtifactPath:
    """artifact_path(artifact_dir, node_id, fmt) → Path."""

    def test_json_extension(self, tmp_path):
        p = artifact_path(tmp_path, "file.py:func", "json")
        assert p.suffix == ".json"
        assert p.parent == tmp_path

    def test_pickle_extension(self, tmp_path):
        p = artifact_path(tmp_path, "file.py:func", "pickle")
        assert p.suffix == ".pkl"
        assert p.parent == tmp_path

    def test_parquet_extension(self, tmp_path):
        p = artifact_path(tmp_path, "file.py:func", "parquet")
        assert p.suffix == ".parquet"
        assert p.parent == tmp_path

    def test_uses_safe_node_id(self, tmp_path):
        p = artifact_path(tmp_path, "file.py:func[t=X]", "json")
        # Should not contain raw special chars in the filename
        assert ":" not in p.stem
        assert "[" not in p.stem


# ─── End-to-end: detect + serialize + deserialize ────────────────────────────


class TestEndToEnd:
    """Full flow: detect_format → serialize → deserialize for various types."""

    def _round_trip(self, value, tmp_path, explicit=None):
        fmt = detect_format(value, explicit=explicit)
        path = artifact_path(tmp_path, "test.py:func", fmt)
        serialize(value, path, fmt)
        return deserialize(path, fmt), fmt

    def test_dict_auto(self, tmp_path):
        value = {"key": "value"}
        result, fmt = self._round_trip(value, tmp_path)
        assert fmt == "json"
        assert result == value

    def test_set_auto(self, tmp_path):
        value = {1, 2, 3}
        result, fmt = self._round_trip(value, tmp_path)
        assert fmt == "pickle"
        assert result == value

    def test_pandas_auto(self, tmp_path):
        df = pd.DataFrame({"x": [1, 2]})
        result, fmt = self._round_trip(df, tmp_path)
        assert fmt == "parquet"
        pd.testing.assert_frame_equal(result, df)

    def test_override_forces_format(self, tmp_path):
        value = {"key": "value"}
        result, fmt = self._round_trip(value, tmp_path, explicit="pickle")
        assert fmt == "pickle"
        assert result == value


# ─── resolve_format (parquet → pickle downgrade) ─────────────────────────────


class TestResolveFormat:
    def test_dataframe_stays_parquet(self):
        assert resolve_format(pd.DataFrame({"x": [1]}), "parquet") == "parquet"

    def test_polars_stays_parquet(self):
        assert resolve_format(pl.DataFrame({"x": [1]}), "parquet") == "parquet"

    def test_duckdb_relation_stays_parquet(self):
        duckdb = pytest.importorskip("duckdb")
        assert resolve_format(duckdb.sql("select 1 as x"), "parquet") == "parquet"

    def test_pyarrow_table_stays_parquet(self):
        pa = pytest.importorskip("pyarrow")
        assert resolve_format(pa.table({"x": [1]}), "parquet") == "parquet"

    def test_non_dataframe_downgrades_to_pickle(self, capsys):
        assert resolve_format({"not": "a df"}, "parquet") == "pickle"
        assert "falling back to pickle" in capsys.readouterr().err

    def test_non_parquet_untouched(self):
        assert resolve_format({"x": 1}, "json") == "json"
        assert resolve_format({1, 2}, "pickle") == "pickle"

    def test_downgrade_path_and_bytes_agree(self, tmp_path):
        """Regression: the old fallback wrote .pkl bytes at a .parquet receipt path."""
        value = {"not": "a df"}
        fmt = resolve_format(value, "parquet")
        path = artifact_path(tmp_path, "f.py:node", fmt)
        serialize(value, path, fmt)
        assert str(path).endswith(".pkl")
        assert path.exists()
        assert deserialize(path, fmt) == value

    def test_serialize_parquet_of_non_dataframe_raises(self, tmp_path):
        with pytest.raises(TypeError, match="parquet format requires a DataFrame"):
            serialize({"not": "a df"}, tmp_path / "out.parquet", "parquet")
        assert not (tmp_path / "out.parquet").exists()
        assert not list(tmp_path.glob("*.tmp"))


# ─── Staged writes ───────────────────────────────────────────────────────────


class _ExplodesOnPickle:
    def __getstate__(self):
        raise RuntimeError("boom mid-serialize")


@pytest.fixture
def _memory_fs():
    """Clean fsspec memory filesystem around each test."""
    yield
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


class TestStagedWrites:
    def test_local_failure_leaves_no_dest_and_no_temp(self, tmp_path):
        dest = tmp_path / "out.pkl"
        with pytest.raises(RuntimeError, match="boom"):
            serialize(_ExplodesOnPickle(), dest, "pickle")
        assert not dest.exists()
        assert list(tmp_path.iterdir()) == []

    def test_local_failure_does_not_clobber_previous_artifact(self, tmp_path):
        dest = tmp_path / "out.pkl"
        serialize({1, 2}, dest, "pickle")
        with pytest.raises(RuntimeError):
            serialize(_ExplodesOnPickle(), dest, "pickle")
        assert deserialize(dest, "pickle") == {1, 2}

    def test_remote_failure_leaves_no_object(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        dest = "memory://arts/out.pkl"
        with pytest.raises(RuntimeError, match="boom"):
            serialize(_ExplodesOnPickle(), dest, "pickle")
        assert _storage.exists(dest) is False
        staging = tmp_path / ".barca" / "staging"
        assert not staging.exists() or list(staging.glob("*.tmp")) == []

    def test_remote_write_cleans_staging(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        size = serialize({"a": 1}, "memory://arts/out.json", "json")
        assert size > 0
        assert list((tmp_path / ".barca" / "staging").glob("*.tmp")) == []


class TestRemoteRoundTrip:
    def test_json_via_memory(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        value = {"users": [1, 2, 3]}
        serialize(value, "memory://arts/x.json", "json")
        assert deserialize("memory://arts/x.json", "json") == value

    def test_pickle_via_memory(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        value = {1, 2, 3}
        serialize(value, "memory://arts/x.pkl", "pickle")
        assert deserialize("memory://arts/x.pkl", "pickle") == value

    def test_parquet_via_memory(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        df = pd.DataFrame({"a": [1, 2], "b": ["x", "y"]})
        size = serialize(df, "memory://arts/x.parquet", "parquet")
        assert size == _storage.size("memory://arts/x.parquet")
        result = deserialize("memory://arts/x.parquet", "parquet")
        pd.testing.assert_frame_equal(result, df)

    def test_artifact_path_remote_is_uri(self):
        p = artifact_path("memory://arts", "f.py:node", "parquet")
        assert isinstance(p, str)
        assert p == "memory://arts/f.py--node.parquet"

    def test_deserialize_cleans_staging(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        serialize([1, 2], "memory://arts/x.json", "json")
        assert deserialize("memory://arts/x.json", "json") == [1, 2]
        assert list((tmp_path / ".barca" / "staging").glob("*.tmp")) == []


# ─── Content-addressed layout ────────────────────────────────────────────────


class TestContentAddressedPaths:
    def test_run_hash_layout_local(self, tmp_path):
        p = artifact_path(tmp_path, "f.py:node", "parquet", run_hash="3fa9c2e1")
        assert p == tmp_path / "f.py--node" / "3fa9c2e1.parquet"

    def test_run_hash_layout_remote(self):
        p = artifact_path("memory://arts", "f.py:node[t=A]", "json", run_hash="beef")
        assert p == "memory://arts/f.py--node_t_A/beef.json"

    def test_no_run_hash_is_legacy_layout(self, tmp_path):
        p = artifact_path(tmp_path, "f.py:node", "json")
        assert p == tmp_path / "f.py--node.json"

    def test_none_run_hash_is_legacy_layout(self, tmp_path):
        p = artifact_path(tmp_path, "f.py:node", "json", run_hash=None)
        assert p == tmp_path / "f.py--node.json"

    def test_serialize_creates_subdir(self, tmp_path):
        p = artifact_path(tmp_path, "f.py:node", "json", run_hash="cafe01")
        serialize({"x": 1}, p, "json")
        assert deserialize(p, "json") == {"x": 1}


class TestFrameLikeOutputsMaterializeAsParquet:
    """detect → resolve → path → write, the way the worker materializes a step result."""

    def _materialize(self, value, tmp_path):
        fmt = resolve_format(value, detect_format(value))
        path = artifact_path(tmp_path, "f.py:node", fmt)
        serialize(value, path, fmt)
        return fmt, path

    def test_duckdb_relation_writes_parquet_without_serializer(self, tmp_path):
        duckdb = pytest.importorskip("duckdb")
        fmt, path = self._materialize(duckdb.sql("select 42 as answer"), tmp_path)
        assert fmt == "parquet"
        assert str(path).endswith(".parquet")
        assert deserialize(path, fmt)["answer"].tolist() == [42]

    def test_pyarrow_table_writes_parquet_without_serializer(self, tmp_path):
        pa = pytest.importorskip("pyarrow")
        fmt, path = self._materialize(pa.table({"x": [1, 2]}), tmp_path)
        assert fmt == "parquet"
        assert str(path).endswith(".parquet")
        assert deserialize(path, fmt, frame_type="pyarrow").num_rows == 2
