"""How barca uploads a file to an object store (#1007): the S3 part size and the number of
blocks or parts in flight that barca chose, for artifacts and sinks alike.

The option tests run anywhere. The emulator tests (python/tests/emulators.py decides whether
they skip or fail) upload a mid-size file to MinIO, which must go in parts, and a sink's large
file to Azurite; both must read back identical.
"""

import hashlib
import json
import os
import socket
import uuid
from urllib.parse import urlsplit

import pytest

from barca import _sinks, _storage

from . import emulators

MIB = 1 << 20


@pytest.fixture(autouse=True)
def _fresh_fs_cache(monkeypatch):
    monkeypatch.setattr(_storage, "_fs_cache", {})
    monkeypatch.delenv("BARCA_STORAGE_OPTIONS", raising=False)


# ─── The options barca passes ────────────────────────────────────────────────


def test_the_chosen_values():
    """The values the #1007 sweep chose; a change to them needs a new measurement."""
    assert _storage.AZURE_CONCURRENCY == 64
    assert _storage.S3_CONCURRENCY == 10
    assert (_storage.S3_PART_MIN, _storage.S3_PART_MAX) == (5 * MIB, 50 * MIB)


@pytest.mark.parametrize("uri", ["abfs://c/a/b.parquet", "abfss://c@acct.dfs.core.windows.net/b"])
def test_azure_sends_64_blocks_at_a_time(uri):
    assert _storage.upload_options(uri, 100 * MIB) == {"max_concurrency": 64}


@pytest.mark.parametrize(
    ("size_mib", "part_mib"),
    [
        (1, 5),
        (9, 5),
        (50, 5),
        (51, 6),
        (64, 7),
        (100, 10),
        (128, 13),
        (400, 40),
        (500, 50),
        (2000, 50),
    ],
)
def test_s3_parts_are_one_round_of_ten_between_5_and_50_mib(size_mib, part_mib):
    for uri in ("s3://bucket/key", "s3a://bucket/key"):
        assert _storage.upload_options(uri, size_mib * MIB) == {
            "max_concurrency": 10,
            "chunksize": part_mib * MIB,
        }


def test_s3_parts_grow_so_a_huge_file_fits_in_10000():
    size = 1024 * 1024 * MIB  # 1 TiB: 50 MiB parts would be 20,972 of them
    part = _storage.upload_options("s3://b/k", size)["chunksize"]
    assert part % MIB == 0
    assert -(-size // part) <= _storage.S3_MAX_PARTS


def test_a_file_under_10_mib_is_one_s3_request():
    """s3fs sends one PutObject below twice the part size; the smallest part is 5 MiB."""
    part = _storage.upload_options("s3://b/k", 10 * MIB - 1)["chunksize"]
    assert 10 * MIB - 1 < 2 * part


@pytest.mark.parametrize("uri", ["gs://b/k", "gcs://b/k", "memory://k"])
def test_other_stores_upload_as_their_library_does(uri):
    assert _storage.upload_options(uri, 100 * MIB) == {}


def test_a_configured_concurrency_is_used_instead(monkeypatch):
    monkeypatch.setenv(
        "BARCA_STORAGE_OPTIONS",
        json.dumps({"s3": {"max_concurrency": 4}, "abfs": {"max_concurrency": 32}}),
    )
    assert _storage.upload_options("abfs://c/k", MIB) == {"max_concurrency": 32}
    # With 4 at a time, a 64 MiB file takes 16 MiB parts to stay one round.
    assert _storage.upload_options("s3://b/k", 64 * MIB) == {
        "max_concurrency": 4,
        "chunksize": 16 * MIB,
    }


def test_the_azure_client_keeps_the_sdk_sizes(monkeypatch):
    """Which Azure files go in blocks stays the SDK's choice (over 64 MiB, 4 MiB blocks) until a
    run against real Azure decides it (#1007)."""
    pytest.importorskip("adlfs")
    conn = (
        "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey="
        + "a" * 88
        + ";BlobEndpoint=http://127.0.0.1:1/devstoreaccount1;"
    )
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", json.dumps({"abfs": {"connection_string": conn}}))
    config = _storage.get_fs("abfs://c/k").service_client._config
    assert (config.max_single_put_size, config.max_block_size) == (64 * MIB, 4 * MIB)


class _RecordingFs:
    def __init__(self):
        self.calls = []

    def makedirs(self, *a, **k):
        pass

    def put_file(self, lpath, rpath, **kwargs):
        self.calls.append(("put_file", lpath, rpath, kwargs))

    def put(self, lpaths, rpaths, **kwargs):
        self.calls.append(("put", lpaths, rpaths, kwargs))

    def invalidate_cache(self, *a):
        pass

    def find(self, *a, **k):
        return []

    def _strip_protocol(self, uri):
        return uri.split("://", 1)[-1]


def test_put_file_passes_the_options_for_its_size(tmp_path, monkeypatch):
    fs = _RecordingFs()
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    src = tmp_path / "a.bin"
    src.write_bytes(b"x" * (64 * MIB))
    _storage.put_file(src, "s3://bucket/a.bin")
    assert fs.calls == [
        ("put_file", str(src), "s3://bucket/a.bin", {"max_concurrency": 10, "chunksize": 7 * MIB})
    ]


def test_a_sink_passes_the_options_for_its_largest_file(tmp_path, monkeypatch):
    """adlfs uploads each file of a multi-file put one block at a time unless told otherwise."""
    fs = _RecordingFs()
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    stage = tmp_path / "stage"
    (stage / "k=1").mkdir(parents=True)
    (stage / "k=1" / "small.parquet").write_bytes(b"x")
    (stage / "k=1" / "big.parquet").write_bytes(b"x" * (100 * MIB))
    target = _sinks._RemoteTarget("abfs://c/exports/scored")
    target.swap_in(stage, "", None)
    (call,) = [c for c in fs.calls if c[0] == "put"]
    assert call[3] == {"max_concurrency": 64}

    fs.calls.clear()
    _sinks._RemoteTarget("s3://b/exports/scored").swap_in(stage, "", None)
    (call,) = [c for c in fs.calls if c[0] == "put"]
    assert call[3] == {"max_concurrency": 10, "chunksize": 10 * MIB}


def test_a_sinks_large_azure_files_share_the_64_blocks(tmp_path, monkeypatch):
    """A sink sends its files at once: four files over the SDK's 64 MiB single-request limit
    get 16 blocks each, so the sink holds no more blocks in memory than one file would."""
    fs = _RecordingFs()
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    stage = tmp_path / "stage"
    stage.mkdir()
    for i in range(4):
        with open(stage / f"big-{i}.parquet", "wb") as f:
            f.truncate(65 * MIB)  # sparse: the size is what counts
    (stage / "small.parquet").write_bytes(b"x")
    _sinks._RemoteTarget("abfs://c/exports/scored").swap_in(stage, "", None)
    (call,) = [c for c in fs.calls if c[0] == "put"]
    assert call[3] == {"max_concurrency": 16}
    assert _storage.upload_options("abfs://c/k", 65 * MIB, blocked_files=100) == {
        "max_concurrency": 1
    }


# ─── Against the emulators ───────────────────────────────────────────────────

S3_ENDPOINT = os.environ.get("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
AZURITE_HOST = os.environ.get("BARCA_TEST_AZURITE_HOST", "127.0.0.1:9210")
AZURITE_KEY = (
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="
)
AZURITE_CONN = (
    f"DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey={AZURITE_KEY};"
    f"BlobEndpoint=http://{AZURITE_HOST}/devstoreaccount1;"
)


def _reachable(url: str) -> bool:
    parts = urlsplit(url)
    try:
        with socket.create_connection((parts.hostname, parts.port or 80), timeout=0.5):
            return True
    except OSError:
        return False


@pytest.fixture
def midsize(tmp_path):
    """24 MiB of random bytes: one PutObject for s3fs before #1007."""
    path = tmp_path / "mid.bin"
    data = os.urandom(24 * MIB)
    path.write_bytes(data)
    return path, hashlib.sha256(data).hexdigest()


def _sha(path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def test_a_midsize_file_goes_to_s3_in_parts(midsize, tmp_path, monkeypatch):
    emulators.require("s3", _reachable(S3_ENDPOINT), S3_ENDPOINT)
    pytest.importorskip("s3fs")
    options = {"key": "minioadmin", "secret": "minioadmin", "endpoint_url": S3_ENDPOINT}
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", json.dumps({"s3": options}))
    bucket = f"barca-test-{uuid.uuid4().hex[:12]}"
    fs = _storage.get_fs(f"s3://{bucket}")
    fs.mkdir(bucket)
    src, digest = midsize
    _storage.put_file(src, f"s3://{bucket}/a/mid.bin")

    # A multipart object's ETag ends in the number of parts: 24 MiB in 5 MiB parts is 5.
    etag = fs.info(f"{bucket}/a/mid.bin")["ETag"].strip('"')
    assert etag.endswith("-5"), etag
    back = tmp_path / "back.bin"
    _storage.get_file(f"s3://{bucket}/a/mid.bin", back)
    assert _sha(back) == digest


def test_a_sinks_large_file_goes_to_azure_in_blocks(tmp_path, monkeypatch):
    """A sink is a multi-file put, which adlfs sends one block at a time unless told. The block
    list is the same either way; this checks the SDK takes barca's concurrency for a sink and
    the file arrives whole (the unit test above checks that 64 is what barca passes)."""
    emulators.require("abfs", _reachable(f"http://{AZURITE_HOST}"), AZURITE_HOST)
    pytest.importorskip("adlfs")
    from azure.storage.blob import BlobServiceClient

    monkeypatch.setenv(
        "BARCA_STORAGE_OPTIONS", json.dumps({"abfs": {"connection_string": AZURITE_CONN}})
    )
    container = f"barca-test-{uuid.uuid4().hex[:12]}"
    _storage.get_fs(f"abfs://{container}").mkdir(container)
    stage = tmp_path / "stage"
    (stage / "day=1").mkdir(parents=True)
    data = os.urandom(68 * MIB)  # over the SDK's 64 MiB single-request limit
    (stage / "day=1" / "part-0.parquet").write_bytes(data)
    (stage / "day=1" / "small.parquet").write_bytes(b"x")
    _sinks._RemoteTarget(f"abfs://{container}/exports").swap_in(stage, "", None)

    blob = BlobServiceClient.from_connection_string(AZURITE_CONN).get_blob_client(
        container, "exports/day=1/part-0.parquet"
    )
    committed, _ = blob.get_block_list("committed")
    assert [b.size for b in committed] == [4 * MIB] * 17
    back = tmp_path / "back.bin"
    _storage.get_file(f"abfs://{container}/exports/day=1/part-0.parquet", back)
    assert _sha(back) == hashlib.sha256(data).hexdigest()
