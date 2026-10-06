"""Concurrent barca processes in one project must queue on the metadata DB, not fail.

Turso takes an exclusive, non-blocking lock on the database file, so two processes that
overlap used to fail with "Failed locking file '.barca/metadata.db'. File is locked by another
process". Barca now holds a short cross-process lock around each DB operation instead.
"""

import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset


@asset()
def a() -> dict:
    return {"x": 1}


@asset(inputs={"a": a})
def b(a: dict) -> dict:
    return {"x": a["x"] + 1}
"""

PROCESSES = 12


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def run(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([_find_binary(), *args], cwd=cwd, capture_output=True, text=True)


def failures(results: list[subprocess.CompletedProcess]) -> list[str]:
    return [
        r.stderr.strip().splitlines()[-1] if r.stderr.strip() else "no stderr"
        for r in results
        if r.returncode != 0
    ]


def test_concurrent_gets_in_one_project_all_succeed(project):
    assert run(project, "get", "b", "pipeline.py").returncode == 0  # create the DB once
    with ThreadPoolExecutor(PROCESSES) as pool:
        results = list(
            pool.map(
                lambda _: run(project, "get", "b", "pipeline.py", "--refresh-all"), range(PROCESSES)
            )
        )
    assert not failures(results), (
        f"{len(failures(results))}/{PROCESSES} failed: {failures(results)[:3]}"
    )


def test_concurrent_first_runs_in_a_fresh_project_all_succeed(project):
    # No DB yet: every process races to create and initialize it.
    with ThreadPoolExecutor(PROCESSES) as pool:
        results = list(
            pool.map(lambda _: run(project, "get", "b", "pipeline.py"), range(PROCESSES))
        )
    assert not failures(results), (
        f"{len(failures(results))}/{PROCESSES} failed: {failures(results)[:3]}"
    )


def test_readers_and_writers_can_overlap(project):
    assert run(project, "get", "b", "pipeline.py").returncode == 0
    jobs = [("get", "b", "pipeline.py", "--refresh-all"), ("history",)] * (PROCESSES // 2)
    with ThreadPoolExecutor(PROCESSES) as pool:
        results = list(pool.map(lambda args: run(project, *args), jobs))
    assert not failures(results), (
        f"{len(failures(results))}/{len(jobs)} failed: {failures(results)[:3]}"
    )
