"""Local bootstrap and real server token lifecycle, without a user-account system."""

from concurrent.futures import ThreadPoolExecutor
import json
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request

import pytest
from barca.api import _find_binary
from barca.client import Client

from .test_serve_robustness import Server, _env, wait_for

pytestmark = pytest.mark.filterwarnings("ignore:barca.Client is deprecated:DeprecationWarning")


class TokenServer(Server):
    def _up(self):
        assert self.proc.poll() is None, "token server exited during startup"
        try:
            return self.request("GET", "/auth/status")[0] == 200
        except OSError:
            return False


def exchange(server, method, path, token=None, body=None):
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(
        f"http://127.0.0.1:{server.port}{path}",
        data=json.dumps(body).encode() if body is not None else None,
        headers=headers,
        method=method,
    )
    try:
        response = urllib.request.urlopen(request, timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        payload = response.read()
        return response.status, json.loads(payload) if payload else None


def cli(project, *args, token=None):
    env = _env()
    if token is not None:
        env["BARCA_SERVER_TOKEN"] = token
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )


@pytest.fixture
def project(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 42\n"
    )
    return tmp_path


def test_local_bootstrap_connects_a_real_server_and_cli_and_python_use_issued_tokens(
    project, monkeypatch
):
    nested = project / "nested"
    nested.mkdir()
    bootstrap = cli(nested, "tokens", "bootstrap")
    assert bootstrap.returncode == 0, bootstrap.stderr
    code = bootstrap.stdout.strip()
    assert code not in bootstrap.stderr
    assert re.fullmatch(r"barca_bootstrap_v1_[0-9a-f]{64}_[0-9a-f]{64}", code)
    assert not (nested / ".barca").exists()
    assert code not in (project / ".barca/auth/state.json").read_text()
    server = TokenServer(project, "--auth", "token", "--no-schedule")
    try:
        assert exchange(server, "GET", "/health")[0] == 401
        status, connection = exchange(server, "POST", "/auth/bootstrap", body={"code": code})
        assert status == 200
        bearer = connection["token"]
        assert exchange(server, "POST", "/auth/bootstrap", body={"code": code})[0] == 401
        status, issued = exchange(server, "POST", "/auth/tokens", bearer, {"name": "test laptop"})
        assert status == 201
        token = issued["token"]
        assert issued["expires_at"] is None
        result = cli(
            project,
            "get",
            "value",
            "pipeline.py",
            "--server",
            f"http://127.0.0.1:{server.port}",
            "-o",
            "value",
            "--json",
            token=token,
        )
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["final_output"] == 42
        monkeypatch.setenv("BARCA_SERVER_TOKEN", token)
        assert Client(f"http://127.0.0.1:{server.port}").health()["status"] == "ok"
        status, listed = exchange(server, "GET", "/auth/tokens", token)
        assert status == 200
        assert token not in json.dumps(listed)
        assert exchange(server, "DELETE", f"/auth/tokens/{issued['id']}", bearer)[0] == 204
        started = time.monotonic()
        denied = cli(
            project, "history", "--server", f"http://127.0.0.1:{server.port}", "--json", token=token
        )
        assert denied.returncode == 3
        assert time.monotonic() - started < 5
        assert json.loads(denied.stderr)["reason"] == "invalid_token"
        for secret in [code, bearer, token]:
            assert secret not in result.stdout + result.stderr + denied.stdout + denied.stderr
            assert secret not in server.log.read_text()
    finally:
        server.stop()


def test_flag_auth_off_overrides_toml_token_mode(project):
    (project / "barca.toml").write_text('[server]\nauth = "token"\n')
    server = TokenServer(project, "--auth", "off", "--read-only")
    try:
        assert exchange(server, "GET", "/auth/status") == (200, {"mode": "off"})
        assert exchange(server, "GET", "/health")[0] == 200
        assert exchange(server, "POST", "/auth/tokens", body={"name": "blocked"})[0] == 403
        assert not (project / ".barca/auth").exists()
    finally:
        server.stop()


def test_invalid_auth_mode_is_usage_without_creating_credentials(project):
    result = cli(project, "serve", "--auth", "wrong")
    assert result.returncode == 2
    assert not (project / ".barca/auth").exists()


def test_metadata_backup_restore_and_restart_do_not_resurrect_revoked_tokens(project, tmp_path):
    store = tmp_path / "store"
    (project / "barca.toml").write_text(f"[remote]\nuri = {json.dumps(str(store))}\n")
    code = cli(project, "tokens", "bootstrap").stdout.strip()
    server = TokenServer(project, "--auth", "token", "--no-schedule")
    try:
        _, connection = exchange(server, "POST", "/auth/bootstrap", body={"code": code})
        admin = connection["token"]
        status, old = exchange(server, "POST", "/auth/tokens", admin, {"name": "before restore"})
        assert status == 201
    finally:
        server.stop()
    seeded = cli(project, "get", "value", "pipeline.py", "--local", "--json")
    assert seeded.returncode == 0, seeded.stderr
    backups = list((project / ".barca/backups").glob("*.db"))
    assert backups
    archived = tmp_path / "metadata-snapshot.db"
    archived.write_bytes(backups[0].read_bytes())
    assert all(old["token"].encode() not in backup.read_bytes() for backup in backups)
    server = TokenServer(project, "--auth", "token", "--no-schedule")
    try:
        assert exchange(server, "GET", "/health", old["token"])[0] == 200
        assert exchange(server, "DELETE", f"/auth/tokens/{old['id']}", admin)[0] == 204
        _, fresh = exchange(server, "POST", "/auth/tokens", admin, {"name": "after rotation"})
    finally:
        server.stop()
    auth_path = project / ".barca/auth/state.json"
    auth_before = auth_path.read_bytes()
    restored = cli(project, "state", "restore", "--from", str(archived), "--json")
    assert restored.returncode == 0, restored.stderr
    assert auth_path.read_bytes() == auth_before
    server = TokenServer(project, "--auth", "token", "--no-schedule")
    try:
        assert exchange(server, "GET", "/health", old["token"])[0] == 401
        assert exchange(server, "GET", "/health", fresh["token"])[0] == 200
        assert exchange(server, "POST", "/auth/bootstrap", body={"code": code})[0] == 401
    finally:
        server.stop()
    # A fresh host restoring only the same ledger gains no API credentials.
    other = tmp_path / "fresh_host"
    other.mkdir()
    (other / "pipeline.py").write_text((project / "pipeline.py").read_text())
    (other / "barca.toml").write_text("")
    restored = cli(other, "state", "restore", "--from", str(archived), "--json")
    assert restored.returncode == 0, restored.stderr
    assert not (other / ".barca/auth").exists()
    server = TokenServer(other, "--auth", "token", "--read-only")
    try:
        assert exchange(server, "GET", "/health", old["token"])[0] == 401
        assert exchange(server, "GET", "/health", fresh["token"])[0] == 401
    finally:
        server.stop()


@pytest.mark.parametrize("change", ["revoke", "bootstrap_rotation"])
def test_idle_stream_observes_auth_changes_from_another_server_process(project, change):
    (project / "pipeline.py").write_text(
        "from pathlib import Path\nimport time\nfrom barca import task\n"
        '@task()\ndef slow():\n    Path("started").touch()\n'
        '    while not Path("release").exists(): time.sleep(0.02)\n'
    )
    first = TokenServer(project, "--auth", "token", "--no-schedule")
    second = None
    response = None
    reader = None
    run_id = None
    admin = None
    done = threading.Event()
    observed = threading.Event()
    errors = []
    try:
        code = cli(project, "tokens", "bootstrap").stdout.strip()
        _, connection = exchange(first, "POST", "/auth/bootstrap", body={"code": code})
        _, operator = exchange(
            first, "POST", "/auth/tokens", connection["token"], {"name": "operator"}
        )
        admin = operator["token"]
        _, issued = exchange(first, "POST", "/auth/tokens", admin, {"name": "stream"})
        stream_token = issued["token"] if change == "revoke" else connection["token"]
        # A distinct environment has its own ledger and serve lock, sharing only auth.
        second = TokenServer(project, "--env", "observer", "--auth", "token", "--read-only")
        assert first.proc.pid != second.proc.pid
        status, accepted = exchange(
            first, "POST", "/runs", admin, {"command": "run", "targets": ["slow"]}
        )
        assert status == 202
        run_id = accepted["run_id"]
        wait_for(lambda: (project / "started").exists(), "idle remote task")
        request = urllib.request.Request(
            f"http://127.0.0.1:{first.port}/runs/{run_id}/events",
            headers={"Authorization": f"Bearer {stream_token}"},
        )
        response = urllib.request.urlopen(request, timeout=3)

        def consume():
            try:
                while response.read(1):
                    observed.set()
            except OSError as error:
                errors.append(type(error).__name__)
            finally:
                done.set()

        reader = threading.Thread(target=consume)
        reader.start()
        assert observed.wait(2)
        assert not done.wait(0.1), "the active stream should be idle, not already closed"
        started = time.monotonic()
        if change == "revoke":
            assert exchange(second, "DELETE", f"/auth/tokens/{issued['id']}", admin)[0] == 204
        else:
            code = cli(project, "tokens", "bootstrap").stdout.strip()
            assert exchange(second, "POST", "/auth/bootstrap", body={"code": code})[0] == 200
        assert done.wait(1.25), "cross-process auth change left an idle stream open"
        assert time.monotonic() - started < 1.25
        assert not errors, errors
        assert exchange(first, "GET", "/health", stream_token)[0] == 401
        assert exchange(first, "GET", f"/runs/{run_id}", admin)[1]["status"] == "running"
    finally:
        (project / "release").touch()
        if run_id and admin:
            exchange(first, "DELETE", f"/runs/{run_id}", admin)
        if second:
            second.stop()
        first.stop()
        if response:
            response.close()
        if reader:
            reader.join(timeout=4)
            assert not reader.is_alive()


def test_bootstrap_is_consumed_once_across_two_server_processes(project):
    first = TokenServer(project, "--auth", "token", "--no-schedule", "--read-only")
    second = None
    try:
        second = TokenServer(project, "--env", "observer", "--auth", "token", "--read-only")
        code = cli(project, "tokens", "bootstrap").stdout.strip()
        barrier = threading.Barrier(2)

        def redeem(server):
            barrier.wait(timeout=5)
            return exchange(server, "POST", "/auth/bootstrap", body={"code": code})

        with ThreadPoolExecutor(max_workers=2) as pool:
            answers = list(pool.map(redeem, [first, second]))
        assert sorted(status for status, _ in answers) == [200, 401]
        token = next(body["token"] for status, body in answers if status == 200)
        assert exchange(first, "GET", "/health", token)[0] == 200
        assert exchange(second, "GET", "/health", token)[0] == 200
        assert len(exchange(first, "GET", "/auth/tokens", token)[1]) == 1
    finally:
        if second:
            second.stop()
        first.stop()
