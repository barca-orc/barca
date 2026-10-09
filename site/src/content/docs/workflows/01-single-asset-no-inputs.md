---
title: "Workflow: Single Asset, No Inputs"
description: One asset with no inputs, run with barca 0.18.0. What is written to disk, when it is served from cache, and how to look at the result.
---

One asset with no inputs: what `barca get` does with it, what ends up on disk, and when the
function runs again. Output on this page is from barca 0.18.0.

This page used to be a design specification (a `.barcafiles/` directory with `code.txt` and
`metadata.json`, an indexing step that imports modules, a hash that covers `uv.lock` and the
Python version). None of that was built. What follows is what the binary does.

## The asset

Save as `pipeline.py` in an empty directory:

```python
from barca import asset

PREFIX = "yellow"


@asset()
def a() -> str:
    return f"{PREFIX} banana"
```

The decorator returns the function unchanged, so `a()` called from Python still returns
`"yellow banana"`.

## See what barca sees

```bash
barca list pipeline.py
```

```
NAME           KIND   FRESHNESS  DEPS
-------------------------------------
pipeline.py:a  asset  always     -
```

Barca reads the source to find this. It does not import the file. The node id is
`<file>:<function>`; on the command line the bare name `a` is enough.

## Run it

```bash
barca get a pipeline.py
```

In a terminal:

```
[barca] 1/1 steps | done in 0.0s
Run 52255d4e3e20 | got 'a' in 0.138s (1 step, 1 phase)

Value:
"yellow banana"
```

Piped or captured, stdout is one JSON object instead (`--json` forces it, `--pretty` forces the
table). Progress stays on stderr:

```json
{"elapsed_seconds":0.887860791,"final_output":"yellow banana","phases":1,"run_id":"522b0b6dc4d0","status":"success","steps":[{"detail":"no cached result for this code and these inputs","id":"pipeline.py:a","kind":"asset","reason":"not_materialized","run_hash":"380bca24...","status":"ran"}],"steps_executed":1,"warnings":[]}
```

Run the same command again and the function does not run:

```
Run 52252d1eb128 | got 'a' in 0.021s (0 steps, 1 phase)

Value:
"yellow banana"
```

In JSON the step has `"status":"cached"`, an `artifact` path, and `steps_executed` is 0.

## What is on disk

The run created `.barca/` in the project root:

```
.barca/.gitignore
.barca/metadata.db            (plus metadata.db-wal and metadata.db.lock)
.barca/artifacts/pipeline.py--a/380bca24....json
```

- `metadata.db` holds run history and the record of each result.
- The artifact is the returned value, stored as json because a string is JSON-serializable
  (`barca docs types` lists the formats). It is cached by run hash: the file name is a hash of
  the function's code and its inputs, not of the output.
- `.barca/.gitignore` contains `*`, so git ignores the directory.

Do not read or delete files under `.barca/` by hand. Use these:

```bash
barca status pipeline.py
```

```
NAME  KIND   STATE   WHY           LAST RUN                           SHAPE  DEPS
a     asset  cached  materialized  success 2026-10-07 18:18:14 0.00s  str    -

1 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 0 always run
```

```bash
barca sql "select * from a"
```

```
json
yellow banana
```

`barca sql` needs `duckdb` installed in the same Python environment. A result that is not a
table (a string here) is one row with one column named `json`. `barca history` lists the runs.

## When the function runs again

The run hash covers the function's source, the decorator arguments that affect its result
(`barca docs cache` lists them), and the module-level names it uses. Each of these was run in
order after the steps above:

| Change | Result |
|---|---|
| Nothing | cached, 0 steps |
| Add a comment at the end of the file, or a constant `a` does not use | cached, 0 steps |
| `PREFIX = "green"` | runs; a second artifact file appears beside the first |
| `PREFIX = "yellow"` again | cached, 0 steps: the first artifact is still there and its hash matches |
| `barca get a pipeline.py --refresh a` | runs, and overwrites the artifact for that hash |

What the hash does not cover (installed packages, environment variables that are not declared
with `env=`, files the function reads) is listed in `barca docs cache`. A function that reads a
file or a bucket is computed once and then served from cache; see
[Sensors and external observations](/workflows/06-sensors-and-external-observations/).

`--no-cache` still works as a deprecated spelling of `--refresh-all` and prints a warning.

## Preview without running

```bash
barca get a pipeline.py --dry-run
```

reports each step as `cached` or `will run` with the reason, executes nothing and writes
nothing. `barca plan pipeline.py` prints the execution plan as JSON.

## From Python

```python
import barca

barca.get("a", "pipeline.py")   # "yellow banana"
```

`barca.get` runs the `barca` binary and returns the deserialized value, with the same caching.

Next: [one asset with one input](/workflows/02-single-asset-one-input/).
