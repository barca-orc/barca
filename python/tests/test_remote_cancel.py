"""Ctrl-C while barca is talking to an artifact store (#249), end to end through the binary.

The rule (`barca docs remote`, "Ctrl-C"):

- The first Ctrl-C cancels the run. Transfers in flight are abandoned, what finished is
  recorded, and the run wraps up: it backs its history up to the store (RFC-0009), for at most
  ten seconds.
- A second Ctrl-C abandons the wrap-up at once.
- Whatever the moment, the count and the target of the signals (the whole job, as a terminal
  sends it, or barca alone), and whether the store answers: exit 130 with the `cancelled`
  envelope, no traceback, no helper process left, a step recorded only if its artifact is in
  the store, no half-written file, and a next run that succeeds.
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
# `barca_core::interrupt::WRAP_UP_LIMIT`: how long a cancelled run may spend on its backup.
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
    """Files a transfer in flight leaves if it is not cleaned up: staged artifacts and staged
    backup copies (`*.tmp`)."""
    patterns = ("*.tmp",)
    return sorted(str(p) for root in roots for pat in patterns for p in root.rglob(pat))


def cancelled_cleanly(run: Run, ran: bool = True) -> None:
    """What holds after every cancelled command. `ran` is false when it was cancelled before
    its run existed."""
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
    """The next run on the machine succeeds, and its history reaches the store."""
    run.release()
    (run.root / "release-slow").write_text("")
    again = cli(run.root, run.store, "get", target, "--json")
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout)["final_output"] == expected
    assert "backed up the metadata DB" in again.stderr, again.stderr
    assert not list((run.root / ".barca" / "backups").glob("*.unsent"))
    return again


@pytest.fixture
def project(tmp_path):
    def make(name: str) -> Path:
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        return root

    # The store is a directory that is there (a mounted share): backups go nowhere else.
    (tmp_path / "store").mkdir()
    return tmp_path / "store", make


def produced(root: Path, store: Path) -> None:
    """`total` computed on this machine and in the store, its local copies gone: the next get
    fetches it."""
    assert cli(root, store, "get", "total", "--json").returncode == 0
    for path in (root / ".barca" / "artifacts").rglob("*.json"):
        path.unlink()


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
    # The wrap-up backed the run's record up all the same.
    assert "backed up the metadata DB" in run.stderr, run.stderr
    assert "did not finish" not in run.stderr, run.stderr
    recovers(run, "numbers", [1, 2, 3])


@GROUP
def test_ctrl_c_during_a_fetch(project, whole_group):
    store, make = project
    root = make("m")
    produced(root, store)
    run = Run(root, store, "get", "get", "total", "--json", "--agent").held_at("get")
    # The download is under way: its temp file is there.
    assert len(temp_files(run.root)) == 1
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    assert list((run.root / ".barca" / "artifacts").rglob("*.json")) == []
    again = recovers(run, "total", {"sum": 6})
    assert "fetched 1 cached artifact" in again.stderr, again.stderr


@GROUP
def test_ctrl_c_during_the_end_of_run_backup(project, whole_group):
    """The steps finished and their artifacts are in the store; the backup's upload is cut.
    The run is `cancelled`; its copy waits here, and the next backup uploads it."""
    store, make = project
    run = Run(make("m"), store, "backup", "get", "total", "--json", "--agent").held_at("backup")
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    assert sorted(recorded(run.root)) == ["numbers", "total"]
    assert len(run.arrivals("backup")) == 1
    assert "the metadata backup was stopped" in run.stderr, run.stderr
    assert not list((store / "default" / "state" / "backups").glob("*Z.db"))
    assert len(list((run.root / ".barca" / "backups").glob("*.unsent"))) == 1
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
    points = "put,backup"
    run = Run(make("m"), store, points, "get", "numbers", "--json", "--agent", stall=True)
    run.held_at("put").printed("1/1 steps | done")
    started = time.monotonic()
    run.sigint()
    run.end(timeout=WRAP_UP_LIMIT + 60)
    took = time.monotonic() - started

    cancelled_cleanly(run)
    assert took >= WRAP_UP_LIMIT, f"the wrap-up backup was not given its {WRAP_UP_LIMIT}s: {took}"
    assert (
        f"[barca] the metadata backup did not finish (not within {WRAP_UP_LIMIT}s); the run is "
        "recorded on this machine and the next backup takes it" in run.stderr
    ), run.stderr
    assert not list((store / "default" / "state" / "backups").glob("*Z.db"))
    # The store answers again: the next run backs the cancelled run's record up with its own.
    recovers(run, "numbers", [1, 2, 3])
    assert [r["status"] for r in history(run.root, store)] == ["success", "cancelled"]


@GROUP
def test_a_second_ctrl_c_abandons_the_wrap_up_at_once(project, whole_group):
    store, make = project
    run = Run(make("m"), store, "put,backup", "get", "numbers", "--json", "--agent", stall=True)
    run.held_at("put").printed("1/1 steps | done")
    run.sigint(whole_group)
    # The first Ctrl-C cut the upload and the wrap-up started a backup, which stalls too.
    run.held_at("backup")
    assert run.proc.poll() is None
    run.sigint(whole_group)
    run.end()

    cancelled_cleanly(run)
    assert "the metadata backup did not finish (stopped by a second interrupt)" in run.stderr
    assert not list((store / "default" / "state" / "backups").glob("*Z.db"))
    recovers(run, "numbers", [1, 2, 3])


def test_a_third_ctrl_c_changes_nothing(project):
    store, make = project
    run = Run(make("m"), store, "put,backup", "get", "numbers", "--json", "--agent", stall=True)
    run.held_at("put").printed("1/1 steps | done")
    run.sigint()
    run.held_at("backup")
    run.sigint()
    run.sigint()
    run.end()
    cancelled_cleanly(run)
    recovers(run, "numbers", [1, 2, 3])


def test_a_wrap_up_backup_that_fails_is_a_note_and_the_exit_code_stays_130(project):
    store, make = project
    run = Run(make("m"), store, "put", "get", "slow", "--json", "--agent").held_at("put")
    wait_until((run.root / "slow.started").exists, "the second step to start", run.proc)
    # Something the backup cannot get past: a file where its directory belongs.
    (store / "default" / "state").mkdir(parents=True)
    (store / "default" / "state" / "backups").write_text("in the way")
    run.sigint()
    run.end()

    assert run.proc.returncode == 130, run.stderr
    envelope = json.loads(
        next(line for line in reversed(run.stderr.splitlines()) if line.startswith("{"))
    )
    assert envelope["kind"] == "cancelled", envelope
    assert "[barca] the metadata backup was not uploaded (backup to " in run.stderr, run.stderr
    assert history(run.root, store)[0]["status"] == "cancelled"


# ─── barca itself is killed ──────────────────────────────────────────────────

KILLED = {
    # phase: (points, target, text barca prints before the helper is held, needs a producer)
    "upload": ("copied", "numbers", "1/1 steps | done", False),
    "backup": ("backup", "total", "", False),
    "fetch": ("get", "total", "", True),
}


@pytest.mark.parametrize("phase", sorted(KILLED))
def test_helpers_exit_on_their_own_when_barca_is_killed(project, phase):
    """Helpers are deaf to Ctrl-C, so nothing but the death of the coordinator tells them to
    stop: they must notice it themselves (the closed socket, the lifeline) and go quietly."""
    points, target, printed, needs_producer = KILLED[phase]
    store, make = project
    root = make("m")
    if needs_producer:
        produced(root, store)
    run = Run(root, store, points, "get", target, "--json", "--agent")
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
    # No half-written artifact or object. (The staged copy a killed backup leaves in
    # .barca/backups is replaced by the next backup's.)
    assert [p for p in temp_files(run.root, store) if "/.barca/backups/" not in p] == []
    run.proc.stdout.close()
    # Workers of the killed run may still hold the group; end them before the next run.
    try:
        os.killpg(run.proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    recovers(run, target, [1, 2, 3] if target == "numbers" else {"sum": 6})
    assert [p for p in temp_files(run.root, store) if "/.barca/backups/" not in p] == []


# ─── every phase, one to three signals, either target, either kind of store ──

PHASES = {
    # phase: (hold points, target, needs a producer)
    "step": ("put", "slow", False),
    "fetch": ("get", "total", True),
    "upload": ("put", "numbers", False),
    "backup": ("backup", "total", False),
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
    root = make("m")
    if needs_producer:
        produced(root, store)
    if stalled and "backup" not in points:
        points += ",backup"  # the wrap-up backup stalls as well
    run = Run(root, store, points, "get", target, "--json", "--agent", stall=stalled)
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

    cancelled_cleanly(run)
    recovers(run, target, EXPECTED[target])
