"""Real one-minute shared progress and cross-machine recovery (#214)."""

import json
import os
import signal
import sqlite3
import subprocess
import time

from barca.api import _find_binary

from .test_incremental_persist import PIPELINE
from .test_serve_robustness import _env, wait_for


def remote_rows(path):
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return (
            conn.execute("SELECT status, steps_executed, finished_at FROM runs").fetchall(),
            conn.execute("SELECT node_id, artifact_path FROM materializations").fetchall(),
        )
    finally:
        conn.close()


def test_real_minute_checkpoint_survives_sigkill_and_skips_clean_ticks(tmp_path):
    shared = tmp_path / "shared"
    source = PIPELINE.replace("deadline = time.time() + 60", "deadline = time.time() + 240")
    machine_a = tmp_path / "a"
    machine_b = tmp_path / "b"
    for machine in (machine_a, machine_b):
        machine.mkdir()
        (machine / "pipeline.py").write_text(source)
        (machine / "barca.toml").write_text("")
    env = _env()
    env.update(BARCA_REMOTE_URI=str(shared), BARCA_STATE="optimistic", BARCA_POOL_SIZE="2")
    log = machine_a / "stderr"
    started = time.monotonic()
    with log.open("w") as stderr:
        proc = subprocess.Popen(
            [_find_binary(), "get", "pipeline.py", "--agent"],
            cwd=machine_a,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=stderr,
            start_new_session=True,
        )
    remote = shared / "default/state/metadata.db"
    try:
        wait_for(lambda: (machine_a / "slow.started").exists(), "held computation")
        assert proc.poll() is None
        assert not remote.exists(), log.read_text()
        # Publication must happen during the held step, on the real fixed minute timer.
        deadline = started + 85
        while not remote.exists():
            assert proc.poll() is None, log.read_text()
            assert time.monotonic() < deadline, log.read_text()
            time.sleep(0.1)
        assert time.monotonic() - started >= 55
        runs, rows = remote_rows(remote)
        assert runs == [("running", 2, None)]
        assert {node for node, _ in rows} == {"pipeline.py:first", "pipeline.py:second"}
        for _, path in rows:
            assert os.path.isfile(path)
        identity = (remote.stat().st_ino, remote.stat().st_mtime_ns, remote.stat().st_size)
        # Cross a second real timer tick with no newly committed results. An identical
        # replacement would change inode/mtime too, so this verifies no extra publication.
        while time.monotonic() - started < 125:
            assert proc.poll() is None, log.read_text()
            assert (
                remote.stat().st_ino,
                remote.stat().st_mtime_ns,
                remote.stat().st_size,
            ) == identity
            time.sleep(0.2)
        print(
            f"one shared checkpoint, {identity[2]} bytes, no unchanged-progress upload at second tick"
        )
        proc.kill()
        assert proc.wait(timeout=15) == -signal.SIGKILL
        (machine_b / "release").touch()
        resumed = subprocess.run(
            [_find_binary(), "get", "pipeline.py", "--agent"],
            cwd=machine_b,
            env=env,
            text=True,
            capture_output=True,
            timeout=30,
        )
        assert resumed.returncode == 0, resumed.stderr
        result = json.loads(resumed.stdout.splitlines()[-1])
        assert (result["steps_executed"], result["final_output"]) == (1, 3)
        assert not (machine_b / "first.ran").exists()
        assert not (machine_b / "second.ran").exists()
        runs, rows = remote_rows(remote)
        assert len(runs) == 2
        assert any(status == "success" and count == 1 for status, count, _ in runs)
        assert len(rows) == 3
    finally:
        (machine_a / "release").touch()
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=15)
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
