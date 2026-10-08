"""Finished steps are recorded while a run is still going (#214).

A run used to write nothing to the metadata DB until it ended, so `barca status` in another
terminal saw none of its progress, and a run killed with SIGKILL lost every step it had finished
even though the artifacts were on disk. Steps are now recorded as they finish (in batches, at
most twice a second), and the end-of-run write adds only what is missing.
"""

import json
import os
import signal
import sqlite3
import subprocess
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

# Two quick steps, then one that holds the run open until the test lets it go (or kills it).
PIPELINE = """
import time
from pathlib import Path

from barca import asset


@asset()
def first() -> int:
    Path("first.ran").open("a").write("x")
    return 1


@asset(inputs={"x": first})
def second(x: int) -> int:
    Path("second.ran").open("a").write("x")
    return x + 1


@asset(inputs={"x": second})
def slow(x: int) -> int:
    Path("slow.started").write_text("")
    deadline = time.time() + 60
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return x + 1
"""

# Ten partitions that together outlast several recording intervals.
PARTITIONED = """
import time

from barca import asset, collect, partitions


@asset(partitions={"k": partitions([str(i) for i in range(10)])})
def part(k: str) -> str:
    time.sleep(0.25)
    return k


@asset(inputs={"_p": collect(part)})
def after(_p) -> int:
    time.sleep(0.6)
    return 1
"""

WAIT = 30.0


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    env = {**os.environ, "BARCA_POOL_SIZE": "2"}
    return subprocess.run([_find_binary(), *args], cwd=cwd, capture_output=True, text=True, env=env)


def start_get(cwd: Path, *args: str) -> subprocess.Popen:
    env = {**os.environ, "BARCA_POOL_SIZE": "2"}
    return subprocess.Popen(
        [_find_binary(), "get", "pipeline.py", "--agent", *args],
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )


def states(cwd: Path) -> dict[str, str]:
    out = barca(cwd, "status", "pipeline.py", "--json")
    assert out.returncode == 0, out.stderr
    return {n["name"]: n["cache"]["state"] for n in json.loads(out.stdout)["nodes"]}


def latest_run(cwd: Path) -> dict:
    out = barca(cwd, "history", "--json")
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout)["runs"][0]


def wait_for(predicate, what: str):
    deadline = time.time() + WAIT
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    pytest.fail(f"timed out after {WAIT:.0f}s waiting for {what}")


def wait_until_slow_is_running(cwd: Path) -> None:
    wait_for(lambda: (cwd / "slow.started").exists(), "the slow step to start")


def finish(proc: subprocess.Popen, cwd: Path) -> None:
    (cwd / "release").write_text("")
    _, stderr = proc.communicate(timeout=WAIT)
    assert proc.returncode == 0, stderr


def test_status_from_a_second_process_shows_steps_a_running_get_has_finished(project):
    proc = start_get(project)
    try:
        wait_until_slow_is_running(project)
        seen = wait_for(
            lambda: (s := states(project))["second"] == "cached" and s,
            "status to show the finished steps",
        )
        assert proc.poll() is None, "the run must still be going when status is read"
        assert seen == {"first": "cached", "second": "cached", "slow": "never_run"}

        # The run is honest about being unfinished, and says how far it has got.
        run = latest_run(project)
        assert run["status"] == "running"
        assert run["steps_executed"] == 2
        assert run["steps_total"] == 3
        assert run["finished_at"] is None
    finally:
        finish(proc, project)

    run = latest_run(project)
    assert (run["status"], run["steps_executed"], run["steps_cached"]) == ("success", 3, 0)
    assert set(states(project).values()) == {"cached"}


def test_a_run_killed_with_sigkill_keeps_the_steps_it_finished(project):
    proc = start_get(project)
    wait_until_slow_is_running(project)
    wait_for(lambda: states(project)["second"] == "cached", "the finished steps to be recorded")
    os.kill(proc.pid, signal.SIGKILL)
    # wait(), not communicate(): the orphaned worker still holds the pipes until it notices.
    assert proc.wait(timeout=WAIT) == -signal.SIGKILL
    for pipe in (proc.stdout, proc.stderr):
        pipe.close()

    # Nobody saw the run end: it is reported as interrupted, with no finish time.
    run = latest_run(project)
    assert run["status"] == "interrupted"
    assert run["steps_executed"] == 2
    assert run["finished_at"] is None
    assert run["elapsed_seconds"] is None

    # Every recorded step points at an artifact that exists.
    status = json.loads(barca(project, "status", "pipeline.py", "--json").stdout)
    for node in status["nodes"]:
        if node["cache"]["state"] == "cached":
            assert (project / node["cache"]["artifact"]).is_file()

    # The next run reuses them and computes only what was left.
    (project / "release").write_text("")
    out = barca(project, "get", "pipeline.py", "--json")
    assert out.returncode == 0, out.stderr
    result = json.loads(out.stdout)
    by_step = {s["id"].split(":")[-1]: s["status"] for s in result["steps"]}
    assert by_step == {"first": "cached", "second": "cached", "slow": "ran"}
    assert result["steps_executed"] == 1
    assert (project / "first.ran").read_text() == "x"
    assert (project / "second.ran").read_text() == "x"

    # History keeps both runs, each with its own outcome.
    runs = json.loads(barca(project, "history", "--json").stdout)["runs"]
    assert [r["status"] for r in runs] == ["success", "interrupted"]


def test_a_step_recorded_mid_run_is_not_recorded_again_at_the_end(project):
    proc = start_get(project)
    try:
        wait_until_slow_is_running(project)
        wait_for(lambda: states(project)["second"] == "cached", "the finished steps")
    finally:
        finish(proc, project)

    for name in ("first", "second", "slow"):
        out = barca(project, "stats", name, "pipeline.py", "--json")
        assert out.returncode == 0, out.stderr
        assert json.loads(out.stdout)["total_runs"] == 1, name


def test_status_counts_the_partitions_a_running_get_has_finished(tmp_path):
    (tmp_path / "pipeline.py").write_text(PARTITIONED)

    def partitions_of_part() -> dict:
        out = barca(tmp_path, "status", "pipeline.py", "--json")
        assert out.returncode == 0, out.stderr
        nodes = {n["name"]: n for n in json.loads(out.stdout)["nodes"]}
        return nodes["part"]["partitions"]

    proc = start_get(tmp_path)
    try:
        # Some partitions recorded, and the run is still going (it has `after` left at least).
        mid = wait_for(
            lambda: (p := partitions_of_part())["cached"] > 0 and proc.poll() is None and p,
            "status to count finished partitions mid-run",
        )
        assert mid["total"] == 10
    finally:
        _, stderr = proc.communicate(timeout=WAIT)
    assert proc.returncode == 0, stderr

    assert partitions_of_part()["cached"] == 10
    run = latest_run(tmp_path)
    assert (run["status"], run["steps_executed"]) == ("success", 11)
    again = json.loads(barca(tmp_path, "get", "pipeline.py", "--json").stdout)
    assert again["steps_executed"] == 0


def test_a_cancelled_run_is_recorded_as_cancelled_not_interrupted(project):
    proc = start_get(project)
    wait_until_slow_is_running(project)
    wait_for(lambda: states(project)["second"] == "cached", "the finished steps")
    proc.send_signal(signal.SIGINT)
    proc.communicate(timeout=WAIT)
    assert proc.returncode == 130

    run = latest_run(project)
    assert run["status"] == "cancelled"
    assert run["finished_at"] is not None
    for name in ("first", "second"):
        stats = json.loads(barca(project, "stats", name, "pipeline.py", "--json").stdout)
        assert stats["total_runs"] == 1, name


def test_sigterm_cancels_a_run_the_way_ctrl_c_does(project):
    """A supervisor, a CI timeout and `docker stop` send SIGTERM. It used to kill barca at
    once: no exit code of its own, workers left behind, and the run `running` in history until
    a later command reported it `interrupted` (#289)."""
    proc = start_get(project)
    wait_until_slow_is_running(project)
    wait_for(lambda: states(project)["second"] == "cached", "the finished steps")
    proc.send_signal(signal.SIGTERM)
    _, stderr = proc.communicate(timeout=WAIT)
    # 130, the exit code of `cancelled`: not -15 (killed by the signal) and not 143.
    assert proc.returncode == 130, stderr
    envelope = json.loads(stderr.strip().splitlines()[-1])
    assert (envelope["kind"], envelope["code"]) == ("cancelled", 130)
    assert "[barca] 2/3 steps | cancelled after" in stderr

    run = latest_run(project)
    assert run["status"] == "cancelled"
    assert run["finished_at"] is not None
    # The steps that finished are kept, as after Ctrl-C.
    assert states(project) == {"first": "cached", "second": "cached", "slow": "never_run"}


def run_rows(cwd: Path) -> dict[str, tuple]:
    db = sqlite3.connect(cwd / ".barca" / "metadata.db")
    try:
        return {
            r[0]: r[1:] for r in db.execute("select run_id, status, pid, host, owner from runs")
        }
    finally:
        db.close()


def a_dead_pid() -> int:
    child = subprocess.Popen(["true"])
    child.wait()
    return child.pid


def test_a_run_killed_in_a_container_is_interrupted_after_a_restart(project):
    """In a container the coordinator is process 1, the next start is process 1 again, and the
    host name is new on every start. Judged by pid and host, a killed run looked alive and
    foreign, and stayed `running` for ever (#290)."""
    proc = start_get(project)
    wait_until_slow_is_running(project)
    wait_for(lambda: states(project)["second"] == "cached", "the finished steps to be recorded")
    os.kill(proc.pid, signal.SIGKILL)
    assert proc.wait(timeout=WAIT) == -signal.SIGKILL
    for pipe in (proc.stdout, proc.stderr):
        pipe.close()

    # What the restarted container sees: the pid the run recorded belongs to a live process
    # (this one stands for the new process 1) and the host name is not its own.
    (run_id, (_, _, _, owner)) = next(iter(run_rows(project).items()))
    assert owner, "the run recorded its owner"
    db = sqlite3.connect(project / ".barca" / "metadata.db")
    with db:
        db.execute("update runs set pid = ?, host = 'the-previous-container'", (os.getpid(),))
    db.close()
    assert run_rows(project)[run_id][:3] == ("running", os.getpid(), "the-previous-container")

    run = latest_run(project)
    assert run["status"] == "interrupted"
    assert run["finished_at"] is None

    # The next run records it, reuses what the killed one finished, and removes the dead
    # process's lock file once no running row names it.
    (project / "release").write_text("")
    out = barca(project, "get", "pipeline.py", "--json")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["steps_executed"] == 1
    assert run_rows(project)[run_id][0] == "interrupted"
    runs = json.loads(barca(project, "history", "--json").stdout)["runs"]
    assert [r["status"] for r in runs] == ["success", "interrupted"]
    assert not (project / ".barca" / "run-owners" / f"{owner}.lock").exists()


def test_a_running_run_started_somewhere_else_is_left_alone(project):
    """With shared history the database also holds other machines' runs, and one of them may
    be going right now. A run whose owner never held its lock in this directory is not ours
    to judge, even when the other machine has this machine's host name and the pid it
    recorded is dead here (by pid and host alone that read as interrupted)."""
    out = barca(project, "get", "first", "pipeline.py", "--json")
    assert out.returncode == 0, out.stderr
    host = next(iter(run_rows(project).values()))[2]
    db = sqlite3.connect(project / ".barca" / "metadata.db")
    with db:
        db.execute(
            "insert into runs (run_id, command, files, status, pid, host, owner)"
            " values ('theirs', 'get', '[\"pipeline.py\"]', 'running', ?, ?, ?)",
            (a_dead_pid(), host, "0123456789abcdef0123456789abcdef"),
        )
    db.close()

    def status_of_theirs() -> str:
        runs = json.loads(barca(project, "history", "--json").stdout)["runs"]
        return next(r["status"] for r in runs if r["run_id"] == "theirs")

    assert status_of_theirs() == "running"
    out = barca(project, "get", "second", "pipeline.py", "--json")
    assert out.returncode == 0, out.stderr
    assert run_rows(project)["theirs"][0] == "running"
    assert status_of_theirs() == "running"
