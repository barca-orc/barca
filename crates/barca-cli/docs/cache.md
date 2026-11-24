# Caching, artifacts and environments

Barca skips a step when it has already computed the same thing. Below: what "the
same" means (the run hash), what the hash does and does not see, the states a cached result
can be in, incremental runs and environments. Forcing a recompute and `--dry-run` are in
`barca docs refresh`; data that changes in place and `cutoff=True` are in `barca docs external-data`.

## At a glance

| What changed | What runs again |
|---|---|
| Nothing | No asset: each is served from cache. Sensors and tasks in the run still run |
| An asset's code, or helper code it calls | That asset and everything downstream of it |
| A value a `@sensor` returns | The assets that read the sensor, and everything downstream |
| A variable listed in the asset's `env=[...]` | That asset and everything downstream |
| `inputs=`, `serializer=`, a partition dimension name or `partitions_from` source, a `@sink(...)`, or another decorator wrapping the function | That asset and everything downstream |
| `retries=`, `timeout_seconds=`, `freshness=` or another option that does not change the result, or formatting | Nothing (`barca docs hashing`, "Which decorator arguments count") |
| A file, table or bucket the asset reads in its own body, or an environment variable it does not list | Nothing: barca cannot see it. Read it through a `@sensor` (`barca docs external-data`) or list it in `env=` |
| A new partition key | That key's steps, and any `collect()` of the asset |
| An `@asset(cutoff=True)` reran and wrote the same bytes | Nothing below it (`barca docs external-data`, "Early cutoff") |
| Nothing, but you want a recompute | `--refresh name` reruns it and what is downstream; `--refresh-all` reruns the cone (`barca docs refresh`) |

Tasks and sensors are never served from cache: they run every time they are in a run.
`barca status` says which state each node is in and why, and `--dry-run` shows what would run.
Both decide by the rules a run follows: a `Manual` or pinned node is read as it is, so what is
below one is `cached` while it is up to date against that node's current result, and the
`Manual` node itself reads `stale` once its inputs move on (`barca docs status`, "Cache state").

## What is cached

Each asset step has a **run hash**: a hash of the function's definition, its inputs' run hashes,
the output of any sensor or `Manual` asset it reads (and, in place of the run hash, the output
of an `@asset(cutoff=True)` it reads: `barca docs external-data`, "Early cutoff"), for partitioned assets the partition key,
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
  variable in `param` (`barca docs contract`, "Plan warnings").
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
decorator arguments count, and what is not followed, are in `barca docs hashing`.

## Cached, stale, evicted, unsynced

A result is a row in `.barca/metadata.db` (the run hash it was computed under) plus an artifact
file. `barca status` names the state each node is in; the table of states and reasons is owned
by `barca docs status`, "Cache state". In short: `cached` is served; `stale` runs again, because
the run hash moved (`changed`, `upstream_stale`), the last attempt failed (`failed`), or the
recorded file is gone and something needs to read it (`artifact_missing`, or `evicted` when the
cache size limit deleted it, `barca docs disk`; a copy the artifact store holds is downloaded instead
and is not stale); `unsynced` is cached but not where it should be, and the next `get` repairs
that without running a step (`barca docs remote`, `barca docs server-mode`); `blocked` keys wait
on failed keys of an input (`barca docs partitions`). A missing or evicted artifact that nothing
needs to read leaves the node `cached` (`barca docs artifacts`, "A cached result whose artifact is missing").

Store availability reported by inspection can reuse a cloud observation for five minutes.
Local file and plain-directory-store identity changes invalidate digest evidence immediately.
Inspection shape/digest evidence is bounded separately from the ledger; required reads and
ledger publication still verify directly (`barca docs remote`, "Inspection cost and observation window").

## What a run reports, and missing artifacts

What `get` and `run` report per step (status, reason, timing, byte checks against an earlier
result), and what happens when a cached result's file is gone, are in `barca docs artifacts`.

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

`.barca/` has an eviction target (`[cache] max_size`, default 10% of its filesystem) and `barca gc`
works toward it after every run; what counts, what is evicted in which order and how the
metadata database keeps to its share are in `barca docs disk`.

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
`barca docs layout`.

## Read completed results without running

`barca get total pipeline.py --cached-only` reads the latest successful materialization
recorded in the selected environment, even when stale. It does not import pipeline source,
execute sensors/assets/tasks, enqueue or record a run, publish results, retry pending uploads,
or consult a server's remote ledger. Ordinary `get` is unchanged.

Lookup is disk, then the configured artifact store, then a typed miss. Use
`barca get total pipeline.py --cached-only=local` for disk only: no store calls, including
metadata probes. `--local` still selects this machine; `--server` asks the server to perform
these same tiers on its own records, disk and store. The cache-only tier is independent of
where the command runs. `barca.get("total", "pipeline.py", cached_only=True)` returns the
value; `cached_only="local"` chooses the strict tier.

JSON contains `outcomes` for every selected node/key and `final_output` in the requested
output mode. A hit reports `node`, `partition` (null when unpartitioned), `run_hash`, `format`,
`size_bytes`, `path`, `fresh`, and `source` (`disk` or `store`). Freshness uses static source
and recorded inputs, not live sensor values. Failed/in-progress attempts never replace the
latest success. A miss reports `status: "miss"` and `reason`: `never_run`, `evicted`, or
`not_fetched` when local-only excluded a recorded store copy. Any miss fails the whole call:
exit 1, stderr kind `cache_miss`, and Python `barca.CacheMiss` with `outcomes` and `missing`.
No automatic fallback starts work. Authorization, transport, filesystem, size and hash errors
are infra failures (exit 3), never misses. An absent object in a reachable store is evicted.

`--partition` selects only those keys, including explicit keys with no recorded success.
Literal domains can discover never-run keys; dynamic expressions are not evaluated. Without
complete explicit coordinates, dynamic domains use only recorded keys. An undiscoverable
empty domain requires explicit coordinates. A selection never starts work to discover keys.

Reads retain a completed inode in process-owned staging while validating and reading it;
GC protects that active read for the process lifetime, including reads longer than five minutes.
Across filesystems, a private copy from one open source handle replaces the hard link. Path output names an immutable
local cache reference under `.barca/sql-cache/`, subject to GC after its five-minute read grace;
it is not a permanent lease. `path_scope: "server"` identifies paths on the server, which a
remote caller may not access. Use `-o value` for remote JSON values or `-o url` for a signed
cloud object URL. URL output never uploads missing objects and cannot be combined with
`--cached-only=local`, since signing can contact storage. Both tiers update last-read times;
fetched files and retained references count toward the cache's disk budget.

Cache-only conflicts with refresh, dry-run, batch, queue and allow-paused flags. It does not
honor pins as an alternative version: its answer is always the latest success. Content digests
in the answer, bounded remote binary access and results recorded only by another machine
remain outside this first version.

Cached value access accepts JSON/parquet/CSV and pickle representations made only of plain
Python data. Pickle globals, custom classes, reducers, extension/persistent references and
reconstruction hooks are refused before unpickling, with infra reason
`cached_value_unsupported`. This includes library-class pickles that need reconstruction;
use an artifact path or a data format such as parquet. Path access still returns the verified
artifact. No fallback imports project source or runs a normal get. Ordinary get's pickle
reader is unchanged.
### Refreshes across a server restart

`--refresh-all` and `--refresh` require a matching successful materialization recorded at or
after the request was accepted. The acceptance time survives a drain/restart: completed
assets and partition keys are reused, including results another run computed after acceptance.
A new request gets a new acceptance time. Older results or changed run hashes still compute;
selective cascade and cutoff rules are unchanged. A fully cached step satisfied by that
boundary has reason `refreshed_by_this_run`. Tasks outside ticks can repeat, including side
effects; this is not an exactly-once guarantee. See `barca docs serve-ops`.


### Failed refreshes and cache-aware retries

Before a refresh executes, successful results older than its acceptance time in the planned
refresh scope become ineligible for ordinary cache reuse. This includes downstream steps
that a failure prevents from running. A cache-aware retry therefore computes the missing
results from the refreshed inputs instead of reviving pre-refresh successes, even after a
restart or another failed retry. New successes remain reusable.

Partition selection, cutoff boundaries, held nodes and explicit pins still constrain the
scope. `--no-cascade` and consumers outside the requested cone remain unchanged. Historical
rows stay available for inspection and explicit pins. Against a remote ledger, the server
must acknowledge invalidation before computation starts; upgrade an older server that does
not support it. Ordinary cache reads do not need this handshake.

See also: `barca docs disk`, `barca docs layout`, `barca docs remote`, `barca docs types`,
`barca docs partitions`, `barca docs status`, `barca docs server-mode`, `barca docs contract`.
