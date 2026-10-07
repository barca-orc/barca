"""Lazy inputs read in place against real object-store clients (#234).

test_remote_lazy.py pins the behavior on a fake store. This runs the same reads through adlfs,
s3fs and gcsfs against the local emulators the state-backend suite uses (MinIO, fake-gcs-server,
Azurite), each skipped when its emulator is unreachable.
"""

import pytest

from barca import _artifacts, _storage

from . import emulators
from .test_state_backends import AzureBackend, GcsBackend, S3Backend

duckdb = pytest.importorskip("duckdb")
pl = pytest.importorskip("polars")
pytest.importorskip("pyarrow")


@pytest.fixture(params=[S3Backend(), GcsBackend(), AzureBackend()], ids=lambda b: b.id)
def remote(request, tmp_path, monkeypatch):
    """A fresh remote artifact URI holding an 8-column parquet file; yields (uri, size)."""
    be = request.param
    emulators.require(be.id, be.available())
    for k, v in be.env().items():
        monkeypatch.setenv(k, v)
    # Emulator only: skip gcsfs's gRPC bucket-layout probe, which fake-gcs cannot answer.
    monkeypatch.setenv("GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT", "false")
    monkeypatch.chdir(tmp_path)
    _storage._fs_cache.clear()
    _storage._range_fs_cache.clear()

    base = be.make_uri(tmp_path).rsplit("/state/", 1)[0]
    uri = f"{base}/arts/orders/h1.parquet"
    duckdb.sql(
        """copy (select i::bigint as a, 'x' || (i % 1000) as b, random() as c, random() as d,
                        random() as e, random() as f, random() as g, random() as h
                 from range(200000) t(i))
           to 'src.parquet' (format parquet, row_group_size 10000)"""
    )
    _storage.put_file("src.parquet", uri)

    def no_download(*args, **kwargs):
        raise AssertionError("a lazy input must not download the whole object")

    monkeypatch.setattr(_storage, "get_file", no_download)
    yield uri, _storage.size(uri)
    _storage._fs_cache.clear()
    _storage._range_fs_cache.clear()


def _count_reads(monkeypatch, uri) -> list[int]:
    """Count the bytes the store's read handles return (exact ranges: what crossed the wire)."""
    inner = _storage.get_fs(uri)
    fetched = [0]
    real_open = inner.open

    def counting_open(path, mode="rb", **kwargs):
        f = real_open(path, mode, **kwargs)
        real_read = f.read

        def read(length=-1):
            data = real_read(length)
            fetched[0] += len(data)
            return data

        f.read = read
        return f

    monkeypatch.setattr(inner, "open", counting_open)
    return fetched


def test_duckdb_input_reads_one_column_in_place(remote, monkeypatch):
    uri, size = remote
    fetched = _count_reads(monkeypatch, uri)
    rel = _artifacts.deserialize(uri, "parquet", frame_type="duckdb")
    assert rel.aggregate("sum(a)").fetchone()[0] == sum(range(200000))
    assert 0 < fetched[0] < size * 0.25, f"fetched {fetched[0]} of {size} bytes"


def test_polars_lazy_input_reads_one_column_in_place(remote, monkeypatch):
    uri, size = remote
    fetched = _count_reads(monkeypatch, uri)
    lf = _artifacts.deserialize(uri, "parquet", frame_type="polars_lazy")
    assert lf.select(pl.col("a").sum()).collect().item() == sum(range(200000))
    assert 0 < fetched[0] < size * 0.25, f"fetched {fetched[0]} of {size} bytes"
