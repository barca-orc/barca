"""Barca runtime — socket communication with the Rust executor.

When running inside a barca worker (BARCA_SOCKET env var set), all
communication goes through a Unix domain socket using length-prefixed
JSON frames. When running standalone, operations fall back to local
execution.
"""

import json
import os
import socket
import struct
import threading

try:
    import orjson  # ty: ignore[unresolved-import]
except ImportError:
    orjson = None  # ty: ignore[invalid-assignment]


# ─── Socket connection ────────────────────────────────────────────────────────

_socket: socket.socket | None = None
# Separate locks: whole frames never interleave on either direction, and a
# thread parked in recv_message never blocks senders (heartbeats, or the
# transfer helper's pool threads replying while its main thread reads).
_send_lock = threading.Lock()
_recv_lock = threading.Lock()


def connect() -> socket.socket | None:
    """Connect to the executor's Unix socket (once per worker process).
    Returns None if not running inside a barca worker.
    """
    global _socket
    if _socket is not None:
        return _socket

    path = os.environ.get("BARCA_SOCKET")
    if not path:
        return None

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    _socket = sock
    return _socket


def is_worker() -> bool:
    """Are we running inside a barca worker process?"""
    return os.environ.get("BARCA_SOCKET") is not None


# ─── Framing ──────────────────────────────────────────────────────────────────


def send_message(msg: dict) -> None:
    """Send a length-prefixed JSON message to the executor."""
    assert _socket is not None, "send_message called before connect()"
    with _send_lock:
        payload = orjson.dumps(msg) if orjson else json.dumps(msg).encode("utf-8")
        header = struct.pack(">I", len(payload))
        _socket.sendall(header + payload)


def recv_message() -> dict:
    """Read a length-prefixed JSON message from the executor (blocks)."""
    with _recv_lock:
        header = _recv_exact(4)
        if not header:
            raise RuntimeError("executor disconnected")
        length = struct.unpack(">I", header)[0]
        payload = _recv_exact(length)
        if not payload:
            raise RuntimeError("executor disconnected mid-message")
        return orjson.loads(payload) if orjson else json.loads(payload)


def _recv_exact(n: int) -> bytes | None:
    """Read exactly n bytes from the socket."""
    assert _socket is not None, "_recv_exact called before connect()"
    data = b""
    while len(data) < n:
        chunk = _socket.recv(n - len(data))
        if not chunk:
            return None
        data += chunk
    return data


# ─── High-level protocol ─────────────────────────────────────────────────────


def emit_step_completed(node_id: str, artifact: dict) -> None:
    """Report a step completed successfully."""
    send_message(
        {
            "type": "step_completed",
            "node_id": node_id,
            "artifact": artifact,
        }
    )


def emit_step_error(
    node_id: str, error_type: str, message: str, traceback: str, elapsed: float
) -> None:
    """Report a step failed."""
    send_message(
        {
            "type": "step_error",
            "node_id": node_id,
            "error_type": error_type,
            "message": message,
            "traceback": traceback,
            "elapsed": elapsed,
        }
    )


def emit_blocked(node_id: str, reason: str) -> None:
    """Report a step was blocked."""
    send_message(
        {
            "type": "blocked",
            "node_id": node_id,
            "reason": reason,
        }
    )


def emit_heartbeat() -> None:
    """Send a heartbeat."""
    send_message({"type": "heartbeat"})


def submit_and_wait(work_items: list[dict]) -> list[dict]:
    """Submit work items to the executor and block until all complete.

    This is the core primitive for parallel(). Sends a Submit message,
    then blocks reading the socket for the ParallelResponse.

    Args:
        work_items: List of {"fn_ref": "mod:func", "args": [...], "kwargs": {...}}

    Returns:
        List of {"status": "ok", "result": ...} or {"status": "error", "error": "..."}
    """
    send_message(
        {
            "type": "submit",
            "items": work_items,
        }
    )
    # Block until executor sends back the results
    response = recv_message()
    if response.get("type") != "parallel_response":
        raise RuntimeError(f"unexpected response type: {response.get('type')}")
    return response.get("results", [])


# ─── Heartbeat thread ─────────────────────────────────────────────────────────

_heartbeat_thread: threading.Thread | None = None
_heartbeat_stop = threading.Event()


def start_heartbeat(interval: float = 5.0) -> None:
    """Start a background thread that sends heartbeats every `interval` seconds."""
    global _heartbeat_thread
    if _heartbeat_thread is not None:
        return

    def _loop():
        while not _heartbeat_stop.is_set():
            try:
                emit_heartbeat()
            except Exception:
                break
            _heartbeat_stop.wait(interval)

    _heartbeat_thread = threading.Thread(target=_loop, daemon=True)
    _heartbeat_thread.start()


def stop_heartbeat() -> None:
    """Stop the heartbeat thread."""
    _heartbeat_stop.set()


# ─── Cleanup ──────────────────────────────────────────────────────────────────


def disconnect() -> None:
    """Close the socket connection."""
    global _socket
    stop_heartbeat()
    if _socket is not None:
        try:
            _socket.close()
        except Exception:
            pass
        _socket = None
