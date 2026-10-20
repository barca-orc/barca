"""CSV boundary conformance: specs/artifact-formats.md."""

import hashlib
import json
import os
import subprocess
from pathlib import Path

import duckdb
import pandas as pd
import polars as pl
import pyarrow as pa
import pytest
from barca import _inspect, _storage
from barca._artifacts import deserialize, serialize_hashed
from barca._worker import _write_sinks
from barca.api import _find_binary


@pytest.mark.parametrize("writer", ["pandas", "polars", "polars_lazy", "pyarrow", "duckdb"])
@pytest.mark.parametrize("reader", ["pandas", "polars", "polars_lazy", "pyarrow", "duckdb"])
def test_csv_writers_and_readers(tmp_path, writer, reader):
    data = {"id": [1, 2], "label": ["comma,value", 'quote"\nnewline']}
    frame = pd.DataFrame(data, index=[8, 9])
    values = {
        "pandas": frame,
        "polars": pl.DataFrame(data),
        "polars_lazy": pl.DataFrame(data).lazy(),
        "pyarrow": pa.table(data),
        "duckdb": duckdb.from_arrow(pa.table(data)),
    }
    path = tmp_path / "frame.csv"
    size, digest = serialize_hashed(values[writer], path, "csv")
    assert size == path.stat().st_size
    assert digest == hashlib.sha256(path.read_bytes()).hexdigest()
    loaded = deserialize(path, "csv", frame_type=reader)
    if reader == "pandas":
        result = loaded.to_dict("list")
    elif reader == "polars":
        result = loaded.to_dict(as_series=False)
    elif reader == "polars_lazy":
        result = loaded.collect().to_dict(as_series=False)
    elif reader == "pyarrow":
        result = loaded.to_pydict()
    else:
        result = loaded.to_arrow_table().to_pydict()
    assert result == data
    assert _inspect.shape(str(path), "csv", sample=1) == {
        "type": "table",
        "rows": 2,
        "columns": [{"name": "id", "type": "string"}, {"name": "label", "type": "string"}],
        "sample": [{"id": "1", "label": "comma,value"}],
    }


@pytest.mark.parametrize("reader", ["polars_lazy", "duckdb"])
def test_remote_lazy_csv_survives_download_cleanup(tmp_path, reader):
    path = "memory://csv-conformance/frame.csv"
    serialize_hashed(pd.DataFrame({"id": [1, 2]}), path, "csv")
    try:
        loaded = deserialize(path, "csv", frame_type=reader)
        # Materialization after deserialize has already removed its staging download.
        if reader == "polars_lazy":
            assert loaded.collect()["id"].to_list() == [1, 2]
        else:
            assert loaded.fetchall() == [(1,), (2,)]
        assert _inspect.shape(path, "csv", sample=1)["sample"] == [{"id": "1"}]
    finally:
        _storage.get_fs(path).rm(path)


def test_csv_sink_inference_and_invalid_value_are_atomic(tmp_path):
    path = tmp_path / "frame.csv"
    step = {"sinks": [{"path": str(path)}]}
    assert _write_sinks(pd.DataFrame({"id": [1]}), step, "frame", "parquet")[0]["status"] == "ok"
    before = path.read_bytes()
    outcome = _write_sinks({"id": 2}, step, "frame", "json")[0]
    assert outcome["status"] == "error"
    assert "csv format requires" in outcome["error"]
    assert path.read_bytes() == before
    with pytest.raises(TypeError, match="csv format requires"):
        serialize_hashed([1, 2], path, "csv")
    assert path.read_bytes() == before


def test_csv_cli_output_cache_downstream_status_and_sql(tmp_path):
    (tmp_path / "pipeline.py").write_text("""
import pandas as pd
import polars as pl
from barca import asset, sink

@asset(serializer="csv")
@sink("export.csv")
def orders():
    return pd.DataFrame({"id": [1, 2], "amount": [9.5, 12.0]})

@asset(inputs={"orders": orders})
def total(orders: pl.LazyFrame):
    return {"total": orders.select(pl.col("amount").sum()).collect().item()}
""")
    env = {
        **os.environ,
        "PYTHONPATH": str(Path(__file__).resolve().parents[1]),
        "BARCA_OUTPUT": "human",
    }

    def cli(*args):
        run = subprocess.run(
            [_find_binary(), *args],
            cwd=tmp_path,
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
        )
        assert run.returncode == 0, run.stderr
        return json.loads(run.stdout)

    plan = cli("plan", "pipeline.py")
    assert plan["total_steps"] == 2
    result = cli("get", "orders", "pipeline.py", "--json")
    ref = result["final_output"]["_barca_artifact"]
    assert ref["format"] == "csv" and ref["path"].endswith(".csv")
    assert (tmp_path / "export.csv").read_text().startswith("id,amount\n")
    assert cli("get", "total", "pipeline.py", "-o", "value", "--json")["final_output"] == {
        "total": 21.5
    }
    # Cache hits must retain their CSV format and remain readable.
    assert cli("get", "total", "pipeline.py", "-o", "value", "--json")["final_output"] == {
        "total": 21.5
    }
    status = cli("status", "orders", "pipeline.py", "--json", "--sample", "1")
    assert "csv" in json.dumps(status) and "9.5" in json.dumps(status)
    assert cli("sql", "select sum(amount) as total from orders", "pipeline.py", "--json")[
        "rows"
    ] == [{"total": 21.5}]


@pytest.mark.parametrize("kind", ["sensor", "task"])
def test_csv_sensor_and_task_execution(tmp_path, kind, capsys):
    from barca._worker import run_batch

    source = tmp_path / "steps.py"
    source.write_text("import pandas as pd\ndef frame(): return pd.DataFrame({'id': [7]})\n")
    run_batch(
        {
            "stream_id": "csv-test",
            "artifact_dir": str(tmp_path / "artifacts"),
            "steps": [
                {
                    "node_id": "steps.py:frame",
                    "source_file": str(source),
                    "function_name": "frame",
                    "kind": kind,
                    "serializer": "csv",
                }
            ],
        }
    )
    messages = [
        json.loads(line.removeprefix("BARCA:2:"))
        for line in capsys.readouterr().err.splitlines()
        if line.startswith("BARCA:2:")
    ]
    assert len(messages) == 1 and messages[0]["type"] == "result"
    artifact = messages[0]["artifact"]
    assert artifact["format"] == "csv"
    assert deserialize(artifact["path"], "csv")["id"].tolist() == [7]
    if kind == "sensor":
        assert (
            artifact["content_hash"]
            == hashlib.sha256(Path(artifact["path"]).read_bytes()).hexdigest()
        )
