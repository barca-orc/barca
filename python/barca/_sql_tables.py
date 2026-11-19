"""barca's own tables in DuckDB, for both query helpers: `barca sql` (`barca._sql`, one process
per command, with result views and file access) and `POST /sql/query` (`barca._sql_server`, one
locked-down process per server).

What they share is here: the in-memory database, how a table Rust sends becomes a table or a
placeholder, `show tables` over every schema, a missing table told from other errors, and a
DuckDB value as JSON. How the rows get in is each helper's own (`barca._sql` reads them from a
file it wrote; `barca._sql_server` has no file access and binds them as a parameter), and so is
everything that makes the server's helper safe to reach from the network.

Nothing here reads a file, and this module imports no other part of barca than the identifier
quoting: the server's helper imports it.
"""

from __future__ import annotations

import datetime as _dt
import decimal
import math
import re
import uuid
from typing import Any

from barca._duckdb import _quote as quote

SHOW_TABLES = re.compile(r"^\s*show\s+tables\s*$", re.IGNORECASE)

# `show tables` over every schema: views by their name, barca's tables as `barca.<name>`.
ALL_TABLES = """
SELECT CASE WHEN schema_name = 'main' THEN name ELSE schema_name || '.' || name END AS name
FROM (
    SELECT schema_name, view_name AS name FROM duckdb_views() WHERE NOT internal
    UNION ALL
    SELECT schema_name, table_name AS name FROM duckdb_tables() WHERE NOT internal
)
WHERE schema_name NOT IN ('information_schema', 'pg_catalog')
ORDER BY schema_name = 'barca', name
"""

_MISSING_TABLE = re.compile(r"Table with name (.+?) does not exist")


def connect(duckdb: Any) -> Any:
    """The database a query helper works in: in memory, gone with the process."""
    return duckdb.connect(":memory:")


def quote_str(s: str) -> str:
    """`s` as a SQL string literal."""
    return "'" + s.replace("'", "''") + "'"


def qualified(table: dict) -> str:
    """The table's quoted `schema.name`."""
    return f"{quote(table['schema'])}.{quote(table['name'])}"


def create(con: Any, table: dict, not_loaded: str) -> None:
    """Create one of barca's tables, empty, from the `{"schema", "name", "columns", "rows"}`
    Rust sends. Its schema must exist.

    A table sent without rows (`rows` null) becomes a placeholder: a view with the table's
    columns that fails with `not_loaded` when read. Filling a table is the caller's.
    """
    columns = table["columns"]
    if table.get("rows") is None:
        nulls = ", ".join(f"CAST(NULL AS {ty}) AS {quote(c)}" for c, ty in columns)
        con.execute(
            f"CREATE VIEW {qualified(table)} AS SELECT {nulls} "
            f"WHERE error({quote_str(not_loaded)}) IS NULL"
        )
        return
    con.execute(
        f"CREATE TABLE {qualified(table)} ({', '.join(f'{quote(c)} {ty}' for c, ty in columns)})"
    )


def missing_table(duckdb: Any, error: Exception) -> str | None:
    """The table `error` says does not exist, as DuckDB names it; None for any other error."""
    m = _MISSING_TABLE.search(str(error))
    if m and isinstance(error, duckdb.CatalogException):
        return m.group(1)
    return None


def jsonable(v: Any) -> Any:
    """A value DuckDB returned, as JSON data."""
    if isinstance(v, float):
        return None if math.isnan(v) or math.isinf(v) else v
    if isinstance(v, decimal.Decimal):
        return float(v)
    if isinstance(v, _dt.date | _dt.datetime | _dt.time):
        return v.isoformat()
    if isinstance(v, _dt.timedelta):
        return v.total_seconds()
    if isinstance(v, uuid.UUID):
        return str(v)
    if isinstance(v, bytes | bytearray | memoryview):
        return bytes(v).hex()
    if isinstance(v, dict):
        return {str(k): jsonable(x) for k, x in v.items()}
    if isinstance(v, list | tuple):
        return [jsonable(x) for x in v]
    return v
