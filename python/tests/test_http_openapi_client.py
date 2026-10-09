"""Real stdlib Client requests and parsing conform to the same normative HTTP schemas."""

from __future__ import annotations

import io
import json
import os
import re
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

import jsonschema
import pytest
import yaml
from barca.api import BarcaError, _find_binary
from barca.client import Client

from .test_serve_load_isolation import BROKEN, PIPELINE
from .test_serve_robustness import Server

SPEC = yaml.safe_load((Path(__file__).parents[2] / "specs/server-api.openapi.yaml").read_text())


def validate(schema, body):
    jsonschema.Draft202012Validator(
        {"$schema": SPEC["jsonSchemaDialect"], "components": SPEC["components"], "allOf": [schema]}
    ).validate(body)


def operation(method, url):
    path = urllib.parse.urlsplit(url).path
    for template, methods in SPEC["paths"].items():
        pattern = re.sub(r"\{[^}]+\}", "[^/]+", template)
        if re.fullmatch(pattern, path) and method.lower() in methods:
            return template, methods[method.lower()]
    raise AssertionError(f"undocumented Client request {method} {path}")


@pytest.fixture
def server(tmp_path):
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "pipeline.py").write_text(
        """from barca import asset, task, Schedule
@asset()
def value() -> dict:
    return {"n": 2}
@asset()
def broken() -> dict:
    raise ValueError("expected client failure")
@task(inputs={"value": value})
def publish(value: dict) -> None:
    print("published", value["n"])
@task()
def slow() -> None:
    import time
    time.sleep(60)
@task(freshness=Schedule("0 0 1 1 *"))
def annual() -> None:
    pass
"""
    )
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    process = subprocess.Popen(
        [_find_binary(), "serve", "sub/pipeline.py", "--port", str(port)],
        cwd=tmp_path,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    client = Client(f"http://127.0.0.1:{port}", timeout=10)
    try:
        deadline = time.monotonic() + 20
        while True:
            try:
                client.health()
                break
            except BarcaError:
                if process.poll() is not None or time.monotonic() > deadline:
                    raise AssertionError("HTTP contract fixture failed to start") from None
                time.sleep(0.05)
        yield client
    finally:
        process.send_signal(signal.SIGTERM)
        try:
            _, stderr = process.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            _, stderr = process.communicate()
        assert process.returncode == 0, stderr


def test_real_client_requests_and_terminal_parsing_match_openapi(server, monkeypatch):
    original = urllib.request.urlopen
    seen = set()

    def checked_urlopen(request, timeout=None):
        method = request.get_method()
        template, op = operation(method, request.full_url)
        assert request.data is None, "current Client triggers are bodyless"
        seen.add((method, template))
        try:
            with original(request, timeout=timeout) as response:
                body = response.read()
                assert response.headers.get_content_type() == "application/json"
                validate(
                    op["responses"][str(response.status)]["content"]["application/json"]["schema"],
                    json.loads(body),
                )
                return io.BytesIO(body)
        except urllib.error.HTTPError as error:
            body = error.read()
            assert error.headers.get_content_type() == "application/json"
            validate(
                op["responses"][str(error.code)]["content"]["application/json"]["schema"],
                json.loads(body),
            )
            error.fp = io.BytesIO(body)
            raise

    monkeypatch.setattr("barca.client.urllib.request.urlopen", checked_urlopen)
    assert server.health()["status"] == "ok"
    assert server.assets()
    assert server.asset("value")["asset"]["id"].endswith(":value")
    assert server.plan()["total_steps"] > 0
    # Health is liveness; the scheduler initializes history asynchronously.
    deadline = time.monotonic() + 10
    while not (schedules := server.schedules()):
        assert time.monotonic() < deadline, "scheduler did not publish the configured job"
        time.sleep(0.02)
    assert schedules[0]["cron"] == "0 0 1 1 *"
    assert server.get("value").wait(timeout=20, poll=0.02)["status"] == "complete"
    assert server.get("sub/pipeline.py:value").wait(timeout=20, poll=0.02)["status"] == "complete"
    assert server.run("publish").wait(timeout=20, poll=0.02)["status"] == "complete"
    assert server.get("broken").wait(timeout=20, poll=0.02)["status"] == "failed"
    # The no-target current Client maps to POST /run, not a proposed command endpoint.
    assert server.get().wait(timeout=20, poll=0.02)["status"] == "failed"
    slow = server.run("slow")
    assert slow.cancel()["status"] == "cancelling"
    assert slow.wait(timeout=20, poll=0.02)["status"] == "cancelled"
    with pytest.raises(BarcaError, match=r"failed \(404\)"):
        server.get("no-such-node")
    with pytest.raises(BarcaError, match=r"failed \(409\)"):
        slow.cancel()
    assert seen == {
        ("GET", "/health"),
        ("GET", "/assets"),
        ("GET", "/assets/{name}"),
        ("GET", "/plan"),
        ("GET", "/schedule"),
        ("GET", "/status/{run_id}"),
        ("POST", "/get/{target}"),
        ("POST", "/run/{target}"),
        ("POST", "/run"),
        ("DELETE", "/run/{target}"),
    }


@pytest.mark.parametrize("empty", [False, True])
def test_real_client_partial_load_health_and_empty_admission_match_openapi(tmp_path, empty):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(BROKEN if empty else PIPELINE)
    (tmp_path / "broken.py").write_text(BROKEN)
    process = Server(tmp_path, ".", "--no-schedule")
    client = Client(f"http://127.0.0.1:{process.port}")
    try:
        assets = client.assets()
        health = client.health()
        validate({"$ref": "#/components/schemas/Health"}, health)
        assert health["status"] == "ok" and health["load_errors"]
        if empty:
            assert assets == []
            assert len(health["load_errors"]) == 2
            with pytest.raises(BarcaError, match=r"failed \(400\).+no loaded assets or sensors"):
                client.get()
        else:
            assert {asset["id"] for asset in assets} == {"pipeline.py:safe", "pipeline.py:healthy"}
            affected = {node for error in health["load_errors"] for node in error["affected_nodes"]}
            assert affected == {"pipeline.py:blocked", "pipeline.py:dependent"}
            result = client.get("safe").wait(timeout=20, poll=0.02)
            assert result["status"] == "complete"
    finally:
        process.stop()
