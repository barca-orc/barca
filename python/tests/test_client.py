"""Tests for the ``barca.Client`` HTTP SDK.

The unit tests mock ``urllib`` so they need neither a server nor the binary. A
final integration test drives a real ``barca serve`` subprocess when one can be
started, and skips cleanly otherwise.
"""

from __future__ import annotations

import io
import json
import socket
import subprocess
import time
import urllib.error

import pytest

from barca.api import BarcaError
from barca.client import Client, Run

# ─── transport unit tests (mocked urllib) ────────────────────────────────────


class FakeResp:
    """Minimal stand-in for the urlopen context manager."""

    def __init__(self, body: bytes) -> None:
        self._body = body

    def __enter__(self) -> FakeResp:
        return self

    def __exit__(self, *exc: object) -> bool:
        return False

    def read(self) -> bytes:
        return self._body


def json_urlopen(payload, captured=None):
    body = json.dumps(payload).encode()

    def urlopen(req, timeout=None):
        if captured is not None:
            captured["url"] = req.full_url
            captured["method"] = req.get_method()
        return FakeResp(body)

    return urlopen


def test_get_builds_post_url_and_returns_run(monkeypatch):
    captured: dict = {}
    monkeypatch.setattr(
        "barca.client.urllib.request.urlopen",
        json_urlopen({"run_id": "abc123"}, captured),
    )
    run = Client("http://localhost:9999").get("daily_report")
    assert isinstance(run, Run)
    assert run.run_id == "abc123"
    assert captured["url"] == "http://localhost:9999/get/daily_report"
    assert captured["method"] == "POST"


def test_get_and_run_hit_correct_paths(monkeypatch):
    captured: dict = {}
    monkeypatch.setattr(
        "barca.client.urllib.request.urlopen",
        json_urlopen({"run_id": "r1"}, captured),
    )
    c = Client("http://h:1")
    c.get()  # no target → POST /run: every asset and sensor, no tasks
    assert captured["url"] == "http://h:1/run"
    c.run("cleanup")
    assert captured["url"] == "http://h:1/run/cleanup"
    assert captured["method"] == "POST"


def test_read_endpoints_parse_json(monkeypatch):
    captured: dict = {}
    monkeypatch.setattr(
        "barca.client.urllib.request.urlopen",
        json_urlopen([{"id": "f.py:daily", "cron": "0 5 * * *"}], captured),
    )
    jobs = Client().schedules()
    assert captured["url"].endswith("/schedule")
    assert captured["method"] == "GET"
    assert jobs[0]["cron"] == "0 5 * * *"


def test_base_url_trailing_slash_is_stripped(monkeypatch):
    captured: dict = {}
    monkeypatch.setattr(
        "barca.client.urllib.request.urlopen",
        json_urlopen({"status": "ok"}, captured),
    )
    Client("http://h:1/").health()
    assert captured["url"] == "http://h:1/health"


def test_http_error_body_becomes_barca_error(monkeypatch):
    def urlopen(req, timeout=None):
        raise urllib.error.HTTPError(
            req.full_url, 404, "Not Found", {}, io.BytesIO(b'{"error":"no such asset"}')
        )

    monkeypatch.setattr("barca.client.urllib.request.urlopen", urlopen)
    with pytest.raises(BarcaError, match="no such asset"):
        Client().get("nope")


def test_connection_refused_becomes_barca_error(monkeypatch):
    def urlopen(req, timeout=None):
        raise urllib.error.URLError("Connection refused")

    monkeypatch.setattr("barca.client.urllib.request.urlopen", urlopen)
    with pytest.raises(BarcaError, match="cannot reach barca server"):
        Client().health()


# ─── Run.wait polling ────────────────────────────────────────────────────────


def test_run_wait_polls_until_terminal(monkeypatch):
    c = Client()
    seq = iter([{"status": "running"}, {"status": "running"}, {"status": "complete", "result": {}}])
    monkeypatch.setattr(c, "status", lambda run_id: next(seq))
    result = Run(c, "r1").wait(timeout=5, poll=0)
    assert result["status"] == "complete"


def test_run_wait_times_out(monkeypatch):
    c = Client()
    monkeypatch.setattr(c, "status", lambda run_id: {"status": "running"})
    with pytest.raises(BarcaError, match="did not finish"):
        Run(c, "r1").wait(timeout=0.05, poll=0.01)


# ─── live-server integration (skips if it can't start) ───────────────────────


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _barca_binary() -> str | None:
    try:
        from barca.api import _find_binary

        return _find_binary()
    except Exception:
        return None


def test_end_to_end_run_via_server(tmp_path):
    binary = _barca_binary()
    if binary is None:
        pytest.skip("barca binary not available")

    module = tmp_path / "pipe.py"
    module.write_text(
        "from barca import asset\n\n@asset()\ndef hello() -> dict:\n    return {'msg': 'hi'}\n"
    )
    port = _free_port()
    proc = subprocess.Popen(
        [binary, "serve", str(module), "--port", str(port), "--no-schedule"],
        cwd=tmp_path,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        client = Client(f"http://127.0.0.1:{port}")
        # Wait for the server to bind.
        for _ in range(50):
            try:
                if client.health().get("status") == "ok":
                    break
            except BarcaError:
                time.sleep(0.2)
        else:
            pytest.skip("server did not come up")

        result = client.get("hello").wait(timeout=30)
        assert result["status"] == "complete"

        names = {a["id"].split(":")[-1] for a in client.assets()}
        assert "hello" in names
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
