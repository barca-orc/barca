"""Credential context and best available principal identity; never expose credential values."""

import base64
import hashlib
import inspect
import json
import os
import shlex
from urllib.parse import urlencode, urlsplit
from urllib.request import urlopen


def _fingerprint(value):
    return (
        hashlib.sha256(value.encode()).hexdigest()[:16]
        if isinstance(value, str) and value
        else None
    )


def _claims(token):
    try:
        payload = token.split(".")[1]
        result = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
        return result if isinstance(result, dict) else {}
    except (ValueError, IndexError, UnicodeError):
        return {}


def context(uri, fs=None):
    """Configured target and credential source, available even when authentication failed."""
    from barca import _storage

    scheme = _storage._scheme(uri)
    protocol = {"s3a": "s3", "abfss": "abfs", "gs": "gcs"}.get(scheme, scheme)
    options = _storage.storage_options(protocol)
    result = {
        "provider": protocol,
        "subject": None,
        "account": None,
        "source": "provider default credential chain",
        "recovery": "Verify the selected store's credentials and list permissions.",
    }
    if protocol == "s3":
        endpoint = (getattr(fs, "client_kwargs", {}) or options.get("client_kwargs", {})).get(
            "endpoint_url", ""
        )
        profile = options.get("profile") or os.environ.get("AWS_PROFILE", "default")
        key = getattr(fs, "key", None) or options.get("key") or os.environ.get("AWS_ACCESS_KEY_ID")
        result["source"] = f"AWS profile {profile}" if not key else "explicit access key"
        if _fingerprint(key):
            result["credential_fingerprint"] = _fingerprint(key)
        result["recovery"] = (
            "Reload the selected S3 access key/session token in storage options or AWS environment variables; run aws sts get-caller-identity with those same credentials."
            if key
            else f"Run aws sts get-caller-identity using AWS_PROFILE={shlex.quote(profile)}; refresh that profile's credentials (for SSO: aws sso login --profile {shlex.quote(profile)})."
        )
        hostname = (urlsplit(endpoint).hostname or "").lower()
        if hostname.endswith(".r2.cloudflarestorage.com"):
            result.update(
                provider="r2",
                account=hostname.split(".")[0],
                source="R2 API access key",
                recovery="Reload or rotate the bucket-scoped R2 API credentials in the s3 storage options.",
            )
        elif endpoint and not (hostname == "amazonaws.com" or hostname.endswith(".amazonaws.com")):
            result.update(
                provider="s3-compatible",
                recovery="Verify the selected endpoint's access key and bucket list permission.",
            )
    elif protocol == "abfs":
        account = (
            getattr(fs, "account_name", None)
            or options.get("account_name")
            or os.environ.get("AZURE_STORAGE_ACCOUNT_NAME")
        )
        if not account and "@" in urlsplit(uri).netloc:
            account = urlsplit(uri).netloc.split("@", 1)[1].split(".")[0]
        tenant = options.get("tenant_id") or os.environ.get("AZURE_TENANT_ID")
        result.update(provider="azure", account=account, configured_tenant=tenant)
        result["source"] = "Azure DefaultAzureCredential chain"
        client = options.get("client_id") or os.environ.get("AZURE_CLIENT_ID")
        if client:
            result.update(source="Azure application credentials", configured_client_id=client)
            result["recovery"] = (
                "Verify AZURE_CLIENT_ID and AZURE_CLIENT_SECRET for the configured tenant, or clear those overrides to use Azure CLI credentials."
            )
        else:
            tenant_flag = f" --tenant {shlex.quote(tenant)}" if tenant else ""
            result["recovery"] = (
                f"Run az login{tenant_flag}; verify this identity has the required Storage Blob Data role on the selected account."
            )
        key = (
            getattr(fs, "account_key", None)
            or options.get("account_key")
            or os.environ.get("AZURE_STORAGE_ACCOUNT_KEY")
        )
        sas = getattr(fs, "sas_token", None) or options.get("sas_token")
        if key or sas:
            result.update(
                source="Azure shared key" if key else "Azure SAS",
                credential_fingerprint=_fingerprint(key or sas),
            )
            result["recovery"] = (
                "Reload the selected account's shared key or SAS; verify its read and list permissions."
            )
    elif protocol == "gcs":
        result.update(
            provider="gcs",
            account=getattr(fs, "project", None),
            source="Google application default or explicit credentials",
            recovery="Run gcloud auth application-default login, or point GOOGLE_APPLICATION_CREDENTIALS at the intended identity; verify bucket list permission.",
        )
    elif protocol == "memory":
        result.update(
            subject="in-process memory filesystem",
            source="no credentials",
            recovery="No cloud identity is required.",
        )
    return result


def error_hint(uri, exc, fs=None):
    """One safe human line before an authentication/authorization provider error."""
    from barca import _storage

    if not _storage.is_remote(uri) or "Identity:" in str(exc):
        return None
    names = {type(exc).__name__, type(exc.__cause__).__name__ if exc.__cause__ else ""}
    credential_errors = {
        "NoCredentialsError",
        "PartialCredentialsError",
        "DefaultCredentialsError",
        "CredentialUnavailableError",
        "ClientAuthenticationError",
        "RefreshError",
        "TokenRefreshError",
        "AuthenticationError",
    }
    if not (
        isinstance(exc, PermissionError)
        or _storage.http_status(exc) in (401, 403)
        or names & credential_errors
    ):
        return None
    details = context(uri, fs)
    account = f"; selected account {details['account']}" if details["account"] else ""
    tenant = (
        f"; configured tenant {details['configured_tenant']}"
        if details.get("configured_tenant")
        else ""
    )
    return f"Identity: {details['source']}{account}{tenant}; effective principal unavailable after authentication failure. {details['recovery']}"


def _sync(fs, fn):
    from fsspec.asyn import sync

    return sync(fs.loop, fn)


def resolve(uri):
    """Identify the credentials that successfully probed the store; lookup failures stay explicit."""
    from barca import _storage

    fs = _storage.get_fs(uri)
    result = context(uri, fs)
    try:
        if result["provider"] == "s3":

            async def caller():
                from aiobotocore.config import AioConfig

                await fs.set_session()
                credentials = {
                    name: value
                    for name, value in (
                        ("aws_access_key_id", fs.key),
                        ("aws_secret_access_key", fs.secret),
                        ("aws_session_token", fs.token),
                    )
                    if value
                }
                region = (
                    fs.client_kwargs.get("region_name")
                    or os.environ.get("AWS_REGION")
                    or os.environ.get("AWS_DEFAULT_REGION")
                )
                if region:
                    credentials["region_name"] = region
                config = AioConfig(connect_timeout=3, read_timeout=3, retries={"max_attempts": 0})
                async with fs.session.create_client("sts", config=config, **credentials) as client:
                    return await client.get_caller_identity()

            caller_info = _sync(fs, caller)
            result.update(subject=caller_info["Arn"], account=caller_info["Account"])
        elif result["provider"] in ("r2", "s3-compatible"):
            if result.get("credential_fingerprint"):
                result["subject"] = f"access key fingerprint {result['credential_fingerprint']}"
        elif result["provider"] == "azure":
            credential = getattr(fs, "credential", None)
            if result.get("credential_fingerprint"):
                result["subject"] = f"credential fingerprint {result['credential_fingerprint']}"
            elif hasattr(credential, "get_token"):

                async def token():
                    access = credential.get_token("https://storage.azure.com/.default")
                    return await access if inspect.isawaitable(access) else access

                claims = _claims(_sync(fs, token).token)
                result.update(
                    subject=claims.get("preferred_username")
                    or claims.get("upn")
                    or claims.get("oid")
                    or claims.get("sub"),
                    tenant=claims.get("tid"),
                    application=claims.get("appid") or claims.get("azp"),
                )
        elif result["provider"] == "gcs":
            wrapped = getattr(fs, "credentials", None)
            credential = getattr(wrapped, "credentials", wrapped)
            email = getattr(credential, "service_account_email", None)
            if email and email != "default":
                result["subject"] = email
            elif isinstance(credential, dict) and credential.get("client_email"):
                result["subject"] = credential["client_email"]
            else:
                claims = _claims(getattr(credential, "id_token", "") or "")
                result["subject"] = claims.get("email") or claims.get("sub")
                if not result["subject"] and getattr(credential, "token", None):
                    endpoint = "https://oauth2.googleapis.com/tokeninfo?" + urlencode(
                        {"access_token": credential.token}
                    )
                    # A token goes only to Google's fixed identity endpoint. Never include
                    # this URL or an exception's URL in a diagnostic.
                    with urlopen(endpoint, timeout=3) as reply:
                        info = json.load(reply)
                    result["subject"] = info.get("email") or info.get("sub")
    except Exception as exc:
        # A reachable store can have an identity lookup blocked separately. Keep its probe
        # result and say exactly what remains unknown, without serializing SDK credentials.
        result["lookup_error"] = type(exc).__name__
    if result["subject"] is None:
        result["lookup_status"] = "unresolved"
    return result
