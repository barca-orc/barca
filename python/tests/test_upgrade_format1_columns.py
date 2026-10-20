"""A metadata database written by barca 0.21-0.23 opens with this release.

Format 1 gained `materializations.bytes_changed` in 0.24.0 without a version bump, and 0.24.0
to 0.24.2 refused every older format-1 database with exit 3. Opening one now adds the missing
column in place (specs/metadata-schema.md, "Additions within format 1").
"""

import json
import os
import sqlite3
import subprocess
from contextlib import closing
from pathlib import Path

import pytest
from barca.api import _find_binary

PIPELINE = """
from barca import asset

@asset()
def one() -> int:
    return 1
"""


def _barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
        text=True,
        capture_output=True,
        timeout=120,
    )


def _as_written_by_0_23(project: Path) -> None:
    """Remove what format 1 added in 0.24.0, as a database last written by 0.23 lacks it."""
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as db:
        db.execute("ALTER TABLE materializations DROP COLUMN bytes_changed")
        db.commit()


def _columns(project: Path) -> set[str]:
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as db:
        return {row[1] for row in db.execute("PRAGMA table_info(materializations)")}


@pytest.mark.parametrize(
    "first",
    [
        ["status", "p.py", "--json"],
        ["history", "--json"],
        ["list", "p.py", "--json"],
        ["get", "one", "p.py", "--json"],
    ],
    ids=lambda args: args[0],
)
def test_a_database_from_before_bytes_changed_opens(tmp_path, first):
    (tmp_path / "p.py").write_text(PIPELINE)
    assert _barca(tmp_path, "get", "one", "p.py", "--json").returncode == 0
    _as_written_by_0_23(tmp_path)
    assert "bytes_changed" not in _columns(tmp_path)

    result = _barca(tmp_path, *first)
    assert result.returncode == 0, result.stderr

    again = _barca(tmp_path, "get", "one", "p.py", "--json")
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout)["status"] == "success"
    assert "bytes_changed" in _columns(tmp_path)

    history = _barca(tmp_path, "history", "--json")
    assert history.returncode == 0, history.stderr
