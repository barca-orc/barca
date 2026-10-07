# Assets, inputs and freshness

An asset is a function whose output barca caches and tracks.

```python
from barca import asset, Always, Manual, Schedule


@asset()                                   # freshness=Always (default)
def raw() -> dict:
    return {"x": 1}


@asset(inputs={"data": raw}, retries=3, retry_backoff=2.0, timeout_seconds=600)
def clean(data: dict) -> dict:
    return {"x": data["x"] + 1}


@asset(freshness=Manual)                   # only recomputed on explicit refresh
def pinned() -> dict:
    return {"x": 0}


@asset(freshness=Schedule("0 5 * * *"))    # fires daily at 05:00 under `barca serve`
def daily() -> dict:
    return {"x": 2}
```

## Options on `@asset`

| Option | Meaning |
|---|---|
| `inputs={"param": upstream}` | Wire upstream nodes to parameters. The param name must match a function parameter. |
| `name="..."` | Explicit node name (the default is the function name). |
| `freshness=` | `Always` (default), `Manual`, or `Schedule("<cron>")`. |
| `serializer=` | Force `"json"`, `"pickle"` or `"parquet"`. See `barca docs types`. |
| `partitions=` | Fan one asset out over keys. See `barca docs partitions`. |
| `timeout_seconds=` | Per-attempt time limit (default 300). |
| `retries=` | Total attempts on failure; 1 means no retry. |
| `retry_backoff=` | Base delay in seconds; the delay grows linearly with the attempt number. |
| `env=["NAME", ...]` | Environment variables the function reads. Their values are part of the cache key and are reported per step. See below. |
| `description=`, `tags=` | Metadata. |

## Environment variables: `env=`

An asset that reads an environment variable (a source path, a region, a model name) should
declare it. Barca reads the declared variables when it plans the run, folds each name and value
into the run hash, and reports the values each step used.

```python
import os

from barca import asset


@asset(env=["SOURCE_CSV", "API_TOKEN"])
def raw() -> dict:
    return {"source": os.environ.get("SOURCE_CSV", "default.csv")}


@asset(inputs={"data": raw})
def summary(data: dict) -> dict:
    return {"from": data["source"]}
```

```bash
export SOURCE_CSV=a.csv
barca get summary pipeline.py --agent     # raw and summary run
barca get summary pipeline.py --agent     # both cached
export SOURCE_CSV=b.csv
barca get summary pipeline.py --agent     # raw and summary run again
```

- Changing a declared variable re-materializes the asset and everything downstream of it.
  Unset is its own value, distinct from an empty string. Variables that are not declared are
  not part of the cache key.
- The JSON result's `steps` entry for the node carries `"env": {"API_TOKEN": null,
  "SOURCE_CSV": "b.csv"}` (`null` = unset), and `--agent` step lines end with
  `env API_TOKEN=<unset> SOURCE_CSV=b.csv`.
- Names ending in `_TOKEN`, `_SECRET`, `_KEY` or `_PASSWORD` (any case, or the bare word) are
  hashed like any other but shown as `<redacted>` in all output.
- `barca list` shows the declared names in an ENV column (`env` in `--json`).
- `env=` must be a literal list of string literals; anything else (a variable, a tuple, a
  computed name) is a parse error. It is also accepted on `@task` and `@sensor`, which always
  run: there it only records the values used.

**Limitation:** barca cannot see environment variables your code reads without declaring them.
Those are not part of the cache key and are not reported, so a changed value does not
invalidate the asset.

## How a node is identified

A node id is `<file>:<function>` (for example `pipeline.py:clean`), or the explicit `name=`.
Targets on the command line can use the bare function name (`barca get clean pipeline.py`).
Several targets are one comma-separated list (`barca get clean,report pipeline.py`): their
upstream cones are planned together, so an asset both need materializes once, and the JSON
output is keyed by target (`barca docs agents`).
With no target, `barca get pipeline.py` materializes every asset and sensor in the file and skips
tasks (previously it ran them too); run a task with `barca run <task> pipeline.py`.
An input defined in another file is imported like any Python name (`from pipelines.sources
import raw`, then `inputs={"r": raw}`); barca resolves the import statically. Use
`asset_ref("other/file.py:raw")` to name one without importing it. See `barca docs discovery`.

## Naming

An asset is an ordinary Python function, so its name is an ordinary Python name. If a file
imports a module (`import carry_forward_registry`) and also defines an asset with the same
name, the `def` rebinds the name and shadows the module for the rest of the file. Give the
import an alias (`import carry_forward_registry as cf_registry`) or rename the asset. Barca does
not warn about this; Python simply uses the later definition.

## Static analysis

Planning never imports your code. The decorators, `inputs=` and `freshness=` must be written
literally enough for barca to read them from the source. Dynamic decorator construction
(building `inputs` in a loop, calling a decorator through a variable) is not visible to the
planner. Mark code barca cannot reason about with `@unsafe` (silences purity warnings only).

## Unused inputs

A declared input costs something even when the function ignores it: the upstream is
materialized first, its value is loaded and passed, and it is part of the step's cache key. At
plan time barca reads each function body (statically: nothing is imported or run) and warns
about an input the function never uses:

```python
from barca import asset


@asset()
def raw() -> list:
    return [1, 2, 3]


@asset(inputs={"raw": raw})
def report(raw: list) -> int:      # `raw` is never used
    return 42
```

```
[barca] warning: pipeline.py:report never uses its input `raw`. It is still loaded in full each time the step runs, and it counts toward the step's cache key. Use it, remove it from inputs=, or rename the parameter `_raw` if it is there for ordering only (a `_` input is not loaded and never flagged)
```

**The rule.** An input is reported when its parameter name does not start with `_` and the
function body never mentions the name, or mentions it only as `del name`. A mention is the name
in code, anywhere in the body (reading it, passing it to a helper, a nested function, a
comprehension, an f-string, assigning to it), or the name as a whole word inside any string in
the body: `duckdb.sql("select * from orders")`, `pl.sql("... from orders")` and
`df.query("amount > @threshold")` read variables by name, so `orders` and `threshold` are used
there. The check is deliberately conservative, so a warning means the input really is unused;
when barca cannot tell, it says nothing.

The string match is on whole identifiers and is case-sensitive: `orders` is not found in
`reorders`, `orders_v2` or `Orders`. Every string and bytes literal in the body counts,
multi-line, concatenated and the text of f-strings included, except a string that is a statement
on its own: a docstring that describes an input does not use it. Comments never count.

**Never reported:**

- an input whose name starts with `_`. This is the way to say "unused on purpose": the step
  still runs after the upstream and still re-runs when the upstream changes, but nothing is
  loaded and the parameter is `None` (`barca docs tasks`). Rename both
  the key in `inputs=` and the parameter;
- a function whose body is only a docstring, `pass`, `...` or `raise`: a stub, or a gate that
  only raises, uses nothing by definition;
- a function that takes `**kwargs`, or whose body mentions a name through which Python can
  reach a parameter without naming it, as a bare name or as an attribute
  (`builtins.locals()`, `inspect.currentframe().f_locals`, `sys._getframe()`, `**locals()`).
  Then no input of that function is reported.
  Dynamic access names: `locals`, `vars`, `eval`, `exec`, `currentframe`, `_getframe`, `f_locals`, `f_back`, `getargvalues`, `inspect.stack`.
- a function that passes text barca cannot read to a call that resolves names from text: an
  argument is a variable, a module constant, an f-string or a concatenation rather than a
  literal (`duckdb.sql(QUERY)`, `con.execute(q)`, `duckdb.table(name)`, `pl.sql(q)`,
  `pl.SQLContext(frames)`, `df.query(expr)`; `df.eval(expr)` is covered by `eval` above). Then
  no input of that function is reported. The call is recognised by its name alone, as a method
  or attribute of anything, or as a bare name, also under an import alias
  (`from duckdb import sql as dsql`); any argument that is not a literal makes it unreadable.
  Using such a call as a value instead of calling it (`q = duckdb.sql`,
  `map(con.execute, queries)`) silences the function too. The one exception: a call on a name
  that `import` binds to a module of the Python standard library or to one of the packages
  listed below, and that the function never rebinds, is not a query entry point
  (`pa.table(d)`, `np.view(...)`, `json.query(...)`). A call on any other module stays silent:
  it may be your own (`mylib.sql(QUERY)` where `mylib` does `from duckdb import sql`, a `db`
  helper wrapping a connection) or a package barca does not know.
  Unrelated packages: `pyarrow`, `numpy`, `matplotlib`, `scipy`, `sklearn`, `requests`, `httpx`, `aiohttp`, `urllib3`, `yaml`, `orjson`, `torch`, `tensorflow`, `xgboost`, `lightgbm`, `statsmodels`, `seaborn`, `plotly`, `networkx`, `sympy`, `boto3`, `botocore`, `fsspec`, `tqdm`, `pydantic`, `click`, `rich`, `PIL`, `cv2`, `jinja2`, `dateutil`, `pytz`, `barca`.
  Query entry points: `sql`, `execute`, `executemany`, `query`, `from_query`, `table`, `view`, `read_sql`, `read_sql_query`, `SQLContext`.
- an input annotated `duckdb.DuckDBPyRelation`: barca binds it as a view named after the
  parameter, so SQL in a helper function, which this check does not read, can use it without
  the step's body naming it at all (`barca docs types`);
- an input that comes from a `@sensor`: depending on a sensor without reading its value is how
  a step is made to re-run when outside state changes (`barca docs cache`).

**Where it appears.** `barca plan`, `barca get`, `barca run` and `get|run --dry-run` report the
same warnings in the same two places: one `[barca] warning: ...` line per unused input on
stderr, in every output mode (`--pretty`, `--json`, `--agent`), printed before any step runs; and
the `warnings` array of the JSON output, `[]` when there are none:

```json
{"kind": "unused_input", "node": "pipeline.py:report", "param": "raw", "message": "pipeline.py:report never uses its input `raw`. ..."}
```

Only the steps the command plans are checked: `barca get report pipeline.py` warns about
`report` and what is upstream of it, not about other steps in the file; `barca plan` and
`barca get` with no target cover the whole file. A step is reported once per unused input, however
many partition keys it has, and whether or not it is served from cache: the list depends on the
source and the target only. `barca list` and `barca status` do not report it. A warning never
changes the exit code or the cache.

For an input annotated `pl.LazyFrame` the message says, instead of "loaded in full", that a
parquet artifact is opened but not read.

**Limitations.** The check works on names, in one function body, and prefers silence to a wrong
warning. That costs missed warnings:

- It does not follow the value. An input that is only assigned to (`raw = None`) or only passed
  to a helper that ignores it is not reported.
- A string that happens to contain the input's name counts as a use, whatever the string is
  for: `return {"orders": 1}` or a log message naming `orders` hides an unused `orders`.
- Any call named like a query entry point with an argument that is not a literal silences the
  whole function, SQL or not, unless it is a call on a standard-library module or one of the
  listed packages: `client.query(params)`, `cursor.execute(statement, values)`,
  `tensor.view(n, -1)` and `mylib.table(name)` silence, because a local variable or a module
  barca does not know may hold anything. So does any use of such an attribute as a value
  (`request.query`, `self.table`), and any mention of a dynamic access name.
- An unused `duckdb.DuckDBPyRelation` input and an unused sensor input are never reported.

And one wrong warning it cannot avoid: a function whose input is read only from somewhere this
check does not look. That is a helper function that inspects its caller's frame, DuckDB with
`python_scan_all_frames` reading a caller's variable from SQL inside a helper, or an alias made
outside the body (`grab = locals` at module level, then `grab()`), or a file of your own that
is named like a standard-library module or a listed package and re-exports a query entry point
(a project `json.py` doing `from duckdb import sql`). Mention the input by name
in the body, or `_`-prefix it, to say otherwise. There is no flag or configuration key that
turns the warning off.

## Sensors

`@sensor` observes external state and returns `(update_detected: bool, value)`. Sensors have no
inputs and must use `Manual` or `Schedule(...)` freshness, never `Always`.

A sensor always runs, and its `value` is part of the run hash of every asset that reads it: when
the value changes, those assets and everything downstream of them re-run; when it is the same,
they are served from cache. This is how to track external data that changes in place: a sensor
returns a blob's etag and the asset that reads the blob depends on the sensor (`barca docs cache`,
"External data that changes in place"). Return only what identifies the data: a value that
changes on every run, such as a timestamp, re-runs the sensor's consumers every time. The
`update_detected` flag is not used for caching.

`barca get <sensor> pipeline.py` observes one sensor. `barca get pipeline.py` (no target) observes
every sensor, including one nothing depends on: a sensor is something `get` can target, and
observing is read-only. Tasks are the only nodes a bare `get` skips.

See also: `barca docs tasks`, `barca docs cache`, `barca docs scheduling`.
