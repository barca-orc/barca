"""Barca server client — a thin HTTP wrapper over a running ``barca serve``.

.. deprecated::
    ``barca.Client`` is deprecated in favour of :mod:`barca.api`, which runs a command on a
    server with ``server=True`` (``barca.get("daily_report", server=True)``) and returns the
    same values as a local run. ``Client`` warns when constructed and will be removed in a
    later minor release (RFC-0007 §3.2).

Unlike :mod:`barca.api` (which shells out to the ``barca`` binary for one-shot
commands), this talks to a long-running server: trigger runs, poll their status,
and inspect the cron schedule. Standard-library only (``urllib``) — no
third-party dependencies.

    from barca import Client

    c = Client("http://127.0.0.1:8274")
    run = c.get("daily_report")      # trigger, returns immediately
    result = run.wait()              # block until complete/failed
    for job in c.schedules():
        print(job["id"], job["cron"], job["next_fire"])
"""

from __future__ import annotations

import base64
import json
import os
import time
import warnings
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

from barca.api import BarcaError

DEFAULT_URL = "http://127.0.0.1:8274"


def _server_scope(base: str, destination: str) -> bool:
    def origin(parts):
        return (
            parts.scheme,
            parts.hostname,
            parts.port or (443 if parts.scheme == "https" else 80),
        )

    try:
        here, there = urllib.parse.urlsplit(base), urllib.parse.urlsplit(destination)
        prefix = here.path.rstrip("/")
        decoded_prefix = urllib.parse.unquote(prefix)
        decoded = urllib.parse.unquote(there.path)
        return (
            origin(here) == origin(there)
            and there.username is None
            and there.password is None
            and not any(part in (".", "..") for part in decoded.split("/"))
            and (not prefix or there.path == prefix or there.path.startswith(prefix + "/"))
            and (
                not decoded_prefix
                or decoded == decoded_prefix
                or decoded.startswith(decoded_prefix + "/")
            )
        )
    except ValueError:
        return False


class _ServerRedirects(urllib.request.HTTPRedirectHandler):
    def __init__(self, base_url: str) -> None:
        self.base_url = base_url

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not _server_scope(self.base_url, newurl):
            raise urllib.error.URLError("server redirect left the configured origin or proxy path")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


# Terminal run states reported by ``GET /status``.
_TERMINAL = {"complete", "failed", "cancelled"}


def _quote(target: str) -> str:
    """A target as one URL path segment: a full node id may contain ``/``."""
    return urllib.parse.quote(target, safe=":")


class Run:
    """Handle to a run triggered on the server; poll it via :meth:`status`."""

    def __init__(self, client: Client, run_id: str) -> None:
        self.client = client
        self.run_id = run_id

    def __repr__(self) -> str:
        return f"Run(run_id={self.run_id!r})"

    def status(self) -> dict:
        """Fetch the current status payload for this run."""
        return self.client.status(self.run_id)

    def cancel(self) -> dict:
        """Cancel this run if it is still pending/running."""
        return self.client.cancel(self.run_id)

    def wait(self, timeout: float = 600.0, poll: float = 0.5) -> dict:
        """Poll until the run reaches a terminal state, returning its status.

        Raises :class:`BarcaError` if ``timeout`` seconds elapse first. Does not
        raise on run failure — inspect ``["status"]``/``["error"]`` in the result.
        """
        deadline = time.monotonic() + timeout
        while True:
            payload = self.status()
            if payload.get("status") in _TERMINAL:
                return payload
            if time.monotonic() >= deadline:
                raise BarcaError(f"run {self.run_id} did not finish within {timeout}s")
            time.sleep(poll)


class Client:
    """Client for the ``barca serve`` JSON API."""

    def __init__(self, base_url: str = DEFAULT_URL, timeout: float = 30.0) -> None:
        warnings.warn(
            "barca.Client is deprecated and will be removed in a later minor release; use "
            "barca.api with server=True instead, e.g. barca.get('daily_report', server=True) "
            "(barca docs server-mode)",
            DeprecationWarning,
            stacklevel=2,
        )
        try:
            parts = urllib.parse.urlsplit(base_url.rstrip("/"))
            if (
                parts.scheme not in ("http", "https")
                or not parts.hostname
                or "?" in base_url
                or "#" in base_url
            ):
                raise ValueError
            _ = parts.port
            user = urllib.parse.unquote(parts.username or "", errors="strict")
            password = urllib.parse.unquote(parts.password or "", errors="strict")
        except (ValueError, UnicodeError):
            raise BarcaError("invalid server URL (host, port or proxy path)") from None
        has_basic = parts.username is not None or parts.password is not None
        token = os.environ.get("BARCA_SERVER_TOKEN")
        if token is not None and has_basic:
            raise BarcaError("BARCA_SERVER_TOKEN and server URL userinfo cannot be combined")
        self._authorization = None
        self._secrets: list[str] = []
        if token is not None:
            if (
                not token
                or not token.isascii()
                or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in token)
            ):
                raise BarcaError(
                    "BARCA_SERVER_TOKEN must be nonempty ASCII without whitespace or control characters"
                )
            try:
                token.encode("latin-1")
            except UnicodeError:
                raise BarcaError("BARCA_SERVER_TOKEN is not a valid authorization header") from None
            self._authorization = f"Bearer {token}"
            self._secrets.append(token)
        elif has_basic:
            if ":" in user or any(ord(c) < 32 or ord(c) == 127 for c in user + password):
                raise BarcaError(
                    "server Basic credentials contain an invalid username or control character"
                )
            encoded = base64.b64encode(f"{user}:{password}".encode()).decode("ascii")
            self._authorization = f"Basic {encoded}"
            self._secrets.extend([encoded, password or user, parts.password or user])
        if self._authorization:
            self._secrets.append(self._authorization)
        self._secrets = sorted({s for s in self._secrets if s}, key=len, reverse=True)
        self.base_url = urllib.parse.urlunsplit(
            (parts.scheme, parts.netloc.rsplit("@", 1)[-1], parts.path.rstrip("/"), "", "")
        )
        self._raw_url = base_url.rstrip("/")
        self._opener = urllib.request.build_opener(_ServerRedirects(self.base_url))
        self.timeout = timeout

    # ── transport ──────────────────────────────────────────────────────────

    def _redact(self, text: str) -> str:
        text = text.replace(self._raw_url, self.base_url)
        for secret in self._secrets:
            text = text.replace(secret, "<redacted>")
        return text

    def _error_value(self, value):
        if isinstance(value, str):
            return self._redact(value)
        if isinstance(value, dict):
            return {key: self._error_value(item) for key, item in value.items()}
        if isinstance(value, list):
            return [self._error_value(item) for item in value]
        return value

    def _open(self, req):
        return self._opener.open(req, timeout=self.timeout)

    def _request(self, method: str, path: str) -> Any:
        url = f"{self.base_url}{path}"
        if not _server_scope(self.base_url, url):
            raise BarcaError("server request left the configured origin or proxy path")
        headers = {"Authorization": self._authorization} if self._authorization else {}
        req = urllib.request.Request(url, method=method, headers=headers)
        try:
            with self._open(req) as resp:
                body = resp.read()
        except urllib.error.HTTPError as e:
            # The API returns {"error": "..."} bodies on failure.
            detail = e.read().decode("utf-8", "replace")
            try:
                value = self._error_value(json.loads(detail))
                detail = value.get("error", value) if isinstance(value, dict) else value
            except (json.JSONDecodeError, AttributeError):
                detail = self._redact(detail)
            raise BarcaError(f"{method} {path} failed ({e.code}): {detail}") from None
        except urllib.error.URLError as e:
            raise BarcaError(
                f"cannot reach barca server at {self.base_url}: {self._redact(str(e.reason))}"
            ) from None

        if not body:
            return None
        return json.loads(body)

    # ── read endpoints ─────────────────────────────────────────────────────

    def health(self) -> dict:
        """Server liveness + version (``GET /health``)."""
        return self._request("GET", "/health")

    def assets(self) -> list[dict]:
        """Every node with kind, freshness, and inputs (``GET /assets``)."""
        return self._request("GET", "/assets")

    def asset(self, name: str) -> dict:
        """One asset's summary joined with stats (``GET /assets/{name}``)."""
        return self._request("GET", f"/assets/{name}")

    def plan(self) -> dict:
        """Execution plan for the served DAG (``GET /plan``)."""
        return self._request("GET", "/plan")

    def schedules(self) -> list[dict]:
        """Scheduled jobs with cron, next fire, and last run (``GET /schedule``)."""
        return self._request("GET", "/schedule")

    def status(self, run_id: str) -> dict:
        """Status payload for a run (``GET /status/{run_id}``)."""
        return self._request("GET", f"/status/{run_id}")

    def cancel(self, run_id: str) -> dict:
        """Cancel an in-flight run (``DELETE /run/{run_id}``).

        The server stops the run's workers and its status transitions to
        ``cancelled``. Cancelling an already-finished run raises
        :class:`BarcaError` (HTTP 409).
        """
        return self._request("DELETE", f"/run/{run_id}")

    # ── trigger endpoints ──────────────────────────────────────────────────
    #
    # These mirror the CLI verbs. ``get`` matches ``barca get [TARGET]`` (the
    # target is optional; omit it to get every asset and sensor) and ``run``
    # matches ``barca run TARGET``.

    def get(self, target: str | None = None) -> Run:
        """Materialize an asset, or every asset and sensor if ``target`` is None.

        Mirrors ``barca get [TARGET]`` (``POST /get/{target}``, or ``POST /run``
        with no target). Tasks never run without a target; use ``run``.

        The server checks the target before it starts anything: an unknown name
        (HTTP 404), a name that matches several nodes (409) or a task (400)
        raises :class:`BarcaError` with the server's message, and no run exists.
        """
        path = f"/get/{_quote(target)}" if target is not None else "/run"
        payload = self._request("POST", path)
        return Run(self, payload["run_id"])

    def run(self, target: str) -> Run:
        """Run a task (``POST /run/{target}``). Unlike ``barca run TARGET``, the server
        recomputes every upstream asset of the task.

        An unknown name (HTTP 404), a name that matches several nodes (409) or an
        asset (400) raises :class:`BarcaError` with the server's message, and no
        run exists."""
        payload = self._request("POST", f"/run/{_quote(target)}")
        return Run(self, payload["run_id"])
