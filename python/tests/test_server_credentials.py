"""Credentials apply to every coordinator request without escaping the server's scope."""

import base64
import json
import signal
import subprocess
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
from barca.api import BarcaError, _find_binary
from barca.client import Client

from .test_serve_robustness import Server, _env, wait_for

pytestmark = pytest.mark.filterwarnings("ignore:barca.Client is deprecated:DeprecationWarning")

PIPELINE = """from pathlib import Path
import os
import time
from barca import asset, task
@asset()
def value(): return 42
@asset()
def token_in_worker(): return os.environ.get("BARCA_SERVER_TOKEN")
@task()
def job(): return 1
@task()
def streaming():
    for i in range(8):
        print(f"café-{i}", flush=True)
        time.sleep(0.15)
    return 8
@task()
def slow():
    Path("slow.started").touch()
    while not Path("release").exists(): time.sleep(0.02)
"""

TOKEN = "server-test-private-token"


@pytest.fixture(params=["basic", "bearer"])
def proxy(tmp_path, request):
    store = tmp_path / "store"
    store.mkdir()
    deployed, client = tmp_path / "deployed", tmp_path / "client"
    for root in (deployed, client):
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        (root / "barca.toml").write_text(f"[remote]\nuri = {json.dumps(str(store))}\n")
    backend = Server(deployed, "--no-schedule")
    expected = (
        "Bearer " + TOKEN
        if request.param == "bearer"
        else "Basic " + base64.b64encode(b"alice:s3cret").decode()
    )
    state = {
        "calls": [],
        "redirect": None,
        "reflect": False,
        "drop_streams": 0,
        "drop_utf8": False,
        "expire_streams": False,
        "cursors": [],
    }

    class Handler(BaseHTTPRequestHandler):
        def forward(self):
            authorization = self.headers.get("Authorization")
            state["calls"].append((self.command, self.path, authorization))
            if authorization != expected or state["reflect"]:
                self.send_response(401)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(
                    json.dumps({"kind": "infra", "code": 3, "error": authorization}).encode()
                )
                return
            if state["redirect"]:
                self.send_response(302)
                self.send_header("Location", state["redirect"])
                self.end_headers()
                return
            assert self.path.startswith("/project/")
            route = self.path.removeprefix("/project")
            if route.endswith("/events") and state["expire_streams"]:
                self.send_response(410)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"kind":"infra","code":3,"reason":"event_cursor_expired"}')
                return
            size = int(self.headers.get("Content-Length", "0"))
            data = self.rfile.read(size) if size else None
            upstream = urllib.request.Request(
                f"http://127.0.0.1:{backend.port}{route}",
                data=data,
                method=self.command,
                headers={
                    "Content-Type": "application/json",
                    **(
                        {"Last-Event-ID": self.headers["Last-Event-ID"]}
                        if "Last-Event-ID" in self.headers
                        else {}
                    ),
                },
            )
            try:
                response = urllib.request.urlopen(upstream, timeout=15)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                self.send_response(response.status)
                self.send_header(
                    "Content-Type", response.headers.get("Content-Type", "application/json")
                )
                self.end_headers()
                try:
                    if route.endswith("/events"):
                        state["cursors"].append(self.headers.get("Last-Event-ID"))
                        if state["drop_streams"]:
                            state["drop_streams"] -= 1
                            # Deliver one whole event, then only a prefix of the next.
                            frame = bytearray()
                            while chunk := response.read(1):
                                frame.extend(chunk)
                                self.wfile.write(chunk)
                                self.wfile.flush()
                                if state["drop_utf8"]:
                                    if frame.endswith(b"caf\xc3"):
                                        break
                                elif frame.endswith(b"\n\n") or frame.endswith(b"\r\n\r\n"):
                                    break
                            if not state["drop_utf8"]:
                                self.wfile.write(response.read(7))
                            self.wfile.flush()
                            return
                        while chunk := response.read1(1024):
                            self.wfile.write(chunk)
                            self.wfile.flush()
                    else:
                        self.wfile.write(response.read())
                except (BrokenPipeError, ConnectionResetError):
                    pass

        do_GET = do_POST = do_DELETE = forward

        def log_message(self, *_args):
            pass

    gateway = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=gateway.serve_forever)
    thread.start()
    authority = f"127.0.0.1:{gateway.server_port}"
    url = f"http://{'alice:s3cret@' if request.param == 'basic' else ''}{authority}/project"
    env = _env() | {"BARCA_SERVER": url, "BARCA_SERVER_RECONNECT": "0"}
    if request.param == "bearer":
        env["BARCA_SERVER_TOKEN"] = TOKEN
    try:
        yield client, deployed, url, env, expected, state
    finally:
        (deployed / "release").touch()
        backend.stop()
        gateway.shutdown()
        gateway.server_close()
        thread.join(timeout=3)
        assert not thread.is_alive()


def cli(root, env, *args):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env,
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )


def test_credentials_cover_execution_stream_ledger_and_cancel(proxy, monkeypatch):
    client, deployed, url, env, expected, state = proxy
    remote = cli(client, env, "get", "value", "pipeline.py", "--server", "--json")
    assert remote.returncode == 0, remote.stderr
    cached = cli(client, env, "get", "value", "pipeline.py", "--refresh-all", "--json")
    assert cached.returncode == 0, cached.stderr
    # No --server: run locally against the configured server's ledger and publish the result.
    local = cli(client, env, "run", "job", "pipeline.py", "--json")
    assert local.returncode == 0, local.stderr
    if "BARCA_SERVER_TOKEN" in env:
        monkeypatch.setenv("BARCA_SERVER_TOKEN", env["BARCA_SERVER_TOKEN"])
    else:
        monkeypatch.delenv("BARCA_SERVER_TOKEN", raising=False)
    assert Client(url).health()["status"] == "ok"
    following = subprocess.Popen(
        [_find_binary(), "run", "slow", "pipeline.py", "--server", "--json"],
        cwd=client,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        wait_for(lambda: (deployed / "slow.started").exists(), "remote slow task")
        wait_for(
            lambda: any(path.endswith("/events") for _, path, _ in state["calls"]), "event stream"
        )
        following.send_signal(signal.SIGINT)
        stdout, stderr = following.communicate(timeout=20)
        assert following.returncode == 130, (stdout, stderr)
    finally:
        if following.poll() is None:
            following.send_signal(signal.SIGCONT)
            following.kill()
            following.wait(timeout=5)
    calls = state["calls"]
    assert any(path == "/project/ledger/lookup" for _, path, _ in calls)
    assert any(path == "/project/ledger/runs" for _, path, _ in calls)
    assert any(path.endswith("/events") for _, path, _ in calls)
    assert any(method == "DELETE" for method, _, _ in calls)
    assert all(authorization == expected for _, _, authorization in calls)
    assert (
        TOKEN not in remote.stdout + remote.stderr + local.stdout + local.stderr + stdout + stderr
    )
    assert (
        "s3cret"
        not in remote.stdout + remote.stderr + local.stdout + local.stderr + stdout + stderr
    )


def test_reflected_credentials_do_not_escape_cli_or_python_errors(proxy, monkeypatch):
    client, _, url, env, _, state = proxy
    state["reflect"] = True
    result = cli(client, env, "history", "--server", "--json")
    assert result.returncode == 3
    if "BARCA_SERVER_TOKEN" in env:
        monkeypatch.setenv("BARCA_SERVER_TOKEN", env["BARCA_SERVER_TOKEN"])
    else:
        monkeypatch.delenv("BARCA_SERVER_TOKEN", raising=False)
    with pytest.raises(BarcaError) as failure:
        Client(url).health()
    for secret in (TOKEN, "s3cret", base64.b64encode(b"alice:s3cret").decode()):
        assert secret not in result.stdout + result.stderr + str(failure.value)


@pytest.mark.parametrize("destination", ["outside-prefix", "other-origin"])
def test_redirect_cannot_escape_the_configured_server_scope(proxy, monkeypatch, destination):
    client, _, url, env, _, state = proxy
    clean = url.replace("alice:s3cret@", "")
    state["redirect"] = (
        clean.removesuffix("/project") + "/outside"
        if destination == "outside-prefix"
        else clean.replace("127.0.0.1", "localhost") + "/health"
    )
    result = cli(client, env, "history", "--server", "--json")
    assert result.returncode == 3
    if "BARCA_SERVER_TOKEN" in env:
        monkeypatch.setenv("BARCA_SERVER_TOKEN", env["BARCA_SERVER_TOKEN"])
    else:
        monkeypatch.delenv("BARCA_SERVER_TOKEN", raising=False)
    with pytest.raises(BarcaError, match="redirect left"):
        Client(url).health()
    assert all(path.startswith("/project/") for _, path, _ in state["calls"])


def test_the_reserved_bearer_token_is_not_inherited_by_workers(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    result = cli(
        tmp_path,
        _env() | {"BARCA_SERVER_TOKEN": TOKEN},
        "get",
        "token_in_worker",
        "pipeline.py",
        "--local",
        "-o",
        "value",
        "--json",
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["final_output"] is None


@pytest.mark.parametrize("partial_utf8", [False, True])
def test_authenticated_proxy_drop_resumes_without_duplicate_logs_or_work(proxy, partial_utf8):
    client, _, _, env, expected, state = proxy
    state["drop_streams"] = 1
    state["drop_utf8"] = partial_utf8
    result = cli(
        client,
        env | {"BARCA_SERVER_RECONNECT": "10"},
        "run",
        "streaming",
        "pipeline.py",
        "--server",
        "--pretty",
    )
    assert result.returncode == 0, result.stderr
    assert len(state["cursors"]) == 2, state["cursors"]
    assert state["cursors"][0] is None
    assert state["cursors"][1] is not None
    for i in range(8):
        assert result.stderr.count(f"café-{i}") == 1, result.stderr
    assert (
        sum(method == "POST" and path == "/project/runs" for method, path, _ in state["calls"]) == 1
    ), state["calls"]
    assert all(authorization == expected for _, _, authorization in state["calls"])


def test_expired_replay_keeps_healthy_run_following_and_authenticated_cancellation(proxy):
    client, deployed, _, env, expected, state = proxy
    state["expire_streams"] = True
    following = subprocess.Popen(
        [_find_binary(), "run", "slow", "pipeline.py", "--server", "--json"],
        cwd=client,
        env=env | {"BARCA_SERVER_RECONNECT": "1"},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        wait_for(lambda: (deployed / "slow.started").exists(), "remote slow task")
        wait_for(
            lambda: sum(
                method == "GET" and "/runs/" in path and not path.endswith("/events")
                for method, path, _ in state["calls"]
            )
            >= 2,
            "healthy outcome polls beyond the outage window",
        )
        assert following.poll() is None
        following.send_signal(signal.SIGINT)
        stdout, stderr = following.communicate(timeout=20)
        assert following.returncode == 130, (stdout, stderr)
        assert "event replay is incomplete" in stderr
        assert any(method == "DELETE" for method, _, _ in state["calls"])
        assert (
            sum(method == "POST" and path == "/project/runs" for method, path, _ in state["calls"])
            == 1
        )
        assert all(authorization == expected for _, _, authorization in state["calls"])
    finally:
        if following.poll() is None:
            following.send_signal(signal.SIGCONT)
            following.kill()
            following.wait(timeout=5)
