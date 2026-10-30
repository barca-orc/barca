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

SPEC = yaml.safe_load(
    (Path(__file__).parents[2] / "specs/interfaces/server-api.openapi.yaml").read_text()
)


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
    original = server._open
    seen = set()

    def checked_open(request):
        method = request.get_method()
        template, op = operation(method, request.full_url)
        assert request.data is None, "current Client triggers are bodyless"
        seen.add((method, template))
        try:
            with original(request) as response:
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

    monkeypatch.setattr(server, "_open", checked_open)
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


def _raw(server, method, path, body=None):
    """One request with an optional JSON body: status, content type and raw bytes."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{server.port}{path}", data=data, method=method
    )
    if data is not None:
        request.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.headers.get_content_type(), response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers.get_content_type(), error.read()


def _checked(server, method, path, status, body=None):
    """A request whose answer the OpenAPI document describes: its status and validated JSON."""
    got, content_type, raw = _raw(server, method, path, body)
    assert got == status, (method, path, raw)
    template, op = operation(method, f"http://x{path}")
    assert content_type == "application/json", (method, path, content_type)
    value = json.loads(raw)
    validate(op["responses"][str(status)]["content"]["application/json"]["schema"], value)
    return value, raw


def _events(server, run_id):
    """Every event of a run's stream, read to its end: (id, event)."""
    request = urllib.request.Request(f"http://127.0.0.1:{server.port}/runs/{run_id}/events")
    with urllib.request.urlopen(request, timeout=60) as response:
        text = response.read().decode()
    events = []
    for frame in text.split("\n\n"):
        lines = frame.splitlines()
        data = [line[6:] for line in lines if line.startswith("data: ")]
        if data:
            event_id = next(line[4:] for line in lines if line.startswith("id: "))
            event = json.loads(data[0])
            validate({"$ref": "#/components/schemas/RunEvent"}, event)
            events.append((event_id, event))
    return events


CONTRACT_PIPELINE = """from barca import asset, task
@asset()
def value() -> dict:
    return {"n": 2}
@asset()
def broken() -> dict:
    raise ValueError("expected contract failure")
@task(inputs={"value": value})
def publish(value: dict) -> None:
    print("published", value["n"])
"""


def test_run_contract_and_inspection_match_openapi_and_the_cli(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(CONTRACT_PIPELINE)
    server = Server(tmp_path, "--no-schedule")
    try:
        accepted, _ = _checked(
            server, "POST", "/runs", 202, {"command": "get", "targets": ["value"], "agent": True}
        )
        run_id = accepted["run_id"]
        events = _events(server, run_id)
        cursors = [re.fullmatch(r"([0-9a-f]{32}):([1-9][0-9]*)", cursor) for cursor, _ in events]
        assert all(cursors), events
        assert len({cursor.group(1) for cursor in cursors}) == 1
        assert [int(cursor.group(2)) for cursor in cursors] == list(range(1, len(events) + 1))
        assert events[0][1] == {"type": "run_started", "run_id": run_id}
        assert any(e["type"] == "line" and e["when"] == "agent" for _, e in events)
        finished = events[-1][1]
        assert finished["type"] == "run_finished" and finished["ok"] is True
        # As `barca get --json` prints it: the artifact pointer (paths by default, #404).
        pointer = finished["outcome"]["final_output"]["_barca_artifact"]
        assert pointer["format"] == "json"
        assert json.loads((tmp_path / pointer["path"]).read_text()) == {"n": 2}
        view, _ = _checked(server, "GET", f"/runs/{run_id}", 200)
        assert view["triggered_by"] == "api" and view["outcome"] == finished["outcome"]
        status, content_type, raw = _raw(server, "GET", f"/runs/{run_id}/output")
        assert (status, content_type, json.loads(raw)) == (200, "application/json", {"n": 2})
        # `values` (`get -o value`): the server reads the value, as `-o value --json` prints it.
        valued, _ = _checked(
            server, "POST", "/runs", 202, {"command": "get", "targets": ["value"], "values": True}
        )
        assert _events(server, valued["run_id"])[-1][1]["outcome"]["final_output"] == {"n": 2}

        # A task, then a failure: the outcome carries the CLI's failed body and envelope.
        task, _ = _checked(server, "POST", "/runs", 202, {"command": "run", "targets": ["publish"]})
        events = [e for _, e in _events(server, task["run_id"]) if e["type"] == "log"]
        assert [(e["line"], e["stream"]) for e in events] == [("published 2", "stdout")]
        recorded, _ = _checked(server, "GET", f"/runs/{task['run_id']}/logs", 200)
        assert recorded["status"] == "success"
        assert [(e["line"], e["ts"]) for e in recorded["logs"]] == [
            ("published 2", events[0]["ts"])
        ]
        _checked(server, "GET", f"/runs/{task['run_id']}/logs?stream=stderr", 200)
        _checked(server, "GET", f"/runs/{task['run_id']}/logs?step=nowhere", 400)
        failed, _ = _checked(
            server, "POST", "/runs", 202, {"command": "get", "targets": ["broken"]}
        )
        outcome = _events(server, failed["run_id"])[-1][1]["outcome"]
        assert outcome["status"] == "failed" and outcome["envelope"]["code"] == 1
        _checked(server, "GET", f"/runs/{failed['run_id']}/output", 404)
        _checked(server, "DELETE", f"/runs/{failed['run_id']}", 409)
        _checked(server, "POST", "/runs", 404, {"command": "get", "targets": ["missing"]})
        plan, _ = _checked(
            server, "POST", "/runs", 200, {"command": "get", "targets": ["value"], "dry_run": True}
        )
        assert plan["summary"]["cached"] == 1

        # Inspection bodies are the CLI's --json output, byte for byte.
        env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}

        def cli(*args):
            done = subprocess.run(
                [_find_binary(), *args], cwd=tmp_path, env=env, capture_output=True, text=True
            )
            assert done.returncode == 0, done.stderr
            return done.stdout.rstrip("\n").encode()

        for path, args in [
            ("/list", ["list", "pipeline.py", "--json", "--all"]),
            ("/history", ["history", "--json"]),
            ("/stats/value", ["stats", "value", "pipeline.py", "--json"]),
        ]:
            _, raw = _checked(server, "GET", path, 200)
            assert raw == cli(*args), path
        # `barca logs --json` is the route's lines without their `seq`.
        recorded, _ = _checked(server, "GET", f"/runs/{task['run_id']}/logs", 200)
        assert json.loads(cli("logs", task["run_id"], "--json", "--local")) == [
            {k: line[k] for k in ("node_id", "stream", "ts", "line")} for line in recorded["logs"]
        ]
        nodes, _ = _checked(server, "GET", "/nodes?targets=value", 200)
        status = json.loads(cli("status", "value", "pipeline.py", "--json", "--all"))
        assert nodes == status
        # `barca sql` with a server: the views it resolves and barca.materializations' rows.
        catalog, _ = _checked(server, "GET", "/sql/views", 200)
        [view] = [v for v in catalog["views"] if v["name"] == "value"]
        assert view["node"] == "pipeline.py:value" and view["stale"] is False
        steps, _ = _checked(server, "GET", "/materializations?limit=1000", 200)
        table = json.loads(
            cli("sql", "select * from barca.materializations", "--json", "--all", "--local")
        )
        assert steps["total"] == len(table["rows"]) == len(steps["materializations"])
        key = lambda r: (r["node_id"], r["created_at"], r["status"])  # noqa: E731
        assert sorted(table["rows"], key=key) == sorted(steps["materializations"], key=key)
        history, _ = _checked(server, "GET", "/history?limit=1", 200)
        assert history["truncated"] is True and history["runs"][0]["triggered_by"] == "api"
        health, _ = _checked(server, "GET", "/health", 200)
        assert health["api_version"] == 1 and health["env"] == "default"
        project, _ = _checked(server, "GET", "/project", 200)
        assert [f["path"] for f in project["files"]] == ["pipeline.py"]
    finally:
        server.stop()
