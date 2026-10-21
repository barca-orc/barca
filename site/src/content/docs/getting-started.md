---
title: Getting Started
description: Install barca, run two assets, see them cached, and find out what reruns when the code changes.
---

## Install

You need Python 3.12 or later. With [uv](https://docs.astral.sh/uv/):

```bash
uv init --app my-project
cd my-project
uv add 'barca[sql]'
```

uv is recommended, not required. `pip install 'barca[sql]'` in a virtualenv works the same
way; then type `barca` wherever this page says `uv run barca`. The `sql` extra installs DuckDB for
`barca sql`. Wheels exist for macOS on Apple Silicon and for Linux on x86-64 and arm64, glibc
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
[barca] importing /home/you/my-project/pipeline.py
[barca] 2/2 steps | done in 0.0s
Run 151401da97ef | all assets in 0.133s (2 steps, 1 phase)

.barca/artifacts/pipeline.py--summary/2d10c4c5ef45….json
  {
    "count": 3,
    "total": 6
  }
```

Barca read `pipeline.py` without importing it, saw that `summary` depends on `raw_data`,
and ran both in a Python worker process, which imports the file to call the functions (the
`importing` line). It then printed where the last asset's result is, with a preview of the
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

Three commands show what barca found and what is cached. None of them runs a step.

```bash
uv run barca list      # every asset, sensor and task, with its inputs
uv run barca status    # per node: cached or stale and why, last run, shape of the result
uv run barca sql "select count(*) as n, sum(x) as total from raw_data"
```

`barca sql` queries the cached results with DuckDB. Each asset is a view named after its
function:

```
n  total
3  6
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
NAME      KIND   STATE   WHY           LAST RUN                           SHAPE           DEPS
raw_data  asset  cached  materialized  success 2026-10-21 02:36:50 0.00s  3 rows x 1 col  -
summary   asset  stale   changed       success 2026-10-21 02:36:50 0.00s  dict (2 keys)   raw_data

1 cached, 1 stale, 0 never run, 0 partial, 0 unknown, 0 always run
```

```
Dry run: barca get summary (nothing executed, nothing written)

STATUS    WHY                                              STEP
cached    -                                                pipeline.py:raw_data
will run  no cached result for this code and these inputs  pipeline.py:summary

1 will run, 1 cached, 0 unknown
```

Here the command names a target, `summary`. The target comes first and files, if any,
after it: `barca get summary pipeline.py`.

```bash
uv run barca get summary
```

```
[barca] importing /home/you/my-project/pipeline.py
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
