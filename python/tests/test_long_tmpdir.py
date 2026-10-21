"""A long TMPDIR does not break a run (#557).

Barca's coordination sockets (coordinator <-> workers, coordinator <-> transfer helper) lived
under `$TMPDIR/barca-sockets/`, and a Unix socket path is limited to 108 bytes on Linux and 104
on macOS, so a `TMPDIR` longer than about 80 characters failed every run with "socket bind: path
must be shorter than SUN_LEN". Barca now falls back to `/tmp/barca-<uid>/` (0700) when the
`TMPDIR` path would not fit.
"""

import json
import os
import stat
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary

PIPELINE = """
from functools import partial

from barca import asset, parallel


def square(n: int) -> int:
    return n * n


@asset()
def total() -> dict:
    return {"total": sum(parallel(*[partial(square, n) for n in (1, 2, 3)]))}
"""


def _socket_dir() -> Path:
    return Path("/tmp") / f"barca-{os.getuid()}"


def _our_sockets() -> set:
    d = _socket_dir()
    return {p.name for p in d.iterdir()} if d.is_dir() else set()


@pytest.fixture
def long_tmpdir(tmp_path):
    d = tmp_path / ("t" * (150 - len(str(tmp_path)) - 1))
    d.mkdir()
    assert len(str(d)) == 150
    return d


@pytest.mark.parametrize("remote", [False, True], ids=["local", "transfer-helper"])
def test_a_run_with_a_150_character_tmpdir_succeeds(tmp_path, long_tmpdir, remote):
    project = tmp_path / "project"
    project.mkdir()
    (project / "pipeline.py").write_text(PIPELINE)
    env = {**os.environ, "TMPDIR": str(long_tmpdir), "BARCA_REMOTE": "off"}
    if remote:
        store = tmp_path / "store"
        store.mkdir()
        env.pop("BARCA_REMOTE")
        env["BARCA_ARTIFACT_URI"] = f"file://{store}"
    before = _our_sockets()
    proc = subprocess.run(
        [_find_binary(), "get", "total", "pipeline.py", "--json"],
        check=False,
        cwd=project,
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
    )
    assert proc.returncode == 0, proc.stderr
    assert "SUN_LEN" not in proc.stderr
    out = json.loads(proc.stdout)
    assert out["status"] == "success"
    artifact = project / out["final_output"]["_barca_artifact"]["path"]
    assert json.loads(artifact.read_text()) == {"total": 14}
    # The sockets went to the short private directory, and the run removed its own.
    mode = stat.S_IMODE(_socket_dir().stat().st_mode)
    assert mode == 0o700, oct(mode)
    assert _our_sockets() - before == set()
    if remote:
        assert any(store.rglob("*.json")), "the transfer helper uploaded nothing"
