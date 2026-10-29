"""RFC-0015: served runs share cadence; final backups survive long intervals and off."""

import json
import signal
import sqlite3
import subprocess

import pytest
from barca.api import _find_binary

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
