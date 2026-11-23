"""An explicit task skip is neither a successful refresh nor a failed tick (#811)."""

import json

import pytest

from .helpers import barca, write_module


def test_task_skip_records_reason_without_a_result_or_retry(tmp_path):
    write_module(
        tmp_path,
        "pipeline.py",
        """
from pathlib import Path
from barca import Skip, task

@task(retries=3)
def copy():
    with Path("attempts").open("a") as f:
        f.write("called\\n")
    raise Skip("another copy holds the lock")
""",
        strip=True,
    )
    proc = barca(tmp_path, "run", "copy", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    assert result["status"] == "skipped"
    assert result["skip_reason"] == "another copy holds the lock"
    assert result["final_output"] is None
    assert result["steps"][0]["status"] == "skipped"
    assert result["steps"][0]["skip_reason"] == "another copy holds the lock"
    assert (tmp_path / "attempts").read_text() == "called\n"
    history = json.loads(barca(tmp_path, "history", result["run_id"], "--json").stdout)
    assert history["run"]["status"] == "skipped"
    assert history["steps"][0]["status"] == "skipped"
    assert history["steps"][0]["skip_reason"] == "another copy holds the lock"


def test_task_skip_mixed_runs_keep_success_and_failure(tmp_path):
    write_module(
        tmp_path,
        "pipeline.py",
        """
from barca import Skip, task
@task
def skipped():
    raise Skip("no work")
@task
def good():
    return 42
@task
def bad():
    raise ValueError("broken")
""",
        strip=True,
    )
    for targets, status, code in [("skipped,good", "success", 0), ("skipped,bad", "failed", 1)]:
        proc = barca(tmp_path, "run", targets, "pipeline.py", "--json")
        assert proc.returncode == code, proc.stderr
        result = json.loads(proc.stdout)
        assert result["status"] == status
        assert result["targets"]["skipped"]["status"] == "skipped"
        history = json.loads(barca(tmp_path, "history", result["run_id"], "--json").stdout)
        assert history["run"]["status"] == status


def test_skip_in_asset_is_an_error(tmp_path):
    write_module(
        tmp_path,
        "pipeline.py",
        """
from barca import Skip, asset
@asset
def value():
    raise Skip("tasks only")
""",
        strip=True,
    )
    proc = barca(tmp_path, "get", "value", "pipeline.py", "--json")
    assert proc.returncode == 1
    assert json.loads(proc.stdout)["status"] == "failed"


def test_task_skip_blocks_downstream_without_copying_reason(tmp_path):
    write_module(
        tmp_path,
        "pipeline.py",
        """
from barca import Skip, task
@task
def source():
    raise Skip("source unavailable")
@task(inputs={"source": source})
def consumer(source):
    raise AssertionError("must not run")
""",
        strip=True,
    )
    proc = barca(tmp_path, "run", "consumer", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    assert result["status"] == "skipped"
    steps = {s["id"].split(":")[-1]: s for s in result["steps"]}
    assert steps["source"]["skip_reason"] == "source unavailable"
    assert steps["consumer"]["reason"] == "upstream_skipped"
    assert "skip_reason" not in steps["consumer"]


@pytest.mark.parametrize("batch", ["none", "all"])
def test_skipped_partition_history_counts_survive_pagination(tmp_path, serve, batch):
    import sqlite3

    write_module(
        tmp_path,
        "pipeline.py",
        """
from barca import Skip, partitions, task
@task(partitions={"key": partitions(["a", "b", "c"])})
def copy(key):
    raise Skip(f"locked {key}")
""",
        strip=True,
    )
    proc = barca(tmp_path, "run", "copy", "pipeline.py", "--json", "--batch", batch)
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    assert result["status"] == "skipped"
    run_id = result["run_id"]
    history = json.loads(barca(tmp_path, "history", run_id, "--json", "-l", "1").stdout)
    counts = history["explanation"]["counts"]
    assert (counts["ran"], counts["ok"], counts["skipped"], counts["failed"]) == (3, 0, 3, 0)
    assert history["steps_page"]["total"] == 3
    assert len(history["steps"]) == 1
    with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
        rows = db.execute(
            "SELECT status, run_hash, artifact_path, output_json FROM materializations"
        ).fetchall()
    assert len(rows) == 3
    assert all(row == ("skipped", None, None, None) for row in rows)
    reader = serve("--no-schedule")
    page = reader.get(f"/runs/{run_id}?limit=1&offset=1")
    assert page["explanation"]["counts"]["skipped"] == 3
    assert page["steps_page"]["total"] == 3
    assert len(page["steps"]) == 1
    assert page["steps"][0]["skip_reason"].startswith("locked ")
    listing = reader.get("/runs?status=skipped")
    assert listing["runs"][0]["status"] == "skipped"


@pytest.mark.parametrize("after", ["success", "failed", "scheduler_skip"])
def test_schedule_reports_skipped_task_reason_after_restart(tmp_path, serve, after):
    import sqlite3

    write_module(
        tmp_path,
        "pipeline.py",
        """
from pathlib import Path
from barca import Schedule, Skip, task
@task(freshness=Schedule("* * * * *"))
def copy():
    if Path("next").exists():
        if Path("next").read_text() == "failed":
            raise ValueError("copy failed")
        return "copied"
    raise Skip("external lock held")
""",
        strip=True,
    )
    proc = barca(tmp_path, "run", "copy", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    node = result["steps"][0]["id"]
    with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
        db.execute(
            "UPDATE runs SET triggered_by='schedule', schedule_id=?, scheduled_at=strftime('%Y-%m-%dT%H:%M:%SZ', started_at) WHERE run_id=?",
            (node, result["run_id"]),
        )
    reader = serve("--no-schedule")
    schedule = reader.get("/schedule")[0]
    assert schedule["last_status"] == "skipped"
    assert schedule["last_task_skip_reason"] == "external lock held"
    assert schedule["failures"] == 0
    assert not schedule["failing"]
    state = json.loads(barca(tmp_path, "status", "pipeline.py", "--json").stdout)
    copy = next(n for n in state["nodes"] if n["id"] == node)
    assert copy["last_attempt"] == "skipped"
    assert copy["last_materialization"]["skip_reason"] == "external lock held"
    reader.stop()
    if after == "scheduler_skip":
        with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
            db.execute(
                "INSERT INTO schedule_ticks (schedule_id,scheduled_at,action) VALUES (?,strftime('%s','now')+60,'skipped')",
                (node,),
            )
    else:
        (tmp_path / "next").write_text(after)
        next_run = barca(tmp_path, "run", "copy", "pipeline.py", "--json")
        next_id = json.loads(next_run.stdout)["run_id"]
        with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
            db.execute(
                "UPDATE runs SET triggered_by='schedule', schedule_id=?, scheduled_at=strftime('%Y-%m-%dT%H:%M:%SZ', started_at) WHERE run_id=?",
                (node, next_id),
            )
    next_schedule = serve("--no-schedule").get("/schedule")[0]
    assert "last_task_skip_reason" not in next_schedule
    assert next_schedule["last_status"] == ("skipped" if after == "scheduler_skip" else after)


def test_served_skip_matches_wire_contract(tmp_path, serve):
    from .helpers import wait_until
    from .test_http_openapi_client import validate

    write_module(
        tmp_path,
        "pipeline.py",
        """
from barca import Skip, task
@task
def copy():
    raise Skip("already handled elsewhere")
""",
        strip=True,
    )
    server = serve("--no-schedule")
    started = server.post("/runs", {"command": "run", "targets": ["copy"]})
    run_id = started["run_id"]
    wait_until(lambda: server.get(f"/runs/{run_id}")["run"]["status"] not in ("pending", "running"))
    detail = server.get(f"/runs/{run_id}")
    assert detail["run"]["status"] == "skipped", detail
    from barca.client import Client, Run

    with pytest.warns(DeprecationWarning):
        client = Client(server.url)
    assert Run(client, run_id).wait(timeout=2)["run"]["status"] == "skipped"
    validate({"$ref": "#/components/schemas/RunView"}, detail)
    assert detail["run"]["skip_reason"] == "already handled elsewhere"
    assert detail["run"]["outcome"]["status"] == "skipped"
    assert detail["explanation"]["counts"]["ok"] == 0
    assert detail["explanation"]["counts"]["skipped"] == 1
    listing = server.get("/runs")
    validate({"$ref": "#/components/schemas/Runs"}, listing)
    assert listing["activity"]["total"]["by_status"]["skipped"] == 1
    assert listing["activity"]["total"]["not_successful"] == 0


def test_mixed_partition_skips_leave_successful_keys_available(tmp_path):
    write_module(
        tmp_path,
        "pipeline.py",
        """
from barca import Skip, partitions, task
@task(partitions={"key": partitions(["a", "b"])})
def source(key):
    if key == "a":
        raise Skip("locked a")
    return key
@task(inputs={"source": source}, partitions={"key": partitions(["a", "b"])})
def consumer(source, key):
    assert key == "b"
    return source
""",
        strip=True,
    )
    proc = barca(tmp_path, "run", "consumer", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    assert result["status"] == "success"
    assert result["final_output"] == {"key=b": "b"}
    skipped = next(s for s in result["steps"] if s.get("skip_reason"))
    assert skipped["skip_reason"] == "locked a"


def test_requeued_task_skip_stays_with_its_archived_attempt(tmp_path, serve):
    """Requeue preserves skip evidence without assigning it to the pending attempt."""
    write_module(
        tmp_path,
        "pipeline.py",
        """
import time
from pathlib import Path
from barca import Skip, task

@task()
def skipped():
    raise Skip("no work in this attempt")

@task()
def held():
    Path("held.started").touch()
    while not Path("release").exists():
        time.sleep(0.02)
""",
        strip=True,
    )
    server = serve("--no-schedule", "--drain-timeout", "0", root=tmp_path)
    run_id = server.post("/runs", {"command": "run", "targets": ["skipped", "held"]})["run_id"]
    server.wait("held task to start", lambda: (tmp_path / "held.started").exists())
    server.wait(
        "explicit skip to arrive",
        lambda: any(step.get("skip_reason") for step in server.get(f"/runs/{run_id}")["steps"]),
    )
    server.post("/queue/pause", {})
    assert server.stop() == 0, server.stderr
    restarted = serve("--no-schedule", root=tmp_path)
    pending = restarted.get(f"/runs/{run_id}")
    assert pending["run"]["status"] == "pending", pending
    assert not pending["run"].get("skip_reason"), pending
    assert pending["steps"] == [], pending
    assert pending["explanation"]["counts"].get("skipped", 0) == 0, pending
    attempt = pending["run"]["previous_attempts"][0]
    archived = restarted.get(f"/runs/{attempt['run_id']}")
    assert archived["run"]["parent_run_id"] == run_id
    assert archived["run"]["run_attempt"] == 1
    assert archived["explanation"]["counts"]["skipped"] == 1, archived
    skips = [step for step in archived["steps"] if step["status"] == "skipped"]
    assert len(skips) == 1, archived
    assert skips[0]["skip_reason"] == "no work in this attempt"
    (tmp_path / "release").touch()
    restarted.post("/queue/resume", {})
    assert restarted.finished(run_id)["run"]["status"] == "success"
    assert restarted.get(f"/runs/{attempt['run_id']}")["steps"] == archived["steps"]
