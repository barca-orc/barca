"""The remote store fails soft once the steps are done (#390), end to end through the binary.

When every step succeeded and only an upload (or the end-of-run history push) failed, the run
exits 0 with one warning naming what was not published and why. The results are recorded and
kept on this machine: `barca status` shows them `unsynced` / `local_only`, the run JSON carries
`uploads: {pending, error, nodes}`, `--dry-run` lists them, and the next command that opens the
store uploads them before its own work.

The store is a plain directory. A read-only directory where the artifacts go makes every upload
fail with a permission error while the store can still be listed (so the startup check passes);
making it writable again is the fix.
"""

import json
import os
import sqlite3
import stat
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

pytestmark = pytest.mark.skipif(os.geteuid() == 0, reason="root writes into read-only directories")

PIPELINE = """
from barca import asset


@asset()
def numbers() -> list:
    return [3, 4]


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"sum": sum(numbers)}
"""


def cli(root: Path, store: Path, *args: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["BARCA_REMOTE_URI"] = str(store)
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def read_only(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    path.chmod(stat.S_IRUSR | stat.S_IXUSR)


def writable(path: Path) -> None:
    path.chmod(stat.S_IRWXU)


def states(root: Path, store: Path) -> dict:
    doc = ok(cli(root, store, "status", "--json"))
    return {
        n["id"].rsplit(":", 1)[1]: (n["cache"]["state"], n["cache"]["reason"]) for n in doc["nodes"]
    }


def pending_rows(root: Path) -> list:
    db = sqlite3.connect(root / ".barca" / "metadata.db")
    try:
        if not db.execute("SELECT 1 FROM sqlite_master WHERE name = 'pending_uploads'").fetchone():
            return []
        return db.execute("SELECT node_id FROM pending_uploads ORDER BY node_id").fetchall()
    finally:
        db.close()


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    store = tmp_path / "store"
    artifacts = store / "default" / "artifacts"
    read_only(artifacts)
    yield root, store, artifacts
    writable(artifacts)


def test_a_failed_upload_after_the_steps_is_a_warning_and_the_next_get_uploads_it(project):
    root, store, artifacts = project

    proc = cli(root, store, "get", "total", "-o", "value", "--json")

    # Every step succeeded: exit 0, the value, and one warning naming what and why.
    doc = ok(proc)
    assert doc["final_output"] == {"sum": 7}
    assert {s["id"].rsplit(":", 1)[1]: s["status"] for s in doc["steps"]} == {
        "numbers": "ran",
        "total": "ran",
    }
    uploads = doc["uploads"]
    assert uploads["pending"] == 2, uploads
    assert uploads["nodes"] == ["pipeline.py:numbers", "pipeline.py:total"]
    assert "PermissionError" in uploads["error"], uploads
    warnings = [
        line
        for line in proc.stderr.splitlines()
        if line.startswith("[barca] warning: 2 results not uploaded")
    ]
    assert len(warnings) == 1, proc.stderr
    assert (
        "pipeline.py:numbers, pipeline.py:total" in warnings[0] and "PermissionError" in warnings[0]
    )
    assert '"kind"' not in proc.stderr  # no error envelope
    assert not list(artifacts.rglob("*.json"))

    # Recorded and kept here: unsynced, local only.
    assert states(root, store) == {
        "numbers": ("unsynced", "local_only"),
        "total": ("unsynced", "local_only"),
    }
    # Its shape is read from this machine's copy, not from the store that lacks it.
    shapes = ok(cli(root, store, "status", "--json"))
    assert all("note" not in n["shape"] for n in shapes["nodes"]), shapes
    assert shapes["summary"]["unsynced"] == 2
    table = cli(root, store, "status", "--pretty")
    assert table.returncode == 0, table.stderr
    assert "unsynced  local only" in table.stdout, table.stdout
    assert "2 unsynced" in table.stdout, table.stdout

    # The dry run lists them, and predicts nothing to compute.
    dry = ok(cli(root, store, "get", "total", "--dry-run", "--json"))
    assert dry["uploads"]["pending"] == 2 and dry["uploads"]["nodes"] == uploads["nodes"], dry
    assert dry["summary"] == {"will_run": 0, "cached": 2, "unknown": 0}
    pretty = cli(root, store, "get", "total", "--dry-run", "--pretty")
    assert "2 pending uploads from an earlier run, uploaded first" in pretty.stdout, pretty.stdout

    # Still failing: a cached get serves the local copy, exits 0 and keeps them pending.
    again = cli(root, store, "get", "total", "-o", "value", "--json")
    doc = ok(again)
    assert doc["final_output"] == {"sum": 7}
    assert doc["steps_executed"] == 0
    assert doc["uploads"]["pending"] == 2
    assert "2 pending results still not uploaded" in again.stderr, again.stderr

    # The store is fixed: the next get uploads them before its own work, then they are cached.
    writable(artifacts)
    fixed = cli(root, store, "get", "total", "--json")
    doc = ok(fixed)
    assert "uploads" not in doc, doc
    assert doc["steps_executed"] == 0
    assert "[barca] uploading 2 pending results" in fixed.stderr, fixed.stderr
    assert "[barca] uploaded 2 pending results" in fixed.stderr, fixed.stderr
    assert sorted(p.read_text() for p in artifacts.rglob("*.json")) == ["[3, 4]", '{"sum": 7}']
    assert states(root, store) == {
        "numbers": ("cached", "materialized"),
        "total": ("cached", "materialized"),
    }
    assert pending_rows(root) == []
    dry = ok(cli(root, store, "get", "total", "--dry-run", "--json"))
    assert "uploads" not in dry


def test_a_pending_upload_whose_local_file_is_gone_is_dropped(project):
    root, store, artifacts = project
    doc = ok(cli(root, store, "get", "numbers", "--json"))
    assert doc["uploads"]["pending"] == 1
    for path in (root / ".barca" / "artifacts").rglob("*.json"):
        path.unlink()
    writable(artifacts)

    proc = cli(root, store, "get", "numbers", "--json")

    doc = ok(proc)
    assert "dropped the pending upload of pipeline.py:numbers" in proc.stderr, proc.stderr
    assert "uploads" not in doc
    assert pending_rows(root) == []
    # Nothing to serve it from: computed again and uploaded.
    assert [s["status"] for s in doc["steps"]] == ["ran"]
    assert [json.loads(p.read_text()) for p in artifacts.rglob("*.json")] == [[3, 4]]


def test_a_failed_backup_upload_after_the_steps_is_a_warning(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    store = tmp_path / "store"
    ok(cli(root, store, "get", "numbers", "--json"))
    backups = store / "default" / "state" / "backups"
    assert len(list(backups.glob("*.db"))) == 1
    read_only(backups)
    try:
        proc = cli(root, store, "get", "total", "-o", "value", "--json")
        doc = ok(proc)
        assert doc["final_output"] == {"sum": 7}
        assert doc["uploads"]["pending"] == 0, doc
        assert "backup to" in doc["uploads"]["error"], doc
        assert "[barca] warning: the metadata backup was not uploaded" in proc.stderr, proc.stderr
        # The copy was taken here, and waits for the store.
        assert len(list((root / ".barca" / "backups").glob("*.unsent"))) == 1
    finally:
        writable(backups)
    # The next run's backup reaches the store.
    doc = ok(cli(root, store, "get", "total", "--json"))
    assert "uploads" not in doc
    assert not list((root / ".barca" / "backups").glob("*.unsent"))
    newest = max(p.name for p in (root / ".barca" / "backups").glob("*.db"))
    assert (backups / newest).is_file()


def test_a_store_that_fails_before_any_step_is_still_an_infra_error(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    store = tmp_path / "store"
    store.write_text("not a directory")

    proc = cli(root, store, "get", "numbers", "--json")

    assert proc.returncode == 3, proc.stderr
    assert not (root / ".barca" / "artifacts").exists() or not list(
        (root / ".barca" / "artifacts").rglob("*.json")
    )
