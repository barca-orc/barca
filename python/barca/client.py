"""Barca server client — a thin HTTP wrapper over a running ``barca serve``.

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

import json
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

from barca.api import BarcaError

DEFAULT_URL = "http://127.0.0.1:8274"

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
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    # ── transport ──────────────────────────────────────────────────────────

    def _request(self, method: str, path: str) -> Any:
        url = f"{self.base_url}{path}"
        req = urllib.request.Request(url, method=method)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                body = resp.read()
        except urllib.error.HTTPError as e:
            # The API returns {"error": "..."} bodies on failure.
            detail = e.read().decode("utf-8", "replace")
            try:
                detail = json.loads(detail).get("error", detail)
            except (json.JSONDecodeError, AttributeError):
                pass
            raise BarcaError(f"{method} {path} failed ({e.code}): {detail}") from e
        except urllib.error.URLError as e:
            raise BarcaError(f"cannot reach barca server at {self.base_url}: {e.reason}") from e

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
