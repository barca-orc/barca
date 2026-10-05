"""A local copy of a stored result is checked against the hash recorded when it was uploaded.

Two working directories share one plain-directory store, so this needs no emulator: the checks
are the same for every backend, since they hash the local file.
"""

import json
import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest

from .test_remote_inspect import SCRUB, _find_binary

PIPELINE = """
from barca import asset


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"sum": sum(numbers)}
"""


def cli(cwd: Path, store: Path, *args: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**env, "BARCA_REMOTE_URI": str(store)},
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


@pytest.fixture
def shared(tmp_path):
    """(store, machine factory); `numbers` and `total` are already in the store."""
    store = tmp_path / "store"

    def make(name: str) -> Path:
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        return root

    first = cli(make("producer"), store, "get", "total", "--json")
    assert first.returncode == 0, first.stderr
    return store, make


def _one(root: Path, pattern: str) -> Path:
    (path,) = root.glob(pattern)
    return path


def test_the_uploaded_hash_is_recorded_with_the_row(shared, tmp_path):
    store, _ = shared
    import hashlib
    import sqlite3

    artifact = _one(store, "default/artifacts/*numbers*/*.json")
    db = sqlite3.connect(tmp_path / "producer" / ".barca" / "metadata.db")
    (recorded,) = db.execute(
        "select output_hash from materializations where node_id like '%:numbers'"
    ).fetchone()
    assert recorded == hashlib.sha256(artifact.read_bytes()).hexdigest()


def test_a_changed_local_copy_is_replaced_before_a_step_reads_it(shared):
    store, make = shared
    root = make("reader")
    assert cli(root, store, "get", "total", "--json").returncode == 0
    # A new step makes this machine read `numbers`, whose copy it now holds is wrong.
    assert cli(root, store, "get", "numbers", "--json").returncode == 0
    _one(root, ".barca/artifacts/*numbers*/*.json").write_text("[9, 9, 9]")
    (root / "pipeline.py").write_text(
        PIPELINE + '\n\n@asset(inputs={"numbers": numbers})\ndef doubled(numbers: list) -> list:\n'
        "    return [n * 2 for n in numbers]\n"
    )

    proc = cli(root, store, "get", "doubled", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == [2, 4, 6]
    assert "fetched 1 cached artifact" in proc.stderr, proc.stderr
    assert _one(root, ".barca/artifacts/*numbers*/*.json").read_text() == "[1, 2, 3]"


def test_an_intact_local_copy_is_not_fetched_again(shared):
    store, make = shared
    root = make("reader")
    assert cli(root, store, "get", "total", "--json").returncode == 0
    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    assert "fetched" not in again.stderr, again.stderr


def test_a_store_copy_that_differs_from_the_recorded_hash_is_used_with_a_warning(shared):
    # An artifact path is {node}/{run_hash}, so the object can be overwritten legitimately.
    store, make = shared
    _one(store, "default/artifacts/*total*/*.json").write_text('{"sum": 7}')
    root = make("reader")
    proc = cli(root, store, "get", "total", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == {"sum": 7}
    assert "warning" in proc.stderr and "total" in proc.stderr, proc.stderr
    assert "--refresh pipeline.py:total" in proc.stderr, proc.stderr

    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    assert "fetched" not in again.stderr, again.stderr


NONDETERMINISTIC = """
import os
import time
import uuid

from barca import asset


@asset()
def up() -> dict:
    return {"id": uuid.uuid4().hex}


@asset(inputs={"up": up})
def slow(up: dict) -> dict:
    time.sleep(float(os.environ.get("SLOW", "0")))
    return up


@asset(inputs={"up": up})
def quick(up: dict) -> dict:
    return up
"""


def _machine(tmp_path: Path, name: str) -> Path:
    root = tmp_path / name
    root.mkdir()
    (root / "pipeline.py").write_text(NONDETERMINISTIC)
    return root


def _start(cwd: Path, store: Path, *args: str, **env: str) -> subprocess.Popen:
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.Popen(
        [_find_binary(), *args],
        cwd=cwd,
        env={**base, "BARCA_REMOTE_URI": str(store), **env},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _finish(proc: subprocess.Popen) -> subprocess.CompletedProcess:
    out, err = proc.communicate(timeout=120)
    return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)


def test_overlapping_runs_of_the_same_step_leave_other_machines_working(tmp_path):
    """Two machines compute `up` at once: the store holds one result, the newest row the other."""
    store = tmp_path / "store"
    b = _start(_machine(tmp_path, "b"), store, "get", "slow", "--json", SLOW="6")
    time.sleep(2.5)
    a = _finish(_start(_machine(tmp_path, "a"), store, "get", "quick", "--json"))
    assert a.returncode == 0, a.stderr
    b = _finish(b)
    assert b.returncode == 0, b.stderr
    assert "conflict" in b.stderr, "the runs did not overlap: " + b.stderr

    for root in (tmp_path / "a", _machine(tmp_path, "c")):
        proc = cli(root, store, "get", "up", "--json")
        assert proc.returncode == 0, f"{root.name}: {proc.stderr}"
        assert "id" in json.loads(proc.stdout)["final_output"]


def test_a_refresh_killed_after_its_upload_does_not_lock_the_step(tmp_path):
    store = tmp_path / "store"
    root = _machine(tmp_path, "d")
    assert cli(root, store, "get", "slow", "--json").returncode == 0
    killed = _start(root, store, "get", "slow", "--refresh", "up", "--json", SLOW="30")
    time.sleep(5)
    killed.kill()
    killed.communicate(timeout=30)

    proc = cli(root, store, "get", "up", "--json")
    assert proc.returncode == 0, proc.stderr
    assert "id" in json.loads(proc.stdout)["final_output"]


def test_independent_histories_sharing_a_store_survive_a_deleted_local_copy(tmp_path):
    store = tmp_path / "store"
    b, a = _machine(tmp_path, "b"), _machine(tmp_path, "a")

    def get(root: Path, target: str) -> subprocess.CompletedProcess:
        return _finish(_start(root, store, "get", target, "--json", BARCA_STATE="off"))

    assert get(b, "up").returncode == 0
    assert get(a, "up").returncode == 0
    shutil.rmtree(b / ".barca" / "artifacts")
    for target in ("up", "quick"):
        proc = get(b, target)
        assert proc.returncode == 0, f"{target}: {proc.stderr}"
