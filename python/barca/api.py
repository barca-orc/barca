"""Barca Python API — programmatic access to the asset orchestrator.

Calls the barca binary under the hood, parses results, and returns
Python objects. Artifact files are deserialized automatically.
"""

import datetime
import json
import os
import shutil
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

from barca._artifacts import deserialize


class BarcaError(Exception):
    """Raised when a barca command fails.

    When the CLI reported a structured error (the JSON envelope on stderr, see
    ``barca docs agents``), its fields are attributes: ``kind`` (``usage`` |
    ``step_failed`` | ``infra`` | ``cancelled``), ``code`` (the exit code),
    ``remediation``, and for ``step_failed`` also ``node``, ``traceback``,
    ``artifact_dir`` and ``phase`` (where the step failed: ``user_code`` for the step's own
    code; ``load_inputs``, ``split_output``, ``write_output`` or ``sink`` for barca's own work
    around it; ``None`` when unknown). They are ``None`` when the CLI printed plain text.
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
        self.phase: str | None = env.get("phase")


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


def _read_output(output_ref: Any, server: "bool | str" = False, run_id: str | None = None) -> Any:
    """Deserialize a final_output value.

    If it's already a plain value (dict, list, int, etc.), return as-is.
    If it's a sentinel-wrapped artifact reference, read from disk. For a run on a server, the
    path is the server's, so the bytes come from ``GET /runs/{run_id}/output``.
    """
    if isinstance(output_ref, dict) and "_barca_artifact" in output_ref:
        meta = output_ref["_barca_artifact"]
        if server and run_id:
            return _server_output(_server_url(server), run_id, meta["format"])
        return deserialize(meta["path"], meta["format"])
    return output_ref


def _read_final_output(output: Any, server: "bool | str" = False, run_id: str | None = None) -> Any:
    """Read `final_output` from `barca get --json`: one artifact pointer, or for a partitioned
    target a mapping of partition key (``"region=emea"``) to pointer, read as key to value.
    A target of more keys than a run lists (``{"_barca_partitions": ...}``) is an error naming
    ``partition=``."""
    if isinstance(output, dict) and "_barca_partitions" in output:
        summary = output["_barca_partitions"]
        raise BarcaError(
            f"the target returned {summary.get('keys')} partition keys, more than a run lists; "
            'select the keys to read with partition= (barca docs partitions, "Many keys")'
        )
    if (
        isinstance(output, dict)
        and output
        and "_barca_artifact" not in output
        and all(isinstance(v, dict) and "_barca_artifact" in v for v in output.values())
    ):
        if server:
            raise BarcaError(
                "a partitioned target's values cannot be read from a server yet; "
                "get it on this machine (server=False)"
            )
        return {key: _read_output(ref) for key, ref in output.items()}
    return _read_output(output, server, run_id)


def _server_args(server: "bool | str") -> list[str]:
    """The CLI flag for ``server=``: ``True`` is the configured server, a string its URL."""
    if server is True:
        return ["--server"]
    if isinstance(server, str) and server:
        return ["--server", server]
    return []


def _server_url(server: "bool | str") -> str:
    """The server URL as the CLI resolves it: the one given, else ``BARCA_SERVER``, else
    ``[server] url`` in the nearest barca.toml at or above the cwd."""
    if isinstance(server, str) and server:
        return server.rstrip("/")
    url = os.environ.get("BARCA_SERVER", "")
    if not url:
        cwd = Path.cwd()
        for directory in (cwd, *cwd.parents):
            toml = directory / "barca.toml"
            if toml.is_file():
                with toml.open("rb") as f:
                    url = tomllib.load(f).get("server", {}).get("url", "") or ""
                break
    if not url:
        raise BarcaError("server=True but no server is configured (BARCA_SERVER or [server] url)")
    return url.rstrip("/")


def _server_output(url: str, run_id: str, fmt: str) -> Any:
    """A served run's final artifact, read from ``GET /runs/{run_id}/output``."""
    import tempfile

    try:
        with urllib.request.urlopen(f"{url}/runs/{run_id}/output", timeout=300) as resp:
            body = resp.read()
    except (urllib.error.URLError, OSError) as e:
        raise BarcaError(f"cannot read the output of run {run_id} from {url}: {e}") from e
    suffix = {"parquet": ".parquet", "pickle": ".pkl", "csv": ".csv", "json": ".json"}.get(fmt, "")
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / f"output{suffix}"
        path.write_bytes(body)
        return deserialize(str(path), fmt)


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


def _batch_args(batch) -> list[str]:
    """``--batch`` for ``batch=``: a value for every node that declares ``batch=``, or a dict
    of node name to value."""
    if batch is None:
        return []
    if isinstance(batch, dict):
        out: list[str] = []
        for node, value in batch.items():
            out += ["--batch", f"{node}={'none' if value is None else value}"]
        return out
    return ["--batch", str(batch)]


def get(
    target_or_file: str,
    *extra_files: str,
    refresh: list[str] | None = None,
    refresh_all: bool = False,
    cascade: bool = True,
    partition: "str | dict[str, Any] | None" = None,
    server: bool | str = False,
    batch: "str | int | dict[str, str | int] | None" = None,
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
    Select a key or set with ``refresh=["source[account=A]", "source[account=C]"]``.
    Keyed refresh retains the complete target domain and fan-ins, reuses fresh siblings,
    and cascades only through affected keys (stopping at unchanged cutoff bytes).
    Each selector names one dimension and value; unlike ``partition``, it does not narrow
    the output domain.

    A partitioned target returns a dict of partition key (``"region=emea"``) to
    value, for every key or for the keys ``partition`` selects: ``"region=emea,amer"``
    or a dict for compound keys, ``{"region": "emea", "day": ["d1", "d2"]}``
    (``--partition``). The selection applies to the whole cone: every partitioned asset
    with a selected dimension computes only those keys, and a ``collect()`` fan-in over
    them is skipped. It works without a target too (``get("pipeline.py", partition=...)``).
    Without ``partition``, a target of more than 1000 keys raises :class:`BarcaError`: a
    run lists only the keys it computed then, so select the keys to read.
    With ``server=``, the values are read on the server and come back as JSON (a table as a
    list of row objects).

    ``server=True`` runs it on the configured barca server (``BARCA_SERVER`` or
    ``[server] url``), ``server="https://..."`` on that one (``barca get --server``); files
    are then optional. The value is the same as a local run's.

    ``batch=`` overrides the ``batch=`` of the nodes that declare one for this run
    (``--batch``): ``"all"``, ``"none"`` or a whole number for every such node, or a dict
    ``{"scored": "all"}`` for the nodes named. Experimental.

    Returns the deserialized value of the target asset directly.
    """
    args: list[str] = ["get", target_or_file, *extra_files, "--json"]
    args += _refresh_args(refresh, refresh_all, cascade)
    args += _batch_args(batch)
    if partition is not None:
        args += ["--partition", partition if isinstance(partition, str) else json.dumps(partition)]
        if server:
            # A server returns the selected keys' values read on it (``-o value``), as JSON.
            args += ["-o", "value"]
    args += _server_args(server)
    result = _exec(args)
    output = result.get("final_output")
    if output is not None:
        return _read_final_output(output, server, result.get("run_id"))
    return output


def run(
    target: str,
    *files: str,
    refresh: list[str] | None = None,
    refresh_all: bool = False,
    cascade: bool = True,
    partition: "str | dict[str, Any] | None" = None,
    server: bool | str = False,
    batch: "str | int | dict[str, str | int] | None" = None,
) -> Any:
    """Run a task (and its cone). The task always re-runs.

    Upstream assets are served from cache when fresh (same as ``get``). Pass
    ``refresh=["source[account=A]", "source[account=C]"]`` refreshes a set of keys
    while keeping the full target domain, cached siblings and complete fan-ins.
    Each keyed selector names one dimension and value. Use
    ``refresh=["asset_name", ...]`` to force re-materialize those assets and every
    asset downstream of them in the task's cone; add ``cascade=False``
    (``--no-cascade``) to re-materialize only the named assets. Pass
    ``refresh_all=True`` to refresh every upstream asset. ``partition=`` selects partition
    keys across the task's cone, as for :func:`get` (``--partition``). ``server=`` runs it
    on a barca server, and ``batch=`` overrides ``batch=``, as for :func:`get`.

    A partitioned task returns a dict of partition key to value (tables as JSON rows),
    as ``get`` does. A large partition summary requires selecting keys with ``partition=``.
    Returns the value of the target task, or its keyed values when partitioned.
    """
    args: list[str] = ["run", target, *files, "--json"]
    args += _refresh_args(refresh, refresh_all, cascade)
    args += _batch_args(batch)
    if partition is not None:
        args += ["--partition", partition if isinstance(partition, str) else json.dumps(partition)]
    args += _server_args(server)
    result = _exec(args)
    output = result.get("final_output")
    if output is not None:
        return _read_final_output(output, server, result.get("run_id"))
    return output


def plan(file: str, *extra_files: str, partition: "str | dict[str, Any] | None" = None) -> dict:
    """Return the execution plan as a dict.

    ``partition=`` plans only the selected partition keys (``barca plan --partition``); the
    plan then has a ``partition_selection`` entry.

    Returns a dict with:
        - total_steps: int
        - phases: list of phase dicts
    """
    files = [file, *extra_files]
    args = ["plan", *files]
    if partition is not None:
        args += ["--partition", partition if isinstance(partition, str) else json.dumps(partition)]
    return _exec(args)


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


def signed_url(uri: str, expires: "int | float | datetime.timedelta" = 3600) -> str:
    """A time-limited, read-only HTTPS URL for the object at ``uri``. Experimental.

    Anyone holding the URL can download the object, without credentials, until it expires.
    ``uri`` names an object in S3 (``s3://``, ``s3a://``; R2 and MinIO through the endpoint
    in ``BARCA_STORAGE_OPTIONS``), GCS (``gs://``, ``gcs://``) or Azure (``abfs://``,
    ``abfss://``), signed with the same credentials barca uses for its store. ``expires`` is
    seconds or a ``datetime.timedelta``: one hour by default, at most 7 days. The link
    downloads the object under its own name. It cannot be revoked before it expires.

    The object must exist. Raises :class:`BarcaError` naming the URI and the reason (``kind``
    ``usage`` for a URI or expiry that cannot be signed, ``infra`` for a missing object or
    credentials that cannot sign). Needs the store's extra (``barca[s3]``, ``barca[gcs]``,
    ``barca[azure]``), not the barca binary.
    """
    from barca import _storage

    try:
        url, _ = _storage.sign_url(uri, expires)
    except _storage.SignError as exc:
        error = BarcaError(str(exc))
        error.kind = "usage" if exc.kind == "usage" else "infra"
        error.code = 2 if exc.kind == "usage" else 3
        raise error from None
    return url
