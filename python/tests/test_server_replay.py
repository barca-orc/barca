"""Replays retain the durable execution scope, including after a server restart."""

import json

from .helpers import clean_env

PIPELINE = """
from pathlib import Path
from barca import asset, partitions, task

def mark(name):
    p = Path(name + ".calls")
    p.write_text(str(int(p.read_text()) + 1 if p.exists() else 1))

@asset
def source():
    mark("source")
    return 42

@asset(inputs={"source": source}, partitions={"region": partitions(["emea", "amer"])})
def regional(region, source):
    mark(region)
    return source

@task(inputs={"source": source})
def publish(source):
    print(source)
    mark("publish")
"""


def calls(root, name):
    path = root / f"{name}.calls"
    return int(path.read_text()) if path.exists() else 0


def replay(server, run_id, **options):
    status, accepted = server.request("POST", f"/runs/{run_id}/replay", options)
    assert status == 202, accepted
    result = server.finished(accepted["run_id"])
    assert result["status"] == "success", result
    return result


def test_replay_keeps_partition_and_selective_refresh_after_restart(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    server = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    accepted = server.post(
        "/runs",
        {
            "command": "get",
            "targets": ["regional"],
            "partition": "region=emea",
            "refresh": ["regional"],
            "cascade": False,
            "include_manual": True,
            "queue": "last",
            "batch": ["none"],
        },
    )
    first = server.finished(accepted["run_id"])
    assert first["status"] == "success", first
    scope = first["scope"]
    assert json.loads(scope["partition"]) == {"region": ["emea"]}
    assert scope["refresh"] == ["regional"]
    assert scope["pin_manual"] is False
    assert scope["hold_paused"] is True
    assert scope["queue"] == "last"
    assert scope["batch"] == ["none"]
    server.stop()
    server = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    assert server.get(f"/runs/{first['run_id']}")["scope"] == scope
    replay(server, first["run_id"])
    assert [calls(tmp_path, n) for n in ("source", "emea", "amer")] == [1, 2, 0]
    replay(server, first["run_id"], cache="cached")
    assert [calls(tmp_path, n) for n in ("source", "emea", "amer")] == [1, 2, 0]
    replay(server, first["run_id"], cache="refresh_all")
    assert [calls(tmp_path, n) for n in ("source", "emea", "amer")] == [2, 3, 0]


def test_retry_step_selects_one_key_and_rejects_unrecorded_steps(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    server = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    accepted = server.post("/runs", {"command": "get", "targets": ["regional"]})
    first = server.finished(accepted["run_id"])
    step = next(s["node_id"] for s in first["steps"] if s["node_id"].endswith("[region=emea]"))
    rerun = replay(server, first["run_id"], cache="refresh_all", step=step)
    assert json.loads(rerun["scope"]["partition"]) == {"region": ["emea"]}
    assert [calls(tmp_path, n) for n in ("emea", "amer")] == [2, 1]
    status, _ = server.request(
        "POST", f"/runs/{first['run_id']}/replay", {"step": "pipeline.py:publish"}
    )
    assert status == 400


def test_scheduled_sensor_and_task_follower_replay_as_the_original_tick(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text("""
from pathlib import Path
from barca import sensor, task, Schedule, Always

@sensor(freshness=Schedule("* * * * * *"))
def signal():
    return Path("version").read_text()

@task(freshness=Always, inputs={"signal": signal})
def publish(signal):
    print(signal)
    p = Path("publish.calls")
    p.write_text(str(int(p.read_text()) + 1 if p.exists() else 1))
""")
    (tmp_path / "version").write_text("one")
    server = serve(env=clean_env(BARCA_POOL_SIZE="2"))
    job = server.wait("sensor tick", lambda: (j := server.schedule()["signal"])["last_run"] and j)
    first = server.finished(job["last_run"])
    server.stop()
    server = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    before = calls(tmp_path, "publish")
    replay(server, first["run_id"], cache="cached")
    assert calls(tmp_path, "publish") == before
    (tmp_path / "version").write_text("two")
    result = replay(server, first["run_id"], cache="cached")
    assert result["scope"]["tick"] == first["scope"]["tick"]
    assert result["triggered_by"] == "api"
    assert result["scheduled_at"] is None
    assert calls(tmp_path, "publish") == before + 1
    task = next(s["node_id"] for s in first["steps"] if s["node_id"].endswith(":publish"))
    retry = replay(server, first["run_id"], cache="cached", step=task)
    assert retry["command"] == "run"
    assert calls(tmp_path, "publish") == before + 2


def test_replay_retains_paused_target_override_and_rejects_unknown_old_scope(tmp_path, serve):
    import sqlite3

    (tmp_path / "pipeline.py").write_text(PIPELINE)
    server = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    server.post("/pauses", {"target": "publish"})
    first = server.finished(
        server.post(
            "/runs",
            {
                "command": "run",
                "targets": ["publish"],
                "allow_paused": True,
            },
        )["run_id"]
    )
    assert first["status"] == "success", first
    replay(server, first["run_id"], cache="cached")
    assert calls(tmp_path, "publish") == 2
    server.stop()
    # Simulate history from before scope recording, while no server owns the database.
    with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
        db.execute("UPDATE runs SET scope_json = NULL WHERE run_id = ?", (first["run_id"],))
    server = serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))
    assert server.get(f"/runs/{first['run_id']}")["scope"] is None
    status, body = server.request("POST", f"/runs/{first['run_id']}/replay", {})
    assert status == 409, body
    assert "no recorded request scope" in body["error"]
    assert calls(tmp_path, "publish") == 2
