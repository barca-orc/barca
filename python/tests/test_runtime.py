"""Tests for the barca._runtime module — socket communication layer.

Tests cover:
  - Framing: encode/decode round-trip via a local socket pair
  - is_worker() returns False without BARCA_SOCKET env var
  - submit_and_wait with a mock executor socket server
  - parallel() standalone fallback with nested calls
  - High-level emit helpers (step_completed, step_error, blocked, heartbeat)
"""

import json
import os
import socket
import struct
import tempfile
import threading
from functools import partial
from unittest.mock import patch

import pytest


# ─── Framing round-trip ──────────────────────────────────────────────────────


class TestFraming:
    """Test the length-prefixed JSON framing protocol."""

    def _make_socketpair(self):
        """Create a connected Unix socket pair for testing."""
        s1, s2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        return s1, s2

    def _frame_encode(self, msg: dict) -> bytes:
        """Encode a message using the protocol framing."""
        payload = json.dumps(msg).encode("utf-8")
        return struct.pack(">I", len(payload)) + payload

    def _frame_decode(self, data: bytes) -> dict:
        """Decode a framed message."""
        length = struct.unpack(">I", data[:4])[0]
        payload = data[4 : 4 + length]
        return json.loads(payload)

    def test_round_trip_simple(self):
        """Simple dict round-trips through framing."""
        msg = {"type": "heartbeat"}
        encoded = self._frame_encode(msg)
        decoded = self._frame_decode(encoded)
        assert decoded == msg

    def test_round_trip_complex(self):
        """Complex nested message round-trips."""
        msg = {
            "type": "step_completed",
            "node_id": "my_module::compute[ticker=AAPL]",
            "artifact": {
                "path": "/tmp/artifacts/abc123.json",
                "format": "json",
                "size_bytes": 1024,
            },
        }
        encoded = self._frame_encode(msg)
        decoded = self._frame_decode(encoded)
        assert decoded == msg

    def test_round_trip_unicode(self):
        """Unicode content survives framing."""
        msg = {"type": "step_error", "message": "Error: \u2603 snowman \U0001f680 rocket"}
        encoded = self._frame_encode(msg)
        decoded = self._frame_decode(encoded)
        assert decoded == msg

    def test_send_recv_over_socket(self):
        """Send and receive a message over an actual socket pair."""
        from barca import _runtime

        s1, s2 = self._make_socketpair()
        try:
            # Simulate the runtime sending on s1
            # We manually set the module-level socket
            original_socket = _runtime._socket
            _runtime._socket = s1
            try:
                msg = {"type": "heartbeat"}
                _runtime.send_message(msg)

                # Read from the other end
                header = s2.recv(4)
                length = struct.unpack(">I", header)[0]
                payload = s2.recv(length)
                received = json.loads(payload)
                assert received == msg
            finally:
                _runtime._socket = original_socket
        finally:
            s1.close()
            s2.close()

    def test_recv_message_over_socket(self):
        """recv_message reads a framed message from the socket."""
        from barca import _runtime

        s1, s2 = self._make_socketpair()
        try:
            original_socket = _runtime._socket
            _runtime._socket = s1
            try:
                # Send from s2 (simulating the executor)
                msg = {"type": "parallel_response", "results": [{"status": "ok", "result": 42}]}
                payload = json.dumps(msg).encode("utf-8")
                s2.sendall(struct.pack(">I", len(payload)) + payload)

                # Read from s1 (the runtime side)
                received = _runtime.recv_message()
                assert received == msg
            finally:
                _runtime._socket = original_socket
        finally:
            s1.close()
            s2.close()

    def test_multiple_messages_in_sequence(self):
        """Multiple messages can be sent/received in sequence without corruption."""
        from barca import _runtime

        s1, s2 = self._make_socketpair()
        try:
            original_socket = _runtime._socket
            _runtime._socket = s1
            try:
                messages = [
                    {"type": "heartbeat"},
                    {"type": "step_completed", "node_id": "a"},
                    {"type": "step_completed", "node_id": "b"},
                ]
                # Send all from runtime side
                for msg in messages:
                    _runtime.send_message(msg)

                # Receive all from executor side
                for expected in messages:
                    header = s2.recv(4)
                    length = struct.unpack(">I", header)[0]
                    payload = s2.recv(length)
                    assert json.loads(payload) == expected
            finally:
                _runtime._socket = original_socket
        finally:
            s1.close()
            s2.close()

    def test_send_from_thread_while_recv_blocks(self):
        """A thread can send while another thread is parked in recv_message.

        The transfer helper's main thread sits in recv while pool threads
        send replies; a single shared lock would deadlock that.
        """
        from barca import _runtime

        s1, s2 = self._make_socketpair()
        s2.settimeout(5)
        original_socket = _runtime._socket
        _runtime._socket = s1
        try:
            received = []
            reader = threading.Thread(target=lambda: received.append(_runtime.recv_message()))
            reader.start()
            # Give the reader time to take its lock and block in recv.
            reader.join(timeout=0.1)
            assert reader.is_alive()

            sender = threading.Thread(target=_runtime.send_message, args=({"type": "done"},))
            sender.start()
            sender.join(timeout=2)
            assert not sender.is_alive(), "send blocked behind a pending recv"

            header = s2.recv(4)
            payload = s2.recv(struct.unpack(">I", header)[0])
            assert json.loads(payload) == {"type": "done"}

            # Unblock the reader.
            body = json.dumps({"type": "shutdown"}).encode()
            s2.sendall(struct.pack(">I", len(body)) + body)
            reader.join(timeout=2)
            assert received == [{"type": "shutdown"}]
        finally:
            _runtime._socket = original_socket
            s1.close()
            s2.close()

    def test_concurrent_sends_never_interleave(self):
        """Frames from many sender threads arrive whole."""
        from barca import _runtime

        s1, s2 = self._make_socketpair()
        s2.settimeout(5)
        original_socket = _runtime._socket
        _runtime._socket = s1
        try:
            big = "x" * 200_000  # bigger than a socket buffer → partial writes
            threads = [
                threading.Thread(target=_runtime.send_message, args=({"id": i, "pad": big},))
                for i in range(8)
            ]
            got = []

            def drain():
                for _ in range(8):
                    header = b""
                    while len(header) < 4:
                        header += s2.recv(4 - len(header))
                    n = struct.unpack(">I", header)[0]
                    buf = b""
                    while len(buf) < n:
                        buf += s2.recv(n - len(buf))
                    got.append(json.loads(buf)["id"])

            d = threading.Thread(target=drain)
            d.start()
            for t in threads:
                t.start()
            for t in threads:
                t.join(timeout=5)
            d.join(timeout=5)
            assert sorted(got) == list(range(8))
        finally:
            _runtime._socket = original_socket
            s1.close()
            s2.close()


# ─── is_worker() ─────────────────────────────────────────────────────────────


class TestIsWorker:
    """Test is_worker() env var detection."""

    def test_not_worker_without_env(self):
        """Without BARCA_SOCKET, is_worker() returns False."""
        from barca import _runtime

        with patch.dict(os.environ, {}, clear=False):
            # Ensure BARCA_SOCKET is not set
            os.environ.pop("BARCA_SOCKET", None)
            assert _runtime.is_worker() is False

    def test_is_worker_with_env(self):
        """With BARCA_SOCKET set, is_worker() returns True."""
        from barca import _runtime

        with patch.dict(os.environ, {"BARCA_SOCKET": "/tmp/barca_test.sock"}):
            assert _runtime.is_worker() is True


# ─── connect() ────────────────────────────────────────────────────────────────


class TestConnect:
    """Test connect() behavior."""

    def test_connect_returns_none_without_env(self):
        """connect() returns None when BARCA_SOCKET is not set."""
        from barca import _runtime

        original_socket = _runtime._socket
        _runtime._socket = None
        try:
            with patch.dict(os.environ, {}, clear=False):
                os.environ.pop("BARCA_SOCKET", None)
                result = _runtime.connect()
                assert result is None
        finally:
            _runtime._socket = original_socket

    def test_connect_with_real_socket(self):
        """connect() connects to a real Unix socket when BARCA_SOCKET is set."""
        from barca import _runtime

        original_socket = _runtime._socket
        _runtime._socket = None
        try:
            with tempfile.TemporaryDirectory() as tmpdir:
                sock_path = os.path.join(tmpdir, "test.sock")
                # Create a listening socket
                server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                server.bind(sock_path)
                server.listen(1)

                with patch.dict(os.environ, {"BARCA_SOCKET": sock_path}):
                    conn = _runtime.connect()
                    assert conn is not None
                    # Clean up
                    conn.close()
                    _runtime._socket = None

                server.close()
        finally:
            _runtime._socket = original_socket

    def test_connect_returns_cached_socket(self):
        """connect() returns the cached socket on subsequent calls."""
        from barca import _runtime

        original_socket = _runtime._socket
        sentinel = object()
        _runtime._socket = sentinel
        try:
            result = _runtime.connect()
            assert result is sentinel
        finally:
            _runtime._socket = original_socket


# ─── submit_and_wait ──────────────────────────────────────────────────────────


class TestSubmitAndWait:
    """Test submit_and_wait with a mock executor."""

    def test_submit_and_wait_success(self):
        """submit_and_wait sends items and receives results."""
        from barca import _runtime

        s1, s2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        original_socket = _runtime._socket
        _runtime._socket = s1

        try:
            items = [
                {"fn_ref": "mod:func_a", "args": [1], "kwargs": {}},
                {"fn_ref": "mod:func_b", "args": [2], "kwargs": {"x": 10}},
            ]

            # Mock executor: read the submit message, send back results
            def mock_executor():
                # Read the submit message
                header = s2.recv(4)
                length = struct.unpack(">I", header)[0]
                payload = s2.recv(length)
                request = json.loads(payload)
                assert request["type"] == "submit"
                assert len(request["items"]) == 2

                # Send back results
                response = {
                    "type": "parallel_response",
                    "results": [
                        {"status": "ok", "result": 100},
                        {"status": "ok", "result": 200},
                    ],
                }
                resp_payload = json.dumps(response).encode("utf-8")
                s2.sendall(struct.pack(">I", len(resp_payload)) + resp_payload)

            executor_thread = threading.Thread(target=mock_executor)
            executor_thread.start()

            results = _runtime.submit_and_wait(items)
            executor_thread.join(timeout=5)

            assert len(results) == 2
            assert results[0] == {"status": "ok", "result": 100}
            assert results[1] == {"status": "ok", "result": 200}
        finally:
            _runtime._socket = original_socket
            s1.close()
            s2.close()

    def test_submit_and_wait_with_errors(self):
        """submit_and_wait handles mixed success/error results."""
        from barca import _runtime

        s1, s2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        original_socket = _runtime._socket
        _runtime._socket = s1

        try:
            items = [
                {"fn_ref": "mod:ok_func", "args": [], "kwargs": {}},
                {"fn_ref": "mod:bad_func", "args": [], "kwargs": {}},
            ]

            def mock_executor():
                header = s2.recv(4)
                length = struct.unpack(">I", header)[0]
                s2.recv(length)

                response = {
                    "type": "parallel_response",
                    "results": [
                        {"status": "ok", "result": "success"},
                        {"status": "error", "error": "ValueError: boom"},
                    ],
                }
                resp_payload = json.dumps(response).encode("utf-8")
                s2.sendall(struct.pack(">I", len(resp_payload)) + resp_payload)

            executor_thread = threading.Thread(target=mock_executor)
            executor_thread.start()

            results = _runtime.submit_and_wait(items)
            executor_thread.join(timeout=5)

            assert results[0]["status"] == "ok"
            assert results[1]["status"] == "error"
            assert "boom" in results[1]["error"]
        finally:
            _runtime._socket = original_socket
            s1.close()
            s2.close()

    def test_submit_and_wait_unexpected_response_type(self):
        """submit_and_wait raises on unexpected response type."""
        from barca import _runtime

        s1, s2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        original_socket = _runtime._socket
        _runtime._socket = s1

        try:

            def mock_executor():
                header = s2.recv(4)
                length = struct.unpack(">I", header)[0]
                s2.recv(length)

                response = {"type": "unexpected_thing"}
                resp_payload = json.dumps(response).encode("utf-8")
                s2.sendall(struct.pack(">I", len(resp_payload)) + resp_payload)

            executor_thread = threading.Thread(target=mock_executor)
            executor_thread.start()

            with pytest.raises(RuntimeError, match="unexpected response type"):
                _runtime.submit_and_wait([{"fn_ref": "x:y", "args": [], "kwargs": {}}])

            executor_thread.join(timeout=5)
        finally:
            _runtime._socket = original_socket
            s1.close()
            s2.close()


# ─── High-level emit helpers ─────────────────────────────────────────────────


class TestEmitHelpers:
    """Test emit_step_completed, emit_step_error, emit_blocked, emit_heartbeat."""

    def _setup_socket_pair(self):
        """Set up a socket pair and patch _runtime._socket."""
        from barca import _runtime

        s1, s2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        original = _runtime._socket
        _runtime._socket = s1
        return s1, s2, original

    def _teardown(self, s1, s2, original):
        from barca import _runtime

        _runtime._socket = original
        s1.close()
        s2.close()

    def _read_message(self, sock):
        """Read a framed message from a socket."""
        header = sock.recv(4)
        length = struct.unpack(">I", header)[0]
        payload = sock.recv(length)
        return json.loads(payload)

    def test_emit_step_completed(self):
        from barca import _runtime

        s1, s2, original = self._setup_socket_pair()
        try:
            _runtime.emit_step_completed(
                "my_node",
                {"path": "/tmp/art.json", "format": "json", "size_bytes": 42},
            )
            msg = self._read_message(s2)
            assert msg["type"] == "step_completed"
            assert msg["node_id"] == "my_node"
            assert msg["artifact"]["format"] == "json"
        finally:
            self._teardown(s1, s2, original)

    def test_emit_step_error(self):
        from barca import _runtime

        s1, s2, original = self._setup_socket_pair()
        try:
            _runtime.emit_step_error(
                node_id="failing_node",
                error_type="ValueError",
                message="something broke",
                traceback="Traceback...\nValueError: something broke",
                elapsed=1.5,
            )
            msg = self._read_message(s2)
            assert msg["type"] == "step_error"
            assert msg["node_id"] == "failing_node"
            assert msg["error_type"] == "ValueError"
            assert msg["message"] == "something broke"
            assert msg["elapsed"] == 1.5
        finally:
            self._teardown(s1, s2, original)

    def test_emit_blocked(self):
        from barca import _runtime

        s1, s2, original = self._setup_socket_pair()
        try:
            _runtime.emit_blocked("downstream_node", "upstream 'data' unavailable")
            msg = self._read_message(s2)
            assert msg["type"] == "blocked"
            assert msg["node_id"] == "downstream_node"
            assert "upstream" in msg["reason"]
        finally:
            self._teardown(s1, s2, original)

    def test_emit_heartbeat(self):
        from barca import _runtime

        s1, s2, original = self._setup_socket_pair()
        try:
            _runtime.emit_heartbeat()
            msg = self._read_message(s2)
            assert msg == {"type": "heartbeat"}
        finally:
            self._teardown(s1, s2, original)


# ─── parallel() standalone fallback ──────────────────────────────────────────


class TestParallelStandalone:
    """Test parallel() when NOT running inside a barca worker (standalone mode)."""

    def test_standalone_executes_sequentially(self):
        """parallel() falls back to sequential execution without BARCA_SOCKET."""
        from barca import parallel

        def add(x, y):
            return x + y

        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BARCA_SOCKET", None)
            # Reset _runtime._socket to ensure standalone mode
            from barca import _runtime

            original = _runtime._socket
            _runtime._socket = None
            try:
                results = parallel(partial(add, 1, 2), partial(add, 3, 4))
                assert results == [3, 7]
            finally:
                _runtime._socket = original

    def test_standalone_error_handling(self):
        """Errors in standalone mode produce ParallelError objects."""
        from barca import ParallelError, parallel

        def fail():
            raise ValueError("standalone boom")

        def succeed():
            return "ok"

        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BARCA_SOCKET", None)
            from barca import _runtime

            original = _runtime._socket
            _runtime._socket = None
            try:
                results = parallel(partial(succeed), partial(fail))
                assert results[0] == "ok"
                assert isinstance(results[1], ParallelError)
                assert "boom" in str(results[1])
            finally:
                _runtime._socket = original

    def test_standalone_empty(self):
        """parallel() with no args returns [] in standalone mode."""
        from barca import parallel

        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BARCA_SOCKET", None)
            from barca import _runtime

            original = _runtime._socket
            _runtime._socket = None
            try:
                assert parallel() == []
            finally:
                _runtime._socket = original

    def test_standalone_nested_parallel(self):
        """Nested parallel() calls work in standalone mode (sequential fallback)."""
        from barca import parallel

        def inner_work(x):
            return x * 2

        def outer_work(items):
            # This calls parallel() recursively
            return parallel(*(partial(inner_work, i) for i in items))

        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BARCA_SOCKET", None)
            from barca import _runtime

            original = _runtime._socket
            _runtime._socket = None
            try:
                results = parallel(
                    partial(outer_work, [1, 2, 3]),
                    partial(outer_work, [4, 5]),
                )
                assert results[0] == [2, 4, 6]
                assert results[1] == [8, 10]
            finally:
                _runtime._socket = original

    def test_standalone_parallel_map(self):
        """parallel_map works in standalone mode."""
        from barca import parallel_map

        def double(x):
            return x * 2

        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BARCA_SOCKET", None)
            from barca import _runtime

            original = _runtime._socket
            _runtime._socket = None
            try:
                results = parallel_map(double, [1, 2, 3, 4, 5])
                assert results == [2, 4, 6, 8, 10]
            finally:
                _runtime._socket = original

    def test_non_partial_raises_typeerror(self):
        """parallel() rejects non-partial callables."""
        from barca import parallel

        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("BARCA_SOCKET", None)
            from barca import _runtime

            original = _runtime._socket
            _runtime._socket = None
            try:
                with pytest.raises(TypeError, match="functools.partial"):
                    parallel(lambda: 1)
            finally:
                _runtime._socket = original


# ─── Heartbeat thread ─────────────────────────────────────────────────────────


class TestHeartbeat:
    """Test the heartbeat background thread."""

    def test_heartbeat_sends_messages(self):
        """start_heartbeat sends periodic heartbeats until stopped."""
        from barca import _runtime

        s1, s2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        original_socket = _runtime._socket
        original_thread = _runtime._heartbeat_thread
        _runtime._socket = s1
        _runtime._heartbeat_thread = None
        _runtime._heartbeat_stop.clear()

        try:
            _runtime.start_heartbeat(interval=0.05)

            # Wait for at least one heartbeat
            s2.settimeout(2.0)
            header = s2.recv(4)
            length = struct.unpack(">I", header)[0]
            payload = s2.recv(length)
            msg = json.loads(payload)
            assert msg == {"type": "heartbeat"}

            _runtime.stop_heartbeat()
            # Give the thread time to stop
            import time

            time.sleep(0.1)
        finally:
            _runtime._socket = original_socket
            _runtime._heartbeat_thread = original_thread
            _runtime._heartbeat_stop.clear()
            s1.close()
            s2.close()


# ─── disconnect() ─────────────────────────────────────────────────────────────


class TestDisconnect:
    """Test disconnect cleanup."""

    def test_disconnect_closes_socket(self):
        """disconnect() closes the socket and resets to None."""
        from barca import _runtime

        s1, s2 = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        original_socket = _runtime._socket
        _runtime._socket = s1

        try:
            _runtime.disconnect()
            assert _runtime._socket is None
            # Verify the socket is actually closed
            with pytest.raises(OSError):
                s1.send(b"test")
        finally:
            _runtime._socket = original_socket
            s2.close()

    def test_disconnect_when_not_connected(self):
        """disconnect() is safe to call when not connected."""
        from barca import _runtime

        original_socket = _runtime._socket
        _runtime._socket = None
        try:
            _runtime.disconnect()  # Should not raise
            assert _runtime._socket is None
        finally:
            _runtime._socket = original_socket
