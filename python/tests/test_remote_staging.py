"""Staging files for remote artifacts live as long as something reads them, and no longer.

A remote input is downloaded to `.barca/staging/{pid}/` before it is loaded. Two things used to
remove that file too early: a duckdb-typed input is lazy and was read after its file was deleted,
and a worker starting up swept the temp files of every other worker.
"""

import os
import subprocess
import sys
import textwrap
import time
from pathlib import Path

import pytest

from barca import _artifacts, _runtime, _storage, _worker


@pytest.fixture
def project(tmp_path, monkeypatch):
    """Run in an empty project directory with a clean memory:// store."""
    monkeypatch.chdir(tmp_path)
    yield tmp_path
    _artifacts.release_fetched()
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


def _staged_files() -> list[Path]:
    root = Path(_artifacts._STAGING_DIR)
    return [p for p in root.rglob("*") if p.is_file()] if root.is_dir() else []


def _dead_pid() -> int:
    proc = subprocess.Popen([sys.executable, "-c", "pass"])
    proc.wait()
    return proc.pid


# ─── duckdb inputs ────────────────────────────────────────────────────────────


def _run_step(project, monkeypatch, body, function_name):
    """Run one daemon-mode step whose `orders` input is a remote parquet artifact."""
    duckdb = pytest.importorskip("duckdb")
    duckdb.sql("select * from (values (1, 'a'), (2, 'b')) t(k, v)").write_parquet("src.parquet")
    _storage.put_file("src.parquet", "memory://arts/orders/h1.parquet")

    source = project / "mod.py"
    source.write_text(textwrap.dedent(body))
    errors = []
    monkeypatch.setattr(_runtime, "emit_step_error", lambda **kw: errors.append(kw))
    step = {
        "node_id": f"mod.py:{function_name}",
        "function_name": function_name,
        "source_file": str(source),
        "kind": "task",
        "inputs": {"orders": "memory://arts/orders/h1.parquet"},
        "param_types": {"orders": "duckdb"},
        "run_hash": "h2",
    }
    ok = _worker._run_daemon_step(step, {}, "memory://arts", _worker._ArtifactLRU())
    assert errors == []
    assert ok
    return step


def test_duckdb_input_reads_a_remote_artifact(project, monkeypatch):
    step = _run_step(
        project,
        monkeypatch,
        """
        import duckdb

        def validate_orders(orders: duckdb.DuckDBPyRelation):
            return orders.order("k").fetchall()
        """,
        "validate_orders",
    )
    out = _artifacts.artifact_path("memory://arts", step["node_id"], "json", "h2")
    assert _artifacts.deserialize(out, "json") == [[1, "a"], [2, "b"]]


def test_relation_returned_from_a_remote_input_is_materialized(project, monkeypatch):
    """The result is still lazy when the function returns: the fetched file must outlive it."""
    step = _run_step(
        project,
        monkeypatch,
        """
        import duckdb

        def big_orders(orders: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
            return orders.filter("k > 1")
        """,
        "big_orders",
    )
    out = _artifacts.artifact_path("memory://arts", step["node_id"], "parquet", "h2")
    assert _artifacts.deserialize(out, "parquet").to_dict("records") == [{"k": 2, "v": "b"}]


def test_fetched_files_are_removed_when_the_step_ends(project, monkeypatch):
    _run_step(
        project,
        monkeypatch,
        """
        import duckdb

        def validate_orders(orders: duckdb.DuckDBPyRelation):
            return orders.count("*").fetchone()[0]
        """,
        "validate_orders",
    )
    assert _staged_files() == []


def test_eager_remote_read_leaves_nothing_staged(project):
    _artifacts.serialize({"x": 1}, "memory://arts/n/h.json", "json")
    assert _artifacts.deserialize("memory://arts/n/h.json", "json") == {"x": 1}
    assert _staged_files() == []


# ─── cleanup at worker startup ────────────────────────────────────────────────


def test_staging_is_per_process(project):
    assert _artifacts._staging_dir() == Path(".barca/staging") / str(os.getpid())


def test_clean_staging_keeps_live_workers_files_and_removes_dead_ones(project):
    root = Path(".barca/staging")
    live = root / str(os.getppid()) / "fetch-inflight.tmp"
    dead = root / str(_dead_pid()) / "stage-orphan.tmp"
    for f in (live, dead):
        f.parent.mkdir(parents=True)
        f.write_bytes(b"x")

    _artifacts.clean_staging()

    assert live.exists()
    assert not dead.parent.exists()


def test_clean_staging_removes_only_old_loose_temp_files(project):
    """Loose files come from versions that shared one directory; a recent one may be in use."""
    root = Path(".barca/staging")
    root.mkdir(parents=True)
    recent, old = root / "fetch-recent.tmp", root / "fetch-old.tmp"
    for f in (recent, old):
        f.write_bytes(b"x")
    two_hours_ago = time.time() - 7200
    os.utime(old, (two_hours_ago, two_hours_ago))

    _artifacts.clean_staging()

    assert recent.exists()
    assert not old.exists()
