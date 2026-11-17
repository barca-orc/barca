"""The server's query helper: `POST /sql/query`, invoked by Rust as `python -m barca._sql_server`.

One long-lived process per `barca serve`, holding an in-memory DuckDB with barca's own records
(`barca.runs`, `barca.materializations`, `barca.nodes`) and nothing else: no result view, no
file. Rust hands it the rows of a table when the metadata database or the graph changed, and
one query at a time.

A query comes from whoever can reach the server, so the database is closed before the first
one runs, and each of these holds without the others:

- No file, network or extension access: `enable_external_access` is off, extensions are neither
  installed nor loaded, Python variables are not scanned, and the configuration is locked, so no
  statement can turn any of it back on. Rows arrive as a JSON parameter, never as a file.
- One statement, a `SELECT`, as DuckDB's own parser reads it (not a pattern over the text).
- A read-only transaction, rolled back: DuckDB 2 accepts `WITH x AS (DELETE ... RETURNING *)
  SELECT ...` as a `SELECT`, and the transaction refuses the write.
- Memory and threads are bounded, and nothing is spilled to disk: a query that needs more
  memory fails.

Protocol: one JSON document per line. The helper's first line is `{"ready": true, "duckdb":
version}` or `{"error": {"kind": "no_duckdb", ...}}` (and it exits). Then, from Rust:

  {"op": "load", "id": n,
   "table": {"schema", "name", "columns": [[name, type]], "rows": [...] | null}}
      -> {"id": n, "ok": true}            (rows null: a placeholder that says it is not loaded)
  {"op": "query", "id": n, "query": str, "limit": int}
      -> {"id": n, "columns": [...], "rows": [{col: value}], "total": int, "truncated": bool}
  {"op": "cancel", "id": n}               (no answer: interrupts request n if it is running)

Either request can answer `{"id": n, "error": {"kind", "message", "table"?}}`, with kind
`refused` (not one SELECT, or a write), `missing_table`, `sql`, `interrupted` or
`out_of_memory`.

Requests are read on the main thread and run on one worker thread, so a `cancel` is seen while
a query runs. Stdin is also the lifeline (`barca._lifeline`): when the server is gone, however
it went, stdin reaches end-of-file and the helper exits.
"""

from __future__ import annotations

import json
import os
import queue
import sys
import threading
from typing import Any

from barca._duckdb import _quote
from barca._sql import _ALL_TABLES, _MISSING_TABLE, _SHOW_TABLES, _jsonable, _quote_str

# The most memory DuckDB uses for the tables and a query over them.
MEMORY_LIMIT = "1GB"
# Threads a query runs on: a query nobody waits for any more must not take the machine.
THREADS = 2
# Rows handed to DuckDB in one statement.
LOAD_BATCH = 5000

# Applied in order before the first query. `lock_configuration` comes last: after it no
# statement changes a setting, these included.
_LOCKDOWN = (
    # No spilling to disk: by default DuckDB writes what does not fit in memory to `.tmp/` in
    # the working directory, the server's project root. Set before external access goes: after
    # it the setting cannot be changed.
    "SET temp_directory = ''",
    f"SET memory_limit = '{MEMORY_LIMIT}'",
    f"SET threads = {THREADS}",
    "SET autoinstall_known_extensions = false",
    "SET autoload_known_extensions = false",
    "SET allow_community_extensions = false",
    "SET python_enable_replacements = false",
    "SET enable_external_access = false",
    "SET lock_configuration = true",
)
# Settings a supported DuckDB may not have; the rest must apply or the helper does not start.
_OPTIONAL = ("allow_community_extensions", "python_enable_replacements")


class Refused(Exception):
    """The request is not one read of barca's tables."""


class Helper:
    def __init__(self, duckdb: Any):
        self.duckdb = duckdb
        self.con = duckdb.connect(":memory:")
        for statement in _LOCKDOWN:
            try:
                self.con.execute(statement)
            except duckdb.Error:
                if not any(name in statement for name in _OPTIONAL):
                    raise
        self.lock = threading.Lock()
        self.running: int | None = None
        # Requests told to stop before or while they ran, and the last one answered: a cancel
        # that arrives after its request ended is dropped.
        self.cancelled: set[int] = set()
        self.finished = -1

    # ─── cancelling ───────────────────────────────────────────────────────────

    def cancel(self, request: int) -> None:
        with self.lock:
            if request <= self.finished:
                return
            self.cancelled.add(request)
            if self.running == request:
                self.con.interrupt()

    def _check(self, request: int) -> None:
        with self.lock:
            if request in self.cancelled:
                raise self.duckdb.InterruptException("Interrupted!")

    # ─── tables ───────────────────────────────────────────────────────────────

    def load(self, table: dict) -> None:
        """Replace one of barca's tables with `rows`, or (rows null) with a placeholder."""
        con = self.con
        qualified = f"{_quote(table['schema'])}.{_quote(table['name'])}"
        columns = table["columns"]
        rows = table.get("rows")
        con.execute("BEGIN")
        try:
            con.execute(f"CREATE SCHEMA IF NOT EXISTS {_quote(table['schema'])}")
            # What is there now is a placeholder (a view) or rows (a table); each has its own
            # DROP, and the wrong one is an error even with IF EXISTS.
            is_view = con.execute(
                "SELECT count(*) FROM duckdb_views() WHERE schema_name = ? AND view_name = ?",
                [table["schema"], table["name"]],
            ).fetchall()[0][0]
            con.execute(f"DROP {'VIEW' if is_view else 'TABLE'} IF EXISTS {qualified}")
            if rows is None:
                why = (
                    f"barca: {table['schema']}.{table['name']} is not loaded; name it in the query"
                )
                nulls = ", ".join(f"CAST(NULL AS {ty}) AS {_quote(c)}" for c, ty in columns)
                con.execute(
                    f"CREATE VIEW {qualified} AS SELECT {nulls} "
                    f"WHERE error({_quote_str(why)}) IS NULL"
                )
            else:
                con.execute(
                    f"CREATE TABLE {qualified} "
                    f"({', '.join(f'{_quote(c)} {ty}' for c, ty in columns)})"
                )
                # The rows as one JSON parameter per batch: a file would need the file access
                # this database does not have.
                spec = json.dumps([{c: ty for c, ty in columns}])
                insert = (
                    f"INSERT INTO {qualified} SELECT unnest(from_json(?::JSON, ?), max_depth := 2)"
                )
                for at in range(0, len(rows), LOAD_BATCH):
                    batch = [
                        {c: row.get(c) for c, _ in columns} for row in rows[at : at + LOAD_BATCH]
                    ]
                    con.execute(insert, [json.dumps(batch), spec])
            con.execute("COMMIT")
        except BaseException:
            con.execute("ROLLBACK")
            raise

    # ─── queries ──────────────────────────────────────────────────────────────

    def _select(self, query: str) -> None:
        """Refuse anything but one SELECT, as DuckDB parses it."""
        duckdb = self.duckdb
        try:
            statements = self.con.extract_statements(query)
        except duckdb.Error as e:
            raise Refused(str(e).splitlines()[0]) from e
        if len(statements) != 1:
            raise Refused(f"one SELECT statement at a time, not {len(statements)}")
        kind = statements[0].type
        if kind != duckdb.StatementType.SELECT:
            name = str(kind).rsplit(".", 1)[-1].replace("_STATEMENT", "")
            raise Refused(f"only a SELECT, not a {name} statement")

    def query(self, request: int, query: str, limit: int) -> dict:
        query = query.strip().rstrip(";").strip()
        self._select(query)
        if _SHOW_TABLES.match(query):
            query = _ALL_TABLES
        con = self.con
        self._check(request)
        con.execute("BEGIN TRANSACTION READ ONLY")
        try:
            rel = con.sql(query)
            if rel is None:
                raise Refused("the statement returns no rows")
            columns = list(rel.columns)
            fetched = rel.limit(limit + 1).fetchall()
            truncated = len(fetched) > limit
            if truncated:
                fetched = fetched[:limit]
                self._check(request)
                counted = con.sql(f"SELECT count(*) FROM ({query}) AS barca_sql").fetchall()
                total = counted[0][0] if counted else limit
            else:
                total = len(fetched)
        finally:
            try:
                con.execute("ROLLBACK")
            except self.duckdb.Error:
                pass  # an error already ended the transaction
        rows = [{c: _jsonable(v) for c, v in zip(columns, r, strict=False)} for r in fetched]
        return {"columns": columns, "rows": rows, "total": total, "truncated": truncated}

    # ─── one request ──────────────────────────────────────────────────────────

    def answer(self, message: dict) -> dict:
        duckdb = self.duckdb
        request = message["id"]
        with self.lock:
            self.running = request
        try:
            if message["op"] == "load":
                self.load(message["table"])
                return {"id": request, "ok": True}
            return {"id": request, **self.query(request, message["query"], message["limit"])}
        except Refused as e:
            return _error(request, "refused", str(e))
        except duckdb.InterruptException as e:
            return _error(request, "interrupted", str(e))
        except duckdb.OutOfMemoryException as e:
            return _error(request, "out_of_memory", str(e))
        except duckdb.TransactionException as e:
            # A write hidden in a SELECT (a data-modifying CTE): the read-only transaction.
            return _error(request, "refused", str(e))
        except duckdb.Error as e:
            text = str(e)
            m = _MISSING_TABLE.search(text)
            if m and isinstance(e, duckdb.CatalogException):
                answer = _error(request, "missing_table", text)
                answer["error"]["table"] = m.group(1)
                return answer
            return _error(request, "sql", text)
        finally:
            with self.lock:
                self.running = None
                self.finished = request
                self.cancelled.discard(request)


def _error(request: int, kind: str, message: str) -> dict:
    return {"id": request, "error": {"kind": kind, "message": message}}


def _say(out, document: dict) -> None:
    out.write(json.dumps(document))
    out.write("\n")
    out.flush()


def main() -> None:
    # The protocol owns stdout: nothing else (a library's print) may write to it.
    out = os.fdopen(os.dup(1), "w")
    os.dup2(2, 1)
    try:
        import duckdb
    except ImportError:
        _say(
            out,
            {
                "error": {
                    "kind": "no_duckdb",
                    "message": "the query route needs duckdb in the Python environment "
                    "barca serve uses",
                }
            },
        )
        return
    helper = Helper(duckdb)
    _say(out, {"ready": True, "duckdb": duckdb.__version__})

    work: queue.Queue[dict] = queue.Queue()

    def worker() -> None:
        while True:
            message = work.get()
            try:
                answer = helper.answer(message)
            except Exception as e:  # noqa: BLE001 - the server restarts a helper that says so
                answer = _error(message.get("id", -1), "internal", f"{type(e).__name__}: {e}")
            _say(out, answer)

    threading.Thread(target=worker, name="barca-sql-query", daemon=True).start()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        message = json.loads(line)
        if message.get("op") == "cancel":
            helper.cancel(message["id"])
        else:
            work.put(message)
    # End of stdin: the server is gone (or closed the helper). A running query dies with us.
    os._exit(0)


if __name__ == "__main__":
    main()
