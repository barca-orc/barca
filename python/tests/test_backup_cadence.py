"""RFC-0015: served runs share cadence; final backups survive long intervals and off."""

import json
import os
import signal
import sqlite3
import subprocess
import time

import pytest
from barca.api import _find_binary

from . import test_serve_robustness as serve_helpers
from .test_remote_cancel import HOLD_SHIM, alive
from .test_serve_robustness import Server, _env, wait_for


@pytest.mark.parametrize("interval", ["off", "1h"])
def test_served_runs_share_cadence_and_shutdown_covers_every_run(tmp_path, interval):
    store = tmp_path / "store"
    store.mkdir()
    (tmp_path / "pipeline.py").write_text(
        "from barca import task\n@task()\ndef tick():\n    return 1\n"
    )
    (tmp_path / "barca.toml").write_text(
        f"[remote]\nuri = {json.dumps(str(store))}\nbackup_interval = {json.dumps(interval)}\n"
    )
    backups = store / "default/state/backups"
    server = Server(tmp_path)
    try:
        for _ in range(3):
            status, response = server.request("POST", "/run/tick")
            assert status == 200, response
            run_id = response["run_id"]
            view = wait_for(
                lambda run_id=run_id: (
                    doc
                    if (doc := server.request("GET", f"/runs/{run_id}")[1])["status"]
                    not in ("pending", "running", "queued")
                    else None
                ),
                "served task completion",
            )
            assert view["status"] == "success", view
        routine = sorted(backups.glob("*.db"))
        assert len(routine) == (0 if interval == "off" else 1), server.log.read_text()
        server.proc.send_signal(signal.SIGTERM)
        assert server.proc.wait(timeout=30) == 0, server.log.read_text()
        copies = sorted(backups.glob("*.db"))
        assert len(copies) == len(routine) + 1, server.log.read_text()
        with sqlite3.connect(copies[-1]) as conn:
            assert (
                conn.execute("SELECT count(*) FROM runs WHERE status = 'success'").fetchone()[0]
                == 3
            )
    finally:
        server.stop()


def test_off_retains_standalone_final_backup_and_environment_overrides_toml(tmp_path):
    store = tmp_path / "store"
    store.mkdir()
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value():\n    return 42\n"
    )
    (tmp_path / "barca.toml").write_text(
        f'[remote]\nuri = {json.dumps(str(store))}\nbackup_interval = "1h"\n'
    )
    env = _env() | {"BARCA_BACKUP_INTERVAL": "off"}
    out = subprocess.run(
        [_find_binary(), "get", "pipeline.py", "--agent"],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    assert out.returncode == 0, out.stderr
    assert len(list((store / "default/state/backups").glob("*.db"))) == 1
    env["BARCA_BACKUP_INTERVAL"] = ""
    out = subprocess.run(
        [_find_binary(), "status", "--json"],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    assert out.returncode == 2 and "BARCA_BACKUP_INTERVAL" in out.stderr


def test_server_timer_uses_the_configured_interval_after_restart(tmp_path):
    store = tmp_path / "store"
    store.mkdir()
    (tmp_path / "pipeline.py").write_text(
        "from barca import task\n@task()\ndef tick():\n    return 1\n"
    )
    (tmp_path / "barca.toml").write_text(
        f'[remote]\nuri = {json.dumps(str(store))}\nbackup_interval = "1s"\n'
    )
    subprocess.run(
        [_find_binary(), "run", "tick", "pipeline.py", "--agent"],
        cwd=tmp_path,
        env=_env(),
        capture_output=True,
        text=True,
        timeout=30,
        check=True,
    )
    backups = store / "default/state/backups"
    before = set(backups.glob("*.db"))
    # Restart with future wall-clock evidence: suppression must still end within one
    # configured interval of observing it. No served run end or recorder can take a copy.
    (tmp_path / ".barca/backups/.last-attempt").write_text(str(time.time_ns() + 3_600_000_000_000))
    server = Server(tmp_path)
    try:
        with sqlite3.connect(tmp_path / ".barca/metadata.db") as conn:
            conn.execute(
                "INSERT INTO runs (run_id, command, files, status) VALUES ('dirty', 'get', '[]', 'success')"
            )
        wait_for(
            lambda: set(backups.glob("*.db")) - before,
            "configured one-second timer backup",
            timeout=10,
        )
    finally:
        server.stop()


def test_off_keeps_the_final_backup_task_and_its_deadline_for_a_stalled_upload(
    tmp_path, monkeypatch
):
    store = tmp_path / "store"
    store.mkdir()
    hold = tmp_path / "hold"
    hold.mkdir()
    (tmp_path / "pipeline.py").write_text(
        "from barca import task\n@task()\ndef tick():\n    return 1\n"
    )
    (tmp_path / "barca.toml").write_text(
        f'[remote]\nuri = {json.dumps(str(store))}\nbackup_interval = "off"\n'
        "[server]\ndrain_timeout = 0\n"
    )
    env = _env() | {
        "PYTHONPATH": HOLD_SHIM + os.pathsep + os.environ.get("PYTHONPATH", ""),
        "BARCA_TEST_HOLD": f"backup:{hold}",
        "BARCA_TEST_STALL": "1",
    }
    monkeypatch.setattr(serve_helpers, "_env", lambda: env)
    server = Server(tmp_path)
    try:
        run_id = server.request("POST", "/run/tick")[1]["run_id"]
        wait_for(
            lambda: server.request("GET", f"/runs/{run_id}")[1]["status"] == "success",
            "task completion",
        )
        assert not (hold / "backup.started").exists()
        started = time.monotonic()
        server.proc.send_signal(signal.SIGTERM)
        wait_for(lambda: (hold / "backup.started").exists(), "final backup despite off")
        assert server.proc.wait(timeout=12) == 0, server.log.read_text()
        assert time.monotonic() - started < 12
        pids = [int(pid) for pid in (hold / "backup.pids").read_text().splitlines()]
        wait_for(lambda: all(not alive(pid) for pid in pids), "stalled backup helper cleanup")
        assert "metadata backup failed" in server.log.read_text()
    finally:
        (hold / "release").touch()
        server.stop()
