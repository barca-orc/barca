---
title: Anti-Patterns
description: Things that fail or give stale or wrong results in barca, what was observed on 0.18.0, and what to do instead.
---

Each section names something to avoid, says what happens on barca 0.18.0, and gives the
alternative.

## Reading outside data in an asset body

```python
import csv
from pathlib import Path

from barca import asset


@asset()
def orders() -> list:
    with Path("orders.csv").open() as f:
        return list(csv.DictReader(f))


@asset(inputs={"orders": orders})
def total(orders: list) -> dict:
    return {"rows": len(orders), "total": sum(float(o["amount"]) for o in orders)}
```

**What happens.** `orders` has no inputs, so its run hash depends only on its code. It is
computed once and then served from cache, whatever happens to the file. After a row is
appended to `orders.csv`, `barca get total pipeline.py` runs 0 steps and returns the old
total. The same holds for a bucket, a database table or an API.

**What to do instead.** Put a `@sensor` in front of the asset that returns something
identifying the current version of the data, and make the asset take it as an input. A sensor
runs every time, and its value is part of the run hash of the assets that read it.

```python
import hashlib

from barca import sensor


@sensor()
def orders_version() -> tuple[bool, str]:
    # Return only what identifies the data. For a bucket: the object's etag.
    return True, hashlib.sha256(Path("orders.csv").read_bytes()).hexdigest()


@asset(inputs={"version": orders_version})
def orders(version: str) -> list:
    with Path("orders.csv").open() as f:
        return list(csv.DictReader(f))
```

For a one-off recompute without a sensor, name the asset:
`barca get total pipeline.py --refresh orders`.

[Sensors and External Observations](/workflows/06-sensors-and-external-observations/) shows
both versions with output, what `--dry-run` and `barca status` report, and what goes wrong
when a sensor's value changes on every run.

## Task as input to an asset

```python
from barca import asset, task


@task()
def migrate_db() -> None:
    print("migrating")


@asset(inputs={"_done": migrate_db})
def user_counts(_done) -> dict:
    return {"users": 3}
```

**What happens.** Barca rejects the graph before anything runs (exit 2):

```
DAG error: task 'bad.py:migrate_db' cannot be an input to asset 'bad.py:user_counts' (tasks are never cached, so this would poison caching)
```

**What to do instead.** If the upstream produces a value worth caching, make it an `@asset`.
If it is an action, keep it a `@task` and make the things that must follow it tasks too.

## Calling an asset function directly

```python
@asset()
def cleaned() -> list:
    return [1, 2, 3]


@asset()
def summary() -> dict:
    return {"count": len(cleaned())}
```

**What happens.** The decorators return the function unchanged, so this is an ordinary Python
call and it works. But `cleaned` is not a dependency of `summary` (`barca list` shows `-`
under DEPS), its result is not stored on its own, and it runs inside `summary` every time
`summary` runs.

**What to do instead.** Declare it: `@asset(inputs={"cleaned": cleaned})`.

## Mutating asset inputs in place

```python
@asset(inputs={"data": raw_data})
def processed(data: dict) -> dict:
    data["new_field"] = compute()  # mutating the input
    return data
```

**What happens.** Mutating an input never changes the artifact on disk, and each step gets its
own copy of a value its worker has cached, so an in-place edit normally stays inside the step
that made it. The copy has gaps: a pandas DataFrame nested inside a dict or list, a pandas
Series, a polars DataFrame built over a numpy array, and Arrow-backed pandas columns can share
memory with the cached value. Mutating one of those in place can change what a later step in
the same worker receives.

**What to do instead.** Build a new value and return it:

```python
@asset(inputs={"data": raw_data})
def processed(data: dict) -> dict:
    return {**data, "new_field": compute()}
```

## Fixed names on DuckDB's shared connection

`duckdb.sql(...)`, `duckdb.register(...)` and `duckdb.read_parquet(...)` all use DuckDB's
default connection, and there is one per process. A worker process runs several steps, and a
test process runs several tests. A view or table created under a fixed name by one of them is
still there for the next.

**What happens: a failure.** Two steps each create a view called `picked`:

```python
import duckdb
from barca import asset


@asset()
def orders() -> duckdb.DuckDBPyRelation:
    return duckdb.sql("""
        select * from (values (1, 'emea', 120.0), (2, 'amer', 80.0), (3, 'emea', 45.5))
        t(id, region, amount)
    """)


@asset(inputs={"orders": orders})
def emea_total(orders: duckdb.DuckDBPyRelation) -> dict:
    duckdb.sql("create view picked as select * from orders where region = 'emea'")
    return {"total": duckdb.sql("select sum(amount)::double from picked").fetchone()[0]}


@asset(inputs={"orders": orders, "_after": emea_total})
def amer_total(orders: duckdb.DuckDBPyRelation, _after) -> dict:
    duckdb.sql("create view picked as select * from orders where region = 'amer'")
    return {"total": duckdb.sql("select sum(amount)::double from picked").fetchone()[0]}
```

```
$ barca get amer_total pipeline.py
[barca] 2/3 steps | failed in 25.1s
[barca] run failed: step 'pipeline.py:amer_total' failed (exit 1)
Worker failed: CatalogException: Catalog Error: View with name "picked" already exists!
```

Running the same command again succeeds: `emea_total` is now cached, so `amer_total` is the
only step in its worker. Whether the failure appears depends on which steps share a process.

**What happens: a wrong result.** Here the first step registers a DataFrame as `picked` and
the second relies on DuckDB finding its own local variable called `picked`:

```python
import duckdb
import pandas as pd
from barca import asset


@asset()
def orders() -> pd.DataFrame:
    return pd.DataFrame({"id": [1, 2, 3], "region": ["emea", "amer", "emea"],
                         "amount": [120.0, 80.0, 45.5]})


@asset(inputs={"orders": orders})
def emea_total(orders: pd.DataFrame) -> dict:
    duckdb.register("picked", orders[orders.region == "emea"])
    return {"total": duckdb.sql("select sum(amount)::double from picked").fetchone()[0]}


@asset(inputs={"orders": orders, "_after": emea_total})
def amer_total(orders: pd.DataFrame, _after) -> dict:
    picked = orders[orders.region == "amer"]
    return {"total": duckdb.sql("select sum(amount)::double from picked").fetchone()[0]}
```

The registered view wins over the local variable. `amer_total` returns the EMEA total, the
run succeeds, and the wrong value is cached:

```
$ barca get amer_total pipeline.py
[barca] 3/3 steps | done in 1.7s
...
{
  "total": 165.5
}
$ barca get amer_total pipeline.py --refresh amer_total     # alone in a new worker
[barca] 1/3 steps | done in 2.0s
...
{
  "total": 80.0
}
```

The same two functions called from two pytest tests in one process fail the same way:
`test_amer_total` passes when run alone and fails with `assert 165.5 == 80.0` when it runs
after `test_emea_total`.

**What to do instead.** Any one of these; each was run against the examples above and gave
`80.0` for `amer_total` with both steps in one worker:

- Do not create named objects. Keep a relation in a Python variable and use the relation API
  or a `with` clause in the SQL:
  `picked = orders.filter("region = 'emea'")`, then `picked.aggregate("sum(amount)::double")`.
- If you need a name, make it unique to the step and drop it when done:

  ```python
  duckdb.sql("create or replace temp view amer_total_picked as select * from orders where region = 'amer'")
  try:
      total = duckdb.sql("select sum(amount)::double from amer_total_picked").fetchone()[0]
  finally:
      duckdb.sql("drop view amer_total_picked")
  ```

- When the inputs are not DuckDB relations (pandas, polars, Arrow), use a connection of the
  step's own: `con = duckdb.connect()`, `con.register("picked", df)`, `con.sql(...)`,
  `con.close()`. This does not work with inputs annotated `duckdb.DuckDBPyRelation`: they live
  on barca's connection and cannot be combined with another one (`barca docs types`,
  "DuckDB connections").
- In tests, give every test a new default connection:

  ```python
  # conftest.py
  import duckdb
  import pytest


  @pytest.fixture(autouse=True)
  def fresh_duckdb_default_connection():
      duckdb.set_default_connection(duckdb.connect())
      yield
  ```

  Do this in tests only. In a worker, barca owns the default connection.

Views that barca itself binds for `duckdb.DuckDBPyRelation` inputs, named after their
parameters, are dropped after each step and are not affected. Checked with duckdb 1.5.6.

## Calling the barca CLI from inside a step

```python
import subprocess

from barca import task


@task()
def orchestrate() -> dict:
    p = subprocess.run(["barca", "get", "inner", "pipeline.py", "--json"],
                       capture_output=True, text=True)
    return {"returncode": p.returncode}
```

**What happens.** It works: the inner command waits its turn for the metadata database and
completes. But it is a second, separate run. It has its own entry in `barca history`, its own
workers, and its steps are not part of the outer run's plan, `--dry-run` or step list. If the
inner run fails, the outer step succeeds unless it checks the return code.

**What to do instead.** Declare the dependency with `inputs=` and let one run plan both. To
fan work out from inside a task, use
[`parallel()`](/patterns/04-parallel-tasks/).

## Sharing outside state between steps with no dependency

```python
@task()
def write_report() -> None:
    upload("reports/latest.json")


@task()
def announce_report() -> None:
    post_link("reports/latest.json")
```

**What happens.** Barca orders steps only by their declared inputs. Two steps with no edge
between them may run in either order, or at the same time in different workers.

**What to do instead.** Declare the order. An input whose name starts with `_` orders two
steps without passing a value ([Ordering-Only Dependencies](/patterns/03-ordering-only-deps/)):

```python
@task(inputs={"_write": write_report})
def announce_report(_write) -> None:
    post_link("reports/latest.json")
```
