---
title: "Pattern: Ordering-Only Dependencies"
description: Start an input's name with an underscore when a step must run after another but does not need its value.
---

Use this when one step must run after another but does not need its result: seed a database
after migrating it, read an external table after the step that wrote it. Start the input's
name with `_`, both the key in `inputs=` and the parameter.

## Example

```python
from barca import task


@task()
def migrate_db():
    run_migrations()


@task(inputs={"_migrate": migrate_db})
def seed_data(_migrate):
    insert_seed_records()
    return {"seeded": 2, "received": repr(_migrate)}
```

`run_migrations()` and `insert_seed_records()` stand for your own code. In the run below they
print `migrating` and `seeding`.

```bash
barca run seed_data pipeline.py
```

## What barca does

`migrate_db` runs first, then `seed_data`. The `_migrate` parameter receives `None`: the
upstream result is not read from disk.

```
migrating
seeding
[barca] 2/2 steps | done in 0.1s
Run 5360fb511240 | ran 'seed_data' in 0.878s (2 steps, 1 phase)

Value:
{
  "received": "None",
  "seeded": 2
}
```

The same works between assets (`@asset(inputs={"_events": events})`). The edge is a real
dependency: the downstream asset runs after the upstream and runs again when the upstream
changes. Only removing the input removes the dependency.

## An unused input without the prefix is loaded, and reported

An input whose name does not start with `_` is loaded and passed whether or not the function
uses it. At plan time barca reads each function body, and when a step never mentions an input
(or only `del`s it), `barca plan`, `barca get`, `barca run` and `--dry-run` print one line on
stderr and add an entry to the `warnings` array of their JSON output:

```python
@task(inputs={"migrate": migrate_db})
def seed_data(migrate):      # never used
    insert_seed_records()
```

```
[barca] warning: unused.py:seed_data never uses its input `migrate`. It is still loaded in full each time the step runs, and it counts toward the step's cache key. Use it, remove it from inputs=, or rename the parameter `_migrate` if it is there for ordering only (a `_` input is not loaded and never flagged)
```

A `_`-prefixed input is never reported. There is no flag or configuration key that turns the
warning off. A name that appears inside a string in the body counts as used (SQL such as
`duckdb.sql("select * from orders")`, a pandas `query("x > @limit")`). The exact rule, and what
is never reported (stubs, `**kwargs`, `locals()`, queries built outside the body, DuckDB
relation inputs, sensor inputs), is in `barca docs assets`, "Unused inputs".

## Limits

- **The function must still accept the parameter.** Barca passes `_migrate=None`. Without the
  parameter the step fails:

  ```
  Worker failed: TypeError: seed_data() got an unexpected keyword argument '_migrate'
  ```

- **There is no `after=` keyword.** Early prototypes had one. On 0.18.0 `@task(after=[migrate_db])`
  is not an error: the keyword is ignored, `migrate_db` does not run, and the command exits 0.
  Use `inputs={"_name": upstream}`.
- **A task cannot be an ordering-only input to an asset**, for the same reason it cannot be a
  data input: [Asset-to-Task](/patterns/02-asset-to-task/).
- A linter that reports unused parameters may flag `_migrate`. Most linters can be configured
  to allow parameters that start with `_`.
