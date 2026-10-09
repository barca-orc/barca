"""Terminal write failures must not report successful, partially recorded runs."""

import json
import os
import sqlite3
import subprocess
from pathlib import Path

from barca.api import _find_binary


def run(project: Path, *args: str):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, "BARCA_POOL_SIZE": "1"},
        capture_output=True,
        text=True,
        timeout=30,
    )


def sql(project: Path, statement: str):
    conn = sqlite3.connect(project / ".barca" / "metadata.db")
    try:
        result = conn.execute(statement).fetchall()
        conn.commit()
        return result
    finally:
        conn.close()


def test_terminal_insert_fault_is_an_infra_error_and_preserves_prior_history(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset\ndef value(): return 42\n"
    )
    first = run(tmp_path, "get", "pipeline.py", "--json")
    assert first.returncode == 0, first.stderr
    before = sql(tmp_path, "SELECT run_id, node_id, status FROM materializations")
    assert len(before) == 1
    # No Barca process is running while the fault is installed/removed.
    sql(tmp_path, "CREATE UNIQUE INDEX terminal_fault ON materializations(node_id)")
    failed = run(tmp_path, "get", "pipeline.py", "--refresh-all", "--json")
    assert failed.returncode == 3, failed.stderr
    error = json.loads(failed.stderr.strip().splitlines()[-1])
    assert error["code"] == 3
    assert "failed to record terminal step" in error["error"]
    assert sql(tmp_path, "SELECT run_id, node_id, status FROM materializations") == before
    statuses = sql(tmp_path, "SELECT status, finished_at FROM runs ORDER BY rowid")
    assert statuses[0][0] == "success"
    assert statuses[1] == ("running", None)
    sql(tmp_path, "DROP INDEX terminal_fault")
    retried = run(tmp_path, "get", "pipeline.py", "--refresh-all", "--json")
    assert retried.returncode == 0, retried.stderr
    assert json.loads(retried.stdout)["final_output"] == 42
    assert len(sql(tmp_path, "SELECT node_id FROM materializations")) == 2
