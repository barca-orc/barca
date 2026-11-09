---
title: "barca pin: Pinning and Rollback"
description: "barca pin and unpin: roll an asset back to an earlier materialization, held for every run until unpinned."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/pins.md
---

<!-- Generated from crates/barca-cli/docs/pins.md (barca docs pins). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

When an asset produced bad data, `barca pin` puts a previous good materialization back in
service at once, without editing code or recomputing anything. Artifacts are addressed by run
hash (`.barca/artifacts/<node>/<run_hash><ext>`), so earlier results are usually still on disk.

```bash
barca pin orders --versions                  # recent successful materializations, newest first
barca pin orders --previous                  # roll back to the one before the latest
barca pin orders --run-hash 3f9a1c --reason "bad feed on 11-07"   # or pick one (a hash prefix)
barca pin orders --from-file fixed.parquet --reason "hand-corrected"   # or override with your data
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

## Repairing one partition

Name one complete key in brackets, quoting it for your shell. All dimensions must be supplied:

```bash
barca pin 'orders[region=emea,day=2026-11-08]' --versions
barca pin 'orders[region=emea,day=2026-11-08]' --from-file fixed.json --reason "bad feed"
barca unpin 'orders[region=emea,day=2026-11-08]'
```

The pin holds only that key. Its consumers use the replacement's run hash; neighboring keys
keep their own data and normal cache decisions. Uploading to an unknown value is refused.
Runtime-derived keys are checked against their source's current recorded output; run the source
first if its keys are not known. The source still supplies membership during a run: a pin does
not invent a key or retain one that the source removed. Unpinning restores normal computation.

The key explorer shows held keys as **pinned**. Open a key to upload replacement data, select
an earlier version, or unpin. The confirmation names the complete key and accepts a reason.
The output preview and input previews read pinned versions when present.

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
request made with a project token ([Server Mode: a Shared Ledger and --server](/reference/server-mode/)), nothing otherwise.

## Pinning to data you supply

When no earlier result is good, `barca pin TARGET --from-file PATH` pins the asset to a file you
provide, with the same semantics as any pin:

- The format comes from the extension: `.json`, `.parquet`, `.csv`, `.pkl` or `.pickle`
  (another extension exits 2). The file is checked by loading it with the reader runs use
  (parquet and CSV need pyarrow or pandas, as for a computed result); one that does not load
  exits 2 and nothing is pinned.
- Its bytes are stored as a new artifact of the asset,
  `.barca/artifacts/<node>/<run_hash><ext>`, whose run hash is the SHA-256 of `barca-upload\0`
  followed by the bytes: the same file uploaded again is the same version, and what reads it is
  computed (or served from cache) against it.
- It is recorded as a successful materialization without a run (`run_id` null, duration 0),
  marked as an upload in `asset_uploads` with when, who, the file name and `--reason`.
  `--versions` notes it `uploaded`; the version's `upload` key is
  `{uploaded_at, uploaded_by, file_name, reason}` (null for a computed version).
- `barca unpin TARGET` returns the asset to normal. The uploaded version stays in the list and
  can be pinned again with `--run-hash`, until the cache size limit evicts it.

The file may be at most 256 MiB. The value is not checked against the asset's return type or
what reads it: a consumer that expects other columns fails when it runs, as with any input.

## JSON output

| Command | Shape |
|---|---|
| `barca pin --json` | `{pins: [{node_id, run_hash, pinned_at, pinned_by, reason, run_id, upload}]}` (`run_id`: the run that computed the pinned version, null for an upload; `upload`: as on a version) |
| `barca pin TARGET --versions --json` | `{node_id, pinnable, pin, versions: [{run_hash, run_id, materialized_at, elapsed_seconds, size_bytes, format, artifact, available, current, pinned, upload}]}` |
| `barca pin TARGET --previous --json` | `{node_id, pin, version, replaced}` (`replaced`: the earlier pin, or null) |
| `barca pin TARGET --from-file PATH --json` | the same; `version.upload` is set |
| `barca unpin TARGET --json` | `{node_id, removed}` (`removed`: the pin, or null when it was not pinned) |

Times are RFC 3339 UTC. Errors use the usual envelope; asking for a version that does not exist,
an evicted one, or pinning something that cannot be pinned exits 2.

## On a server

`barca serve` has the same operations: `GET /pins`, `GET /assets/{name}/versions?limit=N`,
`POST /assets/{name}/pin` with `{"run_hash": "..."}` or `{"previous": true}` and an optional
`"reason"`, `POST /assets/{name}/pin/upload?file_name=NAME&format=FMT&reason=TEXT` with the
file's bytes as the body (`format` or a `file_name` with a known extension is required; over
256 MiB is refused with 413; a body that does not load is 400), and
`DELETE /assets/{name}/pin`. A `--read-only` server refuses the writes (403).
The web UI's asset panel lists the versions with **Roll back to this** and offers **Upload
replacement…** (a file picker, then a confirmation naming the file and its size), and shows a
banner with **Unpin** while the asset is pinned, naming who uploaded the override. `barca pin --server` and `barca unpin --server` (or a
configured server) act on the server's database.

## Limits

- Only assets or complete keys of partitioned assets can be pinned. Tasks, sensors and a whole partitioned asset cannot be pinned.
- A pin lives in one metadata database. A server's pin holds for the server's own runs
  (scheduled, requested, `--server`); a laptop's local `barca get` against that server's ledger
  does not read the server's pins, and a local pin is not sent to a server.
- If the pinned row or artifact disappears anyway (a restored database, a file deleted by hand),
  a run that reads the asset stops with exit 2 naming the pin (a scheduled tick reports its
  target `blocked`) until it is unpinned or pinned again.
- `barca status` reports the asset's cache state for its current definition; a pinned asset
  reads `cached` with reason `pinned` in `barca get --dry-run`.
- An uploaded file is written to the local artifact directory of the machine that pins (the
  server's, for `--server` and the UI); with a remote store it is not copied to the store.

See also: [The Cache](/reference/cache/) (run hashes and artifacts), [Disk Use](/reference/disk/) (what eviction keeps),
[Server Mode: a Shared Ledger and --server](/reference/server-mode/).

---

*The same text is in your terminal: `barca docs pins`.*
