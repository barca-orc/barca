"""Content-digest/recompute boundary: specs/materialization-digests.md."""

import hashlib
import json
import os
import sqlite3
import subprocess
from contextlib import closing
from pathlib import Path

import pytest
from barca.api import _find_binary


def _cli(tmp_path, *args):
    result = subprocess.run(
        [_find_binary(), *args],
        cwd=tmp_path,
        env={
            **os.environ,
            "PYTHONPATH": str(Path(__file__).resolve().parents[1]),
            "BARCA_OUTPUT": "human",
        },
        text=True,
        capture_output=True,
        timeout=60,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout), result.stderr


def _rows(tmp_path):
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        return db.execute(
            "SELECT run_id, run_hash, artifact_path, output_hash, bytes_changed "
            "FROM materializations WHERE status='success' ORDER BY id"
        ).fetchall()


@pytest.mark.parametrize("fmt", ["json", "pickle", "parquet", "csv"])
@pytest.mark.parametrize("trigger", ["refresh", "missing"])
def test_recompute_compares_digest_and_retains_last_event(tmp_path, fmt, trigger):
    source = f"""
from pathlib import Path
import pandas as pd
from barca import asset
@asset(serializer={fmt!r})
def result():
    value = int(Path("external.txt").read_text())
    return pd.DataFrame({{"value": [value]}}) if {fmt!r} in ("parquet", "csv") else {{"value": value}}
"""
    (tmp_path / "pipeline.py").write_text(source)
    (tmp_path / "external.txt").write_text("7")
    first, err = _cli(tmp_path, "get", "result", "pipeline.py", "--json")
    assert "not deterministic" not in err
    rows = _rows(tmp_path)
    assert len(rows) == 1 and rows[0][4] == 0
    assert rows[0][3] == hashlib.sha256((tmp_path / rows[0][2]).read_bytes()).hexdigest()
    unchanged, err = _cli(tmp_path, "get", "result", "pipeline.py", "--refresh", "result", "--json")
    assert not unchanged["steps"][0].get("bytes_changed") and "not deterministic" not in err
    (tmp_path / "external.txt").write_text("8")
    flags = ["--refresh", "result"]
    if trigger == "missing":
        (tmp_path / _rows(tmp_path)[-1][2]).unlink()
        flags = []
    changed, err = _cli(tmp_path, "get", "result", "pipeline.py", *flags, "--json")
    assert changed["steps"][0]["bytes_changed"] is True
    assert (
        err.count(
            "produced different bytes for the same code and inputs; the function is not deterministic"
        )
        == 1
    )
    rows = _rows(tmp_path)
    assert rows[-1][4] == 1 and rows[-1][1] == rows[0][1]
    stable, err = _cli(tmp_path, "get", "result", "pipeline.py", "--refresh", "result", "--json")
    assert not stable["steps"][0].get("bytes_changed") and "not deterministic" not in err
    status, _ = _cli(tmp_path, "status", "pipeline.py", "--json")
    node = status["nodes"][0]
    assert node["last_bytes_change"]["run_hash"] == rows[-1][1]
    assert node["last_bytes_change"]["content_hash"] == rows[-1][3]
    assert node["last_materialization"]["content_hash"] == _rows(tmp_path)[-1][3]
    # Editing code changes the run hash; changing bytes then is expected.
    (tmp_path / "pipeline.py").write_text(source.replace('"external.txt"', '"other.txt"'))
    (tmp_path / "other.txt").write_text("9")
    edited, err = _cli(tmp_path, "get", "result", "pipeline.py", "--json")
    assert not edited["steps"][0].get("bytes_changed") and "not deterministic" not in err
    assert _rows(tmp_path)[-1][1] != rows[-1][1]


def test_partition_digest_events_are_per_key(tmp_path):
    (tmp_path / "pipeline.py").write_text("""
from pathlib import Path
from barca import asset, partitions
@asset(partitions={"k": partitions(["a", "b"])})
def result(k):
    return {"value": Path(f"{k}.txt").read_text()}
""")
    (tmp_path / "a.txt").write_text("7")
    (tmp_path / "b.txt").write_text("7")
    _cli(tmp_path, "get", "result", "pipeline.py", "--json")
    (tmp_path / "b.txt").write_text("8")
    changed, err = _cli(tmp_path, "get", "result", "pipeline.py", "--refresh", "result", "--json")
    assert changed["steps"][0]["bytes_changed"] is True
    assert err.count("not deterministic") == 1 and "result[k=b]" in err
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        events = db.execute("SELECT node_id FROM materializations WHERE bytes_changed=1").fetchall()
    assert len(events) == 1 and events[0][0].endswith("result[k=b]")


def test_legacy_rows_without_digests_establish_a_baseline(tmp_path):
    (tmp_path / "pipeline.py").write_text("""
from pathlib import Path
from barca import asset
@asset()
def result(): return Path("external.txt").read_text()
""")
    (tmp_path / "external.txt").write_text("old")
    _cli(tmp_path, "get", "result", "pipeline.py", "--json")
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        db.execute("UPDATE materializations SET output_hash=NULL")
        db.commit()
    (tmp_path / "external.txt").write_text("new")
    result, err = _cli(tmp_path, "get", "result", "pipeline.py", "--refresh", "result", "--json")
    assert not result["steps"][0].get("bytes_changed") and "not deterministic" not in err
    assert _rows(tmp_path)[-1][3]


def test_sensor_returning_to_earlier_value_compares_earlier_run_hash(tmp_path):
    (tmp_path / "pipeline.py").write_text("""
from pathlib import Path
from barca import asset, sensor
@sensor()
def tick(): return True, Path("tick.txt").read_text()
@asset(inputs={"tick": tick})
def result(tick): return {"tick": tick, "value": Path("external.txt").read_text()}
""")
    hashes = []
    for tick, value, refresh in [("a", "1", False), ("b", "2", False), ("a", "3", True)]:
        (tmp_path / "tick.txt").write_text(tick)
        (tmp_path / "external.txt").write_text(value)
        flags = ["--refresh", "result"] if refresh else []
        result, err = _cli(tmp_path, "get", "result", "pipeline.py", *flags, "--json")
        step = next(step for step in result["steps"] if step["id"].endswith(":result"))
        hashes.append(step["run_hash"])
        assert bool(step.get("bytes_changed")) is refresh
    assert hashes[0] == hashes[2] and hashes[0] != hashes[1]
    assert "result produced different bytes" in err


def test_checkpoint_and_terminal_keep_one_byte_change(tmp_path):
    import time

    (tmp_path / "pipeline.py").write_text("""
from pathlib import Path
import time
from barca import asset
@asset()
def value(): return Path("external.txt").read_text()
@asset(inputs={"value": value})
def later(value):
    while Path("hold").exists():
        time.sleep(0.02)
    return value
""")
    (tmp_path / "external.txt").write_text("old")
    _cli(tmp_path, "get", "later", "pipeline.py", "--json")
    (tmp_path / "external.txt").write_text("new")
    hold = tmp_path / "hold"
    hold.touch()
    proc = subprocess.Popen(
        [_find_binary(), "get", "later", "pipeline.py", "--refresh-all", "--json"],
        cwd=tmp_path,
        env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                out, err = proc.communicate()
                pytest.fail(f"held run ended before checkpoint: {err}")
            status, _ = _cli(tmp_path, "status", "value", "pipeline.py", "--json")
            if any(node.get("last_bytes_change") for node in status["nodes"]):
                break
            time.sleep(0.05)
        else:
            pytest.fail("changed upstream materialization was not checkpointed")
        hold.unlink()
        out, err = proc.communicate(timeout=30)
        assert proc.returncode == 0, err
        assert err.count("value produced different bytes") == 1
        run_id = json.loads(out)["run_id"]
        rows = [row for row in _rows(tmp_path) if row[0] == run_id]
        assert len(rows) == 2 and all(row[4] == 1 for row in rows)
    finally:
        hold.unlink(missing_ok=True)
        if proc.poll() is None:
            proc.terminate()
            proc.communicate(timeout=10)


def test_a_manual_asset_refreshed_by_hand_with_new_bytes_does_not_warn(tmp_path):
    """A `Manual` asset is refreshed by hand to pick up new data: new bytes are the point."""
    (tmp_path / "pipeline.py").write_text("""
from pathlib import Path
from barca import asset, Manual
@asset(freshness=Manual)
def fx(): return float(Path("ref.txt").read_text())
@asset(inputs={"fx": fx})
def price(fx): return 100 * fx
""")
    (tmp_path / "ref.txt").write_text("1.08")
    _cli(tmp_path, "get", "price", "pipeline.py", "--refresh", "fx", "--json")
    (tmp_path / "ref.txt").write_text("1.20")
    result, err = _cli(tmp_path, "get", "fx", "pipeline.py", "--refresh", "fx", "--json")
    assert "not deterministic" not in err, err
    assert not any(step.get("bytes_changed") for step in result["steps"]), result["steps"]
    status, _ = _cli(tmp_path, "status", "pipeline.py", "--json")
    assert all(node.get("last_bytes_change") is None for node in status["nodes"]), status
    # The refresh still reaches what reads it (#548).
    result, err = _cli(tmp_path, "get", "price", "pipeline.py", "-o", "value", "--json")
    assert result["final_output"] == 120.0 and "not deterministic" not in err, err
