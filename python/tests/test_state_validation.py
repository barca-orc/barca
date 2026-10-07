"""A pulled shared history must be valid before it replaces the local one, and the local
database a pull replaces is kept (#243, RFC-0006 section 4.1, `barca docs remote`).

The shared history is one object. Every machine downloads it and puts it in the place of its
own `.barca/metadata.db`, so an object that is not a database barca can use must stop at the
door: the command exits 3, names the object, and the local database, its unpushed rows and the
object itself are exactly as they were. "Valid" is defined in
`crates/barca-core/src/state_validate.rs`; the cases here are what a user can meet.

"Machines" are project directories that share one state location on the local filesystem.
"""

import json
import shutil
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest

from barca.api import _find_binary

from . import test_state_pull
from .test_state_pull import (
    KEPT_THE_KILLED_RUN,
    WAIT,
    Machine,
    no_pull_leftovers,
    quick,
    shared_runs,
)

# The same machines and shared state location as the pull tests.
machines = test_state_pull.machines
state_uri = test_state_pull.state_uri

PAGE = 4096
NOT_USABLE = "is not a database barca can use"


def sqlite_file(path: Path, *statements: str) -> bytes:
    """The bytes of a new SQLite database made by running `statements`."""
    path.unlink(missing_ok=True)
    with sqlite3.connect(path) as conn:
        for statement in statements:
            conn.execute(statement)
    conn.close()
    return path.read_bytes()


def edited(blob: bytes, scratch: Path, *statements: str) -> bytes:
    """`blob` (a shared history) after running `statements` on it with stock SQLite."""
    scratch.write_bytes(blob)
    with sqlite3.connect(scratch) as conn:
        for statement in statements:
            conn.execute(statement)
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()
    return scratch.read_bytes()


def invalid_blobs(good: bytes, scratch: Path) -> dict[str, tuple[bytes, str]]:
    """Objects that must never replace a local history: name -> (bytes, what the error says)."""
    assert len(good) % PAGE == 0 and len(good) >= 12 * PAGE, len(good)
    zeroed = bytearray(good)
    zeroed[4 * PAGE : 8 * PAGE] = bytes(4 * PAGE)
    return {
        "empty": (b"", "empty (0 bytes)"),
        "garbage": (b"<html>503 Service Unavailable</html>", "too short to be a database"),
        "not sqlite": (b"x" * (3 * PAGE), "does not start with a SQLite header"),
        # A transfer that stopped part-way: inside a page, and exactly between two pages
        # (then the header and the length both look right).
        "cut short inside a page": (good[: len(good) - 1000], "cut short"),
        "cut short between pages": (good[: 6 * PAGE], ""),
        "pages overwritten": (bytes(zeroed), ""),
        "a database with no tables": (
            sqlite_file(scratch, "PRAGMA user_version = 7", "VACUUM"),
            "not a barca history",
        ),
        "another program's database": (
            sqlite_file(
                scratch, "CREATE TABLE notes (body TEXT)", "INSERT INTO notes VALUES ('x')"
            ),
            "no `runs` table",
        ),
        "tables with barca's names and other columns": (
            sqlite_file(
                scratch,
                "CREATE TABLE runs (id INTEGER PRIMARY KEY, name TEXT)",
                "CREATE TABLE materializations (id INTEGER PRIMARY KEY, what TEXT)",
            ),
            "has no `run_id` column",
        ),
        "a newer schema this version cannot write to": (
            edited(
                good,
                scratch,
                "ALTER TABLE runs RENAME TO runs_old",
                "CREATE TABLE runs (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT UNIQUE"
                " NOT NULL, command TEXT NOT NULL, files TEXT NOT NULL, target TEXT, status TEXT"
                " NOT NULL DEFAULT 'running', steps_total INTEGER, steps_executed INTEGER"
                " DEFAULT 0, steps_cached INTEGER DEFAULT 0, started_at TEXT, finished_at TEXT,"
                " elapsed_seconds REAL, pid INTEGER, host TEXT, tenant TEXT NOT NULL)",
                "DROP TABLE runs_old",
            ),
            "newer barca",
        ),
    }


INVALID = [
    "empty",
    "garbage",
    "not sqlite",
    "cut short inside a page",
    "cut short between pages",
    "pages overwritten",
    "a database with no tables",
    "another program's database",
    "tables with barca's names and other columns",
    "a newer schema this version cannot write to",
]


def local_files(machine: Machine) -> dict[str, bytes]:
    """Every file of the local history, by name: the database, its log, the kept generation."""
    return {
        p.name: p.read_bytes()
        for p in sorted(machine.db.parent.iterdir())
        if p.is_file() and p.name.startswith("metadata.db") and not p.name.endswith(".lock")
    }


def refused(out: subprocess.CompletedProcess, state_uri: Path, why: str = "") -> None:
    """The command failed as an infrastructure failure that names the object, says what is
    wrong with it, that nothing changed, and where the repair is described."""
    assert out.returncode == 3, (out.returncode, out.stderr)
    assert str(state_uri) in out.stderr, out.stderr
    assert NOT_USABLE in out.stderr and why in out.stderr, out.stderr
    assert "was left as it was, and nothing was uploaded" in out.stderr, out.stderr
    assert "If the shared history is damaged" in out.stderr, out.stderr
    # Not the advice for an unreachable store.
    assert "connection or credentials" not in out.stderr, out.stderr


@pytest.mark.parametrize("kind", INVALID)
def test_an_invalid_shared_history_never_replaces_the_local_one(
    kind, machines, state_uri, tmp_path
):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    a_local = a.get("a_local", BARCA_STATE="off")  # recorded here only, never uploaded
    good = state_uri.read_bytes()
    bad, why = invalid_blobs(good, tmp_path / "scratch.db")[kind]
    state_uri.write_bytes(bad)
    before = local_files(a)

    assert set(invalid_blobs(good, tmp_path / "scratch.db")) == set(INVALID)

    # Every command that pulls: a run, a dry run, and status.
    for args in (
        ("get", "a_one.py", "--json"),
        ("get", "a_one.py", "--dry-run", "--json"),
        ("status", "a_one.py", "--json"),
    ):
        out = a.barca(*args)
        refused(out, state_uri, why)
        # In JSON mode the reason is the error envelope, the last line on stderr.
        envelope = json.loads(out.stderr.strip().splitlines()[-1])
        assert (envelope["code"], envelope["kind"]) == (3, "infra"), envelope
        assert local_files(a) == before, args
        assert state_uri.read_bytes() == bad, "the command wrote to the shared history"
        no_pull_leftovers(a)
    assert a.local_runs() == {a_first, a_local}

    # Once the object is good again the machine carries on, and what it had recorded only
    # locally goes up with its next run.
    state_uri.write_bytes(good)
    a_next = a.get("a_two")
    assert set(shared_runs(state_uri)) == {a_first, b_run, a_local, a_next}


def test_an_invalid_shared_history_does_not_become_the_history_of_a_new_machine(
    machines, state_uri, tmp_path
):
    a, new = machines("a"), machines("new")
    a.get("a_one")
    good = state_uri.read_bytes()
    for kind, (bad, why) in invalid_blobs(good, tmp_path / "scratch.db").items():
        state_uri.write_bytes(bad)
        (new.root / "n_one.py").write_text(quick("n_one"))
        for args in (("get", "n_one.py", "--json"), ("status", "n_one.py", "--json")):
            refused(new.barca(*args), state_uri, why)
            assert not new.db.exists(), f"{kind}: a database was created from an invalid download"
            assert not Path(f"{new.db}.prev").exists(), kind
            assert state_uri.read_bytes() == bad, kind
            if new.db.parent.exists():
                no_pull_leftovers(new)


def test_a_killed_runs_rows_survive_an_invalid_download_and_are_carried_by_the_next_good_one(
    machines, state_uri
):
    a, b = machines("a"), machines("b")
    b_run = b.get("b_one")
    killed = a.kill(a.start_slow())  # its run row and first step are local only
    good = state_uri.read_bytes()

    bad = good[: len(good) // 2 // PAGE * PAGE]
    state_uri.write_bytes(bad)
    for args in (("status", "slow.py", "--json"), ("get", "slow.py", "--json")):
        refused(a.barca(*args), state_uri)
        assert a.local_runs() == {b_run, killed}
        assert state_uri.read_bytes() == bad
    assert (a.root / "first.ran").read_text() == "x"  # the refused `get` ran nothing

    state_uri.write_bytes(good)
    out = a.resume_slow()
    assert KEPT_THE_KILLED_RUN in out.stderr, out.stderr
    # The step the killed run finished was not run again.
    assert (a.root / "first.ran").read_text() == "x"
    assert json.loads(out.stdout)["steps_executed"] == 1, out.stdout
    assert shared_runs(state_uri)[killed] == "interrupted"
    assert set(shared_runs(state_uri)) == {b_run, killed, json.loads(out.stdout)["run_id"]}


def test_a_valid_download_whose_carry_fails_changes_nothing(machines, state_uri, tmp_path):
    a, b = machines("a"), machines("b")
    a.get("a_one")
    a.barca("status", "a_one.py", "--json")
    b.get("b_one")
    assert a.barca("status", "a_one.py", "--json").returncode == 0  # a pull that changes A
    kept = Path(f"{a.db}.prev").read_bytes()
    # Two runs recorded on A only, each with a step of the same node.
    for _ in range(2):
        out = a.barca("get", "a_one.py", "--refresh-all", "--json", BARCA_STATE="off")
        assert out.returncode == 0, out.stderr
        assert json.loads(out.stdout)["steps_executed"] == 1, out.stdout

    # The shared history is a valid barca database on which A's rows cannot all be added: an
    # index only it has refuses the second step of a node (after the runs and the first step
    # are in).
    good = state_uri.read_bytes()
    state_uri.write_bytes(
        edited(
            good,
            tmp_path / "scratch.db",
            "DELETE FROM materializations WHERE node_id LIKE '%a_one'",
            "CREATE UNIQUE INDEX one_step_per_node ON materializations(node_id)",
        )
    )
    before = local_files(a)
    runs_before = a.local_runs()
    out = a.barca("status", "a_one.py", "--json")
    assert out.returncode == 3, out.stderr
    assert "left as it was" in out.stderr, out.stderr
    assert local_files(a) == before
    assert Path(f"{a.db}.prev").read_bytes() == kept
    no_pull_leftovers(a)

    state_uri.write_bytes(good)
    assert a.barca("status", "a_one.py", "--json").returncode == 0
    assert a.local_runs() == runs_before


# ─── the database a pull replaces is kept ────────────────────────────────────


def runs_in(db_file: Path, tmp_path: Path) -> set[str]:
    """The runs in a single database file, read from a copy with stock SQLite."""
    copy = tmp_path / "look.db"
    for leftover in tmp_path.glob("look.db*"):
        leftover.unlink()
    shutil.copyfile(db_file, copy)
    with sqlite3.connect(copy) as conn:
        assert conn.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        runs = {r for (r,) in conn.execute("SELECT run_id FROM runs")}
    conn.close()
    return runs


def test_the_database_a_pull_replaces_is_kept_as_prev(machines, state_uri, tmp_path):
    a, b = machines("a"), machines("b")
    prev = Path(f"{a.db}.prev")
    a_first = a.get("a_one")
    # Nothing to keep yet: there was no local database before A's first pull, and the pulls
    # since have found the shared history equal to the local one.
    assert a.barca("status", "a_one.py", "--json").returncode == 0
    assert not prev.exists()

    a_local = a.get("a_local", BARCA_STATE="off")
    b_run = b.get("b_one")
    assert a.barca("status", "a_one.py", "--json").returncode == 0
    # The pull replaced A's database with B's upload plus A's own rows; what it replaced is
    # one whole file, the unpushed run included.
    assert a.local_runs() == {a_first, a_local, b_run}
    assert runs_in(prev, tmp_path) == {a_first, a_local}
    first_generation = (prev.read_bytes(), prev.stat().st_ino)

    # Pulls that change nothing leave it alone, so it stays what the last change replaced.
    for _ in range(3):
        assert a.barca("status", "a_one.py", "--json").returncode == 0
        assert a.barca("get", "a_one.py", "--dry-run", "--json").returncode == 0
    assert (prev.read_bytes(), prev.stat().st_ino) == first_generation

    # One generation: the next pull that changes the database replaces it.
    a_next = a.get("a_two")  # uploads; the shared history then equals the local one
    b_two = b.get("b_two")
    assert a.barca("status", "a_one.py", "--json").returncode == 0
    assert runs_in(prev, tmp_path) == {a_first, a_local, b_run, a_next}
    assert a.local_runs() == {a_first, a_local, b_run, a_next, b_two}
    no_pull_leftovers(a)


def test_restoring_the_kept_database_as_documented(machines, state_uri, tmp_path):
    """`barca docs remote`, "Going back to the local history from before a pull"."""
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    assert a.barca("status", "a_one.py", "--json").returncode == 0
    assert a.local_runs() == {a_first, b_run}

    # The two commands of the manual, in the project directory, with no barca command running.
    for command in (
        "cp .barca/metadata.db.prev .barca/metadata.db",
        "rm -f .barca/metadata.db-wal .barca/metadata.db-shm",
    ):
        subprocess.run(command, shell=True, cwd=a.root, check=True)

    # With shared history off, the machine works on the history it had before the pull.
    assert a.local_runs() == {a_first}
    out = a.barca("get", "a_one.py", "--json", BARCA_STATE="off")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["steps_executed"] == 0, out.stdout
    a_off = json.loads(out.stdout)["run_id"]
    assert a.local_runs() == {a_first, a_off}

    # With it on again, the next pull brings the shared history back and keeps what was
    # recorded meanwhile.
    assert a.barca("status", "a_one.py", "--json").returncode == 0
    assert a.local_runs() == {a_first, a_off, b_run}


def test_making_the_kept_database_the_shared_history_again_as_documented(
    machines, state_uri, tmp_path
):
    """`barca docs remote`, "Going back to the local history from before a pull", last step:
    the shared history itself is put back to what this machine had."""
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    a_second = a.get("a_two")
    b.get("b_one")  # the upload to be undone
    assert a.barca("status", "a_one.py", "--json").returncode == 0
    assert runs_in(Path(f"{a.db}.prev"), tmp_path) == {a_first, a_second}

    # The manual: restore the local database from `.prev`, remove the shared object, run.
    for command in (
        "cp .barca/metadata.db.prev .barca/metadata.db",
        "rm -f .barca/metadata.db-wal .barca/metadata.db-shm",
    ):
        subprocess.run(command, shell=True, cwd=a.root, check=True)
    state_uri.unlink()
    out = a.barca("get", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert "no shared state yet" in out.stderr, out.stderr
    assert json.loads(out.stdout)["steps_executed"] == 0, out.stdout
    a_third = json.loads(out.stdout)["run_id"]
    assert set(shared_runs(state_uri)) == {a_first, a_second, a_third}


def test_repairing_a_damaged_shared_history_from_a_local_copy_as_documented(machines, state_uri):
    """`barca docs remote`, "If the shared history is damaged": remove the object, then run on
    the machine whose local history is the most complete."""
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    b_run = b.get("b_one")
    a_second = a.get("a_two")  # A now holds everything
    b_local = b.get("b_local", BARCA_STATE="off")  # and B one run nobody else has
    state_uri.write_bytes(b"garbage")

    refused(a.barca("get", "a_one.py", "--json"), state_uri)
    refused(b.barca("status", "b_one.py", "--json"), state_uri)

    # 1. Which machine holds the most: `barca history` reads the local copy and pulls nothing.
    assert a.local_runs() == {a_first, b_run, a_second}
    assert b.local_runs() == {a_first, b_run, b_local}
    # 2. Move the damaged object out of the way. 3. Run on that machine.
    state_uri.rename(state_uri.with_name("metadata.db.damaged"))
    out = a.barca("get", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert "no shared state yet" in out.stderr, out.stderr
    a_third = json.loads(out.stdout)["run_id"]
    assert set(shared_runs(state_uri)) == {a_first, b_run, a_second, a_third}
    # 4. Every other machine pulls it and adds what only it holds with its next run.
    b_next = b.get("b_two")
    everything = {a_first, b_run, a_second, a_third, b_local, b_next}
    assert set(shared_runs(state_uri)) == everything
    assert b.local_runs() == everything


# ─── at the same time, and switched off ──────────────────────────────────────

# Stands in for a store that hands one of two simultaneous downloads a file cut short. Both
# downloads wait until the other has started, so the two pulls overlap; the one that finds
# `cut.next` (taken with an atomic rename, so exactly one does) truncates what it downloaded.
ONE_BAD_DOWNLOAD = """#!/bin/sh
case "$*" in
  *"barca._state pull"*)
    for last in "$@"; do :; done
    touch "pull.started.$$"
    while [ "$(ls pull.started.* 2>/dev/null | wc -l)" -lt 2 ]; do sleep 0.05; done
    {python} "$@"; status=$?
    if mv cut.next "cut.taken.$$" 2>/dev/null; then
      {python} -c "import os,sys; os.truncate(sys.argv[1], os.path.getsize(sys.argv[1]) // 2 // 4096 * 4096)" "$last"
    fi
    exit $status ;;
esac
exec {python} "$@"
"""


def test_of_two_pulls_at_once_one_given_a_bad_download_fails_and_the_other_lands(
    machines, state_uri, tmp_path
):
    a, b = machines("a"), machines("b")
    a_first = a.get("a_one")
    a_local = a.get("a_local", BARCA_STATE="off")
    b_run = b.get("b_one")
    good = state_uri.read_bytes()

    wrapped = tmp_path / "wrapped-bin"
    wrapped.mkdir()
    (wrapped / "barca").write_bytes(Path(_find_binary()).read_bytes())
    (wrapped / "barca").chmod(0o755)
    (wrapped / "python").write_text(ONE_BAD_DOWNLOAD.format(python=sys.executable))
    (wrapped / "python").chmod(0o755)
    (a.root / "cut.next").write_text("")

    procs = [
        subprocess.Popen(
            [str(wrapped / "barca"), "status", "a_one.py", "--json"],
            cwd=a.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=a.env,
        )
        for _ in range(2)
    ]
    done = [(p.communicate(timeout=WAIT), p.returncode) for p in procs]
    # One download was cut. The pull that got it fails, unless the other pull landed first:
    # then its download was already overtaken, is thrown away unread, and it downloads again.
    assert len(list(a.root.glob("cut.taken.*"))) == 1
    codes = sorted(code for _, code in done)
    assert codes in ([0, 3], [0, 0]), [(code, err) for (_, err), code in done]
    for (_, stderr), code in done:
        if code == 3:
            refused(subprocess.CompletedProcess([], 3, "", stderr), state_uri)

    # The good download landed, with A's unpushed run; the bad one changed nothing.
    assert a.local_runs() == {a_first, a_local, b_run}
    assert runs_in(Path(f"{a.db}.prev"), tmp_path) == {a_first, a_local}
    assert state_uri.read_bytes() == good
    no_pull_leftovers(a)


def test_with_shared_history_off_an_invalid_object_is_not_looked_at(machines, state_uri):
    a = machines("a")
    a_first = a.get("a_one")
    state_uri.write_bytes(b"garbage")
    a_off = a.get("a_two", BARCA_STATE="off")
    out = a.barca("status", "a_one.py", "--json", BARCA_STATE="off")
    assert out.returncode == 0, out.stderr
    assert a.local_runs() == {a_first, a_off}
    assert state_uri.read_bytes() == b"garbage"
    assert not Path(f"{a.db}.prev").exists()


def test_an_upload_is_a_valid_shared_history_and_keeps_nothing(machines, state_uri, tmp_path):
    """The push path is as it was: what a run uploads passes the check the next pull makes,
    and uploading does not touch `.prev`."""
    a, b = machines("a"), machines("b")
    runs = {a.get("a_one"), a.get("a_two")}
    assert not Path(f"{a.db}.prev").exists()
    assert set(shared_runs(state_uri)) == runs  # also asserts integrity_check is ok
    # A machine with nothing pulls it as its history, with no complaint.
    (b.root / "a_one.py").write_text(quick("a_one"))
    out = b.barca("status", "a_one.py", "--json")
    assert out.returncode == 0, out.stderr
    assert NOT_USABLE not in out.stderr
    assert b.local_runs() == runs
