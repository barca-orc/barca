"""Remote startup and upload reporting through the real CLI and HTTP server."""

import json
import os
import sqlite3
import subprocess
import time

import pytest
from barca.api import _find_binary

from . import test_serve_robustness as serving


@pytest.fixture()
def project(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from pathlib import Path\nfrom barca import asset\n"
        'Path("user.imported").touch()\n@asset()\ndef quick():\n    Path("user.started").touch()\n    return 1\n'
    )
    (tmp_path / "barca.toml").write_text(
        '[remote]\nuri = "s3://bucket/project"\nstate = "off"\ntransfer_timeout = 1\n'
    )
    shim = tmp_path / "shim"
    shim.mkdir()
    (shim / "sitecustomize.py").write_text("""
import os, time
from barca import _storage

def check(root):
    from pathlib import Path
    Path("helper.pid").write_text(str(os.getpid()))
    mode = os.environ.get("PREFLIGHT_MODE", "ok")
    if mode == "denied":
        raise PermissionError("bad credentials")
    if mode == "stall":
        time.sleep(60)

def put(local, remote):
    time.sleep(float(os.environ.get("UPLOAD_DELAY", "0.3")))
    if os.environ.get("UPLOAD_FAIL"):
        raise PermissionError("upload denied")

_storage.check_store = check
_storage.put_file = put
""")
    env = serving._env()
    env["PYTHONPATH"] = str(shim) + os.pathsep + os.environ.get("PYTHONPATH", "")
    return tmp_path, env


def run(root, env):
    return subprocess.run(
        [_find_binary(), "get", "quick", "pipeline.py", "--agent"],
        cwd=root,
        env=env,
        text=True,
        capture_output=True,
        check=False,
        timeout=15,
    )


@pytest.mark.parametrize("mode,detail", [("denied", "bad credentials"), ("stall", "timed out")])
def test_state_off_rejects_bad_store_before_importing_user_code(project, mode, detail):
    root, env = project
    env["PREFLIGHT_MODE"] = mode
    started = time.monotonic()
    out = run(root, env)
    assert out.returncode == 3, out.stderr
    assert time.monotonic() - started < 8
    assert "[barca] checking artifact store s3://bucket/project" in out.stderr
    assert detail in out.stderr
    assert not (root / "user.started").exists()
    assert not (root / "user.imported").exists()
    with pytest.raises(ProcessLookupError):
        os.kill(int((root / "helper.pid").read_text()), 0)
    envelope = json.loads(out.stderr.splitlines()[-1])
    assert envelope["kind"] == "infra"
    with sqlite3.connect(root / ".barca/metadata.db") as db:
        assert db.execute("SELECT status, steps_executed FROM runs").fetchall() == [("failed", 0)]


def test_serve_records_preflight_failure_without_running_user_code(project, monkeypatch):
    root, env = project
    env["PREFLIGHT_MODE"] = "denied"
    monkeypatch.setattr(serving, "_env", lambda: env)
    server = serving.Server(root, "--no-schedule")
    try:
        status, accepted = server.request("POST", "/get/quick")
        assert status == 200, accepted

        def failed():
            status, body = server.request("GET", f"/runs/{accepted['run_id']}")
            return body if status == 200 and body.get("run", {}).get("status") == "failed" else None

        body = serving.wait_for(failed, "a failed preflight run")
        assert "bad credentials" in json.dumps(body)
        assert not (root / "user.started").exists()
        assert not (root / "user.imported").exists()
    finally:
        server.stop()


def test_upload_wait_and_failure_attempts_are_reported(project):
    root, env = project
    env["UPLOAD_FAIL"] = "1"
    out = run(root, env)
    assert out.returncode == 3, out.stderr
    assert "[barca] uploading 1 artifacts to s3://bucket/project" in out.stderr
    assert "timeout 1s per attempt" in out.stderr
    assert "1 attempt(s)" in out.stderr
    assert "upload denied" in out.stderr
    assert (root / "user.started").exists()


def test_first_run_can_create_a_directory_store(project):
    root, env = project
    (root / "barca.toml").write_text('[remote]\nuri = "store"\nstate = "off"\n')
    # No probe shim: exercise actual local storage operations.
    env["PYTHONPATH"] = os.environ.get("PYTHONPATH", "")
    out = run(root, env)
    assert out.returncode == 0, out.stderr
    assert list((root / "store").rglob("*.json"))


def test_transfer_errors_redact_uri_and_storage_secrets(monkeypatch):
    from barca import _storage

    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", '{"s3":{"secret":"TOPSECRET"}}')
    assert _storage.safe_error("failed s3://user:pass@bucket/path?token=signed TOPSECRET") == (
        "failed s3://<redacted>@bucket/path <redacted>"
    )


def test_upload_progress_is_visible_while_the_drain_is_still_running(project):
    root, env = project
    (root / "barca.toml").write_text(
        '[remote]\nuri = "s3://bucket/project"\nstate = "off"\ntransfer_timeout = 15\n'
    )
    env["UPLOAD_DELAY"] = "11"
    with subprocess.Popen(
        [_find_binary(), "get", "quick", "pipeline.py", "--agent"],
        cwd=root,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ) as proc:
        lines = []
        for line in proc.stderr:
            lines.append(line)
            if "still waiting for artifact uploads" in line:
                assert proc.poll() is None
                break
        stdout, stderr = proc.communicate(timeout=10)
        assert proc.returncode == 0, "".join(lines) + stderr
        assert "[barca] uploading 1 artifacts" in "".join(lines)
        assert "still waiting for artifact uploads" in "".join(lines)
        assert "uploaded 1 artifact" in stderr
        assert json.loads(stdout)["steps_executed"] == 1
