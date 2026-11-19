"""The server's query helper: `POST /sql/query`, invoked by Rust as `python -m barca._sql_server`.

One long-lived process per `barca serve`, holding an in-memory DuckDB with barca's own records
(`barca.runs`, `barca.materializations`, `barca.nodes`, `barca.schedules`, `barca.ticks`,
`barca.queue`) and nothing else: no result view, no file. Rust hands it the rows of a table
when the metadata database or the graph changed, and one query at a time.

A query comes from whoever can reach the server, so the database is closed before the first
one runs, and each of these holds without the others:

- No file, network or extension access: `enable_external_access` is off, extensions are neither
  installed nor loaded, Python variables are not scanned, and the configuration is locked, so no
  statement can turn any of it back on. Rows arrive as a JSON parameter, never as a file.
- One statement, a `SELECT`, as DuckDB's own parser reads it (not a pattern over the text),
  and one that starts as a query does (`_QUERY_WORDS`). The second is there for DuckDB 1.x,
  whose parser rewrites a table-function pragma (`PRAGMA database_list`, `PRAGMA
  table_info(...)`) into a `SELECT` before it says what the statement is, and runs the pragma
  to do it; 2.x reads those as `PRAGMA`. The word is checked first, so no pragma reaches the
  parser on either.
- A read-only transaction, rolled back: DuckDB 2 accepts `WITH x AS (DELETE ... RETURNING *)
  SELECT ...` as a `SELECT`, and the transaction refuses the write.
- Memory and threads are bounded, and nothing is spilled to disk: a query that needs more
  memory fails, and the helper goes on.
- The answer is bounded: rows are fetched a few at a time and encoded as they come, and a
  query whose rows pass `MAX_ANSWER_BYTES` is refused before more are read. DuckDB's memory
  limit covers its own buffers only; without this the rows would be held again in Python, in
  the line sent to the server, and in the server.
- The helper is started outside the project (an empty working directory that is also its
  `HOME`, `python -P`), with DuckDB's secret and extension directories unset: the project is
  not importable, and no setting or error names the server's home.

Protocol: one JSON document per line. The helper's first line is `{"ready": true, "duckdb":
version}` or `{"error": {"kind": "no_duckdb", ...}}` (and it exits). Then, from Rust:

  {"op": "load", "id": n,
   "table": {"schema", "name", "columns": [[name, type]], "rows": [...] | null}}
      -> {"id": n, "ok": true}            (rows null: a placeholder that says it is not loaded)
  {"op": "query", "id": n, "query": str, "limit": int}
      -> {"id": n, "columns": [...], "rows": [{col: value}], "total": int, "truncated": bool}
  {"op": "cancel", "id": n}               (no answer: interrupts request n if it is running)

Either request can answer `{"id": n, "error": {"kind", "message", "table"?}}`, with kind
`refused` (not one SELECT, a write, or more rows than an answer holds), `missing_table`, `sql`,
`interrupted` or `out_of_memory`. A message is cut at `MAX_MESSAGE` characters.

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

from barca import _helper, _sql_tables
from barca._duckdb import _quote
from barca._sql_tables import jsonable as _jsonable

# The most memory DuckDB uses for the tables and a query over them.
MEMORY_LIMIT = "1GB"
# Threads a query runs on: a query nobody waits for any more must not take the machine.
THREADS = 2
# Rows handed to DuckDB in one statement.
LOAD_BATCH = 5000
# The most bytes of encoded rows one answer holds. The server reads no longer line than this
# and a little (`MAX_LINE` in `sql_query.rs`).
MAX_ANSWER_BYTES = 4 * 1024 * 1024
# Rows taken from DuckDB at a time while the answer is counted.
FETCH_ROWS = 64
# The most characters of an error message sent to the server (DuckDB quotes the query).
MAX_MESSAGE = 2000

# Applied in order before the first query. `lock_configuration` comes last: after it no
# statement changes a setting, these included.
_LOCKDOWN = (
    # No spilling to disk: by default DuckDB writes what does not fit in memory to `.tmp/` in
    # the working directory, the server's project root. Set before external access goes: after
    # it the setting cannot be changed.
    "SET temp_directory = ''",
    f"SET memory_limit = '{MEMORY_LIMIT}'",
    f"SET threads = {THREADS}",
    # No directory of the server's user: persistent secrets and installed extensions live
    # under `~/.duckdb` by default, and DuckDB names those paths in settings and errors.
    "SET secret_directory = ''",
    "SET allow_persistent_secrets = false",
    "SET extension_directory = '/nonexistent'",
    "SET autoinstall_known_extensions = false",
    "SET autoload_known_extensions = false",
    "SET allow_community_extensions = false",
    "SET python_enable_replacements = false",
    "SET enable_external_access = false",
    "SET lock_configuration = true",
)
# Settings a supported DuckDB may not have; the rest must apply or the helper does not start.
_OPTIONAL = (
    "allow_community_extensions",
    "python_enable_replacements",
    "allow_persistent_secrets",
)


# How a query starts: the first word of the statement, after whitespace, comments and opening
# parentheses. Everything else is refused before DuckDB parses it. `PIVOT` is left out: DuckDB
# parses it as more than one statement.
_QUERY_WORDS = frozenset(
    {
        "SELECT",
        "WITH",
        "FROM",
        "VALUES",
        "TABLE",
        "SHOW",
        "DESCRIBE",
        "DESC",
        "SUMMARIZE",
        "UNPIVOT",
    }
)


def _first_word(query: str) -> str:
    """The first word of `query`, upper case: past whitespace, `--` and `/* */` comments (which
    nest) and opening parentheses. Empty when it does not start with a word."""
    at, end = 0, len(query)
    while at < end:
        if query[at].isspace() or query[at] == "(":
            at += 1
        elif query.startswith("--", at):
            newline = query.find("\n", at)
            at = end if newline < 0 else newline + 1
        elif query.startswith("/*", at):
            depth, at = 1, at + 2
            while at < end and depth:
                if query.startswith("/*", at):
                    depth, at = depth + 1, at + 2
                elif query.startswith("*/", at):
                    depth, at = depth - 1, at + 2
                else:
                    at += 1
        else:
            break
    word = at
    while word < end and (query[word].isalpha() or query[word] == "_"):
        word += 1
    return query[at:word].upper()


class Refused(Exception):
    """The request is not one read of barca's tables."""


class Helper:
    def __init__(self, duckdb: Any):
        self.duckdb = duckdb
        self.con = _sql_tables.connect(duckdb)
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
        qualified = _sql_tables.qualified(table)
        columns = table["columns"]
        rows = table.get("rows")
        not_loaded = f"barca: {table['schema']}.{table['name']} is not loaded; name it in the query"
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
            if _sql_tables.create(con, table, not_loaded):
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

    def _select(self, query: str) -> str:
        """The query as one SELECT, as DuckDB parses it; anything else is refused."""
        duckdb = self.duckdb
        word = _first_word(query)
        if word not in _QUERY_WORDS:
            raise Refused(f"only a SELECT, not a {word or 'statement like this'} statement")
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
        return query

    def _drop_result(self) -> None:
        """Let go of a result that was not read to its end (DuckDB 2 keeps the connection
        for it until then)."""
        abort = getattr(self.con, "abort", None)
        if abort is not None:
            try:
                abort()
            except self.duckdb.Error:
                pass

    def query(self, request: int, query: str, limit: int) -> str:
        """The answer to `query`, as the JSON line to send: rows are encoded once."""
        query = self._select(query.strip())
        if _sql_tables.SHOW_TABLES.match(query.rstrip(";").strip()):
            query = _sql_tables.ALL_TABLES
        con = self.con
        self._check(request)
        con.execute("BEGIN TRANSACTION READ ONLY")
        try:
            # The statement as it was sent, never inside another: a wrapper (`SELECT ... FROM
            # (<query>) LIMIT n`) breaks on a trailing `-- comment` or `;`, and telling those
            # from the same characters in a string is the parser's job. The limit is where
            # reading stops; the rows past it are counted without being kept.
            result = con.execute(query)
            columns = [d[0] for d in result.description or []]
            encoded: list[str] = []
            size = 0
            total = 0
            while True:
                self._check(request)
                rows = result.fetchmany(FETCH_ROWS)
                if not rows:
                    break
                total += len(rows)
                for row in rows[: max(0, limit - len(encoded))]:
                    line = json.dumps({c: _jsonable(v) for c, v in zip(columns, row, strict=False)})
                    size += len(line)
                    if size > MAX_ANSWER_BYTES:
                        raise Refused(
                            f"the rows are more than the {MAX_ANSWER_BYTES // (1024 * 1024)}MB "
                            f"an answer holds (reached at row {len(encoded) + 1}): select "
                            "fewer or narrower columns, aggregate, or send a smaller limit"
                        )
                    encoded.append(line)
        finally:
            self._drop_result()
            try:
                con.execute("ROLLBACK")
            except self.duckdb.Error:
                pass  # an error already ended the transaction
        head = json.dumps({"id": request, "columns": columns})
        tail = json.dumps({"total": total, "truncated": total > len(encoded)})
        return f'{head[:-1]}, "rows": [{", ".join(encoded)}], {tail[1:]}'

    # ─── one request ──────────────────────────────────────────────────────────

    def answer(self, message: dict) -> str:
        """The JSON line that answers `message`."""
        answer = self._answer(message)
        return answer if isinstance(answer, str) else json.dumps(answer)

    def _answer(self, message: dict) -> dict | str:
        duckdb = self.duckdb
        request = message["id"]
        with self.lock:
            self.running = request
        try:
            if message["op"] == "load":
                self.load(message["table"])
                return {"id": request, "ok": True}
            return self.query(request, message["query"], message["limit"])
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
            table = _sql_tables.missing_table(duckdb, e)
            if table is not None:
                answer = _error(request, "missing_table", text)
                answer["error"]["table"] = table
                return answer
            return _error(request, "sql", text)
        finally:
            with self.lock:
                self.running = None
                self.finished = request
                self.cancelled.discard(request)


def _error(request: int, kind: str, message: str) -> dict:
    return {"id": request, "error": {"kind": kind, "message": message[:MAX_MESSAGE]}}


def _say(out, document: dict | str) -> None:
    out.write(document if isinstance(document, str) else json.dumps(document))
    out.write("\n")
    out.flush()


def main() -> None:
    # The protocol owns stdout: nothing else (a library's print) may write to it.
    out = _helper.protocol_stdout()
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
                answer: dict | str = helper.answer(message)
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
