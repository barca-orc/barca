"""What a pull of the shared state does to the local database (#221, RFC-0006 section 4.1).

Two things must hold at once:

- A pull never lets stale local state overwrite or drop history other machines pushed. The
  metadata DB is a main file plus a write-ahead log; a pull used to replace only the main file,
  so whatever the old local database still had in its log (a killed run's rows, a database just
  created by `barca history`) was applied on top of the pulled one, and the next push then
  dropped other machines' runs.
- Local rows that were never pushed are not lost by a pull: a killed run's row and the steps it
  had recorded are on top of the pulled database afterwards, so the next run reuses them, and
  they reach the shared copy with the next push.

"Machines" here are project directories that share one state location on the local filesystem.
"""

import json
import os
import signal
import sqlite3
import subprocess
import sys
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
    Path("first.ran").open("a").write("x")
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
KEPT_THE_KILLED_RUN = "kept 1 run and 1 finished step recorded only on this machine"


def quick(name: str) -> str:
    return f"from barca import asset\n\n\n@asset()\ndef {name}() -> str:\n    return {name!r}\n"


def wait_for(predicate, what: str):
    deadline = time.time() + WAIT
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    pytest.fail(f"timed out after {WAIT:.0f}s waiting for {what}")


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

    def barca(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [_find_binary(), *args],
            cwd=self.root,
            capture_output=True,
            text=True,
            env={**self.env, **env},
        )

    def get(self, name: str, **env: str) -> str:
        """Run a one-asset pipeline to completion; return its run id."""
        (self.root / f"{name}.py").write_text(quick(name))
        out = self.barca("get", f"{name}.py", "--json", **env)
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)["run_id"]

    def start_slow(self) -> subprocess.Popen:
        """Start the two-step pipeline and return once its first step is recorded in the local
        database and its second is running."""
        (self.root / "slow.py").write_text(SLOW)
        proc = subprocess.Popen(
            [_find_binary(), "get", "slow.py", "--json"],
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=self.env,
        )

        def recorded():
            assert proc.poll() is None, proc.stderr.read()
            # `history` reads the local database as it is; it does not pull.
            runs = self.history()
            return (
                (self.root / "slow.started").exists()
                and runs
                and runs[0]["status"] == "running"
                and runs[0]["steps_executed"] == 1
            )

        wait_for(recorded, "the run to record its first step")
        return proc

    def kill(self, proc: subprocess.Popen) -> str:
        """SIGKILL a run started by `start_slow`; return its run id."""
        run_id = self.history()[0]["run_id"]
        os.kill(proc.pid, signal.SIGKILL)
        assert proc.wait(timeout=WAIT) == -signal.SIGKILL
        for pipe in (proc.stdout, proc.stderr):
            pipe.close()
        return run_id

    def finish_slow(self, proc: subprocess.Popen) -> str:
        (self.root / "release").write_text("")
        stdout, stderr = proc.communicate(timeout=WAIT)
        assert proc.returncode == 0, stderr
        return json.loads(stdout)["run_id"]

    def resume_slow(self) -> subprocess.CompletedProcess:
        """Run the two-step pipeline again, letting the slow step through."""
        (self.root / "release").write_text("")
        out = self.barca("get", "slow.py", "--json")
        assert out.returncode == 0, out.stderr
        return out

    def history(self) -> list[dict]:
        """The local database's runs, newest first."""
        out = self.barca("history", "--all", "--json")
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)["runs"]

    def local_runs(self) -> set[str]:
        runs = [r["run_id"] for r in self.history()]
        assert len(runs) == len(set(runs)), f"a run is recorded twice: {runs}"
        return set(runs)

    def states(self, file: str = "slow.py") -> dict[str, str]:
        out = self.barca("status", file, "--json")
        assert out.returncode == 0, out.stderr
        return {n["name"]: n["cache"]["state"] for n in json.loads(out.stdout)["nodes"]}

    @property
    def db(self) -> Path:
        return self.root / ".barca" / "metadata.db"


@pytest.fixture()
def state_uri(tmp_path) -> Path:
    return tmp_path / "shared" / "metadata.db"


@pytest.fixture()
def machines(tmp_path, state_uri):
    made: dict[str, Machine] = {}

    def machine(name: str) -> Machine:
        if name not in made:
            made[name] = Machine(tmp_path / name, state_uri)
        return made[name]

    return machine


def shared(state_uri: Path, sql: str) -> list[tuple]:
    """Query the shared state itself: the blob is a complete SQLite file."""
    with sqlite3.connect(f"file:{state_uri}?mode=ro", uri=True) as conn:
        assert conn.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        return conn.execute(sql).fetchall()


def shared_runs(state_uri: Path) -> dict[str, str]:
    """run id -> status in the shared state; fails if a run or a step is there twice."""
    rows = shared(state_uri, "SELECT run_id, status FROM runs")
    assert len(rows) == len({r for r, _ in rows}), f"a run is in the shared state twice: {rows}"
    steps = shared(
        state_uri,
        "SELECT run_id, node_id, COUNT(*) FROM materializations "
        "WHERE run_id IS NOT NULL GROUP BY run_id, node_id HAVING COUNT(*) > 1",
    )
    assert steps == [], f"a step is in the shared state twice: {steps}"
    return dict(rows)


def no_pull_leftovers(machine: Machine) -> None:
    names = sorted(p.name for p in machine.db.parent.iterdir())
    assert [n for n in names if ".pull-" in n or ".push-" in n or ".tmp" in n] == [], names


def test_a_killed_run_is_resumed_and_other_machines_history_is_kept(machines, state_uri):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")

    # A is killed mid-run. Its run row and its finished step are in A's local database only.
    killed = a.kill(a.start_slow())
    assert killed not in shared_runs(state_uri)

    # Meanwhile B runs and pushes.
    b_runs = {b.get(f"b_{i}") for i in range(4)}
    assert set(shared_runs(state_uri)) == {a_first} | b_runs

    # A runs again. It pulls B's history, keeps what the killed run recorded, so the finished
    # step is served from cache and only the unfinished one runs.
    out = a.resume_slow()
    assert KEPT_THE_KILLED_RUN in out.stderr, out.stderr
    result = json.loads(out.stdout)
    assert result["steps_executed"] == 1, result
    assert {s["id"].split(":")[-1]: s["status"] for s in result["steps"]} == {
        "first": "cached",
        "slow": "ran",
    }
    assert (a.root / "first.ran").read_text() == "x", "the finished step must not run again"

    # Nothing of B's is lost, and the killed run is now in the shared history, as interrupted.
    everything = {a_first, killed, result["run_id"]} | b_runs
    in_shared = shared_runs(state_uri)
    assert set(in_shared) == everything
    assert in_shared[killed] == "interrupted"
    assert a.local_runs() == everything
    no_pull_leftovers(a)

    # Another machine sees all of it after its next pull, and a later pull on A adds nothing.
    b_last = b.get("b_last")
    assert b.local_runs() == everything | {b_last}
    a_last = a.get("a_last")
    assert set(shared_runs(state_uri)) == everything | {b_last, a_last}


def test_the_scenario_of_issue_221_ends_with_every_run_in_the_shared_state(machines, state_uri):
    # A runs and pushes; A is killed mid-run; B does six runs, each pushed; A runs again; one
    # more run on B. On 0.17.0 the shared state ends with 4 runs: B's six are gone.
    a, b = machines("a"), machines("b")
    a.get("other1")
    killed = a.kill(a.start_slow())
    for i in range(1, 7):
        b.get(f"other{i}")
    a.resume_slow()
    b.get("other1")

    runs = shared_runs(state_uri)
    assert len(runs) == 10, runs
    assert runs[killed] == "interrupted"
    assert sorted(runs.values()) == ["interrupted"] + ["success"] * 9
    assert a.local_runs() <= set(runs)
    assert b.local_runs() == set(runs)


def test_a_killed_runs_step_whose_artifact_is_gone_is_not_a_cache_hit(machines, state_uri):
    a, b = machines("a"), machines("b")
    killed = a.kill(a.start_slow())
    artifacts = [p for p in (a.root / ".barca" / "artifacts").rglob("*") if p.is_file()]
    assert len(artifacts) == 1, artifacts
    artifacts[0].unlink()
    b.get("b_one")

    # The run is kept; its step is not, because there is no result behind it any more.
    out = a.resume_slow()
    assert "kept 1 run recorded only on this machine" in out.stderr, out.stderr
    assert "left out 1 step whose result file is no longer here" in out.stderr, out.stderr
    assert json.loads(out.stdout)["steps_executed"] == 2
    assert (a.root / "first.ran").read_text() == "xx"
    assert killed in shared_runs(state_uri)
    assert shared(
        state_uri, f"SELECT COUNT(*) FROM materializations WHERE run_id = '{killed}'"
    ) == [(0,)]


def test_failed_and_cancelled_runs_are_uploaded(machines, state_uri):
    a = machines("a")
    (a.root / "bad.py").write_text(
        "from barca import asset\n\n\n@asset()\ndef bad() -> int:\n    raise ValueError('no')\n"
    )
    out = a.barca("get", "bad.py", "--json")
    assert out.returncode == 1, out.stderr
    failed = json.loads(out.stdout)["run_id"]

    proc = a.start_slow()
    cancelled = a.history()[0]["run_id"]
    proc.send_signal(signal.SIGINT)
    proc.communicate(timeout=WAIT)
    assert proc.returncode == 130

    assert shared_runs(state_uri) == {failed: "failed", cancelled: "cancelled"}


def test_a_second_run_started_during_a_run_pulls_and_both_keep_their_history(machines, state_uri):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")

    proc = a.start_slow()
    try:
        live = a.history()[0]["run_id"]
        b_run = b.get("b_one")
        # A second run in A's project, while the first is live: it pulls B's push, runs, pushes.
        a_second = a.get("a_two")
        # The pull brought B's run in and left the live run's row and finished step in place.
        assert a.local_runs() == {a_first, a_second, live, b_run}
        assert {r["run_id"]: r["status"] for r in a.history()}[live] == "running"
        assert proc.poll() is None, "the first run must still be going"
    finally:
        a_slow = a.finish_slow(proc)
    assert a_slow == live

    everything = {a_first, a_second, a_slow, b_run}
    in_shared = shared_runs(state_uri)
    assert set(in_shared) == everything
    assert in_shared[a_slow] == "success"
    assert a.local_runs() == everything
    # The first run's steps are recorded once each, although the second run pushed the one it
    # had finished by then.
    assert shared(
        state_uri,
        f"SELECT node_id FROM materializations WHERE run_id = '{a_slow}' ORDER BY node_id",
    ) == [("slow.py:first",), ("slow.py:slow",)]


def test_a_db_created_by_a_read_command_does_not_shadow_the_pulled_one(machines, state_uri):
    b, c = machines("b"), machines("c")
    b_runs = {b.get(f"b_{i}") for i in range(4)}

    # C has never run anything. `barca history` creates its local database (an empty one, still
    # in the write-ahead log) without pulling; the next run pulls underneath it.
    out = c.barca("history", "--json")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["runs"] == []
    c_run = c.get("c_one")

    assert c.local_runs() == b_runs | {c_run}
    assert set(shared_runs(state_uri)) == b_runs | {c_run}


def test_status_and_dry_run_during_a_run_pull_and_keep_the_runs_progress(machines, state_uri):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")

    proc = a.start_slow()
    try:
        live = a.history()[0]["run_id"]
        # Another machine pushes while A's run is going.
        b_run = b.get("b_one")
        assert b_run not in a.local_runs()

        for args in (("status", "slow.py", "--json"), ("get", "slow.py", "--dry-run", "--json")):
            out = a.barca(*args)
            assert out.returncode == 0, out.stderr
            json.loads(out.stdout)
            # They pulled: B's run is in A's local history now. And the live run's row and
            # the step it has finished are still there.
            assert a.local_runs() == {a_first, live, b_run}, args
            assert a.states() == {"first": "cached", "slow": "never_run"}, args
        assert proc.poll() is None, "the run must still be going"
        assert a.history()[0]["status"] == "running"
    finally:
        a_slow = a.finish_slow(proc)

    # The run was not disturbed: it finished, merged with B's push, and nothing is there twice.
    assert a_slow == live
    assert shared_runs(state_uri) == {a_first: "success", a_slow: "success", b_run: "success"}
    assert (a.root / "first.ran").read_text() == "x"
    no_pull_leftovers(a)


def test_with_state_off_nothing_is_pulled_or_pushed_and_the_rows_are_carried_later(
    machines, state_uri
):
    a, b = machines("a"), machines("b")
    off = {"BARCA_STATE": "off"}

    # state = "off": local history only, exactly as without a shared state.
    a_off_1 = a.get("a_one", **off)
    assert not state_uri.exists()
    b_run = b.get("b_one")
    a_off_2 = a.get("a_two", **off)
    assert a.local_runs() == {a_off_1, a_off_2}
    assert set(shared_runs(state_uri)) == {b_run}
    no_pull_leftovers(a)
    # A killed run resumes from the local database, which nothing replaced.
    env, a.env = a.env, {**a.env, **off}
    try:
        killed = a.kill(a.start_slow())
        out = a.resume_slow()
    finally:
        a.env = env
    assert json.loads(out.stdout)["steps_executed"] == 1
    assert "kept" not in out.stderr and "pulled state" not in out.stderr
    local_only = {a_off_1, a_off_2, killed, json.loads(out.stdout)["run_id"]}
    assert a.local_runs() == local_only
    assert set(shared_runs(state_uri)) == {b_run}

    # Sharing switched on: the first pull keeps the runs made while it was off, and the push
    # at the end of that run shares them.
    a_on = a.get("a_three")
    assert a.local_runs() == local_only | {b_run, a_on}
    assert set(shared_runs(state_uri)) == local_only | {b_run, a_on}


def test_each_environment_carries_its_own_unpushed_rows(tmp_path):
    # With a store, each environment has its own shared state and its own local database.
    store = tmp_path / "store"

    def machine(name: str) -> Machine:
        m = Machine(tmp_path / name, tmp_path / "unused")
        del m.env["BARCA_STATE_URI"]
        m.env["BARCA_REMOTE_URI"] = str(store)
        return m

    def get(m: Machine, name: str, *args: str, **env: str) -> str:
        (m.root / f"{name}.py").write_text(quick(name))
        out = m.barca("get", f"{name}.py", "--json", *args, **env)
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)["run_id"]

    a, b = machine("a"), machine("b")
    staging, default = (store / env / "state" / "metadata.db" for env in ("staging", "default"))

    # Runs recorded locally only, one in each environment.
    default_local = get(a, "one", BARCA_STATE="off")
    staging_local = get(a, "one", "--env", "staging", BARCA_STATE="off")
    assert not staging.exists() and not default.exists()
    # Another machine creates the shared state of `staging`.
    b_staging = get(b, "zero", "--env", "staging")

    # A's next run in `staging` pulls it and carries over the staging run only.
    staging_run = get(a, "two", "--env", "staging")
    assert set(shared_runs(staging)) == {b_staging, staging_local, staging_run}
    assert not default.exists()

    # `default` has no shared state yet: A's local history there becomes it, as before.
    default_run = get(a, "two")
    assert set(shared_runs(default)) == {default_local, default_run}
    assert set(shared_runs(staging)) == {b_staging, staging_local, staging_run}


@pytest.mark.skipif(os.geteuid() == 0, reason="root can write to a read-only directory")
def test_a_run_whose_upload_failed_is_uploaded_by_the_next_run(machines, state_uri):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")

    # The upload at the end of A's next run fails: the shared location is not writable.
    (a.root / "a_two.py").write_text(quick("a_two"))
    for path in (state_uri.parent, state_uri):
        path.chmod(0o555 if path.is_dir() else 0o444)
    try:
        out = a.barca("get", "a_two.py", "--json")
    finally:
        state_uri.parent.chmod(0o755)
        state_uri.chmod(0o644)
    assert out.returncode == 3, out.stderr
    assert "shared state push" in out.stderr
    (not_uploaded,) = a.local_runs() - {a_first}
    assert set(shared_runs(state_uri)) == {a_first}

    b_run = b.get("b_one")
    (a.root / "a_three.py").write_text(quick("a_three"))
    out = a.barca("get", "a_three.py", "--json")
    assert out.returncode == 0, out.stderr
    assert "kept 1 run and 1 finished step recorded only on this machine" in out.stderr
    everything = {a_first, not_uploaded, b_run, json.loads(out.stdout)["run_id"]}
    assert shared_runs(state_uri) == dict.fromkeys(everything, "success")
    # Its result is still a cache hit.
    out = a.barca("get", "a_two.py", "--json")
    assert json.loads(out.stdout)["steps_executed"] == 0, out.stdout


def test_a_damaged_shared_state_does_not_replace_the_local_history(machines, state_uri):
    a = machines("a")
    a_first = a.get("a_one")
    good = state_uri.read_bytes()
    state_uri.write_bytes(b"garbage")

    for args in (("get", "a_one.py", "--json"), ("status", "a_one.py", "--json")):
        out = a.barca(*args)
        assert out.returncode == 3, (args, out.stderr)
        # Reworded with #243 (test_state_validation.py covers every kind of invalid object).
        assert "is not a database barca can use" in out.stderr, out.stderr
        assert str(state_uri) in out.stderr, out.stderr
        assert "left as it was" in out.stderr, out.stderr
        assert a.local_runs() == {a_first}
        no_pull_leftovers(a)

    # Once the shared state is repaired, the machine carries on.
    state_uri.write_bytes(good)
    assert a.local_runs() | {a.get("a_two")} == set(shared_runs(state_uri))


def test_a_local_history_that_cannot_be_opened_is_replaced_with_a_warning(machines, state_uri):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    a.db.write_bytes(b"garbage")
    Path(f"{a.db}-wal").write_bytes(b"more garbage")

    out = a.barca("status", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert "warning: the local history file held no barca history" in out.stderr, out.stderr
    assert a.local_runs() == {a_first, b_run}


def test_what_a_pull_kept_is_said_once_until_it_is_pushed(machines, state_uri):
    a, b = machines("a"), machines("b")
    a.get("a_one")
    killed = a.kill(a.start_slow())
    b.get("b_one")

    # The first command to pull says what it kept. Commands that only look cannot push it, and
    # do not say it again: neither while the shared state stays as it is...
    first = a.barca("status", "slow.py", "--json")
    assert first.returncode == 0, first.stderr
    assert KEPT_THE_KILLED_RUN in first.stderr, first.stderr
    again = a.barca("status", "slow.py", "--json")
    assert "kept" not in again.stderr, again.stderr
    # ...nor after another machine pushed and the same rows had to be kept once more.
    b_two = b.get("b_two")
    for args in (("get", "slow.py", "--dry-run", "--json"), ("status", "slow.py", "--json")):
        out = a.barca(*args)
        assert out.returncode == 0, out.stderr
        assert "kept" not in out.stderr, (args, out.stderr)
    assert {killed, b_two} <= a.local_runs()
    assert a.states() == {"first": "cached", "slow": "never_run"}
    assert killed not in shared_runs(state_uri)

    # The next run pushes it.
    a.resume_slow()
    assert shared_runs(state_uri)[killed] == "interrupted"


def test_a_local_history_held_open_by_another_program_is_not_replaced(machines, state_uri):
    a, b = machines("a"), machines("b")
    a.get("a_one")
    killed = a.kill(a.start_slow())
    b.get("b_one")

    # Another program has A's database open, in the middle of reading it.
    holder = subprocess.Popen(
        [
            sys.executable,
            "-c",
            "import sqlite3, sys, time\n"
            "cursor = sqlite3.connect(sys.argv[1]).execute('SELECT * FROM runs')\n"
            "cursor.fetchone()\n"
            "print('held', flush=True)\n"
            "time.sleep(120)\n",
            str(a.db),
        ],
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        assert holder.stdout.readline().strip() == "held"
        out = a.barca("status", "slow.py", "--json")
    finally:
        holder.kill()
        holder.wait(timeout=WAIT)
        holder.stdout.close()
    assert out.returncode == 3, (out.returncode, out.stderr)
    assert "in use by another program" in out.stderr, out.stderr
    assert "left as it was" in out.stderr, out.stderr
    assert "replaced" not in out.stderr
    no_pull_leftovers(a)

    # Nothing was lost: once the other program lets go, the killed run resumes and is shared.
    assert killed in a.local_runs()
    out = a.resume_slow()
    assert json.loads(out.stdout)["steps_executed"] == 1
    assert shared_runs(state_uri)[killed] == "interrupted"


@pytest.mark.skipif(os.geteuid() == 0, reason="root can read a file without read permission")
def test_a_local_history_that_cannot_be_read_is_not_replaced(machines, state_uri):
    a, b = machines("a"), machines("b")
    a.get("a_one")
    killed = a.kill(a.start_slow())
    b.get("b_one")

    a.db.chmod(0o000)
    try:
        out = a.barca("get", "slow.py", "--dry-run", "--json")
    finally:
        a.db.chmod(0o644)
    assert out.returncode == 3, (out.returncode, out.stderr)
    assert "left as it was" in out.stderr, out.stderr
    no_pull_leftovers(a)
    assert killed in a.local_runs()
    assert json.loads(a.resume_slow().stdout)["steps_executed"] == 1
    assert shared_runs(state_uri)[killed] == "interrupted"


def test_a_truncated_local_history_is_replaced_with_a_warning(machines, state_uri):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    a.get("a_two", BARCA_STATE="off")  # a local write, so the file is looked at
    whole = a.db.read_bytes()
    a.db.write_bytes(whole[: len(whole) - 1000])
    Path(f"{a.db}-wal").unlink(missing_ok=True)

    out = a.barca("status", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert "warning: the local history file held no barca history" in out.stderr, out.stderr
    assert "cut short" in out.stderr, out.stderr
    assert a.local_runs() == {a_first, b_run}


# ─── nothing is concluded from `.barca/metadata.db.base` ────────────────────
#
# The base record only tells a pull whether the database was replaced or pushed while its
# download was on its way. Whatever it says, and whatever happened to the local database, a
# pull downloads, keeps what only the local database has, and swaps. Each test ends by
# checking that the shared state still holds every run of every machine.

TASK = "from barca import task\n\n\n@task()\ndef chore() -> int:\n    return 1\n"


def recreate_with(a: Machine, command: str) -> set[str]:
    """Delete A's local database (the record stays) and let `command` be the next thing to
    touch the project. Returns the runs that command recorded locally, if any."""
    a.db.unlink()
    Path(f"{a.db}-wal").unlink(missing_ok=True)
    assert Path(f"{a.db}.base").exists()
    off = {"BARCA_STATE": "off"}
    made: set[str] = set()
    if command == "history":
        assert a.barca("history", "--json").returncode == 0
    elif command == "stats":
        a.barca("stats", "a_one", "a_one.py", "--json")
    elif command == "list":
        assert a.barca("list", "a_one.py", "--json").returncode == 0
    elif command == "plan":
        assert a.barca("plan", "a_one.py").returncode == 0
    elif command == "status":
        assert a.barca("status", "a_one.py", "--json", **off).returncode == 0
    elif command == "get with state off":
        made.add(a.get("a_local", **off))
    elif command == "run with state off":
        (a.root / "chore.py").write_text(TASK)
        out = a.barca("run", "chore", "chore.py", "--json", **off)
        assert out.returncode == 0, out.stderr
        made.add(json.loads(out.stdout)["run_id"])
    elif command == "serve":
        proc = subprocess.Popen(
            [_find_binary(), "serve", "a_one.py", "--port", "0"],
            cwd=a.root,
            env={**a.env, **off},
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.time() + 5
            while not a.db.exists() and time.time() < deadline and proc.poll() is None:
                time.sleep(0.05)
        finally:
            proc.kill()
            proc.wait(timeout=WAIT)
    else:
        raise AssertionError(command)
    return made


@pytest.mark.parametrize(
    "command",
    [
        "history",
        "stats",
        "list",
        "plan",
        "status",
        "get with state off",
        "run with state off",
        "serve",
    ],
)
def test_a_database_recreated_under_an_old_record_is_not_pushed_over_the_shared_history(
    machines, state_uri, command
):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    a_second = a.get("a_two")  # A is in sync, and has a base record
    everything = {a_first, b_run, a_second}
    assert set(shared_runs(state_uri)) == everything

    made = recreate_with(a, command)

    out = a.barca("get", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert "[barca] pulled state" in out.stderr, out.stderr
    everything |= made | {json.loads(out.stdout)["run_id"]}
    assert set(shared_runs(state_uri)) == everything
    assert a.local_runs() == everything


@pytest.mark.parametrize("preserve_times", [False, True], ids=["cp", "cp -p"])
def test_an_older_copy_put_back_over_the_database_is_not_pushed_over_the_shared_history(
    machines, state_uri, preserve_times
):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    backup = a.root / "backup.db"
    subprocess.run(["cp", "-p", str(a.db), str(backup)], check=True)
    b_run = b.get("b_one")
    a_second = a.get("a_two")
    everything = {a_first, b_run, a_second}
    assert set(shared_runs(state_uri)) == everything

    # Someone restores yesterday's copy; the base record is today's.
    Path(f"{a.db}-wal").unlink(missing_ok=True)
    subprocess.run(["cp", *(["-p"] if preserve_times else []), str(backup), str(a.db)], check=True)
    assert a.local_runs() == {a_first}

    out = a.barca("get", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert "[barca] pulled state" in out.stderr, out.stderr
    everything.add(json.loads(out.stdout)["run_id"])
    assert set(shared_runs(state_uri)) == everything
    assert a.local_runs() == everything


def test_a_record_copied_from_another_project_is_not_believed(machines, state_uri):
    a, b, c = machines("a"), machines("b"), machines("c")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    a_second = a.get("a_two")
    everything = {a_first, b_run, a_second}

    # C has a database of its own with one local run, and A's record beside it.
    c_local = c.get("c_one", BARCA_STATE="off")
    record = Path(f"{c.db}.base")
    record.write_bytes(Path(f"{a.db}.base").read_bytes())

    out = c.barca("get", "c_one.py", "--json")
    assert out.returncode == 0, out.stderr
    everything |= {c_local, json.loads(out.stdout)["run_id"]}
    assert set(shared_runs(state_uri)) == everything
    assert c.local_runs() == everything


def test_a_write_by_another_program_that_leaves_size_and_mtime_alone_is_noticed(
    machines, state_uri
):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    a_second = a.get("a_two")  # in sync, record written

    # Another program adds a row. The file keeps its size, and its modification time is put
    # back (as a coarse-timestamp filesystem, `touch -r` or a restoring copy would leave it).
    before = a.db.stat()
    conn = sqlite3.connect(a.db)
    conn.execute(
        "INSERT INTO runs (run_id, command, files, status) VALUES ('by-hand', 'get', '[]', 'success')"
    )
    conn.commit()
    conn.close()
    Path(f"{a.db}-wal").unlink(missing_ok=True)
    Path(f"{a.db}-shm").unlink(missing_ok=True)
    os.utime(a.db, ns=(before.st_atime_ns, before.st_mtime_ns))
    after = a.db.stat()
    assert (after.st_size, after.st_mtime_ns) == (before.st_size, before.st_mtime_ns)

    # The shared state moves, then A pulls: the row must be compared and kept, not replaced.
    b_two = b.get("b_two")
    out = a.barca("get", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    everything = {a_first, b_run, a_second, b_two, "by-hand", json.loads(out.stdout)["run_id"]}
    assert set(shared_runs(state_uri)) == everything


def test_resetting_and_rolling_back_the_shared_history_as_documented(machines, state_uri):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    older = state_uri.read_bytes()
    b_run = b.get("b_one")
    a_second = a.get("a_two")

    # Rolled back to an older copy: the next machine to run puts back every run it holds.
    state_uri.write_bytes(older)
    out = a.barca("status", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert a.local_runs() == {a_first, b_run, a_second}
    assert set(shared_runs(state_uri)) == {a_first}  # status uploads nothing
    a_third = a.get("a_three")
    everything = {a_first, b_run, a_second, a_third}
    assert set(shared_runs(state_uri)) == everything

    # Deleted: the next run creates it again from that machine's whole local copy.
    state_uri.unlink()
    b_two = b.get("b_two")
    assert set(shared_runs(state_uri)) == {a_first, b_run, b_two}
    a_fourth = a.get("a_four")
    everything |= {b_two, a_fourth}
    assert set(shared_runs(state_uri)) == everything

    # Reset on purpose, as the manual says: the shared file and three files on each machine.
    state_uri.unlink()
    for machine in (a, b):
        for suffix in ("", "-wal", ".base"):
            Path(f"{machine.db}{suffix}").unlink(missing_ok=True)
    a_new = a.get("a_five")
    b_new = b.get("b_five")
    assert set(shared_runs(state_uri)) == {a_new, b_new}
    assert b.local_runs() == {a_new, b_new}


SLOW_UPLOAD = """#!/bin/sh
# Stands in for a slow object store: the upload of the shared state takes a long time.
case "$*" in
  *"barca._state push"*) touch upload.started; while [ ! -e upload.release ]; do sleep 0.1; done ;;
esac
exec {python} "$@"
"""


def test_a_slow_upload_keeps_no_other_command_waiting_and_loses_no_write(
    machines, state_uri, tmp_path
):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")

    # A barca whose state uploads stall until the test lets them go: the binary looks for
    # `python` beside itself, so a copy of it beside a wrapper is enough.
    slow_bin = tmp_path / "slow-bin"
    slow_bin.mkdir()
    (slow_bin / "barca").write_bytes(Path(_find_binary()).read_bytes())
    (slow_bin / "barca").chmod(0o755)
    (slow_bin / "python").write_text(SLOW_UPLOAD.format(python=sys.executable))
    (slow_bin / "python").chmod(0o755)
    (a.root / "a_two.py").write_text(quick("a_two"))
    pushing = subprocess.Popen(
        [str(slow_bin / "barca"), "get", "a_two.py", "--json"],
        cwd=a.root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=a.env,
    )
    try:
        wait_for(lambda: (a.root / "upload.started").exists(), "the upload to start")
        # While the upload is on its way, the project is not locked: reading the history,
        # asking for status (which pulls) and a run that only records locally all finish.
        started = time.time()
        assert a_first in a.local_runs()
        assert a.states("a_one.py") == {"a_one": "cached"}
        during = a.get("a_local", BARCA_STATE="off")
        assert time.time() - started < 20, "the other commands waited for the upload"
        assert pushing.poll() is None, "the upload must still be going"
    finally:
        (a.root / "upload.release").write_text("")
        stdout, stderr = pushing.communicate(timeout=WAIT)
    assert pushing.returncode == 0, stderr
    a_two = json.loads(stdout)["run_id"]

    # The run noticed that something was written while it uploaded, and pushed again: the
    # run recorded meanwhile is in the shared history too, and nothing is there twice.
    assert "conflict retr" in stderr, stderr
    assert set(shared_runs(state_uri)) == {a_first, b_run, a_two, during}
    assert a.local_runs() == {a_first, b_run, a_two, during}
    no_pull_leftovers(a)


BUSY_SIBLING = """#!/bin/sh
# Stands in for another run going in the same project: every time the shared state is
# uploaded, something is recorded in the local database while the upload is on its way.
case "$*" in
  *"barca._state push"*)
    echo x >> uploads
    BARCA_STATE=off {barca} get w.py --refresh-all --json >/dev/null 2>&1 ;;
esac
exec {python} "$@"
"""


def test_writes_during_every_upload_cost_one_more_upload_not_one_per_retry(
    machines, state_uri, tmp_path
):
    a = machines("a")
    a_first = a.get("a_one")
    (a.root / "w.py").write_text(quick("w"))
    (a.root / "a_two.py").write_text(quick("a_two"))

    busy_bin = tmp_path / "busy-bin"
    busy_bin.mkdir()
    (busy_bin / "barca").write_bytes(Path(_find_binary()).read_bytes())
    (busy_bin / "barca").chmod(0o755)
    (busy_bin / "python").write_text(
        BUSY_SIBLING.format(python=sys.executable, barca=_find_binary())
    )
    (busy_bin / "python").chmod(0o755)
    out = subprocess.run(
        [str(busy_bin / "barca"), "get", "a_two.py", "--json"],
        cwd=a.root,
        capture_output=True,
        text=True,
        env=a.env,
        timeout=WAIT * 2,
    )
    assert out.returncode == 0, out.stderr
    a_two = json.loads(out.stdout)["run_id"]

    # The run uploaded, saw the write, and uploaded once more. It did not go on chasing a
    # writer that writes during every upload (that was one pull and one upload per retry,
    # `push_retries` times).
    assert (a.root / "uploads").read_text().count("x") == 2, out.stderr
    assert "after 1 conflict retry" in out.stderr, out.stderr
    during = [r["run_id"] for r in reversed(a.history()) if r["run_id"] not in (a_first, a_two)]
    assert len(during) == 2, a.history()
    # What was written during the first upload went with the second; what was written
    # during the second is local, and goes with the next run from this machine.
    assert set(shared_runs(state_uri)) == {a_first, a_two, during[0]}
    a_three = a.get("a_three")
    assert set(shared_runs(state_uri)) == {a_first, a_two, a_three, *during}
    no_pull_leftovers(a)
