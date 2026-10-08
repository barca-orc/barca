# Caching, artifacts and environments

## What is cached

Each asset step has a **run hash**: a hash of the function's definition, its inputs' run hashes,
the output of any sensor it reads, for partitioned assets the partition key, and the values of
any environment variables it declares with `env=[...]`. If the hash matches a previous successful
materialization, the artifact is reused and the function does not run. Change the function's
code, any upstream, a sensor's output or a declared environment variable and the hash changes, so
only the affected subgraph re-runs.

### What the definition covers

The definition part of the hash is the function's source, its decorator arguments, and its
**dependency cone**: every module-level function, class, constant and import the function uses,
followed transitively. That includes **helper modules in your project**. Both import styles are
followed, at the same precision:

```python
# helpers.py
def clean(rows):
    return [r for r in rows if r]


def unused():
    return "editing this re-runs nothing"
```

```python
# pipeline.py
import helpers
from barca import asset


@asset()
def rows() -> list:
    return helpers.clean([1, 0, 2])
```

Editing `clean` (or anything `clean` calls) changes `rows`' hash and it re-runs; editing `unused`
does not. `from helpers import clean` + `clean(...)`, `import pkg.mod as m` + `m.f()`,
`import pkg.mod` + `pkg.mod.f()` and `from pkg import mod` + `mod.f()` hash exactly the same
way: only the definitions the step uses, never the whole module.

The pipeline file can be named any way on the command line: `barca get rows pipeline.py`,
`./pipeline.py`, an absolute path, and `barca get rows project/pipeline.py` from the parent
directory all compute the same run hash and the same node id, relative to the project root
(`pipeline.py:rows`, or `project/pipeline.py:rows` when the root is the parent directory).

#### What counts as a use

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
- **A module used as a value** (`getattr(helpers, name)()`, `run(helpers)`, `m = helpers`):
  which attribute is read cannot be known statically, so the **whole module** is hashed, and
  what its definitions use. This is deliberately conservative: any edit to that module re-runs
  the step. It applies only when the name really is the module: a parameter, local variable,
  loop or comprehension variable called `helpers` is not.

For functions, classes, constants and `module.attr`, barca does not work out local scopes: a
parameter called `rate` counts as a use of a module-level `rate`. That can only cause an extra
re-run, never a stale result.

#### Which file a module name means

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
when Python imports it. The project root is the boundary (see "Where things live"): a module in
`../shared/` outside it, or in a directory you add to `sys.path` or `PYTHONPATH` yourself, is not
followed. As in Python, a package (`helpers/__init__.py`) wins over a module (`helpers.py`) in
the same directory.

A pipeline file can also be imported by its file name from a pipeline file in another directory
(`a/p.py` does `from shared import f`, and `b/shared.py` is a pipeline file), because a worker
keeps the directory of every pipeline file it has loaded on its import path. Barca follows it:
`b/shared.py` is hashed when nothing on `a/p.py`'s own import path is a module called `shared`,
and **as well as** that module when something is, since which of the two a worker runs depends
on what it has loaded before. A pipeline file is never a package: `shared.sub` always means
`sub.py` inside a `shared/` directory. Avoid the ambiguity by giving helper modules and pipeline
files distinct names (`barca docs discovery`, "Node ids").

Only files a step imports are read, each once per command. Barca never walks the project to
look for helpers, and it never reads or hashes the standard library or installed packages,
wherever the virtualenv is (`.venv/`, `venv/`, inside or outside the project): after upgrading
a package, recompute with `--refresh-all` or `--refresh` (below).

#### Not followed

An edit in one of these does not change the hash; recompute with `--refresh-all` or `--refresh`:

- modules outside the project root, and modules only a custom `sys.path` or `PYTHONPATH` entry
  makes importable;
- imports built at run time: `importlib.import_module(name)`, `__import__(name)`, and
  `getattr` on anything but a module barca can see imported. Static analysis cannot follow
  these, and barca does not run your code to plan;
- `from helpers import *`, and names a module defines anywhere but at its top level (inside an
  `if` or `try`, or assigned dynamically);
- module-level constants bound by tuple unpacking (`A, B = 1, 2`), and augmented assignments to
  a module-level name (`A += 1`): only a plain `A = ...` or `A: int = ...` is a definition;
- a module reached through another module's `import` (`from helpers import other` where
  `helpers.py` does `import other`): import it directly;
- what a helper refers to only in a decorator, a default argument value, an `except` clause's
  exception type, a `match` pattern, a set literal (`{f()}`), an assignment target
  (`table[key()] = ...`) or a loop's `else:` block. The helper's own text is hashed, but those
  references are not followed;
- helpers more than six project modules away along an import chain.

Two pipeline directories that each have a `helpers.py` are hashed correctly, each against its
own, but share one `sys.path` in a worker (`barca docs discovery`, "Node ids"): give such
helpers distinct names.

#### After upgrading to 0.18

Barca 0.18 started following classes, imports inside function bodies, aliased `from` imports,
modules used as values, and root modules imported from a subdirectory. A step that uses one of
these has a new hash, so it **recomputes once** on the first run after upgrading; its downstream
steps recompute with it. Specifically, a step recomputes once if its function, or a helper it
reaches, uses:

- a class defined in the project (in the pipeline file or imported);
- a project module imported inside a function or class body;
- `from module import name as alias`, where `module` is a project module;
- a project module as a value (`getattr(helpers, name)`, passing `helpers` along);
- from a pipeline in a subdirectory, a module that lives in the project root;
- a helper module with the same name as a pipeline file in another directory (both are hashed
  now);
- or if its pipeline file is inside a package (`pkg/__init__.py` next to it) and imports
  project modules (`from .helpers import f`, `from pkg.helpers import f`): these now resolve
  from the root, as they do when the step runs.

One more case can recompute once: a step whose helpers import the **same name from two
different modules outside the project** (one helper does `from numpy import array`, another
`from jax.numpy import array`). Before 0.18 such a step had two possible hashes, picked at random
on every run, so it missed its cache about half the time; it now has one, and recomputes once
if its last result was recorded under the other.

Every other step keeps its hash and its cached results.

Barca runs exactly the source it hashed. It never runs stale bytecode for your pipeline files or
the project modules they import, from the file's directory tree or from the project root: their
`__pycache__` .pyc files are checked against a hash of the source, not its mtime and size, so an
edit that keeps both (a same-size edit within one second, or a tool that pins mtimes such as
Nix, Bazel, `touch -t` or `rsync -t`) still runs the new code. Installed packages import as
usual.

Environment variables a function reads **without** declaring them are not part of the hash:
changing one does not invalidate anything. Declare them with `@asset(env=["NAME"])`
(`barca docs assets`). Nodes that declare no env hash exactly as they did before `env=` existed,
so upgrading does not invalidate existing caches.

Tasks and sensors are never served from cache. Partitioned assets are cached per key (see
`barca docs partitions`).

## External data that changes in place

A blob overwritten at the same path, a table updated in place, a file someone re-exports: the
asset that reads it has the same code and the same inputs, so its run hash does not change and it
is served from cache with the old data. Put a `@sensor` in front of it that returns something
identifying the current version of the data (an etag, a last-modified time, a row count), and
make the asset read the sensor:

```python
import hashlib
from pathlib import Path
from barca import asset, sensor


@sensor()
def orders_etag() -> tuple[bool, str]:
    # Stands in for a blob's etag. With azure-storage-blob, for example:
    #   BlobClient.from_blob_url(url, credential).get_blob_properties().etag
    return True, hashlib.md5(Path("orders.csv").read_bytes()).hexdigest()


@asset(inputs={"etag": orders_etag})
def bronze(etag: str) -> list:
    return Path("orders.csv").read_text().splitlines()


@asset(inputs={"rows": bronze})
def silver(rows: list) -> int:
    return len(rows)
```

```bash
barca get silver pipeline.py     # the sensor runs; bronze and silver run
barca get silver pipeline.py     # the sensor runs, returns the same etag: bronze and silver cached
barca get silver pipeline.py     # after orders.csv changes: a new etag, bronze and silver run
```

How it works:

- A sensor always runs. Its output is serialized like any other (`barca docs types`), and the
  SHA-256 of those bytes is folded into the run hash of every asset that reads the sensor
  directly. Everything downstream of those assets changes with them, through their run hashes.
  Assets that do not depend on the sensor are not affected.
- The same output gives the same run hash, so the consumer is served from cache. Going back to
  an earlier output (an etag that was current before) serves the materialization made then.
- Sensors run in a phase of their own, before their consumers, so a consumer's cache decision
  always uses the value the sensor returned in this run.
- The `bool` in the sensor's `(update_detected, value)` return is not used for caching; only
  `value` is hashed.
- A partitioned asset that reads a sensor re-runs every key when the sensor's output changes.
- `--refresh` (with its cascade), `--no-cascade` and `--refresh-all` work as before, on `get`
  and `run`.

**The trap: return only what identifies the data.** Every sensor's output is hashed, so a sensor
whose value changes on every run (a timestamp, `datetime.now()`, a request id, a dict that
includes the time it was fetched) makes every asset that reads it re-run every time:

```python
@sensor()
def orders_etag() -> tuple[bool, dict]:
    props = get_blob_properties()
    return True, {"etag": props.etag, "checked_at": time.time()}   # re-runs bronze every time
```

Return `props.etag` alone. That is the intended meaning: an asset that reads a sensor depends on
what the sensor returns.

Upgrading: previously a sensor's output was not part of its consumers' run hashes, and an asset
reading a sensor was served from cache whatever the sensor returned. Assets that read a sensor
re-run once after upgrading (their run hash now includes the sensor's output). Pipelines without
sensors keep exactly the same run hashes and caches.

`--dry-run` and `barca status` execute nothing, so they cannot know what a sensor will return.
They predict with the sensor's **last recorded output**, and the step's `detail` says so:
`assumes sensor 'orders_etag' returns the same value as its last run`. If the sensor has no
recorded output (it never ran, or last ran before this version), its consumers and everything
downstream of them are `unknown` with `reason: "sensor_output_unknown"`. Running the sensor alone
(`barca get orders_etag pipeline.py`, or a scheduled sensor under `barca serve`) records its
output, so the next `--dry-run` or `barca status` shows its consumers as stale.

## Where things live

```
.barca/metadata.db                          run history and materialization records (local DB)
.barca/metadata.db.base                     with shared history: a counter of pulls and uploads (`barca docs remote`)
.barca/metadata.db.prev                     with shared history: the local DB as it was before the last pull that changed it
.barca/metadata.db.pull-*, .push-*          with shared history: a download or upload in progress; what a killed command left is removed by the next pull
.barca/artifacts/<node>/<run_hash>.<ext>    one file per result
```

`<ext>` is `.json`, `.pkl` or `.parquet` (see `barca docs types`). An artifact's path names the
computation, not the bytes: the run hash covers the step's code and inputs, so the same step
with the same inputs always writes the same path. A step is meant to be a pure function of its
code and inputs (what changes outside comes in through a sensor, whose output is part of the
run hash), so computing it again is expected to write the same bytes.
Barca does not enforce that. Computing the result again (`--refresh`, `--refresh-all`, or a
missing artifact that something needs) overwrites the file, and a function that is not
deterministic then leaves different bytes at the same path; with an artifact store, a machine
whose history still has the earlier hash is warned (`barca docs remote`, "Checking a local copy
against the store"). Because the path is the same on every machine, artifacts can be shared:
set `BARCA_REMOTE_URI` and your cloud's credentials (`barca docs remote`).

`.barca/` lives in the **project root**: the nearest directory at or above the one you run barca
from that holds a `barca.toml`. Without a `barca.toml` above you, the current directory is the
root. Barca changes into the root before doing anything, so:

- running from a subdirectory reads and writes the same cache as running from the root
  (`cd sub && barca get out ../pipeline.py` is a cache hit after `barca get out pipeline.py`);
- file arguments are read relative to where you typed them, and node ids are relative to the
  root, so `pipelines/p.py:out` is the same id from any directory;
- steps run with the root as their working directory, so `Path("data.txt")` inside a step means
  the root's `data.txt` wherever you invoke barca;
- stderr says `barca: project root: <path>` whenever the root is not the current directory.

Put an empty `barca.toml` at the top of a project to anchor it.

Never delete files under `.barca/` to force a recompute: use `--refresh <asset>` (below), which
also keeps the metadata DB consistent. To look at a cached result, use `barca sql` (`barca docs sql`).

## A cached result whose artifact is missing

A cached result is a row in `.barca/metadata.db`; its artifact is a file. The file can go
missing while the row stays: a disk cleanup, a container restarted without a volume for
`.barca/artifacts/`, or someone deleting the directory. Barca then computes the result again
when, and only when, something needs to read it:

```bash
barca run publish pipeline.py                    # model ran, publish ran
rm .barca/artifacts/pipeline.py--model/*.json
barca run publish pipeline.py --dry-run --json   # model: "action": "run", "reason": "artifact_missing"
barca run publish pipeline.py                    # model runs again, then publish; exit 0
barca run publish pipeline.py                    # model is cached again
```

- **Needed** means one of: a step that is going to run takes the artifact as an input; a
  `partitions_from` step is expanded from it; or it is the output the command returns: the
  targets you named, or with no target the one asset whose value is `final_output` (the last
  asset). A partitioned asset is returned as a whole, so every one of its partitions is
  checked (one file lookup per key, a few milliseconds at 5,000 keys). Every other asset at the
  end of a pipeline is treated like an intermediate and is not looked at.
- **Missing** means not on this machine's disk and, with an artifact store, not in the store
  either (`barca docs remote`). The store has to be there for that to count: if its bucket,
  container or directory is gone, misnamed or unreachable, the run fails with exit 3 and
  computes nothing, because then nothing is known about the artifact.
- The step runs with `reason: "artifact_missing"` and a warning on stderr names the file:
  `[barca] warning: pipeline.py:model: the artifact of its cached result is missing: <path>.
  Computing it again.` Its run hash does not change, so the artifact lands at the same path and
  nothing downstream is invalidated. If the recomputed step reads an input that is missing too,
  that one is computed first, by the same rule. For a partitioned asset only the keys whose
  artifact is missing run.
- **An artifact nothing reads is not looked at.** With `model` deleted and `report` (which
  reads it) cached, `barca get report pipeline.py` executes 0 steps and `model` stays `cached`:
  you can prune large intermediates and keep the final outputs. `model` is computed again the
  first time something needs it.
- `--dry-run` and `barca status` predict the same thing: `model` is `run` / `stale` with reason
  `artifact_missing` under `barca run publish`, and `cached` under `barca get report`.
- Under `barca serve` a scheduled task whose input was deleted recomputes the input once, on its
  next tick, and keeps succeeding.

### A directory at an artifact's path

An artifact is one file. If a **directory** sits at an artifact's path (made by hand, by a tool
that unpacked something there, or by a mistaken `mkdir -p`), it is not an artifact, and barca
treats the result exactly as if the file were missing: it is fetched from the artifact store, or
computed again with `reason: "artifact_missing"`, when something needs to read it, and it is not
looked at otherwise. The same holds for a symlink there that leads to a directory or to nothing.

Barca then has to put a file where the directory is. It never deletes what it finds:

- an **empty** directory is removed;
- a directory **with anything in it** is renamed, contents and all, to
  `<run_hash>.<ext>.moved-aside` beside it (`.moved-aside-2`, `-3`, ... if that name is taken),
  and stderr says so:
  `[barca] warning: <path> is a directory, not an artifact. Moved it, with its contents, to
  <path>.moved-aside; barca does not use it, delete it if you do not need it.`
- a **symlink** is replaced by the artifact file. Only the link goes; what it pointed to is not
  touched.

The run goes on and exits 0. Only a directory is ever moved: an artifact file that another
barca process wrote in the same instant is never renamed.

**Where they are, and getting rid of them.** A moved-aside directory stays beside the artifact,
in `.barca/artifacts/<node>/` (under `.barca/envs/<env>/artifacts/` for a named environment).
Barca never reads one again, never lists them in `barca status` or any other command, and never
deletes one: they are yours, and they take disk space until you remove them. To see them all:

```
find .barca -name '*.moved-aside*'
```

Delete the ones you do not need. Deleting the whole of `.barca/artifacts/` to reclaim disk
removes them with everything else (results are computed again, or downloaded again from an
artifact store, when something needs them).

**If barca may not move it.** Renaming needs write permission on the directory that holds the
artifact. Without it the run exits 3 (an infrastructure error, not a failed step) and names the
path and the permission; nothing is deleted. Grant the permission, or move or remove the
directory yourself, and run the command again.

This applies only inside barca's own artifact directory (`.barca/artifacts/`). Barca moves,
renames and replaces nothing anywhere else:

- a `@sink` path is yours. Barca writes the file there and changes nothing else: a directory at
  the path fails that sink (`[barca] SINK FAILED: ... IsADirectoryError`) and is left as it is,
  and a symlink is written through (the file it points to is written, the link stays). The
  asset itself succeeds either way (`barca docs sinks`);
- a directory at an object's path in an artifact store that is a shared directory fails the
  fetch or the upload with exit 3, naming the path, and is left as it is. The error says to
  remove or rename it there; `--refresh-all` does not help (`barca docs remote`).

Known limits:

- Only whether the artifact is a file is checked. A file that is there but truncated or edited
  is read as it is (with an artifact store, a copy that does not match its recorded hash is
  replaced: `barca docs remote`). A symlink to a file counts as that file and is read; writing
  the artifact again replaces the link.
- `--agent` announces each step once, with its outcome. A cached step whose artifact is not on
  disk waits: it prints `step:<id> completed` if it is computed again, or `step:<id> cached` at
  the end of the run if nothing needed it. The one exception is a result in a remote store
  whose object turns out to be deleted when it is fetched: its `cached` line was already
  printed, and the warning and a `completed` line for the same step follow
  (`barca docs contract`).
- A function that is not deterministic may return a different value when it is computed again;
  cached steps downstream of it keep the results they were computed with (as after
  `--refresh --no-cascade`).
- A dry run does not contact a remote artifact store. A result recorded in one is predicted as
  `cached` even if the object has since been deleted from the bucket; the real run finds out
  when it fetches it, and computes it again. A store that is a directory is checked, and if
  the directory itself is gone the dry run predicts `cached` where the run exits 3.

## Controlling the cache

| Goal | Command |
|---|---|
| Normal, cache-aware | `barca get target pipeline.py` |
| Recompute everything in the cone | `barca get target pipeline.py --refresh-all` |
| Recompute chosen assets and everything downstream of them | `barca get target pipeline.py --refresh a,b` |
| Recompute only the chosen assets | `barca get target pipeline.py --refresh a,b --no-cascade` |
| Run a task, cached upstream | `barca run task pipeline.py` |
| Run a task, refresh chosen upstream assets and everything downstream of them | `barca run task pipeline.py --refresh a,b` |
| Run a task, refresh only the chosen assets | `barca run task pipeline.py --refresh a,b --no-cascade` |
| Run a task, refresh all upstream assets | `barca run task pipeline.py --refresh-all` |

`barca run` previously refreshed every upstream asset by default and called the selective flag
`--burst`. The default is now cache-aware and the flag is `--refresh`.

Previously `--refresh` did not cascade: it re-ran only the named assets and left their downstream
assets cached. It now cascades by default; `--no-cascade` keeps the old behavior.

`barca get` takes the same three flags as `barca run` (it used to have only `--no-cache`). On
`get` the target is an asset, so `--refresh` may name it too. `--no-cache` still works on both
commands as a deprecated spelling of `--refresh-all`: it prints
`[barca] warning: --no-cache is deprecated ...` and will be removed in a future minor release.

### Exactly what `--refresh` does

- `--refresh a,b` re-materializes the assets you name **and every asset downstream of them** in
  the target's cone (the cascade), so the refreshed data reaches the target. A step re-run by the
  cascade reports `reason: "refresh_cascade"` and a `detail` naming the asset it cascaded from.
- It takes one comma-separated list; `--refresh a b` is an error ("'b' is not a .py file"). A
  name that is not an asset in the target's cone is an error that lists the valid names, so a
  typo never silently does nothing.
- It does **not** rebuild the upstream of what you name, or assets in the cone that do not depend
  on it. Those keep serving from cache.
- `--no-cascade` re-materializes **only** the assets you name. Run hashes cover definitions and
  upstream hashes, not output contents, so a cached downstream asset still matches and the
  refreshed data never reaches it. Barca prints
  `warning: 'mid' was served from cache but depends on refreshed 'src' ...` when this happens.
  `--no-cascade` without `--refresh` is a usage error (exit 2).

For external data that changes in place (a blob overwritten at the same path), the canonical
answer is a sensor (see "External data that changes in place" above). Without one, `--refresh`
the asset that reads it, and everything built from it re-runs.

## Seeing what will happen: `--dry-run`

`barca get` and `barca run` take `--dry-run`. It reports, for exactly that command and flags, which
steps would be served from cache and which would run, and why. It executes nothing and writes
nothing: no `.barca` directory is created and no run is recorded.

```bash
barca run report pipeline.py --dry-run --json          # JSON on one line
barca run report pipeline.py --dry-run --pretty        # a table for humans
barca run report pipeline.py --dry-run --refresh src   # preview a refresh and its cascade
barca get total pipeline.py --dry-run --refresh-all
```

```json
{"dry_run": true, "command": "run", "target": "report",
 "steps": [{"id": "pipeline.py:src", "kind": "asset", "action": "cached",
            "run_hash": "…", "artifact": ".barca/artifacts/…"},
           {"id": "pipeline.py:report", "kind": "task", "action": "run",
            "reason": "task", "detail": "tasks always re-run"}],
 "summary": {"will_run": 1, "cached": 1, "unknown": 0}}
```

Each step has an `action`:

| `action` | Meaning |
|---|---|
| `cached` | Served from the cache (`artifact` is the file). |
| `run` | Will execute; `reason` says why (below). |
| `partial` | A partitioned asset where some keys are cached; `partitions` lists the counts and the keys that will run. |
| `unknown` | Cannot be known without running: a dynamic partition (`partitions_from`) whose source has to run first to produce its keys (`reason: "partitions_unknown"`), or an asset reading a sensor with no recorded output (`reason: "sensor_output_unknown"`), and anything that depends on either. |

`reason` is one of `task` and `sensor` (always run), `refresh` (named in `--refresh`),
`refresh_cascade` (downstream of an asset named in `--refresh`), `refresh_all` (`--refresh-all`),
`not_materialized` (no cached result for this code and these inputs: never run, or the code, an
upstream or a sensor's output changed), or `artifact_missing` (the result is cached, but its
artifact is gone and something needs to read it: see "A cached result whose artifact is missing"
above). Under `--no-cascade`, a cached step downstream of a
refreshed asset carries a `warning` (see the refresh notes above). A step that reads a sensor is
predicted from the sensor's last recorded output, and its `detail` says so. `summary` counts
steps, one per partition key.

A dry run makes the same decisions a real run makes (it calls the same code), and the test suite
checks that `will_run` equals the real run's `steps_executed` across cold, warm, `--refresh`,
`--refresh --no-cascade` and `--refresh-all` runs.

## What a run reports

A real `barca get` / `barca run` returns the same per-step information in a `steps` array, with a
`status` of `ran`, `cached` or `partial` (and the same `reason` / `warning`). In `--agent` mode a
cached step also prints `[barca] step:<id> cached` on stderr, so a log shows what was served from
cache as well as what ran. A step whose node declares `env=[...]` also carries `env`, the values
it was hashed with (`null` when unset, `<redacted>` for secret-looking names), in both the JSON
`steps` entry and the `--agent` line (`... env SOURCE_CSV=b.csv`). With an artifact store, a step
whose stored result (or an input it read) is not the copy that was recorded carries
`artifact_mismatch: true` and a `warning` (`barca docs remote`). `barca history --json` and
`barca stats` show runs and timings over time.

## Concurrent runs

Several barca processes can run in one project at once (parallel scripts or agents, `barca serve`
alongside the CLI). The metadata DB is a single-file database that one process opens at a time,
so each process holds a short lock on it (`.barca/metadata.db.lock`) only while it reads or
writes, and releases it while your Python runs. Processes queue instead of failing. If a
process waits more than 60 seconds for the lock you get an error that names the lock file;
an `... File is locked by another process` error means something outside barca (a DB browser, a
backup tool, an older barca) has `.barca/metadata.db` open.

## While a run is going, and after one is killed

A run records each step in `.barca/metadata.db` as the step finishes, not only when the run
ends. Finished steps are written in batches, at most twice a second, so a step is recorded
within about half a second of finishing.

```bash
barca status pipeline.py        # from another terminal: steps the running get has finished are `cached`
barca history --json            # the run is `running`; `steps_executed` is the steps recorded so far
```

- **Progress.** `barca status` in a second terminal shows what a running `barca get` or
  `barca run` has finished: an asset is `cached`, a partitioned asset is `partial` with its
  `cached` / `missing` counts. A step that is still running shows its previous state.
- **A killed run keeps what it finished.** If the process is killed (`kill -9`, out of memory, a
  lost machine), the next `barca get` serves the recorded steps from cache and computes the rest.
  A step is recorded only after its artifact is completely written, so a recorded step always has
  its file. A step that finished in the last half second before the kill may not be recorded: it
  runs again.
- **History says so.** `barca history` reports a run whose process no longer exists as
  `interrupted`, with `finished_at` and `elapsed_seconds` `null` (nobody saw it end) and
  `steps_executed` at what it had recorded. Ctrl-C is different: the run stops its workers,
  records itself and is `cancelled` (exit 130). With an artifact store that also holds while
  artifacts upload, download or the shared history is pushed, and the cancelled run then shares
  its record for at most 10 seconds; a second Ctrl-C ends that (`barca docs remote`, "Ctrl-C").

Known limits:

- With shared remote state (`barca docs remote`) all of this holds on the machine the run is on:
  the pull at the start of a command keeps what a run recorded locally, so progress and resume
  work whatever other machines upload in the meantime. Other machines see a run when it ends,
  and a killed run once a later run on its machine has ended.
- With a remote artifact store, steps are not recorded as they finish: a row is written only
  once the artifact's upload is confirmed, which happens when the run ends. Such a run shows no
  progress in `barca status`, and a killed one records nothing.
- `interrupted` is decided by looking for the run's process on this machine. A run started by an
  older barca, or on another machine, stays `running`; so does a run whose process id has since
  been reused by another program.
- Failed steps are recorded when the run ends, so a killed run records its successes only.

## Environments

`--env <name>` (or `BARCA_ENV`, or `default_env` in `barca.toml`, else `default`) fully
separates cache, artifacts and shared state. Use it for dev/staging/prod isolation.

## Seeing what happened

```bash
barca history --json            # recent runs: status, steps executed, steps cached
barca stats total pipeline.py --json   # timing percentiles and cache hit rate for one asset
barca plan pipeline.py          # what would run, in phases, without running it
```

`steps_executed` in `barca get`'s JSON output is the number of steps that actually ran; a fully
cached second run reports 0 for assets.
