"""The principal behind the store's credentials, probed through the provider's SDK; never
exposes credential values. The configured context and the one-line hint on an authentication
error live in `_storage` (`context`, `error_hint`), re-exported here for `barca doctor`."""

import base64
import inspect
import json
import os
from urllib.parse import urlencode
from urllib.request import urlopen

from barca import _storage
from barca._storage import context, error_hint

__all__ = ["context", "error_hint", "resolve"]


def _claims(token):
    try:
        payload = token.split(".")[1]
        result = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
        return result if isinstance(result, dict) else {}
    except (ValueError, IndexError, UnicodeError):
        return {}


def _sync(fs, fn):
    from fsspec.asyn import sync

    return sync(fs.loop, fn)


def resolve(uri):
    """Identify the credentials that successfully probed the store; lookup failures stay
    explicit."""
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
            elif (get_token := getattr(credential, "get_token", None)) is not None:

                async def token():
                    access = get_token("https://storage.azure.com/.default")
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
                if not result["subject"] and (access_token := getattr(credential, "token", None)):
                    endpoint = "https://oauth2.googleapis.com/tokeninfo?" + urlencode(
                        {"access_token": access_token}
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
