---
title: "Forcing a Recompute"
description: "barca get --refresh, --refresh-all and --no-cascade: what reruns, refreshing selected partition keys, Manual upstreams, and --dry-run."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/refresh.md
---

<!-- Generated from crates/barca-cli/docs/refresh.md (barca docs refresh). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

A step that is cached is not run again ([The Cache: At a glance](/reference/cache/#at-a-glance)). Below:
forcing one to run anyway: `--refresh` and `--refresh-all`, what they do to what is downstream,
refreshing selected partition keys, `Manual` upstreams, and `--dry-run`, which shows what a
command would run without running it.

## Commands at a glance

| Goal | Command |
|---|---|
| Normal, cache-aware | `barca get target pipeline.py` |
| Recompute everything in the cone | `barca get target pipeline.py --refresh-all` |
| Recompute chosen assets and everything downstream of them | `barca get target pipeline.py --refresh a,b` |
| Recompute only the chosen assets | `barca get target pipeline.py --refresh a,b --no-cascade` |
| Recompute some keys of a partitioned target | `barca get sales pipeline.py --partition region=emea --refresh sales` |
| Run a task, cached upstream | `barca run task pipeline.py` |
| Run a task, refresh chosen upstream assets and everything downstream of them | `barca run task pipeline.py --refresh a,b` |
| Run a task, refresh only the chosen assets | `barca run task pipeline.py --refresh a,b --no-cascade` |
| Run a task, refresh all upstream assets | `barca run task pipeline.py --refresh-all` |

`barca get` and `barca run` take the same three flags. On `get` the target is an asset, so
`--refresh` may name it too. In Python, use `barca.get(..., refresh_all=True)` or
`refresh=[...]`.

### Exactly what `--refresh` does

- `--refresh a,b` re-materializes the assets you name **and every asset downstream of them** in
  the target's cone (the cascade), so the refreshed data reaches the target. A step re-run by the
  cascade reports `reason: "refresh_cascade"` and a `detail` naming the asset it cascaded from.
- It takes one comma-separated list; `--refresh a b` is an error ("'b' is not a .py file"). A
  name that is not an asset in the target's cone is an error that lists the valid names, so a
  typo never silently does nothing.
- It does **not** rebuild the upstream of what you name, or assets in the cone that do not depend
  on it. Those keep serving from cache.
- It does **not** reach assets outside the target's cone. When a refresh writes new bytes, a
  consumer that another target reads keeps its run hash and its old result, and `barca status`
  reports it `cached`: refresh again with a target below it, or name it in `--refresh`
  ([Batched Partitions: When a consumer's keys are stale](/reference/batched-partitions/#when-a-consumers-keys-are-stale)).
- The cascade stops at an `@asset(cutoff=True)`: what reads it is decided by the bytes the
  refresh wrote ([External Data and Early Cutoff: Early cutoff](/reference/external-data/#early-cutoff-assetcutofftrue)). Same bytes, it stays cached and carries no warning; new
  bytes, it runs as `not_materialized`. Name it in `--refresh`, or use `--refresh-all`, to
  recompute it anyway.
- `--no-cascade` re-materializes **only** the assets you name. Run hashes cover definitions and
  upstream hashes, not output contents, so a cached downstream asset still matches and the
  refreshed data never reaches it. Barca prints
  `warning: 'mid' was served from cache but depends on refreshed 'src' ...` when this happens.
  `--no-cascade` without `--refresh` is a usage error (exit 2).
- With `--partition`, only the selected keys of every partitioned node with that dimension are
  planned, upstream and downstream, so `--refresh` of any of them recomputes only those keys. A
  `collect()` fan-in over them is skipped, and naming it in `--refresh` is a usage error
  ([Partitions: Selecting keys](/reference/partitions/#selecting-keys)).

### Refresh selected keys in a complete DAG

Use keyed selectors to refresh one key or a set independently of the output domain:

```bash
barca get final pipeline.py --refresh 'source[account=A]'
barca get final pipeline.py --refresh 'source[account=A],source[account=C]'
barca run publish pipeline.py --refresh 'source[account=A],source[account=C]'
```

Each selector names one dimension and value; it selects every source key with that coordinate.
Combine selectors with commas to select a set. Bare asset names still refresh all planned keys.
A malformed selector or a selector matching no planned key is a usage error. Quote brackets
so the shell does not expand them. With `--partition`, selectors must match the narrowed domain.

The full target DAG and partition domain remain planned. Only selected keys and their aligned
cascade recompute; fresh siblings keep their hashes and artifacts. An invalidated `collect()`
reads all current keys, combining refreshed keys and cached siblings. Missing or stale siblings
follow normal cache validation and artifact recovery; a failure blocks publication rather than
omitting an input. `batch="all"` receives the keys that need work. `--no-cascade` and cutoff
semantics apply as above. A cutoff whose bytes stay unchanged leaves its consumers cached;
a dry run reports consumers of a running cutoff as `unknown` / `cutoff_pending`.

`--dry-run --json` reports selected work and sibling cache counts in `partitions`, including
`will_run_keys`; `reason` distinguishes `refresh` and `refresh_cascade`. The publisher's step
reports whether the complete fan-in runs or stays cached. A real report has the corresponding
`status` and counts. A cached consumer after the refreshed cutoff reports that propagation
stopped at unchanged cutoff bytes.

Python uses `refresh=["source[account=A]", "source[account=C]"]`; POST `/runs` uses the same
strings in its `refresh` array. Both use the CLI's cache engine.

For external data that changes in place (a blob overwritten at the same path), the canonical
answer is a sensor ([External Data and Early Cutoff](/reference/external-data/)). Without one, `--refresh`
the asset that reads it with a target downstream of everything built from it: what is built from
it in that target's cone re-runs.

Never delete files under `.barca/` to force a recompute: `--refresh` also keeps the metadata DB
consistent. To look at a cached result, use `barca sql` ([barca sql](/reference/cli/sql/)).

### A `Manual` upstream is read as it is

A `Manual` asset or task (`@asset(freshness=Manual)`) is refreshed by hand only. When
`barca get` or `barca run` reads one on the way to the target, it is not recomputed, even when
its inputs or code changed: the run reads its latest successful result, and does not plan
anything above it. Its step is `cached` with a `detail` saying so (`Manual: its last result is
read as it is`, or `Manual and stale: ...`). Nodes below it are hashed against that result:
its run hash and, as for a sensor, the SHA-256 of its bytes. They are served from cache until it
changes, and once it has a new result they are stale relative to it, also when a refresh by hand
gives new bytes for the same code and inputs (a reference table that reads external data):
`barca status` reports them `stale` (reason `changed`), `--dry-run` predicts that they run, and
the next `barca get` of one of them, or the next tick that reaches them, recomputes them. A
refresh that gives the same bytes changes nothing below it. When it is stale, stderr says how to
recompute it:

```text
[barca] note: baseline is Manual and stale; barca get baseline pipeline.py --refresh baseline to recompute
```

- Only naming it in `--refresh` recomputes it: `--refresh baseline` (with the cascade,
  everything below it in the cone too) or `--refresh-all`. That holds when it is the target too:
  `barca get baseline` without `--refresh baseline` returns its current result, with the same
  note when it is stale. A `Manual` task named as the target of `barca run` runs, since
  `--refresh` names assets only.
- `--refresh` cannot name an asset behind it: the run does not plan it, so the name is a usage
  error that lists the assets you can refresh.
- A `Manual` node that has never been computed cannot be read: the command exits 2 and the
  remediation is the command that computes it. `--dry-run` predicts the same.
- Sensors are polled whenever a run needs them, whatever their freshness. A partitioned `Manual`
  node is not pinned yet: it is checked like any other asset. Its keys' bytes count below it as
  an unpartitioned one's do, so refreshing one key with new bytes
  (`--partition region=emea --refresh sales`) makes that key of what reads it stale.
- A `Manual` asset runs in a phase of its own, before what reads it, as a sensor does.
- On `barca serve`, `POST /runs` (what `--server` sends) and ticks read them as they are, as
  `barca get` does ([Scheduling With Cron](/scheduling/)); the older aliases `POST /get/{target}`,
  `POST /run/{target}` and `POST /run` compute `Manual` upstreams as before.

## Seeing what will happen: `--dry-run`

`barca get` and `barca run` take `--dry-run`. It reports, for exactly that command and flags, which
steps would be served from cache and which would run, and why. It executes nothing and writes
nothing: no `.barca` directory is created and no run is recorded.

```bash
barca run report pipeline.py --dry-run --json          # JSON on one line
barca run report pipeline.py --dry-run --pretty        # a table for humans
barca run report pipeline.py --dry-run --refresh src   # preview a refresh and its cascade
barca get total pipeline.py --dry-run --refresh-all
```

```json
{"dry_run": true, "command": "run", "target": "report",
 "steps": [{"id": "pipeline.py:src", "kind": "asset", "action": "cached",
            "run_hash": "…", "artifact": ".barca/artifacts/…"},
           {"id": "pipeline.py:report", "kind": "task", "action": "run",
            "reason": "task", "detail": "tasks always re-run"}],
 "summary": {"will_run": 1, "cached": 1, "unknown": 0}}
```

Each step has an `action`:

| `action` | Meaning |
|---|---|
| `cached` | Served from the cache (`artifact` is the file). |
| `run` | Will execute; `reason` says why (below). |
| `partial` | A partitioned asset where some keys are cached; `partitions` lists the counts and the keys that will run. |
| `unknown` | Cannot be known without running: a dynamic partition (`partitions_from`) whose source has to run first to produce its keys (`reason: "partitions_unknown"`), an asset reading a sensor with no recorded output (`reason: "sensor_output_unknown"`), or an asset reading an `@asset(cutoff=True)` that would run first (`reason: "cutoff_pending"`, [External Data and Early Cutoff: Early cutoff](/reference/external-data/#early-cutoff-assetcutofftrue)), and anything that depends on any of them. |

`reason` is one of `task` and `sensor` (always run), `refresh` (named in `--refresh`),
`refresh_cascade` (downstream of an asset named in `--refresh`), `refresh_all` (`--refresh-all`),
`not_materialized` (no cached result for this code and these inputs: never run, or the code, an
upstream or a sensor's output changed; when the step succeeded before, `changed` lists what
differs from that success and `detail` reads `changed: code (+1 more)`, with the entries
[barca status](/reference/cli/status/) describes; for a partitioned step, those of its first key that runs),
`artifact_missing` (the result is cached, but its
artifact is gone and something needs to read it) or `evicted` (the same, because the cache size
limit deleted it). A `cached` step of an asset pinned with `barca pin` has `reason` `pinned`
([barca pin and unpin](/reference/cli/pin/)). Under `--no-cascade`, a cached step downstream of a refreshed asset carries a
`warning`. A step that reads a sensor is predicted from the sensor's last recorded output, and
its `detail` says so. `summary` counts steps, one per partition key.

A dry run makes the same decisions a real run makes (it calls the same code), and the test suite
checks that `will_run` equals the real run's `steps_executed` across cold, warm, `--refresh`,
`--refresh --no-cascade` and `--refresh-all` runs.

See also: [The Cache](/reference/cache/), [Partitions](/reference/partitions/), [barca status](/reference/cli/status/), [The Run Hash](/reference/hashing/).

---

*The same text is in your terminal: `barca docs refresh`.*
