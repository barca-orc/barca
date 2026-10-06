# Types and output formats

How barca stores each step's output and how it hands values to downstream steps.

## Output: the returned value picks the format

| Step returns | Stored as |
|---|---|
| pandas `DataFrame`, polars `DataFrame`/`LazyFrame`, pyarrow `Table`, duckdb relation | parquet (`.parquet`) |
| JSON-serializable value (dict, list, str, int, float, bool, None) | json (`.json`) |
| anything else | pickle (`.pkl`, protocol 5) |

- Override with `@asset(serializer="json" | "pickle" | "parquet")`.
- The **return annotation does not choose the format**; the value (or `serializer=`) does.
- Asking for parquet on a value that cannot be written as parquet falls back to pickle and
  prints a warning on stderr.
- Lazy values are materialized when the step ends: a polars `LazyFrame` is collected and a
  duckdb relation is executed straight into the parquet file. Nothing lazy crosses a step.
- pandas parquet needs `pyarrow` (`pip install "barca[parquet]"`).

## Input: parameter annotations pick the reader

For parquet artifacts, the annotation on the *consuming* parameter selects the reader:

| Annotation | Downstream receives |
|---|---|
| none | pandas `DataFrame` (default) |
| `pd.DataFrame` / `pandas.DataFrame` | pandas `DataFrame` |
| `pl.DataFrame` / `polars.DataFrame` | polars `DataFrame` |
| `pl.LazyFrame` / `polars.LazyFrame` | polars `LazyFrame` scanning the parquet file (lazy read) |
| `pyarrow.Table` | pyarrow `Table` |
| `duckdb.DuckDBPyRelation` | duckdb relation over the parquet file (lazy read) |

The same upstream parquet can be read differently by different consumers. Annotations are
parsed statically, so use the conventional names above (`pd`, `pl`, `pyarrow`, `duckdb`).
json and pickle artifacts ignore annotations.

**Lazy inputs read only what the step uses.** The eager readers load the whole file. A
`pl.LazyFrame` or duckdb relation reads nothing up front: the query the step builds decides
which columns and row groups are read when it runs. For a large upstream that a step filters,
projects or aggregates, annotate the input as lazy. With a remote artifact store, a lazy input
is read in place and only the byte ranges its query touches are fetched (`barca docs remote`).

```python
import duckdb
import polars as pl
from barca import asset


@asset()
def orders() -> duckdb.DuckDBPyRelation:      # written as parquet
    return duckdb.sql("select 1 as id, 9.5::double as amount")


@asset(inputs={"orders": orders})
def total(orders: duckdb.DuckDBPyRelation) -> dict:   # read as a duckdb relation
    return {"total": orders.sum("amount").fetchone()[0]}


@asset(inputs={"orders": orders})
def as_polars(orders: pl.DataFrame) -> pl.DataFrame:  # same file, read with polars
    return orders.with_columns(doubled=pl.col("amount") * 2)


@asset(inputs={"orders": orders})
def big_ids(orders: pl.LazyFrame) -> pl.LazyFrame:    # same file, scanned lazily
    return orders.filter(pl.col("amount") > 5).select("id")
```

## Reading results back

`barca get` prints one JSON object on stdout (whenever stdout is not a terminal, or with `--json`). `final_output` is the value itself for json
artifacts; for parquet and pickle it is a pointer:

```json
{"final_output": {"_barca_artifact": {"path": ".barca/artifacts/...parquet", "format": "parquet", "size_bytes": 862}}}
```

To see the data, either call the Python API, which deserializes for you
(`import barca; barca.get("orders", "pipeline.py")` returns a pandas `DataFrame` for parquet),
or read the `path` yourself (`duckdb.sql("select * from '<path>'")`).

## Gotchas

- DuckDB `DECIMAL` values (including literals like `9.5` and the result of `sum()` over them)
  arrive in Python as `decimal.Decimal`, which is not JSON-serializable. A dict containing one
  is stored as pickle, not JSON. Cast to `double` in SQL or convert with `float(...)`.
- Pickle fails for objects that cannot be pickled (open connections, duckdb relations,
  generators). Return data, not handles.
- A duckdb relation is executed when the step ends, so any connection it uses must still be
  alive when the step returns.
- **DuckDB connections.** Barca owns one DuckDB connection per worker process: duckdb's default
  connection, the same one `duckdb.sql(...)` and `duckdb.read_parquet(...)` use. Inputs
  annotated `duckdb.DuckDBPyRelation` are loaded on it and also bound as **views named after
  their parameters** for the duration of the step (dropped after the result is written), so
  `duckdb.sql("select * from orders")` works anywhere, helper modules included, with no bind
  code of your own. Configure the connection once per worker process at import time with
  `barca.duckdb_connection()`: extensions, credentials, `SET` options, macros:

```python
import barca

barca.duckdb_connection().execute("SET threads = 4")   # runs once per worker process
```

- Stay on that connection. If a step opens its own `duckdb.connect()`, its relations cannot be
  combined with inputs (`Cannot combine LEFT and RIGHT relations of different connections!`),
  and `con.register("x", input)` fails the same way. Barca recognizes these DuckDB errors
  (including querying a bound input by name from another connection, which DuckDB reports as
  `Table with name ... does not exist`) and appends a `barca:` note to the step failure that
  says what happened and what to do. If you must, copy the input across with
  `con.register("x", input.arrow())` (the data is loaded into memory). A relation does not
  expose its file path.
- Do not `register` on barca's connection an Arrow table that came from a query on that same
  connection: it hangs (duckdb 1.5.6). Use the relation directly, or `create_view`.
- Anything you create on the connection (tables, macros, `SET` options) lives as long as the
  worker process, which runs several steps; barca only cleans up the views it binds for inputs.
  Prefer stateless SQL and `drop` what you create.
- Everything is materialized between steps. To cache several results from one computation,
  define several assets or return the one you want cached.

See also: `barca docs cache`, `barca docs examples/duckdb`.
