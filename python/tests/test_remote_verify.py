"""A local copy of a stored result is checked against the hash recorded when it was uploaded.

Two working directories share one plain-directory store, so this needs no emulator: the checks
are the same for every backend, since they hash the local file.
"""

import json
import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest

from .test_remote_inspect import SCRUB, _find_binary

PIPELINE = """
from barca import asset


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"numbers": numbers})
def total(numbers: list) -> dict:
    return {"sum": sum(numbers)}
"""


def cli(cwd: Path, store: Path, *args: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**env, "BARCA_REMOTE_URI": str(store)},
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


@pytest.fixture
def shared(tmp_path):
    """(store, machine factory); `numbers` and `total` are already in the store."""
    store = tmp_path / "store"

    def make(name: str) -> Path:
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        return root

    first = cli(make("producer"), store, "get", "total", "--json")
    assert first.returncode == 0, first.stderr
    return store, make


def _one(root: Path, pattern: str) -> Path:
    (path,) = root.glob(pattern)
    return path


def test_the_uploaded_hash_is_recorded_with_the_row(shared, tmp_path):
    store, _ = shared
    import hashlib
    import sqlite3

    artifact = _one(store, "default/artifacts/*numbers*/*.json")
    db = sqlite3.connect(tmp_path / "producer" / ".barca" / "metadata.db")
    (recorded,) = db.execute(
        "select output_hash from materializations where node_id like '%:numbers'"
    ).fetchone()
    assert recorded == hashlib.sha256(artifact.read_bytes()).hexdigest()


def test_a_changed_local_copy_is_replaced_before_a_step_reads_it(shared):
    store, make = shared
    root = make("reader")
    assert cli(root, store, "get", "total", "--json").returncode == 0
    # A new step makes this machine read `numbers`, whose copy it now holds is wrong.
    assert cli(root, store, "get", "numbers", "--json").returncode == 0
    _one(root, ".barca/artifacts/*numbers*/*.json").write_text("[9, 9, 9]")
    (root / "pipeline.py").write_text(
        PIPELINE + '\n\n@asset(inputs={"numbers": numbers})\ndef doubled(numbers: list) -> list:\n'
        "    return [n * 2 for n in numbers]\n"
    )

    proc = cli(root, store, "get", "doubled", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == [2, 4, 6]
    assert "fetched 1 cached artifact" in proc.stderr, proc.stderr
    assert _one(root, ".barca/artifacts/*numbers*/*.json").read_text() == "[1, 2, 3]"


def test_an_intact_local_copy_is_not_fetched_again(shared):
    store, make = shared
    root = make("reader")
    assert cli(root, store, "get", "total", "--json").returncode == 0
    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    assert "fetched" not in again.stderr, again.stderr


def test_a_store_copy_that_differs_from_the_recorded_hash_is_used_with_a_warning(shared):
    # An artifact path is {node}/{run_hash}, so the object can be overwritten legitimately.
    store, make = shared
    _one(store, "default/artifacts/*total*/*.json").write_text('{"sum": 7}')
    root = make("reader")
    proc = cli(root, store, "get", "total", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == {"sum": 7}
    assert "warning" in proc.stderr and "total" in proc.stderr, proc.stderr
    assert "--refresh pipeline.py:total" in proc.stderr, proc.stderr
    # The same finding is in stdout, under a stable marker, for callers that parse it.
    steps = {st["id"]: st for st in json.loads(proc.stdout)["steps"]}
    assert steps["pipeline.py:total"]["artifact_mismatch"] is True
    assert "--refresh pipeline.py:total" in steps["pipeline.py:total"]["warning"]
    assert "artifact_mismatch" not in steps.get("pipeline.py:numbers", {})

    again = cli(root, store, "get", "total", "--json")
    assert again.returncode == 0, again.stderr
    assert "fetched" not in again.stderr, again.stderr


def test_a_step_that_read_a_differing_input_reports_it_too(shared):
    store, make = shared
    _one(store, "default/artifacts/*numbers*/*.json").write_text("[5, 5]")
    root = make("reader")
    # `total` is recomputed, so it reads the store's (differing) copy of `numbers`.
    proc = cli(root, store, "get", "total", "--refresh", "total", "--json")
    assert proc.returncode == 0, proc.stderr
    out = json.loads(proc.stdout)
    assert out["final_output"] == {"sum": 10}
    steps = {st["id"]: st for st in out["steps"]}
    assert steps["pipeline.py:numbers"]["artifact_mismatch"] is True
    assert steps["pipeline.py:total"]["status"] == "ran"
    assert steps["pipeline.py:total"]["artifact_mismatch"] is True
    assert "input numbers" in steps["pipeline.py:total"]["warning"]


def test_an_untouched_store_copy_carries_no_marker(shared):
    store, make = shared
    proc = cli(make("reader"), store, "get", "total", "--json")
    assert proc.returncode == 0, proc.stderr
    assert all("artifact_mismatch" not in st for st in json.loads(proc.stdout)["steps"])


def _steps(proc: subprocess.CompletedProcess) -> dict[str, dict]:
    return {st["id"].rsplit(":", 1)[1]: st for st in json.loads(proc.stdout)["steps"]}


def _marked(proc: subprocess.CompletedProcess) -> list[str]:
    return sorted(name for name, st in _steps(proc).items() if "artifact_mismatch" in st)


def test_a_refresh_overwrites_the_store_copy_and_clears_the_mismatch(shared):
    """`barca docs remote`: `--refresh <step>` uploads over the object and records its hash."""
    store, make = shared
    stored = _one(store, "default/artifacts/*total*/*.json")
    stored.write_text('{"sum": 7}')
    reader = make("reader")
    assert _marked(cli(reader, store, "get", "total", "--json")) == ["total"]

    refreshed = cli(reader, store, "get", "total", "--refresh", "total", "--json")
    assert refreshed.returncode == 0, refreshed.stderr
    assert _steps(refreshed)["total"]["status"] == "ran"
    assert _marked(refreshed) == []  # recomputed here, so nothing was fetched for it
    assert json.loads(stored.read_text()) == {"sum": 6}

    third = cli(make("third"), store, "get", "total", "--json")
    assert third.returncode == 0, third.stderr
    assert json.loads(third.stdout)["final_output"] == {"sum": 6}
    assert _marked(third) == [] and "warning" not in third.stderr, third.stderr


def test_the_marker_is_in_a_multi_target_result_and_survives_fields(shared):
    store, make = shared
    _one(store, "default/artifacts/*numbers*/*.json").write_text("[5, 5]")
    proc = cli(make("reader"), store, "get", "numbers,total", "--refresh", "total", "--json")
    assert proc.returncode == 0, proc.stderr
    doc = json.loads(proc.stdout)
    assert doc["targets"]["total"]["final_output"] == {"sum": 10}
    assert _marked(proc) == ["numbers", "total"]

    fields = cli(
        make("fields"), store, "get", "total", "--refresh", "total",
        "--fields", "id,artifact_mismatch",
    )  # fmt: skip
    assert fields.returncode == 0, fields.stderr
    assert json.loads(fields.stdout)["steps"] == [
        {"id": "pipeline.py:numbers", "artifact_mismatch": True},
        {"id": "pipeline.py:total", "artifact_mismatch": True},
    ]
    # Without a mismatch the selected key is simply absent: it is never `false`.
    _one(store, "default/artifacts/*numbers*/*.json").write_text("[1, 2, 3]")
    clean = cli(make("clean"), store, "get", "total", "--fields", "id,artifact_mismatch")
    assert json.loads(clean.stdout)["steps"] == [
        {"id": "pipeline.py:numbers"},
        {"id": "pipeline.py:total"},
    ]


def test_a_step_that_fails_after_reading_a_differing_input_reports_it(shared):
    store, make = shared
    _one(store, "default/artifacts/*numbers*/*.json").write_text("[5, 5]")
    root = make("reader")
    (root / "pipeline.py").write_text(
        PIPELINE + '\n\n@asset(inputs={"numbers": numbers})\ndef broken(numbers: list) -> int:\n'
        '    raise ValueError(f"cannot use {numbers}")\n'
    )
    proc = cli(root, store, "get", "broken", "--json")
    assert proc.returncode == 1, proc.stderr
    doc = json.loads(proc.stdout)  # the failed run's result line
    assert doc["status"] == "failed" and "cannot use [5, 5]" in doc["error"]
    steps = _steps(proc)
    assert steps["broken"]["status"] == "failed"
    assert steps["broken"]["artifact_mismatch"] is True
    assert "input numbers" in steps["broken"]["warning"]
    assert steps["numbers"]["artifact_mismatch"] is True


def test_a_differing_copy_and_a_missing_object_are_told_apart(shared):
    """One object overwritten, another deleted: the first is used and flagged, the second is
    computed again. Neither is taken for the other."""
    store, make = shared
    _one(store, "default/artifacts/*numbers*/*.json").write_text("[5, 5]")
    _one(store, "default/artifacts/*total*/*.json").unlink()
    proc = cli(make("reader"), store, "get", "total", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == {"sum": 10}
    steps = _steps(proc)
    assert (steps["numbers"]["status"], steps["numbers"].get("reason")) == ("cached", None)
    assert steps["numbers"]["artifact_mismatch"] is True
    assert (steps["total"]["status"], steps["total"]["reason"]) == ("ran", "artifact_missing")
    # `total` read the differing `numbers`; its own object was missing, not differing.
    assert steps["total"]["warning"].startswith("input numbers: ")
    assert "the artifact of its cached result is missing" in proc.stderr
    assert proc.stderr.count("is not the one this result was recorded with") == 1
    assert json.loads(_one(store, "default/artifacts/*total*/*.json").read_text()) == {"sum": 10}


def test_barca_serve_reports_the_marker_in_the_run_result(tmp_path):
    """`GET /status/{run_id}` carries the CLI's step entries, the marker included."""
    import signal
    import urllib.error
    import urllib.request

    from .test_serve_schedule import _free_port

    store = tmp_path / "store"
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    # `barca serve` does not share history: results are shared, history stays local.
    env.update(BARCA_REMOTE_URI=str(store), BARCA_STATE="off")
    first = subprocess.run(
        [_find_binary(), "get", "total", "--json"], cwd=root, env=env, capture_output=True
    )
    assert first.returncode == 0, first.stderr
    # The store's copy is overwritten and this machine no longer has its own.
    _one(store, "default/artifacts/*total*/*.json").write_text('{"sum": 7}')
    _one(root, ".barca/artifacts/*total*/*.json").unlink()

    port = _free_port()
    server = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port), "--no-schedule"],
        cwd=root,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    def call(method: str, path: str) -> dict:
        req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", method=method)
        with urllib.request.urlopen(req, timeout=5) as resp:
            return json.loads(resp.read())

    def poll(method: str, path: str, done) -> dict:
        deadline = time.monotonic() + 60
        while True:
            assert server.poll() is None, "the server exited"
            try:
                body = call(method, path)
                if done(body):
                    return body
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                pass  # not listening yet
            assert time.monotonic() < deadline, f"timed out on {method} {path}"
            time.sleep(0.05)

    try:
        poll("GET", "/health", lambda body: True)
        handle = call("POST", "/get/total")["run_id"]
        run = poll("GET", f"/status/{handle}", lambda r: r["status"] not in ("pending", "running"))
    finally:
        server.send_signal(signal.SIGTERM)
        try:
            server.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            server.kill()
            server.communicate()
    assert run["status"] == "complete", run
    steps = {st["id"]: st for st in run["result"]["steps"]}
    assert steps["pipeline.py:total"]["artifact_mismatch"] is True
    assert "--refresh pipeline.py:total" in steps["pipeline.py:total"]["warning"]
    assert "artifact_mismatch" not in steps["pipeline.py:numbers"]
    assert run["result"]["warnings"] == []


def test_a_dry_run_does_not_contact_the_store_and_reports_no_mismatch(shared):
    store, make = shared
    _one(store, "default/artifacts/*numbers*/*.json").write_text("[5, 5]")
    proc = cli(make("reader"), store, "get", "total", "--refresh", "total", "--dry-run", "--json")
    assert proc.returncode == 0, proc.stderr
    assert all("artifact_mismatch" not in st for st in json.loads(proc.stdout)["steps"])


NONDETERMINISTIC = """
import os
import time
import uuid

from barca import asset


@asset()
def up() -> dict:
    return {"id": uuid.uuid4().hex}


@asset(inputs={"up": up})
def slow(up: dict) -> dict:
    time.sleep(float(os.environ.get("SLOW", "0")))
    return up


@asset(inputs={"up": up})
def quick(up: dict) -> dict:
    return up
"""


def _machine(tmp_path: Path, name: str) -> Path:
    root = tmp_path / name
    root.mkdir()
    (root / "pipeline.py").write_text(NONDETERMINISTIC)
    return root


def _start(cwd: Path, store: Path, *args: str, **env: str) -> subprocess.Popen:
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.Popen(
        [_find_binary(), *args],
        cwd=cwd,
        env={**base, "BARCA_REMOTE_URI": str(store), **env},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _finish(proc: subprocess.Popen) -> subprocess.CompletedProcess:
    out, err = proc.communicate(timeout=120)
    return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)


def test_overlapping_runs_of_the_same_step_leave_other_machines_working(tmp_path):
    """Two machines compute `up` at once: the store holds one result, the newest row the other."""
    store = tmp_path / "store"
    b = _start(_machine(tmp_path, "b"), store, "get", "slow", "--json", SLOW="6")
    time.sleep(2.5)
    a = _finish(_start(_machine(tmp_path, "a"), store, "get", "quick", "--json"))
    assert a.returncode == 0, a.stderr
    b = _finish(b)
    assert b.returncode == 0, b.stderr
    assert "conflict" in b.stderr, "the runs did not overlap: " + b.stderr

    for root in (tmp_path / "a", _machine(tmp_path, "c")):
        proc = cli(root, store, "get", "up", "--json")
        assert proc.returncode == 0, f"{root.name}: {proc.stderr}"
        assert "id" in json.loads(proc.stdout)["final_output"]


def test_a_refresh_killed_after_its_upload_does_not_lock_the_step(tmp_path):
    store = tmp_path / "store"
    root = _machine(tmp_path, "d")
    assert cli(root, store, "get", "slow", "--json").returncode == 0
    killed = _start(root, store, "get", "slow", "--refresh", "up", "--json", SLOW="30")
    time.sleep(5)
    killed.kill()
    killed.communicate(timeout=30)

    proc = cli(root, store, "get", "up", "--json")
    assert proc.returncode == 0, proc.stderr
    assert "id" in json.loads(proc.stdout)["final_output"]


def test_independent_histories_sharing_a_store_survive_a_deleted_local_copy(tmp_path):
    store = tmp_path / "store"
    b, a = _machine(tmp_path, "b"), _machine(tmp_path, "a")

    def get(root: Path, target: str) -> subprocess.CompletedProcess:
        return _finish(_start(root, store, "get", target, "--json", BARCA_STATE="off"))

    assert get(b, "up").returncode == 0
    assert get(a, "up").returncode == 0
    shutil.rmtree(b / ".barca" / "artifacts")
    for target in ("up", "quick"):
        proc = get(b, target)
        assert proc.returncode == 0, f"{target}: {proc.stderr}"


def test_fetch_and_warning_lines_reach_a_terminal_through_the_progress_bar(shared):
    """The TTY path prints them with ProgressBar::println; the piped path is covered above."""
    import pty
    import re
    import select

    store, make = shared
    _one(store, "default/artifacts/*numbers*/*.json").write_text("[5, 5]")
    root = make("reader")
    master, slave = pty.openpty()
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    proc = subprocess.Popen(
        [_find_binary(), "get", "total", "--refresh", "total", "--json"],
        cwd=root,
        env={**env, "BARCA_REMOTE_URI": str(store), "TERM": "xterm"},
        stdout=subprocess.PIPE,
        stderr=slave,
        text=True,
    )
    os.close(slave)
    chunks = []
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], 0.2)
        if ready:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            chunks.append(data)
        elif proc.poll() is not None:
            break
    out, _ = proc.communicate(timeout=60)
    os.close(master)
    text = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", b"".join(chunks).decode(errors="replace"))
    assert proc.returncode == 0, text
    assert json.loads(out)["final_output"] == {"sum": 10}
    assert "fetched 1 cached artifact" in text, text
    assert "warning: pipeline.py:numbers" in text, text
    assert "--refresh pipeline.py:numbers" in text, text
