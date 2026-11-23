"""The owner-approved 0.47 run vocabulary and envelopes across hosts (#943)."""

import json
from datetime import datetime

import pytest

from .helpers import barca, clean_env, wait_until
from .served import Serve

PIPELINE = """
import time
from barca import asset, Schedule

@asset(freshness=Schedule("0 0 0 1 1 *"))
def good():
    return 1

@asset
def bad():
    raise RuntimeError("contract failure")

@asset
def slow():
    time.sleep(30)
    return 1
"""


def project(root):
    (root / "pipeline.py").write_text(PIPELINE)


def cli(root, *args):
    result = barca(root, *args, env=clean_env(BARCA_POOL_SIZE="2"))
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def finished(server, run_id):
    # Poll either representation so the assertion, not a helper KeyError, identifies drift.
    def done():
        detail = server.get(f"/runs/{run_id}")
        run = detail.get("run", detail)
        return detail if run["status"] not in ("pending", "running") else None

    return wait_until(done, "terminal run")


def rfc3339(value):
    assert isinstance(value, str) and "T" in value, value
    assert datetime.fromisoformat(value.replace("Z", "+00:00")).utcoffset() is not None


def test_detail_envelope_and_history_names_are_shared(tmp_path):
    project(tmp_path)
    with Serve(tmp_path, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as server:
        accepted = server.post("/runs", {"command": "get", "targets": ["bad"]})
        run_id = accepted["run_id"]
        detail = finished(server, run_id)
        assert {"run", "steps", "steps_page", "explanation"} <= detail.keys(), detail
        assert "status" not in detail
        alias = server.get(f"/status/{run_id}")
        assert alias == detail, (alias, detail)
        run = detail["run"]
        assert run["run_id"] == run_id and run["status"] == "failed"
        rfc3339(run["started_at"])
        rfc3339(run["finished_at"])
        for path in ("/runs", "/history"):
            row = next(r for r in server.get(path)["runs"] if r["run_id"] == run_id)
            assert "id" not in row and "first_failure" not in row, row
            assert row["failure"]["node"] == "pipeline.py:bad"
            assert row["status"] == run["status"]
            rfc3339(row["started_at"])
        local = cli(tmp_path, "history", run_id, "--local", "--json")
        assert {"run", "steps", "steps_page", "explanation"} <= local.keys()
        assert local["run"]["status"] == run["status"]
        rfc3339(local["run"]["started_at"])


def test_pending_runs_are_filterable_history_before_execution(tmp_path):
    project(tmp_path)
    with Serve(tmp_path, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as server:
        server.post("/queue/pause", {})
        run_id = server.post("/runs", {"command": "get", "targets": ["good"]})["run_id"]
        for path in ("/runs?status=pending", "/history?status=pending"):
            status, body = server.request("GET", path)
            assert status == 200, body
            row = next(r for r in body["runs"] if r["run_id"] == run_id)
            assert row["status"] == "pending"
            rfc3339(row["started_at"])
        local = cli(tmp_path, "history", "--local", "--status", "pending", "--json")
        assert any(r["run_id"] == run_id and r["status"] == "pending" for r in local["runs"])
        server.request("DELETE", f"/runs/{run_id}")
        cancelled = cli(tmp_path, "history", run_id, "--local", "--json")["run"]
        assert cancelled["status"] == "cancelled"
        rfc3339(cancelled["finished_at"])


@pytest.mark.parametrize("restart", [False, True])
def test_timeout_is_failed_in_detail_list_and_history(tmp_path, restart):
    project(tmp_path)
    with Serve(
        tmp_path, "--no-schedule", "--run-timeout", "1", env=clean_env(BARCA_POOL_SIZE="2")
    ) as server:
        run_id = server.post("/runs", {"command": "get", "targets": ["slow"]})["run_id"]
        finished(server, run_id)
        if not restart:
            check_timeout(server, run_id)
    if restart:
        with Serve(tmp_path, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as server:
            check_timeout(server, run_id)


def check_timeout(server, run_id):
    detail = server.get(f"/runs/{run_id}")
    run = detail.get("run", detail)
    assert run["status"] == "failed", run
    assert run["error"] and any(
        word in run["error"].lower() for word in ("timeout", "timed out")
    ), run
    assert run["outcome"]["status"] == "failed", run
    for path in ("/runs", "/history"):
        row = next(r for r in server.get(path)["runs"] if r["run_id"] == run_id)
        assert row["status"] == "failed", row


def test_schedule_timestamps_use_rfc3339(tmp_path):
    project(tmp_path)
    with Serve(tmp_path, env=clean_env(BARCA_POOL_SIZE="2")) as server:
        job = server.schedule()["good"]
        rfc3339(job["next_fire"])
        node = next(n for n in server.get("/state") if n["id"].endswith(":good"))
        assert node["next_run"] == job["next_fire"]
        rfc3339(node["prev_run"])
        if node["late_since"] is not None:
            rfc3339(node["late_since"])
        server.post("/schedule/good/pause?until=1h", {})
        [paused] = server.get("/pauses")["schedules"]
        rfc3339(paused["paused_at"])
        rfc3339(paused["until"])
        assert paused["until"] == server.schedule()["good"]["paused_until"]
        for key in ("last_fired", "last_skipped", "late_since", "paused_until"):
            if job[key] is not None:
                rfc3339(job[key])


def test_interrupted_run_has_a_reason_without_inventing_an_end_time(tmp_path):
    import socket
    import sqlite3

    project(tmp_path)
    run_id = cli(tmp_path, "get", "good", "pipeline.py", "--local", "--json")["run_id"]
    # No server or writer is alive: simulate an older process that died before finalization.
    with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
        db.execute(
            "UPDATE runs SET status='running', outcome_json=NULL, finished_at=NULL, "
            "elapsed_seconds=NULL, pid=2147483647, host=?, owner=NULL WHERE run_id=?",
            (socket.gethostname(), run_id),
        )
    with Serve(tmp_path, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as server:
        detail = server.get(f"/runs/{run_id}")
        run = detail.get("run", detail)
        assert run["status"] == "interrupted", run
        assert run["error"], run
        assert run["finished_at"] is None


def test_pending_history_survives_restart_and_becomes_one_completed_run(tmp_path):
    project(tmp_path)
    with Serve(tmp_path, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as server:
        server.post("/queue/pause", {})
        run_id = server.post("/runs", {"command": "get", "targets": ["good"]})["run_id"]
    local = cli(tmp_path, "history", "--local", "--status", "pending", "--json")
    assert [r["run_id"] for r in local["runs"]] == [run_id]
    with Serve(tmp_path, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as server:
        assert server.get(f"/runs/{run_id}")["run"]["status"] == "pending"
        server.post("/queue/resume", {})
        assert finished(server, run_id)["run"]["status"] == "success"
    rows = cli(tmp_path, "history", "--local", "--json")["runs"]
    assert len(rows) == 1 and rows[0]["run_id"] == run_id
    assert rows[0]["status"] == "success"
