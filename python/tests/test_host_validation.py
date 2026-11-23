"""Loopback HTTP servers reject DNS-rebinding hosts before dispatching routes."""

import pytest

from .helpers import barca
from .served import Serve


def test_loopback_server_refuses_foreign_host(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    with Serve(tmp_path, "--no-schedule", wait="health") as server:
        for method, path in [("GET", "/health"), ("POST", "/runs"), ("GET", "/ui/")]:
            status, body = server.request(method, path, headers={"Host": "attacker.example"})
            assert status == 403, (method, path, status, body)
            assert "host" in body["error"].lower(), body


def test_loopback_names_proxy_allowlist_and_cli_work(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    (tmp_path / "barca.toml").write_text('[server]\nallowed_hosts = ["Barca.Example.Com"]\n')
    with Serve(tmp_path, "--no-schedule", wait="health") as server:
        for host in ["localhost", "127.0.0.1:8274", "[::1]:8274", "barca.example.com:443"]:
            status, body = server.request("GET", "/health", headers={"Host": host})
            assert status == 200, (host, body)
        result = barca(tmp_path, "status", "--server", server.url, "--json")
        assert result.returncode == 0, result.stderr
        # Beside the server this goes through the owner-only project socket.
        result = barca(tmp_path, "status", "--json")
        assert result.returncode == 0, result.stderr


@pytest.mark.parametrize(
    "host", ["*.example.com", "https://barca.example.com", "host:8274", "", "bad/name"]
)
def test_invalid_proxy_allowlist_is_a_usage_error(tmp_path, host):
    import json

    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 1\n"
    )
    (tmp_path / "barca.toml").write_text(f"[server]\nallowed_hosts = [{json.dumps(host)}]\n")
    result = barca(tmp_path, "status", "--json")
    assert result.returncode == 2, result.stderr
    assert "allowed_hosts" in result.stderr
