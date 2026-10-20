"""Signed URLs: `barca.signed_url(uri)` and `barca get -o url` (RFC-0011, #452).

Three layers:

- the expiry rule and the per-backend signing code, without a store (the GCS signBlob path,
  user credentials, the Azure user delegation request);
- each backend against its emulator (MinIO, Azurite, fake-gcs-server): MinIO and Azurite
  check the signature, so a plain `urllib` GET proves the link works and that it expires;
  fake-gcs-server does not, so the GCS link is checked for its shape;
- the CLI on MinIO, including where the URL must never appear: stderr, `barca logs`,
  telemetry spans and the metadata database.

On a machine without the emulators the backend tests skip; on CI (`BARCA_TEST_*` set) an
unreachable emulator fails them (`emulators.require`).
"""

import base64
import datetime
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

import pytest

import barca
from barca import _storage, _value
from barca.api import _find_binary

from . import emulators
from .test_state_backends import (
    AZURITE_ACCOUNT,
    AZURITE_CONN,
    AZURITE_HOST,
    AZURITE_KEY,
    GCS_ENDPOINT,
    S3_ENDPOINT,
    S3_KEY,
    S3_SECRET,
    _hostport,
    _reachable,
)
from .test_telemetry_datadog import FakeAgent, datadog

ROOT = Path(__file__).resolve().parents[2]
EXPIRY_CASES = json.loads((ROOT / "crates/barca-cli/src/expiry_cases.json").read_text())
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST", "DD_")
BODY = b'{"total": 3}'


# ─── expiry: the table the CLI's parser shares (crates/barca-cli/src/expiry_cases.json) ───


@pytest.mark.parametrize(
    "case", [c for c in EXPIRY_CASES if "seconds" in c], ids=lambda c: c["input"]
)
def test_expiry_follows_the_shared_table(case):
    for given in (case["seconds"], datetime.timedelta(seconds=case["seconds"])):
        if "error" in case:
            with pytest.raises(_storage.SignError) as err:
                _storage.expiry_seconds(given)
            assert err.value.kind == "usage"
            assert ("7 days" if case["error"] == "too_long" else "at least 1 second") in str(
                err.value
            )
        else:
            assert _storage.expiry_seconds(given) == case["seconds"]


@pytest.mark.parametrize("bad", ["1h", None, True, float("nan")])
def test_expiry_is_seconds_or_a_timedelta(bad):
    with pytest.raises(_storage.SignError) as err:
        _storage.expiry_seconds(bad)
    assert err.value.kind == "usage"


@pytest.mark.parametrize(
    "uri", ["/tmp/x.json", "file:///tmp/x.json", "memory://b/x.json", "http://h/x.json"]
)
def test_only_bucket_objects_can_be_signed(uri):
    with pytest.raises(barca.BarcaError) as err:
        barca.signed_url(uri)
    assert (err.value.kind, err.value.code) == ("usage", 2)
    assert "signed URLs are for objects in S3" in str(err.value)


def test_an_expiry_over_seven_days_is_refused_before_any_store_is_touched():
    with pytest.raises(barca.BarcaError) as err:
        barca.signed_url("s3://bucket/x.json", expires=datetime.timedelta(days=8))
    assert (err.value.kind, err.value.code) == ("usage", 2)
    assert "at most 7 days" in str(err.value)


def test_the_download_name_is_quoted_and_cannot_break_the_header():
    assert _storage._disposition("total.json") == 'attachment; filename="total.json"'
    assert _storage._disposition('a"b/c\n.csv') == 'attachment; filename="a_b_c_.csv"'


# ─── GCS without an emulator: signBlob and user credentials ──────────────────


class _KeylessCredentials:
    """What workload identity or the GCE metadata server gives: a token, an email, no key."""

    service_account_email = "runner@proj.iam.gserviceaccount.com"
    token = "ya29.token"
    valid = True


def test_gcs_keyless_credentials_sign_through_iam_sign_blob(monkeypatch):
    from google.cloud.storage import _signing

    calls = []

    def sign_blob(message, access_token, service_account_email, universe_domain="googleapis.com"):
        calls.append((access_token, service_account_email, universe_domain))
        return base64.b64encode(b"\x01\x02signature").decode()

    monkeypatch.setattr(_signing, "_sign_message", sign_blob)
    url = _storage.gcs_signed_url(
        _KeylessCredentials(),
        "bucket",
        "proj/artifacts/total.json",
        600,
        _storage._disposition("total.json"),
        "https://storage.googleapis.com",
    )
    assert calls == [("ya29.token", "runner@proj.iam.gserviceaccount.com", "googleapis.com")]
    parts = urllib.parse.urlsplit(url)
    query = urllib.parse.parse_qs(parts.query)
    assert parts.path == "/bucket/proj/artifacts/total.json"
    assert query["X-Goog-Algorithm"] == ["GOOG4-RSA-SHA256"]
    assert query["X-Goog-Credential"][0].startswith("runner@proj.iam.gserviceaccount.com/")
    assert query["X-Goog-Expires"] == ["600"]
    assert query["X-Goog-Signature"] == [b"\x01\x02signature".hex()]
    assert query["response-content-disposition"] == ['attachment; filename="total.json"']


def test_gcs_sign_blob_refused_names_the_role_to_grant(monkeypatch):
    from google.auth.exceptions import TransportError
    from google.cloud.storage import _signing

    def refused(*args, **kwargs):
        raise TransportError("Error calling the IAM signBytes API: 403 PERMISSION_DENIED")

    monkeypatch.setattr(_signing, "_sign_message", refused)
    with pytest.raises(_storage.SignError) as err:
        _storage.gcs_signed_url(
            _KeylessCredentials(), "b", "k", 60, "attachment", "https://storage.googleapis.com"
        )
    assert err.value.kind == "credentials"
    assert "roles/iam.serviceAccountTokenCreator" in str(err.value)
    assert "runner@proj.iam.gserviceaccount.com" in str(err.value)


def test_gcs_user_credentials_are_told_to_impersonate_a_service_account():
    from google.oauth2.credentials import Credentials

    user = Credentials(token="ya29.user", refresh_token="r", client_id="c", client_secret="s")
    with pytest.raises(_storage.SignError) as err:
        _storage.gcs_signed_url(user, "b", "k", 60, "attachment", "https://storage.googleapis.com")
    assert err.value.kind == "credentials"
    assert "--impersonate-service-account" in str(err.value)


def _service_account_key(tmp_path: Path) -> Path:
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import rsa

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ).decode()
    path = tmp_path / "service-account.json"
    path.write_text(
        json.dumps(
            {
                "type": "service_account",
                "project_id": "test",
                "private_key_id": "k1",
                "private_key": pem,
                "client_email": "signer@test.iam.gserviceaccount.com",
                "client_id": "1",
                "token_uri": "https://oauth2.googleapis.com/token",
            }
        )
    )
    return path


# ─── Azure without an emulator: the user delegation SAS ──────────────────────


def test_azure_without_a_key_signs_with_a_user_delegation_key():
    """Azurite cannot hand out delegation keys without OAuth over HTTPS, so this checks the
    request made for one and the SAS built from it."""
    from azure.storage.blob import UserDelegationKey

    expires_at = datetime.datetime(2026, 10, 20, 12, 0, tzinfo=datetime.timezone.utc)
    requests = []

    class Service:
        account_name = "acct"

        def get_user_delegation_key(self, key_start_time, key_expiry_time):
            requests.append((key_start_time, key_expiry_time))
            key = UserDelegationKey()
            key.signed_oid = "oid-1"
            key.signed_tid = "tid-1"
            key.signed_start = "2026-10-20T11:00:00Z"
            key.signed_expiry = "2026-10-20T12:00:00Z"
            key.signed_service = "b"
            key.signed_version = "2021-08-06"
            key.value = base64.b64encode(b"delegation-key-bytes").decode()
            return key

    sas = _storage.azure_blob_sas(
        Service(), "out", "proj/total.json", expires_at, 'attachment; filename="total.json"'
    )
    ((start, end),) = requests
    assert end == expires_at and start <= datetime.datetime.now(datetime.timezone.utc)
    query = urllib.parse.parse_qs(sas)
    assert query["sp"] == ["r"]  # read only
    assert query["sr"] == ["b"]  # one blob
    assert query["skoid"] == ["oid-1"] and query["sktid"] == ["tid-1"]
    assert query["se"] == ["2026-10-20T12:00:00Z"]
    assert query["rscd"] == ['attachment; filename="total.json"']
    assert "sig" in query


def test_azure_delegation_refused_names_the_permission():
    class Service:
        account_name = "acct"

        def get_user_delegation_key(self, *args):
            raise PermissionError("AuthorizationPermissionMismatch")

    with pytest.raises(_storage.SignError) as err:
        _storage.azure_blob_sas(
            Service(), "c", "b", datetime.datetime.now(datetime.timezone.utc), "attachment"
        )
    assert err.value.kind == "credentials"
    assert "generateUserDelegationKey" in str(err.value)


# ─── the helper's sign request (specs/worker-protocol.md) ────────────────────


def test_the_value_helper_answers_a_sign_request_per_object(monkeypatch, capsys):
    monkeypatch.setattr(
        "sys.stdin",
        __import__("io").StringIO(
            json.dumps(
                {
                    "op": "sign",
                    "version": _value.SIGN_VERSION,
                    "objects": [
                        {"uri": "/local/x.json", "expires": 60, "filename": "x.json"},
                        {"uri": "s3://b/x.json", "expires": 10**9, "filename": "x.json"},
                    ],
                }
            )
        ),
    )
    _value.main()
    answers = json.loads(capsys.readouterr().out)
    assert [a["kind"] for a in answers] == ["usage", "usage"]


def test_a_link_that_ends_sooner_than_asked_carries_a_note(monkeypatch):
    """Temporary credentials (an SSO session, an instance role) end the link early."""
    soon = datetime.datetime.now(datetime.UTC).replace(microsecond=0) + datetime.timedelta(
        minutes=10
    )
    monkeypatch.setattr(_storage, "sign_url", lambda uri, expires, filename: ("https://u?s", soon))
    answer = _value.sign({"uri": "s3://b/k", "expires": 3600, "filename": "k"})
    assert answer["expires_at"] == soon.isoformat().replace("+00:00", "Z")
    assert "expire at" in answer["note"] and "https://u" not in answer["note"]
    monkeypatch.setattr(
        _storage,
        "sign_url",
        lambda uri, expires, filename: ("https://u?s", soon + datetime.timedelta(minutes=50)),
    )
    assert "note" not in _value.sign({"uri": "s3://b/k", "expires": 3600, "filename": "k"})


def test_the_value_helper_refuses_an_unknown_sign_version(monkeypatch):
    monkeypatch.setattr(
        "sys.stdin", __import__("io").StringIO(json.dumps({"op": "sign", "version": 99}))
    )
    with pytest.raises(SystemExit) as exit_:
        _value.main()
    assert "reinstall matching barca packages" in str(exit_.value)


# ─── against the emulators ───────────────────────────────────────────────────


def _s3_options() -> dict:
    return {
        "s3": {"key": S3_KEY, "secret": S3_SECRET, "client_kwargs": {"endpoint_url": S3_ENDPOINT}}
    }


def _s3_bucket() -> str:
    import fsspec

    bucket = f"barca-sign-{uuid.uuid4().hex[:12]}"
    fsspec.filesystem("s3", skip_instance_cache=True, **_s3_options()["s3"]).mkdir(bucket)
    return bucket


@pytest.fixture
def s3(monkeypatch):
    emulators.require("s3", _reachable(*_hostport(S3_ENDPOINT)), S3_ENDPOINT)
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", json.dumps(_s3_options()))
    _storage._fs_cache.clear()
    yield _s3_bucket()
    _storage._fs_cache.clear()


def _fetch(url: str) -> "tuple[int, bytes, str | None]":
    try:
        with urllib.request.urlopen(url, timeout=30) as r:
            return r.status, r.read(), r.headers.get("Content-Disposition")
    except urllib.error.HTTPError as e:
        return e.code, b"", None


def test_minio_link_serves_the_bytes_then_403_after_expiry(s3, tmp_path):
    uri = f"s3://{s3}/reports/weekly.csv"
    src = tmp_path / "weekly.csv"
    src.write_bytes(b"a,b\n1,2\n")
    _storage.put_file(src, uri)

    url = barca.signed_url(uri)
    assert "X-Amz-Algorithm=AWS4-HMAC-SHA256" in url  # SigV4, whatever botocore defaults to
    assert "X-Amz-Expires=3600" in url
    status, body, disposition = _fetch(url)
    assert (status, body) == (200, b"a,b\n1,2\n")
    assert disposition == 'attachment; filename="weekly.csv"'

    short = barca.signed_url(uri, expires=1)
    time.sleep(2.5)
    assert _fetch(short)[0] == 403


def test_minio_missing_object_is_an_error_naming_the_uri(s3):
    with pytest.raises(barca.BarcaError) as err:
        barca.signed_url(f"s3://{s3}/nope.json")
    assert (err.value.kind, err.value.code) == ("infra", 3)
    assert f"s3://{s3}/nope.json is not in the store" in str(err.value)


@pytest.fixture
def azurite(monkeypatch):
    emulators.require("abfs", _reachable(*_hostport("http://" + AZURITE_HOST)), AZURITE_HOST)
    from azure.storage.blob import BlobServiceClient

    container = f"barca-sign-{uuid.uuid4().hex[:12]}"
    service = BlobServiceClient.from_connection_string(AZURITE_CONN)
    service.create_container(container)
    service.get_blob_client(container, "proj/total.json").upload_blob(BODY)
    _storage._fs_cache.clear()
    yield container
    _storage._fs_cache.clear()


@pytest.mark.parametrize("how", ["connection_string", "account_key"])
def test_azurite_account_key_sas_serves_the_bytes(azurite, monkeypatch, how):
    options = (
        {"connection_string": AZURITE_CONN}
        if how == "connection_string"
        else {
            "account_name": AZURITE_ACCOUNT,
            "account_key": AZURITE_KEY,
            "account_host": f"{AZURITE_HOST}/{AZURITE_ACCOUNT}",
        }
    )
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", json.dumps({"abfs": options}))
    if how == "account_key":
        # adlfs builds https:// from account_host; Azurite speaks http.
        real = _storage._azure_service

        def http(fs):
            fs.account_url = fs.account_url.replace("https://", "http://")
            return real(fs)

        monkeypatch.setattr(_storage, "_azure_service", http)
    url = barca.signed_url(f"abfs://{azurite}/proj/total.json", expires=120)
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
    assert query["sp"] == ["r"] and "skoid" not in query
    status, body, disposition = _fetch(url)
    assert (status, body) == (200, BODY)
    assert disposition == 'attachment; filename="total.json"'
    with pytest.raises(barca.BarcaError) as err:
        barca.signed_url(f"abfs://{azurite}/proj/missing.json")
    assert "is not in the store" in str(err.value)


def test_fake_gcs_link_has_the_v4_shape(tmp_path, monkeypatch):
    emulators.require("gcs", _reachable(*_hostport(GCS_ENDPOINT)), GCS_ENDPOINT)
    from google.auth.credentials import AnonymousCredentials
    from google.cloud import storage

    bucket = f"barca-sign-{uuid.uuid4().hex[:12]}"
    client = storage.Client(
        project="test",
        credentials=AnonymousCredentials(),
        client_options={"api_endpoint": GCS_ENDPOINT},
    )
    client.create_bucket(bucket)
    client.bucket(bucket).blob("proj/total.json").upload_from_string(BODY)
    monkeypatch.setenv(
        "BARCA_STORAGE_OPTIONS",
        json.dumps({"gcs": {"endpoint_url": GCS_ENDPOINT, "token": "anon", "project": "test"}}),
    )
    monkeypatch.setenv("GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT", "false")
    # gcsfs reads anonymously; signing falls back to the default credentials: a key here.
    monkeypatch.setenv("GOOGLE_APPLICATION_CREDENTIALS", str(_service_account_key(tmp_path)))
    _storage._fs_cache.clear()
    try:
        url = barca.signed_url(f"gs://{bucket}/proj/total.json", expires=900)
        with pytest.raises(barca.BarcaError):
            barca.signed_url(f"gs://{bucket}/proj/missing.json")
    finally:
        _storage._fs_cache.clear()
    parts = urllib.parse.urlsplit(url)
    query = urllib.parse.parse_qs(parts.query)
    assert f"{parts.scheme}://{parts.netloc}" == GCS_ENDPOINT
    assert parts.path == f"/{bucket}/proj/total.json"
    assert query["X-Goog-Algorithm"] == ["GOOG4-RSA-SHA256"]
    assert query["X-Goog-Credential"][0].startswith("signer@test.iam.gserviceaccount.com/")
    assert query["X-Goog-Expires"] == ["900"]
    assert query["response-content-disposition"] == ['attachment; filename="total.json"']
    assert len(query["X-Goog-Signature"][0]) == 512  # RSA-2048, hex


# ─── barca get -o url, on MinIO ──────────────────────────────────────────────

PIPELINE = """
from barca import asset, partitions


@asset()
def total() -> dict:
    print("computing total")
    return {"total": 3}


@asset(partitions={"region": partitions(["amer", "emea"])})
def sales(region: str) -> dict:
    return {"region": region}
"""


def _cli(cwd: Path, env: dict, *args: str) -> subprocess.CompletedProcess:
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**base, **env},
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


@pytest.fixture
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


@pytest.fixture
def store(project):
    emulators.require("s3", _reachable(*_hostport(S3_ENDPOINT)), S3_ENDPOINT)
    pytest.importorskip("s3fs")
    bucket = _s3_bucket()
    return {
        "BARCA_REMOTE_URI": f"s3://{bucket}/proj",
        "BARCA_STORAGE_OPTIONS": json.dumps(_s3_options()),
    }


def test_get_o_url_links_a_cached_asset(project, store):
    assert _cli(project, store, "get", "total", "pipeline.py", "--json").returncode == 0
    proc = _cli(project, store, "get", "total", "pipeline.py", "-o", "url", "--expires", "30m")
    assert proc.returncode == 0, proc.stderr
    (url,) = proc.stdout.splitlines()
    assert "/proj/default/artifacts/pipeline.py--total/" in url
    assert "X-Amz-Expires=1800" in url
    status, body, disposition = _fetch(url)
    assert (status, json.loads(body)) == (200, {"total": 3})
    assert disposition == 'attachment; filename="total.json"'

    out = json.loads(
        _cli(project, store, "get", "total", "pipeline.py", "-o", "url", "--json").stdout
    )
    pointer = out["final_output"]["_barca_artifact"]
    assert pointer["format"] == "json" and pointer["path"].endswith(".json")
    assert _fetch(pointer["url"])[0] == 200
    assert "X-Amz-Expires=3600" in pointer["url"]  # the default: one hour
    expires_at = datetime.datetime.fromisoformat(pointer["url_expires_at"].replace("Z", "+00:00"))
    left = expires_at - datetime.datetime.now(datetime.timezone.utc)
    assert datetime.timedelta(minutes=58) < left <= datetime.timedelta(hours=1)


def test_get_o_url_on_a_partitioned_target_links_the_selected_keys(project, store):
    proc = _cli(
        project, store, "get", "sales", "pipeline.py", "-o", "url", "--partition", "region=emea"
    )
    assert proc.returncode == 0, proc.stderr
    (url,) = proc.stdout.splitlines()
    assert "pipeline.py--sales_region_emea/" in url
    assert json.loads(_fetch(url)[1]) == {"region": "emea"}

    proc = _cli(project, store, "get", "sales", "pipeline.py", "-o", "url")
    urls = proc.stdout.splitlines()
    assert [json.loads(_fetch(u)[1])["region"] for u in urls] == ["amer", "emea"]  # -o path order


def test_a_result_missing_from_the_store_is_exit_3_naming_the_node(project, store):
    """The object behind a recorded result is gone (until #444 retries pending uploads first,
    this is also what a `local only` result gets)."""
    import fsspec

    out = json.loads(_cli(project, store, "get", "total", "pipeline.py", "--json").stdout)
    local = Path(out["final_output"]["_barca_artifact"]["path"])
    stored = f"{store['BARCA_REMOTE_URI']}/default/artifacts/{local.parent.name}/{local.name}"
    fsspec.filesystem("s3", skip_instance_cache=True, **_s3_options()["s3"]).rm(stored)

    proc = _cli(project, store, "get", "total", "pipeline.py", "-o", "url", "--json")
    assert proc.returncode == 3, proc.stderr
    assert proc.stdout == ""
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "infra"
    assert "'total'" in envelope["error"] and "is not in the store" in envelope["error"]
    assert "barca doctor" in envelope["remediation"]


def test_get_o_url_with_a_local_store_is_a_usage_error(project):
    proc = _cli(project, {}, "get", "total", "pipeline.py", "-o", "url")
    assert proc.returncode == 2
    assert "-o url needs a cloud artifact store" in proc.stderr
    assert proc.stdout == ""
    proc = _cli(
        project,
        {"BARCA_REMOTE_URI": f"file://{project}/store"},
        "get",
        "total",
        "pipeline.py",
        "-o",
        "url",
    )
    assert proc.returncode == 2
    assert "is a directory, which has no URL to share" in proc.stderr


@pytest.mark.parametrize(
    "args, message",
    [
        (["-o", "url", "--expires", "8d"], "longer than the maximum of 7d"),
        (["--expires", "1h"], "--expires applies only to -o url"),
        (["-o", "path", "--expires", "1h"], "--expires applies only to -o url"),
        (["-o", "url", "--expires", "soon"], "is not a duration"),
    ],
)
def test_expires_usage_errors(project, args, message):
    proc = _cli(project, {}, "get", "total", "pipeline.py", *args)
    assert proc.returncode == 2
    assert message in proc.stderr
    assert not (project / ".barca").exists()  # refused before anything ran


def test_the_url_appears_on_stdout_and_nowhere_else(project, store):
    """A signed URL is a bearer credential: not on stderr, not in `barca logs`, not in
    telemetry spans, not in the metadata database or anything else barca writes."""
    agent = FakeAgent()
    try:
        env = {**store, **datadog(agent)}
        proc = _cli(project, env, "get", "total", "pipeline.py", "-o", "url", "--json", "--agent")
        assert proc.returncode == 0, proc.stderr
        out = json.loads(proc.stdout)
        url = out["final_output"]["_barca_artifact"]["url"]
        signature = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)["X-Amz-Signature"][0]
        secrets = [url, signature]

        logs = _cli(project, env, "logs", out["run_id"], "--json")
        assert logs.returncode == 0, logs.stderr
        assert "computing total" in logs.stdout  # the log is there; the link is not
        history = _cli(project, env, "history", "--json")
        deadline = time.time() + 10
        while not agent.requests and time.time() < deadline:
            time.sleep(0.1)
        assert agent.requests, "telemetry never arrived"
        spans = json.dumps([r["body"] for r in agent.requests] + agent.python_spans)
        for text, where in [
            (proc.stderr, "stderr"),
            (logs.stdout + logs.stderr, "barca logs"),
            (history.stdout + history.stderr, "barca history"),
            (spans, "telemetry spans"),
        ]:
            for secret in secrets:
                assert secret not in text, f"signed URL found in {where}"
        # The metadata database and everything else under .barca (the store's own copy of the
        # state is pushed from the same file).
        files = [p for p in (project / ".barca").rglob("*") if p.is_file()]
        assert any(p.name.endswith(".db") for p in files)
        for path in files:
            data = path.read_bytes()
            assert signature.encode() not in data, f"signed URL found in {path}"
            assert urllib.parse.quote(signature).encode() not in data, path
    finally:
        agent.close()


def test_python_api_and_cli_help_name_the_feature():
    assert "signed_url" in barca.__all__
    help_text = subprocess.run(
        [_find_binary(), "get", "--help"], capture_output=True, text=True, check=True
    ).stdout
    assert "-o url" in help_text and "--expires" in help_text


if __name__ == "__main__":  # pragma: no cover
    sys.exit(pytest.main([__file__, "-v"]))
