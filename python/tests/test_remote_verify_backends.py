"""The local-copy hash check against real object-store clients (#248).

test_remote_verify.py and test_transfer.py pin the checks on a plain-directory store and fsspec's
memory filesystem. The check hashes the local file, so it should not depend on the backend; this
runs the same cases through s3fs, gcsfs and adlfs against the local emulators the state-backend
suite uses (MinIO, fake-gcs-server, Azurite): mismatch, stale-local-copy and injected-download
failure cases. test_remote_crash_backends.py adds real coordinator SIGKILL during refresh and
an actual SDK's staged write; this suite's injected ConnectionResetError is not that evidence.

On a machine without the emulators each backend skips. On CI, when the `backends` check starts
them and sets `BARCA_TEST_*`, an unreachable emulator fails the test (`emulators.require`).
"""

import hashlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

from barca import _storage, _transfer

from . import emulators
from .helpers import RUN, clean_env
from .test_remote_env_config import CASES, _reachable, barca, machines
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
    src = tmp / "overwrite.json"
    src.write_bytes(body)
    code = "import sys; from barca import _storage; _storage.put_file(sys.argv[1], sys.argv[2])"
    subprocess.run(
        [sys.executable, "-c", code, str(src), uri],
        env={**clean_env(), **env},
        check=True,
        timeout=RUN,
    )


@pytest.mark.parametrize("backend", ["s3", "gcs", "azure"])
def test_an_overwritten_object_is_used_and_flagged_in_the_json_result(backend, tmp_path):
    """What a refresh killed after its upload leaves in a bucket: the object has new bytes,
    the history still has the old hash. A machine without a local copy fetches the object,
    uses it and says so, on stderr and under `steps[].artifact_mismatch` (#247)."""
    import shutil

    endpoint, make = CASES[backend]
    emulators.require(backend, _reachable(endpoint), endpoint)
    uri, env = make()
    env = {**env, "BARCA_REMOTE_URI": uri}
    a, _ = machines(tmp_path)
    assert barca(a, env, "get", "total")["final_output"] == {"total": 3}
    (node,) = barca(a, env, "status", "numbers")["nodes"]
    stored = node["cache"]["artifact"]
    assert stored.startswith(f"{uri}/default/artifacts/"), stored
    _overwrite(stored, b'[{"n": 5}]', env, tmp_path)
    # This machine's copies are gone (a new container with the restored history).
    shutil.rmtree(a / ".barca" / "artifacts")

    out = barca(a, env, "get", "total", "--refresh", "total")

    assert out["final_output"] == {"total": 5}
    steps = {st["id"].rsplit(":", 1)[1]: st for st in out["steps"]}
    assert steps["numbers"]["status"] == "cached"
    assert steps["numbers"]["artifact_mismatch"] is True
    assert stored in steps["numbers"]["warning"]
    assert steps["total"]["status"] == "ran"
    assert steps["total"]["artifact_mismatch"] is True

    # A refresh of `numbers` itself clears it: the next fetch finds what was recorded.
    assert barca(a, env, "get", "numbers", "--refresh", "numbers")["final_output"] == [
        {"n": 1},
        {"n": 2},
    ]
    shutil.rmtree(a / ".barca" / "artifacts")
    clean = barca(a, env, "get", "numbers")
    assert all("artifact_mismatch" not in st for st in clean["steps"])


@pytest.mark.parametrize("backend", ["s3", "gcs", "azure"])
def test_recorded_local_copy_repairs_overwritten_store_after_check_window(backend, tmp_path):
    """Pin the local-recorded/store-mismatch outcome of overlapping refreshes."""
    from barca import _inspection_cache

    from .helpers import barca as run_barca
    from .store_window import age_store_checks

    endpoint, make = CASES[backend]
    emulators.require(backend, _reachable(endpoint), endpoint)
    uri, env = make()
    env = {**env, "BARCA_REMOTE_URI": uri, "BARCA_POOL_SIZE": "2", "BARCA_TRACE_TIMING": "1"}
    a, _ = machines(tmp_path)
    barca(a, env, "get", "numbers")
    (node,) = barca(a, env, "status", "numbers")["nodes"]
    stored = node["cache"]["artifact"]
    local = a / ".barca" / "artifacts" / stored.split("/artifacts/", 1)[1]
    original = local.read_bytes()
    recorded = node["last_materialization"]["content_hash"]
    assert hashlib.sha256(original).hexdigest() == recorded
    # Different size: real object stores do not provide a content checksum.
    _overwrite(stored, b'[{"n": 999999999999}]', env, tmp_path)
    _inspection_cache.Cache(str(a / ".barca/inspection-cache/default.json")).invalidate_remote(
        stored
    )
    (node,) = barca(a, env, "status", "numbers")["nodes"]
    assert (node["cache"]["state"], node["cache"]["reason"]) == ("unsynced", "mismatch")

    warning = "pipeline.py:numbers produced different bytes for the same code and inputs"
    # Status observes the mismatch, but does not end the execution store-check window.
    warm = run_barca(a, "get", "numbers", "--json", env=clean_env(**env))
    assert warm.returncode == 0, warm.stderr
    assert "store_check_skipped" in warm.stderr
    assert "transfer helper started" not in warm.stderr
    assert warning not in warm.stderr
    assert json.loads(warm.stdout)["steps_executed"] == 0
    assert not any(step.get("bytes_changed") for step in json.loads(warm.stdout)["steps"])

    age_store_checks(a)
    repaired = run_barca(a, "get", "numbers", "--json", env=clean_env(**env))
    assert repaired.returncode == 0, repaired.stderr
    assert repaired.stderr.count(warning) == 1, repaired.stderr
    out = json.loads(repaired.stdout)
    assert out["steps_executed"] == 0
    assert out["steps"][0]["bytes_changed"] is True
    assert local.read_bytes() == original
    downloaded = tmp_path / "repaired.json"
    subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys; from barca import _storage; _storage.get_file(sys.argv[1], sys.argv[2])",
            stored,
            str(downloaded),
        ],
        env={**clean_env(), **env},
        check=True,
        timeout=RUN,
    )
    assert downloaded.read_bytes() == original
    (node,) = barca(a, env, "status", "numbers")["nodes"]
    assert node["cache"]["state"] == "cached", node
    assert node["last_materialization"]["content_hash"] == recorded

    again = run_barca(a, "get", "numbers", "--json", env=clean_env(**env))
    assert again.returncode == 0, again.stderr
    assert warning not in again.stderr
    assert not any(step.get("bytes_changed") for step in json.loads(again.stdout)["steps"])
