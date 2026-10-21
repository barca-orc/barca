"""The real one-minute backup checkpoint, and recovery of a wiped machine from it (RFC-0009)."""

import json
import os
import signal
import sqlite3
import subprocess
import time

from barca.api import _find_binary

from .test_incremental_persist import PIPELINE
from .test_serve_robustness import _env, wait_for


def backup_rows(path):
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return (
            conn.execute("SELECT status, steps_executed, finished_at FROM runs").fetchall(),
            conn.execute("SELECT node_id, artifact_path FROM materializations").fetchall(),
        )
    finally:
        conn.close()


def store_backups(store):
    return sorted((store / "default/state/backups").glob("*Z.db"))


def test_real_minute_checkpoint_backs_up_progress_and_restores_a_wiped_machine(tmp_path):
    store = tmp_path / "store"
    source = PIPELINE.replace("deadline = time.time() + 60", "deadline = time.time() + 240")
    machine = tmp_path / "machine"
    machine.mkdir()
    (machine / "pipeline.py").write_text(source)
    (machine / "barca.toml").write_text("")
    env = _env()
    env.update(BARCA_REMOTE_URI=str(store), BARCA_POOL_SIZE="2")
    log = machine / "stderr"
    started = time.monotonic()
    with log.open("w") as stderr:
        proc = subprocess.Popen(
            [_find_binary(), "get", "pipeline.py", "--agent"],
            cwd=machine,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=stderr,
            start_new_session=True,
        )
    try:
        wait_for(lambda: (machine / "slow.started").exists(), "held computation")
        assert proc.poll() is None
        assert not store_backups(store), log.read_text()
        # The backup must happen during the held step, on the real fixed minute timer.
        deadline = started + 85
        while not store_backups(store):
            assert proc.poll() is None, log.read_text()
            assert time.monotonic() < deadline, log.read_text()
            time.sleep(0.1)
        assert time.monotonic() - started >= 55
        (backup,) = store_backups(store)
        runs, rows = backup_rows(backup)
        assert runs == [("running", 2, None)]
        assert {node for node, _ in rows} == {"pipeline.py:first", "pipeline.py:second"}
        sidecar = json.loads(backup.with_suffix(".json").read_text())
        assert sidecar["rows"] == 3 and sidecar["run_id"]
        assert (machine / ".barca/backups" / backup.name).is_file()
        # Cross a second real timer tick with no newly recorded results: no second copy.
        while time.monotonic() - started < 125:
            assert proc.poll() is None, log.read_text()
            assert store_backups(store) == [backup]
            time.sleep(0.2)
        proc.kill()
        assert proc.wait(timeout=15) == -signal.SIGKILL

        # The machine is wiped. Nothing comes back by itself: the next command starts empty,
        # and only `barca state restore` brings the history down from the store.
        subprocess.run(["rm", "-rf", str(machine / ".barca")], check=True)
        listed = subprocess.run(
            [_find_binary(), "state", "backups", "--json"],
            cwd=machine,
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
        )
        assert listed.returncode == 0, listed.stderr
        (entry,) = json.loads(listed.stdout)["backups"]
        assert (entry["name"], entry["where"], entry["index"]) == (backup.name, "store", 1)
        restored = subprocess.run(
            [_find_binary(), "state", "restore", "--json"],
            cwd=machine,
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
        )
        assert restored.returncode == 0, restored.stderr
        assert json.loads(restored.stdout)["from"] == "store"
        assert f"restored {backup.name} from the store copy" in restored.stderr

        (machine / "release").touch()
        resumed = subprocess.run(
            [_find_binary(), "get", "pipeline.py", "--agent", "-o", "value", "--json"],
            cwd=machine,
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
        )
        assert resumed.returncode == 0, resumed.stderr
        result = json.loads(resumed.stdout.splitlines()[-1])
        # The two finished steps come from the store, through the restored history.
        assert (result["steps_executed"], result["final_output"]) == (1, 3)
    finally:
        (machine / "release").touch()
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=15)
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
