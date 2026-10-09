"""DuckDB connection handling for steps that take or return duckdb relations.

Relations are bound to the connection that created them, and relations from different
connections cannot be combined. Barca therefore keeps everything on one connection per worker
process: duckdb's default connection, the same one the module-level ``duckdb.sql(...)`` and
``duckdb.read_parquet(...)`` use. Inputs annotated ``duckdb.DuckDBPyRelation`` are loaded on it
(see ``_artifacts._deserialize_parquet``) and, here, additionally bound as views named after
their parameters for the duration of the step, so SQL by name works anywhere — helper modules
included — without the author writing any bind code.

``barca.duckdb_connection()`` exposes the connection so an asset module can configure it once
per worker process (extensions, credentials, settings, macros) at import time.
User-created objects and session changes persist between sequential steps; only the input
views bound by Barca are cleaned up after execution and materialization. Input names are not
an isolated namespace, so avoid collisions with project-created catalog objects.
"""

from __future__ import annotations

import re
from typing import Any


def connection():
    """The process-wide duckdb connection barca binds duckdb-typed inputs to.

    This is duckdb's default connection, so it is also what ``duckdb.sql(...)`` uses. Works
    outside a barca worker too (standalone runs of your module get the same connection).
    """
    import duckdb  # ty: ignore[unresolved-import]

    default = duckdb.default_connection
    # duckdb >= 1.4 exposes a function; older versions expose the connection object itself.
    return default() if callable(default) else default


def _quote(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def bind_inputs(kwargs: dict[str, Any], param_types: dict[str, str | None]) -> list[str]:
    """Bind each duckdb-typed input as a view named after its parameter.

    Returns the names actually bound (pass them to :func:`unbind_inputs` once the step has
    finished *and its result has been materialized* — a returned relation is lazy and may still
    reference these views). A name that is already taken by a table, or an input that is not a
    relation (a fan-in list, an ordering-only ``None``), is skipped rather than treated as an
    error: the relation is still passed to the function and works through ordinary Python
    variable resolution.
    """
    bound: list[str] = []
    for name, frame_type in param_types.items():
        if frame_type != "duckdb":
            continue
        relation = kwargs.get(name)
        if relation is None or not hasattr(relation, "create_view"):
            continue
        try:
            relation.create_view(name, replace=True)
        except Exception:
            continue
        bound.append(name)
    return bound


def unbind_inputs(names: list[str]) -> None:
    """Drop the views created by :func:`bind_inputs` (idempotent, never raises)."""
    if not names:
        return
    try:
        con = connection()
    except Exception:
        return
    for name in names:
        try:
            con.execute(f"drop view if exists {_quote(name)}")
        except Exception:
            pass


_MIXED_NOTE = (
    "barca: this step used DuckDB relations from two different connections.\n"
    "Barca loads duckdb inputs on one shared connection per worker, the same one that "
    "duckdb.sql(...) and duckdb.read_parquet(...) use. Stay on those (or on "
    "barca.duckdb_connection()) instead of opening your own duckdb.connect().\n"
    'If you need your own connection, copy an input onto it: con.register("name", input.arrow()).\n'
    "See `barca docs types`."
)

_MISSING_TABLE = re.compile(r"Table with name (\S+) does not exist")


def explain_error(exc: BaseException, bound_views: list[str]) -> str | None:
    """A plain-language note for the DuckDB errors a step gets from mixing connections.

    DuckDB reports these as ``Cannot combine LEFT and RIGHT relations of different
    connections!``, ``Python Object ... of type "DuckDBPyRelation" not suitable for replacement
    scan``, or (when a bound input is queried from another connection and no Python variable of
    that name is in scope) a misleading ``Table with name X does not exist``. Returns ``None`` for
    anything else, including a missing table that barca did not bind, which is just a typo.
    """
    try:
        import duckdb  # ty: ignore[unresolved-import]
    except ImportError:
        return None
    if not isinstance(exc, duckdb.Error):
        return None
    msg = str(exc)
    if "different connections" in msg:
        return _MIXED_NOTE
    if "not suitable for replacement scan" in msg and "DuckDBPyRelation" in msg:
        return _MIXED_NOTE
    missing = _MISSING_TABLE.search(msg)
    if missing and missing.group(1).strip("\"'") in bound_views:
        name = missing.group(1).strip("\"'")
        return (
            f"barca: this step queried `{name}` on a different connection than the one barca "
            f"bound it on.\n"
            f"`{name}` is an input of this step, available as a view on barca's shared "
            f"connection. Query it with duckdb.sql(...) or barca.duckdb_connection().sql(...), "
            f'or copy it onto your own connection: con.register("{name}", {name}.arrow()).\n'
            "See `barca docs types`."
        )
    return None
