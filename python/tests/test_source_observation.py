"""Worker comparisons must agree with JSON semantics and reject uncertain evidence."""
import hashlib
import json

from barca._source_observation import observe


def previous(tmp_path, value):
    path = tmp_path / "source.json"
    raw = json.dumps(value).encode()
    path.write_bytes(raw)
    return {"path": str(path), "content_hash": hashlib.sha256(raw).hexdigest()}


def test_observations_preserve_order_and_distinguish_json_types(tmp_path):
    old = previous(tmp_path, {"b": {"z": 1, "a": 2}, "a": 1, "gone": 0, "zero": 0.0})
    observed = observe({"new": 4, "a": True, "b": {"a": 2, "z": 1}, "zero": -0.0}, old)
    assert observed["count"] == 4
    assert observed["changed_order"] == ["new", "a", "zero"]
    assert observed["removed"] == ["gone"]
    assert observed["previous_hash"] == old["content_hash"]
    assert observed["changed"]["a"] is True


def test_missing_or_mismatched_previous_bytes_do_not_produce_a_delta(tmp_path):
    old = previous(tmp_path, {"a": 1})
    (tmp_path / "source.json").write_text('{"a":2}')
    assert "previous_hash" not in observe({"a": 2}, old)
    (tmp_path / "source.json").unlink()
    assert "previous_hash" not in observe({"a": 2}, old)
    assert "previous_hash" not in observe({"a": 2}, None)
