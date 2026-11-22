"""One lifecycle vocabulary before/after a server loses its in-memory overlay (#943)."""

import json

from .helpers import barca
from .served import Serve, snapshot


def test_timeout_status_and_reason_survive_restart(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "import time\nfrom barca import task\n@task()\ndef slow(): time.sleep(60)\n"
    )
    with Serve(tmp_path, "--no-schedule", "--run-timeout", "1") as server:
        run_id = server.submit("slow")["run_id"]
        detail_doc = server.wait("timeout", lambda: (d := server.get(f"/runs/{run_id}"))["run"]["status"] == "failed" and d, timeout=5)
        assert set(detail_doc) == {"run", "steps", "steps_page", "explanation"}
        detail = detail_doc["run"]
        listed = next(r for r in server.get("/runs")["runs"] if r["run_id"] == run_id)
        history = server.get("/history")["runs"][0]
        assert (listed["status"], detail["status"], history["status"]) == (
            "failed", "failed", "failed"
        )
        for row in (listed, detail, history):
            assert "timed out" in row["reason"]
        with snapshot(tmp_path) as db:
            assert db.execute("SELECT status FROM runs WHERE run_id = ?", (run_id,)).fetchone() == (
                "failed",
            )
    with Serve(tmp_path, "--no-schedule") as server:
        after = server.get(f"/runs/{run_id}")["run"]
        assert after["status"] == detail["status"]
        assert after["reason"] == detail["reason"]
    cli = barca(tmp_path, "history", "--local", "--json")
    assert cli.returncode == 0, cli.stderr
    assert json.loads(cli.stdout)["runs"][0]["reason"] == detail["reason"]


def test_pending_is_filterable_in_history(tmp_path):
    (tmp_path / "pipeline.py").write_text("from barca import task\n@task()\ndef one(): pass\n")
    with Serve(tmp_path, "--no-schedule") as server:
        server.post("/queue/pause", {})
        run_id = server.submit("one")["run_id"]
        code, history = server.request("GET", "/history?status=pending")
        assert code == 200, history
        assert [(r["run_id"], r["status"]) for r in history["runs"]] == [(run_id, "pending")]
        cli = barca(tmp_path, "history", "--status", "pending", "--json")
        assert cli.returncode == 0, cli.stderr
        assert json.loads(cli.stdout)["runs"][0]["run_id"] == run_id
        server.request("DELETE", f"/runs/{run_id}")


def test_cancelled_and_legacy_terminal_reasons_are_readable(tmp_path):
    import sqlite3

    (tmp_path / "pipeline.py").write_text("from barca import task\n@task()\ndef one(): pass\n")
    with Serve(tmp_path, "--no-schedule") as server:
        server.post("/queue/pause", {})
        run_id = server.submit("one")["run_id"]
        server.request("DELETE", f"/runs/{run_id}")
        cancelled = server.finished(run_id)
        assert cancelled["status"] == "cancelled"
        assert cancelled["reason"]
    # Simulate retention trimming the large result, plus rows written by an older release.
    with sqlite3.connect(tmp_path / ".barca/metadata.db") as db:
        db.execute("UPDATE runs SET outcome_json = NULL WHERE run_id = ?", (run_id,))
        for old_id, status in [("old-error", "error"), ("old-interrupted", "interrupted")]:
            db.execute(
                "INSERT INTO runs (run_id, command, files, status) VALUES (?, 'run', '[]', ?)",
                (old_id, status),
            )
    with Serve(tmp_path, "--no-schedule") as server:
        assert server.run(run_id)["reason"] == cancelled["reason"]
        for old_id, status in [("old-error", "failed"), ("old-interrupted", "interrupted")]:
            doc = server.get(f"/runs/{old_id}")
            assert doc["run"]["status"] == status
            assert doc["run"]["reason"]
            assert server.get(f"/status/{old_id}") == doc
        failed = server.get("/history?status=failed")["runs"]
        assert [r["run_id"] for r in failed] == ["old-error"]


def test_pending_history_survives_restart_and_obeys_filters(tmp_path):
    (tmp_path / "pipeline.py").write_text("from barca import task\n@task()\ndef one(): pass\n")
    with Serve(tmp_path, "--no-schedule") as server:
        server.post("/queue/pause", {})
        run_id = server.submit("one")["run_id"]
    local = barca(tmp_path, "history", "--local", "--status", "pending", "--json")
    assert local.returncode == 0, local.stderr
    assert json.loads(local.stdout)["runs"][0]["run_id"] == run_id
    with Serve(tmp_path, "--no-schedule") as server:
        for path in ["/runs", "/history"]:
            listed = server.get(f"{path}?status=pending&search={run_id}")
            assert listed["total"] == 1
            assert listed["runs"][0]["run_id"] == run_id
            assert listed["runs"][0]["reason"] is None
            assert server.get(f"{path}?status=running")["runs"] == []
        assert server.get("/runs")["activity"]["total"]["by_status"]["pending"] == 1
        server.request("DELETE", f"/runs/{run_id}")


def test_schedule_times_are_rfc3339(tmp_path):
    from datetime import datetime

    (tmp_path / "pipeline.py").write_text(
        'from barca import task, Schedule\n@task(freshness=Schedule("0 0 1 1 *"))\ndef yearly(): pass\n'
    )
    with Serve(tmp_path) as server:
        jobs = server.wait("schedule", lambda: server.get("/schedule"))
        at = jobs[0]["next_fire"]
        assert isinstance(at, str) and at.endswith("Z")
        assert datetime.fromisoformat(at).utcoffset().total_seconds() == 0
        node = server.get("/state")[0]
        assert node["schedule"]["next_fire"] == at
