"""Artifact transfer helper — moves artifact files between local disk and the store.

Spawned once per run by the Rust coordinator (crates/barca-core/src/transfer.rs)
when the artifact store is not the local artifact directory. Workers only ever
read and write local files; this process uploads finished artifacts in the
background and fetches cached artifacts from other machines, so object-store
latency never sits on a step's critical path.

Protocol: the coordinator's Unix socket (BARCA_SOCKET), length-prefixed JSON
frames as in barca._runtime. Requests may be in flight concurrently; each
reply carries the request id.

  → {"type": "put", "id", "local", "remote"}    upload local → remote
  → {"type": "get", "id", "remote", "local"}    download remote → local (atomic)
  → {"type": "shutdown"}                        finish in-flight work, exit
  ← {"type": "done", "id", "size_bytes"}
  ← {"type": "error", "id", "message", "attempts"}
                                                final — transient errors are retried here;
                                                a stalled attempt fails after
                                                BARCA_TRANSFER_TIMEOUT seconds

Transfers go through barca._storage, so credentials and BARCA_STORAGE_OPTIONS
behave exactly as they do for workers and the state helper.
"""

import os
import socket
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from barca import _runtime, _storage

_DEFAULT_CONCURRENCY = 4
_DEFAULT_RETRIES = 3
_DEFAULT_BACKOFF = 0.5
# Per-attempt limit, from when the attempt starts. Generous: it exists to
# turn a stalled connection into an error, not to bound large transfers.
_DEFAULT_TIMEOUT = 600.0

# Failures no retry can fix: missing objects/files, auth, bad config.
_PERMANENT = (
    FileNotFoundError,
    PermissionError,
    IsADirectoryError,
    NotADirectoryError,
    ValueError,
    TypeError,
    ImportError,
)


def _http_status(exc: BaseException) -> int | None:
    """The HTTP status a cloud SDK attached to its error, if any.

    azure.core's HttpResponseError carries `status_code`; gcsfs and
    google.api_core errors carry `code`; requests-style errors carry
    `response.status_code`.
    """
    for value in (
        getattr(exc, "status_code", None),
        getattr(exc, "code", None),
        getattr(getattr(exc, "response", None), "status_code", None),
    ):
        if value is None:
            continue
        try:
            status = int(value)
        except (TypeError, ValueError):
            continue
        if 100 <= status <= 599:
            return status
    return None


def _is_permanent(exc: BaseException) -> bool:
    """True for failures no retry can fix.

    Builtin OSError subclasses cover what s3fs and adlfs translate (missing
    object, permission denied). SDK errors that stay untranslated — e.g.
    Azure's HttpResponseError for a bad account key — are judged by HTTP
    status: 4xx is the client's fault and permanent, except 408 (request
    timeout) and 429 (throttled).
    """
    if isinstance(exc, _PERMANENT):
        return True
    status = _http_status(exc)
    return status is not None and 400 <= status < 500 and status not in (408, 429)


def _staged_get(remote: str, local: str) -> None:
    """Download into a temp file beside `local`, then rename into place."""
    dest = Path(local)
    dest.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=dest.parent, prefix=f".{dest.name}.", suffix=".tmp")
    os.close(fd)
    try:
        _storage.get_file(remote, tmp)
        os.replace(tmp, dest)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def _transfer(msg: dict) -> int:
    """Perform one put/get; return the transferred file's size."""
    if msg["type"] == "put":
        _storage.put_file(msg["local"], msg["remote"])
        return os.stat(msg["local"]).st_size
    _staged_get(msg["remote"], msg["local"])
    return os.stat(msg["local"]).st_size


class _Requests:
    """Tracks every accepted request until exactly one reply has been sent.

    A request is resolved either by its worker thread (done/error) or by the
    watchdog when an attempt exceeds the timeout. Python threads cannot be
    killed, so a timed-out attempt is abandoned: it may keep running, but its
    result is discarded and it is never retried. Whoever resolves first
    replies; the other side finds the request gone and stays silent.
    """

    def __init__(self, timeout: float | None):
        self.timeout = timeout
        self._lock = threading.Lock()
        self._idle = threading.Condition(self._lock)
        # id -> (attempt number, attempt start time); absent until it starts.
        self._running: dict[int, tuple[int, float]] = {}
        self._open: set[int] = set()

    def accept(self, req_id: int) -> None:
        with self._lock:
            self._open.add(req_id)

    def start_attempt(self, req_id: int, attempt: int) -> bool:
        """Mark an attempt as started; False if the request was already resolved."""
        with self._lock:
            if req_id not in self._open:
                return False
            self._running[req_id] = (attempt, time.monotonic())
            return True

    def resolve(self, reply: dict) -> None:
        """Send `reply` unless the request was already resolved."""
        with self._lock:
            req_id = reply["id"]
            if req_id not in self._open:
                return
            self._open.discard(req_id)
            self._running.pop(req_id, None)
            _send(reply)
            self._idle.notify_all()

    def expire(self) -> None:
        """Fail every attempt that has run past the timeout."""
        if self.timeout is None:
            return
        now = time.monotonic()
        with self._lock:
            stalled = [
                (rid, attempt)
                for rid, (attempt, started) in self._running.items()
                if now - started > self.timeout
            ]
        for rid, attempt in stalled:
            self.resolve(
                {
                    "type": "error",
                    "id": rid,
                    "message": f"TimeoutError: transfer attempt made no progress "
                    f"within {self.timeout:g}s",
                    "attempts": attempt,
                }
            )

    def wait_idle(self) -> None:
        """Block until every accepted request has been replied to."""
        with self._lock:
            while self._open:
                self._idle.wait(0.1)

    def abandon_all(self) -> None:
        with self._lock:
            self._open.clear()
            self._running.clear()
            self._idle.notify_all()


def _send(reply: dict) -> None:
    try:
        _runtime.send_message(reply)
    except OSError:
        pass  # coordinator is gone; nothing to report to


def _handle(msg: dict, requests: _Requests, retries: int, backoff: float) -> None:
    req_id = msg["id"]
    attempt = 0
    while True:
        attempt += 1
        if not requests.start_attempt(req_id, attempt):
            return  # timed out (or abandoned) meanwhile: never retried
        try:
            size = _transfer(msg)
            requests.resolve({"type": "done", "id": req_id, "size_bytes": size})
            return
        except Exception as exc:
            if _is_permanent(exc) or attempt > retries:
                requests.resolve(_error(msg, exc, attempt))
                return
            time.sleep(backoff * 2 ** (attempt - 1))


def _error(msg: dict, exc: BaseException, attempts: int) -> dict:
    return {
        "type": "error",
        "id": msg["id"],
        "message": f"{type(exc).__name__}: {exc}",
        "attempts": attempts,
    }


def _watchdog(requests: _Requests, stop: threading.Event) -> None:
    assert requests.timeout is not None
    tick = min(1.0, max(requests.timeout / 4, 0.02))
    while not stop.wait(tick):
        requests.expire()


def serve(
    sock: socket.socket,
    *,
    concurrency: int = _DEFAULT_CONCURRENCY,
    retries: int = _DEFAULT_RETRIES,
    backoff: float = _DEFAULT_BACKOFF,
    timeout: float | None = _DEFAULT_TIMEOUT,
) -> None:
    """Serve transfer requests on `sock` until shutdown or disconnect.

    `timeout` bounds each attempt, measured from when it starts (not from
    when the request was queued behind others).
    """
    _runtime._socket = sock
    requests = _Requests(timeout)
    stop = threading.Event()
    if timeout is not None:
        threading.Thread(
            target=_watchdog, args=(requests, stop), name="barca-xfer-watchdog", daemon=True
        ).start()
    pool = ThreadPoolExecutor(max_workers=max(1, concurrency), thread_name_prefix="barca-xfer")
    try:
        while True:
            try:
                msg = _runtime.recv_message()
            except (RuntimeError, OSError):
                # Coordinator exited: abandon queued work.
                requests.abandon_all()
                return
            kind = msg.get("type")
            if kind == "shutdown":
                # Wait for replies, not threads: an abandoned (timed-out)
                # attempt may still be stuck, and must not hold up exit.
                requests.wait_idle()
                return
            if kind in ("put", "get"):
                requests.accept(msg["id"])
                pool.submit(_handle, msg, requests, retries, backoff)
    finally:
        stop.set()
        pool.shutdown(wait=False, cancel_futures=True)


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    return int(raw) if raw else default


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    return float(raw) if raw else default


def main() -> int:
    if not os.environ.get("BARCA_SOCKET"):
        print("BARCA_SOCKET not set", file=sys.stderr)
        return 1
    sock = _runtime.connect()
    assert sock is not None
    serve(
        sock,
        concurrency=_env_int("BARCA_TRANSFER_CONCURRENCY", _DEFAULT_CONCURRENCY),
        retries=_env_int("BARCA_TRANSFER_RETRIES", _DEFAULT_RETRIES),
        timeout=_env_float("BARCA_TRANSFER_TIMEOUT", _DEFAULT_TIMEOUT),
    )
    _runtime.disconnect()
    # Exit without joining pool threads: a timed-out attempt may be stuck in
    # a network call that would otherwise keep the process alive.
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(0)


if __name__ == "__main__":
    sys.exit(main())
