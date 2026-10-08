"""Ctrl-C and the workers (#292), end to end through the binary.

A terminal sends Ctrl-C to every process of the job, workers included. What a worker does with
it depends on where it is (`python/barca/_worker.py`, "What a worker does with Ctrl-C"):

- its interpreter is starting, or it is importing barca, or connecting: nothing;
- it is importing a step's module, or is idle between steps: nothing;
- it is running a step: the step gets `KeyboardInterrupt`, as before. (The coordinator stops
  the worker right after, so the end-to-end test does not assert that the step's own handling
  of the interrupt ran to its end.)

In every case the command exits 130 promptly with the `cancelled` error, prints no traceback,
records the run as cancelled and leaves no process behind. Before the fix, an interrupt during
start-up ended the worker with a traceback and the coordinator then waited ten seconds for it
to connect.

Each test holds a worker at the stage it is about (a hook in python/tests/hold/, or the
pipeline's own code waiting for a file), waits until it is there, and only then sends the
signal to the process group.
"""

import json
import os
import signal
import subprocess
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

from .test_remote_cancel import HOLD_SHIM, alive, wait_until

# The coordinator used to wait this long for a worker that an interrupt had ended.
OLD_CONNECT_WAIT = 10
# Generous for a loaded machine, and well under the old wait.
PROMPT = 5

PIPELINE = """
import os
import time
from pathlib import Path

from barca import asset


def wait_for(name):
    while not Path(name).exists():
        time.sleep(0.02)


if os.environ.get("BARCA_WORKER") and Path("hold-import").exists():
    try:
        Path("import.started").write_text(str(os.getpid()))
        wait_for("release")
    except KeyboardInterrupt:
        Path("import.interrupted").write_text("x")
        raise


@asset()
def quick() -> int:
    return 1


@asset()
def held() -> int:
    Path("step.started").write_text(str(os.getpid()))
    wait_for("release")
    return 2
"""


class Job:
    """`barca get [<target>]` as a foreground job: in a process group of its own."""

    def __init__(self, root: Path, target: str | None, hold: str = "", pool: int = 2):
        self.root = root
        self.hold_dir = root / "hold"
        self.hold_dir.mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
        env["BARCA_POOL_SIZE"] = str(pool)
        if hold:
            env["PYTHONPATH"] = HOLD_SHIM + os.pathsep + env.get("PYTHONPATH", "")
            env["BARCA_TEST_HOLD"] = f"{hold}:{self.hold_dir}"
        self.err_path = root / "stderr.txt"
        with open(self.err_path, "w") as err:
            self.proc = subprocess.Popen(
                [_find_binary(), "get", *([target] if target else []), "pipeline.py", "--agent"],
                cwd=root,
                env=env,
                stdout=subprocess.PIPE,
                stderr=err,
                text=True,
                start_new_session=True,
            )

    def wait(self, done, what: str) -> None:
        try:
            wait_until(done, what, self.proc)
        except AssertionError as failed:
            raise AssertionError(f"{failed}\n--- stderr:\n{self.err_path.read_text()}") from None

    def ctrl_c(self) -> None:
        self.members = self.group()
        self.sent = time.monotonic()
        os.killpg(self.proc.pid, signal.SIGINT)

    def group(self) -> list[int]:
        """Every process of the job: barca and its workers."""
        out = subprocess.run(
            ["pgrep", "-g", str(self.proc.pid)], capture_output=True, text=True
        ).stdout
        return [int(pid) for pid in out.split()]

    def end(self) -> str:
        self.stdout, _ = self.proc.communicate(timeout=60)
        self.took = time.monotonic() - self.sent
        self.stderr = self.err_path.read_text()
        return self.stderr


def cancelled_promptly_and_quietly(job: Job) -> None:
    err = job.end()
    assert job.proc.returncode == 130, err
    for sign in ("Traceback", "KeyboardInterrupt", "failed to spawn", "Error"):
        assert sign not in err, err
    envelope = json.loads(err.strip().splitlines()[-1])
    assert (envelope["kind"], envelope["code"]) == ("cancelled", 130), envelope
    assert job.stdout == ""
    assert job.took < PROMPT < OLD_CONNECT_WAIT, f"took {job.took:.1f}s\n{err}"
    # The job had workers when it was interrupted, and none of them is left.
    assert len(job.members) >= 2, job.members
    wait_until(
        lambda: not any(alive(pid) for pid in job.members), "every process of the job to exit"
    )
    assert job.group() == []
    history = subprocess.run(
        [_find_binary(), "history", "--json"], cwd=job.root, capture_output=True, text=True
    )
    assert history.returncode == 0, history.stderr
    assert json.loads(history.stdout)["runs"][0]["status"] == "cancelled"


@pytest.fixture
def root(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


@pytest.mark.parametrize("point", ["worker-start", "worker-connect"])
def test_ctrl_c_while_a_worker_starts(root, point):
    job = Job(root, "held", hold=point)
    job.wait((job.hold_dir / f"{point}.started").exists, f"a worker reached '{point}'")
    job.ctrl_c()
    cancelled_promptly_and_quietly(job)
    assert not (root / "step.started").exists()


def test_ctrl_c_while_a_worker_imports_the_steps_module(root):
    (root / "hold-import").write_text("")
    job = Job(root, "held")
    job.wait((root / "import.started").exists, "the worker to import the module")
    job.ctrl_c()
    cancelled_promptly_and_quietly(job)
    # The import was not interrupted from inside: the worker was stopped by the coordinator.
    assert not (root / "import.interrupted").exists()
    assert not (root / "step.started").exists()


def test_ctrl_c_while_a_worker_is_idle_between_steps(root):
    # Two workers: one runs `quick` and is then idle, the other is in `held`.
    job = Job(root, None, pool=2)
    job.wait((root / "step.started").exists, "the held step to start")
    job.wait(lambda: "quick completed" in job.err_path.read_text(), "the quick step to be reported")
    assert len(job.group()) == 3, job.group()  # barca and two workers
    job.ctrl_c()
    cancelled_promptly_and_quietly(job)


def test_ctrl_c_during_a_step(root):
    job = Job(root, "held")
    job.wait((root / "step.started").exists, "the step to start")
    job.ctrl_c()
    cancelled_promptly_and_quietly(job)
    # An interrupted step is left out of the run: it is neither a success nor a failure.
    assert "failed" not in job.stderr


# ─── the worker's own switch, without a coordinator ──────────────────────────

STAGES = """
import os, signal, sys
from pathlib import Path
from barca import _runtime, _worker

reported = []
_runtime.emit_step_error = lambda **kw: reported.append(kw["error_type"])
_runtime.emit_step_completed = lambda node_id, artifact: reported.append("completed")
_worker._use_socket = True
root = Path(sys.argv[1])


def interrupt_me():
    os.kill(os.getpid(), signal.SIGINT)
    for _ in range(1000):  # give the interpreter every chance to deliver it
        pass


def run(name, body):
    source = root / f"{name}.py"
    source.write_text(body)
    step = {"node_id": f"{name}.py:step", "function_name": "step", "source_file": str(source),
            "kind": "asset", "inputs": {}, "run_hash": "h"}
    ok = _worker._run_daemon_step(step, {}, str(root / "arts"), _worker._ArtifactLRU())
    interrupt_me()  # between steps
    return ok


_worker._ctrl_c_does_nothing()  # as run_daemon does first
interrupt_me()  # before any step
send = "import os, signal\\nos.kill(os.getpid(), signal.SIGINT)\\nfor _ in range(1000): pass\\n"
print("import", run("at_import", send + "def step():\\n    return 1\\n"))
print("step", run("in_step", "import os, signal\\ndef step():\\n    os.kill(os.getpid(), signal.SIGINT)\\n    for _ in range(1000): pass\\n    return 1\\n"))
print("after", run("after", "def step():\\n    return 1\\n"))
print(reported)
"""


def test_a_worker_acts_on_ctrl_c_only_while_a_step_runs(tmp_path):
    """An interrupt before the first step, during a module's import and between steps does
    nothing; one during a step is that step's KeyboardInterrupt, and the worker goes back to
    doing nothing with them once the step is reported."""
    import sys

    proc = subprocess.run(
        [sys.executable, "-c", STAGES, str(tmp_path)], capture_output=True, text=True, timeout=120
    )
    assert proc.returncode == 0, proc.stderr
    assert "Traceback" not in proc.stderr, proc.stderr
    assert proc.stdout.splitlines() == [
        "import True",
        "step False",
        "after True",
        "['completed', 'KeyboardInterrupt', 'completed']",
    ], proc.stdout + proc.stderr
