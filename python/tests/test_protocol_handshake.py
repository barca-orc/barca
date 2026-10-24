"""Negotiation is completed before a worker or transfer helper can receive work."""

import json
import socket
import struct
import threading

import pytest

from barca import _runtime


def exact(sock, n):
    data = bytearray()
    while len(data) < n:
        part = sock.recv(n - len(data))
        if not part:
            raise EOFError("peer closed during hello")
        data.extend(part)
    return bytes(data)


def receive(sock):
    return json.loads(exact(sock, struct.unpack(">I", exact(sock, 4))[0]))


def send(sock, value):
    data = json.dumps(value).encode()
    sock.sendall(struct.pack(">I", len(data)) + data)


@pytest.mark.parametrize("role", ["worker", "transfer"])
@pytest.mark.parametrize("response", ["valid", "mismatch", "eof", "malformed"])
def test_real_connection_negotiates_before_returning(tmp_path, monkeypatch, role, response):
    path = str(tmp_path / "protocol.sock")
    monkeypatch.setenv("BARCA_SOCKET", path)
    monkeypatch.delenv("BARCA_WORKER_ID", raising=False)
    monkeypatch.setattr(_runtime, "_socket", None)
    seen, errors = [], []
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(path)
        listener.listen(1)
        listener.settimeout(5)

        def coordinator():
            try:
                peer, _ = listener.accept()
                with peer:
                    peer.settimeout(5)
                    seen.append(receive(peer))
                    if response == "valid":
                        send(
                            peer,
                            {
                                "type": "hello",
                                "role": "coordinator",
                                "protocol_version": _runtime.PROTOCOL_VERSION,
                            },
                        )
                    elif response == "malformed":
                        peer.sendall(struct.pack(">I", 1) + b"{")
                    elif response == "mismatch":
                        send(
                            peer,
                            {
                                "type": "protocol_error",
                                "message": "worker protocol v2, coordinator expects v3; reinstall barca",
                            },
                        )
            except BaseException as error:
                errors.append(error)

        thread = threading.Thread(target=coordinator)
        thread.start()
        try:
            if response == "valid":
                connection = _runtime.connect(role=role)
                assert connection.gettimeout() is None
            else:
                with pytest.raises(RuntimeError, match="reinstall barca"):
                    _runtime.connect(role=role)
                assert _runtime._socket is None
        finally:
            _runtime.disconnect()
            thread.join(6)
        assert not thread.is_alive() and not errors, errors
        assert seen == [
            {"type": "hello", "role": role, "protocol_version": _runtime.PROTOCOL_VERSION}
        ]


@pytest.mark.parametrize("role", ["worker", "transfer"])
def test_a_worker_names_itself_in_its_hello(tmp_path, monkeypatch, role):
    """Workers start together and connect in any order: the hello's `worker_id` (from
    `BARCA_WORKER_ID`) pairs the connection with the process the coordinator started."""
    path = str(tmp_path / "protocol.sock")
    monkeypatch.setenv("BARCA_SOCKET", path)
    monkeypatch.setenv("BARCA_WORKER_ID", "5")
    monkeypatch.setattr(_runtime, "_socket", None)
    seen = []
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(path)
        listener.listen(1)
        listener.settimeout(5)

        def coordinator():
            peer, _ = listener.accept()
            with peer:
                seen.append(receive(peer))
                send(
                    peer,
                    {"type": "hello", "role": "coordinator", "protocol_version": _runtime.PROTOCOL_VERSION},
                )

        thread = threading.Thread(target=coordinator)
        thread.start()
        try:
            _runtime.connect(role=role)
        finally:
            _runtime.disconnect()
            thread.join(6)
    expected = {"type": "hello", "role": role, "protocol_version": _runtime.PROTOCOL_VERSION}
    if role == "worker":
        expected["worker_id"] = 5
    assert seen == [expected]


@pytest.mark.parametrize("helper", ["worker", "transfer"])
@pytest.mark.parametrize("version", [3, 99])
def test_coordinator_rejects_and_reaps_wrong_version_helper(tmp_path, helper, version):
    import os
    import signal
    import subprocess
    from barca.api import _find_binary

    package = tmp_path / "barca"
    package.mkdir()
    (package / "__init__.py").write_text("")
    (package / f"_{helper}.py").write_text(f"""
import json, os, socket, struct, time
from pathlib import Path
Path("helper-pid").write_text(str(os.getpid()))
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["BARCA_SOCKET"])
data = json.dumps({{"type": "hello", "role": {helper!r}, "protocol_version": {version}}}).encode()
s.sendall(struct.pack(">I", len(data)) + data)
time.sleep(60)
""")
    (tmp_path / "p.py").write_text(
        'from barca import asset\n@asset()\ndef value():\n    open("step-ran", "w").write("unexpected")\n    return 42\n'
    )
    env = {**os.environ, "BARCA_REMOTE": "off"}
    if helper == "transfer":
        env.pop("BARCA_REMOTE")
        env.update(BARCA_REMOTE_URI=str(tmp_path / "store"))
        (tmp_path / "store").mkdir()
    result = subprocess.run(
        [_find_binary(), "get", "value", "p.py", "--json"],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 3, result.stderr
    assert f"{helper} protocol v{version}, coordinator expects v{_runtime.PROTOCOL_VERSION}" in result.stderr
    assert "reinstall barca" in result.stderr
    assert not (tmp_path / "step-ran").exists()
    pid = int((tmp_path / "helper-pid").read_text())
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        pass
    else:
        os.kill(pid, signal.SIGKILL)
        pytest.fail("rejected helper was left alive")
