---
title: 'RFC-0009: The store as backup, and restore'
description: 'A remote store with no server is this machine''s backup and read-through cache. The metadata database is backed up on the minute, five copies are kept, and restore is one explicit command.'
---

- **Status:** Accepted
- **Amended:** 2026-10-20: the `unsynced` reason is spelled `cache.reason` with snake_case values
  (`local_only`, `store_only`, `mismatch`), replacing the `why` wording below; see
  "Implementation notes" at the end for how #403 shipped it.
- **Date:** 2026-10-14
- **Touches:** barca-core | barca-cli | python/barca
- **Prerequisites:** minute checkpoints of the shared history ([#376](https://github.com/barca-orc/barca/pull/376)) and upload receipts ([#381](https://github.com/barca-orc/barca/issues/381)), the content digest per artifact ([#392](https://github.com/barca-orc/barca/issues/392)).
- **Supersedes / Related:** supersedes the shared-history half of [RFC-0006](/rfcs/0006-configuration-and-remote-state/) (optimistic mode, pull-before-run, the conditional push). Sharing between machines is [RFC-0007](/rfcs/0007-server-mode/). Fail-soft uploads are [#390](https://github.com/barca-orc/barca/issues/390); the corrupt-blob problem is [#243](https://github.com/barca-orc/barca/issues/243). Implementation: [#403](https://github.com/barca-orc/barca/issues/403). Decisions recorded in [DIRECTION.md](https://github.com/barca-orc/barca/blob/main/DIRECTION.md#decided-october-2026).

---

## 1. Summary

A remote store configured with no server has one job: it is this machine's backup and its
read-through cache. Artifacts are uploaded as today. The metadata database is backed up to the
store on the existing minute checkpoint and once more when a run ends, from `barca serve` as
well, and the five most recent copies are kept in the store and under `.barca/backups/`.
Nothing is ever pulled before a run. `barca state restore` puts a backup back, only when asked.

Two machines sharing one store with no server is no longer a supported way to share state.
Sharing means a server ([RFC-0007](/rfcs/0007-server-mode/)). `optimistic` mode and `BARCA_STATE`
are removed, with a Breaking line.

`cached` gets a precise meaning when a store is configured: the local artifact exists, the store
object exists, and their content digests agree. Any other combination is `unsynced` with a
reason, and `barca get` repairs it from the owner's copy without recomputing.

## 2. Motivation

RFC-0006's shared history made the store the home of the metadata database: pull it before a
run, push it after with a conditional upload. That design did two jobs with one blob, backup and
sharing, and did the second badly enough to hurt the first:

- A corrupt or truncated blob is pulled by every machine and replaces every machine's history
  ([#243](https://github.com/barca-orc/barca/issues/243)). A backup that can destroy the thing
  it backs up is not a backup.
- Every run, even a fully cached one, uploads the whole database and may conflict with another
  machine's push. The user feedback behind [#390](https://github.com/barca-orc/barca/issues/390)
  was "pushes failed intermittently with 'not authorized' after the work was done, and the
  command exited as failed".
- Two machines writing one blob have no owner. When they disagree there is no rule for who is
  right.

With sharing moved to a server that owns the ledger, the store can be what people expect an
object store to be: a durable copy of this machine's artifacts and history that survives a lost
laptop or a wiped container, and a cache the same machine can read back from.

## 3. Guide-Level Explanation

### 3.1 CLI

```toml
# barca.toml
[remote]
uri = "s3://my-bucket/barca"
```

That is the whole configuration. Artifacts upload in the background as today. Every minute during
a run, and once when it ends, the metadata database is copied to the store and to
`.barca/backups/`, each keeping the five newest copies:

```
s3://my-bucket/barca/state/backups/2026-10-14T18-02-11Z.db
.barca/backups/2026-10-14T18-02-11Z.db
```

Nothing is downloaded before a run. The database on this machine is the truth for this machine.

```bash
barca state backups                  # the copies that exist, newest first, with size and run id
barca state restore                  # put the newest backup back
barca state restore --from 3         # the third newest
barca state restore --from 2026-10-14T18-02-11Z.db
```

`restore` moves the current database to `.barca/metadata.db.pre-restore` before writing, prints
what it restored and from where, and never runs by itself. It prefers the local copy when the
local and store copies have the same name and digest, and says which one it used.

`barca status` with a store:

```
NAME      KIND   STATE     WHY           ...
raw_data  asset  cached    materialized
summary   asset  unsynced  local only    # computed here, upload still pending
report    asset  unsynced  store only    # local artifact missing; get fetches it
prices    asset  unsynced  mismatch      # bytes differ; get keeps this machine's copy and re-uploads
```

The table prints the reason in words; JSON spells it `cache.reason` in snake_case
(`"state": "unsynced"`, `"reason": "local_only"`), like every other status reason (amended
2026-10-20).

`barca get` on an `unsynced` node does not recompute. It serves this machine's copy when there is
one and repairs the store, or fetches the store's copy when there is not. A `mismatch` also
prints the non-determinism warning from #392, because that is what a mismatch means.

`BARCA_REMOTE=off` still turns the store off for one command, as today. `BARCA_STATE` is gone.

### 3.2 Python API

No change. `barca.api` runs the binary.

### 3.4 HTTP API

`barca serve` backs up its database on the same cadence and is the owner of its store. With
RFC-0007 the server is also the ledger other machines post to, and the backup is the recovery
path for that ledger. No new routes.

### 3.5 Dev server / `--watch` / UI

The UI's status view shows `unsynced` and its reason where it shows `cached` today.

---

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Config:** `[remote] uri` as today. `[remote] state` is removed; a `barca.toml` that sets it
  gets a usage error naming this RFC and the release. `BARCA_STATE` is removed the same way.
  `BARCA_REMOTE` is unchanged.
- **Backup cadence:** the checkpoint interval already used for the shared history (one minute,
  #376), plus one at run end, from `get`, `run` and `serve`. A checkpoint that finds no new rows
  since the last backup writes nothing. The copy is taken with the database's online backup API
  so a running write never produces a torn file.
- **Retention:** five copies, in the store under `state/backups/` and locally under
  `.barca/backups/`, each pruned independently after a successful write. Names are UTC
  timestamps with a `.db` suffix; a sidecar `<name>.json` carries `{run_id, rows, digest,
  barca_version, db_format}`.
- **`barca state backups`** lists both locations merged, newest first: name, where (`local`,
  `store`, `both`), size, run id, and whether the two copies agree. `--json` as every inspection
  command.
- **`barca state restore [--from <N|name>]`**: validates the chosen copy with the existing
  `state_validate.rs` checks (format, integrity) before touching anything, moves the current
  database to `metadata.db.pre-restore` (replacing an older `.pre-restore`), writes the copy,
  prints one line. A copy that fails validation is a usage error naming the next newest. Exit 2
  when no backup exists.
- **`cached` and `unsynced`:** with a store configured, a node's STATE is `cached` only when the
  local artifact exists, the store object exists at `{node}/{run_hash}{ext}`, and the digest
  recorded in `materializations` equals both. Otherwise STATE is `unsynced` and WHY is
  `local only`, `store only` or `mismatch`. In JSON: `"cache": {"state": "unsynced", "reason":
  "local_only"}`, with `store_only` and `mismatch` for the others (amended 2026-10-20: the
  reason is `cache.reason`, snake_case, not a separate `why` key).
  Without a store, STATE is `cached` when the local artifact exists, as today.
- **Repair on `get`:** `local only` uploads; `store only` fetches; `mismatch` re-uploads this
  machine's copy, prints `[barca] warning: <node> produced different bytes for the same code and
  inputs; the function is not deterministic`, and sets `bytes_changed: true` on that step.
  A repair is never counted as a step and never changes the exit code.
- **Fail-soft upload (#390):** when every step succeeded and only an upload or a backup failed,
  the run exits 0 with a warning naming what was not published and why; the affected nodes are
  `unsynced`, `local only`. The next command that touches the store retries them before its own
  work. `uploads: {pending: N, error: "..."}` is added to the run JSON.
- **Breaking** (minor bump): `[remote] state = "optimistic"`, `BARCA_STATE`, pull-before-run and
  the conditional push are removed. A team that used them to share moves to a server
  (RFC-0007); a single machine that used them for backup gets the same backup with no setting.

### 4.2 Implementation Details

- `state_sync.rs` and `persist.rs` already run the minute checkpoint and the end-of-run push of
  the shared history. The push becomes a backup write: same timer, same trigger, no conditional
  upload, no pull, plus a prune of the sixth-newest copy. The local copy is written first, then
  uploaded by the transfer helper.
- The digest comparison for `cached` is a local check (the row's digest against the local
  file's) plus one store `HEAD` per node, done once per `status` command and cached for that
  command. Backends without a content checksum compare size and read the digest from the
  sidecar the upload writes with each artifact (#381's receipt).
- `recover.rs` already recomputes a cached step whose artifact is gone. It gains the
  `store only` branch: fetch before recompute.
- `state_base.rs`, `state_carry.rs`, `state_prev.rs` (the pull, replay and carry logic of
  optimistic mode) are removed. `state_validate.rs` stays and guards `restore`.

### 4.3 Rust ↔ Python Boundary

The transfer helper gains two verbs, `backup` (upload one file to `state/backups/` and prune)
and `fetch` for a store-only artifact, both over its existing UDS. No worker change.

### 4.4 Node-Kind Semantics

Unchanged. Sensors and tasks are never `cached`, so they are never `unsynced`.

### 4.5 Edge Cases

- **A backup fails.** The run is not affected; the next checkpoint tries again. A backup that
  fails three checkpoints in a row prints one warning per run, not one per minute.
- **A restore while `serve` is running.** Refused with `kind: infra` naming the lock holder;
  stop the server first.
- **A restore to an older format.** `restore` refuses a copy whose `db_format` is newer than the
  binary understands, and migrates an older one on first open as any database is today.
- **The store has copies this machine has never seen** (a fresh clone of the project, a new
  container). `barca state backups` lists them as `store`, and `restore` fetches the chosen one.
  This is the "wiped laptop" recovery path, and the only time a database comes down from the
  store.
- **Two machines pointed at one store with no server.** Each backs up to the same prefix; the
  five-copy retention interleaves their histories, and each one's artifacts land at hash paths
  the other may also write. Nothing prevents this and nothing coordinates it: the manual says it
  is unsupported and points at RFC-0007. A `mismatch` on such a setup is the user's signal.

## 5. Determinism, Caching & Testing

No cache-key change.

- Backup: a run of N minutes leaves at most N+1 copies, pruned to five; no copy is taken when
  nothing changed; a copy taken mid-write validates.
- Restore: newest by default, `--from` by index and by name, pre-restore file present
  afterwards, refusal on a corrupt copy, refusal under `serve`, exit 2 with none.
- Status: the three `unsynced` reasons, each repaired by `get` without a step running, and the
  mismatch warning fired exactly once.
- Fail-soft: upload failure after a successful run exits 0 with the warning, the node is
  `local only`, the next command uploads it and the node is `cached`.
- Migration: a `barca.toml` with `state = "optimistic"` gets the usage error; a project whose
  store holds the old shared-history blob ignores it.

## 6. Performance

A backup is one file copy and one upload per minute of a running job, and none when idle. For a
large database the upload dominates; it runs on the transfer helper beside artifact uploads and
never blocks a step. `barca status` with a store costs one `HEAD` per node, which the command
already pays for shape sampling against the store today.

## 7. Drawbacks

- Teams that shared through a bucket lose that path and need a server. That is deliberate.
- Five copies of a large database in the store is five times its size. Retention is fixed in
  this RFC; a knob can come later if someone needs one.
- `unsynced` is a fourth state in `barca status`. If `get` always repairs it silently and nobody
  acts on it, it should collapse into a WHY under `cached`.

## 8. Rationale & Alternatives

- **Keep optimistic mode for server-less teams.** The sharing it offers is the kind that breaks
  silently. One canonical way to share is simpler to explain and to test.
- **Pull the backup automatically when the local database is missing.** Tempting for the fresh
  clone case, but it is exactly how #243 propagated a bad blob. One explicit command is the
  whole safety story.
- **Incremental backups.** Smaller uploads, more code, and a restore that has to replay. The
  database is small for most projects; revisit when a measured one is not.
- **Content-addressed artifact paths** (#246) would make `mismatch` impossible by construction.
  They change every path a user has seen; the digest check gives the same signal without that.

## 9. Prior Art

Litestream and SQLite's online backup API, for the copy mechanism. `pg_dump` cron jobs with a
retention count, for the shape of the policy: a fixed number of dated copies and an explicit
restore.

## 10. Unresolved Questions

None at acceptance.

## 11. Future Possibilities

- A retention knob (`[remote] backups = N`).
- `barca gc` for store objects no row references (parked).
- Showing, in the UI, the last backup time and whether it is behind.

## Implementation notes (#403, 2026-10-20)

How the implementation differs from the text above, deliberately:

- The store side is `python -m barca._state` (one helper process per backup, listing, fetch or
  status check: verbs `backup`, `list`, `fetch`, `stat`), not new `backup`/`fetch` verbs on the
  per-run transfer helper, so `barca serve` and `barca state` can use it with no run going. The
  transfer helper gained one verb, `stat`, which `get` uses to repair its targets.
- The copy is the database's main file, taken under the database's lock right after the
  write-ahead log is folded in; the engine has no online backup API. The guarantee is the same:
  a write in progress never produces a torn copy.
- Backups are taken only when the store is configured with `[remote] uri` (or
  `BARCA_REMOTE_URI`); `BARCA_ARTIFACT_URI` / `[remote] artifacts_uri` alone take none.
  `[remote] state_uri` and `push_retries` are removed with `state`, the same way.
- `barca status` compares the store object's size (and its SHA-256 only for a store that is a
  plain directory): object stores give no per-object content checksum and no receipt sidecar is
  written yet, so a same-size object with other bytes is not detected there.
- Partitioned nodes get only the pending-upload `local_only` check.
- `barca get` repairs its targets, not their whole upstream cone. A `mismatch` whose local copy
  is not the recorded one is left to the existing fetch, which takes the store's copy (#247).
