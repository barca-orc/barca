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
    endpoint_var = None

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
    endpoint_var = "BARCA_TEST_S3_ENDPOINT"

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
    endpoint_var = "BARCA_TEST_GCS_ENDPOINT"

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
    endpoint_var = "BARCA_TEST_AZURITE_HOST"

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


def emulator_unreachable(be) -> None:
    """What a test does when its backend's emulator cannot be reached: skip, unless the
    environment names that emulator. The CI job that runs the conformance tests sets the
    `BARCA_TEST_*` endpoint variables and starts the emulators; there an unreachable one is a
    failure, so the job cannot go green by silently skipping what it exists to run. Without
    the variable (a laptop with no emulators) the test is skipped."""
    if be.endpoint_var and os.environ.get(be.endpoint_var):
        pytest.fail(
            f"{be.id} emulator not reachable, but {be.endpoint_var}="
            f"{os.environ[be.endpoint_var]} says it should be"
        )
    pytest.skip(f"{be.id} emulator not reachable")


def test_the_emulators_the_environment_names_are_reachable():
    """Under CI's `backends` job all three variables are set: every backend must be up, and
    this test says so by name rather than leaving it to a count of skips."""
    named = [be for be in ALL_BACKENDS if be.endpoint_var and os.environ.get(be.endpoint_var)]
    if not named:
        pytest.skip("no BARCA_TEST_* endpoint variable is set")
    down = [f"{be.id} ({be.endpoint_var}={os.environ[be.endpoint_var]})" for be in named]
    down = [d for d, be in zip(down, named) if not be.available()]
    assert not down, f"emulators named by the environment are not reachable: {down}"


@pytest.fixture(params=ALL_BACKENDS, ids=lambda b: b.id)
def backend(request, tmp_path, monkeypatch):
    be = request.param
    if not be.available():
        emulator_unreachable(be)
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


# ─── through the CLI: what a pull does to the local database ─────────────────
#
# The same on every backend: the blob is downloaded next to the local database and swapped
# in, and local runs that were never pushed are carried onto it (#221).


def test_a_pull_keeps_local_runs_that_were_never_pushed(backend, tmp_path):
    import json
    import subprocess

    from barca.api import _find_binary

    uri = backend.make_uri(tmp_path)

    def barca(machine: str, *args: str, **extra: str) -> dict:
        root = tmp_path / machine
        root.mkdir(exist_ok=True)
        env = {
            **{k: v for k, v in os.environ.items() if not k.startswith("BARCA_")},
            **backend.env(),
            "BARCA_STATE_URI": uri,
            "BARCA_POOL_SIZE": "2",
            **extra,
        }
        out = subprocess.run(
            [_find_binary(), *args], cwd=root, capture_output=True, text=True, env=env
        )
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)

    def get(machine: str, name: str, **extra: str) -> str:
        source = (
            f"from barca import asset\n\n\n@asset()\ndef {name}() -> str:\n    return {name!r}\n"
        )
        (tmp_path / machine).mkdir(exist_ok=True)
        (tmp_path / machine / f"{name}.py").write_text(source)
        return barca(machine, "get", f"{name}.py", "--json", **extra)["run_id"]

    def history(machine: str) -> list[str]:
        return sorted(r["run_id"] for r in barca(machine, "history", "--all", "--json")["runs"])

    a_first = get("a", "one")  # creates the shared state
    a_local = get("a", "two", BARCA_STATE="off")  # recorded on A only
    b_first = get("b", "three")  # B pushes in the meantime
    a_next = get("a", "four")  # A pulls B's push over its local database, then pushes

    # A lost neither B's run nor its own unpushed one, and B gets all of it, each run once.
    assert history("a") == sorted([a_first, a_local, b_first, a_next])
    b_next = get("b", "five")
    assert history("b") == sorted([a_first, a_local, b_first, a_next, b_next])


# A shared history that is not a database barca can use never replaces the local one (#243).
# The same on every backend: the object is downloaded next to the local database and checked
# there, so nothing depends on what the store says about it.


def _read_object(uri: str, tmp_path) -> bytes:
    if uri.startswith("file://"):
        with open(uri[len("file://") :], "rb") as f:
            return f.read()
    dest = tmp_path / f"read-{uuid.uuid4().hex}"
    _storage.get_file(uri, dest)
    return dest.read_bytes()


def _write_object(uri: str, data: bytes, tmp_path) -> None:
    if uri.startswith("file://"):
        with open(uri[len("file://") :], "wb") as f:
            f.write(data)
        return
    src = tmp_path / f"write-{uuid.uuid4().hex}"
    src.write_bytes(data)
    _storage.put_file(src, uri)


@pytest.mark.parametrize("damage", ["garbage", "empty", "cut short between pages"])
def test_an_invalid_shared_history_never_replaces_the_local_one(backend, tmp_path, damage):
    import json
    import subprocess

    from barca.api import _find_binary

    uri = backend.make_uri(tmp_path)
    root = tmp_path / "a"
    root.mkdir()
    env = {
        **{k: v for k, v in os.environ.items() if not k.startswith("BARCA_")},
        **backend.env(),
        "BARCA_STATE_URI": uri,
        "BARCA_POOL_SIZE": "2",
    }

    def barca(*args: str, **extra: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [_find_binary(), *args],
            cwd=root,
            capture_output=True,
            text=True,
            env={**env, **extra},
        )

    def get(name: str, **extra: str) -> str:
        source = (
            f"from barca import asset\n\n\n@asset()\ndef {name}() -> str:\n    return {name!r}\n"
        )
        (root / f"{name}.py").write_text(source)
        out = barca("get", f"{name}.py", "--json", **extra)
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)["run_id"]

    def history() -> list[str]:
        out = barca("history", "--all", "--json")
        assert out.returncode == 0, out.stderr
        return sorted(r["run_id"] for r in json.loads(out.stdout)["runs"])

    def local_files() -> dict[str, bytes]:
        return {
            p.name: p.read_bytes()
            for p in sorted((root / ".barca").iterdir())
            if p.name.startswith("metadata.db") and not p.name.endswith(".lock")
        }

    pushed = get("one")  # creates the shared history
    unpushed = get("two", BARCA_STATE="off")  # recorded here only
    good = _read_object(uri, tmp_path)
    assert good[:16] == b"SQLite format 3\x00" and len(good) % 4096 == 0
    bad = {
        "garbage": b"<html>503 Service Unavailable</html>",
        "empty": b"",
        "cut short between pages": good[: len(good) // 2 // 4096 * 4096],
    }[damage]
    _write_object(uri, bad, tmp_path)
    before = local_files()

    for args in (("get", "one.py", "--json"), ("status", "one.py", "--json")):
        out = barca(*args)
        assert out.returncode == 3, (args, out.returncode, out.stderr)
        assert uri in out.stderr, out.stderr
        assert "is not a database barca can use" in out.stderr, out.stderr
        assert "was left as it was, and nothing was uploaded" in out.stderr, out.stderr
        assert local_files() == before, args
        assert _read_object(uri, tmp_path) == bad, "the command wrote to the shared history"
    assert history() == sorted([pushed, unpushed])

    # With a good object back, the machine carries on and uploads what it held back.
    _write_object(uri, good, tmp_path)
    after = get("three")
    assert history() == sorted([pushed, unpushed, after])
    final = tmp_path / "final.db"
    assert pull(uri, final) is not None
    import sqlite3

    with sqlite3.connect(final) as conn:
        assert conn.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        assert sorted(r for (r,) in conn.execute("SELECT run_id FROM runs")) == history()
    conn.close()
