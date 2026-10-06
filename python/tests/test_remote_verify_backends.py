"""The local-copy hash check against real object-store clients (#248).

test_remote_verify.py and test_transfer.py pin the checks on a plain-directory store and fsspec's
memory filesystem. The check hashes the local file, so it should not depend on the backend; this
runs the same cases through s3fs, gcsfs and adlfs against the local emulators the state-backend
suite uses (MinIO, fake-gcs-server, Azurite), each skipped when its emulator is unreachable.
"""

import hashlib
from pathlib import Path

import pytest

from barca import _storage, _transfer

from .test_state_backends import AzureBackend, GcsBackend, S3Backend

pytest.importorskip("fsspec")

BODY = b'{"x": 1}'
SHA = hashlib.sha256(BODY).hexdigest()


@pytest.fixture(params=[S3Backend(), GcsBackend(), AzureBackend()], ids=lambda b: b.id)
def store(request, tmp_path, monkeypatch):
    """A fresh remote artifact URI under an emulator bucket holding BODY."""
    be = request.param
    if not be.available():
        pytest.skip(f"{be.id} emulator not reachable")
    for k, v in be.env().items():
        monkeypatch.setenv(k, v)
    # Emulator only: skip gcsfs's gRPC bucket-layout probe, which fake-gcs cannot answer.
    monkeypatch.setenv("GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT", "false")
    monkeypatch.chdir(tmp_path)
    _storage._fs_cache.clear()
    base = be.make_uri(tmp_path).rsplit("/state/", 1)[0]
    uri = f"{base}/arts/total/h1.json"
    _put(uri, BODY, tmp_path)
    yield uri
    _storage._fs_cache.clear()


def _put(uri: str, body: bytes, tmp: Path) -> None:
    src = tmp / "upload.json"
    src.write_bytes(body)
    _storage.put_file(str(src), uri)


def _get(uri: str, local: Path, sha: str | None = SHA) -> dict:
    return _transfer._transfer({"type": "get", "remote": uri, "local": str(local), "sha256": sha})


def test_matching_local_copy_is_kept_without_a_download(store, tmp_path, monkeypatch):
    local = tmp_path / "l" / "h1.json"
    local.parent.mkdir()
    local.write_bytes(BODY)
    monkeypatch.setattr(_storage, "get_file", lambda *a: pytest.fail("downloaded"))
    out = _get(store, local)
    assert (out["fetched"], out["mismatch"], out["sha256"]) == (False, False, SHA)


def test_stale_local_copy_is_replaced_by_the_stores(store, tmp_path):
    local = tmp_path / "l" / "h1.json"
    local.parent.mkdir()
    local.write_bytes(b'{"x": 2}')
    out = _get(store, local)
    assert (out["fetched"], out["mismatch"]) == (True, False)
    assert local.read_bytes() == BODY
    assert sorted(p.name for p in local.parent.iterdir()) == ["h1.json"]


def test_store_copy_with_other_bytes_is_used_and_flagged(store, tmp_path):
    _put(store, b'{"x": 2}', tmp_path)
    local = tmp_path / "l" / "h1.json"
    out = _get(store, local)
    assert (out["fetched"], out["mismatch"]) == (True, True)
    assert local.read_bytes() == b'{"x": 2}'
    assert sorted(p.name for p in local.parent.iterdir()) == ["h1.json"]


def test_a_download_that_dies_midway_leaves_the_local_copy_and_no_temp_file(
    store, tmp_path, monkeypatch
):
    """The staged download is renamed into place only once whole, whatever the backend."""
    local = tmp_path / "l" / "h1.json"
    local.parent.mkdir()
    local.write_bytes(b'{"x": 2}')  # stale: forces a download
    real = _storage.get_file

    def dies_midway(remote, dest):
        real(remote, dest)
        Path(dest).write_bytes(BODY[:3])
        raise ConnectionResetError("killed")

    monkeypatch.setattr(_storage, "get_file", dies_midway)
    with pytest.raises(ConnectionResetError):
        _get(store, local)
    assert local.read_bytes() == b'{"x": 2}'
    assert sorted(p.name for p in local.parent.iterdir()) == ["h1.json"]
