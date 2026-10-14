"""Executable evidence for the current-behavior documentation audit (#294)."""

import json
import os
import signal
import socket
import subprocess
import time
from pathlib import Path

from barca.api import _find_binary
from barca.client import Client


def run(root: Path, *args: str, env=None):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        capture_output=True,
        text=True,
        env={**os.environ, "BARCA_POOL_SIZE": "2", **(env or {})},
        check=False,
        timeout=30,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def test_manual_is_not_a_barrier_and_always_sensor_is_accepted(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    source = """from barca import asset, sensor, Always, Manual

@asset(freshness=Manual)
def source(): return 1

@asset(inputs={"value": source})
def downstream(value): return value + 1

@sensor(freshness=Always)
def observed(): return True, "version"
"""
    (tmp_path / "pipeline.py").write_text(source)
    assert run(tmp_path, "get", "downstream")["steps_executed"] == 2
    assert run(tmp_path, "get", "downstream")["steps_executed"] == 0
    (tmp_path / "pipeline.py").write_text(source.replace("return 1", "return 4"))
    changed = run(tmp_path, "get", "downstream")
    assert changed["steps_executed"] == 2 and changed["final_output"] == 5
    for _ in range(2):
        observed = run(tmp_path, "get", "observed")
        assert observed["steps_executed"] == 1 and observed["final_output"] == "version"


def test_partition_final_output_is_first_key_while_all_keys_are_materialized(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text("""from barca import asset, partitions
@asset(partitions={"key": partitions(["z", "a", "m"])})
def part(key): return {"key": key}
""")
    cold = run(tmp_path, "get", "part")
    assert cold["steps_executed"] == 3 and cold["final_output"] == {"key": "a"}
    assert len(list((tmp_path / ".barca/artifacts").glob("*--part_key_*/*.json"))) == 3
    assert run(tmp_path, "get", "part")["steps_executed"] == 0


def test_state_off_uploads_artifacts_without_cross_machine_cache_discovery(tmp_path):
    store = tmp_path / "store"
    for name in ("one", "two"):
        root = tmp_path / name
        root.mkdir()
        (root / "barca.toml").write_text(f'[remote]\nuri = "{store}"\nstate = "off"\n')
        (root / "pipeline.py").write_text("""from barca import asset
@asset()
def result(): return {"value": 7}
""")
        assert run(root, "get", "result")["steps_executed"] == 1
        assert run(root, "get", "result")["steps_executed"] == 0
    assert list((store / "default/artifacts").rglob("*.json"))
    assert not (store / "default/state/metadata.db").exists()


def test_sql_scalar_list_copy_and_shared_history_side_effects(tmp_path):
    store = tmp_path / "store"
    source = """from barca import asset
@asset()
def numbers(): return [1, 2]
"""
    for name in ("one", "two"):
        root = tmp_path / name
        root.mkdir()
        (root / "barca.toml").write_text(f'[remote]\nuri = "{store}"\n')
        (root / "pipeline.py").write_text(source)
    run(tmp_path / "one", "get", "numbers")
    root = tmp_path / "two"
    assert not (root / ".barca").exists()
    query = run(root, "sql", "select * from numbers")
    assert query["columns"] == ["json"]
    assert query["rows"] == [{"json": 1}, {"json": 2}]
    assert (root / ".barca/metadata.db").exists()
    assert run(root, "history", "--all")["total"] == 1
    run(root, "sql", "COPY (select * from numbers) TO 'export.csv' (HEADER)")
    assert (root / "export.csv").read_text().splitlines() == ["json", "1", "2"]


def test_invalid_transfer_settings_are_infrastructure_errors(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text("from barca import asset\n@asset()\ndef x(): return 1\n")
    for variable in ("BARCA_TRANSFER_CONCURRENCY", "BARCA_TRANSFER_TIMEOUT"):
        proc = subprocess.run(
            [_find_binary(), "get", "x"],
            cwd=tmp_path,
            capture_output=True,
            text=True,
            env={**os.environ, variable: "0"},
            check=False,
            timeout=10,
        )
        assert proc.returncode == 3
        envelope = json.loads(proc.stderr.strip().splitlines()[-1])
        assert envelope["kind"] == "infra" and variable in envelope["error"]
        assert not (tmp_path / ".barca").exists()


def test_scheduled_sensor_ticks_without_triggering_its_consumer(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text("""from pathlib import Path
from barca import asset, sensor, Schedule

@sensor(freshness=Schedule("* * * * * *"))
def observed():
    Path("tick").touch()
    return True, "v1"

@asset(inputs={"value": observed})
def consumer(value):
    Path("consumer-ran").touch()
    return value
""")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    with (tmp_path / "server.log").open("w") as log:
        proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--port", str(port)],
            cwd=tmp_path,
            env={**os.environ, "BARCA_POOL_SIZE": "1"},
            stdout=log,
            stderr=log,
        )
    try:
        deadline = time.monotonic() + 15
        while not (tmp_path / "tick").exists():
            assert proc.poll() is None, (tmp_path / "server.log").read_text()
            assert time.monotonic() < deadline, (tmp_path / "server.log").read_text()
            time.sleep(0.05)
        client = Client(f"http://127.0.0.1:{port}")
        assert client.health()["status"] == "ok"
        assert not (tmp_path / "consumer-ran").exists()
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=15)
    assert not (tmp_path / "consumer-ran").exists()


def test_parallel_from_an_asset_runs_once_then_is_cached(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text("""from functools import partial
from barca import asset, task, parallel

@task()
def branch(value): return value * 2

@asset()
def combined(): return parallel(partial(branch, 2), partial(branch, 3))
""")
    first = run(tmp_path, "get", "combined")
    assert first["final_output"] == [4, 6]
    assert first["steps_executed"] > 0
    second = run(tmp_path, "get", "combined")
    assert second["steps_executed"] == 0 and second["final_output"] == [4, 6]
