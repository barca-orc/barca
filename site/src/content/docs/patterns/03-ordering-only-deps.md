---
title: "Pattern: Ordering-Only Dependencies"
description: Use the underscore-prefix convention to signal a dependency exists only for ordering, not data.
---

When you need one step to run after another but do not need the upstream step's data. Use the `_` prefix convention to signal ordering-only intent.

## The right way

```python
from barca import task

@task
def migrate_db():
    run_migrations()

@task(inputs={"_migrate": migrate_db})
def seed_data(_migrate):
    insert_seed_records()
```

The `_migrate` parameter name starts with `_`, which tells barca to establish the DAG edge (ensuring `migrate_db` finishes before `seed_data` starts) and pass `None` to the function instead of the upstream value. The function must still accept the parameter (barca passes `_migrate=None`; without it the call fails with a `TypeError`), but its body never references `_migrate`.

## Why this works

- **Intent is explicit.** Anyone reading the code immediately sees that the dependency is for ordering, not data flow. The parameter exists in the signature to satisfy static analysis, but the `_` prefix signals "I don't use this value."
- **Value is not passed.** The function receives `None` for `_`-prefixed parameters, making it clear the dependency is structural. The upstream artifact is still materialized and cached as normal -- the `_` prefix only affects what the downstream function sees.
- **DAG is still correct.** The edge is still present in the execution plan. Barca will still schedule `seed_data` in a later tier than `migrate_db`.

## The `_` prefix is how you declare an input unused on purpose

An input without the `_` prefix is loaded and passed whether or not the function uses it. At plan
time barca reads each function body and, when a step never mentions an input (or only `del`s it),
`barca plan`, `barca get`, `barca run` and `--dry-run` print one line on stderr and add an entry
to the `warnings` array of their JSON output:

```
[barca] warning: pipeline.py:seed_data never uses its input `migrate`. It is still loaded in full each time the step runs, and it counts toward the step's cache key. Use it, remove it from inputs=, or rename the parameter `_migrate` if it is there for ordering only (a `_` input is not loaded and never flagged)
```

A `_`-prefixed input is never flagged: the prefix is the documented way to say the input is there
for ordering only. The step still runs after the upstream and still re-runs when the upstream
changes; only removing the input removes that dependency. There is no flag or config key to turn
the warning off. A name that appears inside a string in the body counts as used (SQL such as
`duckdb.sql("select * from orders")`, a pandas `query("x > @limit")`). The exact rule, and what is
never reported (stubs, `**kwargs`, `locals()`, queries built outside the body, duckdb relation
inputs, sensor inputs), is in `barca docs assets`, "Unused inputs".

## Common mistakes

### Using a normal parameter name and ignoring it

```python
# Works but unclear intent
@task(inputs={"migrate": migrate_db})
def seed_data(migrate):  # never used, but barca still passes the value
    insert_seed_records()
```

This runs, but the upstream value is loaded for nothing, the parameter looks like it carries data, and barca prints the unused-input warning above on every `plan`, `get` and `run` that includes the step. Use the `_` prefix to signal that the dependency is for ordering only and the value is not needed.

### Trying to use `after=`

```python
# Wrong -- after= was removed
@task(after=[migrate_db])
def seed_data():
    insert_seed_records()
```

Early prototypes of barca had an `after=` keyword for ordering-only edges. This was removed in favor of the `_` prefix convention on `inputs=`, which keeps a single mechanism for all dependency types. If you see `after=` in old examples, replace it with `inputs={"_name": upstream}`.

## Naming convention

The `_` prefix was chosen deliberately to signal "ordering-only" to barca.
This intentionally overlaps with Python's convention for unused parameters —
if barca won't pass a value, you shouldn't use the parameter anyway.

If you have a linter warning about unused `_` parameters, add a
`# noqa: ARG001` comment or configure your linter to allow `_`-prefixed params.
