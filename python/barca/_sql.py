"""`barca sql` — invoked by Rust as `python -m barca._sql`.

Registers one DuckDB view per cached artifact and runs a query over them, in an in-memory
database. Reads artifact files only: user code is never imported, and nothing is recorded.

A view whose artifact is in remote storage is fetched, when the query names it, into
`cache_dir` (`.barca/sql-cache/<scheme>/<bucket>/<path>`, mirroring the object's URI) through
the fsspec filesystem the workers use, and queried from there. A copy is reused while the
object's size and version (etag, generation, modified time) are unchanged, so a query costs one
metadata request per remote file it reads, and a download only the first time.

Protocol: one JSON document on stdin,
  {"query": str, "limit": int | null, "cache_dir": str,
   "views": [{"name", "node", "format", "files": [{"path", "partition": str | null}]}]}
and one JSON document on stdout, either
  {"columns": [...], "rows": [{col: value}], "total": int, "truncated": bool,
   "unavailable": {view: reason}, "unreachable": {view: {"kind": "driver" | "fetch", "reason"}},
   "fetched": {"files": int, "bytes": int}}
or {"error": {"kind": "missing_table" | "sql" | "no_duckdb", "message": str, "table"?: str},
    "unavailable": {...}, "unreachable": {...}, "fetched": {...}}.
"""

from __future__ import annotations

import datetime as _dt
import decimal
import json
import math
import os
import re
import sys
import uuid
from concurrent.futures import ThreadPoolExecutor
from typing import Any

from barca import _storage

_MISSING_TABLE = re.compile(r"Table with name (.+?) does not exist")
_QUERYABLE = ("parquet", "json")
DEFAULT_CACHE_DIR = os.path.join(".barca", "sql-cache")
# Remote artifacts fetched at once.
MAX_FETCHERS = 8
# What identifies one version of an object, across s3fs, gcsfs, adlfs and memory://.
_VERSION_KEYS = (
    "size",
    "ETag",
    "etag",
    "generation",
    "LastModified",
    "last_modified",
    "updated",
    "created",
)


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
    if fmt not in _QUERYABLE:
        return f"a {fmt} artifact cannot be queried; only parquet and json artifacts can"
    name = _quote_ident(view["name"])
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
    seen = {k: str(info[k]) for k in _VERSION_KEYS if info.get(k) is not None}
    return json.dumps(seen, sort_keys=True)


def fetch(uri: str, cache_dir: str) -> tuple[str, int]:
    """Local copy of a remote artifact: (path, bytes downloaded; 0 when the copy was current)."""
    fs = _storage.get_fs(uri)
    version = _version(fs.info(uri))
    dest = cache_path(cache_dir, uri)
    stamp = dest + ".version"
    if os.path.exists(dest):
        try:
            with open(stamp) as f:
                if f.read() == version:
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
        for uri, got in zip(uris, pool.map(one, uris)):
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
                reason = _storage.one_line(got)
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
    query = request["query"].strip().rstrip(";")
    limit = request.get("limit")
    views, unreachable, fetched = _localize(
        request["views"], query, request.get("cache_dir") or DEFAULT_CACHE_DIR
    )
    con = duckdb.connect(":memory:")
    unavailable: dict[str, str] = {}
    for view in views:
        why = _register(con, view)
        if why:
            unavailable[view["name"]] = why
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
                **remote,
            }
        return {
            "error": {"kind": "sql", "message": message},
            "unavailable": unavailable,
            **remote,
        }
    rows = [{c: _jsonable(v) for c, v in zip(columns, r)} for r in fetched]
    return {
        "columns": columns,
        "rows": rows,
        "total": total,
        "truncated": truncated,
        "unavailable": unavailable,
        **remote,
    }


def main() -> None:
    request = json.load(sys.stdin)
    json.dump(run(request), sys.stdout)


if __name__ == "__main__":
    main()
