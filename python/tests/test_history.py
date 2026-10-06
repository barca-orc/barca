"""Run history and stats tests.

Tests that:
- get() creates a run record visible in history()
- history() returns recent runs with correct fields
- Multiple runs appear in history
- stats() returns timing data after runs
- Failed runs appear in history with status="failed"
"""

import textwrap
from pathlib import Path

import pytest

import barca
from barca.api import BarcaError


@pytest.fixture(autouse=True)
def clean_barca_dir():
    """Remove entire .barca/ directory before and after each test for isolation."""
    import shutil

    barca_dir = Path(".barca")
    if barca_dir.exists():
        shutil.rmtree(barca_dir)
    yield
    if barca_dir.exists():
        shutil.rmtree(barca_dir)


def write_module(tmp_path, filename, code):
    p = tmp_path / filename
    p.write_text(textwrap.dedent(code))
    return str(p)


class TestHistory:
    def test_get_creates_run_record(self, tmp_path):
        f = write_module(
            tmp_path,
            "trivial.py",
            """
            from barca import asset

            @asset()
            def hello():
                return {"msg": "hello"}
        """,
        )
        result = barca.api._exec(["get", f])
        assert "run_id" in result

        runs = barca.history()
        assert len(runs) >= 1
        latest = runs[0]
        assert latest["run_id"] == result["run_id"]
        assert latest["command"] == "get"
        assert latest["status"] == "success"

    def test_history_returns_correct_fields(self, tmp_path):
        f = write_module(
            tmp_path,
            "trivial.py",
            """
            from barca import asset

            @asset()
            def hello():
                return {"msg": "hello"}
        """,
        )
        barca.get(f)
        runs = barca.history()
        assert len(runs) >= 1
        r = runs[0]
        assert "run_id" in r
        assert "command" in r
        assert "status" in r
        assert "steps_executed" in r
        assert "elapsed_seconds" in r

    def test_second_get_shows_in_history(self, tmp_path):
        f = write_module(
            tmp_path,
            "trivial.py",
            """
            from barca import asset

            @asset()
            def hello():
                return {"msg": "hello"}
        """,
        )
        r1 = barca.api._exec(["get", f])
        r2 = barca.api._exec(["get", f])

        runs = barca.history()
        run_ids = [r["run_id"] for r in runs]
        assert r1["run_id"] in run_ids
        assert r2["run_id"] in run_ids
        assert len(runs) >= 2

    def test_failed_run_in_history(self, tmp_path):
        f = write_module(
            tmp_path,
            "failing.py",
            """
            from barca import asset

            @asset()
            def boom():
                raise ValueError("intentional failure")
        """,
        )
        with pytest.raises(BarcaError):
            barca.get(f)

        runs = barca.history()
        assert len(runs) >= 1
        assert runs[0]["status"] == "failed"


class TestStats:
    def test_stats_after_gets(self, tmp_path):
        f = write_module(
            tmp_path,
            "chain.py",
            """
            from barca import asset

            @asset()
            def a():
                return {"value": 1}

            @asset(inputs={"data": a})
            def b(data):
                return {"value": data["value"] + 10}
        """,
        )
        # Get twice so there's stats data.
        barca.get(f)
        barca.get(f, refresh_all=True)

        result = barca.stats("a", f)
        assert result["id"].endswith(":a")
        assert result["total_runs"] >= 2
