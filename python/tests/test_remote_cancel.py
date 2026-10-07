"""Ctrl-C while barca is talking to an artifact store (#249), end to end through the binary.

What must hold at whatever moment the interrupt arrives (`barca docs remote`, "Ctrl-C"):

- the command exits 130 with the `cancelled` envelope, and no helper prints a traceback;
- the run is recorded as `cancelled`;
- a step is recorded only if its artifact is in the store, so a later run never reads an
  object that is not there;
- no half-written file is left, locally or in the store, and the next run succeeds.

Each test stops one of barca's helper processes at a point the test controls (the shim in
python/tests/hold/), waits for it to get there, and only then sends the signal: no sleep
stands in for "the upload is probably running by now".
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
from barca import asset


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"sum": sum(numbers)}
"""

DEADLINE = 60


def env(store: Path, hold: str | None = None, hold_dir: Path | None = None) -> dict[str, str]:
    e = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    e["BARCA_REMOTE_URI"] = str(store)
    if hold:
        e["PYTHONPATH"] = HOLD_SHIM + os.pathsep + e.get("PYTHONPATH", "")
        e["BARCA_TEST_HOLD"] = f"{hold}:{hold_dir}"
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


class Interrupted:
    """`barca <args>` interrupted while a helper is held at `point`."""

    def __init__(self, root: Path, store: Path, point: str, *args: str, after: str = ""):
        self.hold_dir = root.parent / f"hold-{root.name}-{point}"
        self.hold_dir.mkdir()
        self.err_path = self.hold_dir / "stderr.txt"
        with open(self.err_path, "w") as err:
            self.proc = subprocess.Popen(
                [_find_binary(), *args],
                cwd=root,
                env=env(store, point, self.hold_dir),
                stdout=subprocess.PIPE,
                stderr=err,
                text=True,
                start_new_session=True,  # its own process group, like a foreground job
            )
        marker = self.hold_dir / f"{point}.started"
        wait_until(marker.exists, f"a helper reached '{point}'", self.proc)
        if after:
            # Also wait for a line barca prints once it is past its steps.
            wait_until(lambda: after in self.err_path.read_text(), f"'{after}'", self.proc)

    def ctrl_c(self, whole_group: bool = True) -> str:
        """Interrupt as a terminal does (every process of the job) or barca alone."""
        if whole_group:
            os.killpg(self.proc.pid, signal.SIGINT)
        else:
            self.proc.send_signal(signal.SIGINT)
        self.stdout, _ = self.proc.communicate(timeout=DEADLINE)
        self.stderr = self.err_path.read_text()
        return self.stderr


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
    return sorted(str(p) for root in roots for p in root.rglob("*.tmp"))


def cancelled_cleanly(run: Interrupted, root: Path, store: Path) -> None:
    err = run.stderr
    assert run.proc.returncode == 130, err
    assert run.stdout == "", run.stdout  # no result document for a cancelled run
    envelope = json.loads(err.strip().splitlines()[-1])
    assert (envelope["kind"], envelope["code"]) == ("cancelled", 130), envelope
    assert "Traceback" not in err and "KeyboardInterrupt" not in err, err
    assert history(root, store)[0]["status"] == "cancelled"
    # Every recorded step has its artifact in the store.
    for node, path in recorded(root).items():
        assert Path(path).is_file(), f"{node} is recorded but {path} is not in the store"
    assert temp_files(root, store) == []


@pytest.fixture
def project(tmp_path):
    def make(name: str) -> Path:
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        return root

    return tmp_path / "store", make


GROUP = pytest.mark.parametrize("whole_group", [True, False], ids=["job", "barca-only"])


@GROUP
def test_ctrl_c_during_the_end_of_run_upload_wait(project, whole_group):
    store, make = project
    root = make("uploader")
    run = Interrupted(
        root, store, "put", "get", "numbers", "--json", "--agent", after="1/1 steps | done"
    )
    run.ctrl_c(whole_group)

    cancelled_cleanly(run, root, store)
    # The upload never finished, so the step is not recorded and its object does not exist.
    assert recorded(root) == {}
    assert list(store.glob("default/artifacts/*/*")) == []

    again = cli(root, store, "get", "numbers", "--json")
    assert again.returncode == 0, again.stderr
    doc = json.loads(again.stdout)
    assert doc["final_output"] == [1, 2, 3] and doc["steps_executed"] == 1
    assert len(list(store.glob("default/artifacts/*--numbers/*.json"))) == 1


@GROUP
def test_ctrl_c_during_a_fetch(project, whole_group):
    store, make = project
    assert cli(make("producer"), store, "get", "total", "--json").returncode == 0
    root = make("reader")
    run = Interrupted(root, store, "get", "get", "total", "--json", "--agent")
    # The download is under way: its temp file is there.
    assert len(temp_files(root)) == 1
    run.ctrl_c(whole_group)

    cancelled_cleanly(run, root, store)
    assert list((root / ".barca" / "artifacts").rglob("*.json")) == []

    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    doc = json.loads(again.stdout)
    assert doc["final_output"] == {"sum": 6} and doc["steps_executed"] == 0
    assert "fetched 1 cached artifact" in again.stderr, again.stderr


@GROUP
def test_ctrl_c_during_the_state_push(project, whole_group):
    store, make = project
    root = make("pusher")
    run = Interrupted(root, store, "push", "get", "total", "--json", "--agent")
    run.ctrl_c(whole_group)

    cancelled_cleanly(run, root, store)
    # The steps finished and their artifacts were uploaded before the push began; only the
    # shared history was not updated.
    assert sorted(recorded(root)) == ["numbers", "total"]
    assert not (store / "default" / "state" / "metadata.db").exists()

    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout)["final_output"] == {"sum": 6}
    assert (store / "default" / "state" / "metadata.db").is_file()
    assert history(root, store)[0]["status"] == "success"


def test_a_cancelled_push_leaves_the_shared_history_as_it_was(project):
    """With a shared history in place, a push cut short changes nothing in it: the next run
    starts from it, as every run does, and computes the unshared steps again."""
    store, make = project
    root = make("pusher")
    assert cli(root, store, "get", "numbers", "--json").returncode == 0
    shared = (store / "default" / "state" / "metadata.db").read_bytes()

    run = Interrupted(root, store, "push", "get", "total", "--json", "--agent")
    run.ctrl_c()

    cancelled_cleanly(run, root, store)
    assert (store / "default" / "state" / "metadata.db").read_bytes() == shared
    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    doc = json.loads(again.stdout)
    assert doc["final_output"] == {"sum": 6}
    status = {s["id"].rsplit(":", 1)[1]: s["status"] for s in doc["steps"]}
    assert status == {"numbers": "cached", "total": "ran"}


@GROUP
def test_ctrl_c_while_the_shared_history_is_pulled(project, whole_group):
    store, make = project
    assert cli(make("producer"), store, "get", "total", "--json").returncode == 0
    root = make("reader")
    run = Interrupted(root, store, "pull", "get", "total", "--json", "--agent")
    err = run.ctrl_c(whole_group)

    assert run.proc.returncode == 130, err
    envelope = json.loads(err.strip().splitlines()[-1])
    assert (envelope["kind"], envelope["code"]) == ("cancelled", 130), envelope
    assert "Traceback" not in err and "KeyboardInterrupt" not in err, err
    # Nothing ran and no run was created: there is no local history at all yet.
    assert not (root / ".barca" / "metadata.db").exists()
    assert temp_files(root, store) == []

    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout)["steps_executed"] == 0


def test_ctrl_c_during_a_step_with_an_upload_in_flight(project):
    """The case of #249: the interrupt arrives while steps run and the helper is uploading."""
    store, make = project
    root = make("mid-run")
    (root / "pipeline.py").write_text(
        PIPELINE
        + """

import time
from pathlib import Path


@asset(inputs={"numbers": numbers})
def slow(numbers: list) -> int:
    Path("slow.started").write_text("x")
    while not Path("release-slow").exists():
        time.sleep(0.02)
    return len(numbers)
"""
    )
    run = Interrupted(root, store, "put", "get", "slow", "--json", "--agent")
    wait_until((root / "slow.started").exists, "the second step to start", run.proc)
    run.ctrl_c()

    cancelled_cleanly(run, root, store)
    assert recorded(root) == {}  # `numbers` was never confirmed in the store
    assert "cancelled after" in run.stderr and "| done in" not in run.stderr, run.stderr
