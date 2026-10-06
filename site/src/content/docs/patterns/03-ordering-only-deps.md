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
def seed_data():
    insert_seed_records()
```

The `_migrate` parameter name starts with `_`, which tells barca to establish the DAG edge (ensuring `migrate_db` finishes before `seed_data` starts) and pass `None` to the function instead of the upstream value. The function body never references `_migrate`.

## Why this works

- **Intent is explicit.** Anyone reading the code immediately sees that the dependency is for ordering, not data flow. The parameter exists in the signature to satisfy static analysis, but the `_` prefix signals "I don't use this value."
- **Value is not passed.** The function receives `None` for `_`-prefixed parameters, making it clear the dependency is structural. The upstream artifact is still materialized and cached as normal -- the `_` prefix only affects what the downstream function sees.
- **DAG is still correct.** The edge is still present in the execution plan. Barca will still schedule `seed_data` in a later tier than `migrate_db`.

## Unused data inputs are flagged

A data input (no `_` prefix) is loaded in full before the step runs. If the function never
references it, or only `del`s it, `barca plan`, `barca get` and `barca run` print a warning on
stderr and add a `warnings` entry to the JSON output. The fix is to remove the input, or to
rename it `_<name>` when you only need the ordering. Passing the parameter to a helper counts as
using it. See `barca docs assets`.

## Common mistakes

### Using a normal parameter name and ignoring it

```python
# Works but unclear intent
@task(inputs={"migrate": migrate_db})
def seed_data(migrate):  # never used, but barca still passes the value
    insert_seed_records()
```

This is functionally correct but unclear to readers -- the parameter looks like it carries data. Use the `_` prefix to signal that the dependency is for ordering only and the value is not needed.

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
