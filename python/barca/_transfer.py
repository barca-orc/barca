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
       optional "sha256": the hash recorded for the artifact. A local copy with that hash
       is kept as it is; any other is replaced by the store's copy.
  → {"type": "probe", "id", "root"}             is the store holding `root` there and listable?
       Replies "done" only when its bucket, container or root directory positively answers a
       listing; anything else is an "error". Nothing is ever created by a probe.
  → {"type": "shutdown"}                        finish in-flight work, exit
  ← {"type": "done", "id", "size_bytes", "sha256", "fetched", "mismatch"}
       "sha256" is the local file's; "fetched" is false when a get left the local file as it
       was; "mismatch" is true when the store's copy does not have the recorded hash. That
       is not an error: an artifact path is `{node}/{run_hash}`, so a refresh or a second
       machine computing the same step overwrites it, and the store's copy is still used.
  ← {"type": "error", "id", "message", "attempts", "missing"}
                                                final — transient errors are retried here;
                                                a stalled attempt fails after
                                                BARCA_TRANSFER_TIMEOUT seconds.
       "missing" is true when the source does not exist (for a get: the object is not in the
       store). That alone does not say the store is there: a deleted bucket answers the same
       way. The coordinator recomputes such a cached result only after a "probe" succeeded.

Transfers go through barca._storage, so credentials and BARCA_STORAGE_OPTIONS
behave exactly as they do for workers and the state helper.
"""

import hashlib
import os
import signal
import socket
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from barca import _lifeline, _runtime, _storage

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
    # A local directory in the way that cannot be moved: retrying changes nothing.
    _storage.ArtifactPathError,
)


_http_status = _storage.http_status


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


def _sha256(path: "str | Path") -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _local_sha256(path: "str | Path") -> str | None:
    """The hash of a local copy, or None when there is no readable file to hash.

    An unreadable copy is then replaced like any other that does not match.
    """
    try:
        return _sha256(path)
    except OSError:
        return None


def _staged_get(remote: str, local: str, expected: str | None) -> dict:
    """Make `local` the store's copy of `remote`, through a temp file renamed into place.

    Returns the reply fields. The store's copy is used even when it does not have the
    `expected` hash; the caller is told so it can warn. A local file that already holds the
    store's bytes is left untouched. A directory at `local` is not an artifact: it is moved
    out of the way, never deleted (`_storage.make_way`).
    """
    dest = Path(local)
    # Before anything is staged beside it: if the directory cannot be moved, that is the
    # error to report, not a temp file that could not be created next to it.
    _storage.make_way(dest)
    with _storage.staged_beside(dest) as tmp:
        _storage.get_file(remote, tmp)
        digest = _sha256(tmp)
        mismatch = expected is not None and digest != expected
        fetched = not (mismatch and _local_sha256(dest) == digest)
        if fetched:
            _storage.make_way(dest)
            os.replace(tmp, dest)
        return {"sha256": digest, "fetched": fetched, "mismatch": mismatch}


def _transfer(msg: dict) -> dict:
    """Perform one request; for a put/get return the reply fields describing the local file."""
    if msg["type"] == "probe":
        _storage.check_store(msg["root"])
        return {"size_bytes": 0, "fetched": False, "mismatch": False}
    local = msg["local"]
    if msg["type"] == "put":
        _storage.put_file(local, msg["remote"])
        result = {"sha256": _sha256(local), "fetched": True, "mismatch": False}
    else:
        expected = msg.get("sha256")
        if expected is not None and _local_sha256(local) == expected:
            result = {"sha256": expected, "fetched": False, "mismatch": False}
        else:
            result = _staged_get(msg["remote"], local, expected)
    return {"size_bytes": os.stat(local).st_size, **result}


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
            requests.resolve({"type": "done", "id": req_id, **_transfer(msg)})
            return
        except Exception as exc:
            if _is_permanent(exc) or attempt > retries:
                requests.resolve(_error(msg, exc, attempt))
                return
            time.sleep(backoff * 2 ** (attempt - 1))


def _is_missing(exc: BaseException) -> bool:
    """True when the transfer failed because its source does not exist.

    s3fs, adlfs and gcsfs raise FileNotFoundError for an object that is not there; an SDK
    error that stays untranslated is judged by its HTTP status. Everything else (permissions,
    authentication, a store that cannot be reached) is not "missing": the object may well be
    there.
    """
    return isinstance(exc, FileNotFoundError) or _http_status(exc) == 404


def _error(msg: dict, exc: BaseException, attempts: int) -> dict:
    return {
        "type": "error",
        "id": msg["id"],
        "message": _storage.safe_error(f"{type(exc).__name__}: {exc}"),
        "attempts": attempts,
        "missing": _is_missing(exc),
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
            if kind in ("put", "get", "probe"):
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


def _stop(signum, frame) -> None:
    """Asked to stop mid-transfer: leave no half-written temp file behind, then exit."""
    _storage.discard_staged()
    os._exit(128 + signum)


def main() -> int:
    if not os.environ.get("BARCA_SOCKET"):
        print("BARCA_SOCKET not set", file=sys.stderr)
        return 1
    # What Ctrl-C means for the run is the coordinator's decision alone: it cancels the run
    # and stops this helper (SIGTERM). Acting on an interrupt here as well would end the
    # helper under a coordinator that is still waiting on it, and print a KeyboardInterrupt
    # traceback. The coordinator starts this process in a group of its own, which the
    # terminal's Ctrl-C does not reach; a SIGINT sent to it directly is ignored too.
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    # Outside the terminal's foreground group, a write to the terminal (a warning on stderr)
    # would stop the process if the terminal is set to `tostop`. Ignored, the write goes
    # through.
    signal.signal(signal.SIGTTOU, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, _stop)
    # Deaf to Ctrl-C, so this process must notice by itself when the coordinator is gone.
    _lifeline.watch()
    try:
        sock = _runtime.connect()
    except OSError as exc:
        # No socket to connect to. If the coordinator has gone (it failed, or was cancelled,
        # before it ever used this helper) there is nobody to tell: exit without a word.
        if _lifeline.coordinator_gone(wait=1.0):
            return 0
        print(
            f"[barca] transfer helper: cannot reach the coordinator at "
            f"{os.environ['BARCA_SOCKET']}: {type(exc).__name__}: {exc}",
            file=sys.stderr,
        )
        return 1
    assert sock is not None
    serve(
        sock,
        concurrency=_env_int("BARCA_TRANSFER_CONCURRENCY", _DEFAULT_CONCURRENCY),
        retries=_env_int("BARCA_TRANSFER_RETRIES", _DEFAULT_RETRIES),
        timeout=_env_float("BARCA_TRANSFER_TIMEOUT", _DEFAULT_TIMEOUT),
    )
    _runtime.disconnect()
    # Whatever was still in flight is abandoned (the coordinator disconnected, or an attempt
    # timed out): leave no temp file of it.
    _storage.discard_staged()
    # Exit without joining pool threads: a timed-out attempt may be stuck in
    # a network call that would otherwise keep the process alive.
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(0)


if __name__ == "__main__":
    sys.exit(main())
