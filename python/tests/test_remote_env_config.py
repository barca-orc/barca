"""Remote storage configured with environment variables only (no barca.toml).

The promise in `barca docs remote`: set BARCA_REMOTE_URI and the variables your cloud or fsspec
already reads, and two machines share one cache. Each case runs a pipeline on "machine A" (a
project directory), then on "machine B" (a fresh copy with no local .barca/), and B must be a
cache hit served from the bucket. Runs against the local emulators the backend suite uses
(MinIO, fake-gcs-server, Azurite); a backend whose emulator is unreachable is skipped.
"""

import json
import os
import socket
import subprocess
import uuid
from pathlib import Path
from urllib.parse import urlsplit

import pytest

from barca.api import _find_binary

S3_ENDPOINT = os.environ.get("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
GCS_ENDPOINT = os.environ.get("BARCA_TEST_GCS_ENDPOINT", "http://localhost:9200")
AZURITE_HOST = os.environ.get("BARCA_TEST_AZURITE_HOST", "127.0.0.1:9210")
AZURITE_CONN = (
    "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;"
    "AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/"
    f"KBHBeksoGMGw==;BlobEndpoint=http://{AZURITE_HOST}/devstoreaccount1;"
)

PIPELINE = """
from barca import asset


@asset()
def numbers() -> list:
    return [{"n": 1}, {"n": 2}]


@asset(inputs={"rows": numbers})
def total(rows: list) -> dict:
    return {"total": sum(r["n"] for r in rows)}
"""

# Variables that could leak a developer's real cloud setup into the test.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")


def _reachable(url_or_host: str) -> bool:
    parts = urlsplit(url_or_host if "://" in url_or_host else f"http://{url_or_host}")
    try:
        with socket.create_connection((parts.hostname, parts.port), timeout=0.5):
            return True
    except OSError:
        return False


def _bucket(name: str) -> str:
    return f"barca-env-{name}-{uuid.uuid4().hex[:8]}"


def s3_case() -> tuple[str, dict]:
    import fsspec

    bucket = _bucket("s3")
    fs = fsspec.filesystem(
        "s3",
        key="minioadmin",
        secret="minioadmin",
        endpoint_url=S3_ENDPOINT,
        skip_instance_cache=True,
    )
    fs.mkdir(bucket)
    return f"s3://{bucket}/proj", {
        "AWS_ACCESS_KEY_ID": "minioadmin",
        "AWS_SECRET_ACCESS_KEY": "minioadmin",
        "FSSPEC_S3_ENDPOINT_URL": S3_ENDPOINT,
    }


def gcs_case() -> tuple[str, dict]:
    from google.auth.credentials import AnonymousCredentials
    from google.cloud import storage

    bucket = _bucket("gcs")
    client = storage.Client(
        project="test",
        credentials=AnonymousCredentials(),
        client_options={"api_endpoint": GCS_ENDPOINT},
    )
    client.create_bucket(bucket)
    # No STORAGE_EMULATOR_HOST: the fsspec variables alone must reach both the artifact store
    # (gcsfs) and the shared state (google-cloud-storage).
    return f"gs://{bucket}/proj", {
        "FSSPEC_GCS_ENDPOINT_URL": GCS_ENDPOINT,
        "FSSPEC_GCS_TOKEN": "anon",
        "FSSPEC_GCS_PROJECT": "test",
        # Emulator only: skip gcsfs's gRPC bucket-layout probe, which fake-gcs cannot answer.
        "GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT": "false",
    }


def azure_case() -> tuple[str, dict]:
    from azure.storage.blob import BlobServiceClient

    container = _bucket("az")
    BlobServiceClient.from_connection_string(AZURITE_CONN).create_container(container)
    return f"abfs://{container}/proj", {"AZURE_STORAGE_CONNECTION_STRING": AZURITE_CONN}


def azure_fsspec_case() -> tuple[str, dict]:
    """The generic fsspec convention: FSSPEC_<PROTOCOL>_<OPTION>."""
    uri, _ = azure_case()
    return uri, {"FSSPEC_ABFS_CONNECTION_STRING": AZURITE_CONN}


CASES = {
    "s3": (S3_ENDPOINT, s3_case),
    "gcs": (GCS_ENDPOINT, gcs_case),
    "azure": (AZURITE_HOST, azure_case),
    "azure_fsspec": (AZURITE_HOST, azure_fsspec_case),
}


def barca(cwd: Path, env: dict, *args: str) -> dict:
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    proc = subprocess.run(
        [_find_binary(), *args, "--json"],
        cwd=cwd,
        env={**base, **env},
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout.strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])


def machines(tmp_path: Path) -> tuple[Path, Path]:
    dirs = []
    for name in ("machine_a", "machine_b"):
        d = tmp_path / name
        d.mkdir()
        (d / "pipeline.py").write_text(PIPELINE)
        dirs.append(d)
    return dirs[0], dirs[1]


@pytest.mark.parametrize("backend", sorted(CASES))
def test_env_vars_alone_share_one_cache_across_machines(backend, tmp_path):
    endpoint, make = CASES[backend]
    if not _reachable(endpoint):
        pytest.skip(f"{backend} emulator not reachable at {endpoint}")
    uri, env = make()
    env = {**env, "BARCA_REMOTE_URI": uri}
    a, b = machines(tmp_path)

    first = barca(a, env, "get", "total")
    assert first["steps_executed"] == 2
    status = barca(a, env, "status", "total")
    artifacts = [n["cache"]["artifact"] for n in status["nodes"]]
    assert all(p.startswith(f"{uri}/default/artifacts/") for p in artifacts), artifacts

    second = barca(b, env, "get", "total")
    assert second["steps_executed"] == 0, "machine B must hit the cache machine A filled"


def test_gcs_storage_options_in_barca_toml_reach_the_shared_state(tmp_path):
    """The same settings as a barca.toml table instead of FSSPEC_GCS_* variables."""
    if not _reachable(GCS_ENDPOINT):
        pytest.skip(f"gcs emulator not reachable at {GCS_ENDPOINT}")
    uri, _ = gcs_case()
    a, b = machines(tmp_path)
    toml = (
        f'[remote]\nuri = "{uri}"\n\n[remote.storage_options.gcs]\n'
        f'endpoint_url = "{GCS_ENDPOINT}"\ntoken = "anon"\nproject = "test"\n'
    )
    for d in (a, b):
        (d / "barca.toml").write_text(toml)
    env = {"GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT": "false"}
    assert barca(a, env, "get", "total")["steps_executed"] == 2
    assert barca(b, env, "get", "total")["steps_executed"] == 0


# ─── The GCS state client takes gcsfs's options (no network) ─────────────────


def _service_account_info() -> dict:
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import rsa

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ).decode()
    return {
        "type": "service_account",
        "project_id": "from-key-file",
        "private_key_id": "x",
        "private_key": pem,
        "client_email": "barca@from-key-file.iam.gserviceaccount.com",
        "client_id": "1",
        "token_uri": "https://oauth2.googleapis.com/token",
    }


@pytest.fixture
def gcs_state(monkeypatch):
    pytest.importorskip("google.cloud.storage")
    import fsspec.config

    from barca import _state

    for k in list(os.environ):
        if k.startswith(SCRUB):
            monkeypatch.delenv(k)
    monkeypatch.setattr(fsspec.config, "conf", {})
    _state._gcs_clients.clear()
    yield _state
    _state._gcs_clients.clear()


def test_gcs_state_client_reads_fsspec_config(gcs_state, monkeypatch):
    import fsspec.config
    from google.auth.credentials import AnonymousCredentials

    monkeypatch.setattr(
        fsspec.config,
        "conf",
        {"gcs": {"token": "anon", "project": "p1", "endpoint_url": "http://example:1"}},
    )
    client = gcs_state._gcs_client()
    assert isinstance(client._credentials, AnonymousCredentials)
    assert client.project == "p1"
    assert client._connection.API_BASE_URL == "http://example:1"


def test_barca_storage_options_override_fsspec_config(gcs_state, monkeypatch):
    import fsspec.config

    monkeypatch.setattr(fsspec.config, "conf", {"gcs": {"token": "anon", "project": "from-fsspec"}})
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", json.dumps({"gcs": {"project": "from-barca"}}))
    assert gcs_state._gcs_client().project == "from-barca"


def test_gcs_state_client_uses_a_service_account_key_file(gcs_state, monkeypatch, tmp_path):
    from google.oauth2 import service_account

    key = tmp_path / "key.json"
    key.write_text(json.dumps(_service_account_info()))
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", json.dumps({"gcs": {"token": str(key)}}))
    client = gcs_state._gcs_client()
    assert isinstance(client._credentials, service_account.Credentials)
    assert client.project == "from-key-file"
