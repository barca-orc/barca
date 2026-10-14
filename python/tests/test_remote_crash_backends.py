"""Real coordinator SIGKILL at SDK artifact visibility boundaries (#248)."""

import hashlib
import json
import os
import signal
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

import pytest
from barca import _storage
from barca.api import _find_binary

from . import emulators
from .test_remote_env_config import CASES, SCRUB, _reachable, barca

HOLD = str(Path(__file__).parent / "hold")
PIPELINE = """import os
from barca import asset
@asset()
def blob() -> dict:
    return {"value": os.environ.get("VALUE", "old"), "padding": "x" * 262144}
@asset(inputs={"blob": blob})
def observe(blob: dict) -> dict:
    return {"value": blob["value"], "length": len(blob["padding"])}
@asset()
def unrelated() -> int:
    return 17
"""


def wait_for(check, message, proc=None, seconds=30):
    deadline = time.monotonic() + seconds
    while not check():
        assert proc is None or proc.poll() is None, f"process exited before {message}"
        assert time.monotonic() < deadline, f"timed out waiting for {message}"
        time.sleep(0.02)


def running(pid):
    # An orphan may briefly await reaping; a zombie no longer owns resources.
    if not Path("/proc").is_dir():
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False
    try:
        return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0] != "Z"
    except FileNotFoundError:
        return False


@pytest.fixture(params=["s3", "gcs", "azure"])
def remote(request, monkeypatch):
    endpoint, make = CASES[request.param]
    emulators.require(request.param, _reachable(endpoint), endpoint)
    for key in os.environ:
        if key.startswith(SCRUB) and not key.startswith("BARCA_TEST_"):
            monkeypatch.delenv(key)
    uri, options = make()
    options = {**options, "BARCA_REMOTE_URI": uri}
    for key, value in options.items():
        monkeypatch.setenv(key, value)
    _storage._fs_cache.clear()
    yield options
    _storage._fs_cache.clear()


def machine(tmp_path, name):
    root = tmp_path / name
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    return root


def bytes_at(remote, tmp_path):
    local = tmp_path / "inspected-object.json"
    # fsspec reads FSSPEC_* once at import. A fresh real client, like the CLI's
    # helper, observes this backend's environment instead of the pytest process's
    # earlier client configuration (potentially from a different test backend).
    subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys; from barca import _storage; _storage.get_file(sys.argv[1], sys.argv[2])",
            remote,
            str(local),
        ],
        check=True,
        capture_output=True,
        timeout=60,
    )
    return local.read_bytes()


@contextmanager
def held_run(root, options, point, *args):
    gate = root / "gate"
    gate.mkdir()
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env = {
        **base,
        **options,
        "PYTHONPATH": HOLD + os.pathsep + base.get("PYTHONPATH", ""),
        "BARCA_TEST_HOLD": f"{point}:{gate}",
    }
    with (gate / "stdout").open("w") as out, (gate / "stderr").open("w") as err:
        proc = subprocess.Popen(
            [_find_binary(), *args, "pipeline.py", "--json"],
            cwd=root,
            env=env,
            stdout=out,
            stderr=err,
            start_new_session=True,
        )
        try:
            wait_for(lambda: (gate / f"{point}.started").exists(), point, proc)
            yield proc, gate
        finally:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.wait(timeout=10)


def kill_coordinator(proc, gate, point):
    helper = int((gate / f"{point}.started").read_text())
    assert running(helper), "marker must identify a live held helper"
    proc.kill()
    assert proc.wait(timeout=10) == -signal.SIGKILL
    # No synthetic exception or signal to the helper: the real lifeline owns cleanup.
    wait_for(lambda: not running(helper), "helper lifeline exit", seconds=10)


def baseline(root, options):
    unrelated = barca(root, options, "get", "unrelated")["run_id"]
    first = barca(root, options, "get", "blob")
    node = barca(root, options, "status", "blob")["nodes"][0]
    return unrelated, first["run_id"], node["cache"]["artifact"]


def assert_history(root, options, preserved):
    rows = barca(root, options, "history", "--all")["runs"]
    assert preserved <= {row["run_id"] for row in rows}


@pytest.mark.parametrize("point", ["put", "uploaded"])
def test_refresh_killed_before_or_after_real_sdk_upload(remote, tmp_path, point):
    producer = machine(tmp_path, "producer")
    unrelated, first, artifact = baseline(producer, remote)
    old = bytes_at(artifact, tmp_path)
    with held_run(
        producer, {**remote, "VALUE": "new"}, point, "get", "blob", "--refresh", "blob"
    ) as (
        proc,
        gate,
    ):
        staged_value = json.loads(bytes_at(artifact, tmp_path))
        expected = "old" if point == "put" else "new"
        assert staged_value == {"value": expected, "padding": "x" * 262144}
        kill_coordinator(proc, gate, point)
        after = bytes_at(artifact, tmp_path)
        assert json.loads(after) == staged_value
        assert (after == old) == (point == "put")
    reader = machine(tmp_path, "reader")
    observed = barca(reader, remote, "get", "observe")
    assert observed["final_output"] == {"value": expected, "length": 262144}
    blob = next(s for s in observed["steps"] if s["id"].endswith(":blob"))
    assert blob.get("artifact_mismatch", False) == (point == "uploaded")
    assert_history(reader, remote, {unrelated, first})
    repaired = barca(producer, remote, "get", "blob", "--refresh", "blob")
    assert repaired["final_output"]["value"] == "old"
    final = barca(machine(tmp_path, "final"), remote, "get", "blob")
    assert final["final_output"]["value"] == "old"
    assert all("artifact_mismatch" not in step for step in final["steps"])


def test_killed_sdk_download_preserves_old_destination_until_complete_install(remote, tmp_path):
    producer = machine(tmp_path, "producer")
    unrelated, first, artifact = baseline(producer, remote)
    complete = bytes_at(artifact, tmp_path)
    reader = machine(tmp_path, "reader")
    # A complete stale copy forces verification to download rather than keep local.
    relative = artifact.split("/default/artifacts/", 1)[1]
    local = reader / ".barca" / "artifacts" / relative
    local.parent.mkdir(parents=True)
    old = json.dumps({"value": "stale", "padding": "y" * 262144}).encode()
    local.write_bytes(old)
    assert hashlib.sha256(old).digest() != hashlib.sha256(complete).digest()
    with held_run(reader, remote, "partial-get", "get", "observe") as (proc, gate):
        stage = Path((gate / "partial-get.path").read_text())
        assert stage.parent == local.parent and stage.name.endswith(".tmp")
        prefix = stage.read_bytes()
        assert 0 < len(prefix) <= 65536 < len(complete)
        assert len(prefix) < len(complete) and complete.startswith(prefix)
        assert local.read_bytes() == old
        assert bytes_at(artifact, tmp_path) == complete
        kill_coordinator(proc, gate, "partial-get")
        assert local.read_bytes() == old
        assert not stage.exists()
        assert not list(local.parent.glob("*.tmp"))
    resumed = barca(reader, remote, "get", "observe")
    assert resumed["final_output"] == {"value": "old", "length": 262144}
    assert local.read_bytes() == complete
    assert_history(reader, remote, {unrelated, first})


def test_partial_write_shim_preserves_counts_and_ignores_other_files(tmp_path):
    """Child-only shim normal release, including another download on another thread."""
    gate = tmp_path / "gate"
    gate.mkdir()
    script = r"""
import sys, threading
from pathlib import Path
from types import SimpleNamespace
from barca import _storage
sys.argv[0] = "_transfer.py"
root = Path(sys.argv[1])
body = b"x" * 200000
def sdk_get(remote, dest):
    with open(dest, "wb") as file:
        count = file.write(memoryview(body))
    assert count == len(body)
_storage.get_fs = lambda remote: SimpleNamespace(get_file=sdk_get)
def other():
    # Wait for the real first write's hold, then exercise unrelated SDK/file writes.
    import time
    while not (root / "gate" / "partial-get.started").exists(): time.sleep(.01)
    _storage.get_file("s3://bucket/other", root / "other.tmp")
    with open(root / "metadata.tmp", "wb") as file: assert file.write(body) == len(body)
    (root / "unrelated.done").write_text("yes")
thread = threading.Thread(target=other)
thread.start()
_storage.get_file("s3://bucket/selected", root / "selected.tmp")
thread.join(5)
assert not thread.is_alive()
assert (root / "selected.tmp").read_bytes() == body
"""
    env = {
        **os.environ,
        "PYTHONPATH": HOLD + os.pathsep + os.environ.get("PYTHONPATH", ""),
        "BARCA_TEST_HOLD": f"partial-get:{gate}",
    }
    proc = subprocess.Popen([sys.executable, "-c", script, str(tmp_path)], env=env)
    try:
        wait_for(lambda: (gate / "partial-get.started").exists(), "partial write", proc)
        assert (tmp_path / "selected.tmp").read_bytes() == b"x" * 65536
        wait_for(lambda: (tmp_path / "unrelated.done").exists(), "unrelated writes", proc)
        assert (tmp_path / "other.tmp").read_bytes() == b"x" * 200000
        assert (tmp_path / "metadata.tmp").read_bytes() == b"x" * 200000
        (gate / "release").touch()
        assert proc.wait(timeout=10) == 0
    finally:
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=10)
