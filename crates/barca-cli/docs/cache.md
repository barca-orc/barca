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
**dependency cone**: every module-level function, constant and import the function uses, followed
transitively. That includes **helper modules in your project**: `.py` files in the pipeline
file's directory and its subdirectories (packages with or without `__init__.py`). Both import
styles are followed, at the same precision:

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
way: only the definitions the step uses, never the whole module. Modules outside the project
(the standard library, installed packages) are not hashed: after upgrading one, recompute with
`--refresh-all` or `--refresh` (below).

The pipeline file can be named any way on the command line: `barca get rows pipeline.py`,
`./pipeline.py`, an absolute path, and `barca get rows project/pipeline.py` from the parent
directory all compute the same run hash. Node ids keep the spelling you typed (`pipeline.py:rows`
vs `./pipeline.py:rows`), as before.

Not followed yet (an edit there does not change the hash; recompute with `--refresh-all` or
`--refresh`):

- classes (`from helpers import Model`): the import is recorded, but not the class body;
- imports inside the function body (`def rows(): from helpers import clean`);
- a module used as a value rather than through an attribute (`getattr(helpers, name)`);
- modules above the pipeline file's directory.

Barca runs exactly the source it hashed. It never runs stale bytecode for your pipeline files or
the modules they import from the same directory tree: their `__pycache__` .pyc files are checked against a
hash of the source, not its mtime and size, so an edit that keeps both (a same-size edit within
one second, or a tool that pins mtimes such as Nix, Bazel, `touch -t` or `rsync -t`) still runs
the new code. Installed packages import as usual.

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
.barca/artifacts/<node>/<run_hash>.<ext>    one immutable file per materialization
```

`<ext>` is `.json`, `.pkl` or `.parquet` (see `barca docs types`). Artifacts are
content-addressed, so they can be shared between machines when remote state is configured
(`barca.toml`; see https://barca.sh/reference/config/).

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
`refresh_cascade` (downstream of an asset named in `--refresh`), `refresh_all` (`--refresh-all`), or
`not_materialized` (no cached result for this code and these inputs: never run, or the code, an
upstream or a sensor's output changed). Under `--no-cascade`, a cached step downstream of a
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
`steps` entry and the `--agent` line (`... env SOURCE_CSV=b.csv`). `barca history --json` and `barca stats` show the same over time.

## Concurrent runs

Several barca processes can run in one project at once (parallel scripts or agents, `barca serve`
alongside the CLI). The metadata DB is a single-file database that one process opens at a time,
so each process holds a short lock on it (`.barca/metadata.db.lock`) only while it reads or
writes, and releases it while your Python runs. Processes queue instead of failing. If a
process waits more than 60 seconds for the lock you get an error that names the lock file;
an `... File is locked by another process` error means something outside barca (a DB browser, a
backup tool, an older barca) has `.barca/metadata.db` open.

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
