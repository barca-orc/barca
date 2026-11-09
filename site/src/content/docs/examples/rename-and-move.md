---
sidebar:
  order: 4
title: "Worked Example: Renaming and Moving an Asset"
description: What happens to an asset's cache and history when its function is renamed or its file is moved, with and without an explicit name=, rechecked on barca 0.42.0.
---

An asset's identity is its node id: `<file>:<function>`, or the explicit `name=` if it has
one. Cached results and history are stored under that id. Barca does not detect renames or
moves. If the id changes, the asset is a new one as far as barca is concerned; if the id
stays the same, its history continues.

An earlier version of this page also described tables of definition snapshots, an indexing
step and a UI that shows an asset's definition history. Those are not built. Everything
below was observed by renaming and moving assets with barca 0.18.0 and rechecked with 0.42.0.

## Summary

| Change | Without `name=` | With `name="prices"` |
|---|---|---|
| Move the file | new node id; recomputed; history starts again | same id; served from cache; history continues |
| Rename the function | new node id; recomputed; history starts again | same id; recomputed once; history continues |
| Undo the change | old results are served from cache again | - |

## Without `name=`

```python
# assets.py
from barca import asset


@asset()
def prices() -> dict:
    return {"aapl": 465.0}


@asset(inputs={"p": prices})
def report(p: dict) -> dict:
    return {"tickers": len(p)}
```

```bash
barca get report assets.py --agent
```

```
[barca] step:assets.py:prices completed 0.0s (1/2)
[barca] step:assets.py:report completed 0.0s (2/2)
```

Rename `prices` to `fetch_prices`, body unchanged, and run again. Both steps run: the
renamed asset has a new id and no cached result, and `report` reads a different upstream.

```
[barca] step:assets.py:fetch_prices completed 0.0s (1/2)
[barca] step:assets.py:report completed 0.0s (2/2)
```

The old name is gone from every command:

```
$ barca stats prices assets.py
Asset 'prices' not found. Available: assets.py:fetch_prices, assets.py:report

$ barca sql "select * from prices"
no view named 'prices'
Views: fetch_prices, report
```

`barca stats fetch_prices assets.py` reports one materialization: its history starts at the
rename.

Moving the file (`mv assets.py pricing.py`) does the same. Both steps run again as
`pricing.py:fetch_prices` and `pricing.py:report`.

Nothing is deleted. The old results stay in `.barca/metadata.db` and under
`.barca/artifacts/`, one directory per id that has existed:

```
assets.py--fetch_prices
assets.py--prices
assets.py--report
pricing.py--fetch_prices
pricing.py--report
```

Moving the file back to `assets.py` makes the ids match earlier results again, and both steps
are served from cache.

## With `name=`

```python
# assets.py
from barca import asset


@asset(name="prices")
def prices() -> dict:
    return {"aapl": 465.0}


@asset(name="report", inputs={"p": prices})
def report(p: dict) -> dict:
    return {"tickers": len(p)}
```

The node ids are now the names alone, with no file:

```
$ barca list assets.py
NAME    KIND   FRESHNESS  DEPS
------------------------------
prices  asset  always     -
report  asset  always     prices
```

After `barca get report assets.py`, move the file (`mv assets.py pricing.py`) and run
`barca get report pricing.py --agent`. Nothing runs:

```
[barca] step:prices cached
[barca] step:report cached
```

Now also rename the function to `fetch_prices`, keeping `name="prices"`. The id is still
`prices`, but the function's source text changed (its `def` line), so its run hash changed
and it recomputes once, and `report` with it:

```
[barca] step:prices completed 0.0s (1/2)
[barca] step:report completed 0.0s (2/2)
```

The history is continuous. `barca stats` shows both materializations under one asset, and
both artifacts are in one directory, `.barca/artifacts/prices/`:

```
$ barca stats prices pricing.py
Asset: prices
Total materializations: 2
...
```

The asset is addressed by its name. `barca get prices pricing.py` works; `barca get
fetch_prices pricing.py` answers `Asset 'fetch_prices' not found. Available: prices, report`.

## Limits

- **Every command uses `name=`.** After the rename above, `barca status` lists the asset as
  `prices` and the SQL view is `prices`, as in `barca list`, `barca get` and `barca stats`, so
  saved queries keep working across a rename when the asset has a `name=`.
- **Names must be unique in the project.** Two assets with the same `name=` are an error
  (exit 2), and because commands without file arguments read the whole project, the error
  stops those commands too:

  ```
  DAG error: duplicate continuity key: 'prices' defined in both 'dup.py' and 'dup.py'
  ```

- **`name=` keeps the cache across a move, not across a rename.** A renamed function
  recomputes once, with or without `name=`.
- **Old results stay until the cache size limit evicts them.** Results under ids that no longer
  exist stay on disk until `.barca/` reaches its size limit or you run `barca gc`
  ([Disk Use](/reference/disk/)).
- **Downstream assets follow their upstream.** When an asset recomputes because of a rename
  or move, everything downstream of it recomputes too.

If you expect to reorganize files, give assets a `name=` before they accumulate history you
want to keep.
