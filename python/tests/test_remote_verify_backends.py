"""The local-copy hash check against real object-store clients (#248).

test_remote_verify.py and test_transfer.py pin the checks on a plain-directory store and fsspec's
memory filesystem. The check hashes the local file, so it should not depend on the backend; this
runs the same cases through s3fs, gcsfs and adlfs against the local emulators the state-backend
suite uses (MinIO, fake-gcs-server, Azurite): mismatch, stale-local-copy and injected-download
failure cases. test_remote_crash_backends.py adds real coordinator SIGKILL during refresh and
an actual SDK's staged write; this suite's injected ConnectionResetError is not that evidence.

On a machine without the emulators each backend skips. On CI, where the `backends` job starts
them and sets `BARCA_TEST_*`, an unreachable emulator fails the test (`emulators.require`).
"""

import hashlib
import subprocess
import sys
from pathlib import Path

import pytest

from barca import _storage, _transfer

from . import emulators
from .test_remote_env_config import CASES, SCRUB, _reachable, barca, machines
from .test_state_backends import AzureBackend, GcsBackend, S3Backend

if not any(emulators.provided(b) for b in ("s3", "gcs", "abfs")):
    # A laptop without the storage clients. Where the emulators are declared provided (CI),
    # a missing client is an error, not a reason to skip.
    pytest.importorskip("fsspec")

BODY = b'{"x": 1}'
SHA = hashlib.sha256(BODY).hexdigest()


@pytest.fixture(params=[S3Backend(), GcsBackend(), AzureBackend()], ids=lambda b: b.id)
def store(request, tmp_path, monkeypatch):
    """A fresh remote artifact URI under an emulator bucket holding BODY."""
    be = request.param
    emulators.require(be.id, be.available())
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


# ─── through the binary ──────────────────────────────────────────────────────


def _overwrite(uri: str, body: bytes, env: dict, tmp: Path) -> None:
    """Replace the object at `uri` with `body` through barca's own upload path, in a process
    configured like a barca machine (fsspec reads its settings from the environment)."""
    import os

    src = tmp / "overwrite.json"
    src.write_bytes(body)
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    code = "import sys; from barca import _storage; _storage.put_file(sys.argv[1], sys.argv[2])"
    subprocess.run(
        [sys.executable, "-c", code, str(src), uri], env={**base, **env}, check=True, timeout=120
    )


@pytest.mark.parametrize("backend", ["s3", "gcs", "azure"])
def test_an_overwritten_object_is_used_and_flagged_in_the_json_result(backend, tmp_path):
    """What a refresh killed after its upload leaves in a bucket: the object has new bytes,
    the shared history still has the old hash. A second machine uses the object and says so,
    on stderr and under `steps[].artifact_mismatch` (#247)."""
    endpoint, make = CASES[backend]
    emulators.require(backend, _reachable(endpoint), endpoint)
    uri, env = make()
    env = {**env, "BARCA_REMOTE_URI": uri}
    a, b = machines(tmp_path)
    assert barca(a, env, "get", "total")["final_output"] == {"total": 3}
    (node,) = barca(a, env, "status", "numbers")["nodes"]
    stored = node["cache"]["artifact"]
    assert stored.startswith(f"{uri}/default/artifacts/"), stored
    _overwrite(stored, b'[{"n": 5}]', env, tmp_path)

    out = barca(b, env, "get", "total", "--refresh", "total")

    assert out["final_output"] == {"total": 5}
    steps = {st["id"].rsplit(":", 1)[1]: st for st in out["steps"]}
    assert steps["numbers"]["status"] == "cached"
    assert steps["numbers"]["artifact_mismatch"] is True
    assert stored in steps["numbers"]["warning"]
    assert steps["total"]["status"] == "ran"
    assert steps["total"]["artifact_mismatch"] is True

    # The refresh of `total` did not touch `numbers`: a third look still reports it, and a
    # refresh of `numbers` itself clears it for everyone.
    assert barca(b, env, "get", "numbers", "--refresh", "numbers")["final_output"] == [
        {"n": 1},
        {"n": 2},
    ]
    c = tmp_path / "machine_c"
    c.mkdir()
    (c / "pipeline.py").write_text((a / "pipeline.py").read_text())
    clean = barca(c, env, "get", "numbers")
    assert all("artifact_mismatch" not in st for st in clean["steps"])
