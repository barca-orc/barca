"""Tests for the ``barca.Client`` HTTP SDK.

The unit tests mock ``urllib`` so they need neither a server nor the binary. The
integration tests drive a real ``barca serve`` (the ``serve`` fixture).
"""

from __future__ import annotations

import io
import json
import os
import urllib.error

import pytest

from barca.api import BarcaError
from barca.client import Client, Run

from .helpers import barca

# The deprecation is asserted once below; the other tests use the client as before.
pytestmark = pytest.mark.filterwarnings("ignore:barca.Client is deprecated:DeprecationWarning")


def test_constructing_a_client_warns_that_it_is_deprecated():
    with pytest.warns(DeprecationWarning, match=r"barca\.Client is deprecated.*server=True"):
        Client("http://127.0.0.1:1")


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


def _patch_open(monkeypatch, fake):
    monkeypatch.setattr(Client, "_open", lambda self, req: fake(req, timeout=self.timeout))


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
    _patch_open(
        monkeypatch,
        json_urlopen({"run_id": "abc123"}, captured),
    )
    run = Client("http://localhost:9999").get("daily_report")
    assert isinstance(run, Run)
    assert run.run_id == "abc123"
    assert captured["url"] == "http://localhost:9999/get/daily_report"
    assert captured["method"] == "POST"


def test_get_and_run_hit_correct_paths(monkeypatch):
    captured: dict = {}
    _patch_open(
        monkeypatch,
        json_urlopen({"run_id": "r1"}, captured),
    )
    c = Client("http://h:1")
    c.get()  # no target → POST /run: every asset and sensor, no tasks
    assert captured["url"] == "http://h:1/run"
    c.run("cleanup")
    assert captured["url"] == "http://h:1/run/cleanup"
    assert captured["method"] == "POST"


def test_a_target_with_a_path_is_sent_as_one_url_segment(monkeypatch):
    # A full node id is what the server says to use when a name matches several nodes.
    captured: dict = {}
    _patch_open(
        monkeypatch,
        json_urlopen({"run_id": "r1"}, captured),
    )
    c = Client("http://h:1")
    c.get("sub/pipeline.py:orders")
    assert captured["url"] == "http://h:1/get/sub%2Fpipeline.py:orders"
    c.run("sub/pipeline.py:publish")
    assert captured["url"] == "http://h:1/run/sub%2Fpipeline.py:publish"


def test_read_endpoints_parse_json(monkeypatch):
    captured: dict = {}
    _patch_open(
        monkeypatch,
        json_urlopen([{"id": "f.py:daily", "cron": "0 5 * * *"}], captured),
    )
    jobs = Client().schedules()
    assert captured["url"].endswith("/schedule")
    assert captured["method"] == "GET"
    assert jobs[0]["cron"] == "0 5 * * *"


def test_base_url_trailing_slash_is_stripped(monkeypatch):
    captured: dict = {}
    _patch_open(
        monkeypatch,
        json_urlopen({"status": "ok"}, captured),
    )
    Client("http://h:1/").health()
    assert captured["url"] == "http://h:1/health"


def test_http_error_body_becomes_barca_error(monkeypatch):
    def urlopen(req, timeout=None):
        raise urllib.error.HTTPError(
            req.full_url, 404, "Not Found", {}, io.BytesIO(b'{"error":"no such asset"}')
        )

    _patch_open(monkeypatch, urlopen)
    with pytest.raises(BarcaError, match="no such asset"):
        Client().get("nope")


def test_connection_refused_becomes_barca_error(monkeypatch):
    def urlopen(req, timeout=None):
        raise urllib.error.URLError("Connection refused")

    _patch_open(monkeypatch, urlopen)
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


# ─── live-server integration ─────────────────────────────────────────────────

HELLO = "from barca import asset\n\n@asset()\ndef hello() -> dict:\n    return {'msg': 'hi'}\n"


def test_end_to_end_run_via_server(tmp_path, serve):
    module = tmp_path / "pipe.py"
    module.write_text(HELLO)
    # The machine's environment is inherited on purpose, as before this test used `Serve`.
    server = serve("--no-schedule", files=(str(module),), env={**os.environ})
    client = Client(server.url)
    assert client.health().get("status") == "ok"

    result = client.get("hello").wait(timeout=30)
    assert result["status"] == "complete"

    names = {a["id"].split(":")[-1] for a in client.assets()}
    assert "hello" in names


def _serve_and_capture_stderr(tmp_path, serve, extra_args):
    """Start `barca serve`, wait until /health answers on 127.0.0.1, stop it; return its stderr."""
    module = tmp_path / "pipe.py"
    module.write_text(HELLO)
    server = serve("--no-schedule", *extra_args, files=(str(module),), env={**os.environ})
    assert Client(server.url).health().get("status") == "ok"
    server.stop()
    return server.stderr, server.port


def test_serve_host_all_interfaces_is_reachable_and_warns(tmp_path, serve):
    stderr, port = _serve_and_capture_stderr(tmp_path, serve, ["--host", "0.0.0.0"])
    assert f"serving on http://0.0.0.0:{port}" in stderr
    assert "warning: listening on 0.0.0.0 with no authentication" in stderr


def test_serve_default_host_is_loopback_without_warning(tmp_path, serve):
    stderr, port = _serve_and_capture_stderr(tmp_path, serve, [])
    assert f"serving on http://127.0.0.1:{port}" in stderr
    assert "no authentication" not in stderr


def test_serve_host_rejects_a_non_ip_as_a_usage_error(tmp_path):
    proc = barca(tmp_path, "serve", "--host", "localhost", env={**os.environ})
    assert proc.returncode == 2
