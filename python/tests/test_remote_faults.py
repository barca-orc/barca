"""End-to-end artifact transfer against real object-store APIs under injected faults.

Drives the installed `barca` binary against each emulator the CI `backends` job
runs — MinIO (S3), Azurite (Azure Blob) and fake-gcs-server (GCS). Artifacts go
through a local TCP proxy that can reset or stall connections; the shared state
blob goes to a local file so faults hit artifact transfers only. Assertions are
on outcomes (the run, the store, the recorded rows) — not on which layer retried,
since the cloud SDKs retry internally too.

fake-gcs-server does no authentication, so the bad-credentials case runs only
against S3 and Azure.

Each backend is skipped unless its emulator is reachable (BARCA_TEST_S3_ENDPOINT,
BARCA_TEST_AZURITE_HOST, BARCA_TEST_GCS_ENDPOINT — the CI defaults below), and
everything is skipped unless the barca binary is installed next to this
interpreter or on PATH.
"""

import base64
import json
import os
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit

import pytest

S3_ENDPOINT = os.environ.get("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
S3_KEY = os.environ.get("BARCA_TEST_S3_KEY", "minioadmin")
S3_SECRET = os.environ.get("BARCA_TEST_S3_SECRET", "minioadmin")
GCS_ENDPOINT = os.environ.get("BARCA_TEST_GCS_ENDPOINT", "http://localhost:9200")
AZURITE_HOST = os.environ.get("BARCA_TEST_AZURITE_HOST", "127.0.0.1:9210")
# Azurite's well-known dev account (public, not a secret).
AZURITE_ACCOUNT = "devstoreaccount1"
AZURITE_KEY = (
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="
)


def _barca_bin() -> str | None:
    beside = Path(sys.executable).parent / "barca"
    if beside.exists():
        return str(beside)
    return shutil.which("barca")


def _hostport(endpoint: str) -> tuple[str, int]:
    u = urlsplit(endpoint)
    return u.hostname or "localhost", u.port or 80


def _reachable(endpoint: str) -> bool:
    try:
        with socket.create_connection(_hostport(endpoint), timeout=1):
            return True
    except OSError:
        return False


BARCA = _barca_bin()
pytestmark = pytest.mark.skipif(BARCA is None, reason="barca binary not installed")


# ─── Fault-injecting TCP proxy ───────────────────────────────────────────────


class FaultProxy:
    """Forwards to an emulator; can reset or stall new connections."""

    def __init__(self, target: tuple[str, int]):
        self.target = target
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(64)
        self.port = self.listener.getsockname()[1]
        self.resets_left = 0
        self.stall = False
        self.connections = 0
        self._held: list[socket.socket] = []
        self._lock = threading.Lock()
        self._closed = False
        threading.Thread(target=self._accept_loop, daemon=True).start()

    @property
    def endpoint(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def _accept_loop(self) -> None:
        while not self._closed:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn: socket.socket) -> None:
        with self._lock:
            self.connections += 1
            reset = self.resets_left > 0
            if reset:
                self.resets_left -= 1
            stall = self.stall
        if reset:
            # RST, not FIN: what a dropped connection looks like to the client.
            conn.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
            conn.close()
            return
        if stall:
            with self._lock:
                self._held.append(conn)  # accept, then never answer
            return
        try:
            upstream = socket.create_connection(self.target)
        except OSError:
            conn.close()
            return
        for a, b in ((conn, upstream), (upstream, conn)):
            threading.Thread(target=self._pump, args=(a, b), daemon=True).start()

    @staticmethod
    def _pump(src: socket.socket, dst: socket.socket) -> None:
        try:
            while data := src.recv(65536):
                dst.sendall(data)
        except OSError:
            pass
        finally:
            for s in (src, dst):
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def close(self) -> None:
        self._closed = True
        self.listener.close()
        with self._lock:
            for c in self._held:
                c.close()


# ─── Fixtures ────────────────────────────────────────────────────────────────

PIPELINE = """
from barca import asset

@asset()
def numbers() -> list:
    return list(range(1000))

@asset(inputs={"nums": numbers})
def total(nums: list) -> dict:
    return {"sum": sum(nums)}
"""


class S3:
    env: dict[str, str] = {}  # noqa: RUF012
    id = "s3"
    scheme = "s3"
    endpoint = S3_ENDPOINT
    checks_auth = True

    def fs(self):
        import fsspec

        return fsspec.filesystem(
            "s3",
            key=S3_KEY,
            secret=S3_SECRET,
            client_kwargs={"endpoint_url": S3_ENDPOINT},
            skip_instance_cache=True,
        )

    def storage_options(self, endpoint: str, bad_auth: bool) -> str:
        secret = "wrong-secret" if bad_auth else S3_SECRET
        return (
            "[remote.storage_options.s3]\n"
            f'key = "{S3_KEY}"\n'
            f'secret = "{secret}"\n'
            f'client_kwargs = {{ endpoint_url = "{endpoint}" }}\n'
        )


class Azure:
    env: dict[str, str] = {}  # noqa: RUF012
    id = "abfs"
    scheme = "abfs"
    endpoint = f"http://{AZURITE_HOST}"
    checks_auth = True

    @staticmethod
    def _conn(host: str, key: str) -> str:
        return (
            f"DefaultEndpointsProtocol=http;AccountName={AZURITE_ACCOUNT};AccountKey={key};"
            f"BlobEndpoint=http://{host}/{AZURITE_ACCOUNT};"
        )

    def fs(self):
        import adlfs

        return adlfs.AzureBlobFileSystem(
            connection_string=self._conn(AZURITE_HOST, AZURITE_KEY), skip_instance_cache=True
        )

    def storage_options(self, endpoint: str, bad_auth: bool) -> str:
        # A well-formed key for the right account, but the wrong one.
        key = base64.b64encode(os.urandom(64)).decode() if bad_auth else AZURITE_KEY
        host = urlsplit(endpoint).netloc
        return f'[remote.storage_options.abfs]\nconnection_string = "{self._conn(host, key)}"\n'


class Gcs:
    id = "gcs"
    scheme = "gs"
    endpoint = GCS_ENDPOINT
    checks_auth = False  # fake-gcs-server accepts any credentials
    # gcsfs >= 2026.10 defaults to an experimental mode that asks the gRPC
    # Storage Control API for each bucket's type before writing. Real GCS
    # answers; fake-gcs-server only speaks HTTP, so the call hangs. Use the
    # plain HTTP client against the emulator.
    env = {"GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT": "false"}  # noqa: RUF012

    def fs(self):
        # The core (HTTP-only) class directly: the env toggle above is read
        # when gcsfs is first imported, which may already have happened.
        from gcsfs.core import GCSFileSystem

        return GCSFileSystem(
            endpoint_url=GCS_ENDPOINT,
            token="anon",
            project="test",
            skip_instance_cache=True,
        )

    def storage_options(self, endpoint: str, bad_auth: bool) -> str:
        return (
            "[remote.storage_options.gcs]\n"
            f'endpoint_url = "{endpoint}"\n'
            'token = "anon"\n'
            'project = "test"\n'
        )


@pytest.fixture(params=[S3(), Azure(), Gcs()], ids=lambda b: b.id)
def backend(request):
    be = request.param
    if not _reachable(be.endpoint):
        pytest.skip(f"{be.id} emulator not reachable at {be.endpoint}")
    return be


@pytest.fixture
def container(backend):
    """A fresh bucket/container; yields its name."""
    name = f"barca-faults-{uuid.uuid4().hex[:12]}"
    fs = backend.fs()
    fs.mkdir(name)
    yield name
    try:
        fs.rm(name, recursive=True)
    except Exception:
        pass


@pytest.fixture
def proxy(backend):
    p = FaultProxy(_hostport(backend.endpoint))
    yield p
    p.close()


def _stored(backend, container: str) -> list[str]:
    return [p for p in backend.fs().find(f"{container}/proj/artifacts") if not p.endswith("/")]


class Project:
    """One simulated machine: a working dir sharing the container and state file."""

    def __init__(
        self,
        root: Path,
        backend,
        container: str,
        endpoint: str,
        state: Path,
        bad_auth: bool = False,
        **remote,
    ):
        self.dir = root
        self.dir.mkdir(parents=True, exist_ok=True)
        self.state = state
        self.env = {**os.environ, **backend.env}
        (self.dir / "pipeline.py").write_text(PIPELINE)
        extra = "".join(f"{k} = {v}\n" for k, v in remote.items())
        (self.dir / "barca.toml").write_text(
            "[remote]\n"
            f'artifacts_uri = "{backend.scheme}://{container}/proj/artifacts"\n'
            f'state_uri = "{state}"\n'
            f"{extra}\n" + backend.storage_options(endpoint, bad_auth)
        )

    def get(self, *args: str, timeout: float = 120) -> tuple[subprocess.CompletedProcess, float]:
        t0 = time.monotonic()
        proc = subprocess.run(
            [BARCA, "get", *args, "pipeline.py", "--agent"],
            cwd=self.dir,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return proc, time.monotonic() - t0

    def rows(self, like: str) -> list[dict]:
        conn = sqlite3.connect(self.state)
        conn.row_factory = sqlite3.Row
        return [
            dict(r)
            for r in conn.execute(
                "SELECT node_id, status, error_type, artifact_path, attempts, error_message "
                "FROM materializations WHERE node_id LIKE ? ORDER BY id",
                (f"%{like}%",),
            )
        ]


def _explain(proc: subprocess.CompletedProcess) -> str:
    return f"exit={proc.returncode}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"


# ─── Tests ───────────────────────────────────────────────────────────────────


def test_uploads_survive_connection_resets(tmp_path, backend, container, proxy):
    proxy.resets_left = 4
    a = Project(tmp_path / "a", backend, container, proxy.endpoint, tmp_path / "state.db")
    proc, _ = a.get()
    assert proc.returncode == 0, _explain(proc)
    assert proxy.resets_left == 0, "faults were never hit — the test proves nothing"
    assert "uploaded 2 artifacts" in proc.stderr
    stored = _stored(backend, container)
    assert len(stored) == 2, stored
    rows = a.rows("")
    assert [r["status"] for r in rows] == ["success", "success"]
    prefix = f"{backend.scheme}://{container}/proj/artifacts/"
    assert all(r["artifact_path"].startswith(prefix) for r in rows), rows


def test_cross_machine_fetch_survives_connection_resets(tmp_path, backend, container, proxy):
    state = tmp_path / "state.db"
    a = Project(tmp_path / "a", backend, container, proxy.endpoint, state)
    proc, _ = a.get()
    assert proc.returncode == 0, _explain(proc)

    proxy.resets_left = 4
    b = Project(tmp_path / "b", backend, container, proxy.endpoint, state)
    proc, _ = b.get()
    assert proc.returncode == 0, _explain(proc)
    assert proxy.resets_left == 0, "faults were never hit — the test proves nothing"
    assert json.loads(proc.stdout)["steps_executed"] == 0
    assert "fetched 1 cached artifact" in proc.stderr
    assert "499500" in proc.stdout


def test_bad_credentials_fail_fast_without_retries(tmp_path, backend, container, proxy):
    if not backend.checks_auth:
        pytest.skip(f"{backend.id} emulator does not authenticate")
    a = Project(
        tmp_path / "a", backend, container, proxy.endpoint, tmp_path / "state.db", bad_auth=True
    )
    proc, took = a.get()
    assert proc.returncode != 0, _explain(proc)
    assert "upload" in proc.stderr, _explain(proc)
    assert took < 30, f"auth failure took {took:.1f}s — retried a permanent error?"
    rows = a.rows("")
    assert {r["status"] for r in rows} == {"failed"}, rows
    for r in rows:
        assert r["error_type"] == "UploadError"
        assert r["artifact_path"] is None
        # Real auth errors must classify as permanent: one attempt.
        assert r["attempts"] == 1, r


def test_stalled_store_times_out_instead_of_hanging(tmp_path, backend, container, proxy):
    proxy.stall = True
    a = Project(
        tmp_path / "a",
        backend,
        container,
        proxy.endpoint,
        tmp_path / "state.db",
        transfer_timeout=3,
    )
    proc, took = a.get(timeout=90)
    assert proc.returncode != 0, _explain(proc)
    assert "TimeoutError" in proc.stderr, _explain(proc)
    assert took < 45, f"stalled store held the run for {took:.1f}s"
    rows = a.rows("")
    assert rows and all(r["status"] == "failed" for r in rows), rows
    assert all("TimeoutError" in r["error_message"] for r in rows)


def test_cache_hit_with_missing_object_fails_fast_with_hint(tmp_path, backend, container, proxy):
    state = tmp_path / "state.db"
    a = Project(tmp_path / "a", backend, container, proxy.endpoint, state)
    proc, _ = a.get()
    assert proc.returncode == 0, _explain(proc)
    fs = backend.fs()
    for path in _stored(backend, container):
        if "total" in path:
            fs.rm(path)

    b = Project(tmp_path / "b", backend, container, proxy.endpoint, state)
    proc, took = b.get()
    assert proc.returncode != 0, _explain(proc)
    assert "could not fetch" in proc.stderr and "--no-cache" in proc.stderr, _explain(proc)
    assert took < 30, f"missing object took {took:.1f}s — retried a permanent error?"
