---
title: Getting Started
description: Run cached assets, tasks and partitions locally, then run the same project on a server.
---

## Install

You need Python 3.12 or later. With [uv](https://docs.astral.sh/uv/):

```bash
uv init --app my-project
cd my-project
uv add barca
```

uv is recommended, not required. `pip install barca` in a virtualenv works the same
way; then type `barca` wherever this page says `uv run barca`. The asset, task, partition and server examples need no optional extras. Wheels exist for macOS on Apple Silicon and for Linux on x86-64 and arm64, glibc
and musl.

## Write two assets

Create `pipeline.py`:

```python
from barca import asset


@asset()
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]


@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "total": sum(d["x"] for d in data)}
```

An asset is a function whose result barca caches. `inputs=` says that the parameter `data`
receives the result of `raw_data`.

## Run it

```bash
uv run barca get pipeline.py
```

```
[barca] 2/2 steps | done in 0.0s
Run 151401da97ef | all assets in 0.133s (2 steps, 1 phase)

.barca/artifacts/pipeline.py--summary/2d10c4c5ef45….json
  {
    "count": 3,
    "total": 6
  }
```

Barca read `pipeline.py` without importing it, saw that `summary` depends on `raw_data`,
and ran both in a Python worker process, which imports the file to call the functions. It
then printed where the last asset's result is, with a preview of the
value under it when the value is small json. The `[barca]` lines are progress, on stderr. The
rest is the result, on stdout. File names are shortened here; barca prints the whole run hash.

## Where the results went

The run created a `.barca/` directory next to `pipeline.py`:

```
.barca/metadata.db                                     run history and the record of what is cached
.barca/artifacts/pipeline.py--raw_data/a8e7dac3….json  the result of raw_data
.barca/artifacts/pipeline.py--summary/2d10c4c5….json   the result of summary
```

Each file name is the step's run hash: a hash of the function's code and its inputs. Do not
commit `.barca/`; barca writes a `.gitignore` inside it, so git already ignores it.

## Run it again

```bash
uv run barca get pipeline.py
```

```
Run 151168b9247f | all assets in 0.011s (0 steps, 1 phase)

.barca/artifacts/pipeline.py--summary/2d10c4c5ef45….json
  {
    "count": 3,
    "total": 6
  }
```

`0 steps`: neither function ran, and no worker imported the file. Both run hashes match a
recorded result, so barca printed the same file in `.barca/artifacts/`.

## See what barca sees

Two commands show what barca found and what is cached. None of them runs a step.

```bash
uv run barca list      # every asset, sensor and task, with its inputs
uv run barca status    # per node: cached or stale and why, last run, shape of the result
```

With no file argument these commands read every `.py` file in the project that imports barca.
In a terminal they print tables. Piped or captured by a program, or with `--json`, they print
JSON, which is what scripts and AI agents should read (see the
[agent skill](/reference/agent-skill/)).

## Change the code

Edit `summary` so that it returns the mean instead of the total:

```python
@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "mean": sum(d["x"] for d in data) / len(data)}
```

`barca status` now reports `summary` as stale, and `--dry-run` shows what a `get` would do
without running anything:

```bash
uv run barca status
uv run barca get summary --dry-run
```

```
NAME      KIND   STATE   WHY            LAST RUN                               SHAPE           DEPS
raw_data  asset  cached  materialized   success 2026-10-21 02:36:50 UTC 0.00s  3 rows x 1 col  -
summary   asset  stale   changed: code  success 2026-10-21 02:36:50 UTC 0.00s  dict (2 keys)   raw_data

1 cached, 1 stale, 0 never run, 0 partial, 0 unknown, 0 always run
```

```
Dry run: barca get summary (nothing executed, nothing written)

STATUS    WHY            STEP
cached    -              pipeline.py:raw_data
will run  changed: code  pipeline.py:summary

1 will run, 1 cached, 0 unknown
```

Here the command names a target, `summary`. The target comes first and files, if any,
after it: `barca get summary pipeline.py`.

```bash
uv run barca get summary
```

```
[barca] 1/2 steps | done in 0.0s
Run 15134f24969a | got 'summary' in 0.068s (1 step, 1 phase)

.barca/artifacts/pipeline.py--summary/029a5a3b4fad….json
  {
    "count": 3,
    "mean": 2.0
  }
```

One step ran. `raw_data` did not change, so its result came from the cache. If you had
edited `raw_data` instead, both would have run: `raw_data` is an input of `summary`.

## Run an action every time

Add this to `pipeline.py`:

```python
from barca import task


@task(inputs={"value": summary})
def publish(value: dict) -> dict:
    print(f"Publishing {value}")
    return value
```

```bash
uv run barca run publish pipeline.py
uv run barca run publish pipeline.py
```

`publish` runs both times. Its upstream assets still use their cache. Use `get` for a
result and `run` for an action; `inputs=` connects both kinds of function the same way.

## Cache work separately by key

Add an asset with two customer partitions:

```python
from barca import partitions


@asset(partitions={"customer": partitions(["acme", "globex"])})
def report(customer: str) -> dict:
    return {"customer": customer, "score": len(customer)}
```

```bash
uv run barca get report pipeline.py
uv run barca get report pipeline.py --partition customer=acme
```

The first command computes both keys. The second reuses `acme`'s result. Each key has its
own cache entry. To react to changes in external data for individual keys, use a sensor
and `partitions_from`; see [Partitions](/workflows/03-parametrized-assets-and-partitions/).

## Run on another machine

On the machine with the project and its dependencies installed:

```bash
uv run barca serve pipeline.py --host 0.0.0.0
```

On your laptop:

```bash
uv run barca get summary --server http://barca.internal:8274
uv run barca run publish --server http://barca.internal:8274
uv run barca history --server http://barca.internal:8274
```

Replace the URL with your server's address. These commands execute the **server's copy of
the code**, print the result here, and keep the run history there. They do not upload local
edits. The web UI is at the same address. Keep the server on a private network or behind
an authenticating proxy; it has no login.

Remote execution needs no shared bucket. If you also want laptops to compute locally
against the same cache and history, follow [Share Results With a Team](/team-server/).
`--local` uses an independent local history; it does not mean local execution with shared
server state.

## Query cached results when you need SQL

SQL is optional. Install DuckDB support when you want to query asset results as tables:

```bash
uv add 'barca[sql]'
uv run barca sql "select count(*) as n, sum(x) as total from raw_data"
```

Each asset becomes a view named after its function. This query returns `n = 3`, `total = 6`.

## What to read next

- **Data from outside.** An asset that reads a file, a bucket or a table in its own body
  is computed once and then served from cache, even after the data changes. Put a
  `@sensor` in front of it: [Sensors](/guide/#4-sensors-data-from-outside).
- **Large inputs.** An unannotated DataFrame input is read in full with pandas:
  [Large inputs](/patterns/08-large-inputs/).
- **Environments and remote stores.** `--env prod` keeps a separate cache and history, and a
  remote store keeps results and backups of the history: [Configuration](/reference/config/),
  [Remote storage](/reference/remote-storage/).
- **Schedules and the HTTP API.** [Scheduling](/scheduling/).
- **Everything else.** The [Guide](/guide/), the [CLI reference](/reference/cli/), and
  `barca docs`, which prints the manual in the terminal.
