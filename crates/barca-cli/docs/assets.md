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


@asset(freshness=Manual)                   # computed only when named in --refresh
def pinned() -> dict:
    return {"x": 0}


@asset(freshness=Schedule("0 5 * * *"))    # fires daily at 05:00 under `barca serve`
def daily() -> dict:
    return {"x": 2}
```

## Qualified and aliased imports

Barca recognizes top-level imports such as `import barca`, `import barca as b`
and `from barca import asset as a`, for decorators and their helpers:

```python
import barca as b

@b.asset(partitions={"key": b.partitions(["a", "b"])})
def rows(key):
    return {"key": key}
```

Planning reads these bindings statically and never imports your module. A name
reassigned or imported from another library does not define a Barca node; local
parameters or assignments also shadow task-body helpers such as `parallel`.
Dynamic alias assignments and conditional imports are not resolved. Explicit writes to a Barca module export
conservatively disable recognition of that export through every module alias and
direct import, including earlier copies; unaffected exports remain available.

If a foreign decorator previously became a node merely because it was named
`asset`, `sensor` or `task`, import the actual Barca decorator instead. To keep a
foreign wrapper on a node, stack it with a genuine Barca decorator (qualified or
aliased imports work). Bare names with no competing binding retain legacy
source-snippet recognition. Namespace reflection retains conservative discovery,
but disables provenance-based validation and uses conservative hashing.

A later `from shared import *` preserves imported Barca names when the project module has
one literal `__all__` list or tuple of strings, with no other references to `__all__`,
that does not export those local names.
Only colliding names lose recognition, including imported aliases and `group`. Barca uses
the same package and sibling import paths as its workers and reads the source without
importing it. Missing, computed, conditional or mutated export sets, installed modules and
namespace reflection remain uncertain; put the Barca import last or use explicit imports.
Star-import dependency hashing remains conservative.

## Options on `@asset`

| Option | Meaning |
|---|---|
| `inputs={"param": upstream}` | Wire upstream nodes to parameters. The param name must match a function parameter; barca checks this when it reads the file ("Inputs and parameters" below). |
| `name="..."` | Explicit node name (the default is the function name). |
| `freshness=` | `Always` (default), `Manual`, or `Schedule("<cron>", max_failures=5)`. See `barca docs scheduling`. |
| `serializer=` | Force `"json"`, `"pickle"`, `"parquet"` or `"csv"`. See `barca docs types`. |
| `partitions=` | Fan one asset out over keys. See `barca docs partitions`. |
| `timeout_seconds=` | Per-attempt time limit (default 300). |
| `retries=` | Total attempts on failure; 1 means no retry. |
| `retry_backoff=` | Base delay in seconds; the delay grows linearly with the attempt number. |
| `env=["NAME", ...]` | Environment variables the function reads. Their values are part of the cache key and are reported per step. See below. |
| `weight=` | How much of the pool's capacity the step takes while it runs (default 1). Experimental. See "Heavy steps" below. |
| `description=`, `tags=` | Metadata. |

Stacked under `@asset`, `@sink("path")` writes the output to a file as well, and
`@sink("prefix/", partition_by=[...], rows_per_file=N)` (a path ending in `/`) writes it as a
Hive-partitioned dataset with one directory level per partition dimension of the asset. A sink
that fails fails the step. See `barca docs sinks`.

`inputs=`, `serializer=` and the dimension names in `partitions=` are part of the asset's run
hash: changing one runs the asset again. The other options, the partition keys and the
formatting of the decorator are not: editing them re-runs nothing. `barca docs cache` ("Which
decorator arguments count") has the full list and the reason for each.

A step that raises an exception fails without stopping its worker: the same Python process
runs the next step, or the retry, so module-level state carries over from a failed attempt. A
worker is replaced by a fresh one only when the step called `sys.exit()`, raised `MemoryError`,
was still running at its `timeout_seconds`, failed while its module was imported, or crashed
the interpreter.

These are the only options: any other is an error ("Accepted arguments" below).

## Accepted arguments

These are all the arguments barca's decorators and helpers take:

| Call | Positional arguments | Keyword arguments |
|---|---|---|
| `group()` | one | `members`, `output`, `description` |
| `@asset` | none | `name`, `inputs`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env`, `weight` |
| `@sensor` | none | `name`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env`, `weight` |
| `@task` | none | `name`, `inputs`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags`, `env`, `weight` |
| `@sink` | one | `serializer`, `partition_by`, `rows_per_file` |
| `partitions()` | one | none |
| `partitions_from()` | one | none |
| `collect()` | one | none |
| `asset_ref()` | one | none |
| `Schedule()` | one | `max_failures` |

Every argument in the table does something, on each decorator it is listed for: on `@task` and
`@sensor`, `partitions=` runs the function once per key and `serializer=` forces the format its
value is stored in, as on `@asset`.

Any other argument is an error when barca reads the file, before anything runs: exit 2, `kind`
`usage`. Up to 0.18.1 such an argument was ignored without a message, so `@asset(after=other)`
or a misspelt `input=` planned, ran and did nothing. Only arguments that had no effect are
rejected: a pipeline whose arguments all did something plans exactly as before.

After `from barca import asset`:

```python
@asset(input={"raw": raw})          # `input` for `inputs`
def report(raw: dict) -> int:
    return raw["n"]
```

```
$ barca list pipeline.py --pretty
Parse error: pipeline.py:report (line 9): `input` is not an argument of @asset. Did you mean `inputs`? @asset accepts: name, inputs, partitions, serializer, freshness, timeout_seconds, retries, retry_backoff, description, tags, env, weight
Rename `input` to `inputs`, or remove it. See `barca docs assets`.
```

In JSON mode the first line is the envelope's `error` and the second its `remediation`
(`barca docs contract`).

- The error names the node (`file.py:function`), the line of the argument, the argument and
  everything the call accepts. "Did you mean" names the closest accepted argument within one
  edit of what was written (two edits for a name longer than four characters), or two that
  tie: `input`, `partition`, `serialiser`. `after` or `when` get the list and no guess. It is
  the rule every mistyped name follows (`barca docs agents`).
- The helpers and `@sink` take exactly one argument by position. Passing it by keyword
  (`partitions(values=[...])`, `collect(asset_fn=x)`, `Schedule(cron="...")`,
  `@sink(path="...")`) is rejected, and the error says to pass the value as the first argument.
  Before, these were read as "no keys", "no input", "an empty cron" and "no sink". A call with
  none or with two (`@sink()`, `partitions(a, b)`, `@sink("out.txt", "json")`: write
  `serializer="json"`) is rejected too; Python refuses those calls as well.
- `@sink`'s `partition_by=` (a list of column names) and `rows_per_file=` (a whole number)
  apply to a dataset sink, a path ending in `/`; on a file sink, or written as anything but
  literals, they are rejected when the file is read, exit 2 (`barca docs sinks`).
- `@asset`, `@sensor` and `@task` take keyword arguments only. `@asset(**options)`,
  `@asset(*args)` and `@asset("x")` are rejected: barca reads the arguments from the source
  without running it and cannot see what a `**` or `*` holds.
- `@sensor(inputs=...)` keeps its own error (`sensor '...' cannot have inputs`, exit 2).
- **Which commands fail.** The check is made per file, like the check for a syntax error: a
  command fails if it reads a file with such an argument, whatever its target. `list`, `plan`,
  `status`, `get`, `run` (with or without `--dry-run`), `stats` and `sql` exit 2 and run
  nothing. With no file arguments barca reads every pipeline file of the project
  (`barca docs discovery`), so one such argument anywhere stops all of them until it is
  corrected; `barca get report good.py` reads `good.py` only. `barca serve` starts, and
  answers `400` with the same message on the routes that read the pipeline.
- Only the first such argument in a file is reported. Fix it and run the command again.
- The Python signatures in `barca/__init__.py` take the same arguments and no others, so a
  type checker and an IDE flag the argument, and importing the module (`python pipeline.py`)
  raises `TypeError: asset() got an unexpected keyword argument 'after'`.

Not checked:

- A decorator or helper that is not positively Barca's. Qualified and aliased top-level
  Barca imports are checked. Assignments, foreign imports, `def`, `class`, loop or `with`
  targets and `global` writes disable recognition of the affected name. A later star import
  disables recognition only when its exports are unknown or include that local name.
  Celery's or Prefect's `@task(bind=True)` stays untouched. Namespace reflection retains
  conservative discovery but disables argument checks.
- What a helper receives through `*` or `**` (`partitions(*keys)`).
- The values. `inputs=` that is not a dict literal (`inputs=dict(raw=raw)`,
  `inputs=make_inputs()`) is still read as "no inputs", without a message. `env=` is the
  exception: anything but a list of string literals is an error.
- `parallel()` and `parallel_map()`: their arguments are your own.

## Inputs and parameters

Barca calls a step with one keyword argument per `inputs=` key and per partition dimension. Both
sides are in the source, so a mismatch is an error when barca reads the file, before any step
runs: exit 2, `kind` `usage`, on the same commands and with the same scope as an argument barca
does not define (above).

- An `inputs=` key (or a `partitions=` dimension) that is not a parameter of the function:

  ```python
  @asset(inputs={"row": raw})        # the parameter is `rows`
  def amounts(rows: list) -> list:
      return rows
  ```

  ```
  Parse error: pipeline.py:amounts (line 9): the inputs= key `row` is not a parameter: amounts() takes `rows`. Did you mean `rows`?
  Rename the key to `rows`, or add a parameter `row` to amounts(). Barca passes each inputs= key to the function as a keyword argument. See `barca docs assets`.
  ```

- A parameter with no default that is neither an `inputs=` key, a partition dimension nor the
  upstream of a `partitions_from(...)` (passed under its name, `barca docs partitions`). The
  line is the parameter's:

  ```
  Parse error: pipeline.py:amounts (line 10): the parameter `rows` of amounts() has no input and no default: barca passes it no arguments, so calling it would fail.
  Wire it with `inputs={"rows": <upstream>}`, give it a default (`rows=...`), or remove it. See `barca docs assets`.
  ```

- A function with `**kwargs` takes any key; a parameter with a default may be left unfilled.
- A `@task`'s parameters are not reported missing: a task with parameters is how a `parallel()`
  branch receives its arguments (`barca docs tasks`). Its `inputs=` keys are checked.
- When `inputs=` or `partitions=` is not a dict literal with string keys, which arguments
  arrive cannot be known, and no parameter is reported missing.

Up to 0.28 both mistakes failed only when the step ran, as a `TypeError` from the worker after
every upstream had run.

## Environment variables: `env=`

An asset that reads an environment variable (a source path, a region, a model name) should
declare it. Barca reads the declared variables when it plans the run (from its environment, else from the
project's `.env`), folds each name and value
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
- A declared variable that is not set in barca's environment is read from the project's `.env`
  (in the project root, next to `barca.toml`), so a pipeline that loads `.env` at import
  (`load_dotenv()`, before a settings class reads the environment) is hashed with the value it
  runs with: editing `SOURCE_CSV=b.csv` in `.env` re-runs `raw` like an `export` does. The
  environment wins over the file, as with `load_dotenv()`'s default. Barca reads the file
  without importing anything, understands `KEY=VALUE` lines, `export `, `#` comments, single and
  double quotes and `${NAME}` / `${NAME:-default}`, and does not set its values for the step:
  a pipeline that uses them loads the file itself. Another dotenv file (`.env.local`, a path
  given to `load_dotenv(...)`) is not read; declare such variables by exporting them.
- The JSON result's `steps` entry for the node carries `"env": {"API_TOKEN": null,
  "SOURCE_CSV": "b.csv"}` (`null` = unset), and `--agent` step lines end with
  `env API_TOKEN=<unset> SOURCE_CSV=b.csv`.
- Names ending in `_TOKEN`, `_SECRET`, `_KEY` or `_PASSWORD` (any case, or the bare word) are
  hashed like any other but shown as `<redacted>` in all output.
- `barca list` shows the declared names in an ENV column (`env` in `--json`).
- `env=` must be a literal list of string literals; anything else (a variable, a tuple, a
  computed name) is a parse error. It is also accepted on `@task` and `@sensor`, which always
  run: there it only records the values used.

**Undeclared reads are reported, not hashed.** A variable your code reads without declaring it
is not part of the cache key, so changing it does not re-run anything. Barca finds such reads
statically, in the function and in the project helpers it reaches (`os.environ["NAME"]`,
`os.environ.get("NAME")`, `os.getenv("NAME")`, pydantic settings fields), and prints one
line for the command before anything runs:

```text
[barca] warning: 1 step reads environment variables not declared in env=: REGION; barca status lists them
```

The JSON result lists them per step as `undeclared_env` (also in `--dry-run` and in
`barca status --json`), and the `warnings` array has one `undeclared_env` item per variable.
Declare the variable to make it count, or ignore the warning if the value never changes the
result. `barca docs cache`, "Environment variables", has the exact rules and what is not seen.

## Heavy steps: `weight=`

Experimental. Each step runs in its own worker process, and independent steps run at the same
time, up to one per worker. Under a hard memory limit (a container's `mem_limit`), several heavy
steps that become ready together can add up past it, and the kernel kills one: the step fails
with `worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)`. The same step
succeeds alone. `weight=` limits how many such steps run at once without slowing the rest.

```python
from barca import asset


@asset(weight=2)
def embed_a() -> dict:
    return {"rows": 1}


@asset(weight=2)
def embed_b() -> dict:
    return {"rows": 2}


@asset(weight=2)
def embed_c() -> dict:
    return {"rows": 3}
```

- The weights of the steps running at once add up to at most the **capacity**. It is the pool
  size (available cores, or `BARCA_POOL_SIZE`) unless `[execution] capacity` in barca.toml or
  `BARCA_CAPACITY` sets it; the environment variable wins. With a capacity of 4, the three steps
  above run two at a time, and two weight-1 steps can run beside one of them.
- `weight=` is a whole number of at least 1; anything else is a parse error (exit 2). It is
  accepted on `@asset`, `@task` and `@sensor`. It is not part of the run hash: changing it
  re-runs nothing and outputs and cache decisions are the same at any weight or capacity.
- A step whose weight is above the capacity runs alone, with nothing beside it, rather than
  never. Every command that plans it prints a warning on stderr:
  `[barca] warning: 'embed_a' has weight=8, more than the capacity 4 (pool size); it runs alone`.
- Steps start in the order they become ready. A heavy step waiting for room is not overtaken:
  steps that become ready after it wait until it has started, so a stream of light steps
  cannot hold it back for the rest of the run.
- Every partition key of a partitioned node takes the node's weight, and so does every
  `parallel()` branch, which takes the weight of the step that called `parallel()`. While that
  step waits for its branches it is not counted (the branches carry its weight), so its own
  memory is not limited then.
- The capacity is per run: each run has its own pool of workers. Two runs at the same time
  (two `barca get` commands, or `barca serve` running two schedules at once) each fill their
  own capacity.
- `barca plan --json` shows `capacity`, `capacity_source` and `weights` (the planned nodes with a
  weight above 1); `--dry-run` shows `capacity` and a `weight` on each such step; `barca doctor`
  has a `capacity` check.

```bash
barca plan pipeline.py
barca get pipeline.py --dry-run --json
barca doctor pipeline.py
```

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

Static extraction reads source without importing user code. Literal partition key lists
need no resolver import; a nonliteral `partitions(<expression>)` imports the pipeline
module to evaluate its keys. The decorators, `inputs=` and `freshness=` must be written
literally enough for barca to read them from the source. Dynamic decorator construction
(building `inputs` in a loop, calling a decorator through a variable) is not visible to the
planner. An argument barca does not define, and `**options` in a decorator call, are errors
("Accepted arguments" above). Mark code barca cannot reason about with `@unsafe` (silences purity warnings only).

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

When a local name shadows an imported module, calls through that name may resolve SQL or
expression inputs dynamically. This includes names captured by `match` and names bound by
nested `def`, `async def` or `class` definitions. Such calls are treated conservatively;
a module import alone does not prove the receiver still refers to that module.

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

`@sensor` takes the arguments in the table under "Accepted arguments" (`@asset`'s without
`inputs`). `@sensor` observes external state and returns `(update_detected: bool, value)`. Sensors have no
inputs. Their default freshness is `Manual`; `Always` is also accepted. Only
`Schedule(...)` gives a sensor ticks of its own under `barca serve`, and a tick also updates the
`Always` nodes below it (`barca docs scheduling`). Whatever its freshness, a sensor is polled
whenever a run needs its value: `Manual` does not pin a sensor.

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

Explicit references to `globals`, `locals`, `vars`, `exec`, `eval` or `__builtins__` make imported barca
names uncertain, even when the reference is aliased, qualified or shadowed. Such names are
left to Python's argument checks and their nodes conservatively hash the whole module
(`barca docs cache`). The static checks do not resolve arbitrary reflective indirection
or runtime side effects.

See also: `barca docs tasks`, `barca docs cache`, `barca docs scheduling`.
