"""A store checked in the last 5 minutes is not checked again by a run that transfers nothing (#593).

A successful startup check of the artifact store is remembered in `.barca/gc-state.json`, by
environment and store URI. A warm run within 5 minutes of it starts no transfer helper unless it
uploads or fetches. `BARCA_TRACE_TIMING` prints `[trace]  transfer helper started` once per
helper process, and `[trace]  transfer <kind> <uri>` once per request, so these tests count
helper starts and store requests rather than time them.
"""

import json
import os
import socket
import sqlite3
import stat
import subprocess
from contextlib import closing
from pathlib import Path

import pytest
from barca.api import _find_binary

from .store_window import age_store_checks, store_checks

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")
CHECKING = "[barca] checking artifact store "
STARTED = "[trace]  transfer helper started"

PIPELINE = """
from pathlib import Path

from barca import asset, sensor


@sensor()
def feed() -> str:
    return Path("data.txt").read_text()


@asset(inputs={"feed": feed})
def doubled(feed: str) -> str:
    return feed * 2


@asset()
def constant() -> int:
    return 7


@asset(inputs={"doubled": doubled, "constant": constant})
def report(doubled: str, constant: int) -> dict:
    return {"text": doubled, "constant": constant}
"""


@pytest.fixture()
def project(tmp_path: Path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "data.txt").write_text("a")
    (tmp_path / "barca.toml").write_text(f'[remote]\nuri = "{tmp_path / "store"}"\n')
    return tmp_path


def barca(root: Path, *args: str, code: int = 0) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["BARCA_TRACE_TIMING"] = "1"
    out = subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert out.returncode == code, out.stderr
    return out


def get(root: Path, *args: str) -> tuple[dict, str]:
    out = barca(root, "get", *(args or ("report",)), "--json")
    return json.loads(out.stdout), out.stderr


def helpers(stderr: str) -> int:
    """How many transfer helper processes the command started."""
    return sum(1 for line in stderr.splitlines() if line.strip() == STARTED.strip())


def requests(stderr: str, kind: str) -> list[str]:
    """The `kind` requests (put, get, stat, probe) the command sent to the transfer helper."""
    marker = f"[trace]  transfer {kind} "
    return [line.split(marker, 1)[1] for line in stderr.splitlines() if marker in line]


def store_object(root: Path, node: str) -> Path:
    [found] = (root / "store").rglob(f"pipeline.py--{node}/*.json")
    return found


def test_a_warm_run_within_5_minutes_of_a_check_starts_no_helper(project):
    _, cold = get(project)
    assert helpers(cold) == 1 and CHECKING in cold, cold
    [check] = store_checks(project)
    assert check["env"] == "default" and check["uri"].startswith(str(project / "store")), check

    result, warm = get(project)
    assert helpers(warm) == 0, warm
    assert CHECKING not in warm, warm
    assert "[trace]  transfer " not in warm, warm
    assert "store_check_skipped" in warm, warm
    assert "backup_skipped (no new result)" in warm, warm
    assert {s["id"] for s in result["steps"] if s["status"] == "ran"} == {"pipeline.py:feed"}
    # A run within the window does not extend it.
    assert store_checks(project) == [check]

    # A get of a target that is cached here, with no sensor upstream: no helper either.
    _, cached = get(project, "constant")
    assert helpers(cached) == 0, cached


def test_a_deleted_store_object_is_noticed_once_the_window_ends(project):
    get(project)
    report = store_object(project, "report")
    report.unlink()

    _, inside = get(project)
    assert helpers(inside) == 0, inside
    assert not report.exists()

    age_store_checks(project)
    _, after = get(project)
    assert helpers(after) == 1 and CHECKING in after, after
    assert len(requests(after, "stat")) == 1, after
    assert "pipeline.py:report: not in the artifact store; uploading" in after, after
    assert report.exists()
    # The check starts a new window.
    [check] = store_checks(project)
    _, again = get(project)
    assert helpers(again) == 0, again
    assert store_checks(project) == [check]


def test_a_run_that_uploads_within_the_window_starts_the_helper_when_it_needs_it(project):
    get(project)
    (project / "data.txt").write_text("b")
    result, err = get(project)
    assert helpers(err) == 1, err
    # The startup check is not repeated; the uploads go through as usual.
    assert CHECKING not in err, err
    assert len(requests(err, "put")) == 3, err
    assert "uploads" not in result, result
    assert json.loads(store_object(project, "feed").read_text()) == "b"


def test_a_run_that_fetches_within_the_window_checks_the_store_first(project):
    get(project)
    local = next((project / ".barca" / "artifacts").rglob("pipeline.py--constant/*.json"))
    local.unlink()
    result, err = get(project, "constant")
    assert helpers(err) == 1, err
    # A fetch checks the store first, as at startup: a store that is gone is not a missing
    # object to compute again.
    assert CHECKING in err and len(requests(err, "probe")) == 1, err
    assert len(requests(err, "get")) == 1, err
    assert local.exists()
    assert result["steps"][0]["status"] == "cached", result


@pytest.mark.parametrize(
    "case",
    ["expired", "other_store", "other_env", "refresh", "refresh_all", "pending_upload"],
)
def test_these_runs_check_the_store_within_the_window(project, case):
    get(project)
    args: tuple[str, ...] = ("report",)
    if case == "expired":
        age_store_checks(project)
    elif case == "other_store":
        (project / "barca.toml").write_text(f'[remote]\nuri = "{project / "store2"}"\n')
    elif case == "other_env":
        args = ("report", "--env", "dev")
    elif case == "refresh":
        args = ("report", "--refresh", "constant")
    elif case == "refresh_all":
        args = ("report", "--refresh-all")
    elif case == "pending_upload":
        # This machine's result whose upload is pending (its store object is gone).
        store_object(project, "constant").unlink()
        local = next((project / ".barca" / "artifacts").rglob("pipeline.py--constant/*.json"))
        with closing(sqlite3.connect(project / ".barca" / "metadata.db")) as db:
            (at,) = db.execute(
                "SELECT artifact_path FROM materializations "
                "WHERE node_id = 'pipeline.py:constant' ORDER BY id DESC LIMIT 1"
            ).fetchone()
            db.execute(
                "INSERT INTO pending_uploads (store_path, node_id, local_path, host) "
                "VALUES (?, 'pipeline.py:constant', ?, ?)",
                (at, str(local), socket.gethostname()),
            )
            db.commit()
    _, err = get(project, *args)
    assert CHECKING in err, err
    assert len(requests(err, "probe")) == 1, err
    assert helpers(err) == 1, err
    if case == "pending_upload":
        assert "[barca] uploaded 1 pending result" in err, err
        assert store_object(project, "constant").exists()


def test_a_failed_upload_ends_the_window(project):
    get(project)
    store = project / "store"
    modes = {p: p.stat().st_mode for p in [store, *store.rglob("*")]}
    try:
        for p, mode in modes.items():
            p.chmod(mode & ~(stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH))
        (project / "data.txt").write_text("b")
        result, err = get(project)
        assert helpers(err) == 1, err
        assert result["uploads"]["pending"] >= 1, result
        assert store_checks(project) == [], err
    finally:
        for p, mode in modes.items():
            p.chmod(mode)
    _, retry = get(project)
    assert CHECKING in retry, retry
    assert "[barca] uploaded" in retry, retry
    assert len(store_checks(project)) == 1


def test_doctor_and_gc_check_the_store_within_the_window(project):
    get(project)
    assert len(store_checks(project)) == 1

    doctor = json.loads(barca(project, "doctor", "--json").stdout)
    names = json.dumps(doctor)
    assert "store:artifacts" in names, doctor

    # gc looks at the store itself: a local copy whose store object is gone is not one the
    # store holds, so it is not evicted as such.
    store_object(project, "report").unlink()
    gc = json.loads(barca(project, "gc", "--dry-run", "--max-size", "1", "--json").stdout)
    held = [e["path"] for e in gc["evicted"] if e.get("tier") == "store_held"]
    assert not any("pipeline.py--report" in p for p in held), gc
    assert any("pipeline.py--constant" in p for p in held), gc
