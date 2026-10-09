# Discovery: which files make up a project

A barca project is a directory tree. Every command reads the `.py` files in it that import
barca and builds one DAG from all their `@asset`, `@task` and `@sensor` functions. You do not
pass the file list: `barca list`, `barca run validate`, `barca get total` work from anywhere in
the project.

```bash
barca list                       # every node in the project
barca run validate               # the task `validate`, wherever it is defined
barca get total pipelines/       # only files under pipelines/
barca get total pipeline.py      # only this file (the pre-0.13 form, still valid)
```

## The project root

The root is the nearest directory at or above the current one that holds a `barca.toml`;
without one, the current directory is the root. Barca changes into the root before it does
anything, so `.barca/`, node ids and the working directory of your steps are the same wherever
you run it (`barca docs cache`, "Where things live"). Put an empty `barca.toml` at the top of a
project to anchor it.

## What a walk finds

With no file arguments, barca walks the whole root. A directory argument walks that directory.
A walk keeps a `.py` file only if a line starts with `from barca` or `import barca`; helper
modules, notebooks exported to `.py` and scratch scripts are never parsed as pipelines (they
are still read when a step imports them, for cache hashing).

A walk skips:

- directories whose name starts with `.` (`.venv`, `.git`, ...), and `__pycache__`, `venv`,
  `node_modules`, `site-packages`, `build`, `dist`, `tests`, `test`;
- files named `test_*.py`, `*_test.py`, `conftest.py`, `setup.py`.

One-shot commands report a file that imports barca and fails to parse as an error
(exit 2) naming that file. `barca serve` keeps healthy definitions loaded and excludes
invalid sources and graph-dependent definitions, including dependents in otherwise
valid files. Unrelated definitions in those files remain available. Startup stderr,
`GET /health`'s `load_errors` and the UI identify unloaded sources and affected node
IDs. `/state` remains an array of loaded node states. With `--watch`, repairing or
removing configured source refreshes these diagnostics and schedules; newly added
files still need a restart. Arbitrary Python import failures remain run errors:
static loading does not import modules to prove they execute successfully.
A walk finding no file importing barca remains an error (exit 2).

Files you name explicitly are always read, whatever their name or location, and need not import
barca.

## Shaping discovery: `[discovery]` in barca.toml

```toml
[discovery]
exclude = ["scratch/**", "notebooks/**"]   # skipped in addition to the built-in list
include = ["pipelines/**/*.py"]            # only these files (replaces the walk)
```

Patterns are relative to the root and use `/`. `*` and `?` match within one path segment, `**`
matches any number of segments (`scratch/**` is everything under `scratch/`). With `include`,
only matching files are discovered and the built-in skip list does not apply; `exclude` still
does. Unknown keys are an error (exit 2).

## Arguments: target, then files or directories

The first positional is a file argument if it ends in `.py`, ends in `/`, is `.` or `..`, or is
an existing directory written with a `/` in it (`pipelines/sub`). Otherwise it is a target name,
even when a directory has that name: `barca get reconciled` gets the asset `reconciled`;
`barca list reconciled/` lists the directory.

```bash
barca list .                     # the current directory and below
barca list pipelines/ jobs/      # two directories
barca run validate pipelines/    # target first, then the scope
```

## Node ids

A node id is `<file>:<function>` with `<file>` relative to the root (`pipelines/sources.py:ibp_model`),
however you named the file: `../sources.py` from a subdirectory or an absolute path give the
same id, and so share the cache. A bare name (`ibp_model`) selects the node when exactly one
file defines it; when two do, the error lists both full ids.

Two files may share a name in different directories (`east/assets.py`, `west/assets.py`): they
are separate modules, hashed and run separately. Helper modules they import with a bare
`import helpers` must have distinct names, because every pipeline directory goes on one
`sys.path` in a worker, as in any single Python process.

## Cross-file inputs

Import the upstream function the way Python would, and name it in `inputs=`. Barca reads the
import statically (it never runs it) and wires the input to that file's node:

```python
# file: pipelines/__init__.py
```

```python
# file: pipelines/sources.py
from barca import asset


@asset()
def ibp_model() -> dict:
    return {"rows": 3}
```

```python
# file: pipelines/reconcile.py
from barca import asset

from .sources import ibp_model


@asset(inputs={"m": ibp_model})
def reconciled(m: dict) -> dict:
    return {"rows": m["rows"]}
```

```python
# file: pipelines/validate.py
from barca import task

import pipelines.sources as sources
from pipelines.reconcile import reconciled


@task(inputs={"r": reconciled, "m": sources.ibp_model})
def validate(r: dict, m: dict) -> dict:
    assert r["rows"] == m["rows"]
    return {"status": "PASS"}
```

```bash
barca run validate               # from the root or any directory below it
```

Every import form works: `from pipelines.sources import ibp_model`, `from .sources import
ibp_model` (relative), `from sources import ibp_model` (a sibling file), `... import ibp_model as
model`, and `module.ibp_model` after `import pipelines.sources [as module]`; also inside
`collect(...)` and `partitions_from(...)`.

How a name in `inputs=` is resolved, most specific first:

1. a function defined in the same file;
2. for an imported name, the function in the file the import points at (a relative import from
   the file's package; an absolute one from the file's directory, then from the root);
3. otherwise the one function with that name in the project (this also covers a name re-exported
   through a package `__init__.py`).

When none of these picks exactly one node, barca stops with exit 2 instead of guessing: a bare
name defined in several other files lists every candidate id, and an imported name that is not
an `@asset`/`@task`/`@sensor` in its module says so (`'close' is imported from pipelines.common
(pipelines/common.py), but no @asset/@task/@sensor named 'close' is defined there`).

`asset_ref("<file>:<function>")` names a node without importing it (for example, to avoid an
import cycle). The path is relative to the root, or to the referencing file's directory.

A step file inside a package (every directory from the root down has an `__init__.py`) runs as
that package's module (`pipelines.reconcile`), so relative imports work in it and `from
pipelines.reconcile import x` elsewhere gets the same module. Other files run as standalone
modules, as before.

## Known limitations

- `barca serve --watch` re-reads the files it found at start; a file added later needs a restart.
- Ids changed in 0.13: before, an id kept the spelling you typed (`./p.py:f`, an absolute path).
  Cache entries recorded under another spelling are not found once; the first run recomputes them.
