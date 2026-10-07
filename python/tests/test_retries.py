"""End-to-end retry / error-tracking / anti-pileup tests (issue #51).

These exercise the full Rust-owned retry loop and the "continue past independent
failures" worker behavior, asserting *observable* facts:

  - **Flaky counter**: an asset increments a temp-file counter and raises while
    `n < FAILS`, so it is deterministically flaky across attempts.
  - **Process-count probe**: the fixture module appends `os.getpid()` to
    ``$BARCA_PROC_LOG`` at import time. Each worker process imports the user
    module exactly once, so the number of *distinct PIDs* in that file equals the
    number of worker processes Rust spawned. Workers inherit the test's env, so we
    just set the var before invoking barca.
"""

import sqlite3
import textwrap
from pathlib import Path

import barca
import pytest
from barca.api import BarcaError


@pytest.fixture(autouse=True)
def clean_barca_dir(tmp_path, monkeypatch):
    # The API resolves .barca relative to cwd. Give each test its own project,
    # so another pytest worker cannot delete its database or input artifacts.
    monkeypatch.chdir(tmp_path)


@pytest.fixture
def state(tmp_path, monkeypatch):
    """Per-test state dir: a counter base + a process-id log, wired via env vars."""
    proc_log = tmp_path / "pids.log"
    counter_dir = tmp_path / "counters"
    counter_dir.mkdir()
    monkeypatch.setenv("BARCA_PROC_LOG", str(proc_log))
    monkeypatch.setenv("BARCA_TEST_STATE", str(counter_dir))
    return {"proc_log": proc_log, "counter_dir": counter_dir}


def write_module(tmp_path, filename, body):
    """Write the shared PREAMBLE + a per-test asset block. Each piece is dedented
    separately so they concatenate into valid column-0 module source."""
    code = textwrap.dedent(PREAMBLE) + textwrap.dedent(body)
    p = tmp_path / filename
    p.write_text(code)
    return str(p)


# Shared fixture-module preamble: records this worker process's PID once at import,
# and provides a deterministic flaky helper backed by a temp-file counter.
PREAMBLE = """
    import os
    from pathlib import Path
    from barca import asset

    # Record this worker process's PID exactly once (module import == one process).
    _log = os.environ.get("BARCA_PROC_LOG")
    if _log:
        with open(_log, "a") as fh:
            fh.write(f"{os.getpid()}\\n")

    _state = Path(os.environ["BARCA_TEST_STATE"])

    def _attempt(name):
        p = _state / name
        n = int(p.read_text()) if p.exists() else 0
        p.write_text(str(n + 1))
        return n
"""


def distinct_pids(proc_log):
    if not proc_log.exists():
        return 0
    return len({line for line in proc_log.read_text().splitlines() if line.strip()})


def query_db(file_path, fn):
    """Return (status, attempts, error_message) for the latest row of an asset.

    Matches by node_id suffix (`<file>:<fn>`) since the stored node_id carries
    the full source path.
    """
    suffix = f"%{Path(file_path).name}:{fn}"
    db = Path(".barca") / "metadata.db"
    conn = sqlite3.connect(str(db))
    try:
        row = conn.execute(
            "SELECT status, attempts, error_message FROM materializations "
            "WHERE node_id LIKE ? ORDER BY id DESC LIMIT 1",
            (suffix,),
        ).fetchone()
    finally:
        conn.close()
    return row


# ─── 1. Retry succeeds ────────────────────────────────────────────────────────


def test_retry_succeeds_after_transient_failures(tmp_path, state):
    f = write_module(
        tmp_path,
        "retry_ok.py",
        """
        @asset(retries=3, retry_backoff=0.05)
        def flaky():
            n = _attempt("flaky")
            if n < 2:
                raise RuntimeError(f"boom {n}")
            return {"n": n}
        """,
    )
    result = barca.get(f)
    assert result == {"n": 2}

    # Flaky fn ran 3 times (2 failures + 1 success).
    assert int((state["counter_dir"] / "flaky").read_text()) == 3
    # 1 initial dispatch + 2 retries = 3 worker processes.
    assert distinct_pids(state["proc_log"]) == 3

    status, attempts, _ = query_db(f, "flaky")
    assert status == "success"
    assert attempts == 3


# ─── 2. Retry exhausted ───────────────────────────────────────────────────────


def test_retry_exhausted_records_failure(tmp_path, state):
    f = write_module(
        tmp_path,
        "retry_exhausted.py",
        """
        @asset(retries=2, retry_backoff=0.05)
        def always_fails():
            _attempt("always_fails")
            raise ValueError("nope")
        """,
    )
    with pytest.raises(BarcaError) as exc:
        barca.get(f)
    msg = str(exc.value)
    assert "ValueError" in msg
    assert "_worker.py" not in msg  # internal frames filtered

    # 1 initial + 1 retry = 2 processes; the function ran twice.
    assert int((state["counter_dir"] / "always_fails").read_text()) == 2
    assert distinct_pids(state["proc_log"]) == 2

    status, attempts, error_message = query_db(f, "always_fails")
    assert status == "failed"
    assert attempts == 2
    assert error_message and "nope" in error_message


# ─── 3. In-process anti-pileup / no per-chain spawn ──────────────────────────


def test_independent_chain_completes_despite_sibling_failure(tmp_path, state):
    """Two independent chains; chain A's head fails (no retry). Chain B must still
    materialize — and in the SAME worker process (no per-chain spawn)."""
    f = write_module(
        tmp_path,
        "pileup.py",
        """
        @asset()
        def a_head():
            raise RuntimeError("A is poison")

        @asset(inputs={"x": a_head})
        def a_tail(x):
            return {"x": x}

        @asset()
        def b_head():
            return {"b": 1}

        @asset(inputs={"x": b_head})
        def b_tail(x):
            return {"b_tail": x["b"] + 1}
        """,
    )
    with pytest.raises(BarcaError):
        # The run fails overall (A is poison) but B's results are persisted.
        barca.get(f)

    # Healthy chain B fully materialized.
    assert query_db(f, "b_head")[0] == "success"
    assert query_db(f, "b_tail")[0] == "success"
    # Poison chain A recorded failed; its dependent never materialized.
    assert query_db(f, "a_head")[0] == "failed"
    assert query_db(f, "a_tail") is None

    # No per-chain spawn: everything ran in the initial worker(s); a single failing
    # asset with retries=1 triggers no extra process. With 4 tiny assets the planner
    # packs them into one phase → at most a handful of streams, never one-per-chain.
    assert distinct_pids(state["proc_log"]) <= 2


# ─── 4. Backoff is real but does not block independent work ──────────────────


def test_backoff_is_applied(tmp_path, state):
    import time

    f = write_module(
        tmp_path,
        "backoff.py",
        """
        @asset(retries=3, retry_backoff=0.2)
        def slow_flaky():
            n = _attempt("slow_flaky")
            if n < 2:
                raise RuntimeError("retry me")
            return {"ok": True}
        """,
    )
    t0 = time.perf_counter()
    result = barca.get(f)
    elapsed = time.perf_counter() - t0

    assert result == {"ok": True}
    # Two backoffs of 0.2*1 and 0.2*2 = 0.6s minimum (loose lower bound only).
    assert elapsed >= 0.55, f"expected >= ~0.6s of backoff, got {elapsed:.3f}s"


# ─── 5. Default behavior: no retry ───────────────────────────────────────────


def test_default_is_single_attempt(tmp_path, state):
    f = write_module(
        tmp_path,
        "default.py",
        """
        @asset()
        def one_shot():
            _attempt("one_shot")
            raise RuntimeError("fail once")
        """,
    )
    with pytest.raises(BarcaError):
        barca.get(f)

    assert int((state["counter_dir"] / "one_shot").read_text()) == 1
    assert distinct_pids(state["proc_log"]) == 1
    status, attempts, _ = query_db(f, "one_shot")
    assert status == "failed"
    assert attempts == 1
