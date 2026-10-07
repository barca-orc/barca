# barca status

One read-only view of every node: what it is, whether it is cached and why, when it last ran,
and what its artifact looks like. It answers in one call what would otherwise take `barca list`,
`--dry-run`, `barca history` and a script that opens the artifact.

```python
import pandas as pd
from barca import asset, task


@asset()
def orders() -> pd.DataFrame:
    return pd.DataFrame({"id": [1, 2, 3], "amount": [9.5, 3.0, 12.25]})


@asset(inputs={"df": orders})
def total(df: pd.DataFrame) -> dict:
    return {"total": float(df["amount"].sum())}


@task(inputs={"t": total})
def notify(t: dict) -> None:
    print(t)
```

```bash
barca get total pipeline.py
barca status pipeline.py --pretty               # table (the default in a terminal)
barca status total pipeline.py                  # only `total` and its upstream cone
barca status total,notify pipeline.py           # several targets: the union of their cones
barca status pipeline.py --json                 # one JSON document (the default when piped)
barca status pipeline.py --fields id,cache      # JSON with only these keys per node
barca status pipeline.py --json --sample 2      # plus up to 2 sample rows per json/parquet artifact
```

```
NAME    KIND   STATE        WHY           LAST RUN                           SHAPE            DEPS
orders  asset  cached       materialized  success 2026-10-02 11:56:43 0.54s  3 rows x 2 cols  -
total   asset  cached       materialized  success 2026-10-02 11:56:43 0.00s  dict (1 key)     orders
notify  task   always-runs  task          -                                  -                total

2 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 1 always run
```

Status never imports your code and never writes: no `.barca` directory is created, no run is
recorded. Like every inspection command, the result goes to stdout (a table in a terminal, JSON
when piped; `--json` / `--pretty` override) and errors to stderr; an unknown target is a usage
error (exit 2) that ends with ``Run `barca list <files>` to see available assets and tasks.``,
the same remediation as `get` and `run`. A target is one name or several, comma-separated with no
spaces (`a,b`), parsed exactly as `get` and `run` parse them; the JSON names them in `targets`
(and the single one in `target`).

With shared remote state (`barca docs remote`), status first pulls the shared history, as a run
does, so it also shows what other machines computed. A pull keeps what was recorded only on this
machine, so this is safe while a run is going: its finished steps still show.

Status reads the metadata DB as it is at that moment, and a run records each step as it
finishes. So while a `barca get` is running, status in another terminal already shows the steps
it has finished as `cached` (a partitioned asset as `partial`, with counts); see `barca docs
cache`, "While a run is going, and after one is killed".

Like `list`, status shows at most 100 nodes unless you pass `--limit N` or `--all`. The
`summary` still counts every node, and the JSON adds `total` and `truncated` (with a `hint` when
truncated). Each node also lists `env`, the environment variables it declares with `env=[...]`
(`barca docs assets`). `--fields` keeps only the named keys on each node.

## Cache state

`cache.state` is the same decision `barca get --dry-run` makes (both call one function), so the
two cannot disagree. In JSON the states are snake_case, spelled exactly like the `summary` keys
(`never_run`, `always_runs`); the table prints them as `never-run` and `always-runs`.

| state | meaning | `reason` |
|---|---|---|
| `cached` | a successful result matches this code and these inputs; `get` serves it | `materialized` |
| `stale` | it ran before, but `get` would run it again | `changed`, `upstream_stale`, `failed` |
| `never_run` | no successful materialization is recorded | `no_record`, `failed` |
| `partial` | a partitioned asset with some keys cached | `partitions_missing` |
| `unknown` | dynamic partitions (`partitions_from`) whose source has not run yet, or an asset reading a sensor with no recorded output (and what depends on either) | `partitions_unknown`, `sensor_output_unknown` |
| `always_runs` | tasks and sensors are never cached | `task`, `sensor` |

- `changed`: the run hash differs from the last materialization, because this function's code or
  its upstream outputs changed. barca records the combined run hash, not the two parts, so it
  cannot say which.
- A node that reads a `@sensor` is judged by the sensor's last recorded output: status runs
  nothing, so it assumes the sensor returns the same value next time, and `detail` says so. If
  the sensor recorded a new value since the node last ran (for example a scheduled sensor), the
  node shows as `stale` (`barca docs cache`, "External data that changes in place").
- `upstream_stale`: an input is itself not cached, so this node's inputs will change when it runs.
  `detail` names the input.
- `failed`: the last attempt at exactly this code and these inputs raised; `detail` carries the
  message.

`cache.run_hash` is the cache key for the current code and inputs; `cache.artifact` is the file a
`get` would serve (only when `cached`).

## Last materialization

`last_materialization` is the most recent execution recorded in the metadata DB, successful or
failed: `status`, `created_at` (UTC), `elapsed_seconds`, `run_hash`, `artifact`, `format`,
`size_bytes`, and `error` for a failure. A cache hit is not an execution, so serving from cache
does not change it. It is `null` when the node never ran.

## Artifact shape

`shape` describes the artifact of `last_materialization` when that run succeeded (`null`
otherwise). It is read from the file alone by a small Python helper; your modules are never
imported.

| format | shape |
|---|---|
| parquet | `type: "table"`, `rows`, `columns: [{name, type}]` (arrow types) from the file footer |
| json list | `type: "list"`, `rows`; for a list of objects also `columns` with the JSON types seen (`str \| null`) |
| json object | `type: "dict"`, `keys` (first 100; `key_count` when there are more) |
| other json | `type` (`int`, `str`, ...) |
| pickle | `type` only, e.g. `myproject.Model`, read from the pickle opcodes without unpickling |

`--sample N` adds `sample`: the first N rows (parquet, json list), the first N entries (json
object), or the value (other json). It is off by default to keep output small. Pickles are never
sampled: loading one would import and run code.

When the shape cannot be read, `shape` has a `note` instead of `rows`/`columns`: parquet without
pyarrow installed (`pip install 'barca[parquet]'`), a file that no longer exists, or a remote
artifact that is too large or cannot be reached (below).

### Remote artifacts

With remote storage (`barca docs remote`) the shape is read from the bucket, through the same
filesystem, credentials and `[remote.storage_options.*]` the steps use. There is no flag for it:

```bash
barca status total --json --sample 2    # rows, columns and 2 sample rows, read from the bucket
```

- parquet: `rows` and `columns` come from the file footer by ranged requests; the object is not
  downloaded. `--sample N` also reads the first row group, not the whole file. Measured on a
  160 MB file of 20 row groups in S3-compatible storage: 64 KB read for the shape, 8 to 9 MB with
  `--sample 5`.
- json and pickle have to be downloaded to be described, so only objects up to 16 MB are. A
  larger one has `"note": "remote json artifact too large to inspect: 25.1 MB (limit 16 MB)"`.
  `barca sql` can still query a large json result.

Each node with a result costs one or two requests, and up to 8 artifacts are read at a time.
`barca status` with no target reads every node; name a target to read only its upstream cone.

A store that cannot be read never fails the command. A missing driver
(`pip install 'barca[s3]'`), rejected credentials or a network error is the `note` of each
shape, the rest of the status is complete, and the exit code is 0. After the first such failure
the artifacts not yet read report it without another attempt, so an unreachable bucket costs one
wait for the driver's retries (about 10 seconds for an S3 endpoint that refuses connections),
not one per node.

## Partitioned assets

A partitioned asset is one node with a `partitions` summary: `total`, `cached`, `missing` and up
to 20 `missing_keys`. Its state is `cached` when every key is, `partial` when some are, and
`stale`/`never_run` when none are. `last_materialization` is the most recent key that ran, named
in its `partition` field (for example `k=a`), and `shape` describes that one key's artifact.

## JSON

```json
{
  "target": "total",
  "targets": ["total"],
  "nodes": [
    {
      "id": "pipeline.py:orders",
      "name": "orders",
      "kind": "asset",
      "inputs": [],
      "partitioned": false,
      "cache": {"state": "cached", "reason": "materialized", "detail": "...", "run_hash": "6db9...", "artifact": ".barca/artifacts/pipeline.py--orders/6db9....parquet"},
      "last_materialization": {"status": "success", "created_at": "2026-10-02 11:56:43", "elapsed_seconds": 0.54, "run_hash": "6db9...", "artifact": ".barca/artifacts/pipeline.py--orders/6db9....parquet", "format": "parquet", "size_bytes": 2182},
      "shape": {"type": "table", "rows": 3, "columns": [{"name": "id", "type": "int64"}, {"name": "amount", "type": "double"}]}
    }
  ],
  "summary": {"cached": 2, "stale": 0, "never_run": 0, "partial": 0, "unknown": 0, "always_runs": 0}
}
```

Keys inside a `sample` row are printed in alphabetical order; `columns` keeps the file's column
order. `--env <name>` reads another environment's state (`barca docs cache`).
