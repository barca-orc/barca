"""`_storage.get_fs` builds one filesystem per protocol, even when first called from many threads."""

import threading

import pytest

pytest.importorskip("fsspec")

from barca import _storage  # noqa: E402


def test_get_fs_from_many_threads_builds_one_instance(monkeypatch):
    import fsspec

    built = []
    real = fsspec.filesystem

    def slow_filesystem(protocol, **kw):
        built.append(protocol)
        threading.Event().wait(0.05)  # widen the race window
        return real(protocol, skip_instance_cache=True, **kw)

    monkeypatch.setattr(fsspec, "filesystem", slow_filesystem)
    monkeypatch.setattr(_storage, "_fs_cache", {})

    n = 16
    barrier = threading.Barrier(n)
    got = []

    def worker():
        barrier.wait()
        got.append(_storage.get_fs("memory://bucket/x"))

    threads = [threading.Thread(target=worker) for _ in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    assert len(got) == n
    assert len({id(fs) for fs in got}) == 1
    assert built == ["memory"]


def test_one_line():
    assert _storage.one_line(ValueError("first\nsecond")) == "first"
    assert _storage.one_line(RuntimeError("")) == "RuntimeError"
