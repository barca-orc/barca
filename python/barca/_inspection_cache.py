"""Disposable, bounded inspection evidence; execution and ledger validation never use it.

Local results are tied to device/inode/size/mtime/ctime, checked before and after reading.
Remote observations live for at most five minutes without extending on hits. Cache loss,
corruption, permission failures and concurrent eviction only cause another observation.
"""

from __future__ import annotations

from contextlib import contextmanager
import hashlib
import json
import os
import sys
from pathlib import Path
from stat import S_ISREG
import tempfile
import threading
import time
from typing import Callable

MAX_BYTES = 8 * 1024 * 1024
MAX_ENTRIES = 8192
MAX_VALUE_BYTES = 64 * 1024
REMOTE_SECONDS = 300


def fingerprint(path: str | Path) -> list[int] | None:
    try:
        stat = Path(path).stat()
        if not S_ISREG(stat.st_mode):
            return None
        return [stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns]
    except OSError:
        return None


def _checksum(row: dict) -> str:
    content = {key: row.get(key) for key in ("stamp", "checked", "value", "subject")}
    return hashlib.sha256(
        json.dumps(content, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def _valid(row: dict) -> bool:
    try:
        return isinstance(row.get("value"), dict) and row.get("checksum") == _checksum(row)
    except (TypeError, ValueError, RecursionError):
        return False


class Cache:
    def __init__(self, path: str | None, *, clock: Callable[[], float] = time.time):
        self.path = Path(path) if path else None
        self.clock = clock
        self.mutex = threading.Lock()
        selected = {
            name: value
            for name, value in os.environ.items()
            if name.startswith(("AWS_", "AZURE_", "GOOGLE_", "FSSPEC_", "GCSFS_"))
            or name in ("BARCA_STORAGE_OPTIONS", "PYTHONPATH", "STORAGE_EMULATOR_HOST")
        }
        credential_files = {
            name: fingerprint(os.environ[name])
            for name in (
                "GOOGLE_APPLICATION_CREDENTIALS",
                "AWS_SHARED_CREDENTIALS_FILE",
                "AWS_CONFIG_FILE",
            )
            if name in os.environ
        }
        # This is hashed into keys, never persisted as credential text.
        self.scope = json.dumps([selected, credential_files, sys.executable], sort_keys=True)
        self.invalidations: dict[str, float] = {}
        self.entries = self._read()
        self.pending: dict[str, dict] = {}
        self.sizes = {
            key: len(json.dumps(row).encode()) + len(key) for key, row in self.entries.items()
        }
        self.bytes = sum(self.sizes.values()) + len(json.dumps(self.invalidations).encode())

    def _read(self) -> dict:
        self.invalidations = {}
        if self.path is None:
            return {}
        try:
            fd = os.open(self.path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd, "rb") as source:
                raw = source.read(MAX_BYTES + 1)
            if len(raw) > MAX_BYTES:
                return {}
            value = json.loads(raw)
            entries = value.get("entries")
            invalidations = value.get("invalidations", {})
            if (
                value.get("version") != 1
                or not isinstance(entries, dict)
                or not isinstance(invalidations, dict)
                or len(entries) + len(invalidations) > MAX_ENTRIES
                or any(not isinstance(at, (int, float)) for at in invalidations.values())
            ):
                return {}
            self.invalidations = invalidations
            return {key: row for key, row in entries.items() if isinstance(row, dict)}
        except (OSError, ValueError, TypeError, AttributeError, RecursionError):
            return {}

    def _key(self, kind: str, path: str, options: object) -> str:
        text = json.dumps([kind, path, options, self.scope], sort_keys=True)
        return hashlib.sha256(text.encode()).hexdigest()

    def _remember(
        self,
        key: str,
        stamp: list[int] | None,
        value: dict,
        *,
        subject: str | None = None,
        checked: float | None = None,
    ) -> None:
        try:
            encoded = json.dumps(value, default=str).encode()
            if len(encoded) > MAX_VALUE_BYTES:
                return
            value = json.loads(encoded)
        except (TypeError, ValueError, RecursionError):
            return
        if isinstance(value, dict) and "note" in value:
            return
        row = {
            "stamp": stamp,
            "checked": self.clock() if checked is None else checked,
            "value": value,
            "subject": subject,
        }
        row["checksum"] = _checksum(row)
        with self.mutex:
            self.bytes -= self.sizes.pop(key, 0)
            self.entries.pop(key, None)
            self.entries[key] = row
            self.pending[key] = row
            self.sizes[key] = len(json.dumps(row).encode()) + len(key)
            self.bytes += self.sizes[key]
            while self.entries and (
                self.bytes > MAX_BYTES or len(self.entries) + len(self.invalidations) > MAX_ENTRIES
            ):
                oldest = next(iter(self.entries))
                self.entries.pop(oldest)
                self.pending.pop(oldest, None)
                self.bytes -= self.sizes.pop(oldest)

    def local(
        self, kind: str, path: str | Path, options: object, compute: Callable[[], dict]
    ) -> dict:
        path = str(Path(path).absolute())
        before = fingerprint(path)
        key = self._key(kind, path, options)
        with self.mutex:
            row = self.entries.get(key, {})
        if before is not None and row.get("stamp") == before and _valid(row):
            return row["value"]
        value = compute()
        if before is not None and fingerprint(path) == before:
            self._remember(key, before, value)
        return value

    def remote(self, kind: str, uri: str, options: object, compute: Callable[[], dict]) -> dict:
        key = self._key(kind, uri, options)
        subject = hashlib.sha256(uri.encode()).hexdigest()
        epoch = fingerprint(str(self.path) + ".epoch") if self.path else None
        with self.mutex:
            row = self.entries.get(key, {})
        checked = row.get("checked")
        started = self.clock()
        age = started - checked if isinstance(checked, (int, float)) else -1
        if (
            0 <= age < REMOTE_SECONDS
            and row.get("stamp") == epoch
            and checked > self.invalidations.get(subject, float("-inf"))
            and _valid(row)
        ):
            return row["value"]
        value = compute()
        self._remember(key, epoch, value, subject=subject, checked=started)
        return value

    @contextmanager
    def _lock(self):
        import fcntl

        assert self.path is not None
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(str(self.path) + ".lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "a+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            yield

    def _bump_epoch(self) -> None:
        assert self.path is not None
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=self.path.parent, delete=False) as temporary:
            name = temporary.name
            temporary.write(os.urandom(16))
        try:
            os.replace(name, str(self.path) + ".epoch")
        finally:
            if os.path.exists(name):
                os.unlink(name)

    def _publish(self, entries: dict) -> None:
        assert self.path is not None
        if len(self.invalidations) > MAX_ENTRIES // 2:
            self.invalidations = dict(
                sorted(self.invalidations.items(), key=lambda item: item[1])[-(MAX_ENTRIES // 2) :]
            )
            self._bump_epoch()  # Forgotten barriers must not revive old observations.
        ordered = sorted(
            entries.items(),
            key=lambda item: item[1].get("checked", 0)
            if isinstance(item[1].get("checked"), (int, float))
            else 0,
        )
        keep = MAX_ENTRIES - len(self.invalidations)
        ordered = ordered[-keep:] if keep else []
        while True:
            raw = json.dumps(
                {"version": 1, "entries": dict(ordered), "invalidations": self.invalidations},
                separators=(",", ":"),
            ).encode()
            if len(raw) <= MAX_BYTES:
                break
            if ordered:
                ordered = ordered[max(1, len(ordered) // 8) :]
            else:
                self.invalidations.pop(next(iter(self.invalidations)))
                self._bump_epoch()
        name = None
        try:
            with tempfile.NamedTemporaryFile(dir=self.path.parent, delete=False) as temporary:
                name = temporary.name
                temporary.write(raw)
            os.replace(name, self.path)
        finally:
            if name and os.path.exists(name):
                os.unlink(name)

    def flush(self) -> None:
        if self.path is None or not self.pending:
            return
        try:
            with self._lock():
                entries = self._read()
                for key, row in self.pending.items():
                    if row["checked"] > self.invalidations.get(row.get("subject"), float("-inf")):
                        entries[key] = row
                self._publish(entries)
            self.pending.clear()
        except OSError:
            pass

    def invalidate_remote(self, uri: str) -> None:
        if self.path is None:
            return
        subject = hashlib.sha256(uri.encode()).hexdigest()
        try:
            with self._lock():
                entries = self._read()
                self.invalidations[subject] = self.clock()
                self._publish(
                    {key: row for key, row in entries.items() if row.get("subject") != subject}
                )
        except BlockingIOError:
            try:
                self._bump_epoch()  # Conservative nonblocking fallback under contention.
            except OSError:
                pass
        except OSError:
            pass


class TransferObserver:
    """Invalidate only inspected objects; new uncached uploads do no cache-file I/O."""

    def __init__(self, path: str):
        self.path = path
        self.stamp = None
        self.subjects: set[str] = set()
        self.mutex = threading.Lock()

    def changed(self, uri: str) -> None:
        try:
            subject = hashlib.sha256(uri.encode()).hexdigest()
            with self.mutex:
                stamp = fingerprint(self.path)
                if stamp != self.stamp:
                    cache = Cache(self.path)
                    self.subjects = {
                        row.get("subject")
                        for row in cache.entries.values()
                        if isinstance(row.get("subject"), str)
                    }
                    self.stamp = stamp
                if subject in self.subjects:
                    Cache(self.path).invalidate_remote(uri)
                    self.stamp = None
        except (OSError, TypeError, ValueError, RecursionError):
            pass  # Advisory cache failure must not turn a confirmed upload into a failure.
