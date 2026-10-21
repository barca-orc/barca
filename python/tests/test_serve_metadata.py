"""`barca serve` and a metadata DB it cannot use (#513): a writer exits 3 before it binds, a
read-only server keeps serving and says why on `/health`, and the startup line names the DB."""

import json
import sqlite3
from pathlib import Path

import pytest

from .test_serve_robustness import Server, _free_port, barca

PIPELINE = """
from barca import asset, task, Schedule


@asset()
def quick() -> int:
    return 1


@task(freshness=Schedule("0 5 * * *"))
def nightly() -> None:
    print("nightly")
"""

REMEDIATION = "barca state restore"


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


def envelope(stderr: str) -> dict:
    return json.loads(next(line for line in reversed(stderr.splitlines()) if line.startswith("{")))


def broken(project: Path, how: str, env: str | None = None) -> Path:
    """A metadata DB barca cannot use, at the env's path."""
    base = project / ".barca" if env is None else project / ".barca" / "envs" / env
    base.mkdir(parents=True, exist_ok=True)
    db = base / "metadata.db"
    if how == "empty":
        db.write_bytes(b"")
    elif how == "junk":
        db.write_text("not a database at all, just some text that is long enough " * 4)
    elif how == "future":
        conn = sqlite3.connect(db)
        conn.execute("CREATE TABLE runs (id INTEGER PRIMARY KEY)")
        conn.execute("PRAGMA user_version = 99")
        conn.commit()
        conn.close()
    return db


def serve(project: Path, *args: str):
    return barca(
        project, "serve", "pipeline.py", "--port", str(_free_port()), *args, timeout=60
    )


@pytest.mark.parametrize("how", ["empty", "junk", "future"])
def test_a_writer_serve_on_a_database_it_cannot_use_exits_3_before_binding(project, how):
    db = broken(project, how)
    before = db.read_bytes()
    out = serve(project)
    assert out.returncode == 3, out.stderr
    assert "serving on" not in out.stderr, "nothing may bind"
    assert "scheduler disabled" not in out.stderr
    assert ".barca/metadata.db cannot be used" in out.stderr, out.stderr
    assert REMEDIATION in out.stderr
    assert "Parse error" not in out.stderr
    # The startup line still names the database, so the log says which file it was.
    assert "metadata .barca/metadata.db (unusable)" in out.stderr
    if how == "future":
        # Opening it may switch the journal mode in its header; its contents are untouched.
        conn = sqlite3.connect(db)
        assert conn.execute("PRAGMA user_version").fetchone()[0] == 99
        conn.close()
    else:
        assert db.read_bytes() == before, "the database is left as it was"


def test_the_empty_file_is_named_as_empty(project):
    broken(project, "empty")
    out = serve(project)
    assert out.returncode == 3
    assert "the metadata DB .barca/metadata.db cannot be used: it is empty (0 bytes)" in out.stderr


def test_a_future_format_says_so(project):
    broken(project, "future")
    out = serve(project)
    assert out.returncode == 3
    assert "metadata schema version 99 is not supported" in out.stderr, out.stderr
    assert "upgrade barca" in out.stderr


def test_the_startup_line_names_the_env_store_metadata_db_and_format(project):
    assert barca(project, "get", "quick", "pipeline.py").returncode == 0
    server = Server(project)
    try:
        log = server.log.read_text()
        line = (
            "[barca] env default (built-in default); artifact store local (.barca/artifacts); "
            "metadata .barca/metadata.db (db format 1)"
        )
        assert log.splitlines().count(line) == 1, log
        status, health = server.request("GET", "/health")
        assert status == 200
        assert health["scheduler_enabled"] is True
        assert health["metadata_error"] is None
    finally:
        server.stop()


def test_a_writer_with_no_database_yet_creates_none_at_startup(project):
    server = Server(project, "--no-schedule")
    try:
        assert "metadata .barca/metadata.db (not created yet)" in server.log.read_text()
    finally:
        server.stop()
    assert not (project / ".barca" / "metadata.db").exists()


def test_health_says_the_scheduler_is_off_with_no_schedule(project):
    server = Server(project, "--no-schedule")
    try:
        health = server.request("GET", "/health")[1]
        assert health["scheduler_enabled"] is False
        assert health["metadata_error"] is None
    finally:
        server.stop()


def test_a_read_only_server_keeps_serving_and_reports_the_error(project):
    db = broken(project, "junk")
    before = db.read_bytes()
    server = Server(project, "--read-only")
    try:
        log = server.log.read_text()
        assert "metadata .barca/metadata.db (unusable)" in log, log
        assert "[barca] warning: the metadata DB .barca/metadata.db cannot be used" in log
        status, health = server.request("GET", "/health")
        assert status == 200
        assert health["read_only"] is True
        assert health["scheduler_enabled"] is False
        assert ".barca/metadata.db cannot be used" in health["metadata_error"]

        # A route that reads history answers with the structured error, not a SQL error.
        status, body = server.request("GET", "/state")
        assert status == 500
        assert body["kind"] == "infra"
        assert body["code"] == 3
        assert body["reason"] == "metadata_unusable"
        assert ".barca/metadata.db" in body["error"]
        assert REMEDIATION in body["remediation"]
        assert "Parse error" not in body["error"]
    finally:
        server.stop()
    assert db.read_bytes() == before


def test_a_read_only_server_with_no_database_yet_says_so(project):
    server = Server(project, "--read-only")
    try:
        assert "metadata .barca/metadata.db (not created yet)" in server.log.read_text()
        assert server.request("GET", "/health")[1]["metadata_error"] is None
    finally:
        server.stop()
    assert not (project / ".barca" / "metadata.db").exists()


def test_a_stray_empty_root_database_beside_a_named_env_is_warned_about(project):
    broken(project, "empty")
    server = Server(project, "--env", "prod")
    try:
        log = server.log.read_text()
        assert "metadata .barca/envs/prod/metadata.db (not created yet)" in log, log
        warning = (
            "[barca] warning: .barca/metadata.db is not this server's metadata DB "
            "(env prod uses .barca/envs/prod/metadata.db) and cannot be used "
            "(it is empty (0 bytes))"
        )
        assert sum(line.startswith(warning) for line in log.splitlines()) == 1, log
        # The stray file does not stop the cache size check either.
        assert "cache size check failed" not in log
    finally:
        server.stop()


def test_a_valid_root_database_beside_a_named_env_is_not_warned_about(project):
    assert barca(project, "get", "quick", "pipeline.py").returncode == 0
    server = Server(project, "--env", "prod")
    try:
        assert "is not this server's metadata DB" not in server.log.read_text()
    finally:
        server.stop()


def test_get_on_an_empty_database_names_it(project):
    broken(project, "empty")
    out = barca(project, "get", "quick", "pipeline.py", "--json")
    assert out.returncode == 3, out.stderr
    error = envelope(out.stderr)
    assert error["reason"] == "metadata_unusable"
    assert error["error"].startswith("the metadata DB .barca/metadata.db cannot be used")
