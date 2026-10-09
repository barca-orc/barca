"""One process can disable storage without editing shared project configuration."""

import json
import os
import signal
import socket
import sqlite3
import subprocess
import time
from pathlib import Path

import pytest
from barca.api import _find_binary
from barca.client import Client

PIPELINE = """
from barca import asset, partitions

@asset()
def first():
    return [1, 2]

@asset(inputs={"values": first})
def total(values):
    return sum(values)

@asset(partitions={"key": partitions(["a", "b"])})
def part(key):
    return {"value": key}
"""
HOLD = str(Path(__file__).parent / "hold")


@pytest.fixture()
def project(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    store = tmp_path / "store"
    (root / "pipeline.py").write_text(PIPELINE)
    (root / "barca.toml").write_text(f'[remote]\nuri = "{store}"\nstate = "optimistic"\n')
    return root, store


def environment(store, **overrides):
    base = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    return {**base, "BARCA_POOL_SIZE": "2", "BARCA_REMOTE_URI": str(store), **overrides}


def cli(root, env, *args):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )


def succeeded(result):
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def observed_environment(root, store, **overrides):
    observer = root / "observer"
    observer.mkdir(exist_ok=True)
    (observer / "release").touch()  # observe, do not pause the helper
    env = environment(
        store,
        PYTHONPATH=HOLD + os.pathsep + os.environ.get("PYTHONPATH", ""),
        BARCA_TEST_HOLD=f"remote-access:{observer}",
        **overrides,
    )
    return env, observer


def test_remote_off_preserves_history_recomputes_locally_and_reuses_local_cache(project):
    root, store = project
    first = succeeded(cli(root, environment(store), "get", "total", "--json"))
    shared_db = store / "default/state/metadata.db"
    shared_before = shared_db.read_bytes()
    env, observer = observed_environment(
        root,
        store,
        BARCA_REMOTE="off",
        BARCA_STATE="optimistic",
        BARCA_ARTIFACT_URI=str(store / "literal"),
        BARCA_STORAGE_OPTIONS="invalid JSON",
        BARCA_TRANSFER_TIMEOUT="invalid",
    )
    inspected = succeeded(cli(root, env, "status", "total", "--json"))
    assert all(
        n["cache"]["state"] != "cached" and n.get("shape") is None for n in inspected["nodes"]
    )
    local = succeeded(cli(root, env, "get", "total", "--json"))
    assert local["steps_executed"] == 2 and local["final_output"] == 3
    assert succeeded(cli(root, env, "get", "total", "--json"))["steps_executed"] == 0
    inspected = succeeded(cli(root, env, "status", "total", "--json"))
    assert all(".barca/artifacts/" in n["cache"]["artifact"] for n in inspected["nodes"])
    assert not list(observer.glob("*.pids")), "remote-off contacted the configured store"
    assert shared_db.read_bytes() == shared_before
    rows = succeeded(cli(root, env, "history", "--all", "--json"))["runs"]
    assert len(rows) == 3 and first["run_id"] in {r["run_id"] for r in rows}


@pytest.mark.parametrize("remote_format", ["json", "pickle"])
def test_remote_off_uses_older_local_cache_behind_newer_remote_row_and_sql_is_local(
    project, remote_format
):
    pytest.importorskip("duckdb")
    root, store = project
    off = environment(store, BARCA_REMOTE="off")
    succeeded(cli(root, off, "get", "part", "--json"))
    succeeded(cli(root, off, "get", "total", "--json"))
    # A newer remote row must not hide an existing local result with the same identity.
    with sqlite3.connect(root / ".barca/metadata.db") as conn:
        columns = [
            r[1] for r in conn.execute("PRAGMA table_info(materializations)") if r[1] != "id"
        ]
        selection = [
            f"'{store}/external.{remote_format}'"
            if c == "artifact_path"
            else f"'{remote_format}'"
            if c == "artifact_format"
            else c
            for c in columns
        ]
        conn.execute(
            f"INSERT INTO materializations ({','.join(columns)}) SELECT {','.join(selection)} FROM materializations"
        )
    conn.close()
    env, observer = observed_environment(root, store, BARCA_REMOTE="off")
    assert succeeded(cli(root, env, "get", "total", "--json"))["steps_executed"] == 0
    inspected = succeeded(cli(root, env, "status", "total", "--json"))
    assert all(n.get("shape") is not None and "note" not in n["shape"] for n in inspected["nodes"])
    first = next(n for n in inspected["nodes"] if n["name"] == "first")
    assert first["shape"] == {"type": "list", "rows": 2}
    assert first["last_materialization"]["format"] == remote_format
    assert first["last_materialization"]["artifact"].endswith(f"external.{remote_format}")
    query = succeeded(cli(root, env, "sql", "select * from part order by value", "--json"))
    assert [r["value"] for r in query["rows"]] == ["a", "b"]
    assert not list(observer.glob("*.pids"))


@pytest.mark.parametrize("remote", ["", "on"])
def test_empty_keeps_remote_configuration_and_invalid_switch_is_usage_error(project, remote):
    root, store = project
    result = cli(root, environment(store, BARCA_REMOTE=remote), "get", "total", "--json")
    if remote == "":
        assert succeeded(result)["final_output"] == 3
        assert (store / "default/state/metadata.db").is_file()
    else:
        assert result.returncode == 2 and "BARCA_REMOTE" in result.stderr


@pytest.mark.parametrize("override", ["remote", "state"])
def test_serve_reuses_explicit_process_overrides_and_never_silently_discards_state(
    project, override
):
    root, store = project
    refused = cli(root, environment(store), "serve", "pipeline.py", "--no-schedule")
    assert refused.returncode == 2 and "BARCA_STATE=off" in refused.stderr
    env, observer = observed_environment(
        root, store, **({"BARCA_REMOTE": "off"} if override == "remote" else {"BARCA_STATE": "off"})
    )
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    with (root / "serve.log").open("w") as log:
        proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--no-schedule", "--port", str(port)],
            cwd=root,
            env=env,
            stdout=log,
            stderr=log,
            text=True,
        )
    try:
        client = Client(f"http://127.0.0.1:{port}")
        deadline = time.monotonic() + 15
        while True:
            assert proc.poll() is None, (root / "serve.log").read_text()
            try:
                client.health()
                break
            except Exception:
                if time.monotonic() > deadline:
                    pytest.fail("server did not become healthy")
                time.sleep(0.05)
        assert client.get("total").wait(timeout=15)["status"] == "complete"
        if override == "remote":
            assert not list(observer.glob("*.pids")) and not store.exists()
        else:
            assert list(observer.glob("*.pids"))
            assert list((store / "default/artifacts").rglob("*.json"))
            assert not (store / "default/state/metadata.db").exists()
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=15)
