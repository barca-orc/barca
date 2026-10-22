"""Debugging a live pipeline: stderr capture, timestamps, `barca logs -f`, the per-run cap
and logs read from a server (`barca docs logs`)."""

import json
import os
import queue
import re
import subprocess
import threading
import time
from pathlib import Path

import pytest
from barca.api import _find_binary

from .test_serve_robustness import Server, wait_for

TS = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z")

PIPELINE = """
import logging
import sys
import time
import warnings
from pathlib import Path

from barca import asset, task

logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")
log = logging.getLogger("pipeline")


@asset()
def orders():
    print("loading orders")
    log.info("fetched %d orders", 3)
    warnings.warn("old API")
    print("to stderr", file=sys.stderr)
    return [1, 2, 3]


@task(inputs={"orders": orders})
def report(orders):
    log.warning("reporting %d", len(orders))
    raise ValueError("bad report")


@task()
def gated():
    print("first")
    log.info("waiting")
    deadline = time.time() + 60
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    print("second")
    return 1


@task()
def flood():
    for _ in range(17):
        print("x" * (1024 * 1024))
    print("after the cap")
    return 1
"""

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")


def _env(**extra):
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["PYTHONPATH"] = str(Path(__file__).resolve().parents[1])
    env.update(extra)
    return env


@pytest.fixture
def project(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


def cli(project, *args, code=0, **env):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env=_env(**env),
        text=True,
        capture_output=True,
        timeout=60,
    )
    assert proc.returncode == code, proc.stderr
    return proc


def lines_of(rows, stream=None):
    return [row["line"] for row in rows if stream is None or row["stream"] == stream]


class Reader:
    """A child process whose stdout lines are read as they arrive."""

    def __init__(self, project, *args, **env):
        self.proc = subprocess.Popen(
            [_find_binary(), *args],
            cwd=project,
            env=_env(**env),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.lines: queue.Queue = queue.Queue()
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        for line in self.proc.stdout:
            self.lines.put(line.rstrip("\n"))

    def next(self, timeout=30.0):
        return self.lines.get(timeout=timeout)

    def finish(self, timeout=30.0):
        code = self.proc.wait(timeout=timeout)
        rest = []
        while not self.lines.empty():
            rest.append(self.lines.get())
        return code, rest, self.proc.stderr.read()


def test_stderr_is_recorded_per_step_with_times_and_still_shown_live(project):
    run = cli(project, "get", "orders", "pipeline.py", "--json", "--agent")
    doc = json.loads(run.stdout)
    # As before: stderr is written as it comes; stdout is not, in JSON mode.
    assert "INFO pipeline: fetched 3 orders" in run.stderr
    assert "UserWarning: old API" in run.stderr
    assert "loading orders" not in run.stderr
    assert "[barca] importing" in run.stderr
    rows = json.loads(cli(project, "logs", doc["run_id"], "--json").stdout)
    assert all(TS.fullmatch(row["ts"]) for row in rows)
    assert {row["node_id"] for row in rows} == {"pipeline.py:orders"}
    assert lines_of(rows, "stdout") == ["loading orders"]
    stderr = lines_of(rows, "stderr")
    assert stderr[0] == "INFO pipeline: fetched 3 orders"
    assert stderr[1].endswith("UserWarning: old API")
    assert stderr[-1] == "to stderr"
    # barca's own lines are not the step's.
    assert not any(line.startswith("[barca]") for line in lines_of(rows))

    only = json.loads(cli(project, "logs", doc["run_id"], "--stream", "stderr", "--json").stdout)
    assert only == [row for row in rows if row["stream"] == "stderr"]
    text = cli(project, "logs", doc["run_id"], "--stream", "stdout", "--pretty").stdout
    assert text == "loading orders\n"
    stamped = cli(project, "logs", doc["run_id"], "--timestamps", "--pretty").stdout.splitlines()
    assert [line.split(" ", 1)[1] for line in stamped] == lines_of(rows)
    assert all(TS.fullmatch(line.split(" ", 1)[0]) for line in stamped)
    cli(project, "logs", doc["run_id"], "--stream", "both", code=2)


def test_a_failed_step_records_its_traceback_as_its_stderr(project):
    failed = cli(project, "run", "report", "pipeline.py", "--json", code=1)
    run_id = json.loads(failed.stdout)["run_id"]
    text = cli(project, "logs", run_id, "--step", "report", "--stream", "stderr", "--pretty")
    lines = text.stdout.splitlines()
    assert lines[0] == "WARNING pipeline: reporting 3"
    assert lines[1] == "Traceback (most recent call last):"
    assert 'raise ValueError("bad report")' in text.stdout
    assert lines[-1] == "ValueError: bad report"


def test_follow_prints_lines_as_they_arrive_and_exits_with_the_run(project):
    run = subprocess.Popen(
        [_find_binary(), "run", "gated", "pipeline.py", "--json"],
        cwd=project,
        env=_env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        running = wait_for(
            lambda: [
                r
                for r in json.loads(cli(project, "history", "--json").stdout)["runs"]
                if r["status"] == "running"
            ],
            "the run to start",
        )
        run_id = running[0]["run_id"]
        # Without a run id, -f follows the newest running run.
        follower = Reader(project, "logs", "-f", "--json")
        first = json.loads(follower.next())
        assert (first["line"], first["stream"]) == ("first", "stdout")
        assert TS.fullmatch(first["ts"])
        assert json.loads(follower.next())["line"] == "INFO pipeline: waiting"
        # The run is still waiting, so these came from the spool, not the database.
        assert run.poll() is None
        spool = project / ".barca" / "live-logs" / f"{run_id}.jsonl"
        assert spool.exists()
        (project / "release").touch()
        assert json.loads(follower.next())["line"] == "second"
        code, rest, stderr = follower.finish()
        assert code == 0, stderr
        assert rest == []
        assert f"following the newest running run: {run_id}" in stderr
        assert f"run {run_id} ended: success" in stderr
        assert run.wait(timeout=30) == 0
        assert not spool.exists()
        # A finished run: printed at once, and the run's exit code.
        done = cli(project, "logs", run_id, "-f", "--pretty")
        assert done.stdout == "first\nINFO pipeline: waiting\nsecond\n"
        failed = cli(project, "run", "report", "pipeline.py", "--json", code=1)
        ended = cli(project, "logs", json.loads(failed.stdout)["run_id"], "-f", "--pretty", code=1)
        assert "ended: failed" in ended.stderr
    finally:
        (project / "release").touch()
        if run.poll() is None:
            run.kill()
        run.wait(timeout=30)


def test_a_run_records_at_most_its_cap(project):
    doc = json.loads(cli(project, "run", "flood", "pipeline.py", "--json").stdout)
    rows = json.loads(cli(project, "logs", doc["run_id"], "--json").stdout)
    assert len(rows) == 17
    assert rows[-1]["line"].startswith("[barca] log cap reached: this run recorded 16 MiB")
    assert rows[-1]["stream"] == "stderr"
    assert "after the cap" not in lines_of(rows)


def test_logs_of_a_server_run_and_following_it(project):
    server = Server(project, "--no-schedule")
    url = f"http://127.0.0.1:{server.port}"
    try:
        doc = json.loads(
            cli(project, "get", "orders", "pipeline.py", "--server", url, "--json").stdout
        )
        run_id = doc["run_id"]
        status, body = server.request("GET", f"/runs/{run_id}/logs?stream=stderr")
        assert status == 200 and body["status"] == "success"
        assert lines_of(body["logs"])[0] == "INFO pipeline: fetched 3 orders"
        # `barca logs --server` prints what `barca logs` prints where the run ran.
        remote = cli(project, "logs", run_id, "--server", url, "--json")
        local = cli(project, "logs", run_id, "--local", "--json")
        assert remote.stdout == local.stdout
        assert lines_of(json.loads(remote.stdout), "stdout") == ["loading orders"]
        # A configured server answers without --server, and says so; --local reads here.
        configured = cli(project, "logs", "--pretty", "--stream", "stdout", BARCA_SERVER=url)
        assert f"[barca] server {url} (BARCA_SERVER)" in configured.stderr
        assert configured.stdout == "loading orders\n"
        cli(project, "logs", "nope", "--server", url, code=2)

        # Follow a run the server is running.
        started = Reader(project, "run", "gated", "pipeline.py", "--server", url, "--json")
        wait_for(
            lambda: any(
                r["status"] == "running"
                for r in json.loads(cli(project, "history", "--server", url, "--json").stdout)[
                    "runs"
                ]
            ),
            "the server run to start",
        )
        follower = Reader(project, "logs", "--server", url, "-f", "--pretty")
        assert follower.next() == "first"
        assert follower.next() == "INFO pipeline: waiting"
        (project / "release").touch()
        assert follower.next() == "second"
        code, rest, stderr = follower.finish()
        assert (code, rest) == (0, []), stderr
        assert "following the newest running run" in stderr
        assert started.finish()[0] == 0
    finally:
        (project / "release").touch()
        server.stop()


HELLO = """
from barca import task


@task()
def hello():
    print("hi from {where}")
    return 1
"""


def test_a_configured_server_points_at_runs_of_this_machine(tmp_path):
    """The server answers, with no fallback; a run only this machine has is named, with the
    command that reads it."""
    roots = {}
    for where in ("server", "laptop"):
        roots[where] = tmp_path / where
        roots[where].mkdir()
        (roots[where] / "pipeline.py").write_text(HELLO.format(where=where))
        (roots[where] / "barca.toml").write_text("")
    cli(roots["server"], "run", "hello", "pipeline.py", "--json")
    server = Server(roots["server"], "--no-schedule")
    url = f"http://127.0.0.1:{server.port}"
    laptop = roots["laptop"]
    try:
        time.sleep(1.1)  # start times are recorded to the second
        mine = json.loads(cli(laptop, "run", "hello", "pipeline.py", "--json", "--local").stdout)
        run_id = mine["run_id"]
        hint = f"this run is on this machine: barca logs {run_id} --local"
        # 1. A run id the server does not have, but this machine does: the same exit code,
        # with the command as the remediation, with or without --step and -f.
        for extra in ([], ["--step", "hello"], ["-f"]):
            failed = cli(laptop, "logs", run_id, "--json", *extra, code=2, BARCA_SERVER=url)
            envelope = json.loads(failed.stderr.splitlines()[-1])
            assert envelope["remediation"] == hint, failed.stderr
        text = cli(laptop, "logs", run_id, "--pretty", code=2, BARCA_SERVER=url)
        assert hint in text.stderr
        # A run neither has: the server's error as it is.
        unknown = cli(laptop, "logs", "feedfacecafe", "--json", code=2, BARCA_SERVER=url)
        assert "this machine" not in unknown.stderr
        assert cli(laptop, "logs", run_id, "--local", "--pretty").stdout == "hi from laptop\n"

        # 2. No run id: the server's newest run, and one note naming this machine's newer one.
        note = (
            f"[barca] note: this machine has a newer run {run_id} (run hello, success); "
            "barca logs --local reads it"
        )
        for extra in ([], ["-f"]):
            read = cli(laptop, "logs", "--json", *extra, BARCA_SERVER=url)
            assert read.stderr.splitlines().count(note) == 1, read.stderr
            assert "hi from server" in read.stdout
        plain = cli(laptop, "logs", "--json", BARCA_SERVER=url)
        assert json.loads(plain.stdout)[0]["line"] == "hi from server"
        # Not with --server <url>, and not when the server's run is the newer one.
        assert note not in cli(laptop, "logs", "--server", url, "--json").stderr
        time.sleep(1.1)
        cli(laptop, "run", "hello", "pipeline.py", "--server", url, "--json")
        assert "[barca] note" not in cli(laptop, "logs", "--json", BARCA_SERVER=url).stderr
    finally:
        server.stop()
