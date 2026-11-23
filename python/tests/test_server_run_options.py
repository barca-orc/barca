"""Server Run options use the same rules in prediction, execution and durable requeue."""

from pathlib import Path

from .helpers import clean_env

PIPELINE = """
from pathlib import Path
from barca import asset, task, Manual

def mark(name):
    p = Path(name + ".calls")
    p.write_text(str(int(p.read_text()) + 1 if p.exists() else 1))

@asset
def source():
    mark("source")
    return 42

@asset(freshness=Manual, inputs={"value": source})
def model(value):
    mark("model")
    return value

@task(freshness=Manual)
def gate():
    mark("gate")

@task(inputs={"value": model, "gate": gate})
def publish(value, gate):
    mark("publish")
    print(value, gate)
"""


def setup(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return serve("--no-schedule", env=clean_env(BARCA_POOL_SIZE="2"))


def run(server, **options):
    accepted = server.post("/runs", {"command": "run", "targets": ["publish"], **options})
    result = server.finished(accepted["run_id"])
    assert result["status"] == "success", result
    return result


def calls(root: Path, name: str) -> int:
    path = root / f"{name}.calls"
    return int(path.read_text()) if path.exists() else 0


def test_including_manual_is_cache_aware_and_dry_run_matches(tmp_path, serve):
    server = setup(tmp_path, serve)
    run(server, include_manual=True)
    run(server)
    run(server, include_manual=True)
    assert [calls(tmp_path, name) for name in ("source", "model", "publish")] == [1, 1, 3]
    assert calls(tmp_path, "gate") == 2  # Included tasks execute; default holds Manual tasks.
    (tmp_path / "pipeline.py").write_text(PIPELINE.replace("return value", "return value + 1"))
    server.wait(
        "model becomes stale",
        lambda: any(
            n["name"] == "model" and n["cache"]["state"] == "stale" for n in server.get("/state")
        ),
    )
    predicted = server.post(
        "/runs", {"command": "run", "targets": ["publish"], "include_manual": True, "dry_run": True}
    )
    assert any(
        s["id"].endswith(":model") and s["action"] != "cached" for s in predicted["steps"]
    ), predicted
    run(server)
    assert calls(tmp_path, "model") == 1
    accepted = server.post(
        "/runs", {"command": "get", "targets": ["model"], "include_manual": True}
    )
    materialized = server.finished(accepted["run_id"])
    assert materialized["status"] == "success", materialized
    assert materialized["steps_executed"] == 1, materialized
    run(server, include_manual=True)
    assert calls(tmp_path, "model") == 2
    run(server, refresh_all=True)
    assert [calls(tmp_path, name) for name in ("source", "model")] == [2, 3]


def test_hard_refresh_holds_paused_inputs_even_after_restart_and_target_override(tmp_path, serve):
    server = setup(tmp_path, serve)
    run(server, include_manual=True)
    server.post("/pauses", {"target": "source"})
    server.post("/pauses", {"target": "publish"})
    server.post("/queue/pause", {})
    queued = server.post(
        "/runs",
        {
            "command": "run",
            "targets": ["publish"],
            "include_manual": True,
            "refresh_all": True,
            "allow_paused": True,
        },
    )
    port = server.port
    server.stop()
    server = serve("--no-schedule", port=port, env=clean_env(BARCA_POOL_SIZE="2"))
    server.post("/queue/resume", {})
    result = server.finished(queued["run_id"])
    assert result["status"] == "success", result
    assert [calls(tmp_path, name) for name in ("source", "model", "publish")] == [1, 2, 2]
    assert any(
        s["id"].endswith(":source") and "paused" in s.get("detail", "")
        for s in result["outcome"]["steps"]
    ), result


def test_never_computed_paused_input_is_not_executed(tmp_path, serve):
    server = setup(tmp_path, serve)
    server.post("/pauses", {"target": "source"})
    queued = server.post(
        "/runs", {"command": "run", "targets": ["publish"], "include_manual": True}
    )
    result = server.finished(queued["run_id"])
    assert result["status"] != "success", result
    assert "paused" in str(result), result
    status, predicted = server.request(
        "POST",
        "/runs",
        {"command": "run", "targets": ["publish"], "include_manual": True, "dry_run": True},
    )
    assert status == 400 and "paused" in predicted["error"], predicted
    assert calls(tmp_path, "source") == calls(tmp_path, "model") == calls(tmp_path, "publish") == 0


def test_including_manual_survives_restart_without_hard_refresh(tmp_path, serve):
    server = setup(tmp_path, serve)
    server.post("/queue/pause", {})
    queued = server.post(
        "/runs", {"command": "run", "targets": ["publish"], "include_manual": True}
    )
    port = server.port
    server.stop()
    server = serve("--no-schedule", port=port, env=clean_env(BARCA_POOL_SIZE="2"))
    server.post("/queue/resume", {})
    result = server.finished(queued["run_id"])
    assert result["status"] == "success", result
    assert calls(tmp_path, "model") == 1
