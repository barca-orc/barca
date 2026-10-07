"""Tests for barca._transfer — the coordinator's artifact transfer helper.

The helper is driven in-process over a socketpair (the coordinator's end is
`peer`), so the memory:// filesystem is shared with the test. Wire format is
pinned by crates/barca-core/src/protocol.rs (TransferRequest/TransferReply).
"""

import hashlib
import json
import os
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import pytest

from barca import _runtime, _storage, _transfer

# Test-only shim that pauses a helper process at a named point (see its docstring).
HOLD_SHIM = str(Path(__file__).resolve().parent / "hold")


@pytest.fixture(autouse=True)
def _clean_memory_fs():
    yield
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


def _send(sock: socket.socket, msg: dict) -> None:
    body = json.dumps(msg).encode()
    sock.sendall(struct.pack(">I", len(body)) + body)


def _recv(sock: socket.socket) -> dict:
    def exact(n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = sock.recv(n - len(buf))
            if not chunk:
                raise EOFError
            buf += chunk
        return buf

    (n,) = struct.unpack(">I", exact(4))
    return json.loads(exact(n))


def _sized(reply: dict) -> dict:
    """A done reply without its hash and fetched flag (TestChecksums covers those)."""
    return {k: v for k, v in reply.items() if k not in ("sha256", "fetched", "mismatch")}


class Helper:
    """A running helper plus the coordinator's end of its socket."""

    def __init__(self, **kwargs):
        ours, theirs = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        ours.settimeout(10)
        self.peer = ours
        self._saved = _runtime._socket
        kwargs.setdefault("backoff", 0.0)
        self.thread = threading.Thread(target=_transfer.serve, args=(theirs,), kwargs=kwargs)
        self.thread.start()

    def request(self, msg: dict) -> None:
        _send(self.peer, msg)

    def reply(self) -> dict:
        return _recv(self.peer)

    def replies(self, n: int) -> dict[int, dict]:
        out = {}
        for _ in range(n):
            r = self.reply()
            out[r["id"]] = r
        return out

    def close(self) -> None:
        try:
            _send(self.peer, {"type": "shutdown"})
        except OSError:
            pass
        self.thread.join(timeout=10)
        self.peer.close()
        _runtime._socket = self._saved


@pytest.fixture
def helper():
    h = Helper()
    yield h
    h.close()


_SHA_V1 = "9ab2253fc38981f5be9c25cf0a34b62cdf334652344bdef16b3d5dbc0b74f2f1"


class TestPutGet:
    def test_put_then_get_round_trips_bytes(self, helper, tmp_path):
        src = tmp_path / "a.json"
        src.write_bytes(b'{"v": 1}')
        helper.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://s/n/h.json"})
        done = {"type": "done", "size_bytes": 8, "sha256": _SHA_V1, "fetched": True}
        done["mismatch"] = False
        assert helper.reply() == {**done, "id": 1}
        assert _storage.exists("memory://s/n/h.json")

        dest = tmp_path / "mirror" / "n" / "h.json"
        helper.request(
            {"type": "get", "id": 2, "remote": "memory://s/n/h.json", "local": str(dest)}
        )
        assert helper.reply() == {**done, "id": 2}
        assert dest.read_bytes() == b'{"v": 1}'

    def test_get_is_atomic_no_temp_left_behind(self, helper, tmp_path):
        src = tmp_path / "a.bin"
        src.write_bytes(b"x" * 4096)
        _storage.put_file(src, "memory://s/a.bin")
        dest = tmp_path / "m" / "a.bin"
        helper.request({"type": "get", "id": 1, "remote": "memory://s/a.bin", "local": str(dest)})
        assert helper.reply()["type"] == "done"
        assert sorted(p.name for p in dest.parent.iterdir()) == ["a.bin"]

    def test_plain_path_store(self, helper, tmp_path):
        src = tmp_path / "a.bin"
        src.write_bytes(b"abc")
        store = tmp_path / "shared" / "default" / "artifacts" / "n" / "h.bin"
        helper.request({"type": "put", "id": 1, "local": str(src), "remote": str(store)})
        assert helper.reply()["type"] == "done"
        assert store.read_bytes() == b"abc"

        back = tmp_path / "back" / "h.bin"
        helper.request({"type": "get", "id": 2, "remote": f"file://{store}", "local": str(back)})
        assert helper.reply()["type"] == "done"
        assert back.read_bytes() == b"abc"


class TestChecksums:
    """A recorded SHA-256 decides whether a local copy is kept, replaced or refused."""

    BODY = b'{"x": 1}'
    SHA = hashlib.sha256(BODY).hexdigest()

    def _store(self, body: bytes = BODY) -> str:
        remote = "memory://store/a/h.json"
        with _storage.get_fs(remote).open(remote, "wb") as f:
            f.write(body)
        return remote

    def _get(self, helper, remote, local, sha256=SHA) -> dict:
        helper.request(
            {"type": "get", "id": 1, "remote": remote, "local": str(local), "sha256": sha256}
        )
        return helper.reply()

    def test_put_reports_the_hash_of_the_uploaded_bytes(self, helper, tmp_path):
        src = tmp_path / "h.json"
        src.write_bytes(self.BODY)
        helper.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://s/h.json"})
        reply = helper.reply()
        assert (reply["sha256"], reply["fetched"]) == (self.SHA, True)

    def test_matching_local_copy_is_kept_and_nothing_is_downloaded(
        self, helper, tmp_path, monkeypatch
    ):
        local = tmp_path / "h.json"
        local.write_bytes(self.BODY)
        monkeypatch.setattr(_storage, "get_file", lambda *a: pytest.fail("downloaded"))
        reply = self._get(helper, self._store(), local)
        assert (reply["type"], reply["sha256"], reply["fetched"]) == ("done", self.SHA, False)
        assert reply["mismatch"] is False

    def test_local_copy_with_other_bytes_is_replaced_from_the_store(self, helper, tmp_path):
        local = tmp_path / "h.json"
        local.write_bytes(b'{"x": 2}')  # same size, different bytes
        reply = self._get(helper, self._store(), local)
        assert (reply["type"], reply["fetched"]) == ("done", True)
        assert local.read_bytes() == self.BODY

    def test_store_copy_with_other_bytes_is_used_and_flagged(self, helper, tmp_path):
        # The path is {node}/{run_hash}: a refresh or another machine overwrites it.
        local = tmp_path / "h.json"
        reply = self._get(helper, self._store(b'{"x": 2}'), local)
        assert (reply["type"], reply["fetched"], reply["mismatch"]) == ("done", True, True)
        assert local.read_bytes() == b'{"x": 2}'
        assert sorted(p.name for p in tmp_path.iterdir()) == ["h.json"]

    def test_local_copy_equal_to_a_mismatched_store_copy_is_left_alone(self, helper, tmp_path):
        local = tmp_path / "h.json"
        local.write_bytes(b'{"x": 2}')
        before = local.stat().st_mtime_ns
        reply = self._get(helper, self._store(b'{"x": 2}'), local)
        assert (reply["type"], reply["fetched"], reply["mismatch"]) == ("done", False, True)
        assert local.stat().st_mtime_ns == before
        assert sorted(p.name for p in tmp_path.iterdir()) == ["h.json"]

    def test_unreadable_local_copy_is_replaced_like_any_other_mismatch(self, helper, tmp_path):
        local = tmp_path / "h.json"
        local.write_bytes(b'{"x": 2}')
        local.chmod(0)
        reply = self._get(helper, self._store(), local)
        assert (reply["type"], reply["fetched"], reply["mismatch"]) == ("done", True, False)
        assert local.read_bytes() == self.BODY

    def test_without_a_recorded_hash_the_download_is_taken_as_it_is(self, helper, tmp_path):
        local = tmp_path / "h.json"
        reply = self._get(helper, self._store(b'{"x": 2}'), local, sha256=None)
        assert (reply["type"], reply["fetched"]) == ("done", True)
        assert reply["sha256"] == hashlib.sha256(b'{"x": 2}').hexdigest()


class TestDirectoryAtTheArtifactPath:
    """An artifact is one file. A directory where it belongs is moved aside, never deleted
    (`_storage.make_way`, `barca docs cache`)."""

    BODY = TestChecksums.BODY
    SHA = TestChecksums.SHA

    def _get(self, helper, local, sha256=SHA) -> dict:
        store = local.parent / "store"
        store.mkdir(exist_ok=True)
        (store / "h.json").write_bytes(self.BODY)
        msg = {"type": "get", "id": 1, "remote": str(store / "h.json"), "local": str(local)}
        if sha256 is not None:
            msg["sha256"] = sha256
        helper.request(msg)
        return helper.reply()

    def _names(self, directory) -> list[str]:
        return sorted(p.name for p in directory.iterdir() if p.name != "store")

    def test_an_empty_directory_is_removed(self, helper, tmp_path, capfd):
        local = tmp_path / "h.json"
        local.mkdir()
        reply = self._get(helper, local)
        assert (reply["type"], reply["fetched"], reply["mismatch"]) == ("done", True, False)
        assert local.is_file() and local.read_bytes() == self.BODY
        assert self._names(tmp_path) == ["h.json"]
        assert "warning" not in capfd.readouterr().err  # nothing was there to lose

    def test_a_directory_with_contents_is_moved_aside_intact(self, helper, tmp_path, capfd):
        local = tmp_path / "h.json"
        (local / "inner").mkdir(parents=True)
        (local / "inner" / "x").write_text("mine")
        (local / "top").write_text("also mine")
        reply = self._get(helper, local)
        assert (reply["type"], reply["fetched"], reply["mismatch"]) == ("done", True, False)
        assert local.is_file() and local.read_bytes() == self.BODY
        aside = tmp_path / "h.json.moved-aside"
        assert self._names(tmp_path) == ["h.json", "h.json.moved-aside"]
        assert (aside / "inner" / "x").read_text() == "mine"
        assert (aside / "top").read_text() == "also mine"
        err = capfd.readouterr().err
        assert f"[barca] warning: {local} is a directory, not an artifact" in err, err
        assert str(aside) in err, err

    def test_an_earlier_moved_aside_directory_is_never_overwritten(self, helper, tmp_path):
        local = tmp_path / "h.json"
        earlier = tmp_path / "h.json.moved-aside"
        earlier.mkdir()
        (earlier / "old").write_text("first")
        local.mkdir()
        (local / "new").write_text("second")
        assert self._get(helper, local)["type"] == "done"
        assert (earlier / "old").read_text() == "first"
        assert (tmp_path / "h.json.moved-aside-2" / "new").read_text() == "second"
        assert local.read_bytes() == self.BODY

    def test_a_directory_is_moved_aside_without_a_recorded_hash_too(self, helper, tmp_path):
        local = tmp_path / "h.json"
        local.mkdir()
        (local / "x").write_text("mine")
        assert self._get(helper, local, sha256=None)["type"] == "done"
        assert local.read_bytes() == self.BODY
        assert (tmp_path / "h.json.moved-aside" / "x").read_text() == "mine"

    def test_a_symlink_to_a_directory_is_replaced_and_its_target_is_untouched(
        self, helper, tmp_path
    ):
        elsewhere = tmp_path / "elsewhere"
        elsewhere.mkdir()
        (elsewhere / "precious").write_text("keep")
        local = tmp_path / "h.json"
        local.symlink_to(elsewhere, target_is_directory=True)
        assert self._get(helper, local)["type"] == "done"
        assert local.is_file() and not local.is_symlink()
        assert local.read_bytes() == self.BODY
        assert self._names(elsewhere) == ["precious"]
        assert (elsewhere / "precious").read_text() == "keep"
        assert self._names(tmp_path) == ["elsewhere", "h.json"]

    def test_a_symlink_to_a_file_is_replaced_and_its_target_is_untouched(self, helper, tmp_path):
        target = tmp_path / "target.json"
        target.write_text("other bytes")
        local = tmp_path / "h.json"
        local.symlink_to(target)
        assert self._get(helper, local)["type"] == "done"
        assert not local.is_symlink() and local.read_bytes() == self.BODY
        assert target.read_text() == "other bytes"

    def test_a_directory_in_the_store_is_an_error_and_is_left_alone(self, helper, tmp_path):
        # The store is shared: barca moves nothing there.
        src = tmp_path / "a.json"
        src.write_bytes(self.BODY)
        dest = tmp_path / "store" / "n" / "h.json"
        (dest / "theirs").mkdir(parents=True)
        helper.request({"type": "put", "id": 1, "local": str(src), "remote": str(dest)})
        reply = helper.reply()
        assert reply["type"] == "error" and "IsADirectoryError" in reply["message"], reply
        assert (dest / "theirs").is_dir()
        assert sorted(p.name for p in dest.parent.iterdir()) == ["h.json"]


class TestMakeWay:
    def test_a_file_or_nothing_at_the_path_is_left_alone(self, tmp_path):
        assert _storage.make_way(tmp_path / "absent.json") is None
        (tmp_path / "f.json").write_text("x")
        assert _storage.make_way(tmp_path / "f.json") is None
        assert (tmp_path / "f.json").read_text() == "x"

    def test_it_returns_where_the_directory_went(self, tmp_path):
        d = tmp_path / "h.parquet"
        (d / "part-0").mkdir(parents=True)
        assert _storage.make_way(d) == tmp_path / "h.parquet.moved-aside"
        assert not d.exists()
        assert (tmp_path / "h.parquet.moved-aside" / "part-0").is_dir()

    def test_a_file_is_never_moved_even_if_it_looked_like_a_directory(self, tmp_path, monkeypatch):
        # Another process installed the artifact after this one looked: the check said
        # "directory", the thing there now is the artifact.
        dest = tmp_path / "h.json"
        dest.write_text("the artifact")
        monkeypatch.setattr(type(dest), "is_dir", lambda self: True)
        assert _storage.make_way(dest) is None
        assert dest.read_text() == "the artifact"
        assert sorted(p.name for p in tmp_path.iterdir()) == ["h.json"]

    def test_an_artifact_installed_between_the_look_and_the_rename_is_left_alone(
        self, tmp_path, monkeypatch, capfd
    ):
        dest = tmp_path / "h.json"
        (dest / "inner").mkdir(parents=True)
        real = os.path.lexists

        def another_process_gets_there_first(path):
            # Runs just before the rename: the directory is moved away and the artifact
            # installed, as a concurrent `make_way` and install would do.
            if dest.is_dir():
                dest.rename(tmp_path / "moved-by-the-other")
                dest.write_text("the artifact")
            return real(path)

        monkeypatch.setattr(os.path, "lexists", another_process_gets_there_first)
        assert _storage.make_way(dest) is None
        assert dest.read_text() == "the artifact"
        assert sorted(p.name for p in tmp_path.iterdir()) == ["h.json", "moved-by-the-other"]
        assert "is a directory" not in capfd.readouterr().err

    @pytest.mark.skipif(os.geteuid() == 0, reason="root may rename in any directory")
    def test_a_directory_that_cannot_be_moved_is_an_error_that_says_what_to_do(self, tmp_path):
        parent = tmp_path / "node"
        dest = parent / "h.json"
        (dest / "inner").mkdir(parents=True)
        parent.chmod(0o555)
        try:
            with pytest.raises(_storage.ArtifactPathError) as raised:
                _storage.make_way(dest)
        finally:
            parent.chmod(0o755)
        message = str(raised.value)
        assert str(dest) in message and "PermissionError" in message
        assert f"write permission on {parent}" in message and "Nothing was deleted" in message
        assert (dest / "inner").is_dir()

    def test_processes_racing_to_install_one_artifact_never_move_a_file(self, tmp_path):
        """Six processes, each making way and installing the artifact 50 times while a
        seventh keeps putting a directory back. Whatever the interleaving: nothing that was
        moved aside is a file, and no directory's contents are lost."""
        dest = tmp_path / "node" / "h.json"
        dest.parent.mkdir()
        code = (
            "import os, sys\n"
            "from pathlib import Path\n"
            "from barca import _storage\n"
            "dest = Path(sys.argv[1])\n"
            "for i in range(50):\n"
            "    _storage.make_way(dest)\n"
            "    with _storage.staged_beside(dest) as tmp:\n"
            "        tmp.write_text('artifact')\n"
            "        try:\n"
            "            os.replace(tmp, dest)\n"
            "        except (IsADirectoryError, PermissionError):\n"
            "            pass  # a directory came back in between: the next round moves it\n"
        )
        spoil = (
            "import sys\n"
            "from pathlib import Path\n"
            "dest = Path(sys.argv[1])\n"
            "for i in range(40):\n"
            "    try:\n"
            "        dest.unlink()\n"
            "        (dest / f'made-{i}').mkdir(parents=True)\n"
            "    except OSError:\n"
            "        pass\n"
        )
        procs = [
            subprocess.Popen([sys.executable, "-c", code, str(dest)], stderr=subprocess.PIPE)
            for _ in range(6)
        ]
        procs.append(subprocess.Popen([sys.executable, "-c", spoil, str(dest)]))
        errors = [p.communicate(timeout=120)[1] for p in procs]
        assert [p.returncode for p in procs] == [0] * 7, errors
        aside = [p for p in dest.parent.iterdir() if ".moved-aside" in p.name]
        assert all(p.is_dir() for p in aside), [p.name for p in aside if not p.is_dir()]
        # Every warning names a directory that was really moved.
        warned = b"".join(e for e in errors if e).count(b"is a directory, not an artifact")
        assert warned == len(aside)


class ProcessHelper:
    """`python -m barca._transfer` as its own process, as the coordinator runs it.

    `hold` pauses it at a point of python/tests/hold/sitecustomize.py until `release()`.
    """

    def __init__(self, tmp_path, hold: str | None = None, lifeline: bool = False):
        # A Unix socket path is limited to about 100 bytes; pytest's tmp_path is longer.
        self._sockdir = tempfile.mkdtemp(prefix="bx", dir="/tmp")
        self.hold_dir = tmp_path / f"hold-{os.path.basename(self._sockdir)}"
        self.hold_dir.mkdir()
        path = os.path.join(self._sockdir, "s")
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(path)
        server.listen(1)
        server.settimeout(30)
        env = {**os.environ, "BARCA_SOCKET": path}
        if hold:
            env["PYTHONPATH"] = HOLD_SHIM + os.pathsep + env.get("PYTHONPATH", "")
            env["BARCA_TEST_HOLD"] = f"{hold}:{self.hold_dir}"
        self.hold = hold
        if lifeline:
            # As the coordinator starts it: stdin is a pipe only the coordinator holds.
            env["BARCA_LIFELINE"] = "stdin"
        self.proc = subprocess.Popen(
            [sys.executable, "-m", "barca._transfer"],
            env=env,
            stdin=subprocess.PIPE if lifeline else None,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            self.peer, _ = server.accept()
        finally:
            server.close()
        self.peer.settimeout(30)

    def request(self, msg: dict) -> None:
        _send(self.peer, msg)

    def reply(self) -> dict:
        return _recv(self.peer)

    def wait_until_held(self) -> None:
        marker = self.hold_dir / f"{self.hold}.started"
        deadline = time.monotonic() + 30
        while not marker.exists():
            assert self.proc.poll() is None, "the helper exited before reaching the hold"
            assert time.monotonic() < deadline, f"the helper never reached {self.hold}"
            time.sleep(0.02)

    def release(self) -> None:
        (self.hold_dir / "release").write_text("")

    def finish(self) -> tuple[int, str]:
        """Wait for the helper to exit; (exit code, stderr)."""
        try:
            _, err = self.proc.communicate(timeout=30)
        finally:
            self.proc.kill()
            self.peer.close()
            shutil.rmtree(self._sockdir, ignore_errors=True)
        return self.proc.returncode, err


@pytest.fixture
def process_helper(tmp_path):
    helpers = []

    def start(hold: str | None = None, lifeline: bool = False) -> ProcessHelper:
        helpers.append(ProcessHelper(tmp_path, hold, lifeline))
        return helpers[-1]

    yield start
    for h in helpers:
        if h.proc.poll() is None:
            h.proc.kill()
            h.finish()


def _store_file(tmp_path, body: bytes):
    store = tmp_path / "store"
    store.mkdir(exist_ok=True)
    (store / "h.json").write_bytes(body)
    return store / "h.json"


class TestSignals:
    """Ctrl-C is the coordinator's to act on; it stops the helper with SIGTERM
    (crates/barca-core/src/helper_proc.rs)."""

    def test_sigint_is_ignored_and_the_helper_keeps_serving(self, process_helper, tmp_path):
        h = process_helper()
        remote = _store_file(tmp_path, b'{"x": 1}')
        # A reply proves the helper is in its serving loop before the signal is sent.
        h.request({"type": "probe", "id": 1, "root": str(remote.parent)})
        assert h.reply()["type"] == "done"

        h.proc.send_signal(signal.SIGINT)
        local = tmp_path / "local" / "h.json"
        h.request({"type": "get", "id": 2, "remote": str(remote), "local": str(local)})
        assert h.reply()["type"] == "done"  # still serving after the interrupt
        assert local.read_bytes() == b'{"x": 1}'

        h.request({"type": "shutdown"})
        code, err = h.finish()
        assert code == 0, err
        assert "Traceback" not in err and "KeyboardInterrupt" not in err, err

    def test_sigterm_mid_download_removes_the_temp_file_and_exits(self, process_helper, tmp_path):
        h = process_helper(hold="get")
        remote = _store_file(tmp_path, b'{"x": 1}')
        local = tmp_path / "local" / "h.json"
        h.request({"type": "get", "id": 1, "remote": str(remote), "local": str(local)})
        h.wait_until_held()
        # The download is under way: its temp file is there, the artifact is not.
        assert [p.name.endswith(".tmp") for p in local.parent.iterdir()] == [True]

        h.proc.send_signal(signal.SIGTERM)
        code, err = h.finish()
        assert code == 128 + signal.SIGTERM, err
        assert list(local.parent.iterdir()) == []
        assert "Traceback" not in err, err

    def test_sigterm_mid_upload_to_a_directory_store_leaves_no_partial_object(
        self, process_helper, tmp_path
    ):
        h = process_helper(hold="copied")
        src = tmp_path / "a.json"
        src.write_bytes(b'{"x": 1}')
        dest = tmp_path / "store" / "n" / "h.json"
        h.request({"type": "put", "id": 1, "local": str(src), "remote": str(dest)})
        h.wait_until_held()
        # Held between writing the temp file and renaming it: the object is not there yet.
        assert [p.name.endswith(".tmp") for p in dest.parent.iterdir()] == [True]

        h.proc.send_signal(signal.SIGTERM)
        code, err = h.finish()
        assert code == 128 + signal.SIGTERM, err
        assert list(dest.parent.iterdir()) == []


class TestCoordinatorGone:
    """The helper is deaf to Ctrl-C, so it has to notice by itself that the coordinator is
    gone, and then leave without a word and without a half-written file (#249)."""

    def _run(self, tmp_path, **env) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, "-m", "barca._transfer"],
            env={**os.environ, "BARCA_SOCKET": str(tmp_path / "no-such.sock"), **env},
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def test_no_socket_and_no_coordinator_is_a_silent_exit(self, tmp_path):
        # What a helper finds when the command that started it has already ended: the socket
        # is gone and so is the other end of its lifeline (here: stdin at end-of-file).
        proc = self._run(tmp_path, BARCA_LIFELINE="stdin")
        assert (proc.returncode, proc.stdout, proc.stderr) == (0, "", "")

    def test_no_socket_with_the_coordinator_alive_is_one_line_not_a_traceback(self, tmp_path):
        proc = self._run(tmp_path)
        assert proc.returncode == 1
        (line,) = proc.stderr.strip().splitlines()
        assert line.startswith("[barca] transfer helper: cannot reach the coordinator at ")
        assert "Error: " in line and "Traceback" not in proc.stderr

    def test_the_helper_leaves_when_its_lifeline_closes_mid_download(
        self, process_helper, tmp_path
    ):
        h = process_helper(hold="get", lifeline=True)
        remote = _store_file(tmp_path, b'{"x": 1}')
        local = tmp_path / "local" / "h.json"
        h.request({"type": "get", "id": 1, "remote": str(remote), "local": str(local)})
        h.wait_until_held()
        assert [p.name.endswith(".tmp") for p in local.parent.iterdir()] == [True]

        h.proc.stdin.close()  # the coordinator was killed
        code, err = h.finish()
        assert (code, err) == (0, "")
        assert list(local.parent.iterdir()) == []

    def test_the_helper_leaves_when_the_socket_closes_mid_download(self, process_helper, tmp_path):
        h = process_helper(hold="get")
        remote = _store_file(tmp_path, b'{"x": 1}')
        local = tmp_path / "local" / "h.json"
        h.request({"type": "get", "id": 1, "remote": str(remote), "local": str(local)})
        h.wait_until_held()

        h.peer.close()
        code, err = h.finish()
        assert (code, err) == (0, "")
        assert list(local.parent.iterdir()) == []

    def test_the_state_helper_leaves_when_its_lifeline_closes(self, tmp_path):
        hold_dir = tmp_path / "hold"
        hold_dir.mkdir()
        db = tmp_path / "metadata.db"
        db.write_bytes(b"not looked at")
        env = {
            **os.environ,
            "BARCA_LIFELINE": "stdin",
            "PYTHONPATH": HOLD_SHIM + os.pathsep + os.environ.get("PYTHONPATH", ""),
            "BARCA_TEST_HOLD": f"push:{hold_dir}",
        }
        proc = subprocess.Popen(
            [sys.executable, "-m", "barca._state", "push", str(tmp_path / "s" / "m.db"), str(db)],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 30
            while not (hold_dir / "push.started").exists():
                assert proc.poll() is None and time.monotonic() < deadline
                time.sleep(0.02)
            proc.stdin.close()
            out, err = proc.stdout.read(), proc.stderr.read()
            assert proc.wait(timeout=30) == 0
        finally:
            proc.kill()
        assert (out, err) == ("", "")
        assert not (tmp_path / "s" / "m.db").exists()


class TestCrossProcess:
    def test_two_processes_fetching_the_same_local_path_both_succeed(
        self, process_helper, tmp_path
    ):
        """Each downloads to its own temp file and renames it into place, so neither sees a
        partial file and neither removes the other's.

        Both helpers are held with their download staged, so the two fetches are certainly
        in flight at once; whichever rename lands second replaces a whole file with a whole
        file.
        """
        body = b"x" * 200_000
        sha = hashlib.sha256(body).hexdigest()
        remote = _store_file(tmp_path, body)
        local = tmp_path / "local" / "h.json"
        helpers = [process_helper(hold="get"), process_helper(hold="get")]
        for h in helpers:
            h.request(
                {"type": "get", "id": 1, "remote": str(remote), "local": str(local), "sha256": sha}
            )
        for h in helpers:
            h.wait_until_held()
        assert len([p for p in local.parent.iterdir() if p.name.endswith(".tmp")]) == 2
        assert not local.exists()

        for h in helpers:
            h.release()
        replies = [h.reply() for h in helpers]
        assert [(r["type"], r["sha256"], r["mismatch"]) for r in replies] == [
            ("done", sha, False)
        ] * 2
        for h in helpers:
            h.request({"type": "shutdown"})
            code, err = h.finish()
            assert code == 0, err
        assert local.read_bytes() == body
        assert sorted(p.name for p in local.parent.iterdir()) == ["h.json"]


class TestConcurrency:
    def test_many_in_flight_all_answered_by_id(self, tmp_path):
        h = Helper(concurrency=4)
        try:
            for i in range(8):
                src = tmp_path / f"{i}.bin"
                src.write_bytes(bytes([i]) * (i + 1))
                h.request(
                    {
                        "type": "put",
                        "id": 100 + i,
                        "local": str(src),
                        "remote": f"memory://c/{i}.bin",
                    }
                )
            got = h.replies(8)
            assert set(got) == {100 + i for i in range(8)}
            for i in range(8):
                assert _sized(got[100 + i]) == {"type": "done", "id": 100 + i, "size_bytes": i + 1}
        finally:
            h.close()

    def test_transfers_run_in_parallel(self, tmp_path, monkeypatch):
        """With concurrency N, N slow transfers overlap instead of queueing."""
        real_put = _storage.put_file

        def slow_put(local, dest):
            time.sleep(0.3)
            real_put(local, dest)

        monkeypatch.setattr(_storage, "put_file", slow_put)
        h = Helper(concurrency=4)
        try:
            t0 = time.monotonic()
            for i in range(4):
                src = tmp_path / f"{i}.bin"
                src.write_bytes(b"x")
                h.request({"type": "put", "id": i, "local": str(src), "remote": f"memory://p/{i}"})
            h.replies(4)
            assert time.monotonic() - t0 < 0.9
        finally:
            h.close()


class TestErrors:
    def test_missing_object_get_errors_and_helper_keeps_serving(self, helper, tmp_path):
        helper.request(
            {"type": "get", "id": 1, "remote": "memory://nope/x", "local": str(tmp_path / "x")}
        )
        r = helper.reply()
        assert r["type"] == "error" and r["id"] == 1
        assert "FileNotFoundError" in r["message"]
        # Said to be missing, so the coordinator recomputes the step instead of failing (#252).
        assert r["missing"] is True
        assert not (tmp_path / "x").exists()

        src = tmp_path / "ok.bin"
        src.write_bytes(b"ok")
        helper.request({"type": "put", "id": 2, "local": str(src), "remote": "memory://ok/x"})
        assert _sized(helper.reply()) == {"type": "done", "id": 2, "size_bytes": 2}

    def test_a_probe_succeeds_only_when_the_store_is_there(self, helper, tmp_path):
        """What lets the coordinator tell a missing object from a store that is gone (#252)."""
        store = tmp_path / "store"
        (store / "default" / "artifacts").mkdir(parents=True)
        helper.request({"type": "probe", "id": 1, "root": str(store / "default" / "artifacts")})
        assert helper.reply() == {
            "type": "done",
            "id": 1,
            "size_bytes": 0,
            "fetched": False,
            "mismatch": False,
        }

        gone = tmp_path / "unmounted" / "default" / "artifacts"
        helper.request({"type": "probe", "id": 2, "root": str(gone)})
        r = helper.reply()
        assert r["type"] == "error" and r["id"] == 2
        assert "FileNotFoundError" in r["message"]
        assert not gone.exists() and not gone.parent.exists(), "a probe must create nothing"

    def test_a_probe_of_an_object_store_lists_its_bucket(self, helper, tmp_path):
        import fsspec

        fs = fsspec.filesystem("memory")
        fs.pipe("/probe-bucket/proj/artifacts/n/h.json", b"1")
        # The bucket is there, even though this root prefix holds nothing.
        helper.request({"type": "probe", "id": 1, "root": "memory://probe-bucket/other/artifacts"})
        assert helper.reply()["type"] == "done"
        helper.request({"type": "probe", "id": 2, "root": "memory://no-such-bucket/proj/artifacts"})
        r = helper.reply()
        assert r["type"] == "error" and "FileNotFoundError" in r["message"]
        assert not fs.exists("/no-such-bucket"), "a probe must create nothing"

    def test_missing_local_put_errors(self, helper, tmp_path):
        helper.request(
            {"type": "put", "id": 9, "local": str(tmp_path / "gone"), "remote": "memory://x/y"}
        )
        r = helper.reply()
        assert r["type"] == "error" and r["id"] == 9

    def test_transient_failure_is_retried(self, tmp_path, monkeypatch):
        real_put = _storage.put_file
        attempts = []

        def flaky_put(local, dest):
            attempts.append(dest)
            if len(attempts) < 3:
                raise ConnectionError("reset by peer")
            real_put(local, dest)

        monkeypatch.setattr(_storage, "put_file", flaky_put)
        h = Helper(retries=3)
        try:
            src = tmp_path / "a"
            src.write_bytes(b"1")
            h.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://r/a"})
            assert h.reply()["type"] == "done"
            assert len(attempts) == 3
        finally:
            h.close()

    def test_retries_exhausted_reports_last_error(self, tmp_path, monkeypatch):
        def always_fail(local, dest):
            raise ConnectionError("unreachable")

        monkeypatch.setattr(_storage, "put_file", always_fail)
        h = Helper(retries=2)
        try:
            src = tmp_path / "a"
            src.write_bytes(b"1")
            h.request({"type": "put", "id": 5, "local": str(src), "remote": "memory://r/a"})
            r = h.reply()
            assert r == {
                "type": "error",
                "id": 5,
                "message": "ConnectionError: unreachable",
                "attempts": 3,
                "missing": False,
            }
        finally:
            h.close()

    def test_permanent_failure_is_not_retried(self, tmp_path, monkeypatch):
        calls = []

        def denied(local, dest):
            calls.append(1)
            raise PermissionError("denied")

        monkeypatch.setattr(_storage, "put_file", denied)
        h = Helper(retries=5)
        try:
            src = tmp_path / "a"
            src.write_bytes(b"1")
            h.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://r/a"})
            r = h.reply()
            assert r["type"] == "error" and r["attempts"] == 1
            # Denied is not "missing": the object may well be there.
            assert r["missing"] is False
            assert calls == [1]
        finally:
            h.close()

    def test_unknown_request_type_is_ignored(self, helper, tmp_path):
        helper.request({"type": "bogus", "id": 1})
        src = tmp_path / "a"
        src.write_bytes(b"1")
        helper.request({"type": "put", "id": 2, "local": str(src), "remote": "memory://u/a"})
        assert helper.reply()["id"] == 2


class TestLifecycle:
    def test_shutdown_finishes_in_flight_work(self, tmp_path, monkeypatch):
        real_put = _storage.put_file

        def slow_put(local, dest):
            time.sleep(0.2)
            real_put(local, dest)

        monkeypatch.setattr(_storage, "put_file", slow_put)
        h = Helper()
        src = tmp_path / "a"
        src.write_bytes(b"1")
        h.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://l/a"})
        h.request({"type": "shutdown"})
        assert _sized(h.reply()) == {"type": "done", "id": 1, "size_bytes": 1}
        h.thread.join(timeout=5)
        assert not h.thread.is_alive()
        assert _storage.exists("memory://l/a")
        h.peer.close()
        _runtime._socket = h._saved

    def test_coordinator_disconnect_exits(self, tmp_path):
        h = Helper()
        h.peer.shutdown(socket.SHUT_RDWR)
        h.thread.join(timeout=5)
        assert not h.thread.is_alive()
        h.peer.close()
        _runtime._socket = h._saved


class _HttpError(Exception):
    """Shapes the cloud SDKs use to carry an HTTP status on their errors."""

    def __init__(self, msg, *, status_code=None, code=None, response_status=None):
        super().__init__(msg)
        if status_code is not None:
            self.status_code = status_code  # azure.core HttpResponseError
        if code is not None:
            self.code = code  # gcsfs HttpError, google.api_core errors
        if response_status is not None:
            self.response = type("R", (), {"status_code": response_status})()


class TestHttpStatusClassification:
    """SDK errors that aren't builtin OSErrors are classified by HTTP status:
    4xx is permanent (except 408/429), everything else is retried."""

    def _attempts(self, tmp_path, monkeypatch, exc) -> int:
        calls = []

        def failing(local, dest):
            calls.append(1)
            raise exc

        monkeypatch.setattr(_storage, "put_file", failing)
        h = Helper(retries=3)
        try:
            src = tmp_path / "a"
            src.write_bytes(b"1")
            h.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://c/a"})
            r = h.reply()
            assert r["type"] == "error"
            assert r["attempts"] == len(calls)
            return len(calls)
        finally:
            h.close()

    @pytest.mark.parametrize(
        "exc",
        [
            _HttpError("AuthenticationFailed", status_code=403),  # Azure auth
            _HttpError("Unauthorized", status_code=401),
            _HttpError("not found", code=404),  # GCS
            _HttpError("precondition", response_status=412),
            _HttpError("bad request", code="400"),  # string codes too
        ],
        ids=["azure-403", "401", "gcs-404", "response-412", "string-400"],
    )
    def test_client_errors_are_permanent(self, tmp_path, monkeypatch, exc):
        assert self._attempts(tmp_path, monkeypatch, exc) == 1

    @pytest.mark.parametrize(
        ("exc", "missing"),
        [
            (FileNotFoundError("no such key"), True),  # what s3fs, adlfs and gcsfs raise
            (_HttpError("not found", code=404), True),
            (_HttpError("AuthenticationFailed", status_code=403), False),
            (PermissionError("denied"), False),
            (ConnectionError("unreachable"), False),
            (_HttpError("busy", status_code=503), False),
        ],
        ids=["file-not-found", "http-404", "http-403", "permission", "connection", "http-503"],
    )
    def test_only_an_object_that_does_not_exist_is_missing(self, exc, missing):
        assert _transfer._is_missing(exc) is missing

    @pytest.mark.parametrize(
        "exc",
        [
            _HttpError("busy", status_code=503),
            _HttpError("throttled", status_code=429),
            _HttpError("request timeout", code=408),
            _HttpError("no status at all"),
        ],
        ids=["503", "429", "408", "no-status"],
    )
    def test_server_throttle_and_unknown_errors_are_retried(self, tmp_path, monkeypatch, exc):
        assert self._attempts(tmp_path, monkeypatch, exc) == 4


class TestTimeout:
    """A stalled attempt is failed by the helper's watchdog instead of hanging the run."""

    def _stall(self, monkeypatch, seconds, record=None):
        release = threading.Event()

        def stalled_put(local, dest):
            if record is not None:
                record.append(dest)
            release.wait(seconds)

        monkeypatch.setattr(_storage, "put_file", stalled_put)
        return release

    def test_stalled_attempt_times_out_with_error_reply(self, tmp_path, monkeypatch):
        release = self._stall(monkeypatch, 5)
        h = Helper(timeout=0.3)
        try:
            src = tmp_path / "a"
            src.write_bytes(b"1")
            t0 = time.monotonic()
            h.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://t/a"})
            r = h.reply()
            assert time.monotonic() - t0 < 2
            assert r["type"] == "error" and r["id"] == 1 and r["attempts"] == 1
            assert r["message"].startswith("TimeoutError:")
            assert "0.3s" in r["message"]
        finally:
            release.set()
            h.close()

    def test_late_completion_after_timeout_sends_no_second_reply(self, tmp_path, monkeypatch):
        real_put = _storage.put_file
        gate = threading.Event()

        def slow_then_ok(local, dest):
            gate.wait(5)
            real_put(local, dest)

        monkeypatch.setattr(_storage, "put_file", slow_then_ok)
        h = Helper(timeout=0.2)
        try:
            a = tmp_path / "a"
            a.write_bytes(b"1")
            h.request({"type": "put", "id": 1, "local": str(a), "remote": "memory://t/a"})
            assert h.reply()["type"] == "error"
            gate.set()  # the abandoned attempt now finishes
            monkeypatch.setattr(_storage, "put_file", real_put)
            b = tmp_path / "b"
            b.write_bytes(b"22")
            h.request({"type": "put", "id": 2, "local": str(b), "remote": "memory://t/b"})
            # The next reply is for request 2, not a stale "done" for 1.
            assert _sized(h.reply()) == {"type": "done", "id": 2, "size_bytes": 2}
        finally:
            gate.set()
            h.close()

    def test_timed_out_attempt_is_not_retried(self, tmp_path, monkeypatch):
        calls = []
        release = self._stall(monkeypatch, 5, record=calls)
        h = Helper(timeout=0.2, retries=3)
        try:
            src = tmp_path / "a"
            src.write_bytes(b"1")
            h.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://t/a"})
            assert h.reply()["type"] == "error"
            release.set()
            time.sleep(0.3)
            assert len(calls) == 1
        finally:
            release.set()
            h.close()

    def test_timeout_clock_starts_when_the_transfer_starts_not_when_queued(
        self, tmp_path, monkeypatch
    ):
        """With one worker, the second request waits behind the first; its
        queue time must not count against it."""
        real_put = _storage.put_file

        def slowish(local, dest):
            time.sleep(0.25)
            real_put(local, dest)

        monkeypatch.setattr(_storage, "put_file", slowish)
        h = Helper(concurrency=1, timeout=0.4)
        try:
            for i in range(3):
                src = tmp_path / f"{i}"
                src.write_bytes(b"1")
                h.request({"type": "put", "id": i, "local": str(src), "remote": f"memory://q/{i}"})
            got = h.replies(3)
            assert all(r["type"] == "done" for r in got.values()), got
        finally:
            h.close()

    def test_shutdown_does_not_wait_for_abandoned_attempts(self, tmp_path, monkeypatch):
        release = self._stall(monkeypatch, 3)
        h = Helper(timeout=0.2)
        src = tmp_path / "a"
        src.write_bytes(b"1")
        h.request({"type": "put", "id": 1, "local": str(src), "remote": "memory://t/a"})
        assert h.reply()["type"] == "error"
        t0 = time.monotonic()
        h.request({"type": "shutdown"})
        h.thread.join(timeout=5)
        assert not h.thread.is_alive()
        assert time.monotonic() - t0 < 1.5, "shutdown waited for the stuck attempt"
        release.set()
        h.peer.close()
        _runtime._socket = h._saved


class TestEntryPoint:
    def test_main_requires_socket(self, monkeypatch, capsys):
        monkeypatch.delenv("BARCA_SOCKET", raising=False)
        assert _transfer.main() == 1
        assert "BARCA_SOCKET" in capsys.readouterr().err
