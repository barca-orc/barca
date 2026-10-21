"""A store without a server is this machine's backup and read-through cache (RFC-0009, #403).

- `barca status` with a store: `cached` only when this machine's copy, the store's object and
  the recorded digest agree; otherwise `unsynced` with `cache.reason` `local_only`,
  `store_only` or `mismatch`. `barca get` repairs each without running a step.
- The metadata DB is backed up when a run ends (and on the minute checkpoint), five copies in
  `.barca/backups/` and in the store; `barca state backups` lists them and
  `barca state restore` puts one back, only when asked.
"""

import json
import os
import signal
import socket
import subprocess
import time
from pathlib import Path

import pytest
from barca.api import _find_binary

from .store_window import age_store_checks

PIPELINE = """
from barca import asset


@asset()
def numbers() -> list:
    return [3, 4]


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"sum": sum(numbers)}
"""


def cli(root: Path, store: Path | None, *args: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    if store is not None:
        env["BARCA_REMOTE_URI"] = str(store)
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def error(proc: subprocess.CompletedProcess, code: int) -> dict:
    assert proc.returncode == code, proc.stderr
    return json.loads(next(ln for ln in reversed(proc.stderr.splitlines()) if ln.startswith("{")))


def cache(root: Path, store: Path) -> dict:
    doc = ok(cli(root, store, "status", "--json"))
    return {
        n["id"].rsplit(":", 1)[1]: (n["cache"]["state"], n["cache"]["reason"]) for n in doc["nodes"]
    }


def artifact(base: Path, node: str) -> Path:
    (path,) = [
        p
        for p in base.rglob("*.json")
        if f"--{node}" in p.parent.name or p.parent.name.endswith(node)
    ]
    return path


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    return root, tmp_path / "store"


def test_each_unsynced_reason_is_repaired_by_get_without_a_step(project):
    root, store = project
    assert ok(cli(root, store, "get", "total", "--json"))["steps_executed"] == 2
    assert cache(root, store) == {
        "numbers": ("cached", "materialized"),
        "total": ("cached", "materialized"),
    }
    local = artifact(root / ".barca" / "artifacts", "total")
    stored = artifact(store / "default" / "artifacts", "total")
    original = local.read_bytes()

    # local_only: the store lost its object. A get notices once the store check window (#593)
    # has passed.
    stored.unlink()
    age_store_checks(root)
    assert cache(root, store)["total"] == ("unsynced", "local_only")
    proc = cli(root, store, "get", "total", "-o", "value", "--json")
    doc = ok(proc)
    assert doc["steps_executed"] == 0 and doc["final_output"] == {"sum": 7}
    assert "pipeline.py:total: not in the artifact store; uploading" in proc.stderr, proc.stderr
    assert stored.read_bytes() == original
    assert cache(root, store)["total"] == ("cached", "materialized")

    # store_only: this machine lost its copy.
    local.unlink()
    assert cache(root, store)["total"] == ("unsynced", "store_only")
    table = cli(root, store, "status", "--pretty")
    assert "unsynced  store only" in table.stdout, table.stdout
    doc = ok(cli(root, store, "get", "total", "-o", "value", "--json"))
    assert doc["steps_executed"] == 0 and doc["final_output"] == {"sum": 7}
    assert local.read_bytes() == original
    assert cache(root, store)["total"] == ("cached", "materialized")

    # mismatch: the store's object has other bytes for the same code and inputs.
    stored.write_bytes(b'{"sum": 70000}')
    age_store_checks(root)
    assert cache(root, store)["total"] == ("unsynced", "mismatch")
    proc = cli(root, store, "get", "total", "-o", "value", "--json")
    doc = ok(proc)
    assert doc["steps_executed"] == 0 and doc["final_output"] == {"sum": 7}
    step = {s["id"].rsplit(":", 1)[1]: s for s in doc["steps"]}["total"]
    assert step["status"] == "cached" and step["bytes_changed"] is True, step
    warning = "pipeline.py:total produced different bytes for the same code and inputs"
    assert proc.stderr.count(warning) == 1, proc.stderr
    assert stored.read_bytes() == original
    assert cache(root, store)["total"] == ("cached", "materialized")


def test_neither_copy_is_stale_and_status_without_a_store_is_unchanged(project):
    root, store = project
    ok(cli(root, store, "get", "total", "--json"))
    artifact(root / ".barca" / "artifacts", "total").unlink()
    artifact(store / "default" / "artifacts", "total").unlink()
    assert cache(root, store)["total"] == ("stale", "artifact_missing")
    # BARCA_REMOTE=off: no store is asked; the local rule applies as before.
    env_off = {**os.environ, "BARCA_REMOTE": "off"}
    proc = subprocess.run(
        [_find_binary(), "status", "--json"], cwd=root, env=env_off, capture_output=True, text=True
    )
    assert proc.returncode == 0, proc.stderr
    assert all(n["cache"]["state"] != "unsynced" for n in json.loads(proc.stdout)["nodes"])


def test_backups_are_listed_and_restored_only_when_asked(project):
    root, store = project
    ok(cli(root, store, "get", "numbers", "--json"))
    time.sleep(1.1)  # backups are named by the second
    second = cli(root, store, "get", "total", "--json")
    ok(second)
    assert "backed up the metadata DB" in second.stderr, second.stderr

    listed = ok(cli(root, store, "state", "backups", "--json"))
    assert listed["local_dir"] == ".barca/backups"
    assert listed["store"] == f"{store}/default/state/backups"
    backups = listed["backups"]
    assert [b["index"] for b in backups] == [1, 2]
    assert [(b["where"], b["agree"]) for b in backups] == [("both", True), ("both", True)]
    newest, older = backups
    assert newest["name"] > older["name"]
    assert (newest["rows"], older["rows"]) == (4, 2)  # runs + materializations
    assert newest["run_id"] == json.loads(second.stdout)["run_id"]
    table = cli(root, store, "state", "backups", "--pretty")
    assert table.returncode == 0 and "WHERE" in table.stdout and older["name"] in table.stdout

    # By index: the older copy, with one run; the database it replaced is kept aside.
    proc = cli(root, store, "state", "restore", "--from", "2", "--json")
    restored = ok(proc)
    assert (restored["restored"], restored["from"]) == (older["name"], "local")
    assert restored["pre_restore"] == ".barca/metadata.db.pre-restore"
    assert f"restored {older['name']} from the local copy" in proc.stderr, proc.stderr
    assert (root / ".barca" / "metadata.db.pre-restore").is_file()
    assert ok(cli(root, store, "history", "--json"))["total"] == 1
    # By name, without `.db`; then the newest by default.
    ok(cli(root, store, "state", "restore", "--from", older["name"][: -len(".db")], "--json"))
    restored = ok(cli(root, store, "state", "restore", "--json"))
    assert restored["restored"] == newest["name"]
    assert ok(cli(root, store, "history", "--json"))["total"] == 2

    # Refusals: an unknown backup, and a copy that is not a database (naming the next one).
    assert (
        error(cli(root, store, "state", "restore", "--from", "9", "--json"), 2)["kind"] == "usage"
    )
    for copy in (
        root / ".barca" / "backups" / newest["name"],
        store / "default/state/backups" / newest["name"],
    ):
        copy.write_bytes(b"garbage")
    before = (root / ".barca" / "metadata.db").read_bytes()
    refused = error(cli(root, store, "state", "restore", "--json"), 2)
    assert "--from 2" in refused["error"], refused
    assert (root / ".barca" / "metadata.db").read_bytes() == before


def test_with_no_backup_restore_is_a_usage_error(project):
    root, store = project
    assert error(cli(root, store, "state", "restore", "--json"), 2)["kind"] == "usage"
    # Without a store there are no backups at all, and the listing says where it looked.
    listed = ok(cli(root, None, "state", "backups", "--json"))
    assert listed == {"backups": [], "local_dir": ".barca/backups", "store": None}
    assert not (root / ".barca").exists()


def test_restore_is_refused_while_serve_runs(project):
    root, store = project
    ok(cli(root, store, "get", "total", "--json"))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["BARCA_REMOTE_URI"] = str(store)
    log = root.parent / "serve.log"
    with log.open("w") as out:
        server = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--no-schedule", "--port", str(port)],
            cwd=root,
            env=env,
            stdout=out,
            stderr=out,
        )
    try:
        deadline = time.monotonic() + 30
        while "serving on" not in log.read_text():
            assert server.poll() is None, log.read_text()
            assert time.monotonic() < deadline, log.read_text()
            time.sleep(0.05)
        refused = error(cli(root, store, "state", "restore", "--json"), 3)
        assert refused["kind"] == "infra" and "barca serve" in refused["error"], refused
        assert not (root / ".barca" / "metadata.db.pre-restore").exists()
    finally:
        server.send_signal(signal.SIGTERM)
        server.wait(timeout=30)
    # Once it has stopped, the restore goes ahead.
    assert ok(cli(root, store, "state", "restore", "--json"))["from"] == "local"


def test_two_project_copies_on_one_store_are_told_apart(tmp_path):
    """#553: two copies of a project with one store. Each backup names the host and project
    directory that took it; once both write, each is warned; pruning keeps the other's newest;
    restore without --from takes this copy's own newest."""
    store = tmp_path / "store"
    a, b = tmp_path / "a", tmp_path / "b"
    for root in (a, b):
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
    ok(cli(a, store, "get", "numbers", "--json"))
    (first_a,) = ok(cli(a, store, "state", "backups", "--json"))["backups"]
    assert first_a["mine"] is True
    assert first_a["project"] == str(a.resolve())
    assert first_a["host"] == socket.gethostname()
    # B takes over A's history: no backup of its own yet, so no warning.
    proc = cli(b, store, "state", "restore", "--json")
    assert ok(proc)["project"] == str(a.resolve())
    assert "restoring the newest backup" in proc.stderr and f":{a.resolve()} took" in proc.stderr
    proc = cli(b, store, "get", "total", "--json")
    ok(proc)
    assert "warning" not in proc.stderr, proc.stderr
    # A runs again: B took a backup after A's own.
    proc = cli(a, store, "get", "total", "--json")
    ok(proc)
    assert "also holds backups from" in proc.stderr and str(b.resolve()) in proc.stderr
    assert "barca docs server-mode" in proc.stderr
    assert proc.stderr.count("also holds backups from") == 1
    # B runs often, each run recording a new result (a run that records none takes no
    # backup, #554): A's newest backup is still in the store.
    for i in range(6):
        changed = PIPELINE.replace('{"sum": sum(numbers)}', f'{{"sum": sum(numbers), "run": {i}}}')
        assert changed != PIPELINE
        (b / "pipeline.py").write_text(changed)
        ok(cli(b, store, "get", "total", "--json"))
    proc = cli(a, store, "state", "backups", "--json")
    listed = ok(proc)["backups"]
    mine = [x for x in listed if x["mine"]]
    assert mine and mine[0]["index"] > 1, listed
    assert mine[0]["where"] == "both", "the store kept A's newest backup"
    assert "also holds backups from" in proc.stderr
    human = cli(a, store, "state", "backups", "--pretty")
    assert "HOST" in human.stdout and "(this machine)" in human.stdout, human.stdout
    # Restore without --from takes A's own newest, and says why it is not the newest.
    proc = cli(a, store, "state", "restore", "--json")
    restored = ok(proc)
    assert restored["restored"] == mine[0]["name"]
    assert restored["project"] == str(a.resolve())
    assert "restoring this machine's newest backup" in proc.stderr
    assert "--from 1 restores the newest" in proc.stderr
