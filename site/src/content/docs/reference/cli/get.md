---
title: "barca get"
description: "Get assets, from cache when nothing changed; prints where their results are."
---

Compute one or more assets and print where their results are. Only the target and what is
upstream of it are considered, and a step whose code and inputs are unchanged is served from
cache.

## Usage

```txt wrap=false
barca get [target[,target...]] [file.py|dir/ ...]
          [--refresh a,b [--no-cascade] | --refresh-all]
```

## Flags

| Flag | Meaning |
|---|---|
| `--refresh a,b` | Recompute the named assets (one comma-separated list) and everything downstream of them in the target's cone. The target itself may be named. |
| `--no-cascade` | With `--refresh`: recompute only the named assets. Downstream assets stay cached and do not reflect the refresh; barca warns. |
| `--refresh-all` | Recompute every asset in the target's cone. |
| `--dry-run` | Report each step as cached or to run, and why, without running or writing anything. |
| `--json`, `--pretty` | Force JSON or human output. |
| `-o path` | Print only the artifact paths, one per line (one per partition key). With `--json`, the default JSON. |
| `-o value` | Print only the value. With `--json`, the JSON result with the value in `final_output`. |
| `-o json\|pretty` | Older spellings of `--json` / `--pretty`. |
| `--partition KEY=V[,V]` | Select partition keys across the cone (a JSON object for compound keys): every partitioned node with that dimension computes only them, and fan-ins over them are skipped. Works without a target. |
| `--agent` | Plain progress lines on stderr in place of the progress bar. |
| `--fields a,b` | Keep only these keys on each entry of `steps`: `id`, `kind`, `action`, `status`, `reason`, `detail`, `changed`, `run_hash`, `artifact`, `warning`, `artifact_mismatch`, `bytes_changed`, `elapsed_seconds`, `attempts`, `import_seconds`, `partitions`, `env`, `undeclared_env`, `weight`, `batch`, `batch_execution`. |
| `--env <name>` | Use a named environment's cache and history. |
| `-o url`, `--expires <DURATION>` | Print a signed HTTPS link to each result in a cloud store, valid for `--expires` (default `1h`, at most `7d`). Experimental. |
| `--batch <[NODE=]none\|all\|N>` | Override how keys are grouped into calls for nodes that declare `batch=` ([Batched Partitions](/reference/batched-partitions/)). |
| `--allow-paused` | Run a target that `barca pause` paused ([Pausing Assets](/reference/pausing/)). |
| `--server [URL]`, `--local` | Run on a `barca serve` (its deployed code), or ignore a configured server ([Server Mode](/reference/server-mode/)). |


## Example

```
$ barca get summary pipeline.py --json
[barca] 2/2 steps | done in 0.0s
{"elapsed_seconds":0.08844638,"final_output":{"_barca_artifact":{"format":"json","path":".barca/artifacts/pipeline.py--summary/2d10....json","size_bytes":24}},"phases":1,"run_id":"14f3f967c1b6","status":"success","steps":[{"attempts":1,"detail":"no cached result for this code and these inputs","elapsed_seconds":0.0023,"id":"pipeline.py:raw_data","kind":"asset","reason":"not_materialized","run_hash":"a8e7...","status":"ran"},{"attempts":1,"detail":"no cached result for this code and these inputs","elapsed_seconds":0.0007,"id":"pipeline.py:summary","kind":"asset","reason":"not_materialized","run_hash":"2d10...","status":"ran"}],"steps_executed":2,"warnings":[]}
```

The first line is progress, on stderr. The JSON is one line on stdout (hashes shortened here).
`final_output` is a pointer to the artifact, `{"_barca_artifact": {"path", "format",
"size_bytes"}}`, whatever its format. Run it again and every step is `"status":"cached"`, with
its `artifact` path, and `"steps_executed":0`. In a terminal the same run prints a summary and
the path, with a small json value (up to 10 lines) previewed under it:

```
$ barca get summary pipeline.py
Run 14f389a807a2 | got 'summary' in 0.050s (0 steps, 1 phase)

.barca/artifacts/pipeline.py--summary/2d10....json
  {
    "count": 3,
    "total": 6
  }
```

## Reading the result


```bash
barca get summary pipeline.py -o value           # only the value, pretty-printed
barca get summary pipeline.py -o value --json    # the JSON result with the value in final_output
barca get summary pipeline.py -o path | xargs cat   # only the path, one line per artifact
barca get summary pipeline.py -o url --expires 12h  # a signed HTTPS link to the result in the store
```

`-o value` reads json, parquet, CSV and pickle with the reader `barca.get()` uses; a table
prints as a list of row objects. `-o path` is the default spelled out: with `--json` it prints
the default JSON. `-o url` (experimental) prints a signed HTTPS link to
each result in a cloud store, valid for `--expires` (default `1h`, at most `7d`); with `--json`
each `_barca_artifact` pointer gains `url` and `url_expires_at`. See
[Remote storage: signed URLs](/reference/remote-storage/#sharing-a-result-signed-urls).

## Partitioned targets

A partitioned asset returns every key, sorted: `final_output` is an
object keyed by partition key (`"region=emea"`; a compound key is `"day=d1,region=emea"`), each a
pointer, or a value under `-o value`, and `-o path` prints one line per key.
`--partition region=emea,amer` selects keys across the whole run: every partitioned node with
that dimension, upstream of the target and downstream, computes only those keys, they are
returned, and `--refresh` refreshes only them. A JSON object selects on several dimensions,
`--partition '{"region": "emea", "day": ["d1", "d2"]}'`. A selected key has the same run hash as
in a full run, so a later full run serves it from cache. A `collect()` fan-in over a narrowed node
(and what is below it) is not run: status `skipped`, reason `partition_selection`, one note on
stderr, and the exit code is unchanged. With no target the selection applies to every asset and
sensor; `barca run` and `barca plan` take it too. JSON gains `partition_selection` (the
selection, each narrowed node's `selected` and `skipped` key counts, `skipped_nodes`). A
dimension no node in the cone has, a value no node with that dimension has, or a target the
selection would skip is a usage error (exit 2). See [Parametrized Assets and Partitions](/examples/partitions/#selecting-keys-with---partition). When some keys fail the run
fails (exit 1, `failed_node` names the key) and `final_output` in the result line holds the
keys that succeeded; how a partial failure is reported may still change
([Partitions](/reference/partitions/)).

## Many partition keys

A run lists at most 1000 keys (0.30.2). When a partitioned target returns more and no
`--partition` selected them, `final_output` lists the keys this run computed and counts the
ones it served from cache, so a tick that recomputed one key of 200k returns one path:

```json
{"_barca_partitions": {"keys": 200000, "computed": 1, "cached": 199999, "truncated": false,
  "outputs": {"retailer=r000042": {"_barca_artifact": {"path": "...", "format": "json", "size_bytes": 49}}},
  "hint": "..."}}
```

`outputs` holds pointers, values under `-o value` or links under `-o url`, for at most 1000
computed keys (`truncated` when a cold run computed more). `-o path` and the table print those
paths and a note on stderr. `--partition` returns every key it selects, however many, and
`barca sql` reads every current key's rows. `barca.get()` raises for such a target; pass
`partition=`. The same object is the outcome `barca serve` records for the run.

## Without a target

With no target (`barca get pipeline.py`, or `barca get`), barca gets every asset and sensor and
returns the last asset's artifact. Tasks are skipped, and stderr names them and the `barca run`
command. A sensor nothing depends on still runs. A file with only tasks gets nothing: exit 0,
`"steps": []`, and a stderr note pointing at `barca run`.

## Usage errors

Every `get`/`run` usage error exits 2. An unknown or ambiguous target (and `run` without one)
ends with ``Run `barca list <files>` to see available assets and tasks.``; the others state
their own fix: the corrected command for the wrong order, `barca run` for a task given to
`get`, the valid names for an unknown `--refresh` name. A name close to one barca knows
(a target, a `--refresh` name, a flag or a subcommand) is preceded by `Did you mean '...'?`: the
best match within one edit (two for a name longer than four characters), or two that tie. The
exit code is still 2 and nothing runs.



## Refreshing selected keys

To refresh a key or set while retaining the complete output domain:

```bash
barca get final pipeline.py --refresh 'source[account=A],source[account=C]'
```

Each selector names one dimension and value; comma-separated selectors select a set.
Only those source keys and their affected downstream keys refresh. Fresh siblings remain
cached and invalidated `collect()` publishers read the complete domain. Cutoff bytes and
`--no-cascade` retain their usual behavior. `--partition` still narrows the DAG. Python uses
`barca.get("final", "pipeline.py", refresh=["source[account=A]", "source[account=C]"])`;
`barca.run` and the server's `refresh` request array accept the same selectors.

## Dry run

A dry run of the same target:

```
$ barca get total pipeline.py --dry-run
Dry run: barca get total (nothing executed, nothing written)

STATUS    WHY                                              STEP
will run  no cached result for this code and these inputs  pipeline.py:orders
will run  no cached result for this code and these inputs  pipeline.py:total

2 will run, 0 cached, 0 unknown
```

An asset that reads a `@sensor` is cached against the sensor's value: when the sensor returns a
new value, the asset and everything downstream of it re-run. `--dry-run` predicts from the
sensor's last recorded value and says so in `detail`; before the sensor has ever run, its
consumers are `unknown` (`reason: "sensor_output_unknown"`).

An asset that reads an `@asset(cutoff=True)` is cached against the bytes that asset wrote, not
its run hash: when it reruns and writes the same bytes, what reads it stays cached. A dry run
cannot know those bytes before it runs, so its consumers are `unknown`
(`reason: "cutoff_pending"`) until it has; see
[Early cutoff](/reference/api/decorators/#early-cutoff-cutofftrue).

Each entry in the result's `steps` array says what happened to that step. A node that declares
environment variables with `env=[...]` also carries `env`, the values the step was hashed with
(`null` when unset, `<redacted>` for names like `*_TOKEN`), and its `--agent` progress line ends
with `env NAME=value ...`. See [Decorators](/reference/api/decorators/#declared-environment-variables-env).


## Changes from earlier versions

- Up to 0.24, `final_output` was the value of a json result and a terminal printed the value;
  `-o value` prints it now. `--no-cache` was removed in 0.24.0; use `--refresh-all`.
- Up to 0.29.2, `--partition` narrowed only the target.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
