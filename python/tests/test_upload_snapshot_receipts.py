"""Successful receipts describe the bytes sent by that upload (#381)."""

import hashlib
import threading

import pytest
from barca import _artifacts, _storage, _transfer


@pytest.mark.parametrize("destination", ["directory", "memory"])
def test_receipt_describes_uploaded_bytes_after_atomic_source_replacement(
    tmp_path, monkeypatch, destination
):
    source = tmp_path / "value.json"
    remote = (
        str(tmp_path / "store.json")
        if destination == "directory"
        else "memory://receipt/value.json"
    )
    _artifacts.serialize(11, source, "json")
    copied, replaced = threading.Event(), threading.Event()
    replies, errors = [], []
    actual_put = _storage.put_file

    def held_put(local, target):
        actual_put(local, target)
        copied.set()
        assert replaced.wait(5), "producer replacement did not arrive"

    monkeypatch.setattr(_storage, "put_file", held_put)

    def upload():
        try:
            replies.append(
                _transfer._transfer({"type": "put", "local": str(source), "remote": remote})
            )
        except BaseException as error:
            errors.append(error)

    thread = threading.Thread(target=upload)
    thread.start()
    try:
        assert copied.wait(5), errors
        _artifacts.serialize(222, source, "json")
    finally:
        replaced.set()
        thread.join(6)
    assert not thread.is_alive() and not errors, errors
    downloaded = tmp_path / "download.json"
    _storage.get_file(remote, downloaded)
    assert downloaded.read_bytes() == b"11"
    assert source.read_bytes() == b"222"
    assert replies[0]["sha256"] == hashlib.sha256(b"11").hexdigest()
    assert replies[0]["size_bytes"] == 2
    assert not list(tmp_path.glob("*.tmp"))


@pytest.mark.parametrize("point", ["upload-open", "uploaded"])
@pytest.mark.parametrize("kind", ["asset", "sensor"])
def test_concurrent_cli_receipt_belongs_to_serializing_producer(tmp_path, point, kind):
    import contextlib
    import json
    import os
    import signal
    import sqlite3
    import subprocess
    import time
    from pathlib import Path

    from barca.api import _find_binary

    source = tmp_path / "pipeline.py"
    source.write_text(
        f"from barca import {kind}\nfrom pathlib import Path\n"
        f'@{kind}()\ndef value(): return int(Path("external").read_text())\n'
    )
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "external").write_text("11")
    store = tmp_path / "store"
    store.mkdir()
    hold_a, hold_b = tmp_path / "hold-a", tmp_path / "hold-b"
    hold_a.mkdir()
    hold_b.mkdir()
    shim = str(Path(__file__).parent / "hold")
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env.update(
        BARCA_REMOTE_URI=str(store),
        BARCA_POOL_SIZE="1",
        PYTHONPATH=shim + os.pathsep + env.get("PYTHONPATH", ""),
    )
    args = [_find_binary(), "get", "value", "pipeline.py", "--refresh-all", "--json"]
    processes = []

    def start(hold, at):
        proc = subprocess.Popen(
            args,
            cwd=tmp_path,
            env={**env, "BARCA_TEST_HOLD": f"{at}:{hold}"},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        processes.append(proc)
        return proc

    def wait_for(path):
        deadline = time.monotonic() + 15
        while not path.exists():
            for process in processes:
                if process.poll() is not None:
                    output, error = process.communicate(timeout=5)
                    pytest.fail(f"run stopped before {path}: {output}\n{error}")
            assert time.monotonic() < deadline, f"missing event {path}"
            time.sleep(0.01)

    a = start(hold_a, point)
    try:
        wait_for(hold_a / f"{point}.started")
        (tmp_path / "external").write_text("222")
        b = start(hold_b, "upload-open")
        wait_for(hold_b / "upload-open.started")
        local = list((tmp_path / ".barca/artifacts").rglob("*.json"))
        assert len(local) == 1 and local[0].read_bytes() == b"222"
        (hold_a / "release").touch()
        a_out, a_err = a.communicate(timeout=20)
        assert a.returncode == 0, a_err
        a_result = json.loads(a_out)
        # B has serialized, but its upload remains held: observe A's store object
        # before allowing the later request to overwrite that shared destination.
        remote = list((store / "default" / "artifacts").rglob("*.json"))
        assert len(remote) == 1 and remote[0].read_bytes() == b"11"
        with contextlib.closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
            rows_a = db.execute(
                "SELECT run_hash,output_hash,artifact_size_bytes FROM materializations WHERE run_id=?",
                (a_result["run_id"],),
            ).fetchall()
        assert rows_a and all(row[1:] == (hashlib.sha256(b"11").hexdigest(), 2) for row in rows_a)
        (hold_b / "release").touch()
        b_out, b_err = b.communicate(timeout=20)
        assert b.returncode == 0, b_err
        b_result = json.loads(b_out)
        with contextlib.closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
            rows_b = db.execute(
                "SELECT run_hash,output_hash,artifact_size_bytes FROM materializations WHERE run_id=?",
                (b_result["run_id"],),
            ).fetchall()
        assert rows_b and all(row[1:] == (hashlib.sha256(b"222").hexdigest(), 3) for row in rows_b)
        assert {r[0] for r in rows_a} == {r[0] for r in rows_b}, (
            "same code/inputs, overlapping atomic writers"
        )
        assert remote[0].read_bytes() == b"222"
        assert not list((tmp_path / ".barca/staging").rglob("*.tmp"))
    finally:
        (hold_a / "release").touch()
        (hold_b / "release").touch()
        for proc in processes:
            if proc.poll() is None:
                proc.send_signal(signal.SIGINT)
                try:
                    proc.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.communicate(timeout=5)


def test_serialization_snapshot_survives_replacement_with_copy_fallback(tmp_path, monkeypatch):
    import errno
    import os

    source = tmp_path / "value.json"

    def cannot_link(*args):
        raise OSError(errno.EXDEV, "different devices")

    monkeypatch.setattr(os, "link", cannot_link)
    size, digest, snapshot = _artifacts.serialize_for_upload(
        11, source, "json", str(tmp_path / "upload-test-"), want_hash=True
    )
    try:
        _artifacts.serialize(222, source, "json")
        assert size == 2 and digest == hashlib.sha256(b"11").hexdigest()
        from pathlib import Path

        assert Path(snapshot).read_bytes() == b"11" and source.read_bytes() == b"222"
    finally:
        from pathlib import Path

        Path(snapshot).unlink()


def test_cancelled_real_upload_cleans_run_owned_snapshots(tmp_path):
    import contextlib
    import os
    import signal
    import sqlite3
    import subprocess
    import time
    from pathlib import Path

    from barca.api import _find_binary

    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef value(): return 11\n"
    )
    store, hold = tmp_path / "store", tmp_path / "hold"
    store.mkdir()
    hold.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env.update(
        BARCA_REMOTE_URI=str(store),
        BARCA_POOL_SIZE="1",
        BARCA_TEST_HOLD=f"upload-open:{hold}",
        PYTHONPATH=str(Path(__file__).parent / "hold") + os.pathsep + env.get("PYTHONPATH", ""),
    )
    proc = subprocess.Popen(
        [_find_binary(), "get", "value", "pipeline.py", "--json"],
        cwd=tmp_path,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 15
        while not (hold / "upload-open.started").exists():
            assert proc.poll() is None
            assert time.monotonic() < deadline
            time.sleep(0.01)
        assert list((tmp_path / ".barca/staging").rglob("upload-*.tmp"))
        proc.send_signal(signal.SIGINT)
        out, err = proc.communicate(timeout=15)
        assert proc.returncode == 130, (out, err)
        with contextlib.closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
            assert (
                db.execute(
                    "SELECT count(*) FROM materializations WHERE status='success'"
                ).fetchone()[0]
                == 0
            )
        assert not list((tmp_path / ".barca/staging").rglob("*.tmp"))
        pid = int((hold / "upload-open.started").read_text())
        with pytest.raises(ProcessLookupError):
            os.kill(pid, 0)
    finally:
        (hold / "release").touch()
        if proc.poll() is None:
            proc.kill()
            proc.communicate(timeout=5)


def test_confirmed_receipt_is_checkpointed_while_later_step_is_running(tmp_path):
    import contextlib
    import os
    import signal
    import sqlite3
    import subprocess
    import time
    from pathlib import Path

    from barca.api import _find_binary

    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\nfrom pathlib import Path\nimport time\n"
        "@asset()\ndef value(): return 11\n"
        '@asset(inputs={"value": value})\ndef finished(value):\n'
        '    Path("consumer-started").touch()\n'
        "    until = time.monotonic() + 20\n"
        '    while not Path("finish-release").exists():\n'
        '        if time.monotonic() > until: raise TimeoutError("consumer release")\n'
        "        time.sleep(.01)\n"
        "    return value\n"
    )
    store, hold = tmp_path / "store", tmp_path / "hold"
    store.mkdir()
    hold.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env.update(
        BARCA_REMOTE_URI=str(store),
        BARCA_POOL_SIZE="1",
        BARCA_TEST_HOLD=f"uploaded:{hold}",
        PYTHONPATH=str(Path(__file__).parent / "hold") + os.pathsep + env.get("PYTHONPATH", ""),
    )
    proc = subprocess.Popen(
        [_find_binary(), "get", "finished", "pipeline.py", "--json"],
        cwd=tmp_path,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    def row():
        with contextlib.closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
            return db.execute(
                "SELECT m.output_hash,m.artifact_size_bytes,m.artifact_path,r.status "
                "FROM materializations m JOIN runs r ON m.run_id=r.run_id "
                "WHERE m.node_id='pipeline.py:value' AND m.status='success'"
            ).fetchone()

    try:
        until = time.monotonic() + 15
        while not (
            (hold / "uploaded.started").exists() and (tmp_path / "consumer-started").exists()
        ):
            assert proc.poll() is None and time.monotonic() < until
            time.sleep(0.01)
        assert row() is None, "no success row before upload confirmation"
        (hold / "release").touch()
        while (checkpoint := row()) is None:
            assert proc.poll() is None and time.monotonic() < until
            time.sleep(0.01)
        assert checkpoint[:2] == (hashlib.sha256(b"11").hexdigest(), 2)
        assert checkpoint[2].startswith(str(store)) and checkpoint[3] == "running"
        (tmp_path / "finish-release").touch()
        out, err = proc.communicate(timeout=15)
        assert proc.returncode == 0, (out, err)
        assert row()[:3] == checkpoint[:3], "terminal persistence retains the confirmed row"
        assert not list((tmp_path / ".barca/staging").rglob("*.tmp"))
    finally:
        (hold / "release").touch()
        (tmp_path / "finish-release").touch()
        if proc.poll() is None:
            proc.send_signal(signal.SIGINT)
            try:
                proc.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.communicate(timeout=5)


def test_failure_after_pinning_keeps_previous_artifact_and_removes_stages(tmp_path, monkeypatch):
    source = tmp_path / "value.json"
    _artifacts.serialize(7, source, "json")
    pin = _artifacts._pin_file

    def denied_after_pin(*args):
        pin(*args)
        raise PermissionError("controlled failure after snapshot pin")

    monkeypatch.setattr(_artifacts, "_pin_file", denied_after_pin)
    with pytest.raises(PermissionError):
        _artifacts.serialize_for_upload(
            11, source, "json", str(tmp_path / "upload-failed-"), want_hash=False
        )
    assert source.read_bytes() == b"7"
    assert not list(tmp_path.glob("*.tmp"))
    assert not list(tmp_path.glob(".*.tmp"))
