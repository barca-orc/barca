"""What a warm run costs with a store (#554), counted rather than timed.

A warm `barca get` whose value-based sensors return the same values uploads nothing (the store
already holds those bytes, by this machine's record of the confirmed upload), and takes no
metadata backup at its end (it recorded no new result, and the run before it is backed up). `BARCA_TRACE_TIMING` prints one
`[trace]  transfer <kind> <uri>` line per request to the transfer helper, so the counts below
are the requests the store saw. Explicit True signals advance observations even with identical
bytes, so their metadata must still be backed up.
"""

import json
import os
import sqlite3
import subprocess
from contextlib import closing
from pathlib import Path

import pytest
from barca.api import _find_binary

from .store_window import age_store_checks

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
from pathlib import Path

from barca import asset, sensor


@sensor()
def feed() -> str:
    return Path("data.txt").read_text()


@sensor()
def other() -> int:
    return 7


@asset(inputs={"feed": feed})
def doubled(feed: str) -> str:
    return feed * 2


@asset(inputs={"doubled": doubled, "other": other})
def report(doubled: str, other: int) -> dict:
    return {"text": doubled, "other": other}
"""


@pytest.fixture()
def project(tmp_path: Path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "data.txt").write_text("a")
    (tmp_path / "barca.toml").write_text(f'[remote]\nuri = "{tmp_path / "store"}"\n')
    return tmp_path


def get(root: Path, *args: str, target="report") -> tuple[dict, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["BARCA_TRACE_TIMING"] = "1"
    out = subprocess.run(
        [_find_binary(), "get", target, *args, "--json"],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout), out.stderr


def requests(stderr: str, kind: str) -> list[str]:
    """The `kind` requests (put, get, stat, probe) the run sent to the transfer helper."""
    marker = f"[trace]  transfer {kind} "
    return [line.split(marker, 1)[1] for line in stderr.splitlines() if marker in line]


def ran(result: dict) -> set[str]:
    return {s["id"] for s in result["steps"] if s.get("status") == "ran"}


def sensor_rows(root: Path, run_id: str) -> list[tuple[str, str]]:
    with closing(sqlite3.connect(root / ".barca" / "metadata.db")) as db:
        return db.execute(
            "SELECT node_id, artifact_path FROM materializations "
            "WHERE run_id = ? AND status = 'success' ORDER BY node_id",
            (run_id,),
        ).fetchall()


def test_a_warm_run_with_unchanged_sensors_uploads_nothing_and_takes_no_backup(project):
    _, cold = get(project)
    assert len(requests(cold, "put")) == 4, cold
    assert "[barca] backed up the metadata DB" in cold, cold

    # Past the 5-minute store check window (#593), so this run checks the store as it would
    # after a while: the window's own savings are tested in test_store_check_window.py.
    age_store_checks(project)
    warm_result, warm = get(project)
    assert ran(warm_result) == {"pipeline.py:feed", "pipeline.py:other"}, warm_result
    assert requests(warm, "put") == [], warm
    assert "[barca] uploading" not in warm, warm
    assert "[barca] backed up the metadata DB" not in warm, warm
    assert "backup_skipped (no new result)" in warm, warm
    # The cached target is still checked against the store (`get` repairs it), once.
    assert len(requests(warm, "stat")) == 1, warm
    # The sensors' polls are recorded as usual, at their store locations.
    store = str(project / "store")
    rows = sensor_rows(project, warm_result["run_id"])
    assert [node for node, _ in rows] == ["pipeline.py:feed", "pipeline.py:other"], rows
    assert all(path.startswith(store) for _, path in rows), rows
    # The next warm run is covered by the same backup too.
    _, again = get(project)
    assert "backup_skipped (no new result)" in again, again


def test_a_warm_run_backs_up_when_an_earlier_run_is_not_in_the_store(project):
    get(project)
    # The newest backup never reached the store (its upload failed or was cut): a warm run
    # that recorded nothing new still takes the backup that sends it.
    backups = project / ".barca" / "backups"
    [newest] = sorted(backups.glob("*.db"))
    newest.with_suffix(".unsent").write_text("")
    _, err = get(project)
    assert "[barca] backed up the metadata DB" in err, err
    assert not list(backups.glob("*.unsent"))


def test_a_changed_sensor_is_uploaded_with_what_it_changed_and_backed_up(project):
    get(project)
    (project / "data.txt").write_text("b")
    result, err = get(project)
    assert ran(result) == {
        "pipeline.py:feed",
        "pipeline.py:other",
        "pipeline.py:doubled",
        "pipeline.py:report",
    }, result
    # `other` returned the same value: only the changed sensor and what it changed go up.
    uploaded = requests(err, "put")
    assert len(uploaded) == 3, err
    assert not any("pipeline.py--other" in uri for uri in uploaded), uploaded
    assert not any("pipeline.py--feed" in uri for uri in requests(err, "get")), err
    assert "[barca] backed up the metadata DB" in err, err
    store_copy = next((project / "store").rglob("pipeline.py--feed/*.json"))
    assert json.loads(store_copy.read_text()) == "b"


def test_a_store_object_whose_upload_is_pending_is_uploaded_again(project):
    _, cold = get(project)
    # The sensor's object is gone and its upload is recorded as pending: the next unchanged
    # poll does not trust the record, and uploads it.
    [feed] = (project / "store").rglob("pipeline.py--feed/*.json")
    feed.unlink()
    with closing(sqlite3.connect(project / ".barca" / "metadata.db")) as db:
        (local,) = db.execute(
            "SELECT artifact_path FROM materializations WHERE node_id = 'pipeline.py:feed' "
            "ORDER BY id DESC LIMIT 1"
        ).fetchone()
        db.execute(
            "INSERT INTO pending_uploads (store_path, node_id, local_path, host) "
            "VALUES (?, 'pipeline.py:feed', ?, 'elsewhere')",
            (local, str(project / ".barca" / "nowhere.json")),
        )
        db.commit()
    _, err = get(project)
    assert any("pipeline.py--feed" in uri for uri in requests(err, "put")), err
    assert feed.exists()


def test_unchanged_sensor_can_fetch_its_previous_data_for_a_changed_consumer(project):
    pipeline = project / "pipeline.py"
    source = pipeline.read_text().replace(
        '    return Path("data.txt").read_text()',
        '    if Path("unchanged").exists():\n        return False\n'
        '    return True, Path("data.txt").read_text()',
    )
    pipeline.write_text(source)
    get(project)
    [mirror] = (project / ".barca" / "artifacts").rglob("pipeline.py--feed/*.json")
    mirror.unlink()
    (project / "unchanged").touch()
    pipeline.write_text(source.replace("return feed * 2", "return feed * 3"))
    result, err = get(project, "-o", "value")
    assert result["final_output"] == {"text": "aaa", "other": 7}
    assert any("pipeline.py--feed" in uri for uri in requests(err, "get")), err


def test_explicit_change_without_new_bytes_is_still_backed_up(project):
    pipeline = project / "pipeline.py"
    pipeline.write_text(
        pipeline.read_text().replace(
            '    return Path("data.txt").read_text()', "    return True, None"
        )
    )
    get(project, target="feed")
    _, err = get(project, target="feed")
    assert requests(err, "put") == [], err  # The payload bytes already exist.
    assert "[barca] backed up the metadata DB" in err, err
    latest = sorted((project / ".barca" / "backups").glob("*.db"))[-1]
    with closing(sqlite3.connect(project / ".barca" / "metadata.db")) as current:
        expected = current.execute(
            "SELECT observation_hash FROM materializations WHERE node_id='pipeline.py:feed' "
            "ORDER BY id DESC LIMIT 1"
        ).fetchone()[0]
    with closing(sqlite3.connect(latest)) as backup:
        observed = backup.execute(
            "SELECT observation_hash FROM materializations WHERE node_id='pipeline.py:feed' "
            "ORDER BY id DESC LIMIT 1"
        ).fetchone()[0]
    assert observed == expected
