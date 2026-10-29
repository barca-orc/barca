"""Credential-bearing server URLs authenticate requests but never appear in diagnostics."""

import base64
import json
import sqlite3
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from barca.api import _find_binary

from .test_serve_robustness import _env, _free_port
from .test_server_ledger import Team, _kill_server_mid_run, barca, posts, toml


@pytest.fixture(scope="module")
def proxy():
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append((self.path, self.headers.get("Authorization")))
            self.send_response(404)
            self.end_headers()

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield f"http://alice:s3cret@127.0.0.1:{server.server_port}/barca-api", requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
        assert not thread.is_alive()


@pytest.mark.parametrize("source", ["flag", "env", "config"])
@pytest.mark.parametrize("command", ["history", "status", "list", "logs", "doctor"])
def test_404_diagnostics_redact_credentials_without_changing_auth(tmp_path, proxy, source, command):
    url, requests = proxy
    env = {**_env(), "BARCA_SERVER_RECONNECT": "0"}
    argv = [command, "--json", "--server"]
    if source == "flag":
        argv.append(url)
    elif source == "env":
        env["BARCA_SERVER"] = url
    else:
        (tmp_path / "barca.toml").write_text(f'[server]\nurl = "{url}"\n')
    result = subprocess.run(
        [_find_binary(), *argv], cwd=tmp_path, env=env, text=True, capture_output=True, timeout=15
    )
    assert result.returncode == 3
    output = result.stdout + result.stderr
    assert "s3cret" not in output
    assert "alice:" not in output
    assert url.split("@", 1)[1] in output
    assert requests[-1] == (
        "/barca-api/health",
        "Basic " + base64.b64encode(b"alice:s3cret").decode(),
    )
    if command == "doctor":
        report = json.loads(result.stdout)
        check = next(check for check in report["checks"] if check["name"] == "server")
        assert check["data"]["url"] == url.replace("alice:s3cret@", "")


def test_an_unreachable_server_and_invalid_url_do_not_print_userinfo(tmp_path):
    for url, expected in [
        (f"http://alice:s%33cret@127.0.0.1:{_free_port()}/proxy", 3),
        ("ftp://alice:s3cret@host/proxy", 2),
    ]:
        result = subprocess.run(
            [_find_binary(), "history", "--server", url, "--json"],
            cwd=tmp_path,
            env=_env(),
            text=True,
            capture_output=True,
            timeout=15,
        )
        assert result.returncode == expected
        assert "s3cret" not in result.stderr
        assert "s%33cret" not in result.stderr
        assert "alice:" not in result.stderr
        assert "/proxy" in result.stderr


def test_local_ledger_announcement_and_store_error_are_redacted(tmp_path, proxy):
    url, _ = proxy
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    result = subprocess.run(
        [_find_binary(), "get", "pipeline.py", "--json"],
        cwd=tmp_path,
        env={**_env(), "BARCA_SERVER": url},
        text=True,
        capture_output=True,
        timeout=15,
    )
    assert result.returncode == 2
    assert "[barca] server " in result.stderr
    assert "no artifact store" in result.stderr
    assert "s3cret" not in result.stdout + result.stderr


def test_doctor_redacts_errors_recorded_by_an_older_client(tmp_path, proxy):
    url, _ = proxy
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    result = subprocess.run(
        [_find_binary(), "get", "pipeline.py", "--local", "--json"],
        cwd=tmp_path,
        env=_env(),
        text=True,
        capture_output=True,
        timeout=15,
    )
    assert result.returncode == 0
    with sqlite3.connect(tmp_path / ".barca/metadata.db") as conn:
        conn.execute(
            "CREATE TABLE ledger_posts(client_run_id TEXT PRIMARY KEY, server TEXT NOT NULL, "
            "body TEXT NOT NULL, state TEXT NOT NULL, server_run_id TEXT, error TEXT, "
            "attempts INTEGER DEFAULT 0, created_at TEXT DEFAULT (datetime('now')), "
            "updated_at TEXT DEFAULT (datetime('now')))"
        )
        conn.execute(
            "INSERT INTO ledger_posts(client_run_id, server, state, error, body) VALUES (?, ?, ?, ?, ?)",
            ("older-client", url, "unsynced", f"cannot reach {url}", "{}"),
        )
    result = subprocess.run(
        [_find_binary(), "doctor", "--server", url, "--json"],
        cwd=tmp_path,
        env=_env(),
        text=True,
        capture_output=True,
        timeout=15,
    )
    assert result.returncode == 3
    assert "s3cret" not in result.stdout + result.stderr
    report = json.loads(result.stdout)
    check = next(check for check in report["checks"] if check["name"] == "server_posts")
    assert check["data"]["unsynced"] == 1
    assert check["data"]["last_error"] == f"cannot reach {url.replace('alice:s3cret@', '')}"


def test_unrecorded_run_warning_is_redacted_and_the_same_post_can_still_be_retried(tmp_path):
    team = Team(tmp_path)
    url = team.url.replace("://", "://alice:s3cret@")
    (team.a / "barca.toml").write_text(toml(team.store, url))
    try:
        out = _kill_server_mid_run(team, team.a)
        assert out.returncode == 0
        assert f"not recorded on the server {team.url}" in out.stderr
        assert "s3cret" not in out.stdout + out.stderr
        [pending] = posts(team.a)
        assert pending["state"] == "unsynced"
        assert pending["server"] == url
        team.start()
        out = barca(team.a, "get", "base", "--json")
        assert out.returncode == 0
        assert "s3cret" not in out.stdout + out.stderr
        assert all(post["state"] == "shared" for post in posts(team.a))
    finally:
        team.stop()


def test_sql_missing_result_error_redacts_the_origin_server(tmp_path):
    team = Team(tmp_path)
    url = team.url.replace("://", "://alice:s3cret@")
    try:
        out = barca(team.a, "sql", "select * from base", "--server", url, "--json")
        assert out.returncode == 2
        assert f"no result on the server at {team.url}" in out.stderr
        assert "s3cret" not in out.stdout + out.stderr
    finally:
        team.stop()
