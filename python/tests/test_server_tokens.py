"""Local bootstrap and real server token lifecycle, without a user-account system."""

import json
import re
import subprocess
import time
import urllib.error
import urllib.request

import pytest
from barca.api import _find_binary
from barca.client import Client

from .test_serve_robustness import Server, _env

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
