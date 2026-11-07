---
title: "Pinning and Rollback"
description: "barca pin and unpin: roll an asset back to an earlier materialization, held for every run until unpinned."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/pins.md
---

<!-- Generated from crates/barca-cli/docs/pins.md (barca docs pins). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

:::note
This page is the manual topic `barca docs pins`: the CLI prints the same text.
:::

When an asset produced bad data, `barca pin` puts a previous good materialization back in
service at once, without editing code or recomputing anything. Artifacts are addressed by run
hash (`.barca/artifacts/<node>/<run_hash><ext>`), so earlier results are usually still on disk.

```bash
barca pin orders --versions                  # recent successful materializations, newest first
barca pin orders --previous                  # roll back to the one before the latest
barca pin orders --run-hash 3f9a1c --reason "bad feed on 11-07"   # or pick one (a hash prefix)
barca pin                                    # every pinned asset, who pinned it and why
barca unpin orders                           # back to normal
```

## What a pin does

A pin is a row in the metadata database (`asset_pins`), written only by barca. While an asset
is pinned:

- **Every run reads the pinned materialization.** `barca get`, `barca run`, `barca.get()`,
  `--server` runs and the server's scheduled ticks serve the asset from it, as step status
  `cached` with reason `pinned`. `barca get orders` prints the pinned artifact.
- **Nothing computes it.** Not a changed definition, not its inputs changing, not
  `--refresh orders` nor `--refresh-all`, not its own schedule: the asset is never run and its
  pinned result is never overwritten. A pinned asset's upstream steps are not planned by a run
  that only needs them for it.
- **What reads it is hashed against the pinned result.** A consumer's run hash folds in the
  pinned materialization's run hash, exactly as when that materialization was current. A
  consumer computed from it before is served from cache; otherwise it runs once against the
  pinned data, and later runs reuse that result.
- **The pinned result is kept.** `barca gc` and the cache size limit never evict it, and the
  metadata trim never deletes its row.

`barca unpin orders` removes the row. The next run decides `orders` as it would have without
the pin: usually it is computed again (it was stale, or its latest result is the bad one; use
`--refresh orders` to recompute it with the same code), and what reads it follows.

## Choosing a version

`barca pin TARGET --versions` lists the asset's successful materializations that recorded an
artifact, one per run hash (its newest row), newest first: the run hash, when it was recorded,
its duration and size, the run id, and whether it is the latest, the pinned one, or evicted
(the local file was deleted by the cache size limit). `--limit N` shows more (default 20).

- `--previous` pins the newest version before the latest one.
- `--run-hash HASH` pins the version with that run hash, or the one version whose run hash
  starts with `HASH` (at least 6 characters).
- `--reason TEXT` records why; `barca pin` and the UI show it.

Pinning a pinned asset replaces its pin. A version whose artifact is no longer on disk cannot be
pinned. The pin records when, and who: `user@host` for the CLI, `token:<name>` for a server
request made with a project token ([`barca docs server-mode`](/reference/server-mode/)), nothing otherwise.

## JSON output

| Command | Shape |
|---|---|
| `barca pin --json` | `{pins: [{node_id, run_hash, pinned_at, pinned_by, reason}]}` |
| `barca pin TARGET --versions --json` | `{node_id, pinnable, pin, versions: [{run_hash, run_id, materialized_at, elapsed_seconds, size_bytes, format, artifact, available, current, pinned}]}` |
| `barca pin TARGET --previous --json` | `{node_id, pin, version, replaced}` (`replaced`: the earlier pin, or null) |
| `barca unpin TARGET --json` | `{node_id, removed}` (`removed`: the pin, or null when it was not pinned) |

Times are RFC 3339 UTC. Errors use the usual envelope; asking for a version that does not exist,
an evicted one, or pinning something that cannot be pinned exits 2.

## On a server

`barca serve` has the same operations: `GET /pins`, `GET /assets/{name}/versions?limit=N`,
`POST /assets/{name}/pin` with `{"run_hash": "..."}` or `{"previous": true}` and an optional
`"reason"`, and `DELETE /assets/{name}/pin`. A `--read-only` server refuses the writes (403).
The web UI's asset panel lists the versions with **Roll back to this**, and shows a banner with
**Unpin** while the asset is pinned. `barca pin --server` and `barca unpin --server` (or a
configured server) act on the server's database.

## Limits

- Only assets without partitions can be pinned: not tasks, sensors or partitioned assets.
- A pin lives in one metadata database. A server's pin holds for the server's own runs
  (scheduled, requested, `--server`); a laptop's local `barca get` against that server's ledger
  does not read the server's pins, and a local pin is not sent to a server.
- If the pinned row or artifact disappears anyway (a restored database, a file deleted by hand),
  a run that reads the asset stops with exit 2 naming the pin (a scheduled tick reports its
  target `blocked`) until it is unpinned or pinned again.
- `barca status` reports the asset's cache state for its current definition; a pinned asset
  reads `cached` with reason `pinned` in `barca get --dry-run`.
- Uploading a file to become a pinned value is not supported yet.

See also: [`barca docs cache`](/reference/cache/) (run hashes and artifacts), [`barca docs disk`](/reference/disk/) (what eviction keeps),
[`barca docs server-mode`](/reference/server-mode/).
