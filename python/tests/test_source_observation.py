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


def test_large_changed_values_stay_in_artifacts_instead_of_protocol_frames(tmp_path):
    old = previous(tmp_path, {"a": "before"})
    for value in ("x" * 1_000_000, "\U0001f600" * 24_000):
        observed = observe({"a": value}, old)
        assert observed["count"] == 1
        assert "previous_hash" not in observed
        assert len(json.dumps(observed).encode()) < 200


def test_large_removed_key_sets_use_full_validation_instead_of_large_frames(tmp_path):
    old = previous(tmp_path, {f"key-{i:06d}": 1 for i in range(20_000)})
    observed = observe({}, old)
    assert observed["count"] == 0
    assert "previous_hash" not in observed
