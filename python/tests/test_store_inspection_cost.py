"""Repeated status inspection must bound reads of unchanged store artifact payloads."""

import json
import os
import sqlite3
import subprocess
import sys

import pytest
from barca.api import _find_binary

from .test_serve_robustness import Server, _env, wait_for


@pytest.fixture
def inspected_project(tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    store = tmp_path / "store"
    audit = tmp_path / "audit"
    audit.mkdir()
    reads = tmp_path / "store-reads.jsonl"
    (project / "barca.toml").write_text(f"[remote]\nuri = {json.dumps(str(store))}\n")
    (project / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 42\n"
    )
    # Observe real helper payload opens, rather than asserting a wall-clock threshold.
    (audit / "sitecustomize.py").write_text(
        "import sys, os, json\n"
        f"prefix = {str(store) + os.sep!r}\n"
        f"log = {str(reads)!r}\n"
        "def observe(event, args):\n"
        "    if event != 'open' or not isinstance(args[0], str): return\n"
        "    path = args[0]\n"
        "    if path.startswith(prefix) and '/artifacts/' in path and path.endswith('.json'):\n"
        "        with open(log, 'a') as out: out.write(json.dumps(path) + '\\n')\n"
        "sys.addaudithook(observe)\n"
    )
    env = _env() | {"PYTHONPATH": str(audit) + os.pathsep + os.environ.get("PYTHONPATH", "")}

    def command(*args):
        result = subprocess.run(
            [_find_binary(), *args, "--json"],
            cwd=project,
            env=env,
            text=True,
            capture_output=True,
            timeout=20,
        )
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    command("get", "value", "pipeline.py")
    artifact = next((store / "default/artifacts").rglob("*.json"))
    control = subprocess.run(
        [sys.executable, "-c", "import sys; open(sys.argv[1], 'rb').read()", str(artifact)],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        timeout=5,
    )
    assert control.returncode == 0, control.stderr
    assert reads.read_text().splitlines(), "the payload-read audit hook must be active"
    reads.write_text("")
    monkeypatch.setenv("PYTHONPATH", env["PYTHONPATH"])
    return project, artifact, reads, command


def test_warm_status_does_not_rehash_an_unchanged_store_copy(inspected_project):
    _, artifact, reads, command = inspected_project
    first = command("status", "pipeline.py")
    first_reads = reads.read_text().splitlines()
    assert first["nodes"][0]["cache"]["state"] == "cached", first
    second = command("status", "pipeline.py")
    assert second["nodes"][0]["cache"]["state"] == "cached", second
    repeated_reads = reads.read_text().splitlines()[len(first_reads) :]
    assert not repeated_reads, f"unchanged warm status reopened store payloads: {repeated_reads}"

    # Same-sized store edits must invalidate digest evidence immediately, including when
    # the old modification time is restored; ctime/inode are part of the fingerprint.
    before = artifact.stat()
    artifact.write_text("43")
    os.utime(artifact, ns=(before.st_atime_ns, before.st_mtime_ns))
    changed = command("status", "pipeline.py")
    assert changed["nodes"][0]["cache"]["state"] == "unsynced", changed
    assert changed["nodes"][0]["cache"]["reason"] == "mismatch", changed
    artifact.unlink()
    missing = command("status", "pipeline.py")
    assert missing["nodes"][0]["cache"]["reason"] == "local_only", missing


def test_server_db_refresh_reuses_artifact_evidence_from_the_stable_environment(inspected_project):
    project, _, reads, command = inspected_project
    command("status", "pipeline.py")
    reads.write_text("")
    server = Server(project, "--read-only", "--no-schedule")
    try:
        assert server.request("GET", "/state")[1][0]["cache"]["state"] == "cached"
        for elapsed in (17.25, 23.5):
            with sqlite3.connect(project / ".barca/metadata.db") as database:
                database.execute("UPDATE materializations SET elapsed_seconds = ?", (elapsed,))

            def updated():
                status, rows = server.request("GET", "/state")
                assert status == 200
                return rows[0]["last_materialization"]["elapsed_seconds"] == elapsed

            wait_for(updated, "new metadata reflected in state")
        assert not reads.read_text().splitlines(), (
            "DB invalidation reread unchanged artifact payloads"
        )
    finally:
        server.stop()
