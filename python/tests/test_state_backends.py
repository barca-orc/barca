"""Backend conformance for the metadata backups in the store (RFC-0009, `barca._state`).

The same contract is asserted against every store backend, so that S3 (and S3-compatible R2),
GCS and Azure behave as the plain-directory store does:

    file://           always
    s3://    MinIO
    gs://    fake-gcs
    abfs://  Azurite

Object-store backends run against **local emulators** (no cloud creds, no cost). Each is
skipped when its emulator is unreachable, so the suite still runs (file:// only) on a laptop
with no Docker. CI starts the emulators as service containers and points the ``BARCA_TEST_*``
env vars at them.

The contract:

    1. list(absent prefix)          -> no backups
    2. backup(db, sidecar)          -> both objects in the prefix; list shows it with its sidecar
    3. backups beyond `keep`        -> the oldest are removed with their sidecars; anything that is
                                       not a backup name is left alone
    4. fetch                        -> the exact bytes
    5. stat                         -> exists and size for a backup; not there for a missing name
"""

import os
import socket
import uuid
from urllib.parse import urlsplit

import pytest

from barca import _storage
from barca import _state

from . import emulators

# ─── emulator endpoints (overridable for CI) ─────────────────────────────────

S3_ENDPOINT = os.environ.get("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
S3_KEY = os.environ.get("BARCA_TEST_S3_KEY", "minioadmin")
S3_SECRET = os.environ.get("BARCA_TEST_S3_SECRET", "minioadmin")

GCS_ENDPOINT = os.environ.get("BARCA_TEST_GCS_ENDPOINT", "http://localhost:9200")

# Azurite's well-known dev account (public, not a secret).
AZURITE_ACCOUNT = "devstoreaccount1"
AZURITE_KEY = (
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="
)
AZURITE_HOST = os.environ.get("BARCA_TEST_AZURITE_HOST", "127.0.0.1:9210")
AZURITE_CONN = (
    f"DefaultEndpointsProtocol=http;AccountName={AZURITE_ACCOUNT};AccountKey={AZURITE_KEY};"
    f"BlobEndpoint=http://{AZURITE_HOST}/{AZURITE_ACCOUNT};"
)


def _reachable(host: str, port: int, timeout: float = 0.5) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _hostport(url: str) -> "tuple[str, int]":
    parts = urlsplit(url)
    return parts.hostname or "localhost", parts.port or 80


# ─── per-backend setup ───────────────────────────────────────────────────────
#
# Each backend exposes:
#   available() -> bool           emulator reachable / always for file
#   env         -> dict[str,str]  env the backup helper would see in prod
#   make_uri()  -> str            a FRESH, empty backup prefix (bucket ensured)


class FileBackend:
    id = "file"

    def available(self):
        return True

    def env(self):
        return {}

    def make_uri(self, tmp_path):
        d = tmp_path / uuid.uuid4().hex
        d.mkdir()
        return f"file://{d}/state/backups"


class S3Backend:
    id = "s3"

    def available(self):
        return _reachable(*_hostport(S3_ENDPOINT))

    def env(self):
        import json

        return {
            "BARCA_STORAGE_OPTIONS": json.dumps(
                {
                    "s3": {
                        "key": S3_KEY,
                        "secret": S3_SECRET,
                        "client_kwargs": {"endpoint_url": S3_ENDPOINT},
                    }
                }
            )
        }

    def make_uri(self, tmp_path):
        import fsspec

        bucket = f"barca-test-{uuid.uuid4().hex[:12]}"
        fs = fsspec.filesystem(
            "s3", key=S3_KEY, secret=S3_SECRET, client_kwargs={"endpoint_url": S3_ENDPOINT}
        )
        fs.mkdir(f"s3://{bucket}")
        return f"s3://{bucket}/state/backups"


class GcsBackend:
    id = "gcs"

    def available(self):
        return _reachable(*_hostport(GCS_ENDPOINT))

    def env(self):
        import json

        return {
            # SDK push path (google-cloud-storage) routes via these:
            "STORAGE_EMULATOR_HOST": GCS_ENDPOINT,
            "GOOGLE_CLOUD_PROJECT": "test",
            # token read (gcsfs) routes via storage_options:
            "BARCA_STORAGE_OPTIONS": json.dumps(
                {"gcs": {"endpoint_url": GCS_ENDPOINT, "token": "anon", "project": "test"}}
            ),
        }

    def make_uri(self, tmp_path):
        os.environ["STORAGE_EMULATOR_HOST"] = GCS_ENDPOINT
        os.environ.setdefault("GOOGLE_CLOUD_PROJECT", "test")
        from google.auth.credentials import AnonymousCredentials
        from google.cloud import storage

        bucket = f"barca-test-{uuid.uuid4().hex[:12]}"
        client = storage.Client(project="test", credentials=AnonymousCredentials())
        client.bucket(bucket).create()
        return f"gs://{bucket}/state/backups"


class AzureBackend:
    id = "abfs"

    def available(self):
        return _reachable(*_hostport("http://" + AZURITE_HOST))

    def env(self):
        import json

        return {"BARCA_STORAGE_OPTIONS": json.dumps({"abfs": {"connection_string": AZURITE_CONN}})}

    def make_uri(self, tmp_path):
        import adlfs

        container = f"barca-test-{uuid.uuid4().hex[:12]}"
        fs = adlfs.AzureBlobFileSystem(connection_string=AZURITE_CONN)
        fs.mkdir(container)
        return f"abfs://{container}/state/backups"


ALL_BACKENDS = [FileBackend(), S3Backend(), GcsBackend(), AzureBackend()]


def test_the_emulators_the_environment_names_are_reachable():
    """Under CI's `backends` job all three variables are set: every backend must be up, and
    this test says so by name rather than leaving it to a count of skips. (Each test that
    needs an emulator also fails, rather than skips, there: `emulators.require`.)"""
    named = [be for be in ALL_BACKENDS if be.id != "file" and emulators.provided(be.id)]
    if not named:
        pytest.skip("no BARCA_TEST_* endpoint variable is set")
    down = [
        f"{be.id} ({emulators.variable(be.id)}={os.environ[emulators.variable(be.id)]})"
        for be in named
        if not be.available()
    ]
    assert not down, f"emulators named by the environment are not reachable: {down}"


@pytest.fixture(params=ALL_BACKENDS, ids=lambda b: b.id)
def backend(request, tmp_path, monkeypatch):
    be = request.param
    if be.id != "file":
        emulators.require(be.id, be.available())
    # Apply the env a real backup helper would receive, and clear the
    # per-protocol fs cache so each backend's options take effect.
    for k, v in be.env().items():
        monkeypatch.setenv(k, v)
    _storage._fs_cache.clear()
    yield be
    _storage._fs_cache.clear()


# ─── the contract ────────────────────────────────────────────────────────────


def _backup_files(tmp_path, name: str, data: bytes):
    db = tmp_path / name
    db.write_bytes(data)
    sidecar = tmp_path / (name[: -len(".db")] + ".json")
    sidecar.write_text(
        '{"run_id": "r1", "rows": 2, "digest": "d", "barca_version": "0", "db_format": 1}'
    )
    return str(db), str(sidecar)


def test_an_absent_prefix_holds_no_backups(backend, tmp_path):
    prefix = backend.make_uri(tmp_path)
    assert _state.list_backups(prefix) == {"backups": []}


def test_backup_list_fetch_and_stat_round_trip(backend, tmp_path):
    prefix = backend.make_uri(tmp_path)
    data = b"SQLite format 3\x00" + os.urandom(4096)
    db, sidecar = _backup_files(tmp_path, "2026-10-14T18-02-11Z.db", data)
    assert _state.backup(db, sidecar, prefix, 5) == {
        "uploaded": "2026-10-14T18-02-11Z.db",
        "removed": [],
    }
    (listed,) = _state.list_backups(prefix)["backups"]
    assert listed["name"] == "2026-10-14T18-02-11Z.db"
    assert listed["size_bytes"] == len(data)
    assert listed["sidecar"]["run_id"] == "r1"
    out = tmp_path / "fetched.db"
    _state.fetch(f"{prefix}/2026-10-14T18-02-11Z.db", str(out))
    assert out.read_bytes() == data
    there, missing = _state.stat(
        [f"{prefix}/2026-10-14T18-02-11Z.db", f"{prefix}/2026-10-14T18-02-12Z.db"]
    )["objects"]
    assert there["exists"] and there["size_bytes"] == len(data)
    assert not missing["exists"]


def test_only_the_newest_backups_are_kept(backend, tmp_path):
    prefix = backend.make_uri(tmp_path)
    names = [f"2026-10-14T18-02-1{i}Z.db" for i in range(7)]
    for name in names:
        db, sidecar = _backup_files(tmp_path, name, name.encode())
        result = _state.backup(db, sidecar, prefix, 5)
    assert result["removed"] == [names[1]]
    _storage.put_file(db, f"{prefix}/metadata.db")
    listed = [b["name"] for b in _state.list_backups(prefix)["backups"]]
    assert listed == sorted(names[2:], reverse=True)
    gone, kept, other = _state.stat(
        [
            f"{prefix}/{names[0][:-3]}.json",
            f"{prefix}/{names[2][:-3]}.json",
            f"{prefix}/metadata.db",
        ]
    )["objects"]
    assert not gone["exists"] and kept["exists"] and other["exists"]


def test_millisecond_names_sort_among_to_the_second_ones(backend, tmp_path):
    # #487: names carry milliseconds and a suffix; 0.26.0's to-the-second names still count.
    prefix = backend.make_uri(tmp_path)
    names = [
        "2026-10-14T18-02-10.999Z-0a0b0c0d.db",
        "2026-10-14T18-02-11Z.db",
        "2026-10-14T18-02-11.123Z-1a2b3c4d.db",
        "2026-10-14T18-02-11.123Z-9f8e7d6c.db",
        "2026-10-14T18-02-12Z.db",
    ]
    for name in names:
        db, sidecar = _backup_files(tmp_path, name, name.encode())
        _state.backup(db, sidecar, prefix, 5)
    listed = [b["name"] for b in _state.list_backups(prefix)["backups"]]
    assert listed == list(reversed(names))
    db, sidecar = _backup_files(tmp_path, "2026-10-14T18-02-13.000Z-00000000.db", b"x")
    assert _state.backup(db, sidecar, prefix, 5)["removed"] == [names[0]]
