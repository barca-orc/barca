"""Barca — invisible asset orchestrator.

This module provides decorator stubs and marker classes. The decorators are
pure no-ops (identity functions) — all logic lives in the Rust binary which
parses these statically from source without importing.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from typing import TYPE_CHECKING, Any, Literal, TypeVar, overload

from barca._batch import Failed
from barca._branches import BranchResultError, ParallelError, parallel, parallel_map

if TYPE_CHECKING:
    from barca.api import BarcaError, get, history, plan, run, signed_url, sql, stats
    from barca.client import Client, Run

__version__ = "0.41.1"

__all__ = [
    "group",
    "Group",
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
    "batches",
    "batches_by",
    "Failed",
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
    "sql",
    "signed_url",
    "BarcaError",
    "Client",
    "Run",
]

# A decorated step function: every decorator returns it unchanged.
F = TypeVar("F", bound=Callable[..., Any])
# What `serializer=` accepts, on the decorators and on `@sink`.
Serializer = Literal["json", "pickle", "parquet", "csv"]
# What `batch=` accepts: a size, `"all"`, `batches([...])` (the groups) or `batches_by(fn)`
# (the function).
Batch = int | str | Sequence[Sequence[Any]] | Callable[[Any], str | int] | None

# ─── Freshness markers ───────────────────────────────────────────────────────


class Always:
    """The default: under `barca serve`, brought up to date at each tick of a
    scheduled node above it."""


class Manual:
    """Refreshed by hand only (`--refresh <name>`): never by a tick or by a
    request for something below it, which reads its current result."""


class Schedule:
    """Runs on a cron schedule under `barca serve`.

    `max_failures` caps consecutive failed ticks (default 5): past it the
    schedule is skipped and reported `failing` until an explicit request or a
    change in its inputs.

    `timeout_seconds` limits how long one of its ticks' runs may take under
    `barca serve` (0: no limit); without it the server's run timeout applies
    (`--run-timeout`, `[server] run_timeout_seconds`, none by default).

    `batch` is the `--batch` override its ticks run with: `"all"`, a whole number,
    `None` (as declared), or `"none"` for one key per call. Experimental.
    """

    def __init__(
        self,
        cron: str,
        /,
        *,
        max_failures: int = 5,
        timeout_seconds: int | None = None,
        batch: int | str | None = None,
    ) -> None:
        self.cron = cron
        self.max_failures = max_failures
        self.timeout_seconds = timeout_seconds
        self.batch = batch


# `freshness=`: `Always`, `Manual` (the classes themselves) or a `Schedule(...)`.
Freshness = type[Always] | type[Manual] | Schedule

# ─── Decorators ───────────────────────────────────────────────────────────────


@overload
def asset[F: Callable[..., Any]](fn: F, /) -> F: ...
@overload
def asset(
    *,
    name: str | None = None,
    inputs: dict[str, Any] | None = None,
    partitions: dict[str, Any] | None = None,
    serializer: Serializer | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
    weight: int = 1,
    deterministic: bool = True,
    cutoff: bool = False,
    batch: Batch = None,
) -> Callable[[F], F]: ...
def asset[F: Callable[..., Any]](
    fn: F | None = None,
    /,
    *,
    name: str | None = None,
    inputs: dict[str, Any] | None = None,
    partitions: dict[str, Any] | None = None,
    serializer: Serializer | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
    weight: int = 1,
    deterministic: bool = True,
    cutoff: bool = False,
    batch: Batch = None,
) -> F | Callable[[F], F]:
    """Declare a cached asset node.

    The arguments below are the only ones: the `barca` binary rejects any other when it
    plans (exit 2, naming the argument), and this signature rejects it when the module
    is imported.

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

    `weight` (a whole number, default 1) is how much of the pool's capacity the step
    takes while it runs: the steps running at once total at most the capacity (the
    pool size unless `[execution] capacity` or `BARCA_CAPACITY` sets it). Give a step
    that needs a lot of memory a higher weight so fewer run beside it. It does not
    count toward the run hash. Experimental.

    `deterministic=False` declares that the asset may return other bytes for the same
    code and inputs (a snapshot of an external system), so barca does not warn that it
    is not deterministic. It does not change caching and does not count toward the run
    hash.

    `cutoff=True` makes what reads the asset depend on the bytes it writes instead of
    on its run hash: when it reruns and writes the same bytes, its consumers stay
    cached. Its consumers are decided after it runs. A literal `True` or `False`; not
    part of its own run hash. Experimental (see `barca docs cache`, "Early cutoff").

    `batch` (on a partitioned asset) groups the keys that need to run into fewer calls:
    `batch=20` (at most 20 keys per call), `batch="all"`, `batches([...])` or
    `batches_by(fn, max_size=N)`. The partition parameter then receives the list of the
    call's keys, a partitioned input a dict of key to value (or one lazy table), and the
    function returns a dict of key to value or a table with the dimension as a column. Each
    key keeps its own run hash and artifact. Not part of the run hash. Experimental (see
    `barca docs partitions`, "Processing many keys per call").

    `retries` is the total number of attempts on failure (1 = no retry).
    `retry_backoff` is the base delay in seconds between attempts (delay grows
    linearly: `retry_backoff * attempt`). All parameters are read statically by
    the Rust binary; this stub stays a no-op — they exist for IDE autocomplete
    and type checking.
    """
    if fn is not None:
        return fn

    def decorator(f: F) -> F:
        return f

    return decorator


@overload
def sensor[F: Callable[..., Any]](fn: F, /) -> F: ...
@overload
def sensor(
    *,
    name: str | None = None,
    partitions: dict[str, Any] | None = None,
    serializer: Serializer | None = None,
    freshness: Freshness = Manual,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
    weight: int = 1,
) -> Callable[[F], F]: ...
def sensor[F: Callable[..., Any]](
    fn: F | None = None,
    /,
    *,
    name: str | None = None,
    partitions: dict[str, Any] | None = None,
    serializer: Serializer | None = None,
    freshness: Freshness = Manual,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
    weight: int = 1,
) -> F | Callable[[F], F]:
    """Declare a sensor node (observes external state).

    Return `True`/`False` to report a change, or `(changed, value)` to also supply
    data. `False` retains the previous observation. `True` advances freshness even
    without data. Return another value (e.g. an mtime or ETag) to compare versions.

    Sensors default to `Manual` and are polled whenever needed. `Schedule(...)`
    gives a sensor its own polling cadence; `Always` is also accepted. See
    `asset` for `env`, `weight`, `retries` and `retry_backoff` semantics.

    `partitions` runs the sensor once per key, with the key passed as the parameter
    named in the dict, as on `asset`. `serializer` forces the format its value is
    stored in ("json", "pickle", "parquet" or "csv").
    """
    if fn is not None:
        return fn

    def decorator(f: F) -> F:
        return f

    return decorator


@overload
def task[F: Callable[..., Any]](fn: F, /) -> F: ...
@overload
def task(
    *,
    name: str | None = None,
    inputs: dict[str, Any] | None = None,
    partitions: dict[str, Any] | None = None,
    serializer: Serializer | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
    weight: int = 1,
    batch: Batch = None,
) -> Callable[[F], F]: ...
def task[F: Callable[..., Any]](
    fn: F | None = None,
    /,
    *,
    name: str | None = None,
    inputs: dict[str, Any] | None = None,
    partitions: dict[str, Any] | None = None,
    serializer: Serializer | None = None,
    freshness: Freshness = Always,
    timeout_seconds: int = 300,
    retries: int = 1,
    retry_backoff: float = 0.0,
    description: str | None = None,
    tags: dict[str, str] | None = None,
    env: list[str] | None = None,
    weight: int = 1,
    batch: Batch = None,
) -> F | Callable[[F], F]:
    """Declare a task node (always re-runs; never cached).

    Tasks model workflow-management steps — deploys, notifications, migrations,
    cache warming — that *do* something rather than produce cacheable data. They
    may appear anywhere in the graph and may depend on assets, sensors, or other
    tasks, but must not be an input to an asset or sensor.

    A `@task(freshness=Schedule("<cron>"))` is the simplest way to run something
    on a timer: leave `barca serve` running and it fires on each cron tick (see
    the Scheduling guide). See `asset` for `env`, `weight`, `retries` and
    `retry_backoff` semantics; a task always runs, so `env` only records the values it used.

    `partitions` runs the task once per key, with the key passed as the parameter
    named in the dict, as on `asset`. `serializer` forces the format its return
    value is stored in ("json", "pickle", "parquet" or "csv"). `batch` groups the keys
    into fewer calls, as on `asset`.
    """
    if fn is not None:
        return fn

    def decorator(f: F) -> F:
        return f

    return decorator


def sink(
    path: str,
    /,
    *,
    serializer: Serializer | None = None,
    partition_by: list[str] | tuple[str, ...] | None = None,
    rows_per_file: int | None = None,
) -> Callable[[F], F]:
    """Declare a sink output (stacked on @asset).

    path may be local or a remote URI (abfss://, s3://, gs://). serializer
    overrides the format ("json", "pickle", "parquet", "csv"); it defaults to the
    path extension, then the parent asset's artifact format.

    A path ending in "/" is a dataset sink: a Hive-partitioned dataset with one
    directory level per partition dimension of the asset, then one per
    ``partition_by`` column of the returned table. ``rows_per_file`` caps the rows
    of each parquet file. See ``barca docs sinks``.
    """

    def decorator(f: F) -> F:
        return f

    return decorator


def unsafe[F: Callable[..., Any]](fn: F) -> F:
    """Mark a function as unsafe (untraceable)."""
    return fn


# ─── Marker functions ─────────────────────────────────────────────────────────


def partitions(values: Sequence[Any], /) -> Sequence[Any]:
    """Declare static partition values."""
    return values


def partitions_from[F: Callable[..., Any]](source: F, /) -> F:
    """Derive partitions from an upstream asset or sensor.

    A partitioned `source` gives its keys. An unpartitioned one gives the values of the list it
    returns, or the keys of the dict it returns: then each key receives its own value, and runs
    again only when that value changes (`barca docs partitions`).
    """
    return source


def batches(groups: Sequence[Sequence[Any]], /, *, strict: bool = False) -> Sequence[Sequence[Any]]:
    """``batch=batches([["r1", "r2"], ["r3"]])``: the keys of each group run in one call.

    A key in no group runs alone, or is an error with ``strict=True``. A literal list is read
    without importing; any other expression is evaluated when barca plans.
    """
    return groups


def batches_by(
    fn: Callable[[Any], str | int], /, *, max_size: int | None = None
) -> Callable[[Any], str | int]:
    """``batch=batches_by(region_of, max_size=20)``: keys with the same label share a call.

    ``fn`` receives a key (a dict of dimension to value for a node of several dimensions) and
    returns a string or integer label; ``max_size`` splits a label's keys into calls of at
    most that many.
    """
    return fn


def collect[F: Callable[..., Any]](asset_fn: F, /) -> F:
    """Aggregate all partitions of an upstream asset."""
    return asset_fn


def asset_ref(ref_string: str, /) -> str:
    """Canonical asset reference."""
    return ref_string


# ─── DuckDB ───────────────────────────────────────────────────────────────────


def duckdb_connection() -> Any:
    """The duckdb connection barca binds duckdb-typed inputs to (one per worker process).

    Configure it once at import time of your asset module (``INSTALL``/``LOAD`` extensions,
    credentials, ``SET`` options, macros); every duckdb input, and ``duckdb.sql(...)`` inside
    your steps, runs on this same connection, so relations always combine. User-created
    objects and session settings persist across sequential steps; Barca cleans up only
    its temporary input views. Avoid catalog names that collide with input parameters.
    See ``barca docs types``.
    """
    from barca import _duckdb

    return _duckdb.connection()


# ─── Python API ──────────────────────────────────────────────────────────────

# Loaded on first use: a worker imports `barca` for the decorators, and `barca.api` and
# `barca.client` bring in urllib, http.client and ssl, about half of a worker's start-up (#560).
_LAZY = {
    "BarcaError": "barca.api",
    "get": "barca.api",
    "history": "barca.api",
    "plan": "barca.api",
    "run": "barca.api",
    "signed_url": "barca.api",
    "stats": "barca.api",
    "sql": "barca.api",
    "Client": "barca.client",
    "Run": "barca.client",
}


def __getattr__(name: str) -> Any:
    module = _LAZY.get(name)
    if module is None:
        raise AttributeError(f"module 'barca' has no attribute {name!r}")
    import importlib

    value = getattr(importlib.import_module(module), name)
    globals()[name] = value
    return value


# ─── Groups ──────────────────────────────────────────────────────────────────


class Group:
    """Organizational metadata. Never an executable or cacheable node."""

    def __init__(
        self,
        name: str,
        members: list[object] | tuple[object, ...],
        output: object,
        description: str,
    ) -> None:
        self.name = name
        self.members = members
        self.output = output
        self.description = description


def group(
    name: str,
    /,
    *,
    members: list[object] | tuple[object, ...],
    output: object,
    description: str = "",
) -> Group:
    """Declare a UI/CLI group using literal references; groups may contain groups.

    This metadata does not change execution, dependencies or caching.
    """
    return Group(name, members, output, description)
