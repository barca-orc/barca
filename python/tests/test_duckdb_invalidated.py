"""A worker whose duckdb database is invalidated stops, and its step is tried once more in a
fresh worker (#1083).

duckdb invalidates a database after a fatal or internal error, and every later statement on it
fails with `FatalException`. A reused worker kept taking steps and each failed the same way:
one fault became 1,462 failed steps in one run.

These tests raise duckdb's own `FatalException` from a step. They do not invalidate a real
database: nothing in duckdb's public surface does that on demand (a failed WAL write, a failed
checkpoint and a misbehaving Arrow UDF all leave the default database usable on duckdb 2.0).
"""

import os

import pytest

from .helpers import barca, write_module
from .test_worker_survives_exceptions import TRACE

duckdb = pytest.importorskip("duckdb")


def run(tmp_path, source, target):
    write_module(tmp_path, "pipeline.py", source, strip=True)
    proc = barca(
        tmp_path,
        "get",
        target,
        "pipeline.py",
        "--json",
        env={
            **os.environ,
            "BARCA_REMOTE": "off",
            "BARCA_POOL_SIZE": "1",
            "BARCA_TRACE_TIMING": "1",
            "ATTEMPTS": str(tmp_path / "attempts"),
        },
    )
    found = TRACE.findall(proc.stderr)
    assert found, proc.stderr
    started, kept, _ = (int(n) for n in found[-1])
    attempts = (tmp_path / "attempts").read_text().split()
    return proc, started, kept, attempts


def test_the_step_is_tried_again_in_a_fresh_worker_and_the_run_succeeds(tmp_path):
    proc, started, kept, attempts = run(
        tmp_path,
        """
        import os
        import duckdb
        from barca import asset

        @asset
        def broken() -> int:
            with open(os.environ["ATTEMPTS"], "a") as f:
                f.write(f"{os.getpid()}\\n")
            if len(open(os.environ["ATTEMPTS"]).read().split()) == 1:
                raise duckdb.FatalException(
                    "database has been invalidated because of a previous fatal error"
                )
            return 1

        @asset(inputs={"broken": broken})
        def after(broken: int) -> int:
            return broken + 1
        """,
        "after",
    )
    assert proc.returncode == 0, proc.stderr
    # Two attempts, in two processes: the first worker was stopped, not reused.
    assert len(attempts) == 2 and attempts[0] != attempts[1], attempts
    assert (started, kept) == (2, 0), proc.stderr
    assert "tried once more in a fresh one" in proc.stderr


def test_a_step_that_invalidates_every_worker_fails_after_one_more_attempt(tmp_path):
    proc, started, _, attempts = run(
        tmp_path,
        """
        import os
        import duckdb
        from barca import asset

        @asset
        def broken() -> int:
            with open(os.environ["ATTEMPTS"], "a") as f:
                f.write(f"{os.getpid()}\\n")
            raise duckdb.FatalException("database has been invalidated")

        @asset
        def beside() -> int:
            with open(os.environ["ATTEMPTS"] + ".beside", "a") as f:
                f.write("ran\\n")
            return 1

        @asset(inputs={"broken": broken, "beside": beside})
        def both(broken: int, beside: int) -> int:
            return broken + beside
        """,
        "both",
    )
    assert proc.returncode == 1, proc.stderr
    assert len(attempts) == 2 and attempts[0] != attempts[1], attempts
    assert "FatalException" in proc.stderr
    # The step beside it ran once, on a worker that was not the invalidated one.
    assert (tmp_path / "attempts.beside").read_text().split() == ["ran"]


def test_an_error_inside_a_handler_is_found_through_the_chain():
    from barca import _duckdb

    try:
        try:
            raise duckdb.FatalException("database has been invalidated")
        except duckdb.Error:
            raise RuntimeError("could not write the result") from None
    except RuntimeError as exc:
        assert _duckdb.invalidated(exc)


def test_an_ordinary_duckdb_error_leaves_the_worker_alone():
    from barca import _duckdb

    with pytest.raises(duckdb.Error) as caught:
        duckdb.sql("select * from no_such_table")
    assert not _duckdb.invalidated(caught.value)
    assert not _duckdb.invalidated(ValueError("not duckdb's"))
