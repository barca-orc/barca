# Caching, artifacts and environments

Barca skips a step when it has already computed the same thing. This topic covers what "the
same" means (the run hash), what the hash does and does not see, the states a cached result
can be in, how to force a recompute, the cache size limit, and where everything is stored.

## What is cached

Each asset step has a **run hash**: a hash of the function's definition, its inputs' run hashes,
the output of any sensor or `Manual` asset it reads, for partitioned assets the partition key,
and the values of any environment variables it declares with `env=[...]`. If the hash matches a
previous successful materialization, the artifact is reused and the function does not run.
Change the function's code, any upstream, a sensor's or `Manual` asset's output or a declared
environment variable and the hash changes, so only the affected subgraph re-runs.

- Tasks and sensors are never served from cache. Partitioned assets are cached per key (see
  `barca docs partitions`).
- Each successful materialization also records a short digest of each part of its hash (the
  function, each decorator argument that counts, each helper, each input, each declared env
  var), so `barca status` and `--dry-run` can say which part changed since then
  (`changed: helper pipelines/common.py:bucket_ltv`; `barca docs status`). The digests explain
  a miss; they never decide one.
- Environment variables a function reads **without** declaring them are not part of the hash:
  changing one does not invalidate anything. Declare them with `@asset(env=["NAME"])`
  (`barca docs assets`). Nodes that declare no env hash exactly as if `env=` did not exist.
- Barca runs exactly the source it hashed. It never runs stale bytecode for your pipeline files
  or the project modules they import: their `__pycache__` .pyc files are checked against a hash
  of the source, not its mtime and size, so an edit that keeps both (a same-size edit within one
  second, or a tool that pins mtimes such as Nix, Bazel, `touch -t` or `rsync -t`) still runs
  the new code. Installed packages import as usual.

How the coordinator assembles a step's hash from its upstreams, and the guarantees that come
with it, are specified in `specs/cache-decisions.md`.

### What the definition covers

The definition part of the hash covers what determines the step's result:

- the function, from `def` to its last line, as written (comments and formatting included);
- the decorator arguments that affect the result or what is stored, listed under "Which
  decorator arguments count" below. Arguments that do not, such as `description=`, `tags=`,
  `retries=` and the keys of a `partitions([...])` list, are not part of it: editing them
  re-runs nothing;
- its **dependency cone**: every module-level function, class, constant and import the function
  uses, followed transitively.

The cone includes **helper modules in your project**. Both import styles are followed, at the
same precision:

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

#### Which decorator arguments count

| Part of the decorators | Counts | Why |
|---|---|---|
| `inputs=` | yes | decides which upstream feeds which parameter |
| `serializer=` | yes | changes the bytes that are stored |
| `partitions=` | the shape, not the keys | the dimension names and a `partitions_from` source decide how the function is called; the keys do not count, because each key is part of its own run hash |
| `@sink(...)` | yes | a cached step does not write its sinks, so a new or edited sink has to run the asset to be written |
| any other decorator | yes | it wraps the function and can change what it returns |
| any other argument | yes | conservative fallback for unvalidated calls; recognized barca calls reject it |
| `env=` | no | the declared names and their values are already part of the run hash |
| `name=` | no | it is the node's id, under which results are looked up, not part of the result |
| `freshness=` | no | decides when the step runs, not what it returns |
| `retries=` | no | decides how often a failing step is tried, not what it returns |
| `retry_backoff=` | no | decides how long to wait between attempts, not what the step returns |
| `timeout_seconds=` | no | decides how long the step may take, not what it returns |
| `weight=` | no | decides how many steps run at once, not what the step returns |
| `description=` | no | documentation |
| `tags=` | no | labels for people and tools |
| `@unsafe` | no | a marker barca reads and does not act on |

The same list applies to `@asset`, `@sensor` and `@task`; a test checks every `(decorator,
argument)` pair against it. "Any other decorator" is one that is not barca's:
`@functools.cache`, a wrapper of your own. "Any other argument" is a keyword barca does not
define, a positional argument, or `**options`, in a call barca cannot positively identify from
its imports; a call barca does identify rejects these at plan time (`barca docs assets`,
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
  (`barca docs partitions`).
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
- A sink is written only when its asset runs. Removing a `@sink` also re-runs the asset once,
  although nothing new has to be written.

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
when Python imports it. As in Python, a package (`helpers/__init__.py`) wins over a module
(`helpers.py`) in the same directory. A pipeline file is never a package: `shared.sub` means
`sub.py` inside a `shared/` directory.

Each task imports with its own path; another pipeline's directory is never kept as a fallback.
Use the root-relative module name: `from b.shared import f` for `b/shared.py`, not `from shared
import f` in the hope that another pipeline made it importable. Conflicting project imports are
rejected before user code runs: if two pipeline directories each have `helpers.py`, qualify the
imports (`from east.helpers import value`, `from west.helpers import value`) so hashing and
workers pick the same files (`barca docs discovery`, "Node ids"). Literal imports in inactive
branches and unused functions may conservatively require qualification. Unambiguous sibling
helpers keep ordinary Python lookup.

Only files a step imports are read, each once per command. Barca never walks the project to
look for helpers, and it never reads or hashes the standard library or installed packages,
wherever the virtualenv is (`.venv/`, `venv/`, inside or outside the project).

#### Followed conservatively

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
these names keep their selective cones. The exact rules are in `specs/cache-decisions.md`.

#### Not followed

An edit in one of these does not change the hash; recompute with `--refresh-all` or `--refresh`
("Forcing a recompute"):

- installed packages and the standard library: after upgrading a package, recompute;
- modules outside the project root (`../shared/`), and modules only a custom `sys.path` or
  `PYTHONPATH` entry makes importable;
- imports built at run time: `importlib.import_module(name)`, `__import__(name)`, and
  `getattr` on anything but a module barca can see imported. Static analysis cannot follow
  these, and barca does not run your code to plan;
- names defined dynamically through runtime namespace mutation, and changes to the import path
  made by your code;
- data your function reads that is not an input: a file, a table, an API. Bring it in through a
  sensor (next section).

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
- The sensor's own code counts too. A sensor's run hash covers its function and the helpers it
  reaches, and is part of its consumers' run hashes next to the hash of its output. Editing
  the sensor's function re-runs the assets that read it, even when it returns the same value.
  Editing its `freshness=`, `description=` or another decorator argument that does not count
  ("Which decorator arguments count" above) re-runs nothing.
- The same code and the same output give the same run hash, so the consumer is served from
  cache. Going back to an earlier output (an etag that was current before) serves the
  materialization made then.
- Sensors run in a phase of their own, before their consumers, so a consumer's cache decision
  always uses the value the sensor returned in this run.
- The `bool` in the sensor's `(update_detected, value)` return is not used for caching; only
  `value` is hashed.
- A partitioned asset that reads a sensor re-runs every key when the sensor's output changes.
- `--refresh` (with its cascade), `--no-cascade` and `--refresh-all` work on assets that read a
  sensor as on any other, on `get` and `run`.

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

`--dry-run` and `barca status` execute nothing, so they cannot know what a sensor will return.
They predict with the sensor's **last recorded output**, and the step's `detail` says so:
`assumes sensor 'orders_etag' returns the same value as its last run`. If the sensor has no
recorded output (it never ran), its consumers and everything downstream of them are `unknown`
with `reason: "sensor_output_unknown"`. Running the sensor alone (`barca get orders_etag
pipeline.py`, or a scheduled sensor under `barca serve`) records its output, so the next
`--dry-run` or `barca status` shows its consumers as stale.

## Cached, stale, evicted, unsynced

A result is a row in `.barca/metadata.db` (the run hash it was computed under) plus an artifact
file. `barca status` (`barca docs status`) names the state each node is in:

| State | Meaning | What the next `get` does |
|---|---|---|
| `cached` | a result for the current run hash is recorded and its file is usable | serves it |
| `stale`, reason `changed` / `upstream_stale` / `failed` | the run hash moved (code, an upstream, a sensor's or `Manual` asset's output, a declared env var; `cache.changed` says which) or the last attempt failed | runs it |
| `stale`, reason `artifact_missing` | the result is recorded but its file is gone, and something needs to read it | computes it again under the same run hash ("A cached result whose artifact is missing") |
| `stale`, reason `evicted` | the same, because the cache size limit deleted the file | computes it again ("Disk use"); a copy the artifact store holds is downloaded instead, and is not stale |
| `unsynced` | cached, but not where it should be: with an artifact store, this machine's copy, the store's object and the recorded digest do not all agree; with a configured server, its ledger has not received the result yet | repairs that (an upload, a download) without running a step (`barca docs remote`, `barca docs server-mode`) |
| `never_run`, `partial`, `unknown`, `always_runs` | no result yet; some partition keys cached; not knowable without running; a task or sensor | `barca docs status` |

A missing or evicted artifact that nothing needs to read leaves the node `cached`.

## Forcing a recompute

| Goal | Command |
|---|---|
| Normal, cache-aware | `barca get target pipeline.py` |
| Recompute everything in the cone | `barca get target pipeline.py --refresh-all` |
| Recompute chosen assets and everything downstream of them | `barca get target pipeline.py --refresh a,b` |
| Recompute only the chosen assets | `barca get target pipeline.py --refresh a,b --no-cascade` |
| Recompute some keys of a partitioned target | `barca get sales pipeline.py --partition region=emea --refresh sales` |
| Run a task, cached upstream | `barca run task pipeline.py` |
| Run a task, refresh chosen upstream assets and everything downstream of them | `barca run task pipeline.py --refresh a,b` |
| Run a task, refresh only the chosen assets | `barca run task pipeline.py --refresh a,b --no-cascade` |
| Run a task, refresh all upstream assets | `barca run task pipeline.py --refresh-all` |

`barca get` and `barca run` take the same three flags. On `get` the target is an asset, so
`--refresh` may name it too. In Python, use `barca.get(..., refresh_all=True)` or
`refresh=[...]`.

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
- With `--partition`, only the selected keys of the target are planned, so refreshing the target
  recomputes only them. An upstream partitioned asset named in `--refresh` is recomputed for
  every key: the selection narrows the target only (`barca docs partitions`).

For external data that changes in place (a blob overwritten at the same path), the canonical
answer is a sensor ("External data that changes in place" above). Without one, `--refresh`
the asset that reads it, and everything built from it re-runs.

Never delete files under `.barca/` to force a recompute: `--refresh` also keeps the metadata DB
consistent. To look at a cached result, use `barca sql` (`barca docs sql`).

### A `Manual` upstream is read as it is

A `Manual` asset or task (`@asset(freshness=Manual)`) is refreshed by hand only. When
`barca get` or `barca run` reads one on the way to the target, it is not recomputed, even when
its inputs or code changed: the run reads its latest successful result, and does not plan
anything above it. Its step is `cached` with a `detail` saying so (`Manual: its last result is
read as it is`, or `Manual and stale: ...`). Nodes below it are hashed against that result:
its run hash and, as for a sensor, the SHA-256 of its bytes. They are served from cache until it
changes, and once it has a new result they are stale relative to it, also when a refresh by hand
gives new bytes for the same code and inputs (a reference table that reads external data):
`barca status` reports them `stale` (reason `changed`), `--dry-run` predicts that they run, and
the next `barca get` of one of them, or the next tick that reaches them, recomputes them. A
refresh that gives the same bytes changes nothing below it. When it is stale, stderr says how to
recompute it:

```text
[barca] note: baseline is Manual and stale; barca get baseline pipeline.py --refresh baseline to recompute
```

- Only naming it in `--refresh` recomputes it: `--refresh baseline` (with the cascade,
  everything below it in the cone too) or `--refresh-all`. That holds when it is the target too:
  `barca get baseline` without `--refresh baseline` returns its current result, with the same
  note when it is stale. A `Manual` task named as the target of `barca run` runs, since
  `--refresh` names assets only.
- `--refresh` cannot name an asset behind it: the run does not plan it, so the name is a usage
  error that lists the assets you can refresh.
- A `Manual` node that has never been computed cannot be read: the command exits 2 and the
  remediation is the command that computes it. `--dry-run` predicts the same.
- Sensors are polled whenever a run needs them, whatever their freshness. A partitioned `Manual`
  node is not pinned yet: it is checked like any other asset. Its keys' bytes count below it as
  an unpartitioned one's do, so refreshing one key with new bytes
  (`--partition region=emea --refresh sales`) makes that key of what reads it stale.
- A `Manual` asset runs in a phase of its own, before what reads it, as a sensor does.
- On `barca serve`, `POST /runs` (what `--server` sends) and ticks read them as they are, as
  `barca get` does (`barca docs scheduling`); the older aliases `POST /get/{target}`,
  `POST /run/{target}` and `POST /run` compute `Manual` upstreams as before.

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
upstream or a sensor's output changed; when the step succeeded before, `changed` lists what
differs from that success and `detail` reads `changed: code (+1 more)`, with the entries
`barca docs status` describes; for a partitioned step, those of its first key that runs),
`artifact_missing` (the result is cached, but its
artifact is gone and something needs to read it) or `evicted` (the same, because the cache size
limit deleted it). Under `--no-cascade`, a cached step downstream of a refreshed asset carries a
`warning`. A step that reads a sensor is predicted from the sensor's last recorded output, and
its `detail` says so. `summary` counts steps, one per partition key.

A dry run makes the same decisions a real run makes (it calls the same code), and the test suite
checks that `will_run` equals the real run's `steps_executed` across cold, warm, `--refresh`,
`--refresh --no-cascade` and `--refresh-all` runs.

## What a run reports

A real `barca get` / `barca run` returns the same per-step information in a `steps` array, with a
`status` of `ran`, `cached` or `partial` (and the same `reason` / `warning`). `steps_executed` is
the number of steps that actually ran; a fully cached second run reports 0 for assets. In
`--agent` mode a cached step also prints `[barca] step:<id> cached` on stderr, so a log shows
what was served from cache as well as what ran. A step whose node declares `env=[...]` also
carries `env`, the values it was hashed with (`null` when unset, `<redacted>` for secret-looking
names), in both the JSON `steps` entry and the `--agent` line (`... env SOURCE_CSV=b.csv`). With
an artifact store, a step whose stored result (or an input it read) is not the copy that was
recorded carries `artifact_mismatch: true` and a `warning` (`barca docs remote`).

### Recompute byte checks

A step is meant to be a pure function of its code and inputs, so computing it again under the
same run hash should write the same bytes. Barca checks: every successful materialization records
a SHA-256 digest of the serialized bytes, and a recompute of the same node and run hash
(`--refresh`, `--refresh-all`, a missing artifact) compares with the preceding one. If the bytes
differ, barca warns that the function is not deterministic and sets `steps[].bytes_changed: true`
in run JSON. A new run hash is an expected change and does not warn; a result recorded without a
digest sets the baseline. The check is on serialized bytes, so a serializer that writes variable
metadata can also warn. Artifact paths and exit codes are unchanged. Partitioned steps are
compared per key; the run's step is marked if any key changed. Only assets are checked: a sensor
returns a new value under the same run hash whenever the data it watches changes, and a task runs
every time, so new bytes from either are expected and never warn.

A `Manual` asset is not checked either. It is refreshed by hand with the same code and inputs to pick up
new data, so new bytes are what the refresh is for: no warning, no `bytes_changed`, no
`last_bytes_change`. What reads it is stale after such a refresh, since its bytes count in their
run hashes (`barca docs scheduling`).

`barca status` shows the last such event even after a later unchanged recompute: its JSON has
`last_bytes_change` (node id, run hash, digest, UTC timestamp) and
`last_materialization.content_hash` when a digest was recorded. The boundary is specified in
`specs/materialization-digests.md`.

```bash
barca get orders pipeline.py --refresh orders --json   # compare recomputed bytes with the last digest
barca status orders pipeline.py --json                 # last byte-change event and current digest
```

### Seeing what happened

```bash
barca history --json            # recent runs: status, steps executed, steps cached
barca stats total pipeline.py --json   # timing percentiles and cache hit rate for one asset
barca plan pipeline.py          # what would run, in phases, without running it
```

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
  targets you named, or with no target the one asset whose artifact is `final_output` (the last
  asset). A partitioned target returns every key (or the keys `--partition` selects), so every
  returned partition is checked (one file lookup per key, a few milliseconds at 5,000 keys).
  Every other asset at the end of a pipeline is treated like an intermediate and is not looked
  at.
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
- A function that is not deterministic may return a different value when it is computed again
  ("Recompute byte checks"); cached steps downstream of it keep the results they were computed
  with (as after `--refresh --no-cascade`).
- A dry run does not contact a remote artifact store. A result recorded in one is predicted as
  `cached` even if the object has since been deleted from the bucket; the real run finds out
  when it fetches it, and computes it again. A store that is a directory is checked, and if
  the directory itself is gone the dry run predicts `cached` where the run exits 3.

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
Barca never reads one again and never lists them in `barca status` or any other command. They
take disk space until you remove them, or until the size limit evicts them: they are the first
thing it deletes ("Disk use"). To see them all:

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
  fetch (exit 3) or the upload (a warning; the result stays a pending upload), naming the
  path, and is left as it is. The message says to remove or rename it there; `--refresh-all`
  does not help (`barca docs remote`).

## Disk use

`.barca/` has a size limit. Without one, every recompute under a new run hash (a code edit, an
input change, a sensor tick) adds an artifact and keeps the old one, so the cache only grows.

```toml
# barca.toml
[cache]
max_size = "20GB"        # or "500MB", "20GiB", a number of bytes, "10%" of the filesystem, or "off"
```

- With no `max_size`, the limit is **10% of the filesystem** `.barca/` is on. `"off"` removes the
  limit.
- `BARCA_CACHE_MAX_SIZE` overrides `barca.toml`, for a container that sets its limit when it is
  deployed. An invalid value is a usage error (exit 2) naming where it came from.
- `KB`, `MB`, `GB`, `TB` are decimal (`GB` = 10^9 bytes); `KiB`, `MiB`, `GiB`, `TiB` are binary;
  a bare number is bytes. A percentage is of the filesystem's size, read at each check.
- The size is everything under `.barca/`, every environment included: artifacts, the metadata
  DBs, staging, `sql-cache/`. The DBs are counted and never deleted.

**When.** At the end of `barca get` and `barca run` (after results are recorded and uploads have
finished), after every run under `barca serve`, and once a minute under `barca serve`. When
`.barca/` is over the limit, barca deletes files in this order until it is at or below **80%** of
the limit:

| Order | What | Which first | Costs when needed again |
|---|---|---|---|
| 1 | Leftovers: `staging/<pid>/` of processes that are not running, `*.moved-aside*` directories, `sql-cache/` copies not used in the last 5 minutes | dead staging, then moved-aside, then the least recently used copies | nothing (nothing reads them) |
| 2 | Superseded results: a node (a partition key, for a partitioned asset) whose newest successful result is another one | oldest first | a recompute, only if the code and inputs return to that exact state |
| 3 | Local copies the artifact store holds (its upload was confirmed with a digest) | least recently read first | a download |
| 4 | Current results | least recently read first | a recompute |

**Never evicted:**

- what the command produced, read or returns: a path `barca get` just printed still exists when
  the command exits;
- what a run in flight in the same process holds (under `barca serve`, every run in progress
  pins the results it reads); while a run is starting and its plan is not known yet, only tiers
  1 and 2 are evicted;
- with a run in progress in **another process** on this `.barca/` (the `running` rows of
  `barca history`, any environment), only tiers 1 and 2, and no staging directory (that process
  may be in another container, where a pid that looks dead here is alive);
- the current result of a `freshness=Manual` node that the store does not hold: nothing but a
  person recomputes a `Manual` node. Barca finds `Manual` nodes by reading the project's source
  files (no import); if one of them does not parse, no current result without a store copy is
  evicted;
- a result whose upload to the artifact store is still pending (`unsynced`, `local only` in `barca status`):
  its local copy is the only one until the next command uploads it;
- the metadata DBs, their logs, locks and backups, and any file under `.barca/artifacts/` that no
  result names (written by hand, or by another tool): counted, never deleted. Nothing outside
  `.barca/` is ever deleted, and the artifact store is never touched.

**What you see.** One line on stderr when something was evicted:

```
[barca] cache 21.4 GB over its 20 GB limit; evicted 312 files (5.4 GB): 290 superseded results, 22 local copies held by the store
```

Under `--json` the run result gains `"cache": {"size_bytes", "limit_bytes", "evicted_files",
"evicted_bytes"}` (sizes after eviction); the key is absent when nothing was over the limit. If
everything that may be evicted is not enough, barca says so once per process:
`[barca] warning: cache working set 26 GB is larger than max_size 20 GB; results will be
recomputed often`.

An evicted result is handled like a missing artifact ("A cached result whose artifact is
missing", above), with its own wording: it is downloaded again if the store holds it, or computed
again under the same run hash, only when something needs to read it.

```
[barca] pipeline.py:features: its cached result was evicted to stay under the cache limit; computing it again
```

The step's `reason` is `evicted`. `barca status` shows the node as `stale` with reason `evicted`,
and `--dry-run` predicts `run` with reason `evicted`. A result the store holds is predicted
`cached`: it costs a download, not a recompute.

**`barca gc`** runs the same eviction now:

```bash
barca gc --dry-run              # what would be evicted, per tier, and what it leaves
barca gc                        # evict now, down to 80% of max_size
barca gc --max-size 5GB         # a one-off tighter budget
barca gc --max-size off         # no limit: every leftover and superseded result, nothing else
barca gc --json                 # {size_bytes, limit_bytes, target_bytes, after_bytes, tiers, evicted, failed, ...}
```

`--dry-run` lists exactly what `barca gc` then deletes (the same order, from the same
measurement), unless something changes in between. With the limit `off`, `barca gc` evicts every
leftover and superseded result, which never costs a recompute of anything current. `--env NAME`
evicts only that environment's results (leftovers are shared). A file that cannot be deleted is
skipped with one warning; `barca gc` exits 3 only when a metadata DB cannot be read.

**How it is measured.** The check after a command adds the recorded sizes of the results the
metadata DBs hold, walks everything outside the artifact directories, and applies what the last
full walk of the artifact directories found. A full walk happens when that says `.barca/` is over
the limit, at least once a day, and under `barca gc`. The check under the limit takes about 2 ms
on a small project and about 0.1 s with 100,000 results recorded. Eviction after a command stops
after 5 seconds; what is left waits for the next command. When eviction cannot get under the
limit, the check does not plan again for 10 minutes unless the size grows.

**Read times.** "Least recently read" is the `last_read_at` of a result: set when a step is given
it as an input, when `barca get` returns it, and when a `barca sql` query names its view. File
access times are not used (most filesystems do not keep them).

Known limits:

- Sizes are file lengths, not the blocks the filesystem allocates.
- barca reacts to its own limit only: it does not evict because something else filled the disk,
  and a run that writes more than the free space fails with the operating system's error.
- A run in another process that has just decided a superseded result is cached (an older run
  hash it went back to) can find it gone while it reads it; it is then computed again.
- `materializations` rows are never deleted: `barca history` and `barca stats` keep every run.

## Environments

`--env <name>` (or `BARCA_ENV`, or `default_env` in `barca.toml`, else `default`) fully
separates cache, artifacts, history and its backups. Use it for dev/staging/prod isolation.

`get` and `run` print the environment and artifact store they use once at startup, on
stderr, when the environment comes from `default_env` in `barca.toml` or an artifact store is
configured, so a one-off command on a box set up for another environment says so:

```
[barca] env prod (default_env in barca.toml); artifact store local (.barca/envs/prod/artifacts)
[barca] env dev (--env); artifact store s3://bucket/proj/dev/artifacts
```

The source in brackets is `--env`, `BARCA_ENV`, `default_env in barca.toml` or `built-in
default`. Credentials and signed URI queries are redacted. The line is never on stdout, so
`--json` output is unchanged. With neither setting, nothing is printed. `serve` always prints
the line, with `; metadata <path> (db format <n>)` added (`barca docs scheduling`, "The metadata
DB at startup").

## Concurrent runs

Several barca processes can run in one project at once (parallel scripts or agents, `barca serve`
alongside the CLI). The metadata DB is a single-file database that one process opens at a time,
so each process holds a short lock on it (`.barca/metadata.db.lock`) only while it reads or
writes, and releases it while your Python runs. Processes queue instead of failing. If a
process waits more than 60 seconds for the lock you get an error that names the lock file;
an `... File is locked by another process` error means something outside barca (a DB browser, a
backup tool, an older barca) has `.barca/metadata.db` open.

If a required database write fails, the command reports an infrastructure error (exit 3) and
records no new outcome; what was already recorded stays. With an artifact store, a refresh in
one process can replace an artifact another process is reading: the path names the computation,
not the bytes. What an upload records in that case is specified in
`specs/artifact-upload-receipts.md`.

## While a run is going, and after one is killed

A run records each completed step in `.barca/metadata.db` while it runs, normally within about
half a second (with an artifact store, once its upload is confirmed). The order of these writes
and how a failed write is retried are specified in `specs/cache-decisions.md`.

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
  runs again. Failed steps are recorded when the run ends, so a killed run records its successes
  only.
- **History says so.** `barca history` reports a run whose process no longer exists as
  `interrupted`, with `finished_at` and `elapsed_seconds` `null` (nobody saw it end) and
  `steps_executed` at what it had recorded. It does so as soon as the process is gone, and
  also for a run killed with its container, once a container starts again on the same
  `.barca` (see the limits below). The next `get` or `run` writes `interrupted` to the history.
  Ctrl-C and SIGTERM (a supervisor, `docker stop`) are different: the run stops its workers,
  records itself and is `cancelled` (exit 130). With an artifact store that also holds while
  artifacts upload or download or the history is backed up, and the cancelled run then backs
  its record up for at most 10 seconds; a second Ctrl-C ends that (`barca docs remote`, "Ctrl-C").

**When a run is `interrupted`.** Only when barca can observe that the run's process is gone;
whenever it cannot, the run stays `running`. Barca looks at the process table on the same
kernel, at a marker file the process holds open (`.barca/run-owners/`) from another container on
the same kernel, and at whether the owner's kernel has shut down for a marker on a local disk
(`specs/cache-decisions.md`). Machine ids, host names and clocks decide nothing: a killed run
reads `interrupted` at once, and a suspended laptop or a stopped process (Ctrl-Z) does not make a
live run look dead.

Known limits:

- With a store (`barca docs remote`) all of this holds on the machine the run is on. Newly
  committed progress is backed up every minute in healthy operation, without ending the run,
  and the final outcome when it ends; a backup is restored only by `barca state restore`.
- With a remote artifact store, an unfinished upload is never recorded as reusable progress.
  A failed upload is recorded at the end of the run as a pending upload, `unsynced` in
  `barca status` and uploaded by the next command (`barca docs remote`, "When an upload fails
  after the steps succeeded"). A failed backup leaves local history intact and is retried;
  ticks with nothing new take no backup.
- A run that is going is shown as `running` from everywhere that sees the project directory.
  The one situation in which a history says `interrupted` for a run that is going somewhere is
  a copy of the whole disk (a cloned or restored virtual machine) taken during the run and
  started as another machine: the copy's history is a copy too, and in it the run will never
  end. And two virtual machines resumed from the same memory snapshot are one machine as far as
  anything observable goes (same boot id, same processes): if they share a project directory, a
  run killed on one is shown as `interrupted` there while its twin on the other is still going.
- What stays `running` although its process is gone:
  - a run another machine started (its row came with a restored backup), until that machine
    records it;
  - with Docker Desktop, a run killed on the host as seen from a container, and a run
    killed in a container as seen from the host: they are different kernels and the
    directory between them is not a local disk. The side the run was on shows
    `interrupted`, and once it has recorded that, so does the other;
  - a run killed with its container when `.barca` is on a Docker Desktop bind mount (which
    gives no file handle) and another container was started before the replacement, so
    that the replacement got another pid namespace number. Restarting one container gives
    the same number in practice. A named volume for `.barca` does not depend on it;
  - a run killed in a container by a restart of the machine or of Docker's virtual machine
    when `.barca` is on a bind mount from a Docker Desktop host or on a network
    filesystem;
  - a run killed by a restart of the machine when the project is on a network filesystem.
- A marker is removed by its process when its runs have ended. One left by a killed process
  stays while its run reads `running`, and is removed by a later `get` or `run` once no
  `running` run names it and it is an hour old; so is one left by a process killed between
  creating its marker and recording its run. Markers are empty files.

After verifying that the process or container has stopped, explicitly reconcile an uncertain
row with `barca history --reconcile RUN_ID --json`. This records only that row as
`interrupted` and prints it in the normal history envelope (`runs`, `total: 1`,
`truncated: false`); `--fields` and human output also work. It refuses a proven live owner,
a terminal row, an unknown id, and combining `--reconcile` with `--limit` or `--all`.
It preserves recorded step counts, successful materializations and artifacts, and leaves
unknown end time and duration as `null`. It does not stop a process. Ordinary history reads
still keep an uncertain owner `running`; reconciliation is your assertion that it stopped.
The change is local to the selected environment's metadata database.

## Where things live

```
.barca/metadata.db                          run history and materialization records (local DB)
.barca/backups/<name>.db, <name>.json        with a store: the five newest backups of the metadata DB (`barca docs remote`)
.barca/metadata.db.pre-restore              the database `barca state restore` replaced
.barca/metadata.db.reset-<UTC>              a database `barca state reset` moved aside (with its -wal, -shm)
.barca/serve.lock                           held by a running `barca serve`, so `barca state restore` refuses to run under it
.barca/run-owners/<token>.fifo              a marker a barca process holds open while it has a run in flight ("While a run is going, and after one is killed")
.barca/artifacts/<node>/<run_hash>.<ext>    one file per result
.barca/staging/<pid>/                       a process's files in transit (uploads, downloads, result batches)
.barca/sql-cache/                           copies of store artifacts `barca sql` downloaded (`barca docs sql`)
.barca/gc-state.json                        what the last measurement of `.barca/` found ("Disk use")
.barca/envs/<env>/                          the same layout for a named environment ("Environments")
```

`<ext>` is `.json`, `.pkl`, `.parquet` or `.csv` (see `barca docs types`). An artifact's path
names the computation, not the bytes: the run hash covers the step's code and inputs, so the same
step with the same inputs always writes the same path, and computing it again overwrites the file
("Recompute byte checks"). To keep artifacts and a backup of the history in a bucket, set
`BARCA_REMOTE_URI` and your cloud's credentials (`barca docs remote`).

`.barca/` lives in the **project root**: the nearest directory at or above the one you run barca
from that holds a `barca.toml`. Without a `barca.toml` above you, the current directory is the
root. Barca changes into the root before doing anything, so:

- running from a subdirectory reads and writes the same cache as running from the root
  (`cd sub && barca get out ../pipeline.py` is a cache hit after `barca get out pipeline.py`);
- file arguments are read relative to where you typed them, and node ids are relative to the
  root, so `pipelines/p.py:out` is the same id from any directory;
- steps run with the root as their working directory, so `Path("data.txt")` inside a step means
  the root's `data.txt` wherever you invoke barca;
- stderr says `barca: project root: <path>` whenever the root is not the current directory;
- the project root is the boundary of the dependency cone ("Not followed").

Put an empty `barca.toml` at the top of a project to anchor it. Barca records the metadata
format version and refuses a database it does not support rather than rebuilding it
(`specs/metadata-schema.md`).

## Upgrading

A barca release that changes how run hashes are computed recomputes the affected steps once, on
the first run after upgrading; the release notes say so (`CHANGELOG.md`, GitHub Releases).

See also: `barca docs remote`, `barca docs types`, `barca docs partitions`, `barca docs status`,
`barca docs server-mode`, `barca docs contract`.
