"""A caller of `parallel()` only ever reads what its own branches wrote (#332), and what they
wrote is gone when the run is.

Branch results used to be files in the artifact directory named after the branch's file, its
function and its number within the phase. Two runs of one pipeline at the same time wrote and
read the same files: a caller received another run's results, silently. Nothing removed the
files either.

Now each run writes them under `.barca/branches/<run>-<pid>/<group>/`. The coordinator removes
a group's directory when the step that called `parallel()` ends, the run's directory when the
run ends (success, failure, cancellation), and the directories of killed runs when the next
run starts.

Every run here is the real binary. The branches of the two callers are the same function
with the same branch numbers and differ only in an argument they echo back, so a result that
came from the other run is visible in the value itself.
"""

import json
import os
import signal
import socket
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary
from barca.client import Client

from .test_remote_cancel import alive, wait_until

BRANCHES = 120
# How often the concurrent pairs are run. On 0.18.1 cross-talk showed in most single pairs.
ROUNDS = 8

PIPELINE = f"""
import os
import time
from functools import partial
from pathlib import Path

from barca import asset, parallel, parallel_map, task

N = {BRANCHES}


@task()
def echo(i: int, tag: str) -> dict:
    return {{"tag": tag, "i": i}}


@task()
def echo_big(i: int, tag: str) -> list:
    # Over the size whose text travels with the answer: the caller reads the file.
    return [tag] * 2000 + [i]


def check(tag, results):
    wrong = [r for i, r in enumerate(results) if r != {{"tag": tag, "i": i}}]
    return {{"tag": tag, "count": len(results), "wrong": wrong[:5]}}


def check_big(tag, results):
    wrong = [i for i, r in enumerate(results) if r != [tag] * 2000 + [i]]
    return {{"tag": tag, "count": len(results), "wrong": wrong[:5]}}


@task()
def fan_a() -> dict:
    return check("a", parallel_map(echo, list(range(N)), tag="a"))


@task()
def fan_b() -> dict:
    return check("b", parallel_map(echo, list(range(N)), tag="b"))


@task()
def big_a() -> dict:
    return check_big("a", parallel_map(echo_big, list(range(N)), tag="a"))


@task()
def big_b() -> dict:
    return check_big("b", parallel_map(echo_big, list(range(N)), tag="b"))


@task()
def repeated() -> list:
    # The same call three times in one group, and the same group twice in one step.
    first = parallel(*(partial(echo, 7, "x") for _ in range(3)))
    second = parallel_map(echo, [7, 7, 7], tag="y")
    return [first, second]


@task()
def fails_after() -> dict:
    parallel_map(echo, list(range(20)), tag="a")
    raise RuntimeError("after the branches")


@task()
def held(i: int) -> int:
    Path(f"held.{{i}}.started").write_text(str(os.getpid()))
    while not Path("release").exists():
        time.sleep(0.02)
    return i


@task()
def waits() -> list:
    Path("caller.pid").write_text(str(os.getpid()))
    # Twenty that finish and write their results (too large to travel in a message), and one
    # that holds the group open.
    results = parallel(*(partial(echo_big, i, "a") for i in range(20)), partial(held, 0))
    return [r[-1] for r in results[:-1]] + [results[-1]]
"""


def env(pool: int | None = None) -> dict:
    e = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    e["BARCA_POOL_SIZE"] = str(pool or os.environ.get("BARCA_POOL_SIZE", 4))
    return e


def start(root: Path, *args: str, pool: int | None = None) -> subprocess.Popen:
    return subprocess.Popen(
        [_find_binary(), *args],
        cwd=root,
        env=env(pool),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )


def finish(proc: subprocess.Popen, timeout: float = 180) -> tuple[int, str, str]:
    try:
        out, err = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
        raise AssertionError(f"barca did not finish\n{err}") from None
    return proc.returncode, out, err


def branch_files(root: Path) -> list[str]:
    """Every branch result under `.barca`: the files of the per-run directories, and files
    named the way branch results were named before."""
    barca = root / ".barca"
    found = [p for p in (barca / "branches").rglob("*") if p.is_file()]
    found += [p for p in barca.rglob("*_branch_*") if p.is_file()]
    return sorted(str(p.relative_to(barca)) for p in found)


def files(root: Path) -> list[str]:
    """Every file under `.barca` except the database (its side files come and go), barca's
    own `.gitignore` and the workers' staging directories (swept by the next worker)."""
    barca = root / ".barca"
    return sorted(
        str(p.relative_to(barca))
        for p in barca.rglob("*")
        if p.is_file()
        and not p.name.startswith("metadata.db")
        and p.name != ".gitignore"
        and "staging" not in p.parts
    )


@pytest.fixture
def root(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


# ─── no cross-talk ───────────────────────────────────────────────────────────


# "small": results that travel in messages. "big": results the caller reads from files.
@pytest.mark.parametrize("targets", [("fan_a", "fan_b"), ("big_a", "big_b")], ids=["small", "big"])
def test_two_barca_processes_in_one_project_do_not_read_each_others_branches(root, targets):
    for round_ in range(ROUNDS):
        procs = [start(root, "run", t, "pipeline.py") for t in targets]
        for target, proc in zip(targets, procs):
            code, out, err = finish(proc)
            assert code == 0, err
            got = json.loads(out)["final_output"]
            assert got == {"tag": target[-1], "count": BRANCHES, "wrong": []}, (round_, got)
        assert branch_files(root) == []


def free_port() -> int:
    """A free TCP port above 20000."""
    for port in range(20000 + os.getpid() % 20000, 60000):
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
            except OSError:
                continue
            return port
    raise AssertionError("no free port")


@pytest.fixture
def server(root):
    port = free_port()
    # Its output goes to a file: nobody reads a pipe here, and a server prints a line a step.
    log = open(root / "serve.log", "w")
    proc = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port), "--no-schedule"],
        cwd=root,
        env=env(),
        stdout=log,
        stderr=log,
        start_new_session=True,
    )
    client = Client(f"http://127.0.0.1:{port}")

    def up() -> bool:
        try:
            return client.health().get("status") == "ok"
        except Exception:
            return False

    try:
        wait_until(up, "the server to answer", proc)
        yield client
    finally:
        os.killpg(proc.pid, signal.SIGINT)
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
        log.close()


@pytest.mark.parametrize("targets", [("fan_a", "fan_b"), ("big_a", "big_b")], ids=["small", "big"])
def test_two_runs_under_one_barca_serve_do_not_read_each_others_branches(root, server, targets):
    for round_ in range(ROUNDS):
        runs = [server.run(t) for t in targets]
        for target, run in zip(targets, runs):
            status = run.wait(timeout=180, poll=0.05)
            assert status["status"] == "complete", status
            # The server answers with where the task's result is, not with the value.
            got = json.loads((root / status["result"]["final_output"]["path"]).read_text())
            assert got == {"tag": target[-1], "count": BRANCHES, "wrong": []}, (round_, got)
        wait_until(lambda: branch_files(root) == [], "both runs' branch results to be removed")


def test_the_same_call_repeated_in_a_group_and_the_same_group_twice(root):
    code, out, err = finish(start(root, "run", "repeated", "pipeline.py"))
    assert code == 0, err
    assert json.loads(out)["final_output"] == [
        [{"tag": "x", "i": 7}] * 3,
        [{"tag": "y", "i": 7}] * 3,
    ]


# ─── nothing is left behind ──────────────────────────────────────────────────


@pytest.mark.parametrize("target", ["fan_a", "big_a"])
def test_branch_results_are_gone_after_a_run_and_a_second_run_adds_nothing(root, target):
    code, _, err = finish(start(root, "run", target, "pipeline.py"))
    assert code == 0, err
    after_one = files(root)
    assert branch_files(root) == []
    # The caller's own result, and nothing per branch.
    assert len(after_one) == 1, after_one
    code, _, err = finish(start(root, "run", target, "pipeline.py"))
    assert code == 0, err
    assert files(root) == after_one


def test_branch_results_are_gone_after_a_failed_run(root):
    code, out, err = finish(start(root, "run", "fails_after", "pipeline.py"))
    assert code == 1, err
    assert "after the branches" in json.loads(out)["error"]
    assert branch_files(root) == [] and files(root) == []


def held_group(root: Path) -> subprocess.Popen:
    """`barca run waits`, up to the point where twenty branch results are written and the
    group is still open."""
    proc = start(root, "run", "waits", "pipeline.py", pool=4)
    wait_until((root / "held.0.started").exists, "the held branch to start", proc)
    wait_until(lambda: len(branch_files(root)) >= 20, "twenty branch results on disk", proc)
    return proc


def test_branch_results_are_gone_after_ctrl_c(root):
    proc = held_group(root)
    workers = [int(p) for p in subprocess.run(
        ["pgrep", "-P", str(proc.pid)], capture_output=True, text=True).stdout.split()]  # fmt: skip
    os.killpg(proc.pid, signal.SIGINT)
    code, _, err = finish(proc)
    assert code == 130, err
    assert branch_files(root) == [] and files(root) == []
    wait_until(lambda: not any(alive(w) for w in workers), "the workers to exit")


def test_branch_results_of_a_killed_run_are_swept_by_the_next_run(root):
    proc = held_group(root)
    os.killpg(proc.pid, signal.SIGKILL)  # barca and every worker, the frozen caller included
    proc.communicate()
    left = branch_files(root)
    assert len(left) >= 20, left  # nobody was there to remove them

    (root / "release").write_text("")
    code, _, err = finish(start(root, "run", "fan_a", "pipeline.py"))
    assert code == 0, err
    assert branch_files(root) == []
    assert len(files(root)) == 1, files(root)


def test_a_live_runs_branch_results_are_not_swept_by_another_run_starting(root):
    """The sweep removes the results of dead runs only: a run that starts while another has a
    group open leaves that group's results alone, and the first run then reads them."""
    first = held_group(root)
    before = branch_files(root)
    code, out, err = finish(start(root, "run", "fan_b", "pipeline.py"))
    assert code == 0, err
    assert json.loads(out)["final_output"]["wrong"] == []
    assert set(before) <= set(branch_files(root))

    (root / "release").write_text("")
    code, out, err = finish(first)
    assert code == 0, err
    assert json.loads(out)["final_output"] == list(range(20)) + [0]
    assert branch_files(root) == []


# ─── a frozen caller that dies (#333) ────────────────────────────────────────


def test_a_caller_killed_while_its_branches_run_fails_its_step(root):
    """The step that called parallel() is frozen while its branches run. Killed then (by the
    out-of-memory killer, say), it used to go unnoticed, and the run never ended."""
    proc = held_group(root)
    caller = int((root / "caller.pid").read_text())
    os.kill(caller, signal.SIGKILL)
    (root / "release").write_text("")
    code, out, err = finish(proc, timeout=60)
    assert code == 1, (code, err)
    doc = json.loads(out)
    assert doc["failed_node"] == "pipeline.py:waits"
    assert "worker disconnected while it waited for its parallel() branches" in doc["error"]
    assert branch_files(root) == []
