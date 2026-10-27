---
title: "DuckDB Versions"
description: "duckdb 1.x and 2.x: what a pipeline must change for duckdb 2, and why a database file duckdb 2 writes cannot be opened by 1.x."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/duckdb.md
---

<!-- Generated from crates/barca-cli/docs/duckdb.md (barca docs duckdb). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

:::note
This page is the manual topic `barca docs duckdb`: the CLI prints the same text.
:::

`barca[sql]` accepts duckdb 1.4 up to (not including) 3, and barca's tests run on 1.x and 2.x.
duckdb 1.1 to 1.3 are not supported (on 1.1, for one, dataset sinks fail).

Moving a pipeline to duckdb 2 can need these changes in your own code (checked with 2.0.0):

- Relations no longer have `fetchone()`, `fetchmany()`, `fetch_df_chunk()` or `execute()`.
  `rel.fetchone()` becomes `rel.limit(1).fetchall()[0]` and `rel.fetchone()[0]` becomes
  `rel.fetchall()[0][0]`; both also work on 1.x. The step fails with `AttributeError` otherwise.
- A result of `duckdb.execute(...)` that has not been read to the end (for example
  `duckdb.execute(q).fetchone()`) blocks any other work on the connection:
  `ResourceInUseException: ... unfinished result from execute()`. Barca discards such a
  result when the step returns, so it does not stop the returned relation from being written
  or the next step from running, but a later `duckdb.sql(...)` in the same step fails. Use
  `duckdb.sql(q).fetchall()`, read the result to the end, or call `abort()` on the connection.
- `fetch_arrow_table()` and `fetch_record_batch()` on a relation warn
  (`DeprecationWarning`); use `to_arrow_table()` and `to_arrow_reader()`. `rel.arrow()`
  returns a `RecordBatchReader` that can be read once (every version barca
  supports); `to_arrow_table()` returns a table.
- SQL: the `x -> x + 1` lambda syntax is a binder error (use `lambda x: x + 1`, which 1.x also
  accepts), and integer `//` or `%` by zero raises instead of returning NULL (wrap it in
  `TRY(...)`). DuckDB's migration guide lists the rest:
  https://duckdb.org/docs/2.0/operations_manual/migration
- `barca sql` with a statement that returns no rows (`SET`, `CREATE ...`) reports no columns on
  duckdb 1.x and DuckDB's status column on duckdb 2 (`"columns": ["Success"]` for `SET`, `Count`
  for `CREATE`), with no rows either way.
- **Database files.** A `.duckdb` file that duckdb 2 creates cannot be opened by duckdb 1.x
  (`Trying to read a database file with version number 69`). Barca's own artifacts are
  parquet, csv and json files, which both read; this applies to database files your
  pipeline opens with `duckdb.connect(path)` or `ATTACH`. If workers or machines on 1.x share
  such a file, upgrade them together, or create it with
  `ATTACH 'file.duckdb' (STORAGE_VERSION 'v1.5.0')` so 1.x can still open it.

See also: `barca docs types`, `barca docs big-inputs`, [`barca docs sql`](/reference/sql/), `barca docs examples/duckdb`.
