"""`barca sql` — invoked by Rust as `python -m barca._sql`.

Registers one DuckDB view per cached artifact and runs a query over them, in an in-memory
database. Reads artifact files only: user code is never imported, and nothing is written.

Protocol: one JSON document on stdin,
  {"query": str, "limit": int | null,
   "views": [{"name", "node", "format", "files": [{"path", "partition": str | null}]}]}
and one JSON document on stdout, either
  {"columns": [...], "rows": [{col: value}], "total": int, "truncated": bool,
   "unavailable": {view: reason}}
or {"error": {"kind": "missing_table" | "sql" | "no_duckdb", "message": str, "table"?: str},
    "unavailable": {...}}.
"""

from __future__ import annotations

import datetime as _dt
import decimal
import json
import math
import re
import sys
import uuid
from typing import Any

from barca import _storage

_MISSING_TABLE = re.compile(r"Table with name (.+?) does not exist")


def _quote_ident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _quote_str(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def _source(fmt: str, path: str) -> str:
    if fmt == "parquet":
        return f"read_parquet({_quote_str(path)})"
    if fmt == "json":
        return f"read_json_auto({_quote_str(path)})"
    raise ValueError(f"{fmt} artifacts cannot be queried (only parquet and json)")


def _register(con, view: dict) -> str | None:
    """Create the view; return why it is unavailable, or None."""
    fmt = view["format"]
    if fmt not in ("parquet", "json"):
        return f"a {fmt} artifact cannot be queried; only parquet and json artifacts can"
    files = view["files"]
    for f in files:
        if _storage.is_remote(f["path"]):
            return "its artifact is in remote storage; barca sql reads local artifacts only"
    parts = []
    for f in files:
        src = _source(fmt, f["path"])
        if f.get("partition") is not None:
            parts.append(f"SELECT {_quote_str(f['partition'])} AS partition, * FROM {src}")
        else:
            parts.append(f"SELECT * FROM {src}")
    body = " UNION ALL BY NAME ".join(parts)
    name = _quote_ident(view["name"])
    try:
        con.execute(f"CREATE VIEW {name} AS {body}")
        con.execute(f"SELECT * FROM {name} LIMIT 0")
    except Exception as e:  # noqa: BLE001 - any reader error makes the view unavailable
        try:
            con.execute(f"DROP VIEW IF EXISTS {name}")
        except Exception:  # noqa: BLE001
            pass
        return f"its {fmt} artifact cannot be read as a table ({str(e).splitlines()[0]})"
    return None


def _jsonable(v: Any) -> Any:
    if isinstance(v, float):
        return None if math.isnan(v) or math.isinf(v) else v
    if isinstance(v, decimal.Decimal):
        return float(v)
    if isinstance(v, (_dt.date, _dt.datetime, _dt.time)):
        return v.isoformat()
    if isinstance(v, _dt.timedelta):
        return v.total_seconds()
    if isinstance(v, uuid.UUID):
        return str(v)
    if isinstance(v, (bytes, bytearray, memoryview)):
        return bytes(v).hex()
    if isinstance(v, dict):
        return {str(k): _jsonable(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_jsonable(x) for x in v]
    return v


def run(request: dict) -> dict:
    try:
        import duckdb
    except ImportError:
        return {
            "error": {
                "kind": "no_duckdb",
                "message": "barca sql needs duckdb in the Python environment barca uses",
            }
        }
    con = duckdb.connect(":memory:")
    unavailable: dict[str, str] = {}
    for view in request["views"]:
        why = _register(con, view)
        if why:
            unavailable[view["name"]] = why

    query = request["query"].strip().rstrip(";")
    limit = request.get("limit")
    try:
        rel = con.sql(query)
        if rel is None:  # a statement without a result (SET, CREATE ...)
            return {
                "columns": [],
                "rows": [],
                "total": 0,
                "truncated": False,
                "unavailable": unavailable,
            }
        columns = list(rel.columns)
        fetched = rel.limit(limit + 1).fetchall() if limit is not None else rel.fetchall()
        truncated = limit is not None and len(fetched) > limit
        if truncated:
            fetched = fetched[:limit]
            counted = con.sql(f"SELECT count(*) FROM ({query}) AS barca_sql").fetchone()
            total = counted[0] if counted else limit
        else:
            total = len(fetched)
    except duckdb.Error as e:
        message = str(e)
        m = _MISSING_TABLE.search(message)
        if m and isinstance(e, duckdb.CatalogException):
            return {
                "error": {"kind": "missing_table", "message": message, "table": m.group(1)},
                "unavailable": unavailable,
            }
        return {"error": {"kind": "sql", "message": message}, "unavailable": unavailable}
    rows = [{c: _jsonable(v) for c, v in zip(columns, r)} for r in fetched]
    return {
        "columns": columns,
        "rows": rows,
        "total": total,
        "truncated": truncated,
        "unavailable": unavailable,
    }


def main() -> None:
    request = json.load(sys.stdin)
    json.dump(run(request), sys.stdout)


if __name__ == "__main__":
    main()
