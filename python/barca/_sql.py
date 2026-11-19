"""`barca sql` — invoked by Rust as `python -m barca._sql`.

Registers one DuckDB view per cached artifact and runs a query over them, in an in-memory
database. Reads artifact files only: user code is never imported, and nothing is recorded.

A view whose artifact is in remote storage is fetched, when the query names it, into
`cache_dir` (`.barca/sql-cache/<scheme>/<bucket>/<path>`, mirroring the object's URI) through
the fsspec filesystem the workers use, and queried from there. A copy is reused while the
object's size and version (etag, generation, modified time) are unchanged, so a query costs one
metadata request per remote file it reads, and a download only the first time.

barca's own records (`barca.runs`, `barca.materializations`, `barca.nodes`, `barca.schedules`,
`barca.ticks`, `barca.queue`) arrive as rows from Rust and become tables in the `barca` schema;
a table the query does not name arrives without rows and is a typed placeholder that errors
when read. `show tables` lists the views and those
tables (as `barca.runs`), not only the views of the default schema.

Protocol: one JSON document on stdin,
  {"query": str, "limit": int | null, "cache_dir": str,
   "views": [{"name", "node", "format", "files": [{"path", "partition": str | null}]}],
   "tables": [{"schema", "name", "columns": [[name, duckdb type]], "rows": [{...}] | null}]}
and one JSON document on stdout, either
  {"columns": [...], "rows": [{col: value}], "total": int, "truncated": bool,
   "unavailable": {view: reason}, "unreachable": {view: {"kind": "driver" | "fetch", "reason"}},
   "fetched": {"files": int, "bytes": int}}
or {"error": {"kind": "missing_table" | "sql" | "no_duckdb", "message": str, "table"?: str},
    "unavailable": {...}, "unreachable": {...}, "fetched": {...}}.
"""

from __future__ import annotations

import json
import os
import re
import tempfile
from concurrent.futures import ThreadPoolExecutor

from barca import _helper, _sql_tables, _storage
from barca._duckdb import _quote
from barca._sql_tables import jsonable as _jsonable
from barca._sql_tables import quote_str as _quote_str

_QUERYABLE = ("parquet", "json", "csv")
DEFAULT_CACHE_DIR = os.path.join(".barca", "sql-cache")
# Remote artifacts fetched at once.
MAX_FETCHERS = 8


def _source(fmt: str, path: str) -> str:
    if fmt == "csv":
        return f"read_csv_auto({_quote_str(path)}, header=true)"
    if fmt == "parquet":
        return f"read_parquet({_quote_str(path)})"
    if fmt == "json":
        return f"read_json_auto({_quote_str(path)})"
    raise ValueError(f"{fmt} artifacts cannot be queried (only parquet, json and csv)")


def _register(con, view: dict) -> str | None:
    """Create the view; return why it is unavailable, or None."""
    fmt = view["format"]
    if fmt not in _QUERYABLE:
        return f"a {fmt} artifact cannot be queried; only parquet, json and csv artifacts can"
    name = _quote(view["name"])
    if view.get("placeholder"):
        why = f"barca: the remote artifact of {view['name']} was not fetched for this query"
        con.execute(f"CREATE VIEW {name} AS SELECT error({_quote_str(why)}) AS not_fetched")
        return None
    files = view["files"]
    parts = []
    for f in files:
        src = _source(fmt, f["path"])
        if f.get("partition") is not None:
            parts.append(f"SELECT {_quote_str(f['partition'])} AS partition, * FROM {src}")
        else:
            parts.append(f"SELECT * FROM {src}")
    body = " UNION ALL BY NAME ".join(parts)
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


# ─── barca's tables ───────────────────────────────────────────────────────────


def _table(con, table: dict) -> None:
    """Create one of barca's tables: its rows, or (rows null) a placeholder with its columns."""
    not_loaded = (
        f"barca: {table['schema']}.{table['name']} was not loaded for this query; "
        "name it in the query"
    )
    rows = table.get("rows")
    _sql_tables.create(con, table, not_loaded)
    if not rows:
        return
    columns = table["columns"]
    spec = "{" + ", ".join(f"{_quote_str(c)}: {_quote_str(ty)}" for c, ty in columns) + "}"
    fd, path = tempfile.mkstemp(prefix="barca-sql-", suffix=".ndjson")
    try:
        with os.fdopen(fd, "w") as f:
            for row in rows:
                f.write(json.dumps({c: row.get(c) for c, _ in columns}))
                f.write("\n")
        con.execute(
            f"INSERT INTO {_sql_tables.qualified(table)} SELECT * FROM "
            f"read_json({_quote_str(path)}, format='newline_delimited', columns={spec})"
        )
    finally:
        os.remove(path)


def _tables(con, tables: list[dict]) -> None:
    for schema in sorted({t["schema"] for t in tables}):
        con.execute(f"CREATE SCHEMA IF NOT EXISTS {_quote(schema)}")
    for table in tables:
        _table(con, table)


# ─── remote artifacts ─────────────────────────────────────────────────────────


def _names(query: str, name: str) -> bool:
    """True when `name` appears in the query as a whole identifier (case-insensitive)."""
    pattern = r"(?<![A-Za-z0-9_])" + re.escape(name) + r"(?![A-Za-z0-9_])"
    return re.search(pattern, query, re.IGNORECASE) is not None


def cache_path(cache_dir: str, uri: str) -> str:
    """Where the local copy of `uri` lives: the URI itself, as a path under `cache_dir`."""
    scheme, _, rest = uri.partition("://")
    parts = rest.split("/")
    if not rest or any(p in ("", ".", "..") for p in parts):
        raise ValueError(f"unexpected artifact URI: {uri}")
    return os.path.join(cache_dir, scheme.lower(), *parts)


def _version(info: dict) -> str:
    from barca import _range_cache

    return _range_cache.object_version(info)


def fetch(uri: str, cache_dir: str) -> tuple[str, int]:
    """Local copy of a remote artifact: (path, bytes downloaded; 0 when the copy was current)."""
    fs = _storage.get_fs(uri)
    version = _version(fs.info(uri))
    dest = cache_path(cache_dir, uri)
    stamp = dest + ".version"
    if os.path.exists(dest):
        try:
            with open(stamp) as f:
                current = f.read() == version
            if current:
                # The cache size limit evicts the least recently used copies first: a reuse
                # counts as a use.
                os.utime(stamp)
                return dest, 0
        except OSError:
            pass
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    tmp = f"{dest}.{os.getpid()}.tmp"
    try:
        fs.get_file(uri, tmp)
        os.replace(tmp, dest)
        with open(tmp, "w") as f:
            f.write(version)
        os.replace(tmp, stamp)
    finally:
        if os.path.exists(tmp):
            os.remove(tmp)
    return dest, os.path.getsize(dest)


def _localize(views: list[dict], query: str, cache_dir: str) -> tuple[list[dict], dict, dict]:
    """Replace remote artifact paths with local copies, for the views the query names.

    Returns (views to register, {view: {"kind", "reason"}} for those that could not be fetched,
    {"files", "bytes"} downloaded). A remote view the query does not name is not fetched
    (fetching every artifact of the project to answer a query over one would defeat the point);
    it is returned with `"placeholder": True` so it still shows in `show tables`."""
    keep: list[dict] = []
    wanted: list[dict] = []
    for view in views:
        remote = any(_storage.is_remote(f["path"]) for f in view["files"])
        if not remote or view["format"] not in _QUERYABLE:
            keep.append(view)
        elif _names(query, view["name"]):
            wanted.append(view)
        else:
            keep.append({**view, "placeholder": True})
    unreachable: dict[str, dict] = {}
    fetched = {"files": 0, "bytes": 0}
    if not wanted:
        return keep, unreachable, fetched

    uris = sorted({f["path"] for v in wanted for f in v["files"] if _storage.is_remote(f["path"])})
    results: dict[str, str | Exception] = {}
    gitignore = os.path.join(os.path.dirname(cache_dir) or ".", ".gitignore")
    os.makedirs(cache_dir, exist_ok=True)
    if os.path.basename(os.path.dirname(cache_dir)) == ".barca" and not os.path.exists(gitignore):
        with open(gitignore, "w") as f:
            f.write("*\n")

    def one(uri: str) -> tuple[str, int] | Exception:
        try:
            return fetch(uri, cache_dir)
        except Exception as e:  # noqa: BLE001 - a missing driver, credentials, the network
            return e

    with ThreadPoolExecutor(max_workers=min(MAX_FETCHERS, len(uris))) as pool:
        for uri, got in zip(uris, pool.map(one, uris), strict=False):
            if isinstance(got, Exception):
                results[uri] = got
            else:
                results[uri] = got[0]
                if got[1]:
                    fetched["files"] += 1
                    fetched["bytes"] += got[1]

    for view in wanted:
        files = []
        for f in view["files"]:
            got = results.get(f["path"], f["path"])
            if isinstance(got, Exception):
                kind = "driver" if isinstance(got, ImportError) else "fetch"
                reason = _storage.first_line(got) or type(got).__name__
                if isinstance(got, FileNotFoundError):
                    reason = f"{f['path']} is not in the remote store"
                elif kind == "fetch":
                    reason = f"{type(got).__name__}: {reason}"
                unreachable[view["name"]] = {"kind": kind, "reason": reason}
                break
            files.append({**f, "path": got})
        else:
            keep.append({**view, "files": files})
    return keep, unreachable, fetched


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
    query = request["query"].strip().rstrip(";")
    limit = request.get("limit")
    views, unreachable, fetched = _localize(
        request["views"], query, request.get("cache_dir") or DEFAULT_CACHE_DIR
    )
    con = _sql_tables.connect(duckdb)
    unavailable: dict[str, str] = {}
    for view in views:
        why = _register(con, view)
        if why:
            unavailable[view["name"]] = why
    _tables(con, request.get("tables") or [])
    if _sql_tables.SHOW_TABLES.match(query):
        query = _sql_tables.ALL_TABLES
    remote = {"unreachable": unreachable, "fetched": fetched}
    try:
        rel = con.sql(query)
        if rel is None:  # a statement without a result (SET, CREATE ...)
            return {
                "columns": [],
                "rows": [],
                "total": 0,
                "truncated": False,
                "unavailable": unavailable,
                **remote,
            }
        columns = list(rel.columns)
        fetched = rel.limit(limit + 1).fetchall() if limit is not None else rel.fetchall()
        truncated = limit is not None and len(fetched) > limit
        if truncated:
            fetched = fetched[:limit]
            # fetchall(): duckdb 2 relations have no fetchone().
            counted = con.sql(f"SELECT count(*) FROM ({query}) AS barca_sql").fetchall()
            total = counted[0][0] if counted else limit
        else:
            total = len(fetched)
    except duckdb.Error as e:
        message = str(e)
        table = _sql_tables.missing_table(duckdb, e)
        if table is not None:
            return {
                "error": {"kind": "missing_table", "message": message, "table": table},
                "unavailable": unavailable,
                **remote,
            }
        return {
            "error": {"kind": "sql", "message": message},
            "unavailable": unavailable,
            **remote,
        }
    rows = [{c: _jsonable(v) for c, v in zip(columns, r, strict=False)} for r in fetched]
    return {
        "columns": columns,
        "rows": rows,
        "total": total,
        "truncated": truncated,
        "unavailable": unavailable,
        **remote,
    }


def main() -> None:
    request = _helper.stdin_request()
    _helper.answer(lambda: run(request))


if __name__ == "__main__":
    _helper.run(main)
