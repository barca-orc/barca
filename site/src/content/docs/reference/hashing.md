---
title: "The Run Hash"
description: "Which project code a step's run hash follows from its function, which decorator arguments count, and what barca does not follow."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/hashing.md
---

<!-- Generated from crates/barca-cli/docs/hashing.md (barca docs hashing). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

A step runs again when its run hash changes ([`barca docs cache`](/reference/cache/), "At a glance"). This topic is the
full rule for the part of that hash that comes from code. In short:

- **The function and the project code it uses.** The function as written, and every module-level
  function, class, constant and import it uses, followed into helper modules of your project
  ("What counts as a use"). Editing a helper the step does not use re-runs nothing.
- **Some decorator arguments.** `inputs=`, `serializer=`, the shape of `partitions=` (not its
  keys), `@sink(...)` and any other decorator count; `retries=`, `timeout_seconds=`,
  `freshness=`, `name=`, `description=` and the rest do not ("Which decorator arguments count").
- **Not installed packages, and not code barca cannot see.** Followed only inside the project
  root; dynamic imports and the like are listed under "Not followed".

## What counts as a use

- **Functions and constants**: the definition's source, and what it uses in turn.
- **Classes** (`from helpers import Model`, or a class in the pipeline file): the whole class is
  hashed, its methods and class-level code, and what they use, including its base classes and
  metaclass. Editing `Model.predict` or `Model`'s base class re-runs; editing another class in
  the module does not.
- **Aliases**: `from helpers import clean as c` and `import helpers as h` are followed exactly
  like the unaliased forms.
- **Imports inside a function or class body** (`def rows(): from helpers import clean`),
  including in nested blocks and methods, with or without an alias: followed like a
  module-level import, at the same precision.
- **A module reached through another module's import**: `from wrapper import helpers` or
  `wrapper.helpers.compute()`, where `wrapper.py` does `import helpers` (or `import helpers as
  h`, or re-exports it with `from inner import helpers`), hashes what is read from `helpers.py`.
  Import chains are followed however many project modules deep they go, and modules that import
  each other are followed once each.
- **A module used as a value** (`getattr(helpers, name)()`, `run(helpers)`, `m = helpers`):
  which attribute is read cannot be known statically, so the **whole module** is hashed, and
  what its definitions use. This is deliberately conservative: any edit to that module re-runs
  the step. It applies only when the name really is the module: a parameter, local variable,
  loop or comprehension variable called `helpers` is not.
- **Names anywhere in an expression the function or a helper evaluates**: default argument
  values (positional-only and keyword-only parameters, nested functions and lambdas included;
  a default is resolved in the enclosing scope), exception handler types (tuples and `except*`
  included; a bare `except:` adds nothing), set, list and tuple elements, value, class and
  mapping-key expressions in `match` patterns (capture names are bindings, not reads),
  `for`/`while` `else:` blocks (followed even if a run leaves the loop with `break`),
  decorators of ordinary helper functions and classes with their arguments (resolved in the
  enclosing scope), and assignment receivers and indices inside functions (`table[key()] =
  value`, annotated, unpacked and chained targets, targets in `for`, `with` and comprehensions).
  Writing a bare name does not read its old value.

For functions, classes, constants and `module.attr`, barca does not work out local scopes: a
parameter called `rate` counts as a use of a module-level `rate`. That can only cause an extra
re-run, never a stale result.

## Which decorator arguments count

| Part of the decorators | Counts | Why |
|---|---|---|
| `inputs=` | yes | decides which upstream feeds which parameter |
| `serializer=` | yes | changes the bytes that are stored |
| `partitions=` | the shape, not the keys | the dimension names and a `partitions_from` source decide how the function is called; the keys do not count, because each key is part of its own run hash |
| `@sink(...)` | yes | a cached step writes a sink only when it holds another run hash's output ([`barca docs sinks`](/reference/sinks/)), so a new or edited sink has to run the asset to be written |
| any other decorator | yes | it wraps the function and can change what it returns |
| any other argument | yes | conservative fallback for unvalidated calls; recognized barca calls reject it |
| `env=` | no | the declared names and their values are already part of the run hash |
| `name=` | no | it is the node's id, under which results are looked up, not part of the result |
| `freshness=` | no | decides when the step runs, not what it returns |
| `retries=` | no | decides how often a failing step is tried, not what it returns |
| `retry_backoff=` | no | decides how long to wait between attempts, not what the step returns |
| `timeout_seconds=` | no | decides how long the step may take, not what it returns |
| `weight=` | no | decides how many steps run at once, not what the step returns |
| `deterministic=` | no | silences the warning about new bytes for the same code and inputs; caching is unchanged |
| `cutoff=` | no | decides what the asset's consumers fold into their run hashes (its output's bytes instead of its run hash), not what it returns |
| `batch=` | no | decides how many keys one call of the function computes, not what each key's result is; each key keeps its own run hash |
| `description=` | no | documentation |
| `tags=` | no | labels for people and tools |
| `@unsafe` | no | a marker barca reads and does not act on |

The same list applies to `@asset`, `@sensor` and `@task`; a test checks every `(decorator,
argument)` pair against it. "Any other decorator" is one that is not barca's:
`@functools.cache`, a wrapper of your own. "Any other argument" is a keyword barca does not
define, a positional argument, or `**options`, in a call barca cannot positively identify from
its imports; a call barca does identify rejects these at plan time ([`barca docs assets`](/reference/assets/),
"Accepted arguments").

What this means in practice:

```python
@asset(partitions={"region": partitions(["emea", "amer"])}, description="Sales by region")
def sales(region: str) -> dict:
    return {"region": region}
```

- Add `"apac"` to the list: only `sales[region=apac]` runs. Remove a key or reorder the list:
  nothing runs. This holds however the keys are written: a literal list, a module-level
  constant, a comprehension, a function call, or `partitions_from`
  ([`barca docs partitions`](/reference/partitions/)).
- Edit the description, add `tags=` or `retries=`, change `freshness=`: nothing runs.
- Change `serializer=`, add or edit a `@sink`, change which upstream an `inputs=` entry names,
  rename the partition dimension, or put another decorator on the function: the step runs
  again, and its downstream steps with it.
- Renaming a node with `name=` gives it a new id, and a result is looked up under its id, so the
  renamed step runs once. Its run hash is the same as before, so nothing downstream re-runs.

A counted argument is compared by what it says, not how it is typed. Whitespace, line breaks,
comments, single or double quotes, a trailing comma, the order of barca's own decorator keyword
arguments and the order of entries in `inputs={...}` do not change the hash. Keyword order in
any other call does: a wrapper decorator can observe it through `**kwargs`. Two things are
compared as written: the function itself, from `def` on, and an f-string or `lambda` inside a
counted argument.

Names in a counted argument are followed into the cone exactly like names in the function body:
with `@asset(serializer=FMT)` or `@retry(times=ATTEMPTS)`, editing `FMT` or `ATTEMPTS` re-runs
the step, and so does editing `retry` itself if it is defined in your project. Names in an
argument that does not count are not followed: with `partitions(REGIONS)` or
`description=SUMMARY`, editing `REGIONS` or `SUMMARY` re-runs nothing (a new key in `REGIONS`
runs that key). An upstream named in `inputs=`, `collect(...)` or `partitions_from(...)` is not
followed either, and does not need to be: the upstream's run hash, which covers its code, is
already part of the step's run hash.

**A decorator barca cannot prove is its own counts in full**, even if it is named `asset`,
`sensor`, `task`, `sink` or `unsafe`: its arguments, their order and the wrapper's code can
change the result. Such a decorator on a node (alone or stacked with a barca decorator) makes
the whole module's source and its static dependencies count, so an unrelated edit in that module
can re-run the node. The proof is conservative: a walrus assignment to a decorator name anywhere
in the module, or an explicit reference to `globals`, `locals`, `vars`, `exec`, `eval` or
`__builtins__`, makes every imported barca name uncertain. This costs an extra recompute, never
a stale result. These checks cover the explicit syntax, not arbitrary reflection or runtime side
effects.

Known limits:

- The keys of `partitions(<expression>)` are whatever the expression returns when the plan is
  made. Barca does not hash the expression or the helpers it calls, so if a helper starts
  returning the same keys with a different meaning, nothing re-runs.
- A step whose function reads a constant that the decorator also uses for its keys
  (`partitions(REGIONS)` and `REGIONS` in the body) re-runs every key when the constant is
  edited: the body's use puts the constant in the cone.
- A sink is written when its asset runs, and by a cache hit whose run hash differs from the one
  the sink last published ([`barca docs sinks`](/reference/sinks/), "Cache hits"). Removing a `@sink` also re-runs
  the asset once, although nothing new has to be written.

## Which file a module name means

A module name is looked up exactly where the worker's `import` looks: for a pipeline file inside
a package (every directory from the project root down to the file has an `__init__.py`), in the
project root only; for any other pipeline file, in the file's own directory and then in the
project root; and in the packages below those directories, with or without `__init__.py`.

| Pipeline file | `from helpers import f` means | A sibling module is |
|---|---|---|
| `p.py` in the root | `helpers.py` in the root | `from helpers import f` |
| `pipelines/p.py`, no `pipelines/__init__.py` | `pipelines/helpers.py` if it exists, else `helpers.py` in the root | `from helpers import f` |
| `pkg/p.py`, with `pkg/__init__.py` | `helpers.py` in the root, never `pkg/helpers.py` | `from .helpers import f` or `from pkg.helpers import f` |

So a pipeline in `pipelines/p.py` using `from shared.utils import f` follows `shared/utils.py` in
the root, and a module beside the pipeline file shadows a same-named one in the root, as it does
when Python imports it. As in Python, a package (`helpers/__init__.py`) wins over a module
(`helpers.py`) in the same directory. A pipeline file is never a package: `shared.sub` means
`sub.py` inside a `shared/` directory.

Each task imports with its own path; another pipeline's directory is never kept as a fallback.
Use the root-relative module name: `from b.shared import f` for `b/shared.py`, not `from shared
import f` in the hope that another pipeline made it importable. Conflicting project imports are
rejected before user code runs: if two pipeline directories each have `helpers.py`, qualify the
imports (`from east.helpers import value`, `from west.helpers import value`) so hashing and
workers pick the same files ([`barca docs discovery`](/reference/discovery/), "Node ids"). Literal imports in inactive
branches and unused functions may conservatively require qualification. Unambiguous sibling
helpers keep ordinary Python lookup.

Only files a step imports are read, each once per command. Barca never walks the project to
look for helpers, and it never reads or hashes the standard library or installed packages,
wherever the virtualenv is (`.venv/`, `venv/`, inside or outside the project).

## Followed conservatively

Where static analysis cannot tell which definition is meant, barca hashes more rather than less:
an edit to an unrelated definition in the same module can then re-run the step, but a stale
result is never served. This happens for a name that comes from:

- a module-level `if`, `try`, `for`, `while`, `with` or `match` block: barca does not evaluate
  the condition, so the step hashes the module's source and follows its static imports and
  references, inactive branches included;
- tuple, list or starred unpacking, or an augmented assignment (`x += ...`), at module level;
- a project star import (`from helpers import *`), in a helper module or in the pipeline file,
  including several and transitive star imports: the contributing modules' sources count.

This provenance is kept through later assignments, aliases, functions, classes and imports, so
even overwriting such a name later keeps the conservative hashing. Definitions that do not use
these names keep their selective cones. The exact rules are in `specs/behavior/cache-decisions.md`.

## Not followed

An edit in one of these does not change the hash; recompute with `--refresh-all` or `--refresh`
([`barca docs refresh`](/reference/refresh/), "Forcing a recompute"):

- installed packages and the standard library: after upgrading a package, recompute;
- modules outside the project root (`../shared/`), and modules only a custom `sys.path` or
  `PYTHONPATH` entry makes importable;
- imports built at run time: `importlib.import_module(name)`, `__import__(name)`, and
  `getattr` on anything but a module barca can see imported. Static analysis cannot follow
  these, and barca does not run your code to plan;
- names defined dynamically through runtime namespace mutation, and changes to the import path
  made by your code;
- data your function reads that is not an input: a file, a table, an API. Bring it in through a
  sensor ([`barca docs external-data`](/reference/external-data/), "External data that changes in place").

## A helper module that does not parse

A project module that does not parse stops the steps that cannot run without it, before
anything runs. If `helpers.py` has a syntax error, every step of a file that imports it
(directly, through another project module, or inside a function) is refused with exit 2 by the
command that plans it (`get`, `run`, `--dry-run`, `plan`, `status`), with the module, the line,
the column and the source line:

```
Parse error: syntax error in helpers.py: line 4, column 20: Expected `:`, found newline (imported by pipeline.py, so step 'pipeline.py:raw' cannot run)
helpers.py:4:20:
    def normalize(rows)
                       ^
Fix the syntax error: the worker imports helpers.py when it loads pipeline.py, so no step of pipeline.py can run until it parses.
```

A command that plans only steps of files that do not import the module is not affected, and
`barca list` plans nothing. An import barca cannot see (`importlib.import_module("helpers")`)
still fails in the step; its traceback ends with the file, the line and a caret at the error.

## Naming the pipeline file

The pipeline file can be named any way on the command line: `barca get rows pipeline.py`,
`./pipeline.py`, an absolute path, and `barca get rows project/pipeline.py` from the parent
directory all compute the same run hash and the same node id, relative to the project root
(`pipeline.py:rows`, or `project/pipeline.py:rows` when the root is the parent directory).

See also: [`barca docs cache`](/reference/cache/), [`barca docs assets`](/reference/assets/), [`barca docs discovery`](/reference/discovery/).

---

*The same text is in your terminal: `barca docs hashing`.*
