---
title: "Run Reports and Missing Artifacts"
description: "What barca get and barca run report for each step, recompute byte checks, and what happens when a cached result's file is gone."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/artifacts.md
---

<!-- Generated from crates/barca-cli/docs/artifacts.md (barca docs artifacts). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

What `barca get` and `barca run` report for each step, how barca checks the bytes of a
recomputed result against an earlier one, and what happens when a cached result's file is gone.
How barca decides whether to run a step at all is in [The Cache](/reference/cache/).

## What a run reports

A real `barca get` / `barca run` returns the same per-step information in a `steps` array, with a
`status` of `ran`, `cached` or `partial` (and the same `reason` / `warning`). `steps_executed` is
the number of steps that actually ran, a step that failed included (count `steps[]` by `status`
for how many succeeded and failed); a fully cached second run reports 0 for assets. In
`--agent` mode a cached step also prints `[barca] step:<id> cached` on stderr, so a log shows
what was served from cache as well as what ran. A step whose node declares `env=[...]` also
carries `env`, the values it was hashed with (`null` when unset, `<redacted>` for secret-looking
names), in both the JSON `steps` entry and the `--agent` line (`... env SOURCE_CSV=b.csv`). With
an artifact store, a step whose stored result (or an input it read) is not the copy that was
recorded carries `artifact_mismatch: true` and a `warning` ([Remote Storage](/reference/remote-storage/)).

## Recompute byte checks

A step is meant to be a pure function of its code and inputs, so computing it again under the
same run hash should write the same bytes. Barca checks: every successful materialization records
a SHA-256 digest of the serialized bytes, and a recompute of the same node and run hash
(`--refresh`, `--refresh-all`, a missing artifact) compares with the preceding one. If the bytes
differ, barca warns that the function is not deterministic and sets `steps[].bytes_changed: true`
in run JSON. A new run hash is an expected change and does not warn; a result recorded without a
digest sets the baseline. The check is on serialized bytes, so a serializer that writes variable
metadata can also warn. Artifact paths and exit codes are unchanged. Partitioned steps are
compared per key; the run's step is marked if any key changed. Only assets are checked: a sensor
returns a new value under the same run hash whenever the data it watches changes, and a task runs
every time, so new bytes from either are expected and never warn.

A `Manual` asset is not checked either. It is refreshed by hand with the same code and inputs to pick up
new data, so new bytes are what the refresh is for: no warning, no `bytes_changed`, no
`last_bytes_change`. What reads it is stale after such a refresh, since its bytes count in their
run hashes ([Scheduling With Cron](/scheduling/)).

An asset that is expected to return new bytes for the same code and inputs, such as a snapshot
of an external system, can say so with `@asset(deterministic=False)`: barca then neither warns
nor sets `bytes_changed` or `last_bytes_change` for it. Nothing else changes. Its result is
cached under its run hash like any asset's (it is not recomputed more often), what reads it is
hashed with its run hash as before (new bytes alone do not make them stale; make the external
data an input, a sensor or `freshness=Manual` for that), and the argument is not part of the run
hash, so adding or removing it recomputes nothing.

```python
from barca import asset

@asset(deterministic=False)
def exchange_rates() -> dict:
    return fetch_rates()   # an external snapshot: new bytes are expected
```

`barca status` shows the last such event even after a later unchanged recompute: its JSON has
`last_bytes_change` (node id, run hash, digest, UTC timestamp) and
`last_materialization.content_hash` when a digest was recorded. The boundary is specified in
`specs/interfaces/materialization-digests.md`.

```bash
barca get orders pipeline.py --refresh orders --json   # compare recomputed bytes with the last digest
barca status orders pipeline.py --json                 # last byte-change event and current digest
```

## Seeing what happened

```bash
barca history --json            # recent runs: status, steps executed, steps cached
barca stats total pipeline.py --json   # timing percentiles and cache hit rate for one asset
barca plan pipeline.py          # what would run, in phases, without running it
```

## A cached result whose artifact is missing

A cached result is a row in `.barca/metadata.db`; its artifact is a file. The file can go
missing while the row stays: a disk cleanup, a container restarted without a volume for
`.barca/artifacts/`, or someone deleting the directory. Barca then computes the result again
when, and only when, something needs to read it:

```bash
barca run publish pipeline.py                    # model ran, publish ran
rm .barca/artifacts/pipeline.py--model/*.json
barca run publish pipeline.py --dry-run --json   # model: "action": "run", "reason": "artifact_missing"
barca run publish pipeline.py                    # model runs again, then publish; exit 0
barca run publish pipeline.py                    # model is cached again
```

- **Needed** means one of: a step that is going to run takes the artifact as an input; a
  `partitions_from` step is expanded from it; or it is the output the command returns: the
  targets you named, or with no target the one asset whose artifact is `final_output` (the last
  asset). A partitioned target returns every key (or the keys `--partition` selects), so every
  returned partition is checked (one file lookup per key, a few milliseconds at 5,000 keys).
  Every other asset at the end of a pipeline is treated like an intermediate and is not looked
  at.
- **Missing** means not on this machine's disk and, with an artifact store, not in the store
  either ([Remote Storage](/reference/remote-storage/)). The store has to be there for that to count: if its bucket,
  container or directory is gone, misnamed or unreachable, the run fails with exit 3 and
  computes nothing, because then nothing is known about the artifact.
- The step runs with `reason: "artifact_missing"` and a warning on stderr names the file:
  `[barca] warning: pipeline.py:model: the artifact of its cached result is missing: <path>.
  Computing it again.` Its run hash does not change, so the artifact lands at the same path and
  nothing downstream is invalidated. If the recomputed step reads an input that is missing too,
  that one is computed first, by the same rule. For a partitioned asset only the keys whose
  artifact is missing run.
- **An artifact nothing reads is not looked at.** With `model` deleted and `report` (which
  reads it) cached, `barca get report pipeline.py` executes 0 steps and `model` stays `cached`:
  you can prune large intermediates and keep the final outputs. `model` is computed again the
  first time something needs it.
- `--dry-run` and `barca status` predict the same thing: `model` is `run` / `stale` with reason
  `artifact_missing` under `barca run publish`, and `cached` under `barca get report`.
- Under `barca serve` a scheduled task whose input was deleted recomputes the input once, on its
  next tick, and keeps succeeding.

Known limits:

- Only whether the artifact is a file is checked. A file that is there but truncated or edited
  is read as it is (with an artifact store, a copy that does not match its recorded hash is
  replaced: [Remote Storage](/reference/remote-storage/)). A symlink to a file counts as that file and is read; writing
  the artifact again replaces the link.
- `--agent` announces each step once, with its outcome. A cached step whose artifact is not on
  disk waits: it prints `step:<id> completed` if it is computed again, or `step:<id> cached` at
  the end of the run if nothing needed it. The one exception is a result in a remote store
  whose object turns out to be deleted when it is fetched: its `cached` line was already
  printed, and the warning and a `completed` line for the same step follow
  ([CLI Contract](/reference/cli-contract/)).
- A function that is not deterministic may return a different value when it is computed again
  ("Recompute byte checks"); cached steps downstream of it keep the results they were computed
  with (as after `--refresh --no-cascade`).
- A dry run does not contact a remote artifact store. A result recorded in one is predicted as
  `cached` even if the object has since been deleted from the bucket; the real run finds out
  when it fetches it, and computes it again. A store that is a directory is checked, and if
  the directory itself is gone the dry run predicts `cached` where the run exits 3.

## A directory at an artifact's path

An artifact is one file. If a **directory** sits at an artifact's path (made by hand, by a tool
that unpacked something there, or by a mistaken `mkdir -p`), it is not an artifact, and barca
treats the result exactly as if the file were missing: it is fetched from the artifact store, or
computed again with `reason: "artifact_missing"`, when something needs to read it, and it is not
looked at otherwise. The same holds for a symlink there that leads to a directory or to nothing.

Barca then has to put a file where the directory is. It never deletes what it finds:

- an **empty** directory is removed;
- a directory **with anything in it** is renamed, contents and all, to
  `<run_hash>.<ext>.moved-aside` beside it (`.moved-aside-2`, `-3`, ... if that name is taken),
  and stderr says so:
  `[barca] warning: <path> is a directory, not an artifact. Moved it, with its contents, to
  <path>.moved-aside; barca does not use it, delete it if you do not need it.`
- a **symlink** is replaced by the artifact file. Only the link goes; what it pointed to is not
  touched.

The run goes on and exits 0. Only a directory is ever moved: an artifact file that another
barca process wrote in the same instant is never renamed.

**Where they are, and getting rid of them.** A moved-aside directory stays beside the artifact,
in `.barca/artifacts/<node>/` (under `.barca/envs/<env>/artifacts/` for a named environment).
Barca never reads one again and never lists them in `barca status` or any other command. They
take disk space until you remove them, or until the size limit evicts them: they are the first
thing it deletes ([Disk Use](/reference/disk/)). To see them all:

```
find .barca -name '*.moved-aside*'
```

Delete the ones you do not need. Deleting the whole of `.barca/artifacts/` to reclaim disk
removes them with everything else (results are computed again, or downloaded again from an
artifact store, when something needs them).

**If barca may not move it.** Renaming needs write permission on the directory that holds the
artifact. Without it the run exits 3 (an infrastructure error, not a failed step) and names the
path and the permission; nothing is deleted. Grant the permission, or move or remove the
directory yourself, and run the command again.

This applies only inside barca's own artifact directory (`.barca/artifacts/`). Barca moves,
renames and replaces nothing anywhere else:

- a `@sink` file path is yours. Barca writes the file there and changes nothing else: a
  directory at the path fails the sink, and so the step (`SinkError: sink ...:
  IsADirectoryError`), and is left as it is, and a symlink is written through (the file it
  points to is written, the link stays). A dataset sink (a path ending in `/`) owns its prefix
  instead, and refuses one that holds files it did not write ([Sinks](/reference/sinks/));
- a directory at an object's path in an artifact store that is a shared directory fails the
  fetch (exit 3) or the upload (a warning; the result stays a pending upload), naming the
  path, and is left as it is. The message says to remove or rename it there; `--refresh-all`
  does not help ([Remote Storage](/reference/remote-storage/)).

See also: [The Cache](/reference/cache/), [barca status](/reference/status/), [Disk Use](/reference/disk/).

---

*The same text is in your terminal: `barca docs artifacts`.*
