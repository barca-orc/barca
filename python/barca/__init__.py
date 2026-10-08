"""Barca — invisible asset orchestrator.

This module provides decorator stubs and marker classes. The decorators are
pure no-ops (identity functions) — all logic lives in the Rust binary which
parses these statically from source without importing.
"""

from __future__ import annotations

__version__ = "0.18.1"

__all__ = [
    "asset",
    "sensor",
    "task",
    "sink",
    "unsafe",
    "Always",
    "Manual",
    "Schedule",
    "partitions",
    "partitions_from",
    "collect",
    "asset_ref",
    "parallel",
    "parallel_map",
    "ParallelError",
    "BranchResultError",
    "duckdb_connection",
    "get",
    "run",
    "plan",
    "history",
    "stats",
    "BarcaError",
    "Client",
    "Run",
]

# ─── Freshness markers ───────────────────────────────────────────────────────


class Always:
    """Auto-materializes whenever stale and all upstreams are fresh."""


class Manual:
    """Only runs on explicit refresh."""


class Schedule:
    """Runs on a cron schedule."""

    def __init__(self, cron: str) -> None:
        self.cron = cron


# ─── Decorators ───────────────────────────────────────────────────────────────


def asset(
    fn=None,
    *,
    name=None,
    inputs=None,
    partitions=None,
    serializer=None,
    freshness=Always,
    timeout_seconds=300,
    retries=1,
    retry_backoff=0.0,
    description=None,
    tags=None,
    env: list[str] | None = None,
    **kwargs,
):
    """Declare a cached asset node.

    `freshness` controls when the asset is kept up to date — `Always` (default),
    `Manual`, or `Schedule("<cron>")`. The Rust binary reads it statically; a
    `Schedule` fires under `barca serve` (see the Scheduling guide).

    `env` declares the environment variables the function reads, as a literal
    list of names: `env=["SOURCE_CSV"]`. Their values at plan time are part of
    the run hash (changing one re-materializes this asset and everything
    downstream; unset is its own value) and are reported per step in `--agent`
    lines and JSON results. Names ending in `_TOKEN`, `_SECRET`, `_KEY` or
    `_PASSWORD` are hashed but shown as `<redacted>`. Variables read without
    being declared are not tracked.

    `retries` is the total number of attempts on failure (1 = no retry).
    `retry_backoff` is the base delay in seconds between attempts (delay grows
    linearly: `retry_backoff * attempt`). All parameters are read statically by
    the Rust binary; this stub stays a no-op — they exist for IDE autocomplete
    and type checking.
    """
    if fn is not None:
        return fn

    def decorator(f):
        return f

    return decorator


def sensor(
    fn=None,
    *,
    name=None,
    freshness=Manual,
    timeout_seconds=300,
    retries=1,
    retry_backoff=0.0,
    description=None,
    tags=None,
    env: list[str] | None = None,
    **kwargs,
):
    """Declare a sensor node (observes external state).

    Sensors must use `Manual` or `Schedule(...)` freshness — `Always` is not
    valid for a sensor (its polling cadence must be declared explicitly). See
    `asset` for `env`, `retries` and `retry_backoff` semantics.
    """
    if fn is not None:
        return fn

    def decorator(f):
        return f

    return decorator


def task(
    fn=None,
    *,
    name=None,
    inputs=None,
    freshness=Always,
    timeout_seconds=300,
    retries=1,
    retry_backoff=0.0,
    description=None,
    tags=None,
    env: list[str] | None = None,
    **kwargs,
):
    """Declare a task node (always re-runs; never cached).

    Tasks model workflow-management steps — deploys, notifications, migrations,
    cache warming — that *do* something rather than produce cacheable data. They
    may appear anywhere in the graph and may depend on assets, sensors, or other
    tasks, but must not be an input to an asset or sensor.

    A `@task(freshness=Schedule("<cron>"))` is the simplest way to run something
    on a timer: leave `barca serve` running and it fires on each cron tick (see
    the Scheduling guide). See `asset` for `env`, `retries` and `retry_backoff`
    semantics; a task always runs, so `env` only records the values it used.
    """
    if fn is not None:
        return fn

    def decorator(f):
        return f

    return decorator


def sink(path: str, serializer: str | None = None, **kwargs):
    """Declare a sink output (stacked on @asset).

    path may be local or a remote URI (abfss://, s3://, gs://). serializer
    overrides the format ("json", "pickle", "parquet"); it defaults to the
    path extension, then the parent asset's artifact format.
    """

    def decorator(f):
        return f

    return decorator


def unsafe(fn):
    """Mark a function as unsafe (untraceable)."""
    return fn


# ─── Marker functions ─────────────────────────────────────────────────────────


def partitions(values):
    """Declare static partition values."""
    return values


def partitions_from(source):
    """Derive partitions from an upstream asset."""
    return source


def collect(asset_fn):
    """Aggregate all partitions of an upstream asset."""
    return asset_fn


def asset_ref(ref_string: str) -> str:
    """Canonical asset reference."""
    return ref_string


# ─── Parallel primitives ─────────────────────────────────────────────────────


class ParallelError:
    """Represents a failed branch in a parallel() call."""

    def __init__(self, error: str) -> None:
        self.error = error

    def __repr__(self) -> str:
        return f"ParallelError({self.error!r})"

    def __str__(self) -> str:
        return self.error

    def to_dict(self) -> dict:
        return {"__parallel_error__": True, "error": self.error}


class BranchResultError(RuntimeError):
    """A `parallel()` branch returned, and its return value could not be passed to the caller.

    Raised by `parallel()` in the calling step, which fails like any step that raises. A
    branch that itself raised is different: it comes back as a `ParallelError` in the list.

    The message names the branch, the type of the value and the reason: the value cannot be
    written as json, pickle or parquet (an open file, a lambda, a generator), or what was
    written cannot be read back in the calling worker.
    """


def parallel(*callables):
    """Run callables in parallel across worker processes.

    Each argument should be a `functools.partial` wrapping a @task-decorated
    function. Returns a list of results (or ParallelError objects) in argument
    order.

    When running inside a barca worker (BARCA_SOCKET set), uses the Unix socket
    protocol to request Rust to dispatch branches as separate workers. A branch's
    return value comes back the way a step's output reaches the next step: written
    as an artifact (json, pickle or parquet, by type) and read from it here. A value
    that cannot be passed raises `BranchResultError`; it is never replaced by `None`.
    When running standalone, executes sequentially.
    """
    if not callables:
        return []

    # Build work items from partials
    items = []
    for c in callables:
        if hasattr(c, "func") and hasattr(c, "args") and hasattr(c, "keywords"):
            fn = c.func
            fn_name = fn.__name__
            source_file = getattr(getattr(fn, "__code__", None), "co_filename", "") or ""
            fn_ref = f"{source_file}:{fn_name}" if source_file else fn_name
            items.append(
                {
                    "fn_ref": fn_ref,
                    "args": list(c.args),
                    "kwargs": dict(c.keywords) if c.keywords else {},
                }
            )
        else:
            raise TypeError(f"parallel() expects functools.partial objects, got {type(c).__name__}")

    from barca import _runtime

    if _runtime.is_worker() and _runtime.connect() is not None:
        # Inside a barca worker — dispatch via Unix socket to executor
        from barca import _branches

        return _branches.collect(_runtime.submit_and_wait(items), items)

    # Not inside a worker — execute sequentially (standalone/testing)
    results = []
    for c in callables:
        try:
            results.append(c())
        except Exception as e:
            results.append(ParallelError(f"{type(e).__name__}: {e}"))
    return results


def parallel_map(fn, items, **kwargs):
    """Map a @task function over items in parallel.

    Sugar for `parallel(*(partial(fn, item, **kwargs) for item in items))`.
    """
    from functools import partial

    return parallel(*(partial(fn, item, **kwargs) for item in items))


# ─── DuckDB ───────────────────────────────────────────────────────────────────


def duckdb_connection():
    """The duckdb connection barca binds duckdb-typed inputs to (one per worker process).

    Configure it once at import time of your asset module (``INSTALL``/``LOAD`` extensions,
    credentials, ``SET`` options, macros); every duckdb input, and ``duckdb.sql(...)`` inside
    your steps, runs on this same connection, so relations always combine. See
    ``barca docs types``.
    """
    from barca import _duckdb

    return _duckdb.connection()


# ─── Python API ──────────────────────────────────────────────────────────────

from barca.api import BarcaError, get, history, plan, run, stats  # noqa: E402
from barca.client import Client, Run  # noqa: E402
