"""Inspection evidence is disposable, bounded and invalidated at the correct boundaries."""

from concurrent.futures import ThreadPoolExecutor
import json
import os

import threading
from barca import _inspection_cache as memo


def test_file_evidence_persists_and_invalidates_on_replacement(tmp_path):
    artifact, database = tmp_path / "value.json", tmp_path / "cache.json"
    artifact.write_text("42")
    calls = []

    def read():
        calls.append(1)
        return {"value": artifact.read_text()}

    first = memo.Cache(str(database))
    assert first.local("shape", artifact, ["json", 0], read) == {"value": "42"}
    first.flush()
    before = database.stat().st_mtime_ns
    second = memo.Cache(str(database))
    assert second.local("shape", artifact, ["json", 0], read) == {"value": "42"}
    second.flush()
    assert calls == [1]
    assert database.stat().st_mtime_ns == before
    old = artifact.stat()
    replacement = tmp_path / "replacement"
    replacement.write_text("99")
    os.utime(replacement, ns=(old.st_atime_ns, old.st_mtime_ns))
    replacement.replace(artifact)
    assert second.local("shape", artifact, ["json", 0], read) == {"value": "99"}
    assert calls == [1, 1]
    artifact.unlink()
    artifact.write_text("77")
    assert second.local("shape", artifact, ["json", 0], read) == {"value": "77"}


def test_remote_window_does_not_slide_and_clock_rollback_reobserves(tmp_path, monkeypatch):
    now, calls = [10.0], []
    path = str(tmp_path / "cache.json")

    def observe():
        calls.append(1)
        return {"exists": True}

    cache = memo.Cache(path, clock=lambda: now[0])
    cache.remote("store", "s3://bucket/a", None, observe)
    cache.flush()
    now[0] = 309.999
    cache = memo.Cache(path, clock=lambda: now[0])
    cache.remote("store", "s3://bucket/a", None, observe)
    assert len(calls) == 1
    now[0] = 310.0
    cache.remote("store", "s3://bucket/a", None, observe)
    assert len(calls) == 2
    now[0] = 5.0
    cache.remote("store", "s3://bucket/a", None, observe)
    assert len(calls) == 3
    cache.remote("shape", "s3://bucket/a", ["json", 1], observe)
    cache.remote("shape", "s3://bucket/a", ["json", 2], observe)
    assert len(calls) == 5
    cache.flush()
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", '{"account_name":"other"}')
    memo.Cache(path, clock=lambda: now[0]).remote("store", "s3://bucket/a", None, observe)
    assert len(calls) == 6


def test_mutating_during_a_read_does_not_publish_reusable_evidence(tmp_path):
    artifact = tmp_path / "value"
    artifact.write_text("before")
    cache = memo.Cache(str(tmp_path / "cache.json"))

    def unstable():
        value = artifact.read_text()
        artifact.write_text("after")
        return {"value": value}

    assert cache.local("shape", artifact, None, unstable) == {"value": "before"}
    assert not cache.pending
    assert cache.local("shape", artifact, None, lambda: {"value": artifact.read_text()}) == {
        "value": "after"
    }


def test_corruption_eviction_and_concurrent_writers_cause_cache_misses_only(tmp_path, monkeypatch):
    database = tmp_path / "cache.json"
    database.write_text("broken")
    one, two = memo.Cache(str(database)), memo.Cache(str(database))
    one.remote("store", "one", None, lambda: {"size": 1})
    two.remote("store", "two", None, lambda: {"size": 2})
    one.flush()
    two.flush()
    saved = json.loads(database.read_text())
    assert len(saved["entries"]) == 2
    first_key = next(iter(saved["entries"]))
    saved["entries"][first_key]["value"] = {"size": 999}
    database.write_text(json.dumps(saved))
    repaired = memo.Cache(str(database))
    assert repaired.remote("store", "one", None, lambda: {"size": 3}) == {"size": 3}
    monkeypatch.setattr(memo, "MAX_BYTES", 2048)
    monkeypatch.setattr(memo, "MAX_ENTRIES", 4)
    monkeypatch.setattr(memo, "MAX_VALUE_BYTES", 512)
    cache = memo.Cache(str(database))
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(
            pool.map(
                lambda n: cache.remote("shape", str(n), None, lambda: {"value": "x" * 300}),
                range(64),
            )
        )
    assert len(cache.entries) <= 4
    assert cache.bytes <= 2048
    cache.flush()
    assert database.stat().st_size <= 2048
    assert len(json.loads(database.read_text())["entries"]) <= 4
    assert database.stat().st_mode & 0o777 == 0o600


def test_busy_cache_lock_never_blocks_inspection(tmp_path):
    import fcntl

    database = tmp_path / "cache.json"
    cache = memo.Cache(str(database))
    cache.remote("store", "one", None, lambda: {"exists": True})
    with (tmp_path / "cache.json.lock").open("a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        attempt = threading.Thread(target=cache.flush, daemon=True)
        attempt.start()
        attempt.join(timeout=1)
        blocked = attempt.is_alive()
        assert not database.exists()
    attempt.join(timeout=2)
    assert not attempt.is_alive()
    assert not blocked, "inspection cache writes must skip a busy lock"
    cache.flush()
    assert database.exists()


def test_errors_and_oversized_shapes_are_not_cached(tmp_path):
    cache = memo.Cache(str(tmp_path / "cache.json"))
    cache.remote("shape", "one", None, lambda: {"note": "unavailable"})
    cache.remote("shape", "two", None, lambda: {"rows": "x" * memo.MAX_VALUE_BYTES})
    assert not cache.pending


def test_cloud_inspection_window_never_caches_strict_ledger_checks(tmp_path, monkeypatch):
    import fsspec
    from barca import _state, _storage

    fs = fsspec.filesystem("memory")
    uri = "memory://inspection-window/value"
    fs.pipe(uri, b"42")
    calls = []
    original_info = fs.info

    def info(path):
        calls.append(path)
        return original_info(path)

    monkeypatch.setattr(fs, "info", info)
    monkeypatch.setattr(_storage, "get_fs", lambda _uri: fs)
    actual_cache = memo.Cache
    now = [10.0]
    monkeypatch.setattr(memo, "Cache", lambda path: actual_cache(path, clock=lambda: now[0]))
    cache = str(tmp_path / "cache.json")
    missing = [str(tmp_path / "no-local-copy")]
    assert _state.inspect_copies([uri], missing, cache)["objects"][0]["exists"]
    fs.rm(uri)
    before = len(calls)
    assert _state.inspect_copies([uri], missing, cache)["objects"][0]["exists"]
    assert len(calls) == before
    assert not _state.stat([uri])["objects"][0]["exists"]
    assert len(calls) == before + 1
    now[0] = 310.0
    assert not _state.inspect_copies([uri], missing, cache)["objects"][0]["exists"]
    assert len(calls) == before + 2
