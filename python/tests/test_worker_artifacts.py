"""Integration tests for barca._worker with artifact-based I/O.

These tests exercise the full worker flow: execute a user function,
serialize the result to an artifact file, emit a v2 protocol receipt,
and resolve cross-phase artifact references as inputs.
"""

import json
import textwrap
from pathlib import Path

import pandas as pd
import pytest

from barca import _storage
from barca._artifacts import deserialize


# ─── Helpers ──────────────────────────────────────────────────────────────────


def _make_batch(steps, provided_inputs=None, artifact_dir=None):
    """Build a batch dict matching what Rust sends to the worker."""
    return {
        "stream_id": "test-w0",
        "artifact_dir": str(artifact_dir) if artifact_dir else None,
        "provided_inputs": provided_inputs or {},
        "steps": steps,
    }


def _make_step(
    node_id,
    function_name,
    source_file,
    kind="asset",
    inputs=None,
    partition=None,
    serializer=None,
):
    step = {
        "node_id": node_id,
        "kind": kind,
        "function_name": function_name,
        "source_file": source_file,
        "inputs": inputs or {},
    }
    if partition:
        step["partition"] = partition
    if serializer:
        step["serializer"] = serializer
    return step


def _write_module(tmp_path, filename, code):
    """Write a Python module file and return its path."""
    p = tmp_path / filename
    p.write_text(textwrap.dedent(code))
    return str(p)


def _run_batch_capture_protocol(batch):
    """Run a batch through the worker and capture protocol messages from stderr.

    Returns (protocol_messages: list[dict], error_lines: list[str]).
    """
    import io
    from contextlib import redirect_stderr

    from barca._worker import run_batch

    buf = io.StringIO()
    with redirect_stderr(buf):
        run_batch(batch)

    protocol_msgs = []
    error_lines = []
    for line in buf.getvalue().splitlines():
        if line.startswith("BARCA:2:"):
            json_str = line[len("BARCA:2:") :]
            protocol_msgs.append(json.loads(json_str))
        elif line.strip():
            error_lines.append(line)

    return protocol_msgs, error_lines


# ─── Protocol emission tests ─────────────────────────────────────────────────


class TestProtocolEmission:
    """Worker emits v2 receipt with artifact metadata."""

    def test_dict_emits_artifact_receipt(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_func", "my_func", src)],
            artifact_dir=artifact_dir,
        )
        msgs, errors = _run_batch_capture_protocol(batch)

        assert len(msgs) == 1
        msg = msgs[0]
        assert msg["type"] == "result"
        assert msg["node_id"] == "mod.py:my_func"
        assert "artifact" in msg
        assert "path" in msg["artifact"]
        assert "format" in msg["artifact"]
        assert "size_bytes" in msg["artifact"]
        assert msg["artifact"]["size_bytes"] > 0
        assert "elapsed" in msg

    def test_artifact_file_exists(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_func", "my_func", src)],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)

        artifact_path = Path(msgs[0]["artifact"]["path"])
        assert artifact_path.exists()

    def test_artifact_deserializes_correctly(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1, "y": [2, 3]}
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_func", "my_func", src)],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)

        art = msgs[0]["artifact"]
        result = deserialize(Path(art["path"]), art["format"])
        assert result == {"x": 1, "y": [2, 3]}


# ─── Various output types ────────────────────────────────────────────────────


class TestOutputTypes:
    """Different return types produce correct artifact formats."""

    def _run_func(self, tmp_path, code, func_name="my_func"):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(tmp_path, "mod.py", code)
        batch = _make_batch(
            [_make_step(f"mod.py:{func_name}", func_name, src)],
            artifact_dir=artifact_dir,
        )
        msgs, errors = _run_batch_capture_protocol(batch)
        assert len(msgs) == 1, f"Expected 1 msg, got {len(msgs)}. Errors: {errors}"
        art = msgs[0]["artifact"]
        return art, deserialize(Path(art["path"]), art["format"])

    def test_int(self, tmp_path):
        art, result = self._run_func(
            tmp_path,
            """
            def my_func():
                return 42
        """,
        )
        assert art["format"] == "json"
        assert result == 42

    def test_dict(self, tmp_path):
        art, result = self._run_func(
            tmp_path,
            """
            def my_func():
                return {"key": "value"}
        """,
        )
        assert art["format"] == "json"
        assert result == {"key": "value"}

    def test_list(self, tmp_path):
        art, result = self._run_func(
            tmp_path,
            """
            def my_func():
                return [1, 2, 3]
        """,
        )
        assert art["format"] == "json"
        assert result == [1, 2, 3]

    def test_set(self, tmp_path):
        art, result = self._run_func(
            tmp_path,
            """
            def my_func():
                return {1, 2, 3}
        """,
        )
        assert art["format"] == "pickle"
        assert result == {1, 2, 3}

    def test_custom_object(self, tmp_path):
        art, result = self._run_func(
            tmp_path,
            """
            class Point:
                def __init__(self, x, y):
                    self.x = x
                    self.y = y

            def my_func():
                return Point(3, 4)
        """,
        )
        assert art["format"] == "pickle"
        assert result.x == 3
        assert result.y == 4

    def test_pandas_dataframe(self, tmp_path):
        art, result = self._run_func(
            tmp_path,
            """
            import pandas as pd

            def my_func():
                return pd.DataFrame({"a": [1, 2], "b": [3, 4]})
        """,
        )
        assert art["format"] == "parquet"
        assert list(result.columns) == ["a", "b"]
        assert len(result) == 2

    def test_sensor_unpacking(self, tmp_path):
        """Sensors return (updated, data) — only data gets serialized."""
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_sensor():
                return (True, {"data": 1})
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_sensor", "my_sensor", src, kind="sensor")],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        art = msgs[0]["artifact"]
        result = deserialize(Path(art["path"]), art["format"])
        assert result == {"data": 1}


# ─── Cross-phase input resolution ────────────────────────────────────────────


class TestCrossPhaseInputs:
    """provided_inputs with artifact refs are deserialized for downstream steps."""

    def test_json_artifact_as_input(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()

        # Pre-create an artifact that represents an upstream output.
        from barca._artifacts import serialize as art_serialize

        upstream_path = artifact_dir / "upstream.json"
        art_serialize({"x": 10}, upstream_path, "json")

        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def downstream(data):
                return data["x"] + 1
        """,
        )
        batch = _make_batch(
            [
                _make_step(
                    "mod.py:downstream",
                    "downstream",
                    src,
                    inputs={"data": "mod.py:upstream"},
                )
            ],
            provided_inputs={
                "mod.py:upstream": {
                    "path": str(upstream_path),
                    "format": "json",
                },
            },
            artifact_dir=artifact_dir,
        )
        msgs, errors = _run_batch_capture_protocol(batch)
        assert len(msgs) == 1, f"Errors: {errors}"
        result = deserialize(Path(msgs[0]["artifact"]["path"]), msgs[0]["artifact"]["format"])
        assert result == 11

    def test_parquet_artifact_as_input(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()

        from barca._artifacts import serialize as art_serialize

        upstream_path = artifact_dir / "upstream.parquet"
        df = pd.DataFrame({"val": [10, 20, 30]})
        art_serialize(df, upstream_path, "parquet")

        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def downstream(data):
                return int(data["val"].sum())
        """,
        )
        batch = _make_batch(
            [
                _make_step(
                    "mod.py:downstream",
                    "downstream",
                    src,
                    inputs={"data": "mod.py:upstream"},
                )
            ],
            provided_inputs={
                "mod.py:upstream": {
                    "path": str(upstream_path),
                    "format": "parquet",
                },
            },
            artifact_dir=artifact_dir,
        )
        msgs, errors = _run_batch_capture_protocol(batch)
        assert len(msgs) == 1, f"Errors: {errors}"
        result = deserialize(Path(msgs[0]["artifact"]["path"]), msgs[0]["artifact"]["format"])
        assert result == 60

    def test_pickle_artifact_as_input(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()

        from barca._artifacts import serialize as art_serialize

        upstream_path = artifact_dir / "upstream.pkl"
        art_serialize({1, 2, 3}, upstream_path, "pickle")

        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def downstream(data):
                return len(data)
        """,
        )
        batch = _make_batch(
            [
                _make_step(
                    "mod.py:downstream",
                    "downstream",
                    src,
                    inputs={"data": "mod.py:upstream"},
                )
            ],
            provided_inputs={
                "mod.py:upstream": {
                    "path": str(upstream_path),
                    "format": "pickle",
                },
            },
            artifact_dir=artifact_dir,
        )
        msgs, errors = _run_batch_capture_protocol(batch)
        assert len(msgs) == 1, f"Errors: {errors}"
        result = deserialize(Path(msgs[0]["artifact"]["path"]), msgs[0]["artifact"]["format"])
        assert result == 3


# ─── Serializer override per step ─────────────────────────────────────────────


class TestSerializerOverride:
    """Step-level serializer field forces a specific format."""

    def test_force_pickle_on_dict(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"key": "value"}
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_func", "my_func", src, serializer="pickle")],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["format"] == "pickle"
        result = deserialize(Path(msgs[0]["artifact"]["path"]), "pickle")
        assert result == {"key": "value"}

    def test_no_serializer_uses_autodetect(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"key": "value"}
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_func", "my_func", src)],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["format"] == "json"


# ─── Multi-step batch ─────────────────────────────────────────────────────────


class TestMultiStepBatch:
    """Multiple steps in a single batch — each produces its own artifact."""

    def test_two_steps_same_batch(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def step_a():
                return [1, 2, 3]

            def step_b(data):
                return sum(data)
        """,
        )
        batch = _make_batch(
            [
                _make_step("mod.py:step_a", "step_a", src),
                _make_step(
                    "mod.py:step_b",
                    "step_b",
                    src,
                    inputs={"data": "mod.py:step_a"},
                ),
            ],
            artifact_dir=artifact_dir,
        )
        msgs, errors = _run_batch_capture_protocol(batch)
        assert len(msgs) == 2, f"Errors: {errors}"

        # First step: list
        art_a = msgs[0]["artifact"]
        assert art_a["format"] == "json"
        assert deserialize(Path(art_a["path"]), art_a["format"]) == [1, 2, 3]

        # Second step: int (computed from first)
        art_b = msgs[1]["artifact"]
        assert art_b["format"] == "json"
        assert deserialize(Path(art_b["path"]), art_b["format"]) == 6

    def test_artifacts_have_distinct_paths(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        artifact_dir.mkdir()
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def step_a():
                return 1

            def step_b():
                return 2
        """,
        )
        batch = _make_batch(
            [
                _make_step("mod.py:step_a", "step_a", src),
                _make_step("mod.py:step_b", "step_b", src),
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        paths = [m["artifact"]["path"] for m in msgs]
        assert len(set(paths)) == 2, f"Paths should be distinct: {paths}"


# ─── @sink execution ─────────────────────────────────────────────────────────


@pytest.fixture
def _memory_fs():
    """Clean fsspec memory filesystem around each test."""
    yield
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


def _make_sink_step(node_id, function_name, source_file, sinks, **kwargs):
    step = _make_step(node_id, function_name, source_file, **kwargs)
    step["sinks"] = sinks
    return step


class TestSinkExecution:
    def test_local_sink_written(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        sink_path = str(tmp_path / "exports" / "out.json")
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func", "my_func", src, [{"path": sink_path, "serializer": None}]
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)

        assert msgs[0]["type"] == "result"
        assert deserialize(Path(sink_path), "json") == {"x": 1}
        outcomes = msgs[0]["artifact"]["sinks"]
        assert outcomes == [
            {"path": sink_path, "status": "ok", "size_bytes": Path(sink_path).stat().st_size}
        ]

    def test_stacked_sinks_both_written(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        s1 = str(tmp_path / "out.json")
        s2 = str(tmp_path / "out.pkl")
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func",
                    "my_func",
                    src,
                    [{"path": s1, "serializer": None}, {"path": s2, "serializer": None}],
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert deserialize(Path(s1), "json") == {"x": 1}
        assert deserialize(Path(s2), "pickle") == {"x": 1}
        assert [o["status"] for o in msgs[0]["artifact"]["sinks"]] == ["ok", "ok"]

    def test_serializer_kwarg_overrides_extension(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        # .dat extension, explicit pickle serializer.
        sink_path = str(tmp_path / "out.dat")
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func", "my_func", src, [{"path": sink_path, "serializer": "pickle"}]
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["sinks"][0]["status"] == "ok"
        assert deserialize(Path(sink_path), "pickle") == {"x": 1}

    def test_extension_precedence_over_primary_format(self, tmp_path):
        """Primary artifact is json; .pkl sink extension forces pickle."""
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return [1, 2, 3]
        """,
        )
        sink_path = str(tmp_path / "out.pkl")
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func", "my_func", src, [{"path": sink_path, "serializer": None}]
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["format"] == "json"
        assert deserialize(Path(sink_path), "pickle") == [1, 2, 3]

    def test_parquet_sink_for_dataframe(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            import pandas as pd

            def my_func():
                return pd.DataFrame({"a": [1, 2, 3]})
        """,
        )
        sink_path = str(tmp_path / "exports" / "table.parquet")
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func", "my_func", src, [{"path": sink_path, "serializer": "parquet"}]
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["sinks"][0]["status"] == "ok"
        result = pd.read_parquet(sink_path)
        assert list(result["a"]) == [1, 2, 3]

    def test_sink_failure_fails_the_step(self, tmp_path):
        """A sink with an unusable destination fails the step (0.30.0; it used to be a
        warning while the asset succeeded)."""
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func",
                    "my_func",
                    src,
                    [{"path": "ftp://nowhere/out.json", "serializer": None}],
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)

        assert msgs[0]["type"] == "error"
        assert msgs[0]["error_type"] == "SinkError"
        assert "ftp://nowhere/out.json" in msgs[0]["message"]

    def test_unsupported_serializer_fails_the_step(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func",
                    "my_func",
                    src,
                    [{"path": str(tmp_path / "out.yaml"), "serializer": "yaml"}],
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["type"] == "error"
        assert msgs[0]["error_type"] == "SinkError"
        assert "not supported yet" in msgs[0]["message"]

    def test_memory_uri_sink(self, tmp_path, _memory_fs):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 42}
        """,
        )
        batch = _make_batch(
            [
                _make_sink_step(
                    "mod.py:my_func",
                    "my_func",
                    src,
                    [{"path": "memory://exports/out.json", "serializer": None}],
                )
            ],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["sinks"][0]["status"] == "ok"
        assert deserialize("memory://exports/out.json", "json") == {"x": 42}

    def test_partitioned_sinks_get_suffix(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func(ticker):
                return {"ticker": ticker}
        """,
        )
        sink_path = str(tmp_path / "out.json")
        step = _make_sink_step(
            "mod.py:my_func", "my_func", src, [{"path": sink_path, "serializer": None}]
        )
        step["partition_keys"] = [{"ticker": "AAPL"}, {"ticker": "MSFT"}]
        batch = _make_batch([step], artifact_dir=artifact_dir)
        msgs, _ = _run_batch_capture_protocol(batch)

        assert len(msgs) == 2
        p_aapl = str(tmp_path / "out_ticker_AAPL.json")
        p_msft = str(tmp_path / "out_ticker_MSFT.json")
        assert deserialize(Path(p_aapl), "json") == {"ticker": "AAPL"}
        assert deserialize(Path(p_msft), "json") == {"ticker": "MSFT"}
        assert not Path(sink_path).exists()


# ─── Remote primary artifact store ───────────────────────────────────────────


class TestRemoteArtifactStore:
    def test_batch_writes_to_memory_uri(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_func", "my_func", src)],
            artifact_dir="memory://arts",
        )
        msgs, _ = _run_batch_capture_protocol(batch)

        art = msgs[0]["artifact"]
        assert art["path"].startswith("memory://arts/")
        assert art["format"] == "json"
        assert _storage.exists(art["path"])
        assert deserialize(art["path"], "json") == {"x": 1}

    def test_env_var_used_when_no_artifact_dir(self, tmp_path, monkeypatch, _memory_fs):
        monkeypatch.chdir(tmp_path)
        monkeypatch.setenv("BARCA_ARTIFACT_URI", "memory://envstore")
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return 7
        """,
        )
        batch = _make_batch([_make_step("mod.py:my_func", "my_func", src)])
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["path"].startswith("memory://envstore/")
        assert deserialize(msgs[0]["artifact"]["path"], "json") == 7

    def test_cross_phase_input_from_memory_uri(self, tmp_path, monkeypatch, _memory_fs):
        """A provided_inputs artifact ref with a memory:// path deserializes."""
        monkeypatch.chdir(tmp_path)
        from barca._artifacts import serialize

        serialize({"seed": 10}, "memory://arts/upstream.json", "json")
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def consumer(up):
                return up["seed"] * 2
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:consumer", "consumer", src, inputs={"up": "phase0:up"})],
            provided_inputs={
                "phase0:up": {"path": "memory://arts/upstream.json", "format": "json"}
            },
            artifact_dir="memory://arts",
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert deserialize(msgs[0]["artifact"]["path"], "json") == 20


# ─── Error emission: filtered tracebacks + timeout classification ────────────


class TestErrorEmission:
    def test_user_traceback_strips_barca_frames(self, tmp_path):
        from barca._worker import _user_traceback, load_module

        src = _write_module(
            tmp_path,
            "boom.py",
            """
            def outer():
                return inner()

            def inner():
                return 1 / 0
        """,
        )
        mod = load_module(src)
        try:
            mod.outer()
        except ZeroDivisionError as exc:
            tb = _user_traceback(exc)
        assert "boom.py" in tb
        assert "inner" in tb
        assert "_worker.py" not in tb

    def test_user_traceback_empty_when_all_frames_internal(self):
        from barca._worker import _user_traceback

        # A TypeError raised by calling the fn itself — every frame is barca's.
        def takes_none():
            return 1

        try:
            from barca._worker import _execute

            _execute(takes_none, {"unexpected": 1}, {})
        except TypeError as exc:
            tb = _user_traceback(exc)
        assert "_worker.py" not in tb

    def test_batch_error_receipt_has_filtered_traceback(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def explode():
                raise ValueError("kaboom")
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:explode", "explode", src)],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["type"] == "error"
        assert msgs[0]["error_type"] == "ValueError"
        assert msgs[0]["message"] == "kaboom"
        assert "mod.py" in msgs[0]["traceback"]
        assert "_worker.py" not in msgs[0]["traceback"]

    def test_timeout_emits_timeout_error(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            import time

            def hung():
                time.sleep(5)
        """,
        )
        step = _make_step("mod.py:hung", "hung", src)
        step["timeout_seconds"] = 1
        batch = _make_batch([step], artifact_dir=artifact_dir)
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["type"] == "error"
        assert msgs[0]["error_type"] == "TimeoutError"
        assert "timeout" in msgs[0]["message"].lower()


# ─── Content-addressed artifacts (run_hash in the step) ──────────────────────


class TestContentAddressedArtifacts:
    def test_step_with_run_hash_lands_content_addressed(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        step = _make_step("mod.py:my_func", "my_func", src)
        step["run_hash"] = "deadbeef01"
        batch = _make_batch([step], artifact_dir=artifact_dir)
        msgs, _ = _run_batch_capture_protocol(batch)

        path = msgs[0]["artifact"]["path"]
        assert path.endswith("mod.py--my_func/deadbeef01.json")
        assert deserialize(Path(path), "json") == {"x": 1}

    def test_step_without_run_hash_uses_legacy_layout(self, tmp_path):
        artifact_dir = tmp_path / "artifacts"
        src = _write_module(
            tmp_path,
            "mod.py",
            """
            def my_func():
                return {"x": 1}
        """,
        )
        batch = _make_batch(
            [_make_step("mod.py:my_func", "my_func", src)],
            artifact_dir=artifact_dir,
        )
        msgs, _ = _run_batch_capture_protocol(batch)
        assert msgs[0]["artifact"]["path"].endswith("mod.py--my_func.json")
