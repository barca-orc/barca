"""Cancellation reaches partition and batch resolvers, before any worker can run (#936)."""

import json
import os
import signal
import subprocess

import pytest
from barca.api import _find_binary

from .helpers import clean_env, wait_until
from .served import snapshot


@pytest.mark.parametrize("resolver", ["partition", "batch"])
@pytest.mark.parametrize("host", ["server", "server-files", "cli"])
def test_cancel_hanging_resolver(tmp_path, serve, resolver, host):
    declaration = (
        'partitions={"key": partitions(keys())}'
        if resolver == "partition"
        else 'partitions={"key": partitions(["a"])}, batch=batches_by(group)'
    )
    (tmp_path / "pipeline.py").write_text(f"""
import os
import signal
import time
from pathlib import Path
from barca import asset, partitions, batches_by


def hold():
    if Path("armed").exists():
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        Path("resolver.pid").write_text(str(os.getpid()))
        while not Path("release").exists():
            time.sleep(0.02)


def keys():
    hold()
    return ["a"]


def group(key):
    hold()
    return "all"


@asset({declaration})
def result(key):
    Path("ran").touch()
    return key
""")
    proc = None
    server = None
    try:
        if host.startswith("server"):
            server = serve("--no-schedule", "--drain-timeout", "0")
            server.post("/queue/pause", {})
            status, body = server.request(
                "POST",
                "/runs",
                {
                    "command": "get",
                    "targets": ["result"],
                    "files": ["pipeline.py"] if host == "server-files" else [],
                },
            )
            assert status == 202, body
            run_id = body["run_id"]
            (tmp_path / "armed").touch()
            server.post("/queue/resume", {})
        else:
            (tmp_path / "armed").touch()
            proc = subprocess.Popen(
                [_find_binary(), "get", "result", "--json"],
                cwd=tmp_path,
                env=clean_env(BARCA_POOL_SIZE="2"),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
        wait_until((tmp_path / "resolver.pid").exists, "resolver to hang")
        pid = int((tmp_path / "resolver.pid").read_text())
        if server:
            assert server.request("DELETE", f"/runs/{run_id}")[0] == 200
            server.wait(
                "cancelled resolver run",
                lambda: server.run(run_id)["status"] == "cancelled",
                timeout=5,
            )
            with snapshot(tmp_path) as db:
                assert db.execute(
                    "select status from runs where run_id = ?", (run_id,)
                ).fetchone() == ("cancelled",)
                assert db.execute(
                    "select count(*) from run_queue where run_id = ?", (run_id,)
                ).fetchone() == (0,)
        else:
            proc.send_signal(signal.SIGINT)
            _, err = proc.communicate(timeout=5)
            assert proc.returncode == 130, err
            assert json.loads(err.strip().splitlines()[-1])["kind"] == "cancelled"
        with pytest.raises(ProcessLookupError):
            os.kill(pid, 0)
        assert not (tmp_path / "ran").exists()
    finally:
        (tmp_path / "release").touch()
        if proc is not None and proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=5)
        if server is not None:
            server.stop(timeout=5)


def test_cancel_after_drain_requeues_before_exit(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text("""
import os
import signal
import time
from pathlib import Path
from barca import task, asset, partitions, batches_by

@task()
def work():
    with open("starts", "a") as f:
        f.write("start\\n")
    while not Path("release-work").exists():
        time.sleep(0.02)

def group(key):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    Path("resolver.pid").write_text(str(os.getpid()))
    while not Path("release").exists():
        time.sleep(0.02)
    return "all"

@asset(partitions={"key": partitions(["a"])}, batch=batches_by(group))
def blocker(key):
    return key
""")
    server = serve("--no-schedule", "--drain-timeout", "1", "--max-runs", "2")
    try:
        run_id = server.submit("work")["run_id"]
        wait_until((tmp_path / "starts").exists, "work to start")
        server.submit("blocker", "get")
        wait_until((tmp_path / "resolver.pid").exists, "second run's resolver")
        server.send(signal.SIGTERM)
        server.wait(
            "first run to be requeued", lambda: server.run(run_id)["status"] == "pending", timeout=5
        )
        code, response = server.request("DELETE", f"/runs/{run_id}")
        assert code == 200, response
        with snapshot(tmp_path) as db:
            queued = db.execute(
                "select count(*) from run_queue where run_id = ?", (run_id,)
            ).fetchone()
            status = db.execute("select status from runs where run_id = ?", (run_id,)).fetchone()
        (tmp_path / "release").touch()
        assert server.exited(timeout=10) == 0, server.stderr
        (tmp_path / "release-work").touch()
        restarted = serve("--no-schedule")
        try:
            actual = restarted.finished(run_id)["run"]["status"]
            assert (queued, status, actual, (tmp_path / "starts").read_text()) == (
                (0,),
                ("cancelled",),
                "cancelled",
                "start\n",
            )
            assert response["status"] == "cancelled"
        finally:
            restarted.stop()
    finally:
        (tmp_path / "release").touch()
        server.stop(timeout=5)
