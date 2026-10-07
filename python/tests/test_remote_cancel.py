"""Ctrl-C while barca is talking to an artifact store (#249), end to end through the binary.

The rule (`barca docs remote`, "Ctrl-C"):

- The first Ctrl-C cancels the run. Transfers in flight are abandoned, what finished is
  recorded, and the run wraps up: it pushes its record to the shared history, for at most ten
  seconds.
- A second Ctrl-C abandons the wrap-up at once.
- Whatever the moment, the count and the target of the signals (the whole job, as a terminal
  sends it, or barca alone), and whether the store answers: exit 130 with the `cancelled`
  envelope, no traceback, no helper process left, a step recorded only if its artifact is in
  the store, no half-written file, and a next run that succeeds and shares what the cancelled
  one had recorded.
- If barca itself is killed, its helpers notice and exit on their own.

Each test stops one of barca's helper processes at a point the test controls (the shim in
python/tests/hold/), waits for it to get there, and only then sends the signal: no sleep stands
in for "the upload is probably running by now". The matrix at the end does send further
signals after fixed gaps; what it asserts holds for every interleaving.
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

HOLD_SHIM = str(Path(__file__).resolve().parent / "hold")

PIPELINE = """
import time
from pathlib import Path

from barca import asset


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"sum": sum(numbers)}


@asset(inputs={"numbers": numbers})
def slow(numbers: list) -> int:
    Path("slow.started").write_text("x")
    while not Path("release-slow").exists():
        time.sleep(0.02)
    return len(numbers)
"""

DEADLINE = 60
# `barca_core::interrupt::WRAP_UP_LIMIT`: how long a cancelled run may spend pushing.
WRAP_UP_LIMIT = 10


def env(store: Path, hold: str = "", hold_dir: Path | None = None, stall: bool = False) -> dict:
    e = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    e["BARCA_REMOTE_URI"] = str(store)
    if hold:
        e["PYTHONPATH"] = HOLD_SHIM + os.pathsep + e.get("PYTHONPATH", "")
        e["BARCA_TEST_HOLD"] = f"{hold}:{hold_dir}"
        if stall:
            e["BARCA_TEST_STALL"] = "1"
    return e


def cli(root: Path, store: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env(store),
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


def wait_until(done, what: str, proc: subprocess.Popen | None = None) -> None:
    deadline = time.monotonic() + DEADLINE
    while not done():
        if proc is not None and proc.poll() is not None:
            raise AssertionError(f"barca exited before {what}")
        if time.monotonic() > deadline:
            raise AssertionError(f"timed out waiting for {what}")
        time.sleep(0.02)


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


class Run:
    """`barca <args>` with its helpers held at `points` (comma-separated), in its own
    process group like a foreground job. `stall` makes every store operation at those points
    wait for ever, not only the first."""

    def __init__(self, root: Path, store: Path, points: str, *args: str, stall: bool = False):
        self.root, self.store = root, store
        self.hold_dir = root.parent / f"hold-{root.name}"
        self.hold_dir.mkdir()
        self.err_path = self.hold_dir / "stderr.txt"
        with open(self.err_path, "w") as err:
            self.proc = subprocess.Popen(
                [_find_binary(), *args],
                cwd=root,
                env=env(store, points, self.hold_dir, stall),
                stdout=subprocess.PIPE,
                stderr=err,
                text=True,
                start_new_session=True,
            )

    def held_at(self, point: str) -> "Run":
        """Wait until a helper is held at `point`."""
        marker = self.hold_dir / f"{point}.started"
        self._wait(marker.exists, f"a helper reached '{point}'")
        return self

    def _wait(self, done, what: str) -> None:
        """`wait_until`, and if it gives up: what barca printed and what its job looks like."""
        try:
            wait_until(done, what, self.proc)
        except AssertionError as failed:
            job = subprocess.run(
                ["ps", "-o", "pid,ppid,stat,etime,command", "-g", str(self.proc.pid)],
                capture_output=True,
                text=True,
            ).stdout
            raise AssertionError(
                f"{failed}\n--- stderr so far:\n{self.err_path.read_text()}\n"
                f"--- held: {sorted(p.name for p in self.hold_dir.iterdir())}\n--- job:\n{job}"
            ) from None

    def arrivals(self, point: str) -> list[int]:
        """The pids of every helper that reached `point` so far."""
        listed = self.hold_dir / f"{point}.pids"
        return [int(p) for p in listed.read_text().split()] if listed.exists() else []

    def printed(self, text: str) -> "Run":
        self._wait(lambda: text in self.err_path.read_text(), f"'{text}' on stderr")
        return self

    def sigint(self, whole_group: bool = True) -> None:
        """Interrupt as a terminal does (every process of the job) or barca alone."""
        if self.proc.poll() is not None:
            return  # it has already ended
        try:
            if whole_group:
                os.killpg(self.proc.pid, signal.SIGINT)
            else:
                self.proc.send_signal(signal.SIGINT)
        except (ProcessLookupError, PermissionError):
            pass  # it ended meanwhile (macOS answers EPERM for a group of exited processes)

    def end(self, timeout: float = DEADLINE) -> str:
        """Wait for barca to exit; returns its stderr."""
        self.stdout, _ = self.proc.communicate(timeout=timeout)
        self.stderr = self.err_path.read_text()
        return self.stderr

    def helpers(self) -> list[int]:
        return [int(p) for f in self.hold_dir.glob("*.pids") for p in f.read_text().split()]

    def release(self) -> None:
        (self.hold_dir / "release").write_text("")


def history(root: Path, store: Path) -> list[dict]:
    proc = cli(root, store, "history", "--json")
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)["runs"]


def recorded(root: Path) -> dict[str, str]:
    """node name -> artifact path, for every successful materialization recorded locally."""
    db = sqlite3.connect(root / ".barca" / "metadata.db")
    try:
        return {
            node.rsplit(":", 1)[1]: path
            for node, path in db.execute(
                "select node_id, artifact_path from materializations where status = 'success'"
            )
        }
    finally:
        db.close()


def temp_files(*roots: Path) -> list[str]:
    """Files a transfer in flight leaves if it is not cleaned up: staged artifacts (`*.tmp`)
    and the staged copies of the metadata DB (`metadata.db.pull-*`, `metadata.db.push-*`)."""
    patterns = ("*.tmp", "metadata.db.pull-*", "metadata.db.push-*")
    return sorted(str(p) for root in roots for pat in patterns for p in root.rglob(pat))


def cancelled_cleanly(run: Run, ran: bool = True) -> None:
    """What holds after every cancelled command. `ran` is false when it was cancelled before
    its run existed (during the first pull)."""
    err, root, store = run.stderr, run.root, run.store
    assert run.proc.returncode == 130, err
    assert run.stdout == "", run.stdout  # no result document for a cancelled run
    envelope = json.loads(err.strip().splitlines()[-1])
    assert (envelope["kind"], envelope["code"]) == ("cancelled", 130), envelope
    for sign in ("Traceback", "KeyboardInterrupt", "ConnectionRefusedError", "BrokenPipe"):
        assert sign not in err, err
    # The command returned, so its helpers are gone: none is left to finish something later.
    assert [pid for pid in run.helpers() if alive(pid)] == []
    if ran:
        assert history(root, store)[0]["status"] == "cancelled"
        # Every recorded step has its artifact in the store.
        for node, path in recorded(root).items():
            assert Path(path).is_file(), f"{node} is recorded but {path} is not in the store"
    else:
        assert not (root / ".barca" / "metadata.db").exists()
    assert temp_files(root, store) == []


def recovers(run: Run, target: str, expected) -> subprocess.CompletedProcess:
    """The next run on the machine succeeds, and afterwards another machine has its results."""
    run.release()
    (run.root / "release-slow").write_text("")
    again = cli(run.root, run.store, "get", target, "--json")
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout)["final_output"] == expected
    other = run.root.parent / f"other-{run.root.name}"
    other.mkdir()
    (other / "pipeline.py").write_text(PIPELINE)
    (other / "release-slow").write_text("")
    shared = cli(other, run.store, "get", target, "--json")
    assert shared.returncode == 0, shared.stderr
    doc = json.loads(shared.stdout)
    assert doc["final_output"] == expected and doc["steps_executed"] == 0, doc
    return again


@pytest.fixture
def project(tmp_path):
    def make(name: str) -> Path:
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        return root

    return tmp_path / "store", make


def produced(store: Path, make) -> None:
    assert cli(make("producer"), store, "get", "total", "--json").returncode == 0


GROUP = pytest.mark.parametrize("whole_group", [True, False], ids=["job", "barca-only"])


# ─── one Ctrl-C, a store that answers ────────────────────────────────────────


@GROUP
def test_ctrl_c_during_the_end_of_run_upload_wait(project, whole_group):
    store, make = project
    run = Run(make("m"), store, "put", "get", "numbers", "--json", "--agent")
    run.held_at("put").printed("1/1 steps | done")
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    # The upload never finished, so the step is not recorded and its object does not exist.
    assert recorded(run.root) == {}
    assert list(store.glob("default/artifacts/*/*")) == []
    # The wrap-up shared the run's record all the same.
    assert "pushed state" in run.stderr, run.stderr
    assert "was not updated" not in run.stderr, run.stderr
    recovers(run, "numbers", [1, 2, 3])


@GROUP
def test_ctrl_c_during_a_fetch(project, whole_group):
    store, make = project
    produced(store, make)
    run = Run(make("m"), store, "get", "get", "total", "--json", "--agent").held_at("get")
    # The download is under way: its temp file is there.
    assert len(temp_files(run.root)) == 1
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    assert list((run.root / ".barca" / "artifacts").rglob("*.json")) == []
    again = recovers(run, "total", {"sum": 6})
    assert "fetched 1 cached artifact" in again.stderr, again.stderr


@GROUP
def test_ctrl_c_during_the_state_push(project, whole_group):
    """The steps finished and their artifacts are in the store; the push of the record is cut.
    The run is `cancelled`, and its wrap-up shares that record."""
    store, make = project
    run = Run(make("m"), store, "push", "get", "total", "--json", "--agent").held_at("push")
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    assert sorted(recorded(run.root)) == ["numbers", "total"]
    # Two pushes: the one that was cut, and the wrap-up, which landed.
    assert len(run.arrivals("push")) == 2
    assert "pushed state" in run.stderr, run.stderr
    assert (store / "default" / "state" / "metadata.db").is_file()
    recovers(run, "total", {"sum": 6})


@GROUP
def test_ctrl_c_while_the_shared_history_is_pulled(project, whole_group):
    store, make = project
    produced(store, make)
    run = Run(make("m"), store, "pull", "get", "total", "--json", "--agent").held_at("pull")
    run.sigint(whole_group)
    run.end()

    # Nothing ran and no run was created: there is no local history at all yet.
    cancelled_cleanly(run, ran=False)
    recovers(run, "total", {"sum": 6})


@GROUP
def test_ctrl_c_during_a_step_with_an_upload_in_flight(project, whole_group):
    """The case of #249: the interrupt arrives while steps run and the helper is uploading."""
    store, make = project
    run = Run(make("m"), store, "put", "get", "slow", "--json", "--agent").held_at("put")
    wait_until((run.root / "slow.started").exists, "the second step to start", run.proc)
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    assert recorded(run.root) == {}  # `numbers` was never confirmed in the store
    assert "cancelled after" in run.stderr and "| done in" not in run.stderr, run.stderr
    recovers(run, "slow", 3)


# ─── a store that does not answer: the wrap-up is bounded, and can be abandoned ──


def test_a_stalled_store_holds_a_cancelled_run_for_the_wrap_up_limit_and_no_longer(project):
    store, make = project
    points = "put,push"
    run = Run(make("m"), store, points, "get", "numbers", "--json", "--agent", stall=True)
    run.held_at("put").printed("1/1 steps | done")
    started = time.monotonic()
    run.sigint()
    run.end(timeout=WRAP_UP_LIMIT + 60)
    took = time.monotonic() - started

    cancelled_cleanly(run)
    assert took >= WRAP_UP_LIMIT, f"the wrap-up push was not given its {WRAP_UP_LIMIT}s: {took}"
    assert (
        f"[barca] the shared history was not updated (the upload did not finish within "
        f"{WRAP_UP_LIMIT}s). This run is recorded on this machine; the next barca get or "
        "barca run here uploads it." in run.stderr
    ), run.stderr
    assert not (store / "default" / "state" / "metadata.db").exists()
    # The store answers again: the next run uploads the cancelled run's record with its own.
    recovers(run, "numbers", [1, 2, 3])
    assert [r["status"] for r in history(run.root, store)] == ["success", "cancelled"]


@GROUP
def test_a_second_ctrl_c_abandons_the_wrap_up_at_once(project, whole_group):
    store, make = project
    run = Run(make("m"), store, "push", "get", "total", "--json", "--agent", stall=True)
    run.held_at("push")
    run.sigint(whole_group)
    # The first Ctrl-C cut the push and the wrap-up started another, which stalls too.
    wait_until(lambda: len(run.arrivals("push")) == 2, "the wrap-up push", run.proc)
    assert run.proc.poll() is None
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    assert "the shared history was not updated (stopped by a second Ctrl-C)" in run.stderr
    assert sorted(recorded(run.root)) == ["numbers", "total"]
    assert not (store / "default" / "state" / "metadata.db").exists()
    recovers(run, "total", {"sum": 6})


def test_a_third_ctrl_c_changes_nothing(project):
    store, make = project
    run = Run(make("m"), store, "push", "get", "total", "--json", "--agent", stall=True)
    run.held_at("push")
    run.sigint()
    wait_until(lambda: len(run.arrivals("push")) == 2, "the wrap-up push", run.proc)
    run.sigint()
    run.sigint()
    run.end()
    cancelled_cleanly(run)
    recovers(run, "total", {"sum": 6})


def test_a_cancelled_push_changes_nothing_shared_until_the_wrap_up_lands(project):
    """With a shared history in place and a store that stalls, the cut push and the
    abandoned wrap-up leave the shared history byte for byte as it was."""
    store, make = project
    root = make("m")
    assert cli(root, store, "get", "numbers", "--json").returncode == 0
    shared = (store / "default" / "state" / "metadata.db").read_bytes()

    run = Run(root, store, "push", "get", "total", "--json", "--agent", stall=True)
    run.held_at("push")
    run.sigint()
    wait_until(lambda: len(run.arrivals("push")) == 2, "the wrap-up push", run.proc)
    run.sigint()
    run.end()

    cancelled_cleanly(run)
    assert (store / "default" / "state" / "metadata.db").read_bytes() == shared
    again = recovers(run, "total", {"sum": 6})
    # A pull keeps what was recorded only here: both steps are cache hits, then shared.
    assert json.loads(again.stdout)["steps_executed"] == 0
    assert "recorded only on this machine" in again.stderr, again.stderr


def test_a_wrap_up_push_that_fails_is_a_note_and_the_exit_code_stays_130(project):
    store, make = project
    run = Run(make("m"), store, "put", "get", "slow", "--json", "--agent").held_at("put")
    wait_until((run.root / "slow.started").exists, "the second step to start", run.proc)
    # Something the push cannot get past, put there after the run's pull.
    (store / "default" / "state" / "metadata.db" / "theirs").mkdir(parents=True)
    run.sigint()
    run.end()

    assert run.proc.returncode == 130, run.stderr
    envelope = json.loads(run.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "cancelled", envelope
    assert "[barca] the shared history was not updated (shared state push to " in run.stderr
    assert "is a directory, not the shared history file" in run.stderr, run.stderr
    assert history(run.root, store)[0]["status"] == "cancelled"


# ─── what the run's record says ──────────────────────────────────────────────


def shared_history(store: Path, make, name: str) -> list[str]:
    """The statuses of the runs another machine sees after pulling the shared history."""
    other = make(name)
    assert cli(other, store, "status", "--json").returncode == 0
    return [r["status"] for r in history(other, store)]


def test_a_push_that_landed_just_before_the_interrupt_is_followed_by_the_cancelled_record(
    project,
):
    """The narrow case: the history is in the store with the run as `success`, and barca is
    interrupted before it hears so. The wrap-up pushes again, so every machine sees
    `cancelled`, like this one."""
    store, make = project
    run = Run(make("m"), store, "pushed", "get", "total", "--json", "--agent").held_at("pushed")
    assert (store / "default" / "state" / "metadata.db").is_file()  # it landed
    run.sigint()
    run.end()

    cancelled_cleanly(run)
    assert "pushed state" in run.stderr and "conflict retry" in run.stderr, run.stderr
    assert shared_history(store, make, "other") == ["cancelled"]


def test_ctrl_c_after_a_failed_run_was_shared_leaves_its_record_as_it_is(project):
    """A run with a failed step is recorded and shared as `failed`, then downloads an earlier
    output to return. Interrupted there, the command is cancelled; the run already ended."""
    store, make = project
    assert cli(make("producer"), store, "get", "numbers", "--json").returncode == 0
    root = make("m")
    (root / "pipeline.py").write_text(
        "from barca import asset\n\n\n@asset()\ndef numbers() -> list:\n    return [1, 2, 3]\n"
        "\n\n@asset()\ndef zz_broken() -> int:\n    raise ValueError('no')\n"
    )
    run = Run(root, store, "get", "get", "--json", "--agent").held_at("get")
    run.printed("pushed state")
    run.sigint()
    run.end()

    assert run.proc.returncode == 130, run.stderr
    assert json.loads(run.stderr.strip().splitlines()[-1])["kind"] == "cancelled"
    assert [pid for pid in run.helpers() if alive(pid)] == []
    assert temp_files(root, store) == []
    assert history(root, store)[0]["status"] == "failed"
    assert shared_history(store, make, "other")[0] == "failed"


def test_a_second_ctrl_c_while_a_worker_reports_the_first_prints_no_traceback(tmp_path):
    """Workers are in the terminal's job and do get Ctrl-C: the step is interrupted and the
    worker reports that. A second Ctrl-C arriving during the report used to escape as an
    uncaught KeyboardInterrupt, with a traceback (once in 300 runs of the matrix below).

    Made certain here: the step raises KeyboardInterrupt, and the second signal is sent from
    inside the report."""
    import sys
    import textwrap

    script = textwrap.dedent(
        """
        import os, signal, sys
        from pathlib import Path
        from barca import _duckdb, _runtime, _worker

        reported = []
        _runtime.emit_step_error = lambda **kw: reported.append(kw)
        real = _duckdb.explain_error

        def pressed_again(exc, views):
            os.kill(os.getpid(), signal.SIGINT)
            for _ in range(1000):  # give the interpreter every chance to deliver it
                pass
            return real(exc, views)

        _duckdb.explain_error = pressed_again
        source = Path(sys.argv[1]) / "mod.py"
        source.write_text("def interrupted():\\n    raise KeyboardInterrupt\\n")
        step = {"node_id": "mod.py:interrupted", "function_name": "interrupted",
                "source_file": str(source), "kind": "asset", "inputs": {}, "run_hash": "h"}
        ok = _worker._run_daemon_step(step, {}, str(Path(sys.argv[1]) / "arts"), _worker._ArtifactLRU())
        print("reported", ok, reported[0]["error_type"])
        """
    )
    proc = subprocess.run(
        [sys.executable, "-c", script, str(tmp_path)], capture_output=True, text=True, timeout=120
    )
    assert proc.stdout.strip() == "reported False KeyboardInterrupt", proc.stderr
    assert "Traceback" not in proc.stderr, proc.stderr
    assert proc.returncode == 0


# ─── barca itself is killed ──────────────────────────────────────────────────

KILLED = {
    # phase: (points, target, text barca prints before the helper is held, needs a producer)
    "upload": ("copied", "numbers", "1/1 steps | done", False),
    "push": ("push", "total", "", False),
    "fetch": ("get", "total", "", True),
    "pull": ("pull", "total", "", True),
}


@pytest.mark.parametrize("phase", sorted(KILLED))
def test_helpers_exit_on_their_own_when_barca_is_killed(project, phase):
    """Helpers are deaf to Ctrl-C, so nothing but the death of the coordinator tells them to
    stop: they must notice it themselves (the closed socket, the lifeline) and go quietly."""
    points, target, printed, needs_producer = KILLED[phase]
    store, make = project
    if needs_producer:
        produced(store, make)
    run = Run(make("m"), store, points, "get", target, "--json", "--agent")
    run.held_at(points)
    if printed:
        run.printed(printed)
    helpers = run.helpers()
    assert helpers and all(alive(pid) for pid in helpers)

    run.proc.kill()
    run.proc.wait(timeout=DEADLINE)
    # Well within ten seconds; in practice at once.
    deadline = time.monotonic() + 10
    while any(alive(pid) for pid in helpers) and time.monotonic() < deadline:
        time.sleep(0.02)
    assert [pid for pid in helpers if alive(pid)] == [], "a helper outlived barca"
    err = run.err_path.read_text()
    assert "Traceback" not in err, err
    # No half-written artifact or object. (The staged copy of the metadata DB that a killed
    # pull or push leaves is removed by the next pull.)
    assert [p for p in temp_files(run.root, store) if p.endswith(".tmp")] == []
    run.proc.stdout.close()
    # Workers of the killed run may still hold the group; end them before the next run.
    try:
        os.killpg(run.proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    recovers(run, target, [1, 2, 3] if target == "numbers" else {"sum": 6})
    assert temp_files(run.root, store) == []


# ─── every phase, one to three signals, either target, either kind of store ──

PHASES = {
    # phase: (hold points, target, needs a producer)
    "pull": ("pull", "total", True),
    "step": ("put", "slow", False),
    "fetch": ("get", "total", True),
    "upload": ("put", "numbers", False),
    "push": ("push", "total", False),
}
EXPECTED = {"total": {"sum": 6}, "slow": 3, "numbers": [1, 2, 3]}
# The gaps between signals. BARCA_TEST_SIGNAL_GAPS overrides them ("0.01,0.1,0.5,2").
GAPS = [float(g) for g in os.environ.get("BARCA_TEST_SIGNAL_GAPS", "0.01,0.3").split(",")]
MATRIX = [
    pytest.param(phase, count, gap, whole_group, stalled, id="-".join(map(str, case)))
    for phase in PHASES
    for stalled in (False, True)
    for whole_group in (True, False)
    for count, gap in [(2, g) for g in GAPS] + [(3, g) for g in GAPS]
    for case in [
        (
            phase,
            f"{count}x{gap:g}s",
            "job" if whole_group else "barca-only",
            "stalled" if stalled else "healthy",
        )
    ]
]


@pytest.mark.parametrize("phase,count,gap,whole_group,stalled", MATRIX)
def test_any_number_of_ctrl_c_at_any_gap_ends_cancelled(
    project, phase, count, gap, whole_group, stalled
):
    points, target, needs_producer = PHASES[phase]
    store, make = project
    if needs_producer:
        produced(store, make)
    if stalled and "push" not in points:
        points += ",push"  # the wrap-up push stalls as well
    run = Run(make("m"), store, points, "get", target, "--json", "--agent", stall=stalled)
    run.held_at(points.split(",")[0])
    if phase == "step":
        wait_until((run.root / "slow.started").exists, "the slow step to start", run.proc)
    if phase == "upload":
        run.printed("1/1 steps | done")

    for n in range(count):
        if n:
            time.sleep(gap)
        run.sigint(whole_group)
    # Never longer than the wrap-up limit plus the time to stop the helpers.
    run.end(timeout=WRAP_UP_LIMIT + 60)

    cancelled_cleanly(run, ran=phase != "pull")
    recovers(run, target, EXPECTED[target])
