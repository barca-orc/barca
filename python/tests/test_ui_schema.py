"""On-demand UI schemas: real artifacts across JSON, Parquet and pickle.

CLI shapes remain unchanged unless the inspector's fields option is requested.
"""

import datetime as dt
import json
import pickle
from decimal import Decimal

import pyarrow as pa
import pyarrow.parquet as pq
import pytest
from barca import _inspect, _storage


@pytest.mark.parametrize(
    "value, expected",
    [
        (None, "null"),
        (True, "bool"),
        (False, "bool"),
        (12, "int"),
        (2**70, "int"),
        (1.25, "float"),
        ("", "str"),
        ("雪", "str"),
        ([], "list"),
        ([1, None, "a"], "list"),
        ({}, "dict"),
        ({"nested": {"x": 1}}, "dict"),
    ],
)
def test_json_root_and_field_types(tmp_path, value, expected):
    path = tmp_path / "root.json"
    path.write_text(json.dumps(value))
    shape = _inspect.shape(str(path), "json", fields=True)
    assert shape["type"] == expected
    path.write_text(json.dumps({"value": value}))
    shape = _inspect.shape(str(path), "json", fields=True)
    assert shape["columns"] == [{"name": "value", "type": expected}]
    assert "sample" not in shape


def test_json_mixed_values_nullable_rows_and_empty_values(tmp_path):
    path = tmp_path / "rows.json"
    path.write_text(json.dumps([{"value": 1}, {"value": None}, {"value": "a"}, {"other": True}]))
    got = _inspect.shape(str(path), "json", fields=True)
    assert got["columns"] == [
        {"name": "value", "type": "int | str | null"},
        {"name": "other", "type": "bool"},
    ]
    path.write_text(json.dumps([1, "a", None, False, [], {}]))
    got = _inspect.shape(str(path), "json", fields=True)
    assert got["item_types"] == ["bool", "dict", "int", "list", "null", "str"]
    assert "columns" not in got
    for value in [[], {}]:
        path.write_text(json.dumps(value))
        got = _inspect.shape(str(path), "json", fields=True)
        assert not got.get("columns")


def test_json_field_limit_is_disclosed_and_cli_shape_is_unchanged(tmp_path):
    path = tmp_path / "config.json"
    value = {f"key_{i}": i for i in range(103)}
    path.write_text(json.dumps(value))
    shape = _inspect.shape(str(path), "json", fields=True)
    assert len(shape["columns"]) == 100
    assert shape["key_count"] == 103
    assert "first 100 of 103" in shape["note"]
    assert "columns" not in _inspect.shape(str(path), "json")


PARQUET_TYPES = [
    (pa.bool_(), True),
    *[
        (t(), 1)
        for t in [pa.int8, pa.int16, pa.int32, pa.int64, pa.uint8, pa.uint16, pa.uint32, pa.uint64]
    ],
    *[(t(), 1.25) for t in [pa.float16, pa.float32, pa.float64]],
    (pa.string(), "雪"),
    (pa.large_string(), "a"),
    (pa.binary(), b"abc"),
    (pa.large_binary(), b"abc"),
    (pa.binary(3), b"abc"),
    (pa.decimal128(12, 2), Decimal("12.34")),
    (pa.decimal256(40, 4), Decimal("12.3400")),
    (pa.date32(), dt.date(2026, 10, 7)),
    (pa.timestamp("us", tz="UTC"), dt.datetime(2026, 10, 7, tzinfo=dt.UTC)),
    (pa.timestamp("ns"), dt.datetime(2026, 10, 7)),  # noqa: DTZ001 — test a naive timestamp
    (pa.time32("ms"), dt.time(12, 30)),
    (pa.time64("us"), dt.time(12, 30)),
    (pa.duration("us"), dt.timedelta(seconds=12)),
    (pa.list_(pa.field("element", pa.int64())), [1, 2]),
    (pa.large_list(pa.field("element", pa.string())), ["a"]),
    (pa.list_(pa.field("element", pa.int64()), 2), [1, 2]),
    (pa.struct([("count", pa.int64()), ("label", pa.string())]), {"count": 1, "label": "a"}),
    (pa.map_(pa.string(), pa.int64()), [("a", 1)]),
    (pa.dictionary(pa.int32(), pa.string()), "a"),
    (pa.null(), None),
]


@pytest.mark.parametrize("dtype, value", PARQUET_TYPES, ids=[str(t) for t, _ in PARQUET_TYPES])
def test_parquet_types_and_empty_typed_tables(tmp_path, dtype, value):
    path = tmp_path / "table.parquet"
    for values in [[value, None], []]:
        table = pa.table({"value": pa.array(values, type=dtype)})
        pq.write_table(table, path)
        got = _inspect.shape(str(path), "parquet", fields=True)
        assert "note" not in got, got
        assert got["rows"] == len(values)
        # The schema belongs to the stored artifact, not the in-memory object.
        # Map field names may be canonicalized during Parquet serialization.
        stored_type = str(pq.ParquetFile(path).schema_arrow.field("value").type)
        assert got["columns"] == [{"name": "value", "type": stored_type}]
        assert "sample" not in got


@pytest.mark.parametrize(
    "value, expected",
    [
        ({1, 2}, "set"),
        ((1, "a"), "tuple"),
        (b"abc", "bytes"),
        (frozenset({1}), "frozenset"),
        (complex(1, 2), "complex"),
    ],
)
def test_pickle_top_level_types(tmp_path, value, expected):
    path = tmp_path / "value.pkl"
    for protocol in [2, 4, 5]:
        path.write_bytes(pickle.dumps(value, protocol=protocol))
        assert _inspect.shape(str(path), "pickle", fields=True) == {"type": expected}


def test_pickle_never_executes_payload(tmp_path):
    marker = tmp_path / "executed"

    class Payload:
        def __reduce__(self):
            return eval, (f'__import__("pathlib").Path({str(marker)!r}).touch()',)

    path = tmp_path / "payload.pkl"
    path.write_bytes(pickle.dumps(Payload()))
    assert "type" in _inspect.shape(str(path), "pickle", fields=True)
    assert not marker.exists()


@pytest.mark.parametrize("fmt", ["json", "parquet", "pickle"])
def test_missing_and_corrupt_artifacts(tmp_path, fmt):
    path = tmp_path / "missing"
    assert _inspect.shape(str(path), fmt, fields=True)["note"] == "artifact file not found"
    path.write_bytes(b"not a valid artifact")
    assert "could not read artifact" in _inspect.shape(str(path), fmt, fields=True)["note"]


def test_missing_pyarrow_and_unknown_format(tmp_path, monkeypatch):
    path = tmp_path / "table.parquet"
    pq.write_table(pa.table({"id": [1]}), path)
    monkeypatch.setitem(__import__("sys").modules, "pyarrow.parquet", None)
    assert "pyarrow is not installed" in _inspect.shape(str(path), "parquet", fields=True)["note"]
    assert "unknown format" in _inspect.shape(str(path), "unknown", fields=True)["note"]


def test_remote_json_fields_and_batch_protocol(tmp_path):
    uri = "memory://ui-schema-test/config.json"
    fs = _storage.get_fs(uri)
    fs.pipe_file(uri, b'{"ready":true,"count":2,"items":[1]}')
    try:
        got = _inspect.shapes([{"path": uri, "format": "json"}], fields=True)[0]
        assert got["columns"] == [
            {"name": "ready", "type": "bool"},
            {"name": "count", "type": "int"},
            {"name": "items", "type": "list"},
        ]
    finally:
        fs.rm(uri)
