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
import time

try:
    import orjson  # ty: ignore[unresolved-import]
except ImportError:
    orjson = None  # ty: ignore[invalid-assignment]


# ─── Socket connection ────────────────────────────────────────────────────────

PROTOCOL_VERSION = 5

_socket: socket.socket | None = None
# Separate locks: whole frames never interleave on either direction, and a
# thread parked in recv_message never blocks senders (heartbeats, or the
# transfer helper's pool threads replying while its main thread reads).
_send_lock = threading.Lock()
_recv_lock = threading.Lock()


def connect(role: str = "worker") -> socket.socket | None:
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
    try:
        sock.settimeout(10)
        sock.connect(path)
        _socket = sock
        hello = {"type": "hello", "role": role, "protocol_version": PROTOCOL_VERSION}
        # Workers start together and connect in any order: the id names which process this is.
        worker_id = os.environ.get("BARCA_WORKER_ID", "")
        if role == "worker" and worker_id.isdigit():
            hello["worker_id"] = int(worker_id)
        send_message(hello)
        try:
            reply = recv_message()
        except (RuntimeError, ValueError) as error:
            raise RuntimeError(
                f"invalid coordinator protocol hello: {error}; reinstall barca"
            ) from error
        if not isinstance(reply, dict):
            raise RuntimeError("invalid coordinator protocol hello; reinstall barca")
        if reply.get("type") == "protocol_error":
            raise RuntimeError(reply.get("message", "barca protocol mismatch; reinstall barca"))
        if type(reply.get("protocol_version")) is not int or (
            reply.get("type"),
            reply.get("role"),
            reply.get("protocol_version"),
        ) != (
            "hello",
            "coordinator",
            PROTOCOL_VERSION,
        ):
            raise RuntimeError(
                f"coordinator did not acknowledge {role} protocol v{PROTOCOL_VERSION}; reinstall barca"
            )
        sock.settimeout(None)
        return _socket
    except BaseException:
        sock.close()
        _socket = None
        raise


def is_worker() -> bool:
    """Are we running inside a barca worker process?"""
    return os.environ.get("BARCA_SOCKET") is not None


# ─── Framing ──────────────────────────────────────────────────────────────────


def send_message(msg: dict) -> None:
    """Send a length-prefixed JSON message to the executor."""
    assert _socket is not None, "send_message called before connect()"
    with _send_lock:
        payload = orjson.dumps(msg) if orjson else json.dumps(msg).encode("utf-8")
        if len(payload) > 256 * 1024 * 1024:
            raise ValueError(f"message too large: {len(payload)} bytes")
        header = struct.pack(">I", len(payload))
        _socket.sendall(header + payload)


def recv_message() -> dict:
    """Read a length-prefixed JSON message from the executor (blocks)."""
    with _recv_lock:
        header = _recv_exact(4)
        if not header:
            raise RuntimeError("executor disconnected")
        length = struct.unpack(">I", header)[0]
        if length > 256 * 1024 * 1024:
            raise ValueError(f"message too large: {length} bytes")
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


def _report_repeated_warnings() -> None:
    """Send the warnings this worker suppressed as repeats since the last report.

    Called ahead of every step result, so the coordinator has the counts before it can
    consider the run finished.
    """
    from barca import _dedupe

    counts = _dedupe.take()
    if counts:
        send_message({"type": "repeated_warnings", "counts": counts})


_result_batch = None


class result_batch:
    """Coalesce results; an owned metadata journal preserves completed leases on a crash."""

    def __init__(self, path):
        self.path = path
        self.messages = []
        self.stop = threading.Event()
        self.thread = None
        self.journal = None

    def __enter__(self):
        global _result_batch
        if self.path is not None:
            self.journal = open(self.path, "ab", buffering=0)
            _result_batch = self
            self.thread = threading.Thread(target=self._notify, daemon=True)
            self.thread.start()
        return self

    def _notify(self):
        notified = 0
        while not self.stop.wait(0.01):
            count = len(self.messages)
            if count != notified:
                try:
                    send_message({"type": "result_checkpoint", "completed": count})
                except OSError:
                    return
                notified = count

    def record(self, message):
        from barca import _wire

        # Regular file writes survive a worker process crash; no filesystem durability
        # beyond the lifetime of the coordinator is needed for these transient receipts.
        data = json.dumps(_wire.result_record(message)).encode("utf-8") + b"\n"
        view = memoryview(data)
        while view:
            written = self.journal.write(view)
            view = view[written:]
        self.messages.append(message)

    def __exit__(self, *_):
        global _result_batch
        if self.journal is None:
            return
        self.stop.set()
        self.thread.join()
        try:
            if self.messages:
                from barca import _wire

                send_message(_wire.encode_results(self.messages))
        finally:
            self.journal.close()
            _result_batch = None


def _emit_result(message):
    if _result_batch is not None:
        _result_batch.record(message)
    else:
        from barca import _wire

        send_message(_wire.encode_results([message]))


def emit_step_completed(node_id: str, artifact: dict) -> None:
    """Report a step completed successfully."""
    _report_repeated_warnings()
    _emit_result(
        {
            "type": "step_completed",
            "node_id": node_id,
            "artifact": artifact,
        }
    )


def emit_step_error(
    node_id: str,
    error_type: str,
    message: str,
    traceback: str,
    elapsed: float,
    import_seconds: float | None = None,
    worker_continues: bool = False,
    phase: str = "user_code",
) -> None:
    """Report a step failed.

    `worker_continues` tells the coordinator this worker is still sound and goes on with its
    batch; without it the coordinator stops the worker and starts a fresh one (#523). `phase`
    is where the failure happened (`_worker.PHASES`).
    """
    _report_repeated_warnings()
    _emit_result(
        {
            "type": "step_error",
            "node_id": node_id,
            "error_type": error_type,
            "message": message,
            "traceback": traceback,
            "elapsed": elapsed,
            "phase": phase,
            **({"import_seconds": import_seconds} if import_seconds is not None else {}),
            **({"worker_continues": True} if worker_continues else {}),
        }
    )


def emit_blocked(node_id: str, reason: str) -> None:
    """Report a step was blocked."""
    _report_repeated_warnings()
    _emit_result(
        {
            "type": "blocked",
            "node_id": node_id,
            "reason": reason,
        }
    )


def emit_heartbeat() -> None:
    """Send a heartbeat."""
    send_message({"type": "heartbeat"})


def emit_log(node_id: str, line: str, stream: str = "stdout") -> None:
    """Stream a line of user stdout or stderr captured during a step, with the time it was
    written (seconds since the epoch). Sent at once, one frame per line: a line written just
    before the process dies (a crash, os._exit) still reaches the coordinator."""
    send_message(
        {"type": "log", "node_id": node_id, "line": line, "stream": stream, "t": time.time()}
    )


# Set when a parallel() exchange with the coordinator did not complete (the request was sent
# but no answer was read, or the answer was not one). The socket's framing can no longer be
# trusted, so the worker reports its step's failure and exits instead of taking more work.
exchange_broken = False


def submit_and_wait(work_items: list[dict]) -> list[dict]:
    """Submit work items to the executor and block until all complete.

    This is the core primitive for parallel(). Sends a Submit message,
    then blocks reading the socket for the ParallelResponse.

    Args:
        work_items: List of {"fn_ref": "mod:func", "args": [...], "kwargs": {...}}

    Returns:
        One entry per item: {"status": "ok", "artifact": {"path", "format", "frame_type"?}}
        or {"status": "error", "error": "..."}. Artifact values are read from files.
    """
    global exchange_broken
    exchange_broken = True
    send_message(
        {
            "type": "submit",
            "items": work_items,
            # This worker reads results from their artifacts: send where they are, not the
            # values (crates/barca-core/src/protocol.rs, `Submit::artifact_results`).
            "artifact_results": True,
        }
    )
    # Block until executor sends back the results
    response = recv_message()
    if response.get("type") != "parallel_response":
        raise RuntimeError(f"unexpected response type: {response.get('type')}")
    exchange_broken = False
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
