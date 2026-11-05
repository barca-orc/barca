"""Signed URLs for objects in S3, GCS and Azure (`barca get -o url`, `barca.signed_url`).


A signed URL is a time-limited HTTPS link that anyone holding it can GET without
credentials. fsspec's own `sign()` covers only part of what barca's default credentials are:
adlfs signs only with an account key (barca authenticates with DefaultAzureCredential) and
gcsfs only with a service-account key file. So each cloud is signed here, with the
filesystem barca already built for the store (`get_fs`), so its credentials and options
(BARCA_STORAGE_OPTIONS: an S3 endpoint for R2 or MinIO, an Azure connection string) are the
same ones every other store access uses.

A signed URL is a bearer credential: it is returned, never printed or logged here, and no
error message below contains one.
"""

from __future__ import annotations

import datetime
import math
from typing import Any
from urllib.parse import urlsplit

from barca import _storage

# The longest a link may live: SigV4 and Azure user delegation keys allow at most 7 days.
MAX_SIGNED_URL_SECONDS = 7 * 24 * 3600
DEFAULT_SIGNED_URL_SECONDS = 3600
_SIGNABLE = ("s3", "s3a", "gs", "gcs", "abfs", "abfss")


class SignError(Exception):
    """A URL could not be signed. ``kind`` says whose problem it is:

    - ``usage``: the request is wrong (not a bucket URI, an expiry over 7 days);
    - ``missing``: the object is not in the store;
    - ``credentials``: the credentials in use cannot sign, or may not read the object;
    - ``store``: the store could not be reached or answered with an error.
    """

    def __init__(self, kind: str, message: str) -> None:
        super().__init__(message)
        self.kind = kind


def expiry_seconds(expires: Any) -> int:
    """``expires`` (seconds, or a ``datetime.timedelta``) as whole seconds, at most 7 days."""
    if isinstance(expires, datetime.timedelta):
        seconds = expires.total_seconds()
    elif isinstance(expires, int | float) and not isinstance(expires, bool):
        seconds = float(expires)
    else:
        raise SignError(
            "usage",
            f"expires must be seconds or a datetime.timedelta, not {type(expires).__name__}",
        )
    if not math.isfinite(seconds) or seconds < 1:
        raise SignError("usage", f"expires must be at least 1 second (got {expires!r})")
    if seconds > MAX_SIGNED_URL_SECONDS:
        raise SignError(
            "usage",
            f"expires is at most 7 days ({MAX_SIGNED_URL_SECONDS} seconds); got {int(seconds)} "
            "seconds. A signed URL cannot be revoked before it expires, so keep it short.",
        )
    return int(seconds)


def _disposition(filename: str) -> str:
    """`Content-Disposition` that makes a browser download the object as ``filename``."""
    safe = "".join(c if c.isprintable() and c not in '"\\/' else "_" for c in filename)
    return f'attachment; filename="{safe or "download"}"'


def sign_url(
    uri: str, expires: Any = DEFAULT_SIGNED_URL_SECONDS, filename: str | None = None
) -> tuple[str, datetime.datetime]:
    """A read-only HTTPS URL for the object at ``uri`` and when it stops working (UTC).

    ``uri`` is an ``s3://``/``s3a://`` (also R2 and MinIO, through the endpoint in
    BARCA_STORAGE_OPTIONS), ``gs://``/``gcs://`` or ``abfs://``/``abfss://`` object. The object
    must exist. The link downloads as ``filename`` (default: the object's own name). Raises
    SignError with the URI (never a signed URL) in its message.
    """
    seconds = expiry_seconds(expires)
    scheme = _storage._scheme(uri)
    where = _storage.safe_error(str(uri))
    if scheme not in _SIGNABLE:
        raise SignError(
            "usage",
            f"cannot sign {where}: signed URLs are for objects in S3 (s3://, s3a://), GCS (gs://, "
            "gcs://) or Azure (abfs://, abfss://); a local path or file:// has no URL to share",
        )
    name = filename or urlsplit(str(uri)).path.rstrip("/").rsplit("/", 1)[-1] or "download"
    try:
        fs = _storage.get_fs(uri)
    except ImportError as exc:
        raise SignError("usage", f"cannot sign {where}: {exc}") from exc
    except ValueError as exc:
        raise SignError("usage", f"cannot sign {where}: {_storage.safe_error(str(exc))}") from exc
    if scheme in ("s3", "s3a"):
        signer = _sign_s3
    elif scheme in ("gs", "gcs"):
        signer = _sign_gcs
    else:
        signer = _sign_azure
    try:
        return signer(fs, str(uri), seconds, _disposition(name))
    except SignError:
        raise
    except Exception as exc:  # noqa: BLE001 — every SDK error becomes one SignError
        raise SignError(
            _error_kind(exc), f"cannot sign {where}: {_storage.describe_error(uri, exc)}"
        ) from exc


def _error_kind(exc: BaseException) -> str:
    if isinstance(exc, FileNotFoundError) or _storage.http_status(exc) == 404:
        return "missing"
    if isinstance(exc, PermissionError) or _storage.http_status(exc) in (401, 403):
        return "credentials"
    name = type(exc).__name__
    if name in (
        "NoCredentialsError",
        "DefaultCredentialsError",
        "ClientAuthenticationError",
        "CredentialUnavailableError",
        "RefreshError",
        "ResourceNotFoundError",
    ):
        return "missing" if name == "ResourceNotFoundError" else "credentials"
    return "store"


def _require_object(fs: Any, uri: str) -> None:
    """One HEAD: a typo or a missing upload must not become a link that answers 404 later."""
    try:
        info = fs.info(uri)
    except FileNotFoundError as exc:
        raise SignError("missing", f"{_storage.safe_error(uri)} is not in the store") from exc
    if info.get("type") == "directory":
        raise SignError("usage", f"{_storage.safe_error(uri)} is a directory, not an object")


def _now() -> datetime.datetime:
    return datetime.datetime.now(datetime.UTC).replace(microsecond=0)


def _sign_s3(fs: Any, uri: str, seconds: int, disposition: str) -> tuple[str, datetime.datetime]:
    """Presigned GET with SigV4, through an s3fs client built from the store's own options (so
    the endpoint, profile and credential chain are the same)."""
    fs.invalidate_cache()
    _require_object(fs, uri)
    fs = _s3_signing_fs()
    expires_at = _now() + datetime.timedelta(seconds=seconds)
    url = fs.url(uri, expires=seconds, ResponseContentDisposition=disposition)
    # A URL signed with temporary credentials stops working when they expire.
    credential_expiry = _s3_credential_expiry(fs)
    if credential_expiry is not None and credential_expiry < expires_at:
        expires_at = credential_expiry.replace(microsecond=0)
    return url, expires_at


def _s3_signing_fs() -> Any:
    """The store's s3fs options with SigV4 forced: botocore may otherwise presign with SigV2,
    which buckets created since 2020 reject and which has no 7-day limit."""
    import fsspec

    options = dict(_storage.storage_options("s3"))
    config = dict(options.pop("config_kwargs", None) or {})
    config["signature_version"] = "s3v4"
    return fsspec.filesystem("s3", config_kwargs=config, skip_instance_cache=True, **options)


def _s3_credential_expiry(fs: Any) -> datetime.datetime | None:
    try:
        signer = fs.s3._request_signer
        credentials = getattr(signer, "_credentials", None)
        expiry = getattr(credentials, "_expiry_time", None)
    except Exception:  # noqa: BLE001 — best effort: unknown means "as requested"
        return None
    if isinstance(expiry, datetime.datetime):
        if expiry.tzinfo is None:
            expiry = expiry.replace(tzinfo=datetime.UTC)
        return expiry.astimezone(datetime.UTC)
    return None


GCS_IMPERSONATE_HINT = (
    "user credentials (gcloud auth application-default login) cannot sign URLs. Sign as a "
    "service account: gcloud auth application-default login "
    "--impersonate-service-account=SA_EMAIL (you need roles/iam.serviceAccountTokenCreator on "
    "it), or point GOOGLE_APPLICATION_CREDENTIALS at a service-account key"
)


def _gcs_signing_credentials(fs: Any) -> Any:
    """The credentials gcsfs authenticates with, or Google's default ones when it has none."""
    credentials = getattr(getattr(fs, "credentials", None), "credentials", None)
    from google.auth.credentials import AnonymousCredentials

    if credentials is None or isinstance(credentials, AnonymousCredentials):
        import google.auth

        credentials, _ = google.auth.default(
            scopes=["https://www.googleapis.com/auth/cloud-platform"]
        )
    return credentials


def _sign_gcs(fs: Any, uri: str, seconds: int, disposition: str) -> tuple[str, datetime.datetime]:
    """V4 signing: locally with a key, else through IAM signBlob as the service account."""
    fs.invalidate_cache()
    _require_object(fs, uri)
    bucket, key = fs.split_path(uri)[:2]
    credentials = _gcs_signing_credentials(fs)
    expires_at = _now() + datetime.timedelta(seconds=seconds)
    endpoint = getattr(fs, "_location", None) or "https://storage.googleapis.com"
    return gcs_signed_url(credentials, bucket, key, seconds, disposition, endpoint), expires_at


def gcs_signed_url(
    credentials: Any, bucket: str, key: str, seconds: int, disposition: str, endpoint: str
) -> str:
    """A V4 signed GET for ``gs://bucket/key`` with ``credentials``."""
    from google.auth import credentials as google_credentials
    from google.cloud import storage

    # The client only supplies defaults (the universe domain); the signing credentials are
    # passed explicitly below.
    blob = storage.Client.create_anonymous_client().bucket(bucket).blob(key)
    common = {
        "version": "v4",
        "expiration": datetime.timedelta(seconds=seconds),
        "method": "GET",
        "response_disposition": disposition,
        "api_access_endpoint": endpoint,
    }
    if isinstance(credentials, google_credentials.Signing) and getattr(
        credentials, "service_account_email", None
    ):
        # A service-account key signs locally; impersonated credentials sign through IAM.
        return blob.generate_signed_url(credentials=credentials, **common)
    if type(credentials).__module__ == "google.oauth2.credentials":
        raise SignError("credentials", GCS_IMPERSONATE_HINT)
    # No key: workload identity, the GCE/GKE metadata server, an external account. Sign through
    # IAM signBlob as the credential's own service account.
    import google.auth.transport.requests

    if not getattr(credentials, "valid", False) or not getattr(credentials, "token", None):
        credentials.refresh(google.auth.transport.requests.Request())
    email = getattr(credentials, "service_account_email", None)
    if not email or email == "default":
        raise SignError(
            "credentials",
            f"the Google credentials in use ({type(credentials).__name__}) have no service "
            "account to sign as. Use a service account (workload identity, a key, or "
            "impersonation: gcloud auth application-default login "
            "--impersonate-service-account=SA_EMAIL)",
        )
    try:
        return blob.generate_signed_url(
            service_account_email=email, access_token=credentials.token, **common
        )
    except Exception as exc:  # noqa: BLE001 — google.auth TransportError and friends
        raise SignError(
            "credentials",
            f"IAM signBlob as {email} failed: it needs iam.serviceAccounts.signBlob on that "
            f"account (roles/iam.serviceAccountTokenCreator). {type(exc).__name__}: "
            f"{_storage.safe_error(_storage.first_line(exc))}",
        ) from exc


def _azure_service(fs: Any) -> tuple[Any, str | None]:
    """A synchronous BlobServiceClient with adlfs's settings, and the account key if any."""
    from azure.storage.blob import BlobServiceClient

    connection_string = getattr(fs, "connection_string", None)
    if connection_string:
        service = BlobServiceClient.from_connection_string(connection_string)
        return service, getattr(service.credential, "account_key", None)
    account_url = getattr(fs, "account_url", None) or (
        f"https://{fs.account_name}.blob.core.windows.net"
    )
    if getattr(fs, "account_key", None):
        credential = {"account_name": fs.account_name, "account_key": fs.account_key}
        return BlobServiceClient(account_url, credential=credential), fs.account_key
    if getattr(fs, "sas_token", None):
        raise SignError(
            "credentials",
            "a SAS token cannot sign new links. Configure an account key, or sign in with an "
            "Entra identity (DefaultAzureCredential) that may create user delegation keys",
        )
    import azure.identity

    client_id = getattr(fs, "client_id", None)
    client_secret = getattr(fs, "client_secret", None)
    tenant_id = getattr(fs, "tenant_id", None)
    if client_id and client_secret and tenant_id:
        credential = azure.identity.ClientSecretCredential(tenant_id, client_id, client_secret)
    else:
        credential = azure.identity.DefaultAzureCredential()
    return BlobServiceClient(account_url, credential=credential), None


def _sign_azure(fs: Any, uri: str, seconds: int, disposition: str) -> tuple[str, datetime.datetime]:
    """A read-only blob SAS: a service SAS with an account key, else a user delegation SAS."""
    container, blob = fs.split_path(uri)[:2]
    service, account_key = _azure_service(fs)
    client = service.get_blob_client(container, blob)
    try:
        client.get_blob_properties()
    except Exception as exc:
        if type(exc).__name__ == "ResourceNotFoundError" or _storage.http_status(exc) == 404:
            raise SignError("missing", f"{_storage.safe_error(uri)} is not in the store") from exc
        raise
    expires_at = _now() + datetime.timedelta(seconds=seconds)
    sas = azure_blob_sas(service, container, blob, expires_at, disposition, account_key)
    return f"{client.url}?{sas}", expires_at


def azure_blob_sas(
    service: Any,
    container: str,
    blob: str,
    expires_at: datetime.datetime,
    disposition: str,
    account_key: str | None = None,
) -> str:
    """The SAS query string: signed with the account key, or with a user delegation key that
    lives exactly as long as the link (needs generateUserDelegationKey on the account)."""
    from azure.storage.blob import BlobSasPermissions, generate_blob_sas

    common = {
        "account_name": service.account_name,
        "container_name": container,
        "blob_name": blob,
        "permission": BlobSasPermissions(read=True),
        "expiry": expires_at,
        "content_disposition": disposition,
    }
    if account_key:
        return generate_blob_sas(account_key=account_key, **common)
    try:
        key = service.get_user_delegation_key(_now(), expires_at)
    except Exception as exc:  # noqa: BLE001
        raise SignError(
            "credentials",
            "could not get a user delegation key: the identity needs "
            "Microsoft.Storage/storageAccounts/blobServices/generateUserDelegationKey/action "
            "(Storage Blob Delegator, or Storage Blob Data Contributor) on the account. "
            f"{type(exc).__name__}: {_storage.safe_error(_storage.first_line(exc))}",
        ) from exc
    return generate_blob_sas(user_delegation_key=key, **common)
