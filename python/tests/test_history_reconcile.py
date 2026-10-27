"""Explicit recovery for stopped runs whose owner cannot be verified (#290)."""

import json
import os
import sqlite3
import subprocess
import time
from contextlib import closing

import pytest
from barca.api import _find_binary


def cli(project, *args):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, "BARCA_REMOTE": "off"},
        capture_output=True,
        text=True,
        timeout=30,
    )


@pytest.fixture()
def stale(tmp_path):
    # Fixtures create a real run and artifact, then reproduce a legacy row from
    # an inaccessible owner. Its successful materialization must survive recovery.
    (tmp_path / "p.py").write_text(
        "from barca import asset\n@asset()\ndef value():\n    return 42\n"
    )
    proc = cli(tmp_path, "get", "value", "p.py", "-o", "value", "--json")
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    path = tmp_path / ".barca/metadata.db"
    with closing(sqlite3.connect(path)) as conn:
        conn.execute(
            "UPDATE runs SET status='running', owner='', pid=1, host='unavailable-container', "
            "finished_at=NULL, elapsed_seconds=NULL WHERE run_id=?",
            (result["run_id"],),
        )
        conn.commit()
        rows = conn.execute("SELECT * FROM materializations").fetchall()
    return tmp_path, path, result["run_id"], rows


@pytest.mark.parametrize(
    "mode", [["--json"], ["--pretty"], ["--fields", "run_id,status,finished_at"]]
)
def test_unknown_owner_is_reconciled_only_on_explicit_request(stale, mode):
    project, path, run_id, materializations = stale
    before = cli(project, "history", "--json")
    assert before.returncode == 0, before.stderr
    assert json.loads(before.stdout)["runs"][0]["status"] == "running"
    reconciled = cli(project, "history", "--reconcile", run_id, *mode)
    assert reconciled.returncode == 0, reconciled.stderr
    if "--pretty" in mode:
        assert run_id[:12] in reconciled.stdout and "interrupted" in reconciled.stdout
    else:
        result = json.loads(reconciled.stdout)
        assert result["total"] == 1 and not result["truncated"]
        assert result["runs"][0]["run_id"] == run_id
        assert result["runs"][0]["status"] == "interrupted"
        assert result["runs"][0]["finished_at"] is None
    with closing(sqlite3.connect(path)) as conn:
        assert conn.execute(
            "SELECT status, finished_at, elapsed_seconds FROM runs WHERE run_id=?", (run_id,)
        ).fetchone() == ("interrupted", None, None)
        assert conn.execute("SELECT * FROM materializations").fetchall() == materializations
    cached = cli(project, "get", "value", "p.py", "-o", "value", "--json")
    assert cached.returncode == 0, cached.stderr
    assert json.loads(cached.stdout)["steps_executed"] == 0
    assert json.loads(cached.stdout)["final_output"] == 42


def test_missing_terminal_and_conflicting_requests_are_usage_errors(stale):
    project, path, run_id, _ = stale
    for args in [("missing",), (run_id, "--all"), (run_id, "--limit", "1")]:
        result = cli(project, "history", "--reconcile", *args, "--json")
        assert result.returncode == 2, result.stderr
    assert cli(project, "history", "--reconcile", run_id, "--json").returncode == 0
    terminal = cli(project, "history", "--reconcile", run_id, "--json")
    assert terminal.returncode == 2 and "already interrupted" in terminal.stderr
    with closing(sqlite3.connect(path)) as conn:
        conn.execute("UPDATE runs SET status='success' WHERE run_id=?", (run_id,))
        conn.commit()
    assert cli(project, "history", "--reconcile", run_id, "--json").returncode == 2


def test_reconciliation_refuses_a_proven_live_owner(tmp_path):
    (tmp_path / "p.py").write_text("""
from barca import asset
from pathlib import Path
import time
@asset(timeout_seconds=20)
def held():
    Path("ready").touch()
    while not Path("release").exists():
        time.sleep(0.01)
    return 42
""")
    proc = subprocess.Popen(
        [_find_binary(), "get", "held", "p.py", "-o", "value", "--json"],
        cwd=tmp_path,
        env={**os.environ, "BARCA_REMOTE": "off"},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 15
        while not (tmp_path / "ready").exists():
            assert proc.poll() is None, "run exited before the ready event"
            assert time.monotonic() < deadline, "ready event did not arrive"
            time.sleep(0.01)
        history = cli(tmp_path, "history", "--json")
        assert history.returncode == 0, history.stderr
        run = json.loads(history.stdout)["runs"][0]
        assert run["status"] == "running"
        denied = cli(tmp_path, "history", "--reconcile", run["run_id"], "--json")
        assert denied.returncode == 2 and "live owner" in denied.stderr
        after = cli(tmp_path, "history", "--json")
        assert after.returncode == 0, after.stderr
        assert json.loads(after.stdout)["runs"][0]["status"] == "running"
    finally:
        (tmp_path / "release").touch()
        try:
            stdout, stderr = proc.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.communicate()
            raise
    assert proc.returncode == 0, stderr
    assert json.loads(stdout)["final_output"] == 42
