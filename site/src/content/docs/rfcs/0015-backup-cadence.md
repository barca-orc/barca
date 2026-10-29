---
title: 'RFC-0015: Configurable Metadata Backup Cadence'
description: 'Control routine whole-database backups, retain the final shutdown backup, and report local backup storage outside the cache budget.'
---

- **Status:** Accepted for 0.36.0; implementation present, not released
- **Date:** 2026-10-29
- **Touches:** barca-core | barca-cli | HTTP server
- **Supersedes / Related:** Amends the cadence and local disk accounting in
  [RFC-0009](/rfcs/0009-store-backup-and-restore/) and
  [RFC-0012](/rfcs/0012-cache-size-limit/).
  [#706](https://github.com/barca-orc/barca/issues/706),
  [#707](https://github.com/barca-orc/barca/issues/707).

## 1. Summary

Add `[remote] backup_interval`, overridden by `BARCA_BACKUP_INTERVAL`, with a default of
`"1m"` and an `"off"` value. It controls routine backups of a metadata database, including
backups requested when a server-owned run ends. A server's final shutdown backup remains,
even with `"off"`. Standalone CLI commands retain their final backup.

Keep the existing five-copy local rotation and restore behavior. Exclude recognized local
metadata backups from the cache budget and report their bytes separately, alongside the
physical total. They still consume disk space; they are not made invisible to diagnostics.

The implementation is present but not released. The manual and contracts own its current
behavior; this RFC records the design. Publication as part of 0.36.0 is still required.

## 2. Motivation

The deployment in #706 has a 1.7 GB metadata database and a schedule that records a run every
minute. A changed database can produce a whole copy and upload every minute, plus five local
copies that count against a cache budget but cannot be evicted.

There are three current backup callers: the server timer, a running command's recorder
checkpoint, and the execution path's final backup. Changing only the server timer does not
solve this deployment: scheduled runs still call the final-backup path. The policy must be
applied at the backup admission boundary, rather than independently in each timer.

## 3. Guide-Level Explanation

```toml
[remote]
uri = "abfss://container@account.dfs.core.windows.net/barca"
backup_interval = "1h"
```

Routine requests copy/upload at most once per configured interval for this database. A
schedule tick, an API run and a recorder checkpoint share that limit. A request that finds
no change still avoids writing an identical backup. A failed attempt retries at the next
eligible interval, retaining the existing unsent copy for retry.

```bash
BARCA_BACKUP_INTERVAL=off barca serve pipeline.py
```

`off` disables routine copies and their upload retries. It does not disable artifact
transfers, ledger operations, backup listing or explicit restore. On graceful shutdown the
server still attempts one final backup after its runs have stopped and recorded their
outcomes, subject to the existing shutdown budget. An abrupt kill cannot promise that backup.

Standalone `get` and `run` commands still attempt a final backup when they finish, including
when periodic backups are off. A laptop configured to use a server continues not to back up
its local ledger into the server's backup set.

`barca status`, `barca doctor` and `barca gc` distinguish managed cache bytes from local
metadata backups. For example, a 20 GB cache budget and 8.5 GB of backups are displayed as
separate amounts; the physical total includes both. The quota is not a free-space guarantee.

## 4. Reference-Level Explanation

### 4.1 Configuration

| Surface | Meaning |
|---|---|
| `[remote] backup_interval` | String duration; default `"1m"` |
| `BARCA_BACKUP_INTERVAL` | Overrides TOML with the same grammar |
| `"off"` | No routine backup or routine retry; final backups remain |
| Positive integer, optionally followed by `s`, `m`, `h` or `d` | Seconds without a suffix; otherwise that unit |

Trim surrounding whitespace. Reject empty values, zero, negatives, fractions, compound
durations, unknown suffixes, overflow and non-string TOML values as usage errors (exit 2),
naming the setting. `0` is not an alias for `off`. Resolution follows the existing selected
environment and configuration precedence. A running server uses its startup configuration;
changing the value requires restarting it.

There is no new CLI flag or retention-count setting. A project with no backup root takes no
metadata backup, as today; configuring only `artifacts_uri` does not create a backup root.

### 4.2 One policy for each database

Distinguish a routine request from a final process shutdown request explicitly:

| Request | Interval applies? |
|---|---|
| Server timer | Yes |
| Recorder checkpoint during a run | Yes |
| End of a run owned by `serve`, including a scheduled run | Yes |
| Final standalone CLI backup | No |
| Final server shutdown backup | No |

The same admission gate covers copying, hashing and uploading. A not-due request must return
before taking a snapshot or hashing the database. Do not implement the interval as a check
after the expensive copy has already happened.

Serialize backup admission and the backup operation for one database. Persist the last
admitted attempt time atomically so concurrent callers and a server restart cannot reset
the limit. Acquiring that coordination must honor cancellation and the caller's deadline;
network work must not hold the database write lock. Other environments remain independent.

An attempted backup consumes the routine interval even if it fails, preventing a failing
store from causing a retry storm at every run completion. No-op admission and a request
already covered by an in-flight backup are not backup failures. Keep the current warnings
and fail-soft remote-upload behavior; an unsent copy remains eligible at the next due time.
Final shutdown can retry without waiting for that interval.
Final attempts update the same evidence, so the next routine request does not immediately
repeat their work.

A not-due, disabled or already-in-progress outcome is distinct from an unchanged database
or a confirmed upload. It must not mark a run as backed up or clear a recorder's dirty
generation. When the interval expires, previously skipped progress still needs a backup
even if no additional materializations arrived. Cadence evidence is not proof of coverage.

Use monotonic time while a process lives. Persisted wall-clock evidence must tolerate a
clock moving backward without suppressing backups indefinitely: never postpone a due check
by more than one configured interval because a recorded timestamp is in the future. Missed
ticks are skipped, not replayed in a burst. An overdue database may be checked in the
background at startup; it must not delay binding the server or block request workers.

Final backup still performs change detection and may produce no new copy. Retain the current
bounded drain and backup budgets, including when the interval is `off`; do not accidentally
remove the final-backup task or its supervisor grace allowance when removing its timer.

The default periodic duration remains one minute. Server-owned run completion now respects
that duration rather than forcing an extra copy. This changes the durability boundary for
rapid consecutive served runs: completion does not imply a new remote metadata backup.
Document that distinction and the shutdown exception in the 0.36.0 upgrade notes. The
standalone CLI's final-backup behavior is unchanged.

### 4.3 Local backup accounting

Choose explicit reporting outside the cache budget, rather than automatic deletion of
restore copies. Keep five local backups and the existing remote rotation and cross-writer
protections. Retention remains an upper bound, not archival history.

Only files owned by the backup layout under `.barca/backups/` and
`.barca/envs/<env>/backups/` are excluded. Include their owned sidecars, unsent markers and
temporary backup copies in backup storage. An arbitrary directory named `backups` elsewhere,
or an unrelated file in that directory, must not become a quota bypass. Account across all
environments just as the cache budget currently does.

Public reporting changes:

- Keep the meaning of existing physical totals: `size_bytes` and GC's `after_bytes` still
  describe disk usage including backups.
- Add `budget_bytes` and `backup_bytes` to the existing doctor cache-size data. Compare
  `budget_bytes`, not the physical total, with `limit_bytes`.
- Add `budget_before_bytes`, `budget_after_bytes` and `backup_bytes` to `barca gc` JSON.
  Its limit and low-watermark target apply to budgeted bytes. The existing `held` category
  `backups` remains visible and is labeled as outside that budget in human output.
- Add an experimental `disk_usage` summary to `barca status` JSON and its human output,
  with `size_bytes`, `budget_bytes`, `backup_bytes` and `limit_bytes`. Its scope is the
  project's `.barca/` across environments, even when status selects particular nodes.
  Unmeasured byte counts are `null`, displayed as unmeasured, rather than guessed as zero.
  Server-side status describes the server's project storage.

Status uses the maintained GC accounting and bounded backup-directory measurements. It must
not add a full artifact walk to every status computation. The `/state` node array is
unchanged by this RFC; no disk accounting work is added to that polling route. Explicit
doctor/GC measurements may still perform their documented full survey.

Update RFC-0012's scope: the configured cache budget covers `.barca/` excluding recognized
metadata backup storage. Available filesystem bytes and physical totals still include it.
Also correct the remote manual's claim that `.barca/artifacts/` has no size cap.

### 4.4 Boundaries that stay the same

The worker and artifact-transfer protocols, cache keys, observation revisions and per-key
freshness do not change. No user Python import is needed to resolve this setting or report
disk usage. Backup data and sidecar formats, writer identity, explicit restore and pending
upload recovery retain their existing owners and compatibility.

## 5. Verification

- Parse and precedence tests cover defaults, units, `off`, invalid values and overflow.
- A controllable private clock proves admission before copy/hash/upload, interval boundaries,
  failed attempts, skipped-but-still-dirty progress, restart evidence, wall-clock rollback
  and concurrent callers. No public
  test-only timing setting is introduced.
- A real server with frequent scheduled and API runs produces at most one routine backup
  attempt per interval. A recorder checkpoint cannot bypass it. Test final shutdown with a
  long interval and with `off`, plus abrupt cancellation and a stalled upload within the
  existing stop budget. Preserve standalone final backups and server-client suppression.
- Storage tests cover all environments, the arithmetic of budgeted/backup/physical bytes,
  unknown measurements, unrelated files, pending uploads and unchanged restore behavior.
- Contract snapshots, configuration/manual/site docs and doctor/GC/status schemas move
  together in the implementation PR. A design-only merge does not claim these are shipped.

## 6. Cost and tradeoffs

Backups remain whole uncompressed database copies, with whole-copy hashing and upload.
For database size D and routine interval T, changed-state routine upload volume can approach
D/T, plus final backups and retries. At 1.7 GB, one copy per minute is roughly 102 GB/hour;
one per hour is roughly 1.7 GB/hour. These are byte-volume estimates, not measured throughput.

Five local copies of a 1.7 GB database occupy about 8.5 GB, outside the proposed cache budget.
The live database, WAL and an in-progress temporary copy require additional space. Excluding
backups from the budget preserves restore availability but makes free-space monitoring
necessary; the displayed physical total must make this cost apparent.

Measure copy/hash/upload bytes and elapsed time on a changed large database, and prove that
not-due requests perform none of those operations. A longer cadence increases the recovery
window; graceful shutdown is a best effort, not protection against a lost VM or forced kill.

Incremental/page backups are heard, not planned in this RFC. Configurable retention and
automatic restore are also outside it. The separate #706 store-copy inspection work must
bound repeated artifact hashing and remote probes before store-enabled status is usable at
the reported scale; this backup policy does not establish that result.
