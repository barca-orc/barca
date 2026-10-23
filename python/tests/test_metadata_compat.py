"""This build opens a metadata DB written by every older release (#629).

Format 1 is additive (specs/metadata-schema.md, "Additions within format 1"): a release may add
a column, a table or an index without a new version number, and must add it in place to a
database an older release wrote. 0.29.2 added `logs.stream` and `logs.ts` and refused every
database from before them, because the check that runs before the migration did not know them.

Each fixture under `fixtures/metadata/` is the `.barca/` directory a released barca left after
running `fixtures/metadata/pipeline.py` (a sensor, an asset, a task that prints to stdout and
stderr, and a failure), built by `scripts/make-metadata-fixtures.py` from the wheels on PyPI.
They are committed (about 12 KB each) so CI needs no network. Add the newest release's fixture
when a release changes the schema: a schema change that forgets its migration then fails here.
"""

import json
import os
import shutil
import sqlite3
import subprocess
import tarfile
from contextlib import closing
from pathlib import Path

import pytest
from barca.api import _find_binary

from .served import Serve

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "metadata"
VERSIONS = sorted(
    (p.name.removesuffix(".tar.gz") for p in FIXTURES.glob("*.tar.gz")),
    key=lambda v: tuple(int(part) for part in v.split(".")),
)
# 0.20.1 is the last release in the legacy format 0, 0.21.0 the first in format 1.
LEGACY = {"0.20.1"}


def test_there_is_a_fixture_per_minor_from_the_first_format_1_release():
    minors = {tuple(v.split(".")[:2]) for v in VERSIONS}
    newest = max(int(v.split(".")[1]) for v in VERSIONS)
    assert {("0", str(m)) for m in range(21, newest + 1)} <= minors, VERSIONS


def _env() -> dict[str, str]:
    return {
        **os.environ,
        "PYTHONPATH": str(Path(__file__).resolve().parents[1]),
        "BARCA_OUTPUT": "json",
    }


def _barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        check=False,
        cwd=cwd,
        env=_env(),
        text=True,
        capture_output=True,
        timeout=120,
    )


def _ok(result: subprocess.CompletedProcess) -> object:
    assert result.returncode == 0, f"exit {result.returncode}\n{result.stderr}"
    return json.loads(result.stdout)


def _project(tmp_path: Path, version: str) -> Path:
    project = tmp_path / "project"
    project.mkdir()
    shutil.copy(FIXTURES / "pipeline.py", project / "pipeline.py")
    with tarfile.open(FIXTURES / f"{version}.tar.gz") as tar:
        tar.extractall(project, filter="data")
    return project


def _format(project: Path) -> int:
    # The fixture as it was written: read before barca opens it.
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as db:
        return db.execute("PRAGMA user_version").fetchone()[0]


@pytest.mark.parametrize("version", VERSIONS)
def test_a_database_written_by_an_older_release_opens(tmp_path, version):
    project = _project(tmp_path, version)
    assert _format(project) == (0 if version in LEGACY else 1)

    # A read-only server first, before any writer has migrated the file: it reads snapshots.
    with Serve(project, "--read-only", env=_env()) as server:
        server.ready()
        health = server.get("/health")
        assert health["metadata_error"] is None, health
        runs = server.wait("GET /runs", lambda: server.get("/runs")["runs"])
        assert sorted(r["status"] for r in runs) == ["failed", "success", "success"], runs
        job_run = next(r["run_id"] for r in runs if r["target"] == "job")
        lines = server.get(f"/runs/{job_run}/logs")["logs"]
        assert {"line": "job stdout v1", "stream": "stdout"}.items() <= next(
            line for line in lines if line["line"] == "job stdout v1"
        ).items(), lines

    history = _ok(_barca(project, "history", "--json"))["runs"]
    assert sorted(r["status"] for r in history) == ["failed", "success", "success"], history
    assert any(r["status"] == "failed" and r["target"] == "broken" for r in history), history

    # Lines recorded before 0.29.2 have no stream: they are stdout, with no time.
    logs = _ok(_barca(project, "logs", job_run, "--json"))
    stdout = _ok(_barca(project, "logs", job_run, "--stream", "stdout", "--json"))
    assert "job stdout v1" in [line["line"] for line in logs], logs
    assert "job stdout v1" in [line["line"] for line in stdout], stdout
    assert all(line["stream"] == "stdout" for line in stdout), stdout

    status = _ok(_barca(project, "status", "pipeline.py", "--json"))
    assert status, status

    got = _ok(_barca(project, "get", "refreshed", "pipeline.py", "--json"))
    assert got["status"] == "success", got
    refreshed = next(s for s in got["steps"] if s["id"] == "pipeline.py:refreshed")
    assert refreshed["status"] == "cached", got

    # A writer server starts on the migrated file and answers /health.
    with Serve(project, "--no-schedule", env=_env()) as server:
        server.ready()
        health = server.get("/health")
        assert health["metadata_error"] is None, health
        assert health["status"] == "ok", health
