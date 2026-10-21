"""`barca state reset` and `POST /state/reset` (#508): start the metadata database over.

The database and its -wal and -shm move aside to `metadata.db.reset-<UTC>` beside it and an
empty one takes its place; nothing is deleted. `barca state restore --from <file>` undoes it.
Refused while a run is in flight (exit 3, 409), under `barca serve` on the CLI (exit 3), and
on a `--read-only` server (403).
"""

import json
import os
import pty
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest
from barca.api import _find_binary

PIPELINE = """
from barca import asset, task


@asset()
def numbers() -> list:
    return [3, 4]


@task()
def slow() -> None:
    import time
    time.sleep(60)
"""


def env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}


def cli(root: Path, *args: str, **kw) -> subprocess.CompletedProcess:
    kw.setdefault("stdin", subprocess.DEVNULL)
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=env(),
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
        **kw,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def error(proc: subprocess.CompletedProcess, code: int) -> dict:
    assert proc.returncode == code, proc.stderr
    return json.loads(next(ln for ln in reversed(proc.stderr.splitlines()) if ln.startswith("{")))


def moved_aside(root: Path) -> list[Path]:
    return sorted((root / ".barca").glob("metadata.db.reset-*"))


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    return root


def test_reset_moves_the_database_aside_and_restore_puts_it_back(project):
    root = project
    ok(cli(root, "get", "numbers", "--json"))
    assert ok(cli(root, "history", "--json"))["total"] == 1
    proc = cli(root, "state", "reset", "--yes", "--json")
    reset = ok(proc)
    assert set(reset) == {"database", "moved_to", "moved", "undo"}
    assert reset["database"] == ".barca/metadata.db"
    moved = reset["moved_to"]
    assert Path(moved).name.startswith("metadata.db.reset-")
    assert (root / moved).is_file()
    assert reset["moved"][0] == moved
    assert reset["undo"] == f"barca state restore --from {moved}"
    assert f"the old one is {moved}" in proc.stderr and "Undo: barca state restore" in proc.stderr
    # The next command sees an empty history; the result is computed again.
    assert ok(cli(root, "history", "--json"))["total"] == 0
    assert ok(cli(root, "get", "numbers", "--json"))["steps_executed"] == 1
    # The moved file restores, by its path.
    restored = ok(cli(root, "state", "restore", "--from", moved, "--json"))
    assert restored["from"] == "file"
    assert restored["pre_restore"] == ".barca/metadata.db.pre-restore"
    assert ok(cli(root, "history", "--json"))["total"] == 1


def test_the_artifacts_a_reset_leaves_are_leftovers_gc_can_delete(project):
    """#552: the reset deletes nothing and says how much stays; the files it leaves are named by
    no record, so the cache size limit evicts them first and `gc` reports them."""
    root = project
    path = (
        root / ok(cli(root, "get", "numbers", "--json"))["final_output"]["_barca_artifact"]["path"]
    )
    assert path.is_file()
    proc = cli(root, "state", "reset", "--yes", "--json")
    moved = ok(proc)["moved_to"]
    assert path.is_file(), "a reset deletes nothing"
    assert "of artifacts under .barca/artifacts stay on disk" in proc.stderr, proc.stderr
    assert "barca gc --max-size off" in proc.stderr
    # Over a tiny limit: the unnamed file is the first leftover; the databases are held.
    plan = ok(cli(root, "gc", "--max-size", "1", "--dry-run", "--json"))
    rel = str(path.relative_to(root))
    assert {"tier": "leftovers", "path": rel, "bytes": path.stat().st_size} in plan["evicted"], plan
    held = {h["what"]: h for h in plan["held"]}
    assert held["db_copies"]["evictable"] is False
    assert held["metadata"]["evictable"] is False
    human = cli(root, "gc", "--max-size", "1", "--dry-run", "--pretty")
    assert "held by:" in human.stdout and "databases moved aside" in human.stdout, human.stdout
    done = ok(cli(root, "gc", "--max-size", "off", "--json"))
    assert rel in [e["path"] for e in done["evicted"]]
    assert not path.exists()
    # The moved-aside database is never deleted, and still restores: the result is computed
    # again when it is needed.
    assert (root / moved).is_file()
    restored = ok(cli(root, "state", "restore", "--from", moved, "--json"))
    assert restored["from"] == "file"
    assert ok(cli(root, "get", "numbers", "--json"))["steps_executed"] == 1
    assert path.is_file()


def test_reset_mends_a_database_that_cannot_be_opened(project):
    root = project
    (root / ".barca").mkdir()
    (root / ".barca" / "metadata.db").write_bytes(b"not a database at all")
    assert cli(root, "history", "--json").returncode != 0
    reset = ok(cli(root, "state", "reset", "--yes", "--json"))
    assert (root / reset["moved_to"]).read_bytes() == b"not a database at all"
    assert ok(cli(root, "history", "--json"))["total"] == 0
    # With no database at all, an empty one is created and nothing is moved.
    (root / ".barca" / "metadata.db").unlink()
    fresh = ok(cli(root, "state", "reset", "--yes", "--json"))
    assert (fresh["moved_to"], fresh["moved"], fresh["undo"]) == (None, [], None)


def test_without_yes_it_asks_in_a_terminal_and_refuses_elsewhere(project):
    root = project
    ok(cli(root, "get", "numbers", "--json"))
    refused = error(cli(root, "state", "reset", "--json"), 2)
    assert refused["kind"] == "usage" and "--yes" in refused["error"], refused
    assert moved_aside(root) == []

    def ask(answer: bytes) -> subprocess.CompletedProcess:
        main, child = pty.openpty()
        try:
            proc = subprocess.Popen(
                [_find_binary(), "state", "reset", "--pretty"],
                cwd=root,
                env=env(),
                stdin=child,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            os.close(child)
            os.write(main, answer)
            out, err = proc.communicate(timeout=60)
        finally:
            os.close(main)
        return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)

    no = ask(b"n\n")
    assert no.returncode == 0, no.stderr
    assert "Reset the metadata database? [y/N]" in no.stderr
    assert "nothing was changed" in no.stderr
    assert moved_aside(root) == []
    yes = ask(b"y\n")
    assert yes.returncode == 0, yes.stderr
    assert len(moved_aside(root)) >= 1
    assert ok(cli(root, "history", "--json"))["total"] == 0


def test_reset_is_refused_while_a_run_is_in_progress(project):
    root = project
    run = subprocess.Popen(
        [_find_binary(), "run", "slow", "--json"],
        cwd=root,
        env=env(),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        deadline = time.monotonic() + 60
        while True:
            history = cli(root, "history", "--json")
            if history.returncode == 0 and json.loads(history.stdout)["total"] == 1:
                break
            assert run.poll() is None
            assert time.monotonic() < deadline
            time.sleep(0.1)
        (running,) = json.loads(history.stdout)["runs"]
        refused = error(cli(root, "state", "reset", "--yes", "--json"), 3)
        assert refused["kind"] == "infra", refused
        assert running["run_id"] in refused["error"], refused
        assert "Wait for it to finish" in refused["remediation"], refused
        assert moved_aside(root) == []
    finally:
        os.killpg(run.pid, signal.SIGINT)
        run.wait(timeout=60)


def post(url: str) -> tuple[int, dict]:
    try:
        with urllib.request.urlopen(urllib.request.Request(url, method="POST"), timeout=30) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())


def serve(root: Path, *flags: str):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    log = root.parent / f"serve-{port}.log"
    with log.open("w") as out:
        server = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--no-schedule", "--port", str(port), *flags],
            cwd=root,
            env=env(),
            stdout=out,
            stderr=out,
        )
    deadline = time.monotonic() + 30
    while "serving on" not in log.read_text():
        assert server.poll() is None, log.read_text()
        assert time.monotonic() < deadline, log.read_text()
        time.sleep(0.05)
    return server, f"http://127.0.0.1:{port}"


def test_under_serve_the_cli_refuses_and_the_server_resets(project):
    root = project
    ok(cli(root, "get", "numbers", "--json"))
    server, url = serve(root)
    try:
        refused = error(cli(root, "state", "reset", "--yes", "--json"), 3)
        assert "barca serve" in refused["error"], refused
        # What to do instead is the remediation, not the generic infra "Retrying may succeed".
        assert "POST /state/reset" in refused["remediation"], refused
        assert "Retrying" not in refused["remediation"], refused
        assert moved_aside(root) == []
        status, body = post(f"{url}/state/reset")
        assert status == 200, body
        assert (root / body["moved_to"]).is_file()
        with urllib.request.urlopen(f"{url}/runs?limit=10", timeout=30) as r:
            assert json.loads(r.read())["runs"] == []
    finally:
        server.send_signal(signal.SIGTERM)
        server.wait(timeout=30)
    assert ok(cli(root, "history", "--json"))["total"] == 0


def test_a_read_only_server_refuses_a_reset(project):
    root = project
    ok(cli(root, "get", "numbers", "--json"))
    server, url = serve(root, "--read-only")
    try:
        status, body = post(f"{url}/state/reset")
        assert status == 403 and "read-only" in body["error"], body
    finally:
        server.send_signal(signal.SIGTERM)
        server.wait(timeout=30)
    assert moved_aside(root) == []
    assert ok(cli(root, "history", "--json"))["total"] == 1
