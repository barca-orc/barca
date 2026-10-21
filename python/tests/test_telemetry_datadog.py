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

try:
    import msgpack
except ImportError:  # only the tests of Python spans need it; they skip without it
    msgpack = None

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
        self.python_spans: list[dict] = []
        agent = self

        class Handler(BaseHTTPRequestHandler):
            # barca's coordinator sends JSON; ddtrace in the worker sends msgpack.
            def do_PUT(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                if self.headers.get("Content-Type") == "application/json":
                    agent.requests.append(
                        {"path": self.path, "headers": dict(self.headers), "body": json.loads(body)}
                    )
                elif self.path.startswith("/v0.4/traces") and msgpack is not None:
                    for trace in msgpack.unpackb(body, raw=False, strict_map_key=False):
                        agent.python_spans.extend(trace)
                self.send_response(status)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"{}")

            do_POST = do_PUT

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
    return {
        "BARCA_TELEMETRY": "datadog",
        "DD_TRACE_AGENT_URL": agent.url,
        "DD_TRACE_API_VERSION": "v0.4",
        "DD_INSTRUMENTATION_TELEMETRY_ENABLED": "false",
        **extra,
    }


def by_node(spans: list[dict]) -> dict[str, dict]:
    """`barca.step` spans by resource: the node's short name."""
    return {s["resource"]: s for s in spans if s["name"] == "barca.step"}


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
    assert request["path"] == "/v0.4/traces"
    assert request["headers"]["Content-Type"] == "application/json"

    spans = agent.spans()
    (root,) = [s for s in spans if s["name"] == "barca.run"]
    assert root["resource"] == "run publish"
    assert root["meta"]["barca.job"] == "pipeline.py:publish"
    assert root["meta"]["barca.target"] == "publish"
    assert root["meta"]["barca.targets"] == "publish"
    assert root["meta"]["barca.project"] == project.name
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
    for name, span in steps.items():
        assert span["trace_id"] == root["trace_id"]
        assert span["parent_id"] == root["span_id"]
        assert span["meta"]["barca.outcome"] == "ran"
        assert span["meta"]["barca.asset"] == name
        assert span["meta"]["barca.node_id"] == f"pipeline.py:{name}"
        assert span["meta"]["barca.file"] == "pipeline.py"
        assert span["service"] == "planning" and span["type"] == "worker"
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
    # Lineage and what was written: direct upstream ids, and the digest the run recorded.
    assert "barca.upstream" not in steps["orders"]["meta"]
    assert steps["report"]["meta"]["barca.upstream"] == "pipeline.py:orders"
    assert steps["publish"]["meta"]["barca.upstream"] == "pipeline.py:report"
    for name in ("orders", "report"):
        status = cli(project, "status", name, "pipeline.py", "--json")
        (node,) = [
            n for n in json.loads(status.stdout)["nodes"] if n["id"] == f"pipeline.py:{name}"
        ]
        recorded = node["last_materialization"]
        assert steps[name]["meta"]["barca.digest"] == recorded["content_hash"]
        assert steps[name]["metrics"]["barca.bytes"] == recorded["size_bytes"]


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
    # A cached step wrote nothing: no digest, but its lineage is the same.
    assert "barca.digest" not in steps["report"]["meta"]
    assert steps["report"]["meta"]["barca.upstream"] == "pipeline.py:orders"
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


def test_a_failed_step_span_lasts_from_its_first_attempt_to_its_failure(tmp_path, agent):
    (tmp_path / "pipeline.py").write_text(
        "import time\n"
        "from barca import task\n\n\n"
        "@task(timeout_seconds=1)\n"
        "def slow() -> None:\n"
        "    time.sleep(10)\n\n\n"
        "@task(retries=2)\n"
        "def flaky() -> None:\n"
        "    time.sleep(0.3)\n"
        "    raise ValueError('no')\n"
    )
    for name, low, high in (("slow", 1.0, 3.0), ("flaky", 0.6, 3.0)):
        proc = cli(tmp_path, "run", name, "--json", **datadog(agent))
        assert proc.returncode == 1, proc.stderr
        (row,) = json.loads(proc.stdout)["steps"]
        spans = agent.spans()
        (root,) = [s for s in spans if s["name"] == "barca.run"]
        step = by_node(spans)[name]
        assert step["meta"]["barca.outcome"] == "failed"
        # A timeout reports about its timeout; retries are inside the span.
        assert low * 1e9 <= step["duration"] <= high * 1e9, step["duration"]
        # The span covers at least the time the attempts ran (the run JSON's elapsed_seconds).
        assert step["duration"] >= row["elapsed_seconds"] * 1e9 - 5_000_000
        assert root["start"] <= step["start"]
        assert step["start"] + step["duration"] <= root["start"] + root["duration"] + 50_000_000


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
        while (
            sum(
                span["name"] == "barca.run"
                for request in agent.requests
                for span in request["body"][0]
            )
            < 2
            and time.monotonic() < deadline
        ):
            time.sleep(0.25)
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            serve.communicate()

    roots = [s for r in agent.requests for s in r["body"][0] if s["name"] == "barca.run"]
    assert len(roots) >= 2, "each tick's run should send its own trace"
    assert len({r["trace_id"] for r in roots}) == len(roots)
    assert all(r["service"] == "scheduler" and r["resource"] == "run heartbeat" for r in roots)


def test_a_run_cancelled_by_a_stopping_server_says_why(tmp_path, agent):
    """#190, #434 item 3: a run the drain did not finish is `cancelled` with a reason."""
    import signal
    import socket
    import urllib.request

    (tmp_path / "pipeline.py").write_text(
        "import time\nfrom pathlib import Path\nfrom barca import task\n\n\n"
        "@task()\n"
        "def slow() -> None:\n"
        '    Path("started").write_text("")\n'
        "    time.sleep(60)\n"
    )
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port), "--no-schedule",
         "--drain-timeout", "0"],
        cwd=tmp_path,
        env={**base, **datadog(agent)},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                req = urllib.request.Request(f"http://127.0.0.1:{port}/run/slow", method="POST")
                urllib.request.urlopen(req, timeout=5).read()
                break
            except OSError:
                time.sleep(0.1)
        while not (tmp_path / "started").exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        serve.send_signal(signal.SIGTERM)
        _, err = serve.communicate(timeout=30)
    finally:
        if serve.poll() is None:
            serve.kill()
            serve.communicate()
    assert serve.returncode == 1, err

    roots = [s for r in agent.requests for s in r["body"][0] if s["name"] == "barca.run"]
    assert len(roots) == 1, roots
    assert roots[0]["meta"]["barca.status"] == "cancelled", roots[0]
    assert roots[0]["meta"]["barca.reason"] == "server shutdown", roots[0]


def serve_for(tmp_path: Path, seconds: float, env: dict[str, str], until=None):
    """Run `barca serve` on a scheduled pipeline; stop it after `seconds` or once `until()`."""
    import signal
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port)],
        cwd=tmp_path,
        env={**base, **env},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline and not (until and until()):
            time.sleep(0.25)
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            out, err = serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            out, err = serve.communicate()
    return serve.returncode, out, err


SCHEDULED = (
    "from barca import asset, task, Schedule\n\n\n"
    "@asset()\n"
    "def orders() -> dict:\n"
    '    return {"rows": 3}\n\n\n'
    '@task(inputs={"orders": orders}, freshness=Schedule("* * * * * *"))\n'
    "def heartbeat(orders: dict) -> None:\n"
    '    print("tick", orders["rows"])\n'
)


def test_barca_log_json_under_serve_writes_json_lines_joined_to_the_trace(tmp_path, agent):
    (tmp_path / "pipeline.py").write_text(SCHEDULED)

    def two_runs():
        return sum(s["name"] == "barca.run" for r in agent.requests for s in r["body"][0]) >= 2

    code, out, err = serve_for(
        tmp_path, 60, {"BARCA_LOG": "json", **datadog(agent)}, until=two_runs
    )
    assert code == 0, err
    # Every line on stdout is one JSON object; barca writes none of its lines to stderr.
    lines = [json.loads(line) for line in out.splitlines()]
    assert "[barca]" not in err, err
    for line in lines:
        assert set(line) <= {"ts", "level", "msg", "run_id", "node", "dd.trace_id", "dd.span_id"}
        assert line["level"] in ("info", "warning", "error"), line
        assert line["ts"].endswith("Z"), line
        assert not line["msg"].startswith("[barca]"), line
    assert any(line["msg"].startswith("serving on http://") for line in lines)
    # The worker's own notice is a line too, without a run.
    assert any(line["msg"].startswith("importing ") for line in lines), lines

    roots = {
        s["meta"]["barca.run_id"]: s
        for r in agent.requests
        for s in r["body"][0]
        if s["name"] == "barca.run"
    }
    steps = {
        (s["meta"]["barca.run_id"], s["meta"]["barca.node_id"]): s
        for r in agent.requests
        for s in r["body"][0]
        if s["name"] == "barca.step"
    }
    # The task's print() is an info line with its run and node, joined to the step's span.
    ticks = [line for line in lines if line["msg"] == "tick 3"]
    assert ticks, lines
    joined = [t for t in ticks if t["run_id"] in roots]
    assert joined, (ticks, list(roots))
    for tick in joined:
        assert tick["level"] == "info"
        assert tick["node"] == "pipeline.py:heartbeat"
        span = steps[(tick["run_id"], "pipeline.py:heartbeat")]
        assert tick["dd.trace_id"] == str(span["trace_id"])
        assert tick["dd.span_id"] == str(span["span_id"])
    # A step line names its node; the end-of-run line is joined to the run span.
    run_id = joined[0]["run_id"]
    of_run = [line for line in lines if line.get("run_id") == run_id]
    done = [line for line in of_run if "steps | done in" in line["msg"]]
    assert len(done) == 1 and "node" not in done[0], of_run
    assert done[0]["dd.span_id"] == str(roots[run_id]["span_id"])
    completed = [line for line in of_run if line["msg"].startswith("step:pipeline.py:heartbeat ")]
    assert completed and completed[0]["node"] == "pipeline.py:heartbeat", of_run
    scheduled = [line for line in lines if line["msg"].startswith("scheduled run ")]
    assert scheduled and scheduled[0]["node"] == "pipeline.py:heartbeat"


def test_barca_log_json_without_telemetry_has_no_trace_ids(tmp_path):
    (tmp_path / "pipeline.py").write_text(SCHEDULED)
    code, out, err = serve_for(tmp_path, 4, {"BARCA_LOG": "json"})
    assert code == 0, err
    lines = [json.loads(line) for line in out.splitlines()]
    ticks = [line for line in lines if line["msg"] == "tick 3"]
    assert ticks, lines
    assert all("dd.trace_id" not in line and "dd.span_id" not in line for line in lines)
    assert all(t["run_id"] and t["node"] == "pipeline.py:heartbeat" for t in ticks)


def test_without_barca_log_serve_writes_text_lines_to_stderr(tmp_path):
    (tmp_path / "pipeline.py").write_text(SCHEDULED)
    code, out, err = serve_for(tmp_path, 3, {})
    assert code == 0, err
    assert out == ""
    assert "[barca] serving on http://" in err
    # Captured task output is kept with the run (`barca logs`), not written by serve.
    assert "tick 3" not in err


def test_barca_log_on_another_command_is_a_warning_and_changes_nothing(project):
    plain = cli(project, "get", "orders", "pipeline.py", "--json")
    proc = cli(project, "get", "orders", "pipeline.py", "--json", BARCA_LOG="json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout).keys() == json.loads(plain.stdout).keys()
    assert (
        "[barca] warning: BARCA_LOG applies to barca serve only; this command writes its usual "
        "lines" in proc.stderr
    )
    assert '"ts"' not in proc.stderr + proc.stdout


def test_barca_log_with_an_unknown_value_is_a_usage_error_for_serve(project):
    proc = cli(project, "serve", "pipeline.py", BARCA_LOG="yaml")
    assert proc.returncode == 2
    assert "BARCA_LOG=yaml is not a log format" in proc.stderr
    assert proc.stdout == ""


@pytest.mark.parametrize("value", ["false", "False", "0", "", "no", "off"])
def test_dd_trace_enabled_false_switches_datadog_off_silently(project, agent, value):
    proc = cli(project, "run", "publish", "--json", **datadog(agent, DD_TRACE_ENABLED=value))
    assert proc.returncode == 0, proc.stderr
    assert agent.requests == []
    assert "telemetry" not in proc.stderr, proc.stderr


def test_dd_trace_enabled_true_sends(project, agent):
    proc = cli(project, "run", "publish", "--json", **datadog(agent, DD_TRACE_ENABLED="true"))
    assert proc.returncode == 0, proc.stderr
    assert len(agent.requests) == 1


def test_under_serve_a_delivery_failure_is_reported_once_until_it_recovers(tmp_path):
    import signal
    import socket

    (tmp_path / "pipeline.py").write_text(
        "from barca import task, Schedule\n\n\n"
        '@task(freshness=Schedule("* * * * * *"))\n'
        "def heartbeat() -> None:\n"
        "    pass\n"
    )
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        agent_port = s.getsockname()[1]
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port)],
        cwd=tmp_path,
        env={
            **base,
            "BARCA_TELEMETRY": "datadog",
            "DD_TRACE_AGENT_URL": f"http://127.0.0.1:{agent_port}",
        },
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        time.sleep(5)
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            _, err = serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            _, err = serve.communicate()

    assert err.count("scheduled run") >= 3, err
    assert err.count("telemetry 'datadog' did not receive run") == 1, err


def test_cli_aliases_group_under_the_same_job(project, agent):
    for target in ("publish", "pipeline.py:publish"):
        proc = cli(project, "run", target, "--json", **datadog(agent))
        assert proc.returncode == 0, proc.stderr
        root = agent.spans()[0]
        assert root["resource"] == "run publish"
        assert root["meta"]["barca.target"] == target
        assert all(s["meta"]["barca.job"] == "pipeline.py:publish" for s in agent.spans())
    assert len(agent.requests) == 2
    assert (
        agent.requests[0]["body"][0][0]["trace_id"] != agent.requests[1]["body"][0][0]["trace_id"]
    )


TRACED = """

@task()
def traced():
    import sqlite3
    from ddtrace import tracer
    with tracer.trace("user.work"):
        sqlite3.connect(":memory:").execute("select 1").fetchall()
"""


def test_python_calls_are_children_of_the_step_in_one_service(project, agent):
    pytest.importorskip("ddtrace")
    pytest.importorskip("msgpack")
    # A user-created span, and an instrumented library call that is opted in.
    with (project / "pipeline.py").open("a") as f:
        f.write(TRACED)
    proc = cli(
        project,
        "run",
        "traced",
        "--json",
        **datadog(agent, DD_SERVICE="jobs", DD_TRACE_SQLITE3_ENABLED="true", DD_ENV="prod"),
    )
    assert proc.returncode == 0, proc.stderr
    root = agent.spans()[0]
    (step,) = by_node(agent.spans()).values()
    (execution,) = [s for s in agent.python_spans if s["name"] == "barca.execute"]
    (user,) = [s for s in agent.python_spans if s["name"] == "user.work"]
    sql = [s for s in agent.python_spans if s["name"] == "sqlite.query"]
    assert sql, agent.python_spans
    assert execution["resource"] == "traced"
    assert execution["type"] == "worker"
    assert execution["parent_id"] == step["span_id"]
    assert execution["meta"]["component"] == "python"
    assert execution["meta"]["span.kind"] == "internal"
    assert execution["meta"]["barca.asset"] == "traced"
    assert execution["meta"]["barca.node_id"] == "pipeline.py:traced"
    assert execution["meta"]["barca.job"] == "pipeline.py:traced"
    assert execution["meta"]["barca.version"] == root["meta"]["barca.version"]
    assert execution["meta"]["env"] == "prod"
    # One 128-bit trace: the upper half travels as _dd.p.tid, as on the coordinator's root.
    assert execution["meta"]["_dd.p.tid"] == root["meta"]["_dd.p.tid"]
    assert user["parent_id"] == execution["span_id"]
    assert all(s["parent_id"] == user["span_id"] for s in sql)
    assert all(s["trace_id"] == root["trace_id"] for s in agent.python_spans)
    # One service: barca's spans, the user's and the library's.
    assert {s["service"] for s in agent.python_spans} == {"jobs"}
    assert root["service"] == "jobs"


def test_library_spans_are_off_unless_opted_in(project, agent):
    pytest.importorskip("ddtrace")
    pytest.importorskip("msgpack")
    with (project / "pipeline.py").open("a") as f:
        f.write(TRACED)
    proc = cli(project, "run", "traced", "--json", **datadog(agent))
    assert proc.returncode == 0, proc.stderr
    names = {s["name"] for s in agent.python_spans}
    assert {"barca.execute", "user.work"} <= names
    assert "sqlite.query" not in names

    for opt_in in ({"BARCA_TRACE_LIBRARIES": "1"}, {"DD_PATCH_MODULES": "sqlite3:true"}):
        agent.python_spans.clear()
        proc = cli(project, "run", "traced", "--json", **datadog(agent, **opt_in))
        assert proc.returncode == 0, proc.stderr
        sql = [s for s in agent.python_spans if s["name"] == "sqlite.query"]
        assert sql, (opt_in, agent.python_spans)
        assert {s["service"] for s in sql} == {"barca"}, opt_in


def test_python_failure_is_delivered_before_worker_is_replaced(project, agent):
    pytest.importorskip("ddtrace")
    pytest.importorskip("msgpack")
    proc = cli(project, "run", "broken", "--json", **datadog(agent))
    assert proc.returncode == 1, proc.stderr
    spans = [s for s in agent.python_spans if s["name"] == "barca.execute"]
    (failed,) = [s for s in spans if s["meta"]["barca.node_id"] == "pipeline.py:broken"]
    assert failed["error"] == 1
    assert failed["meta"]["error.type"].endswith("ValueError")
    assert failed["meta"]["error.message"] == "cannot publish"
    root = agent.spans()[0]
    assert all(s["trace_id"] == root["trace_id"] for s in spans)
    assert all(s["meta"]["barca.run_id"] == root["meta"]["barca.run_id"] for s in spans)


def test_retry_attempts_keep_the_same_job_trace(project, agent):
    pytest.importorskip("ddtrace")
    pytest.importorskip("msgpack")
    with (project / "pipeline.py").open("a") as f:
        f.write("""

@task(retries=2)
def retrying():
    from pathlib import Path
    marker = Path("attempted")
    if not marker.exists():
        marker.touch()
        raise ValueError("try again")
    return "done"
""")
    proc = cli(project, "run", "retrying", "--json", **datadog(agent))
    assert proc.returncode == 0, proc.stderr
    executions = [s for s in agent.python_spans if s["name"] == "barca.execute"]
    assert len(executions) == 2
    assert sorted(s["metrics"]["barca.attempt"] for s in executions) == [1, 2]
    assert sorted(s.get("error", 0) for s in executions) == [0, 1]
    root = agent.spans()[0]
    (step,) = by_node(agent.spans()).values()
    assert all(s["trace_id"] == root["trace_id"] for s in executions)
    assert all(s["parent_id"] == step["span_id"] for s in executions)
    assert len({s["span_id"] for s in executions}) == 2


def test_parallel_python_calls_have_a_reported_parent(project, agent):
    pytest.importorskip("ddtrace")
    pytest.importorskip("msgpack")
    with (project / "pipeline.py").open("a") as f:
        f.write("""

@task()
def child():
    from ddtrace import tracer
    with tracer.trace("child.work"):
        return 1

@task()
def fanout():
    from functools import partial
    from barca import parallel
    return parallel(partial(child), partial(child))
""")
    proc = cli(project, "run", "fanout", "--json", **datadog(agent))
    assert proc.returncode == 0, proc.stderr
    root = agent.spans()[0]
    (step,) = by_node(agent.spans()).values()
    executions = [s for s in agent.python_spans if s["name"] == "barca.execute"]
    assert len(executions) == 3, agent.python_spans
    assert all(s["parent_id"] == step["span_id"] for s in executions)
    assert all(s["resource"] == "fanout" for s in executions)
    assert all(s["trace_id"] == root["trace_id"] for s in agent.python_spans)
    children = [s for s in agent.python_spans if s["name"] == "child.work"]
    assert len(children) == 2
    assert all(s["parent_id"] in {e["span_id"] for e in executions} for s in children)


def test_disabled_tracing_sends_no_python_executions(project, agent):
    proc = cli(project, "run", "publish", "--json", **datadog(agent, DD_TRACE_ENABLED="false"))
    assert proc.returncode == 0, proc.stderr
    assert agent.requests == []
    assert agent.python_spans == []


class DogStatsD:
    """A UDP listener standing in for the Agent's DogStatsD port."""

    def __init__(self):
        import socket

        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(0.1)
        self.url = f"udp://127.0.0.1:{self.sock.getsockname()[1]}"
        self.packets: list[str] = []

    def drain(self) -> list[str]:
        while True:
            try:
                self.packets.append(self.sock.recv(65535).decode())
            except OSError:
                return self.packets

    def gauges(self) -> list[tuple[str, float, dict[str, str]]]:
        """`(name, value, tags)` of every gauge received, in order."""
        out = []
        for packet in self.drain():
            if packet.startswith("_e{"):
                continue
            head, _, rest = packet.partition("|g")
            name, _, value = head.partition(":")
            tags = dict(t.split(":", 1) for t in rest.removeprefix("|#").split(",") if t)
            out.append((name, float(value), tags))
        return out


@pytest.fixture
def dogstatsd():
    listener = DogStatsD()
    yield listener
    listener.sock.close()


@pytest.mark.parametrize("mode", ["enabled", "unconfigured", "disabled"])
def test_idle_serve_lifecycle_is_metrics_not_traces(tmp_path, agent, dogstatsd, mode):
    import signal
    import socket
    import urllib.request

    (tmp_path / "pipeline.py").write_text(
        'raise RuntimeError("idle telemetry must not import this module")\n'
    )
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = {key: value for key, value in os.environ.items() if not key.startswith(SCRUB)}
    enabled = mode == "enabled"
    statsd = {"DD_DOGSTATSD_URL": dogstatsd.url, "DD_ENV": "prod", "DD_SERVICE": "jobs"}
    options = (
        datadog(agent, **statsd, **({"DD_TRACE_ENABLED": "false"} if mode == "disabled" else {}))
        if mode != "unconfigured"
        else {"DD_TRACE_AGENT_URL": agent.url, **statsd}
    )
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--no-schedule", "--port", str(port)],
        cwd=tmp_path,
        env={**base, **options},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{port}/health", timeout=1
                ) as response:
                    assert response.status == 200
                break
            except OSError:
                time.sleep(0.05)
        else:
            pytest.fail("idle server did not become ready")
        if enabled:
            while not dogstatsd.drain() and time.monotonic() < deadline:
                time.sleep(0.05)
            assert dogstatsd.packets, "idle start should be visible without a run"
        else:
            time.sleep(0.2)
    finally:
        serve.send_signal(signal.SIGTERM)
        _, stderr = serve.communicate(timeout=10)
    assert serve.returncode == 0, stderr
    assert not (tmp_path / ".barca" / "metadata.db").exists()
    # No lifecycle traces, whatever the mode.
    assert agent.requests == []
    gauges = dogstatsd.gauges()
    if not enabled:
        assert gauges == [] and dogstatsd.packets == []
        return
    up = [(value, tags) for name, value, tags in gauges if name == "barca.serve.up"]
    assert [value for value, _ in up] == [1.0, 0.0], gauges
    tags = up[0][1]
    assert tags["service"] == "jobs" and tags["env"] == "prod"
    assert tags["barca.version"] and tags["barca.project"] == tmp_path.name
    assert tags["barca.scheduling"] == "false"
    files = [value for name, value, _ in gauges if name == "barca.serve.files"]
    assert files == [1.0, 1.0]
    assert [v for n, v, _ in gauges if n == "barca.serve.schedules"] == [0.0, 0.0]
    events = [p for p in dogstatsd.packets if p.startswith("_e{")]
    assert len(events) == 2
    assert ":barca serve started|" in events[0] and ":barca serve stopped|" in events[1]
    # Nothing that names a run, a source file or an address.
    assert not any("pipeline.py" in p or "run_id" in p for p in dogstatsd.packets)


def test_failed_listener_bind_does_not_export_start(tmp_path, agent, dogstatsd):
    import socket

    (tmp_path / "pipeline.py").write_text("raise RuntimeError('not imported')\n")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        sock.listen()
        result = cli(
            tmp_path,
            "serve",
            "pipeline.py",
            "--no-schedule",
            "--port",
            str(sock.getsockname()[1]),
            **datadog(agent, DD_DOGSTATSD_URL=dogstatsd.url),
        )
    assert result.returncode != 0
    assert agent.requests == []
    assert dogstatsd.drain() == []


def test_stalled_idle_export_does_not_delay_readiness_or_shutdown(tmp_path):
    import signal
    import socket
    import urllib.request

    (tmp_path / "pipeline.py").write_text("raise RuntimeError('not imported')\n")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    # The intake accepts TCP connections but never answers HTTP. A second
    # connection for stop can enter the backlog without a serving thread.
    with socket.socket() as intake:
        intake.bind(("127.0.0.1", 0))
        intake.listen(8)
        base = {key: value for key, value in os.environ.items() if not key.startswith(SCRUB)}
        serve = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--no-schedule", "--port", str(port)],
            cwd=tmp_path,
            env={
                **base,
                "BARCA_TELEMETRY": "datadog",
                "DD_TRACE_AGENT_URL": f"http://127.0.0.1:{intake.getsockname()[1]}",
            },
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    with urllib.request.urlopen(
                        f"http://127.0.0.1:{port}/health", timeout=0.2
                    ) as response:
                        assert response.status == 200
                    break
                except OSError:
                    time.sleep(0.05)
            else:
                pytest.fail("Agent delivery delayed server readiness")
        finally:
            started = time.monotonic()
            serve.send_signal(signal.SIGTERM)
            try:
                _, stderr = serve.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                serve.kill()
                serve.communicate()
                pytest.fail("stalled Agent prevented bounded shutdown")
        assert time.monotonic() - started < 5
        assert serve.returncode == 0, stderr
        assert not (tmp_path / ".barca" / "metadata.db").exists()


SHOP = """
import sqlite3

from barca import asset, collect, group, partitions, task


@asset()
def orders() -> dict:
    return {"rows": 3}


@asset(inputs={"orders": orders}, partitions={"region": partitions(["emea", "amer", "apac"])})
def regional(region: str, orders: dict) -> dict:
    if region == "apac":
        raise ValueError(f"no data for {region}")
    sqlite3.connect(":memory:").execute("select 1").fetchall()
    return {"region": region, "rows": orders["rows"]}


@asset(inputs={"orders": orders})
def daily_revenue(orders: dict) -> int:
    return orders["rows"] * 10


@task(inputs={"revenue": daily_revenue})
def publish(revenue: int) -> None:
    print("revenue", revenue)


@task(inputs={"regional": collect(regional)})
def report(regional: list) -> None:
    print(regional)


sales = group("Sales", members=[orders, daily_revenue, publish], output=daily_revenue)
company = group("Company", members=[sales], output=sales)
"""


def tree(spans: list[dict]) -> list[str]:
    """The trace as indented `name resource {key tags}` lines, siblings sorted."""
    children: dict[int, list[dict]] = {}
    ids = {s["span_id"] for s in spans}
    roots = []
    for s in spans:
        parent = s.get("parent_id") or 0
        if parent in ids:
            children.setdefault(parent, []).append(s)
        else:
            roots.append(s)
    keys = (
        "barca.partition",
        "barca.group",
        "barca.outcome",
        "barca.status",
        "barca.failed_asset",
        "error.type",
        "error.message",
        "component",
    )
    lines: list[str] = []

    def walk(span: dict, depth: int) -> None:
        tags = {k: span["meta"][k] for k in keys if k in span["meta"]}
        tags.update(
            {k: int(v) for k, v in span["metrics"].items() if k.startswith("barca.partitions.")}
        )
        lines.append(
            f"{'  ' * depth}{span['name']} [{span['resource']}] service={span['service']} "
            f"type={span.get('type')} error={span.get('error', 0)} {json.dumps(tags, sort_keys=True)}"
        )
        for child in sorted(
            children.get(span["span_id"], []),
            key=lambda c: (c["name"], c["resource"], c["meta"].get("barca.partition", "")),
        ):
            walk(child, depth + 1)

    for root in roots:
        walk(root, 0)
    return lines


def test_a_pipeline_trace_shows_groups_partitions_and_the_failure(tmp_path, agent):
    (tmp_path / "pipeline.py").write_text(SHOP)
    proc = cli(tmp_path, "run", "report,publish", "--json", **datadog(agent))
    assert proc.returncode == 1, proc.stderr
    spans = agent.spans()
    root = spans[0]
    assert root["resource"] == "run publish,report"
    assert root["meta"]["barca.failed_asset"] == "regional"
    assert root["meta"]["error.message"] == "regional[region=apac]: no data for apac"
    steps = by_node(spans)
    assert steps["orders"]["meta"]["barca.group"] == "Company/Sales"
    assert steps["publish"]["meta"]["barca.group"] == "Company/Sales"
    assert "barca.group" not in steps["regional"]["meta"]
    regional = steps["regional"]
    assert regional["metrics"]["barca.partitions.total"] == 3
    assert regional["metrics"]["barca.partitions.failed"] == 1
    keys = [s for s in spans if s["name"] == "barca.partition"]
    assert {k["meta"]["barca.partition"] for k in keys} == {
        "region=emea",
        "region=amer",
        "region=apac",
    }
    assert all(k["resource"] == "regional" and k["parent_id"] == regional["span_id"] for k in keys)
    assert steps["report"]["meta"]["barca.outcome"] == "skipped"
    assert root["metrics"]["barca.steps.skipped"] == 1


def find_test_agent() -> str | None:
    """`ddapm-test-agent` (the `test` extra installs it): named, beside this Python, or on PATH."""
    import shutil
    import sys

    beside = Path(sys.executable).parent / "ddapm-test-agent"
    if os.environ.get("BARCA_DD_TEST_AGENT"):
        return os.environ["BARCA_DD_TEST_AGENT"]
    return str(beside) if beside.exists() else shutil.which("ddapm-test-agent")


SNAPSHOT = Path(__file__).parent / "snapshots" / "datadog" / "shop_trace.txt"


def test_a_pipeline_trace_passes_the_datadog_test_agent(tmp_path):
    """Datadog's APM test agent checks the payloads' format; the trace tree is a snapshot.

    Set BARCA_UPDATE_SNAPSHOTS=1 to rewrite the snapshot after a deliberate change.
    """
    import socket
    import urllib.request

    pytest.importorskip("ddtrace")
    executable = find_test_agent()
    if executable is None:
        pytest.skip("ddapm-test-agent is not installed (pip install ddapm-test-agent)")
    ports = []
    for _ in range(3):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            ports.append(s.getsockname()[1])
    port, otlp_http, otlp_grpc = ports
    test_agent = subprocess.Popen(
        [
            executable,
            "--port",
            str(port),
            "--otlp-http-port",
            str(otlp_http),
            "--otlp-grpc-port",
            str(otlp_grpc),
            # Keep check results for /test/trace_check/* instead of only refusing a payload.
            "--pool-trace-check-failures",
            "true",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    url = f"http://127.0.0.1:{port}"

    def get(path: str):
        with urllib.request.urlopen(url + path, timeout=5) as response:
            return response.status, response.read().decode()

    try:
        deadline = time.monotonic() + 30
        while True:
            try:
                get("/info")
                break
            except OSError:
                if time.monotonic() > deadline:
                    pytest.fail("the test agent did not start")
                time.sleep(0.1)
        (tmp_path / "pipeline.py").write_text(SHOP)
        proc = cli(
            tmp_path,
            "run",
            "report,publish",
            "--json",
            BARCA_TELEMETRY="datadog",
            DD_TRACE_AGENT_URL=url,
            DD_ENV="test",
            DD_TRACE_SQLITE3_ENABLED="true",
            DD_INSTRUMENTATION_TELEMETRY_ENABLED="false",
            DD_REMOTE_CONFIGURATION_ENABLED="false",
        )
        assert proc.returncode == 1, proc.stderr
        # Python spans are flushed before each step reports; give the agent a moment.
        traces: list = []
        while time.monotonic() < deadline:
            _, body = get("/test/traces")
            traces = json.loads(body)
            if sum(len(t) for t in traces) >= 17:
                break
            time.sleep(0.2)
        # The agent answers a payload that fails a check with 400, which barca would report.
        assert "telemetry" not in proc.stderr, proc.stderr
        # Pooled check failures make this answer 400, which urlopen raises.
        status, failures = get("/test/trace_check/failures?return_all=true")
        assert status == 200, failures
    finally:
        test_agent.terminate()
        test_agent.communicate(timeout=10)

    assert len(traces) == 1, "one run is one trace, Python spans included"
    lines = tree(traces[0])
    if os.environ.get("BARCA_UPDATE_SNAPSHOTS"):
        SNAPSHOT.parent.mkdir(parents=True, exist_ok=True)
        SNAPSHOT.write_text("\n".join(lines) + "\n")
    assert lines == SNAPSHOT.read_text().splitlines()
