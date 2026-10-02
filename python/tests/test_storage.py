"""Tests for barca._storage — scheme dispatch, URI helpers, remote round-trips.

Remote behavior is exercised against fsspec's in-memory filesystem
(memory://), which is a process-global singleton — these tests must run
in-process, never through a spawned worker subprocess.
"""

import builtins
from pathlib import Path

import pytest

from barca import _storage


@pytest.fixture(autouse=True)
def _clean_memory_fs():
    """Reset the memory filesystem and the fs cache between tests."""
    yield
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


# ─── Scheme detection ────────────────────────────────────────────────────────


class TestIsRemote:
    def test_plain_path(self):
        assert _storage.is_remote("/data/out.parquet") is False

    def test_relative_path(self):
        assert _storage.is_remote("out.parquet") is False

    def test_file_uri_is_local(self):
        assert _storage.is_remote("file:///data/out.parquet") is False

    def test_abfss(self):
        assert _storage.is_remote("abfss://cont@acct.dfs.core.windows.net/x") is True

    def test_s3(self):
        assert _storage.is_remote("s3://bucket/key.pkl") is True

    def test_gs(self):
        assert _storage.is_remote("gs://bucket/key.json") is True

    def test_memory(self):
        assert _storage.is_remote("memory://arts/x.json") is True

    def test_windows_style_colon_not_a_scheme(self):
        # No "://" — not a URI.
        assert _storage.is_remote("C:label.txt") is False


class TestGetFs:
    def test_local_path_rejected(self):
        with pytest.raises(ValueError, match="local path"):
            _storage.get_fs("/data/x.json")

    def test_unknown_scheme(self):
        with pytest.raises(ValueError, match="Unsupported storage scheme 'ftp://'"):
            _storage.get_fs("ftp://host/x.json")

    def test_memory_fs(self):
        fs = _storage.get_fs("memory://arts/x.json")
        assert fs.protocol == "memory" or "memory" in fs.protocol

    def test_cached_per_protocol(self):
        assert _storage.get_fs("memory://a/x") is _storage.get_fs("memory://b/y")

    def test_missing_driver_names_extra(self, monkeypatch):
        """abfss:// whose driver import fails → error names barca[azure].

        Patches the fsspec construction to raise ImportError directly rather
        than depending on adlfs being uninstalled — the test extra installs
        every backend client, so absence can't be assumed."""
        import fsspec

        _storage._fs_cache.pop("abfs", None)

        def boom(protocol, **kwargs):
            raise ImportError("No module named 'adlfs'")

        monkeypatch.setattr(fsspec, "filesystem", boom)
        with pytest.raises(ImportError, match=r"barca\[azure\]"):
            _storage.get_fs("abfss://cont@acct.dfs.core.windows.net/x.parquet")

    def test_missing_fsspec_names_extra(self, monkeypatch):
        _storage._fs_cache.pop("s3", None)
        real_import = builtins.__import__

        def fake_import(name, *args, **kwargs):
            if name == "fsspec" or name.startswith("fsspec."):
                raise ImportError("No module named 'fsspec'")
            return real_import(name, *args, **kwargs)

        monkeypatch.setattr(builtins, "__import__", fake_import)
        with pytest.raises(ImportError, match=r"barca\[s3\]"):
            _storage.get_fs("s3://bucket/x.pkl")


# ─── URI helpers ─────────────────────────────────────────────────────────────


class TestJoin:
    def test_local_returns_path(self, tmp_path):
        p = _storage.join(tmp_path, "x.json")
        assert p == tmp_path / "x.json"

    def test_uri_join(self):
        assert (
            _storage.join("abfss://cont@acct.dfs.core.windows.net/prefix", "x.parquet")
            == "abfss://cont@acct.dfs.core.windows.net/prefix/x.parquet"
        )

    def test_uri_join_trailing_slash(self):
        assert _storage.join("s3://bucket/prefix/", "x.pkl") == "s3://bucket/prefix/x.pkl"

    def test_uri_double_slash_preserved(self):
        # Proof that pathlib is never applied to URIs (Path would mangle "//").
        joined = _storage.join("memory://arts", "x.json")
        assert joined == "memory://arts/x.json"
        assert "://" in joined


class TestSuffix:
    def test_local(self):
        assert _storage.suffix("/data/out.parquet") == ".parquet"

    def test_uri(self):
        assert _storage.suffix("abfss://cont@acct.dfs.core.windows.net/a/b/model.pkl") == ".pkl"

    def test_uri_with_query_ignored(self):
        assert _storage.suffix("s3://bucket/a/data.json?versionId=3") == ".json"

    def test_no_extension(self):
        assert _storage.suffix("s3://bucket/a/README") == ""

    def test_hidden_file_no_extension(self):
        assert _storage.suffix("/data/.hidden") == ""


# ─── Storage options ─────────────────────────────────────────────────────────


class TestStorageOptions:
    def test_unset(self, monkeypatch):
        monkeypatch.delenv("BARCA_STORAGE_OPTIONS", raising=False)
        assert _storage.storage_options("abfs") == {}

    def test_keyed_by_protocol(self, monkeypatch):
        monkeypatch.setenv(
            "BARCA_STORAGE_OPTIONS",
            '{"abfs": {"account_name": "myacct"}, "s3": {"anon": true}}',
        )
        assert _storage.storage_options("abfs") == {"account_name": "myacct"}
        assert _storage.storage_options("s3") == {"anon": True}
        assert _storage.storage_options("gcs") == {}

    def test_invalid_json(self, monkeypatch):
        monkeypatch.setenv("BARCA_STORAGE_OPTIONS", "{not json")
        with pytest.raises(ValueError, match="not valid JSON"):
            _storage.storage_options("abfs")

    def test_non_object(self, monkeypatch):
        monkeypatch.setenv("BARCA_STORAGE_OPTIONS", "[1, 2]")
        with pytest.raises(ValueError, match="JSON object"):
            _storage.storage_options("abfs")


# ─── Remote round-trip on memory:// ──────────────────────────────────────────


class TestRemoteRoundTrip:
    def test_put_get_exists_size(self, tmp_path):
        src = tmp_path / "payload.bin"
        src.write_bytes(b"x" * 1024)

        dest = "memory://arts/payload.bin"
        assert _storage.exists(dest) is False

        _storage.put_file(src, dest)
        assert _storage.exists(dest) is True
        assert _storage.size(dest) == 1024

        back = tmp_path / "back.bin"
        _storage.get_file(dest, back)
        assert back.read_bytes() == b"x" * 1024

    def test_local_exists_and_size(self, tmp_path):
        f = tmp_path / "x.json"
        f.write_text("{}")
        assert _storage.exists(f) is True
        assert _storage.exists(tmp_path / "missing.json") is False
        assert _storage.size(f) == 2


# ─── Local store fallback (plain path / file:// artifact roots) ─────────────


class TestLocalStoreTransfer:
    """put_file/get_file also serve a plain-path or file:// store root, so a
    shared local/NFS directory works as the artifact store with no fsspec."""

    def test_put_file_plain_path_creates_parents(self, tmp_path):
        src = tmp_path / "src.bin"
        src.write_bytes(b"abc")
        dest = tmp_path / "store" / "node" / "hash.bin"
        _storage.put_file(src, str(dest))
        assert dest.read_bytes() == b"abc"

    def test_put_file_file_uri(self, tmp_path):
        src = tmp_path / "src.bin"
        src.write_bytes(b"abc")
        dest = tmp_path / "store" / "a.bin"
        _storage.put_file(src, f"file://{dest}")
        assert dest.read_bytes() == b"abc"

    def test_get_file_plain_path_and_file_uri(self, tmp_path):
        src = tmp_path / "store" / "a.bin"
        src.parent.mkdir()
        src.write_bytes(b"xyz")
        out1 = tmp_path / "out1.bin"
        out2 = tmp_path / "out2.bin"
        _storage.get_file(str(src), out1)
        _storage.get_file(f"file://{src}", out2)
        assert out1.read_bytes() == b"xyz"
        assert out2.read_bytes() == b"xyz"

    def test_get_file_missing_local_raises(self, tmp_path):
        with pytest.raises(FileNotFoundError):
            _storage.get_file(str(tmp_path / "nope.bin"), tmp_path / "out.bin")

    def test_put_file_local_does_not_import_fsspec(self, tmp_path, monkeypatch):
        real_import = builtins.__import__

        def guarded(name, *args, **kwargs):
            if name == "fsspec" or name.startswith("fsspec."):
                raise AssertionError("fsspec imported for a local store")
            return real_import(name, *args, **kwargs)

        monkeypatch.setattr(builtins, "__import__", guarded)
        src = tmp_path / "s"
        src.write_bytes(b"1")
        _storage.put_file(src, str(tmp_path / "d" / "s"))

    def test_local_path_of(self, tmp_path):
        assert _storage.local_path_of("/a/b") == Path("/a/b")
        assert _storage.local_path_of("file:///a/b") == Path("/a/b")
        assert _storage.local_path_of("s3://bucket/k") is None


# ─── Thread safety ───────────────────────────────────────────────────────────


class TestGetFsConcurrency:
    def test_concurrent_get_fs_constructs_one_filesystem(self, monkeypatch):
        """The transfer helper calls get_fs from a thread pool; construction
        must happen once per protocol, not once per racing thread."""
        import threading

        import fsspec

        calls = []
        real = fsspec.filesystem
        gate = threading.Barrier(8)

        def slow_filesystem(protocol, **kw):
            calls.append(protocol)
            import time

            time.sleep(0.05)
            return real(protocol, **kw)

        monkeypatch.setattr(fsspec, "filesystem", slow_filesystem)
        _storage._fs_cache.pop("memory", None)

        results = []

        def worker():
            gate.wait()
            results.append(_storage.get_fs("memory://x"))

        threads = [threading.Thread(target=worker) for _ in range(8)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert calls == ["memory"]
        assert len({id(fs) for fs in results}) == 1
