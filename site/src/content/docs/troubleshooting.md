---
title: "Why Did (or Didn't) It Run?"
description: "Common surprises: an asset that did not rerun after its data, an environment variable or an option changed; a step that reran when nothing seemed to change; and how to ask barca why."
---

Barca reruns an asset when its **run hash** changes: a hash of its code (and the helper code it
calls), its inputs' run hashes, what the sensors and `Manual` assets it reads returned (the
output bytes of a `cutoff=True` asset), its partition key and the environment variables it lists
in `env=`. Anything outside that list is invisible to the cache.
[The Cache](/reference/cache/#at-a-glance) has the full table.

## First, ask barca

```bash
barca status revenue                 # its state and why: cached, stale (changed, upstream_stale, failed, ...)
barca get revenue --dry-run          # what this command would run and why, without running anything
```

`--dry-run` gives each step an `action` (`cached`, `run`, `partial`, `unknown`) and, for one that
runs, a `reason`. `not_materialized` with `changed: code` means the code changed; `changed` lists
everything that differs from the last success. [Status](/reference/status/) lists every state and
reason.

## I changed a file (a CSV, a table, a bucket) and the asset did not rerun

The asset's code and inputs are the same, so its run hash is the same and the old result is
served. Barca does not read the data a function opens in its own body. Put a `@sensor` in front
of it that returns something identifying the current version of the data (an etag, a
last-modified time, a row count) and make the asset read the sensor. It then reruns exactly when
that value changes. See [External data that changes in place](/reference/cache/#external-data-that-changes-in-place)
and [Sensors](/guide/#4-sensors-data-from-outside).

To recompute once by hand: `barca get revenue --refresh revenue`.

## I changed an environment variable and nothing reran

Only variables listed in the asset's `env=[...]` are part of its run hash. Barca looks for
`os.environ` and `os.getenv` reads it can see and warns before the run:
`[barca] warning: 3 steps read environment variables not declared in env=: ...`. Add the names to
`env=` and a change of value reruns the asset. See
[Environment variables](/reference/cache/#environment-variables).

## I changed `retries=` or `timeout_seconds=` and nothing reran

That is intended. Options that change only when or how often a step runs (`retries=`,
`timeout_seconds=`, `freshness=`, `weight=`, `name=`, `description=` and the like) are not part
of the run hash, and neither is the decorator's formatting. The ones that can change the result
are: `inputs=`, `serializer=`, the dimension names and `partitions_from` source of `partitions=`,
`@sink(...)`, and any other decorator wrapping the function. The full table is in
[Which decorator arguments count](/reference/hashing/#which-decorator-arguments-count).

## Everything downstream reran, although the result did not change

What reads an asset folds in that asset's run hash, so when it reruns, everything below it reruns
too, even if it wrote the same bytes. Mark it `@asset(cutoff=True)` (experimental) and what reads
it depends on the bytes it wrote instead. See [Early cutoff](/reference/cache/#early-cutoff-assetcutofftrue).

## A step reran although I did not touch it

`barca status` names the reason. The usual ones:

- `upstream_stale`: something upstream of it changed and runs again.
- A sensor it reads returned a new value.
- `failed`: its last attempt failed, so it is tried again.
- `artifact_missing` or `evicted`: its result file is gone (deleted, or evicted by the cache size
  limit in [Disk Use](/reference/disk/)) and something needs to read it.

## My task ran again although nothing changed

Tasks always run: that is the difference between `@task` and `@asset`. The assets a task reads
are still served from cache. Use `@asset` for a result you want reused. See
[Tasks](/reference/tasks/).

## I refreshed an asset but something downstream still has the old data

`--refresh name` reruns `name` and everything downstream of it **within the target you asked
for**. With `--no-cascade`, or for an asset outside that target's dependencies, the downstream
asset keeps its old result, and barca prints
`warning: '…' was served from cache but depends on refreshed '…'`. Refresh again with a target
below it, or name it in `--refresh`. See [Exactly what --refresh does](/reference/cache/#exactly-what---refresh-does).

## Nothing recomputes an asset, even with `--refresh`

It may be pinned. A pinned asset (`barca pin`) is served from the result it was pinned to on
every run, `--refresh` and `--refresh-all` included; a run reports its step `cached` with
reason `pinned`.
`barca unpin <asset>` releases it. See [Pinning and Rollback](/reference/pins/).

## A schedule skipped an asset, or `barca get` refuses it

It may be paused. A scheduled tick does not compute an asset paused with `barca pause`: its step
is `skipped` with reason `paused`. A `barca get` that names it is refused (exit 2) unless you add
`--allow-paused`. `barca resume <asset>` lets the next tick compute it again. See
[Pausing Assets](/reference/pausing/).

## Still stuck

[`barca doctor`](/reference/doctor/) checks the project's prerequisites without running anything,
and [Logs](/reference/logs/) shows what a step printed. The
[Anti-Patterns](/recipes/anti-patterns/) page lists the mistakes that give stale or wrong
results.
