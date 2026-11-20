"""What the read commands do to a metadata DB they cannot read in place (#1056, A7).

The read commands read through a read-only view of the database. Three databases have no
view: one written by an earlier release and not yet completed, one in rollback-journal mode
(not WAL), and a 0-byte file. Reading through views must not change what a command does to
those. Each assertion below is what the release before views did, compared with its binary
when this was written:

- `status`, its rollup and `sql` refuse a 0-byte file (exit 3) and leave it as it is;
- they complete an earlier release's database, and return a rollback-journal one to WAL mode,
  at the step that always did (before `status` reads the nodes' history), to the same bytes
  `history` leaves, which has always opened the database as a writer does first;
- `doctor` reads a copy and changes none of them.

A new `.barca/metadata.db.lock` (empty) is expected: these reads are made under the lock.
"""

import json
import shutil
import sqlite3
import tarfile
from contextlib import closing
from pathlib import Path

import pytest

from .helpers import barca

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "metadata"
# The legacy format 0, and the first release of format 1 (test_metadata_compat.py).
OLD = ["0.20.1", "0.21.0"]

STATUS = ("status", "pipeline.py", "--json")
ROLLUP = ("status", "pipeline.py", "--rollup", "--json")
SQL = ("sql", "SELECT count(*) AS n FROM barca.runs", "pipeline.py", "--json")
HISTORY = ("history", "--json")
DOCTOR = ("doctor", "pipeline.py", "--json")


def db(project: Path) -> Path:
    return project / ".barca" / "metadata.db"


def files(project: Path) -> dict[str, bytes]:
    """The database and what lies beside it, apart from the lock file."""
    return {
        p.name: p.read_bytes()
        for p in sorted((project / ".barca").glob("metadata.db*"))
        if p.name != "metadata.db.lock"
    }


def journal_mode(project: Path) -> str:
    with closing(sqlite3.connect(f"file:{db(project)}?mode=ro", uri=True)) as conn:
        return conn.execute("PRAGMA journal_mode").fetchone()[0]


def tables(project: Path) -> dict[str, int]:
    """Rows per table: what a conversion or a completion must not lose."""
    with closing(sqlite3.connect(f"file:{db(project)}?mode=ro", uri=True)) as conn:
        names = [r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")]
        return {n: conn.execute(f'SELECT count(*) FROM "{n}"').fetchone()[0] for n in names}


def old(tmp_path: Path, name: str, version: str) -> Path:
    project = tmp_path / name
    project.mkdir()
    shutil.copy(FIXTURES / "pipeline.py", project / "pipeline.py")
    with tarfile.open(FIXTURES / f"{version}.tar.gz") as tar:
        tar.extractall(project, filter="data")
    return project


def current(tmp_path: Path, name: str) -> Path:
    """A project whose database this build wrote (one run; `broken` fails, so exit 1)."""
    project = tmp_path / name
    project.mkdir()
    shutil.copy(FIXTURES / "pipeline.py", project / "pipeline.py")
    assert barca(project, "get", "pipeline.py").returncode == 1
    assert set(files(project)) == {"metadata.db"}, "a command that writes leaves no log"
    return project


def without_the_instant(out: str) -> dict:
    return {k: v for k, v in json.loads(out).items() if k != "as_of"}


@pytest.mark.parametrize("command", [STATUS, ROLLUP, SQL])
def test_an_empty_file_is_refused_and_left_as_it_is(tmp_path, command):
    project = tmp_path / "project"
    (project / ".barca").mkdir(parents=True)
    shutil.copy(FIXTURES / "pipeline.py", project / "pipeline.py")
    db(project).write_bytes(b"")
    result = barca(project, *command)
    assert result.returncode == 3, result.stderr
    error = json.loads(result.stderr.strip().splitlines()[-1])
    assert error["reason"] == "metadata_unusable", error
    assert "it is empty (0 bytes)" in error["error"], error
    assert result.stdout == ""
    assert files(project) == {"metadata.db": b""}


def test_doctor_changes_no_database_it_cannot_read_in_place(tmp_path):
    empty = tmp_path / "empty"
    (empty / ".barca").mkdir(parents=True)
    shutil.copy(FIXTURES / "pipeline.py", empty / "pipeline.py")
    db(empty).write_bytes(b"")
    rollback = current(tmp_path, "rollback")
    with closing(sqlite3.connect(db(rollback))) as conn:
        assert conn.execute("PRAGMA journal_mode=DELETE").fetchone() == ("delete",)
    projects = [empty, rollback, *(old(tmp_path, v, v) for v in OLD)]
    for project in projects:
        before = files(project)
        result = barca(project, *DOCTOR)
        assert result.returncode in (0, 3), result.stderr
        after = files(project)
        # A reader of a WAL database rebuilds the log's index; the database and its log stay.
        before.pop("metadata.db-shm", None)
        after.pop("metadata.db-shm", None)
        assert after == before, project.name
    assert journal_mode(rollback) == "delete"


@pytest.mark.parametrize("command", [STATUS, ROLLUP, SQL])
def test_a_rollback_journal_database_answers_as_a_wal_one_does(tmp_path, command):
    project = current(tmp_path, "project")
    in_place = barca(project, *command)
    assert in_place.returncode == 0, in_place.stderr
    rows = tables(project)
    with closing(sqlite3.connect(db(project))) as conn:
        assert conn.execute("PRAGMA journal_mode=DELETE").fetchone() == ("delete",)
    for name in ("metadata.db-wal", "metadata.db-shm"):
        (project / ".barca" / name).unlink(missing_ok=True)

    result = barca(project, *command)
    assert result.returncode == 0, result.stderr
    assert without_the_instant(result.stdout) == without_the_instant(in_place.stdout)
    # As before views: the step that completes a database returns it to WAL mode, and the
    # process that did it was a writer, so it leaves no log behind.
    # (Looked at first: this test's own read-only connections below create a log, as any do.)
    assert set(files(project)) == {"metadata.db"}
    assert journal_mode(project) == "wal"
    assert tables(project) == rows


@pytest.mark.parametrize("version", OLD)
def test_an_earlier_release_s_database_is_completed_as_it_always_was(tmp_path, version):
    opened = old(tmp_path, "history", version)
    assert barca(opened, *HISTORY).returncode == 0
    completed = files(opened)
    assert set(completed) == {"metadata.db"}
    answers = {}
    for command in (STATUS, ROLLUP, SQL):
        project = old(tmp_path, command[-2].strip("-") + str(len(command)), version)
        result = barca(project, *command)
        assert result.returncode == 0, result.stderr
        # The same bytes `history` leaves: completed once, by the one migration, no more.
        assert files(project) == completed, command
        answers[command] = result.stdout
        # Complete now, so read in place, and nothing more is written. (The answers are not
        # compared: `status` made its cache decisions before the completion the first time,
        # as it always has, and reads a sensor's recorded output only the second.)
        again = barca(project, *command)
        assert again.returncode == 0, again.stderr
        assert files(project)["metadata.db"] == completed["metadata.db"], command
    assert json.loads(answers[SQL])["rows"] == [{"n": 3}]


def test_a_read_command_leaves_an_empty_log_that_the_next_writer_removes(tmp_path):
    """What `barca docs layout` says remains: a read-only connection cannot delete the log."""
    project = current(tmp_path, "project")
    database = files(project)["metadata.db"]
    for command in (STATUS, HISTORY):
        assert barca(project, *command).returncode == 0
        left = files(project)
        assert left["metadata.db"] == database, command
        assert left["metadata.db-wal"] == b"", command
        assert set(left) == {"metadata.db", "metadata.db-wal", "metadata.db-shm"}, command
    assert barca(project, "get", "pipeline.py").returncode == 1
    assert set(files(project)) == {"metadata.db"}
