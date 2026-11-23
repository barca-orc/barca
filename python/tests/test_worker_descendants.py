"""Worker shutdown owns ordinary inherited subprocesses (#935)."""

import contextlib
import json
import os
import signal
import subprocess
import sys

import pytest

from barca.api import _find_binary

from .helpers import clean_env, wait_until
from .served import Serve
from .test_serve_robustness import process_state

PIPELINE = """
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from functools import partial
from barca import asset, parallel, task


def spawn(name):
    child = subprocess.Popen([sys.executable, "-c", "import os, signal, time; from pathlib import Path; signal.signal(signal.SIGTERM, signal.SIG_IGN); Path('" + name + ".child-ready').touch(); time.sleep(1000)"])
    Path(name + ".json").write_text(json.dumps([os.getpid(), child.pid, os.getpgrp(), os.getpgid(child.pid)]))
    while not Path(name + ".child-ready").exists():
        time.sleep(.01)
    Path(name + ".ready").touch()


@task()
def branch():
    spawn("branch")
    while not Path("finish").exists():
        time.sleep(.01)


@asset()
def quick():
    return 1


@asset()
def held():
    spawn("held")
    if Path("parallel").exists():
        return parallel(partial(branch))
    while not Path("finish").exists():
        time.sleep(.01)
    return 1
"""


@pytest.mark.parametrize("mode", ["cancel", "deadline", "forced", "local", "terminal"])
@pytest.mark.parametrize("parallel", [False, True])
def test_shutdown_leaves_no_worker_descendants(tmp_path, mode, parallel):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    if parallel:
        (tmp_path / "parallel").touch()
    env = clean_env(BARCA_POOL_SIZE="2")
    server = None
    records = []
    # An unrelated process must survive even if its parent is this test runner too.
    unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(1000)"])
    try:
        if mode in ("local", "terminal"):
            with (tmp_path / "local.log").open("w") as log:
                proc = subprocess.Popen(
                    [_find_binary(), "get", "held", "pipeline.py", "--agent"],
                    cwd=tmp_path,
                    env=env,
                    stdout=log,
                    stderr=log,
                    start_new_session=True,
                )
        else:
            server = Serve(
                tmp_path,
                "--no-schedule",
                "--drain-timeout",
                "1" if mode == "deadline" else "30",
                env=env,
            )
            proc = server.proc
            run = server.submit("held", "get")["run_id"]
        names = ["held", "branch"] if parallel else ["held"]
        for name in names:
            wait_until(
                (tmp_path / f"{name}.ready").exists, f"{name} child ready", proc=proc, timeout=10
            )
            records.append(json.loads((tmp_path / f"{name}.json").read_text()))
        if parallel:
            wait_until(
                lambda: (process_state(records[0][0]) or "").startswith("T"),
                "parallel caller stopped",
            )
        if mode == "cancel":
            assert server is not None
            assert server.request("DELETE", f"/runs/{run}")[0] == 200
            server.finished(run)
        elif mode in ("local", "terminal"):
            if mode == "terminal":
                os.killpg(proc.pid, signal.SIGINT)
            else:
                proc.send_signal(signal.SIGTERM)
            assert proc.wait(timeout=15) == 130
        else:
            assert server is not None
            server.send(signal.SIGTERM)
            if mode == "forced":
                server.wait("draining", lambda: server.get("/health")["status"] == "draining")
                server.send(signal.SIGTERM)
            assert server.exited(timeout=15) == (1 if mode == "forced" else 0)
        pids = [pid for worker, child, _, _ in records for pid in (worker, child)]
        wait_until(
            lambda: all(process_state(pid) is None for pid in pids),
            f"descendants to exit: {records}",
            timeout=5,
        )
        assert unrelated.poll() is None
        for worker, _, group, child_group in records:
            assert worker == group == child_group
            assert group != os.getpgrp()
        if mode == "cancel":
            assert server is not None
            assert server.get("/health")["status"] == "ok"
            next_run = server.submit("quick", "get")["run_id"]
            assert server.finished(next_run)["status"] == "success"
    finally:
        # Also clean up the deliberately failing pre-fix run, without group signalling.
        for path in tmp_path.glob("*.json"):
            for pid in json.loads(path.read_text())[:2]:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(pid, signal.SIGCONT)
                    os.kill(pid, signal.SIGKILL)
        if server is not None:
            server.stop()
        elif "proc" in locals() and proc.poll() is None:
            proc.kill()
            proc.wait()
        unrelated.kill()
        unrelated.wait()


@pytest.mark.parametrize("parallel", [False, True])
def test_success_also_cleans_worker_descendants(tmp_path, parallel):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    if parallel:
        (tmp_path / "parallel").touch()
    records = []
    try:
        with Serve(tmp_path, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as server:
            run = server.submit("held", "get")["run_id"]
            for name in ["held", "branch"] if parallel else ["held"]:
                wait_until((tmp_path / f"{name}.ready").exists, f"{name} child ready", timeout=10)
                records.append(json.loads((tmp_path / f"{name}.json").read_text()))
            (tmp_path / "finish").touch()
            assert server.finished(run)["status"] == "success"
            wait_until(
                lambda: all(process_state(pid) is None for record in records for pid in record[:2]),
                "successful run's descendants to exit",
                timeout=5,
            )
    finally:
        for record in records:
            for pid in record[:2]:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(pid, signal.SIGCONT)
                    os.kill(pid, signal.SIGKILL)
