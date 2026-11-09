---
title: "The Cache"
description: "What the run hash covers, artifacts, --refresh and --refresh-all, early cutoff, dry runs, environments and what a run reports."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/cache.md
---

<!-- Generated from crates/barca-cli/docs/cache.md (barca docs cache). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

Barca skips a step when it has already computed the same thing. This topic covers what "the
same" means (the run hash), what the hash does and does not see, the states a cached result
can be in, how to force a recompute, the cache size limit, and where everything is stored.

## At a glance

| What changed | What runs again |
|---|---|
| Nothing | No asset: each is served from cache. Sensors and tasks in the run still run |
| An asset's code, or helper code it calls | That asset and everything downstream of it |
| A value a `@sensor` returns | The assets that read the sensor, and everything downstream |
| A variable listed in the asset's `env=[...]` | That asset and everything downstream |
| `inputs=`, `serializer=` or a partition dimension name | That asset and everything downstream |
| Another decorator option (`retries=`, `timeout_seconds=`, ...) or formatting | Nothing |
| A file, table or bucket the asset reads in its own body, or an environment variable it does not list | Nothing: barca cannot see it. Read it through a `@sensor` ("External data that changes in place") or list it in `env=` |
| A new partition key | That key's steps, and any `collect()` of the asset |
| An `@asset(cutoff=True)` reran and wrote the same bytes | Nothing below it ("Early cutoff") |
| Nothing, but you want a recompute | `--refresh name` reruns it and what is downstream; `--refresh-all` reruns the cone ("Forcing a recompute") |

Tasks and sensors are never served from cache: they run every time they are in a run.
`barca status` says which state each node is in and why, and `--dry-run` shows what would run.

## What is cached

Each asset step has a **run hash**: a hash of the function's definition, its inputs' run hashes,
the output of any sensor or `Manual` asset it reads (and, in place of the run hash, the output
of an `@asset(cutoff=True)` it reads: "Early cutoff" below), for partitioned assets the partition key,
and the values of any environment variables it declares with `env=[...]`. If the hash matches a
previous successful materialization, the artifact is reused and the function does not run.
Change the function's code, any upstream, a sensor's or `Manual` asset's output or a declared
environment variable and the hash changes, so only the affected subgraph re-runs.

- Tasks and sensors are never served from cache. Partitioned assets are cached per key (see
  [`barca docs partitions`](/reference/partitions/)).
- Each successful materialization also records a short digest of each part of its hash (the
  function, each decorator argument that counts, each helper, each input, each declared env
  var), so `barca status` and `--dry-run` can say which part changed since then
  (`changed: helper pipelines/common.py:bucket_ltv`; [`barca docs status`](/reference/status/)). The digests explain
  a miss; they never decide one.
- Environment variables a function reads **without** declaring them are not part of the hash:
  changing one does not invalidate anything. Declare them with `@asset(env=["NAME"])`
  ([`barca docs assets`](/reference/assets/)). Nodes that declare no env hash exactly as if `env=` did not exist.
  A declared variable is read from barca's environment, and when it is not set there, from the
  project's `.env`, so a value a pipeline loads from `.env` at import is the one hashed:
  editing it in `.env` re-runs the asset. Barca warns about the undeclared reads it can see
  ("Environment variables" below).
- barca runs exactly the source it hashed. It never runs stale bytecode for your pipeline files
  or the project modules they import: their `__pycache__` .pyc files are checked against a hash
  of the source, not its mtime and size, so an edit that keeps both (a same-size edit within one
  second, or a tool that pins mtimes such as Nix, Bazel, `touch -t` or `rsync -t`) still runs
  the new code. Installed packages import as usual.

How the coordinator assembles a step's hash from its upstreams, and the guarantees that come
with it, are specified in `specs/behavior/cache-decisions.md`.

### Environment variables

A declared variable (`env=["NAME"]`) is part of the run hash. A variable the code reads without
declaring it is not, so switching it, for example pointing a registry at another CSV, leaves
the step `cached` with the result computed from the old value. To make that visible, barca looks
for environment reads in the step's function and in every project definition its hash follows
(the same helpers as "What the definition covers"), without importing anything. A read counts
when the name is a string literal:

- `os.environ["NAME"]`, `os.environ.get("NAME")`, `.setdefault("NAME", ...)`, `.pop("NAME")`
  and `"NAME" in os.environ`, also through `from os import environ`;
- `os.getenv("NAME")`, or `getenv("NAME")` imported from `os`;
- a pydantic settings class (`class Settings(BaseSettings)`): each field reads its name in upper
  case after the class's `env_prefix`, or the name given by `Field(env=...)`,
  `Field(validation_alias=...)` or `Field(alias=...)`.

A name built in an f-string with a literal part (`os.environ.get(f"{name.upper()}_CSV")`) is
reported as a pattern with `*` for the interpolated parts (`*_CSV`); declaring any name that
matches the pattern covers it. Names that are not literal at all (`os.environ[key]`), reads in
installed packages, and reads of variables starting with `BARCA_` are not reported.
A variable set in the project's `.env` is reported like any other until it is declared: only a
declared variable's `.env` value is part of the hash.

The reads not declared in `env=` are reported, and nothing about caching changes:

- `get`, `run`, their `--dry-run` and `plan` print one line on stderr before anything runs,
  for the whole command, when any planned step has an undeclared read (whether it runs or is
  served from cache): `[barca] warning: 3 steps read environment variables not declared in
  env=: *_REGISTRY_CSV, PLANNING_SOURCE_MODE, REGION (+2 more); barca status lists them`. It
  names at most five variables or patterns, sorted. `--agent` prints the same line.
- The JSON `warnings` array has one item per step and variable: `kind: "undeclared_env"`, the
  variable in `param` ([`barca docs contract`](/reference/cli-contract/), "Plan warnings").
- Each JSON `steps[]` entry of `get`, `run` and `--dry-run`, and each `nodes[]` entry of
  `barca status --json`, carries `undeclared_env`, the sorted list, when it is not empty.
  Experimental.

Declare a variable the result depends on; leave the rest (a log level, a credential that does
not change the data). To recompute once after switching an undeclared variable, use
`--refresh <step>`.

### What the definition covers

The definition part of the hash covers what determines the step's result:

- the function, from `def` to its last line, as written (comments and formatting included);
- the decorator arguments that affect the result or what is stored, listed in `barca docs
  hashing`, "Which decorator arguments count". Arguments that do not, such as `description=`, `tags=`,
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

The full rules for the definition, which helpers are followed and how precisely, which
decorator arguments count, and what is not followed, are in [`barca docs hashing`](/reference/hashing/).

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
- It does **not** reach assets outside the target's cone. When a refresh writes new bytes, a
  consumer that another target reads keeps its run hash and its old result, and `barca status`
  reports it `cached`: refresh again with a target below it, or name it in `--refresh`
  ([`barca docs batched-partitions`](/reference/batched-partitions/), "When a consumer's keys are stale").
- The cascade stops at an `@asset(cutoff=True)`: what reads it is decided by the bytes the
  refresh wrote ("Early cutoff" below). Same bytes, it stays cached and carries no warning; new
  bytes, it runs as `not_materialized`. Name it in `--refresh`, or use `--refresh-all`, to
  recompute it anyway.
- `--no-cascade` re-materializes **only** the assets you name. Run hashes cover definitions and
  upstream hashes, not output contents, so a cached downstream asset still matches and the
  refreshed data never reaches it. Barca prints
  `warning: 'mid' was served from cache but depends on refreshed 'src' ...` when this happens.
  `--no-cascade` without `--refresh` is a usage error (exit 2).
- With `--partition`, only the selected keys of every partitioned node with that dimension are
  planned, upstream and downstream, so `--refresh` of any of them recomputes only those keys. A
  `collect()` fan-in over them is skipped, and naming it in `--refresh` is a usage error
  ([`barca docs partitions`](/reference/partitions/), "Selecting keys").

### Refresh selected keys in a complete DAG

Use keyed selectors to refresh one key or a set independently of the output domain:

```bash
barca get final pipeline.py --refresh 'source[account=A]'
barca get final pipeline.py --refresh 'source[account=A],source[account=C]'
barca run publish pipeline.py --refresh 'source[account=A],source[account=C]'
```

Each selector names one dimension and value; it selects every source key with that coordinate.
Combine selectors with commas to select a set. Bare asset names still refresh all planned keys.
A malformed selector or a selector matching no planned key is a usage error. Quote brackets
so the shell does not expand them. With `--partition`, selectors must match the narrowed domain.

The full target DAG and partition domain remain planned. Only selected keys and their aligned
cascade recompute; fresh siblings keep their hashes and artifacts. An invalidated `collect()`
reads all current keys, combining refreshed keys and cached siblings. Missing or stale siblings
follow normal cache validation and artifact recovery; a failure blocks publication rather than
omitting an input. `batch="all"` receives the keys that need work. `--no-cascade` and cutoff
semantics apply as above. A cutoff whose bytes stay unchanged leaves its consumers cached;
a dry run reports consumers of a running cutoff as `unknown` / `cutoff_pending`.

`--dry-run --json` reports selected work and sibling cache counts in `partitions`, including
`will_run_keys`; `reason` distinguishes `refresh` and `refresh_cascade`. The publisher's step
reports whether the complete fan-in runs or stays cached. A real report has the corresponding
`status` and counts. A cached consumer after the refreshed cutoff reports that propagation
stopped at unchanged cutoff bytes.

Python uses `refresh=["source[account=A]", "source[account=C]"]`; POST `/runs` uses the same
strings in its `refresh` array. Both use the CLI's cache engine.

For external data that changes in place (a blob overwritten at the same path), the canonical
answer is a sensor ("External data that changes in place" below). Without one, `--refresh`
the asset that reads it with a target downstream of everything built from it: what is built from
it in that target's cone re-runs.

Never delete files under `.barca/` to force a recompute: `--refresh` also keeps the metadata DB
consistent. To look at a cached result, use `barca sql` ([`barca docs sql`](/reference/sql/)).

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
  `barca get` does ([`barca docs scheduling`](/scheduling/)); the older aliases `POST /get/{target}`,
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
| `unknown` | Cannot be known without running: a dynamic partition (`partitions_from`) whose source has to run first to produce its keys (`reason: "partitions_unknown"`), an asset reading a sensor with no recorded output (`reason: "sensor_output_unknown"`), or an asset reading an `@asset(cutoff=True)` that would run first (`reason: "cutoff_pending"`, "Early cutoff" below), and anything that depends on any of them. |

`reason` is one of `task` and `sensor` (always run), `refresh` (named in `--refresh`),
`refresh_cascade` (downstream of an asset named in `--refresh`), `refresh_all` (`--refresh-all`),
`not_materialized` (no cached result for this code and these inputs: never run, or the code, an
upstream or a sensor's output changed; when the step succeeded before, `changed` lists what
differs from that success and `detail` reads `changed: code (+1 more)`, with the entries
[`barca docs status`](/reference/status/) describes; for a partitioned step, those of its first key that runs),
`artifact_missing` (the result is cached, but its
artifact is gone and something needs to read it) or `evicted` (the same, because the cache size
limit deleted it). A `cached` step of an asset pinned with `barca pin` has `reason` `pinned`
([`barca docs pins`](/reference/pins/)). Under `--no-cascade`, a cached step downstream of a refreshed asset carries a
`warning`. A step that reads a sensor is predicted from the sensor's last recorded output, and
its `detail` says so. `summary` counts steps, one per partition key.

A dry run makes the same decisions a real run makes (it calls the same code), and the test suite
checks that `will_run` equals the real run's `steps_executed` across cold, warm, `--refresh`,
`--refresh --no-cascade` and `--refresh-all` runs.

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
def orders_etag() -> str:
    # Stands in for a blob's etag. With azure-storage-blob, for example:
    #   BlobClient.from_blob_url(url, credential).get_blob_properties().etag
    return hashlib.md5(Path("orders.csv").read_bytes()).hexdigest()


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

- A sensor always runs. Its output is serialized like any other ([`barca docs types`](/reference/types/)), and the
  SHA-256 of those bytes is folded into the run hash of every asset that reads the sensor
  directly. Everything downstream of those assets changes with them, through their run hashes.
  Assets that do not depend on the sensor are not affected.
- The sensor's own code counts too. A sensor's run hash covers its function and the helpers it
  reaches, and is part of its consumers' run hashes next to the hash of its output. Editing
  the sensor's function re-runs the assets that read it, even when it returns the same value.
  Editing its `freshness=`, `description=` or another decorator argument that does not count
  ([`barca docs hashing`](/reference/hashing/), "Which decorator arguments count") re-runs nothing.
- The same code and the same output give the same run hash, so the consumer is served from
  cache. Going back to an earlier output (an etag that was current before) serves the
  materialization made then.
- Sensors run in a phase of their own, before their consumers, so a consumer's cache decision
  always uses the value the sensor returned in this run.
- These examples return a value for content-based comparison. An explicit `False`
  retains the previous observation; `True` advances a separate freshness revision even
  without a value. `(changed, value)` also supplies data ([`barca docs assets`](/reference/assets/), "Sensors").
- A partitioned asset that reads a sensor re-runs every key when the sensor's output changes.
  To re-run only the keys whose data changed, let the sensor return a dict of key -> version
  and take the keys from it with `partitions_from(sensor)`: each key then folds in its own
  value only ([`barca docs partitions`](/reference/partitions/), "Updating only what changed, across many partitions").
- `--refresh` (with its cascade), `--no-cascade` and `--refresh-all` work on assets that read a
  sensor as on any other, on `get` and `run`.

**The trap: return only what identifies the data.** Every sensor's output is hashed, so a sensor
whose value changes on every run (a timestamp, `datetime.now()`, a request id, a dict that
includes the time it was fetched) makes every asset that reads it re-run every time:

```python
@sensor()
def orders_etag() -> dict:
    props = get_blob_properties()
    return {"etag": props.etag, "checked_at": time.time()}   # re-runs bronze every time
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

## Early cutoff: `@asset(cutoff=True)`

An asset's consumers fold in its run hash, so when it reruns (its code, an upstream or a sensor
it reads changed) everything below it reruns, even when it wrote exactly the bytes it wrote
before. A global step that every partition key reads is the expensive case: one rerun reruns
every key. `cutoff=True` makes what reads the asset depend on the bytes it wrote instead:

```python
from pathlib import Path
from barca import asset, partitions, sensor

CUSTOMERS = ["acme", "globex", "initech"]


@sensor()
def rate_feed() -> float:
    return float(Path("rate.txt").read_text())


@asset(inputs={"raw": rate_feed}, cutoff=True)
def rates(raw: float) -> dict:
    return {"usd_eur": round(raw, 1)}          # most moves of the feed round the same


@asset(inputs={"rates": rates}, partitions={"customer": partitions(CUSTOMERS)})
def billed(customer: str, rates: dict) -> dict:
    return {"customer": customer, "eur": 100 * rates["usd_eur"]}
```

```bash
barca get billed pipeline.py              # rates and every key of billed run
barca get billed pipeline.py              # the feed moved 0.91 -> 0.93: rates runs, billed is cached
barca get billed pipeline.py --dry-run    # after an edit to rates: billed is unknown (cutoff_pending)
```

How it works:

- **What is hashed: the serialized bytes.** Every materialization already records the SHA-256 of
  the artifact file it wrote (`content_hash` in `barca status --json`, "Recompute byte checks"
  below). A consumer folds in that digest and the key it came from in place of the cutoff
  asset's run hash; nothing else is computed. The same value always gives the same bytes for
  `json` and `pickle`, and for `parquet` from pandas, polars, pyarrow and DuckDB with the same
  library version (checked with each: none writes a timestamp). A library upgrade that changes
  the bytes reruns what reads the asset once; it never serves a stale result. A `json` dict
  with the same items in another order is other bytes, and reruns its consumers: they are
  handed the dict in that order.
- **Its consumers are decided after it ran.** A cutoff asset ends its chain in the plan, as a
  sensor does, and what reads it is decided in a later phase, from the bytes it wrote in this
  run (or the digest recorded with its cached result). A partitioned consumer usually has a
  phase of its own already, so the plan does not grow (`benchmarks/RESULTS.md`, "Early cutoff").
- **Partitioned, it cuts off per key.** Each key of a consumer reads the bytes of the key it
  reads; a `collect()` fan-in reads every key's bytes, by key.
- **A dict `partitions_from` source.** An asset that returns `{key: value}` for
  `partitions_from(...)` with `cutoff=True` gives each key its own value only, not the source's
  run hash, as a sensor source does ([`barca docs partitions`](/reference/partitions/), "Updating only what changed"):
  when it reruns, only the keys whose value changed run.
- **Only the cutoff asset's bytes stand in for it.** What reads it still reruns when its own
  code, its other inputs or its declared env change. Its code does not reach its consumers
  another way: a consumer's parameter named after it (`def billed(customer, rates)`) counts as
  a use of the module-level name ([`barca docs hashing`](/reference/hashing/), "What counts as a use"), which for a
  cutoff asset adds a constant marker, not its code. Calling a cutoff asset as a plain function
  from another step is not supported (read it as an input).
- **`cutoff` itself does not count in the asset's run hash**: turning it on or off does not
  rerun the asset. It does change what its consumers fold in, so **the first run after turning
  it on (or off) reruns everything that reads it, once.**
- **Dry runs and `barca status`** cannot know the bytes of a cutoff asset that would run. What
  reads it, and everything below that, is `unknown` with reason `cutoff_pending` and a
  `detail` naming the asset (`reads 'rates' (cutoff=True), which would run first: this step
  runs only if it writes different bytes`). When the cutoff asset itself is cached, its
  recorded digest decides, and the prediction is exact. For a partitioned consumer the whole
  step is `unknown`, also when only some keys of a partitioned cutoff asset would run.
- **`--refresh`** of a cutoff asset recomputes it; its cascade stops there ("Exactly what
  `--refresh` does" above). `--refresh-all` recomputes everything. `--partition` narrows as
  usual.
- **With a server** ([`barca docs server-mode`](/reference/server-mode/)) the ledger is asked for a consumer's run hash in
  the phase that decides it, once the cutoff asset ran or was found cached; the ledger rows
  carry the digest, so a laptop and the server compute the same consumer hashes.
- **Sinks** are written when their step runs. A consumer kept cached by cutoff does not write
  its sinks again; what they hold was written from the same input bytes.
- **A nondeterministic asset** (a timestamp, a random sample, rows in no fixed order) writes
  new bytes on every rerun, so `cutoff=True` saves nothing on it, though it is legal. Barca does
  not warn when you set it; a rerun under the same run hash that writes other bytes gets the
  usual warning ("Recompute byte checks" below), unless the asset also says
  `deterministic=False`. Either way what reads it reruns whenever its bytes differ, so it is
  never served stale. Sort rows before you return them.
- Only on `@asset`. A sensor's output already counts below it; a task is never served from
  cache. Experimental ([`barca docs contract`](/reference/cli-contract/)).

Why opt-in, not automatic for small outputs: turning cutoff on reruns everything below the
asset once, so turning it on for every asset in an upgrade would rerun every pipeline; and every
cutoff asset ends its chain and makes what reads it `unknown` in a dry run until it has run.
Whether an output is small is only known after it ran, so an automatic rule would change the
plan and the dry run from run to run. You choose where the extra phase pays for itself: a global
step read by many keys, or one whose inputs change more often than its result.

## Cached, stale, evicted, unsynced

A result is a row in `.barca/metadata.db` (the run hash it was computed under) plus an artifact
file. `barca status` names the state each node is in; the table of states and reasons is owned
by [`barca docs status`](/reference/status/), "Cache state". In short: `cached` is served; `stale` runs again, because
the run hash moved (`changed`, `upstream_stale`), the last attempt failed (`failed`), or the
recorded file is gone and something needs to read it (`artifact_missing`, or `evicted` when the
cache size limit deleted it, [`barca docs disk`](/reference/disk/); a copy the artifact store holds is downloaded instead
and is not stale); `unsynced` is cached but not where it should be, and the next `get` repairs
that without running a step ([`barca docs remote`](/reference/remote-storage/), [`barca docs server-mode`](/reference/server-mode/)); `blocked` keys wait
on failed keys of an input ([`barca docs partitions`](/reference/partitions/)). A missing or evicted artifact that nothing
needs to read leaves the node `cached` ("A cached result whose artifact is missing").

Store availability reported by inspection can reuse a cloud observation for five minutes.
Local file and plain-directory-store identity changes invalidate digest evidence immediately.
Inspection shape/digest evidence is bounded separately from the ledger; required reads and
ledger publication still verify directly ([`barca docs remote`](/reference/remote-storage/), "Inspection cost and observation window").

## What a run reports

A real `barca get` / `barca run` returns the same per-step information in a `steps` array, with a
`status` of `ran`, `cached` or `partial` (and the same `reason` / `warning`). `steps_executed` is
the number of steps that actually ran, a step that failed included (count `steps[]` by `status`
for how many succeeded and failed); a fully cached second run reports 0 for assets. In
`--agent` mode a cached step also prints `[barca] step:<id> cached` on stderr, so a log shows
what was served from cache as well as what ran. A step whose node declares `env=[...]` also
carries `env`, the values it was hashed with (`null` when unset, `<redacted>` for secret-looking
names), in both the JSON `steps` entry and the `--agent` line (`... env SOURCE_CSV=b.csv`). With
an artifact store, a step whose stored result (or an input it read) is not the copy that was
recorded carries `artifact_mismatch: true` and a `warning` ([`barca docs remote`](/reference/remote-storage/)).

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
run hashes ([`barca docs scheduling`](/scheduling/)).

An asset that is expected to return new bytes for the same code and inputs, such as a snapshot
of an external system, can say so with `@asset(deterministic=False)`: barca then neither warns
nor sets `bytes_changed` or `last_bytes_change` for it. Nothing else changes. Its result is
cached under its run hash like any asset's (it is not recomputed more often), what reads it is
hashed with its run hash as before (new bytes alone do not make them stale; make the external
data an input, a sensor or `freshness=Manual` for that), and the argument is not part of the run
hash, so adding or removing it recomputes nothing.

```python
from barca import asset

@asset(deterministic=False)
def exchange_rates() -> dict:
    return fetch_rates()   # an external snapshot: new bytes are expected
```

`barca status` shows the last such event even after a later unchanged recompute: its JSON has
`last_bytes_change` (node id, run hash, digest, UTC timestamp) and
`last_materialization.content_hash` when a digest was recorded. The boundary is specified in
`specs/interfaces/materialization-digests.md`.

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
  either ([`barca docs remote`](/reference/remote-storage/)). The store has to be there for that to count: if its bucket,
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
  replaced: [`barca docs remote`](/reference/remote-storage/)). A symlink to a file counts as that file and is read; writing
  the artifact again replaces the link.
- `--agent` announces each step once, with its outcome. A cached step whose artifact is not on
  disk waits: it prints `step:<id> completed` if it is computed again, or `step:<id> cached` at
  the end of the run if nothing needed it. The one exception is a result in a remote store
  whose object turns out to be deleted when it is fetched: its `cached` line was already
  printed, and the warning and a `completed` line for the same step follow
  ([`barca docs contract`](/reference/cli-contract/)).
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
thing it deletes ([`barca docs disk`](/reference/disk/)). To see them all:

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

- a `@sink` file path is yours. Barca writes the file there and changes nothing else: a
  directory at the path fails the sink, and so the step (`SinkError: sink ...:
  IsADirectoryError`), and is left as it is, and a symlink is written through (the file it
  points to is written, the link stays). A dataset sink (a path ending in `/`) owns its prefix
  instead, and refuses one that holds files it did not write ([`barca docs sinks`](/reference/sinks/));
- a directory at an object's path in an artifact store that is a shared directory fails the
  fetch (exit 3) or the upload (a warning; the result stays a pending upload), naming the
  path, and is left as it is. The message says to remove or rename it there; `--refresh-all`
  does not help ([`barca docs remote`](/reference/remote-storage/)).

## Incremental runs

A normal `get` or `run` reuses previously validated freshness before expanding partition
keys. When one sensor key changes, only that key's affected chain needs new cache decisions;
unchanged keys remain cached. Target selection plans only the target's dependency cone.
This works with the usual DAG workflow and needs no flag or batching setting.
If a key returns to an older input value, its earlier cached result remains selected on
later unchanged reads. Historical selections are bounded; larger sets use normal cache
validation. Upgrading the freshness-proof format also causes one normal validation.

The first run validates the full requested graph. Code, declared environment, partition
membership and upstream changes invalidate the affected evidence; refreshes and writes from
another run force normal validation. Missing artifacts needed by active work or returned
values are recovered through the usual cache path. Freshness does not promise that unused
artifact files still exist.

A sensor returning every key still discovers and compares every key. A changed `collect`
consumer still reads all its inputs, and returning every small partition value requires
loading those values. Compound partition changes and local execution against a remote
server's ledger use normal validation. Batching controls function calls independently.

## Disk use

`.barca/` has a size limit (`[cache] max_size`, default 10% of its filesystem) and `barca gc`
evicts down to it after every run; what counts, what is evicted in which order and how the
metadata database keeps to its share are in [`barca docs disk`](/reference/disk/).

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
the line, with `; metadata <path> (db format <n>)` added ([`barca docs scheduling`](/scheduling/), "The metadata
DB at startup").

On a machine with more than one environment (`.barca/metadata.db` for `default`, and each
directory of `.barca/envs/`), a command that names none with `--env` or `BARCA_ENV` prints one
note on stderr naming the environment it uses and the ones there are, in every output mode
(stdout and `--json` are unchanged):

```
[barca] note: no --env given, so this uses env staging (default_env in barca.toml); this project has 2 environments (prod, staging). Pass --env NAME or set BARCA_ENV; require_env = true in barca.toml (or BARCA_REQUIRE_ENV=1) makes it required
```

`require_env = true` at the top level of `barca.toml` (or `BARCA_REQUIRE_ENV=1`, which beats
it; `BARCA_REQUIRE_ENV=0` turns it off) makes the environment required: every command that
reads or writes one (`get`, `run`, `status`, `history`, `logs`, `stats`, `sql`, `gc`, `state`
and `serve`) exits 2 without `--env` or `BARCA_ENV`, naming the environments there are, even
when `default_env` is set, and `barca doctor` reports it as a failed `environment` check.
`plan`, `list` and `docs` read no environment and are not affected.

```bash
BARCA_REQUIRE_ENV=1 barca history --env prod   # what a box that requires --env accepts
```

## Where things live, concurrent runs, killed runs, upgrading

The project root, the `.barca/` directory, several barca processes in one project, what a run
records while it goes and after it is killed, and what an upgrade recomputes are in
[`barca docs layout`](/reference/layout/).

See also: [`barca docs disk`](/reference/disk/), [`barca docs layout`](/reference/layout/), [`barca docs remote`](/reference/remote-storage/), [`barca docs types`](/reference/types/),
[`barca docs partitions`](/reference/partitions/), [`barca docs status`](/reference/status/), [`barca docs server-mode`](/reference/server-mode/), [`barca docs contract`](/reference/cli-contract/).

---

*The same text is in your terminal: `barca docs cache`.*
