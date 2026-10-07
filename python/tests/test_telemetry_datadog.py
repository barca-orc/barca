"""`BARCA_TELEMETRY=datadog`: a finished run reaches the Datadog Agent as one trace.

A local HTTP server stands in for the Agent's trace intake and records what it is sent.
"""

import json
import os
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

import pytest
from barca.api import _find_binary

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "DD_")

PIPELINE = """
import time

from barca import asset, task


@asset()
def orders() -> dict:
    time.sleep(0.2)
    return {"rows": 3}


@asset(inputs={"orders": orders})
def report(orders: dict) -> dict:
    return {"total": orders["rows"]}


@task(inputs={"report": report})
def publish(report: dict) -> None:
    print("publish", report["total"])


@task(inputs={"report": report})
def broken(report: dict) -> None:
    raise ValueError("cannot publish")
"""


class FakeAgent:
    """Records every request to the trace intake; answers with `status`."""

    def __init__(self, status: int = 200):
        self.requests: list[dict] = []
        agent = self

        class Handler(BaseHTTPRequestHandler):
            def do_PUT(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                agent.requests.append(
                    {"path": self.path, "headers": dict(self.headers), "body": json.loads(body)}
                )
                self.send_response(status)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *args):
                pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    def spans(self) -> list[dict]:
        """Spans of the only trace of the latest request."""
        (trace,) = self.requests[-1]["body"]
        return trace


@pytest.fixture
def agent():
    fake = FakeAgent()
    yield fake
    fake.close()


@pytest.fixture
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def cli(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**base, **env},
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )


def datadog(agent: FakeAgent, **extra: str) -> dict[str, str]:
    return {"BARCA_TELEMETRY": "datadog", "DD_TRACE_AGENT_URL": agent.url, **extra}


def by_node(spans: list[dict]) -> dict[str, dict]:
    return {s["resource"].rsplit(":", 1)[1]: s for s in spans if s["name"] == "barca.step"}


def test_a_run_is_one_trace_with_a_span_per_step(project, agent):
    before = time.time_ns()
    proc = cli(
        project,
        "run",
        "publish",
        "--json",
        **datadog(
            agent, DD_SERVICE="planning", DD_ENV="staging", DD_VERSION="1.2.3", DD_TAGS="team:data"
        ),
    )
    after = time.time_ns()
    assert proc.returncode == 0, proc.stderr
    assert len(agent.requests) == 1
    request = agent.requests[0]
    assert request["path"] == "/v0.3/traces"
    assert request["headers"]["Content-Type"] == "application/json"

    spans = agent.spans()
    (root,) = [s for s in spans if s["name"] == "barca.run"]
    assert root["resource"] == "run publish"
    assert root["meta"]["barca.target"] == "publish"
    assert root["service"] == "planning"
    assert root["error"] == 0
    assert root["meta"]["env"] == "staging"
    assert root["meta"]["version"] == "1.2.3"
    assert root["meta"]["team"] == "data"
    assert root["meta"]["barca.status"] == "success"
    assert root["meta"]["barca.run_id"] == json.loads(proc.stdout)["run_id"]
    assert root["metrics"]["barca.steps.executed"] == 3
    assert before <= root["start"] <= after
    assert root["start"] + root["duration"] <= after

    steps = by_node(spans)
    assert set(steps) == {"orders", "report", "publish"}
    for span in steps.values():
        assert span["trace_id"] == root["trace_id"]
        assert span["parent_id"] == root["span_id"]
        assert span["meta"]["barca.outcome"] == "ran"
        assert root["start"] <= span["start"] <= after
    assert steps["orders"]["duration"] >= 200_000_000
    assert steps["orders"]["meta"]["barca.kind"] == "asset"
    assert steps["publish"]["meta"]["barca.kind"] == "task"
    # Steps sit at their real times: each starts after the one it depends on has finished.
    orders_end = steps["orders"]["start"] + steps["orders"]["duration"]
    assert steps["orders"]["start"] >= root["start"]
    assert steps["report"]["start"] >= orders_end - 5_000_000, (orders_end, steps["report"])
    assert (
        steps["publish"]["start"]
        >= steps["report"]["start"] + steps["report"]["duration"] - 5_000_000
    )
    assert steps["publish"]["start"] > steps["orders"]["start"] + 150_000_000
    for span in steps.values():
        assert span["start"] + span["duration"] <= root["start"] + root["duration"] + 50_000_000
        assert span["metrics"]["barca.attempts"] == 1
    assert len({s["span_id"] for s in spans}) == len(spans)


def test_a_cached_step_is_reported_as_cached(project, agent):
    assert cli(project, "get", "report", "--json").returncode == 0
    assert agent.requests == [], "telemetry is off unless BARCA_TELEMETRY names it"

    assert cli(project, "run", "publish", "--json", **datadog(agent)).returncode == 0
    spans = agent.spans()
    (root,) = [s for s in spans if s["name"] == "barca.run"]
    steps = by_node(spans)
    assert steps["orders"]["meta"]["barca.outcome"] == "cached"
    assert steps["report"]["meta"]["barca.outcome"] == "cached"
    assert steps["publish"]["meta"]["barca.outcome"] == "ran"
    assert steps["orders"]["duration"] == 0
    assert root["metrics"]["barca.steps.cached"] == 2
    assert root["service"] == "barca"


def test_a_failed_step_marks_its_span_and_the_run_as_errors(project, agent):
    proc = cli(project, "run", "broken", "--json", **datadog(agent))
    assert proc.returncode == 1
    spans = agent.spans()
    (root,) = [s for s in spans if s["name"] == "barca.run"]
    assert root["error"] == 1 and root["meta"]["barca.status"] == "failed"
    broken = by_node(spans)["broken"]
    assert broken["error"] == 1
    assert broken["meta"]["barca.outcome"] == "failed"
    assert broken["meta"]["error.type"] == "ValueError"
    assert broken["meta"]["error.message"] == "cannot publish"
    assert 'raise ValueError("cannot publish")' in broken["meta"]["error.stack"]
    assert by_node(spans)["report"]["error"] == 0


def test_an_unreachable_agent_warns_and_does_not_fail_the_run(project):
    proc = cli(
        project,
        "run",
        "publish",
        "--json",
        BARCA_TELEMETRY="datadog",
        DD_TRACE_AGENT_URL="http://127.0.0.1:1",
    )
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["status"] == "success"
    assert "telemetry 'datadog' did not receive run" in proc.stderr


def test_an_agent_that_refuses_the_trace_warns(project):
    refusing = FakeAgent(status=415)
    try:
        proc = cli(project, "run", "publish", "--json", **datadog(refusing))
    finally:
        refusing.close()
    assert proc.returncode == 0, proc.stderr
    assert "415" in proc.stderr


def test_an_unknown_integration_name_warns(project, agent):
    proc = cli(
        project, "run", "publish", "--json", **{**datadog(agent), "BARCA_TELEMETRY": "datadog,nope"}
    )
    assert proc.returncode == 0, proc.stderr
    assert "unknown telemetry integration 'nope'" in proc.stderr
    assert len(agent.requests) == 1


def test_scheduled_runs_under_serve_are_reported(tmp_path, agent):
    import signal
    import socket

    (tmp_path / "pipeline.py").write_text(
        "from barca import task, Schedule\n\n\n"
        '@task(freshness=Schedule("* * * * * *"))\n'
        "def heartbeat() -> None:\n"
        '    print("tick")\n'
    )
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port)],
        cwd=tmp_path,
        env={**base, **datadog(agent, DD_SERVICE="scheduler")},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 60
        while len(agent.requests) < 2 and time.monotonic() < deadline:
            time.sleep(0.25)
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            serve.communicate()

    assert len(agent.requests) >= 2, "each tick's run should send its own trace"
    roots = [s for r in agent.requests for s in r["body"][0] if s["name"] == "barca.run"]
    assert len({r["trace_id"] for r in roots}) == len(roots)
    assert all(r["service"] == "scheduler" and r["resource"].startswith("run ") for r in roots)


@pytest.mark.parametrize("value", ["false", "False", "0"])
def test_dd_trace_enabled_false_switches_datadog_off_silently(project, agent, value):
    proc = cli(project, "run", "publish", "--json", **datadog(agent, DD_TRACE_ENABLED=value))
    assert proc.returncode == 0, proc.stderr
    assert agent.requests == []
    assert "telemetry" not in proc.stderr, proc.stderr


def test_dd_trace_enabled_true_sends(project, agent):
    proc = cli(project, "run", "publish", "--json", **datadog(agent, DD_TRACE_ENABLED="true"))
    assert proc.returncode == 0, proc.stderr
    assert len(agent.requests) == 1
