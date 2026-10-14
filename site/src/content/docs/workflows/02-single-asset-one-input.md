---
title: "Workflow: Single Asset With One Upstream Input"
description: One asset that reads another, run with barca 0.18.0. What runs after each kind of change, and what --refresh and --no-cascade do.
---

Two assets, one reading the other: how the input is passed, and which steps run after each
kind of change. Output on this page is from barca 0.18.0.

This page used to be a design specification (a proposed decorator signature, an indexing
step, a `.barcafiles/` layout). What follows is what the binary does. The shorter version of
the same pattern is [Asset-to-Asset](/patterns/01-asset-to-asset/).

## The two assets

```python
# pipeline.py
from barca import asset


@asset()
def a() -> str:
    return "banana"


@asset(inputs={"fruit": a})
def b(fruit: str) -> str:
    return fruit.upper()
```

`inputs={"fruit": a}` says that the parameter `fruit` receives the result of `a`. The key
must be the name of a parameter of the function. Barca does not check this when it plans; a
mismatch fails when the step runs:

```
Worker failed: TypeError: b() got an unexpected keyword argument 'fruit'
```

`b` is still an ordinary function. Called from Python, `b("kiwi")` returns `"KIWI"`.

```bash
barca list pipeline.py
```

```
NAME           KIND   FRESHNESS  DEPS
-------------------------------------
pipeline.py:a  asset  always     -
pipeline.py:b  asset  always     pipeline.py:a
```

## Run it

Ask for `b`. Barca runs `a` first, stores its result, and passes it to `b`:

```bash
barca get b pipeline.py
```

```
[barca] 2/2 steps | done in 0.0s
Run 5274349f8960 | got 'b' in 0.165s (2 steps, 1 phase)

Value:
"BANANA"
```

The second run executes nothing (`0 steps`). `barca get a pipeline.py` returns `a` alone,
also from cache.

## What runs after a change

Each row was run in order, starting from the state above. `--agent` prints one line per step
on stderr, which is where the "cached" and "completed" words come from.

| Change | `a` | `b` |
|---|---|---|
| Nothing | cached | cached |
| Edit the body of `b` | cached | runs |
| Edit the body of `a` (`"cherry"`) | runs | runs |
| Put `a` back (`"banana"`) | cached | cached |
| `barca get b pipeline.py --refresh a` | runs | runs |
| `barca get b pipeline.py --refresh a --no-cascade` | runs | cached, with a warning |

- `b`'s run hash includes `a`'s run hash, so a change to `a` reaches `b`.
- Putting `a` back restores its old run hash. The results stored under that hash are still on
  disk, so nothing runs.
- `--refresh a` recomputes `a` and everything downstream of it in the target's cone.
- `--no-cascade` recomputes only what you name. `b` is then served from cache and does not
  reflect the refresh; barca says so:

  ```
  [barca] warning: 'b' was served from cache but depends on refreshed 'a', so it does not reflect the refresh. Drop --no-cascade, add it to --refresh (for example --refresh a,b) or use --refresh-all.
  ```

  The run hash covers code and inputs' hashes, not output bytes. If `a` is not deterministic
  and returns something new, `b` still matches its cached entry.

## See what would run, and what is stored

After editing `a` and before running anything:

```bash
barca status pipeline.py
```

```
NAME  KIND   STATE  WHY             LAST RUN                           SHAPE  DEPS
a     asset  stale  changed         success 2026-10-07 18:23:36 0.00s  str    -
b     asset  stale  upstream_stale  success 2026-10-07 18:23:36 0.00s  str    a

0 cached, 2 stale, 0 never run, 0 partial, 0 unknown, 0 always run
```

`--dry-run` answers the same question for one command and its flags. After editing only `b`:

```bash
barca get b pipeline.py --dry-run
```

```
Dry run: barca get b (nothing executed, nothing written)

STATUS    WHY                                              STEP
cached    -                                                pipeline.py:a
will run  no cached result for this code and these inputs  pipeline.py:b

1 will run, 1 cached, 0 unknown
```

To look at a stored result, use `barca sql "select * from b"`. `barca history` lists the
runs with how many steps each executed and how many it served from cache.

## Limits

- The upstream must be named statically in `inputs=`. An input defined in another file is
  imported like any Python name; see [Discovery](/reference/discovery/).
- `a`'s result is written to a file and read back for `b`. `b` receives a deserialized copy,
  not the object `a` returned. Formats and readers: `barca docs types`.
- An asset that reads a file or a bucket in its body has no input that changes when the data
  does. See [Sensors and External Observations](/workflows/06-sensors-and-external-observations/).

Next: [Parametrized Assets and Partitions](/workflows/03-parametrized-assets-and-partitions/).
