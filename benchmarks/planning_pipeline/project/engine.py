"""DuckDB helpers. Everything runs on DuckDB's default connection, the one barca binds
inputs on, so named views and temp tables created here are shared by every step that runs in
the same worker process.
"""

from contextlib import contextmanager

import duckdb
import polars as pl


def con():
    default = duckdb.default_connection
    return default() if callable(default) else default


def row_count(rel) -> int:
    """Run an aggregate on a lazy input: the scan happens here, before the step decides."""
    return rel.aggregate("count(*)").fetchall()[0][0]


def empty(schema: dict) -> duckdb.DuckDBPyRelation:
    cols = ", ".join(f"NULL::{t} AS {c}" for c, t in schema.items())
    return con().sql(f"SELECT {cols} LIMIT 0")


def from_polars(df: pl.DataFrame) -> duckdb.DuckDBPyRelation:
    return con().from_arrow(df.to_arrow())


@contextmanager
def staged(rel, alias: str):
    """Expose `rel` under fixed names, the way the original steps share SQL text.

    Creates the view `<alias>_rows` (replacing any earlier one), registers the materialized
    rows as `<alias>_frame` and unregisters it on exit, and fills the temp table
    `<alias>_keys`. Steps that use the same alias reuse the same names on the shared
    connection.
    """
    c = con()
    rel.create_view(f"{alias}_rows", replace=True)
    c.register(f"{alias}_frame", rel.to_arrow_table())
    c.execute(f"CREATE OR REPLACE TEMP TABLE {alias}_keys AS SELECT DISTINCT * FROM {alias}_frame")
    try:
        yield c
    finally:
        c.unregister(f"{alias}_frame")
