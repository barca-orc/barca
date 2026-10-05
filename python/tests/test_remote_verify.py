"""A local copy of a stored result is checked against the hash recorded when it was uploaded.

Two working directories share one plain-directory store, so this needs no emulator: the checks
are the same for every backend, since they hash the local file.
"""

import json
import os
import subprocess
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


def test_a_changed_store_copy_stops_the_run_and_names_the_artifact(shared):
    store, make = shared
    _one(store, "default/artifacts/*total*/*.json").write_text('{"sum": 7}')
    proc = cli(make("reader"), store, "get", "total", "--json")
    assert proc.returncode != 0, proc.stdout
    assert "ChecksumMismatch" in proc.stderr and "total" in proc.stderr, proc.stderr
    assert '"sum": 7' not in proc.stdout and '"sum":7' not in proc.stdout
