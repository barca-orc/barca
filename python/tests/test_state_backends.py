"""Backend conformance suite for shared-state pull/push.

The SAME optimistic-concurrency contract is asserted against every state
backend so that S3, GCS (and S3-compatible R2), and Azure are held to the
identical behaviour the ``file://`` backend already guarantees:

    file://           always (stdlib sha256 + flock)
    s3://    MinIO    S3 API — also the code path Cloudflare R2 rides on
    gs://    fake-gcs GCS generation-match
    abfs://  Azurite  Azure ETag If-Match

Object-store backends run against **local emulators** (no cloud creds, no
cost). Each is skipped when its emulator is unreachable, so the suite still
runs (file:// only) on a laptop with no Docker. CI starts the emulators as
service containers and points the ``BARCA_TEST_*`` env vars at them.

The contract (identical for every backend):

    1. pull(absent)              -> None, local file untouched
    2. push(create-only)         -> token; remote now exists
    3. pull                      -> same token + exact bytes
    4. push(create-only, again)  -> ConflictError   (concurrent first-push race)
    5. push(correct token)       -> new token, bytes replaced
    6. push(stale token)         -> ConflictError   (the emulator MUST enforce
                                    the precondition — a lenient emulator makes
                                    this fail loud instead of false-passing)
    7. bytes round-trip intact end to end
"""

import os
import socket
import uuid
from urllib.parse import urlsplit

import pytest

from barca import _storage
from barca._state import ConflictError, pull, push

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
#   env         -> dict[str,str]  env the pull/push child would see in prod
#   make_uri()  -> str            a FRESH, empty state URI (bucket ensured)


class FileBackend:
    id = "file"

    def available(self):
        return True

    def env(self):
        return {}

    def make_uri(self, tmp_path):
        d = tmp_path / uuid.uuid4().hex
        d.mkdir()
        return f"file://{d}/metadata.db"


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
        return f"s3://{bucket}/state/metadata.db"


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
        return f"gs://{bucket}/state/metadata.db"


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
        return f"abfs://{container}/state/metadata.db"


ALL_BACKENDS = [FileBackend(), S3Backend(), GcsBackend(), AzureBackend()]


@pytest.fixture(params=ALL_BACKENDS, ids=lambda b: b.id)
def backend(request, tmp_path, monkeypatch):
    be = request.param
    if not be.available():
        pytest.skip(f"{be.id} emulator not reachable")
    # Apply the env a real pull/push child process would receive, and clear the
    # per-protocol fs cache so each backend's options take effect.
    for k, v in be.env().items():
        monkeypatch.setenv(k, v)
    _storage._fs_cache.clear()
    yield be
    _storage._fs_cache.clear()


# ─── the shared contract ─────────────────────────────────────────────────────


def _write(tmp_path, name, data: bytes):
    p = tmp_path / name
    p.write_bytes(data)
    return p


def test_pull_absent_returns_none(backend, tmp_path):
    uri = backend.make_uri(tmp_path)
    dest = tmp_path / "local.db"
    assert pull(uri, dest) is None
    assert not dest.exists()  # left untouched


def test_create_pull_roundtrip(backend, tmp_path):
    uri = backend.make_uri(tmp_path)
    src = _write(tmp_path, "src.db", b"barca-state-v1")

    token = push(uri, src, None)
    assert token, "create-only push must return a token"

    dest = tmp_path / "pulled.db"
    pulled_token = pull(uri, dest)
    assert pulled_token == token
    assert dest.read_bytes() == b"barca-state-v1"


def test_concurrent_create_conflicts(backend, tmp_path):
    """Two machines both first-push from an empty remote: one must conflict."""
    uri = backend.make_uri(tmp_path)
    a = _write(tmp_path, "a.db", b"from-machine-a")
    push(uri, a, None)  # machine A wins the create

    b = _write(tmp_path, "b.db", b"from-machine-b")
    with pytest.raises(ConflictError):
        push(uri, b, None)  # machine B create-only must fail — object exists


def test_conditional_overwrite_advances(backend, tmp_path):
    uri = backend.make_uri(tmp_path)
    v1 = _write(tmp_path, "v1.db", b"state-one")
    tok1 = push(uri, v1, None)

    v2 = _write(tmp_path, "v2.db", b"state-two-longer")
    tok2 = push(uri, v2, tok1)
    assert tok2 != tok1

    dest = tmp_path / "final.db"
    assert pull(uri, dest) == tok2
    assert dest.read_bytes() == b"state-two-longer"


def test_stale_token_conflicts(backend, tmp_path):
    """The emulator MUST enforce the precondition. If this fails, the emulator
    is too lenient and every conflict test above is silently meaningless."""
    uri = backend.make_uri(tmp_path)
    v1 = _write(tmp_path, "v1.db", b"one")
    tok1 = push(uri, v1, None)

    v2 = _write(tmp_path, "v2.db", b"two")
    push(uri, v2, tok1)  # advances the object; tok1 is now stale

    v3 = _write(tmp_path, "v3.db", b"three")
    with pytest.raises(ConflictError):
        push(uri, v3, tok1)  # stale precondition must be rejected
