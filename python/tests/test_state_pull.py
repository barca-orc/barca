"""A pull of the shared state leaves the local database exactly equal to what was pulled (#221).

The metadata DB is a main file plus a write-ahead log. A pull used to replace only the main
file, so whatever the old local database still had in its log (a killed run's rows, a database
just created by `barca history`) was applied on top of the pulled one, and the next push then
dropped history other machines had added.

"Machines" here are project directories that share one state location on the local filesystem.
"""

import json
import os
import signal
import subprocess
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

# One quick step, then one that holds the run open until the test releases (or kills) it.
SLOW = """
import time
from pathlib import Path

from barca import asset


@asset()
def first() -> int:
    return 1


@asset(inputs={"x": first})
def slow(x: int) -> int:
    Path("slow.started").write_text("")
    deadline = time.time() + 60
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return x + 1
"""

WAIT = 30.0


def quick(name: str) -> str:
    return f"from barca import asset\n\n\n@asset()\ndef {name}() -> str:\n    return {name!r}\n"


class Machine:
    """A project directory using the shared state at `state_uri`."""

    def __init__(self, root: Path, state_uri: Path):
        root.mkdir()
        self.root = root
        self.env = {
            **{k: v for k, v in os.environ.items() if not k.startswith("BARCA_")},
            "BARCA_STATE_URI": str(state_uri),
            "BARCA_POOL_SIZE": "2",
        }

    def barca(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [_find_binary(), *args], cwd=self.root, capture_output=True, text=True, env=self.env
        )

    def get(self, name: str) -> str:
        """Run a one-asset pipeline to completion (it pushes); return its run id."""
        (self.root / f"{name}.py").write_text(quick(name))
        out = self.barca("get", f"{name}.py", "--json")
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)["run_id"]

    def start_slow(self) -> subprocess.Popen:
        (self.root / "slow.py").write_text(SLOW)
        proc = subprocess.Popen(
            [_find_binary(), "get", "slow.py", "--json"],
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=self.env,
        )
        deadline = time.time() + WAIT
        while not (self.root / "slow.started").exists():
            assert proc.poll() is None, proc.stderr.read()
            assert time.time() < deadline, "the slow step never started"
            time.sleep(0.05)
        return proc

    def finish_slow(self, proc: subprocess.Popen) -> str:
        (self.root / "release").write_text("")
        stdout, stderr = proc.communicate(timeout=WAIT)
        assert proc.returncode == 0, stderr
        return json.loads(stdout)["run_id"]

    def local_runs(self) -> set[str]:
        out = self.barca("history", "--all", "--json")
        assert out.returncode == 0, out.stderr
        return {r["run_id"] for r in json.loads(out.stdout)["runs"]}

    @property
    def db(self) -> Path:
        return self.root / ".barca" / "metadata.db"


@pytest.fixture()
def machines(tmp_path):
    state_uri = tmp_path / "shared" / "metadata.db"
    made: dict[str, Machine] = {}

    def machine(name: str) -> Machine:
        if name not in made:
            made[name] = Machine(tmp_path / name, state_uri)
        return made[name]

    return machine


def shared_runs(machines) -> set[str]:
    """The run ids in the shared state, read by a machine that has never run anything: a dry
    run pulls the shared DB and records nothing, and `history` then reads the pulled copy."""
    observer = machines("observer")
    (observer.root / "look.py").write_text(quick("look"))
    out = observer.barca("get", "look.py", "--dry-run", "--json")
    assert out.returncode == 0, out.stderr
    return observer.local_runs()


def test_a_run_after_a_killed_run_does_not_drop_other_machines_history(machines):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")

    # A is killed mid-run: its database is left with rows that were never pushed.
    proc = a.start_slow()
    os.kill(proc.pid, signal.SIGKILL)
    assert proc.wait(timeout=WAIT) == -signal.SIGKILL
    for pipe in (proc.stdout, proc.stderr):
        pipe.close()
    (killed,) = a.local_runs() - {a_first}

    # Meanwhile B runs and pushes.
    b_runs = {b.get(f"b_{i}") for i in range(4)}
    assert shared_runs(machines) == {a_first} | b_runs

    # A runs again: it pulls B's history, runs, and pushes.
    (a.root / "release").write_text("")
    a_last = a.get("a_two")

    assert shared_runs(machines) == {a_first, a_last} | b_runs
    # A's local database is the pulled one plus its new run. The killed run was never pushed,
    # so the pull discarded it.
    assert a.local_runs() == {a_first, a_last} | b_runs
    assert killed not in a.local_runs()
    # So the step the killed run had finished runs again.
    out = a.barca("get", "slow.py", "--json")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["steps_executed"] == 2


def test_failed_and_cancelled_runs_are_uploaded(machines):
    a = machines("a")
    (a.root / "bad.py").write_text(
        "from barca import asset\n\n\n@asset()\ndef bad() -> int:\n    raise ValueError('no')\n"
    )
    out = a.barca("get", "bad.py", "--json")
    assert out.returncode == 1, out.stderr
    failed = json.loads(out.stdout)["run_id"]

    proc = a.start_slow()
    proc.send_signal(signal.SIGINT)
    proc.communicate(timeout=WAIT)
    assert proc.returncode == 130
    (cancelled,) = a.local_runs() - {failed}

    assert shared_runs(machines) == {failed, cancelled}


def test_a_second_run_started_during_a_run_pulls_and_both_keep_their_history(machines):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")

    proc = a.start_slow()
    try:
        b_run = b.get("b_one")
        # A second run in A's project, while the first is live: it pulls B's push, runs, pushes.
        a_second = a.get("a_two")
        assert b_run in a.local_runs()
    finally:
        a_slow = a.finish_slow(proc)

    everything = {a_first, a_second, a_slow, b_run}
    assert shared_runs(machines) == everything
    assert a.local_runs() == everything


def test_a_db_created_by_a_read_command_does_not_shadow_the_pulled_one(machines):
    b, c = machines("b"), machines("c")
    b_runs = {b.get(f"b_{i}") for i in range(4)}

    # C has never run anything. `barca history` creates its local database (an empty one, still
    # in the write-ahead log) without pulling; the next run pulls underneath it.
    out = c.barca("history", "--json")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["runs"] == []
    c_run = c.get("c_one")

    assert c.local_runs() == b_runs | {c_run}
    assert shared_runs(machines) == b_runs | {c_run}


def test_read_only_commands_do_not_pull_over_a_live_run(machines):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    note = "a run is in progress in this project: not pulling the shared state"

    proc = a.start_slow()
    try:
        # Another machine pushes while A's run is going, so a pull would change A's database.
        b_run = b.get("b_one")
        before = a.db.stat().st_ino

        for args in (("status", "slow.py", "--json"), ("get", "slow.py", "--dry-run", "--json")):
            out = a.barca(*args)
            assert out.returncode == 0, out.stderr
            assert note in out.stderr, args
            assert out.stderr.count("\n") == 1, out.stderr
            json.loads(out.stdout)
            # The file the run is using is still the same file, and still lacks B's run.
            assert a.db.stat().st_ino == before, args
            assert b_run not in a.local_runs()
        assert proc.poll() is None, "the run must still be going"
    finally:
        a_slow = a.finish_slow(proc)

    # The run was not disturbed: it finished, merged with B's push, and nothing was lost.
    assert shared_runs(machines) == {a_first, a_slow, b_run}

    # With no run live, the same commands pull again, silently.
    c_run = machines("c").get("c_one")
    out = a.barca("status", "slow.py", "--json")
    assert out.returncode == 0, out.stderr
    assert note not in out.stderr
    assert c_run in a.local_runs()
