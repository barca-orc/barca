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


def test_cloud_evidence_rechecks_when_the_local_materialization_changes(tmp_path):
    import fsspec
    from barca import _state

    fs = fsspec.filesystem("memory")
    uri = "memory://inspection-revision/value"
    local = tmp_path / "value.json"
    cache = str(tmp_path / "cache.json")
    local.write_text("42")
    fs.pipe(uri, b"42")
    assert _state.inspect_copies([uri], [str(local)], cache)["objects"][0]["size_bytes"] == 2
    local.write_text("4242")
    fs.pipe(uri, b"4242")
    assert _state.inspect_copies([uri], [str(local)], cache)["objects"][0]["size_bytes"] == 4


def test_copy_based_upload_repair_invalidates_cloud_evidence_without_changing_local_file(
    tmp_path, monkeypatch
):
    import shutil
    import fsspec
    from barca import _state, _transfer

    fs = fsspec.filesystem("memory")
    uri = "memory://inspection-repair/value"
    local = tmp_path / "value.json"
    local.write_text("42")
    cache = str(tmp_path / "cache.json")
    fs.pipe(uri, b"4242")
    assert _state.inspect_copies([uri], [str(local)], cache)["objects"][0]["size_bytes"] == 4
    monkeypatch.setattr(
        _transfer, "_pin_file", lambda source, target: shutil.copyfile(source, target)
    )
    _transfer._transfer(
        {"type": "put", "local": str(local), "remote": uri}, memo.TransferObserver(cache)
    )
    assert _state.inspect_copies([uri], [str(local)], cache)["objects"][0]["size_bytes"] == 2


def test_transfer_invalidation_preserves_unrelated_observations_and_blocks_late_writers(tmp_path):
    path = str(tmp_path / "cache.json")
    cache = memo.Cache(path)
    cache.remote("store", "one", None, lambda: {"size": 1})
    cache.remote("store", "two", None, lambda: {"size": 2})
    cache.flush()
    late = memo.Cache(path)

    def old_observation():
        memo.TransferObserver(path).changed("one")
        return {"type": "old"}

    late.remote("shape", "one", None, old_observation)
    late.flush()
    fresh = memo.Cache(path)
    assert fresh.remote("store", "one", None, lambda: {"size": 3}) == {"size": 3}
    assert fresh.remote("shape", "one", None, lambda: {"type": "new"}) == {"type": "new"}

    def unexpected():
        raise AssertionError("an unrelated remote object was unnecessarily rechecked")

    assert fresh.remote("store", "two", None, unexpected) == {"size": 2}


def test_busy_invalidation_uses_a_nonblocking_remote_epoch_without_losing_local_evidence(tmp_path):
    import fcntl

    path = str(tmp_path / "cache.json")
    local = tmp_path / "value"
    local.write_text("42")
    cache = memo.Cache(path)
    cache.remote("store", "one", None, lambda: {"size": 1})
    cache.local("shape", local, None, lambda: {"type": "int"})
    cache.flush()
    with (tmp_path / "cache.json.lock").open("a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        attempt = threading.Thread(
            target=lambda: memo.TransferObserver(path).changed("one"), daemon=True
        )
        attempt.start()
        attempt.join(timeout=1)
        blocked = attempt.is_alive()
    attempt.join(timeout=2)
    assert not attempt.is_alive()
    assert not blocked
    fresh = memo.Cache(path)
    assert fresh.remote("store", "one", None, lambda: {"size": 2}) == {"size": 2}

    def unexpected():
        raise AssertionError("remote invalidation re-read unchanged local evidence")

    assert fresh.local("shape", local, None, unexpected) == {"type": "int"}


def test_uncached_uploads_do_not_create_or_rewrite_an_inspection_cache(tmp_path):
    path = tmp_path / "cache.json"
    observer = memo.TransferObserver(str(path))
    for number in range(100):
        observer.changed(f"memory://unused/{number}")
    assert not list(tmp_path.iterdir())


def test_pruning_invalidation_barriers_cannot_revive_a_late_observation(tmp_path, monkeypatch):
    monkeypatch.setattr(memo, "MAX_ENTRIES", 4)
    monkeypatch.setattr(memo, "MAX_BYTES", 2048)
    path = str(tmp_path / "cache.json")
    late = memo.Cache(path)

    def old_observation():
        writer = memo.Cache(path)
        for number in range(6):
            writer.invalidate_remote(f"other-{number}")
        return {"size": 1}

    late.remote("store", "one", None, old_observation)
    late.flush()
    current = memo.Cache(path)
    assert current.remote("store", "one", None, lambda: {"size": 2}) == {"size": 2}
    data = json.loads((tmp_path / "cache.json").read_text())
    assert len(data["entries"]) + len(data["invalidations"]) <= 4
    assert (tmp_path / "cache.json").stat().st_size <= 2048


def test_equal_clock_ticks_do_not_allow_a_pre_repair_observation_to_return(tmp_path):
    path = str(tmp_path / "cache.json")
    clock = [10.0]
    late = memo.Cache(path, clock=lambda: clock[0])

    def old_observation():
        memo.Cache(path, clock=lambda: clock[0]).invalidate_remote("one")
        return {"size": 1}

    late.remote("store", "one", None, old_observation)
    late.flush()
    clock[0] = 11.0
    current = memo.Cache(path, clock=lambda: clock[0])
    assert current.remote("store", "one", None, lambda: {"size": 2}) == {"size": 2}
