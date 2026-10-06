"""Barca Python API — programmatic access to the asset orchestrator.

Calls the barca binary under the hood, parses results, and returns
Python objects. Artifact files are deserialized automatically.
"""

import json
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

from barca._artifacts import deserialize


class BarcaError(Exception):
    """Raised when a barca command fails.

    When the CLI reported a structured error (the JSON envelope on stderr, see
    ``barca docs agents``), its fields are attributes: ``kind`` (``usage`` |
    ``step_failed`` | ``infra`` | ``cancelled``), ``code`` (the exit code),
    ``remediation``, and for ``step_failed`` also ``node``, ``traceback`` and
    ``artifact_dir``. They are ``None`` when the CLI printed plain text.
    """

    def __init__(self, message: str, envelope: dict | None = None, stderr: str | None = None):
        super().__init__(message)
        env = envelope or {}
        self.envelope = envelope
        self.stderr = stderr
        self.kind: str | None = env.get("kind")
        self.code: int | None = env.get("code")
        self.remediation: str | None = env.get("remediation")
        self.node: str | None = env.get("node")
        self.traceback: str | None = env.get("traceback")
        self.artifact_dir: str | None = env.get("artifact_dir")


def _error_envelope(stderr: str) -> dict | None:
    """The CLI's JSON error envelope: the last stderr line that is a JSON object with a kind."""
    for line in reversed(stderr.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict) and "kind" in obj and "error" in obj:
            return obj
    return None


def _failure(result: "subprocess.CompletedProcess[str]") -> BarcaError:
    """Build the BarcaError for a failed barca invocation."""
    stderr = result.stderr.strip()
    envelope = _error_envelope(stderr)
    if envelope is None:
        # Plain-text error (human output mode): stderr is the message.
        message = stderr[len("Error: ") :] if stderr.startswith("Error: ") else stderr
        return BarcaError(message, stderr=stderr)
    parts = [envelope["error"]]
    if envelope.get("traceback"):
        parts.append(envelope["traceback"])
    if envelope.get("remediation"):
        parts.append(envelope["remediation"])
    return BarcaError("\n".join(parts), envelope=envelope, stderr=stderr)


_cached_binary: str | None = None


def _find_binary() -> str:
    """Locate the barca binary and validate version on first call."""
    global _cached_binary
    if _cached_binary is not None:
        return _cached_binary

    binary = None

    # Check sibling of the Python interpreter (same venv bin/).
    bin_dir = Path(sys.executable).parent
    candidate = bin_dir / "barca"
    if candidate.is_file():
        binary = str(candidate)

    # Fall back to PATH.
    if binary is None:
        binary = shutil.which("barca")

    if binary is None:
        raise BarcaError("barca binary not found. Install with: uv add barca")

    # Validate version matches the Python package.
    try:
        import barca

        result = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=5)
        if result.returncode == 0:
            bin_version = result.stdout.strip().split()[-1]
            if bin_version != barca.__version__:
                import warnings

                warnings.warn(
                    f"barca binary version ({bin_version}) differs from Python package "
                    f"({barca.__version__}). This may cause protocol errors. "
                    f"Binary: {binary}",
                    stacklevel=2,
                )
    except Exception:
        pass  # Don't block on version check failures.

    _cached_binary = binary
    return _cached_binary


def _exec(args: list[str]) -> Any:
    """Run barca with args, return parsed JSON from the last stdout line."""
    binary = _find_binary()
    result = subprocess.run(
        [binary, *args],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise _failure(result)

    stdout = result.stdout.strip()
    if not stdout:
        raise BarcaError("No output from barca")
    # Try parsing the full output as JSON first (handles pretty-printed plan output).
    # Fall back to last line (for run/get where user prints precede the JSON).
    try:
        return json.loads(stdout)
    except json.JSONDecodeError:
        last_line = stdout.splitlines()[-1]
        return json.loads(last_line)


def _read_output(output_ref: Any) -> Any:
    """Deserialize a final_output value.

    If it's already a plain value (dict, list, int, etc.), return as-is.
    If it's a sentinel-wrapped artifact reference, read from disk.
    """
    if isinstance(output_ref, dict) and "_barca_artifact" in output_ref:
        meta = output_ref["_barca_artifact"]
        return deserialize(meta["path"], meta["format"])
    return output_ref


def _refresh_args(refresh: list[str] | None, refresh_all: bool, cascade: bool) -> list[str]:
    """The refresh flags shared by ``get`` and ``run``."""
    if refresh_all:
        return ["--refresh-all"]
    if refresh:
        args = ["--refresh", ",".join(refresh)]
        if not cascade:
            args.append("--no-cascade")
        return args
    return []


def get(
    target_or_file: str,
    *extra_files: str,
    refresh: list[str] | None = None,
    refresh_all: bool = False,
    cascade: bool = True,
    no_cache: bool = False,
) -> Any:
    """Get asset value(s).

    If target_or_file ends in .py, gets every asset and sensor in the file and
    returns the last asset's value. Tasks are never run (use ``run``); a file
    with only tasks returns None.
    Otherwise, treats it as a target asset name and remaining args as files.

    Assets come from cache when fresh. ``refresh=["asset", ...]`` re-materializes
    those assets (the target may be one of them) and everything downstream of
    them; ``cascade=False`` (``--no-cascade``) re-materializes only the named
    ones. ``refresh_all=True`` re-materializes every asset in the cone.
    ``no_cache=True`` is the deprecated spelling of ``refresh_all=True``.

    Returns the deserialized value of the target asset directly.
    """
    if no_cache:
        import warnings

        warnings.warn(
            "barca.get(no_cache=True) is deprecated; use refresh_all=True",
            DeprecationWarning,
            stacklevel=2,
        )
        refresh_all = True
    args: list[str] = ["get", target_or_file, *extra_files, "--json"]
    args += _refresh_args(refresh, refresh_all, cascade)
    result = _exec(args)
    output = result.get("final_output")
    if output is not None:
        return _read_output(output)
    return output


def run(
    target: str,
    *files: str,
    refresh: list[str] | None = None,
    refresh_all: bool = False,
    cascade: bool = True,
) -> Any:
    """Run a task (and its cone). The task always re-runs.

    Upstream assets are served from cache when fresh (same as ``get``). Pass
    ``refresh=["asset_name", ...]`` to force re-materialize those assets and every
    asset downstream of them in the task's cone; add ``cascade=False``
    (``--no-cascade``) to re-materialize only the named assets. Pass
    ``refresh_all=True`` to refresh every upstream asset.

    Returns the deserialized value of the target task directly (or ``None``).
    """
    args: list[str] = ["run", target, *files, "--json"]
    args += _refresh_args(refresh, refresh_all, cascade)
    result = _exec(args)
    output = result.get("final_output")
    if output is not None:
        return _read_output(output)
    return output


def plan(file: str, *extra_files: str) -> dict:
    """Return the execution plan as a dict.

    Returns a dict with:
        - total_steps: int
        - phases: list of phase dicts
    """
    files = [file, *extra_files]
    return _exec(["plan", *files])


def history(limit: int = 10) -> list[dict]:
    """Return recent run history.

    Returns a list of dicts, each with:
        - run_id: str
        - command: str
        - files: list[str]
        - target: str | None
        - status: str
        - steps_total: int | None
        - steps_executed: int
        - steps_cached: int
        - started_at: str
        - finished_at: str | None
        - elapsed_seconds: float | None
    """
    # `history --json` is an envelope {runs, total, truncated, hint?}; the API returns the runs.
    return _exec(["history", "--limit", str(limit), "--json"])["runs"]


def stats(target: str, file: str, *extra_files: str) -> dict:
    """Return execution statistics for an asset.

    Returns a dict with:
        - id: str
        - total_runs: int
        - avg_elapsed_seconds: float | None
        - cache_hit_rate: float
        - recent_runs: list of dicts
    """
    files = [file, *extra_files]
    return _exec(["stats", target, *files, "--json"])
