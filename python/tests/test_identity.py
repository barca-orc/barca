"""Identity diagnostics use the selected driver's credentials; no real cloud accounts."""

import asyncio
import base64
import io
import json
from types import SimpleNamespace

import pytest
from barca import _doctor, _identity, _storage, _transfer


def token(**claims):
    body = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
    return f"header.{body}.signature"


@pytest.fixture(autouse=True)
def isolated_credentials(monkeypatch):
    for name in (
        "BARCA_STORAGE_OPTIONS",
        "AWS_PROFILE",
        "AWS_ACCESS_KEY_ID",
        "AZURE_TENANT_ID",
        "AZURE_CLIENT_ID",
        "AZURE_STORAGE_ACCOUNT_KEY",
    ):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(_identity, "_sync", lambda fs, fn: asyncio.run(fn()))


def test_aws_lookup_uses_same_explicit_credentials_and_session(monkeypatch):
    pytest.importorskip("aiobotocore")
    seen = {}

    class Client:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            pass

        async def get_caller_identity(self):
            return {"Arn": "arn:aws:iam::123:user/test", "Account": "123"}

    class Session:
        def create_client(self, service, **kwargs):
            seen.update(service=service, **kwargs)
            return Client()

    async def set_session():
        seen["session_initialized"] = True

    fs = SimpleNamespace(
        key="selected-key",
        secret="selected-secret",
        token="selected-token",
        client_kwargs={"region_name": "us-east-2"},
        session=Session(),
        set_session=set_session,
    )
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    result = _identity.resolve("s3://bucket")
    assert result["subject"] == "arn:aws:iam::123:user/test"
    assert result["account"] == "123"
    assert seen["session_initialized"]
    assert seen["service"] == "sts"
    assert seen["aws_access_key_id"] == fs.key
    assert seen["aws_secret_access_key"] == fs.secret
    assert seen["aws_session_token"] == fs.token
    assert seen["region_name"] == "us-east-2"
    assert "selected-secret" not in json.dumps(result)
    assert "selected-key" not in json.dumps(result)


def test_azure_effective_tenant_and_principal_from_selected_credential(monkeypatch):
    async def get_token(scope):
        assert scope == "https://storage.azure.com/.default"
        return SimpleNamespace(
            token=token(oid="object-id", tid="actual-tenant", appid="application-id")
        )

    fs = SimpleNamespace(
        account_name="storage-account", credential=SimpleNamespace(get_token=get_token)
    )
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    monkeypatch.setenv("AZURE_TENANT_ID", "configured-tenant")
    result = _identity.resolve("abfs://container")
    assert result["subject"] == "object-id"
    assert result["tenant"] == "actual-tenant"
    assert result["configured_tenant"] == "configured-tenant"
    assert result["account"] == "storage-account"


def test_gcs_service_account_and_user_identity(monkeypatch):
    credential = SimpleNamespace(
        service_account_email="service@example.test", token="private-token"
    )
    fs = SimpleNamespace(
        project="selected-project", credentials=SimpleNamespace(credentials=credential)
    )
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    monkeypatch.setattr(
        _identity,
        "urlopen",
        lambda *args, **kwargs: pytest.fail("service account needs no network lookup"),
    )
    assert _identity.resolve("gs://bucket")["subject"] == "service@example.test"
    credential.service_account_email = "default"

    def tokeninfo(url, timeout):
        assert url == "https://oauth2.googleapis.com/tokeninfo?access_token=private-token"
        assert timeout == 3
        return io.BytesIO(b'{"email":"user@example.test"}')

    monkeypatch.setattr(_identity, "urlopen", tokeninfo)
    result = _identity.resolve("gs://bucket")
    assert result["subject"] == "user@example.test"
    assert "private-token" not in json.dumps(result)


@pytest.mark.parametrize(
    "endpoint,provider",
    [
        ("https://account.r2.cloudflarestorage.com", "r2"),
        ("https://objects.example.test", "s3-compatible"),
        ("https://r2.cloudflarestorage.com.evil.test", "s3-compatible"),
    ],
)
def test_custom_endpoints_use_fingerprint_without_sts(monkeypatch, endpoint, provider):
    fs = SimpleNamespace(key="private-key", client_kwargs={"endpoint_url": endpoint})
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    result = _identity.resolve("s3://bucket")
    assert result["provider"] == provider
    assert result["subject"].startswith("access key fingerprint ")
    assert "private-key" not in json.dumps(result)


def test_lookup_failure_preserves_reachability_without_leaking_token(monkeypatch):
    fs = SimpleNamespace(
        project="project",
        credentials=SimpleNamespace(credentials=SimpleNamespace(token="private-token")),
    )
    monkeypatch.setattr(_storage, "get_fs", lambda uri: fs)
    monkeypatch.setattr(_storage, "check_store", lambda uri: None)

    def unavailable(*args, **kwargs):
        raise OSError("https://oauth2.googleapis.com/tokeninfo?access_token=private-token")

    monkeypatch.setattr(_identity, "urlopen", unavailable)
    result = _doctor.probe({"name": "artifacts", "uri": "gs://bucket"})
    assert result["ok"]
    assert result["data"]["identity"]["lookup_status"] == "unresolved"
    assert result["data"]["identity"]["lookup_error"] == "OSError"
    assert "unresolved" in result["detail"]
    assert "private-token" not in json.dumps(result)


def test_auth_error_includes_configured_identity_and_recovery_before_provider(monkeypatch):
    monkeypatch.setenv("AZURE_TENANT_ID", "selected-tenant")
    monkeypatch.setenv(
        "BARCA_STORAGE_OPTIONS",
        json.dumps(
            {"abfs": {"account_name": "selected-account", "client_secret": "private-secret"}}
        ),
    )

    def denied(uri):
        raise PermissionError("provider denied private-secret")

    monkeypatch.setattr(_storage, "check_store", denied)
    result = _doctor.probe({"name": "artifacts", "uri": "abfs://container"})
    assert not result["ok"]
    assert "selected-account" in result["detail"]
    assert "selected-tenant" in result["detail"]
    assert "az login --tenant selected-tenant" in result["detail"]
    assert result["detail"].index("Identity:") < result["detail"].index("provider denied")
    assert "private-secret" not in json.dumps(result)
    transfer_error = _transfer._error(
        {"id": 1, "remote": "abfs://container/object"}, PermissionError("denied"), 1
    )
    assert "selected-tenant" in transfer_error["message"]
    assert (
        _storage.describe_error("/local/path", PermissionError("denied"))
        == "PermissionError: denied"
    )
    assert (
        _storage.describe_error("s3://bucket", ValueError("unrelated")) == "ValueError: unrelated"
    )
    assert (
        _storage.describe_error(
            "s3://bucket", PermissionError("Identity: already annotated")
        ).count("Identity:")
        == 1
    )


def test_provider_errors_redact_environment_credentials(monkeypatch):
    monkeypatch.setenv("AWS_ACCESS_KEY_ID", "private-access-id")
    monkeypatch.setenv("AWS_SECRET_ACCESS_KEY", "private-secret-key")
    monkeypatch.setenv("AWS_SESSION_TOKEN", "private-session-token")
    message = _storage.describe_error(
        "s3://bucket",
        PermissionError("denied private-access-id private-secret-key private-session-token"),
    )
    assert "private-" not in message
    assert "explicit access key" in message
    assert "aws sts get-caller-identity" in message


def test_explicit_s3_credential_recovery_does_not_select_an_unrelated_profile(monkeypatch):
    monkeypatch.setenv("AWS_PROFILE", "unrelated-profile")
    monkeypatch.setenv(
        "BARCA_STORAGE_OPTIONS",
        json.dumps({"s3": {"key": "selected-access-key", "secret": "selected-secret"}}),
    )
    hint = _identity.error_hint("s3://bucket", PermissionError("denied"))
    assert "storage options or AWS environment variables" in hint
    assert "those same credentials" in hint
    assert "unrelated-profile" not in hint
    assert "selected-access-key" not in hint
    assert "selected-secret" not in hint
