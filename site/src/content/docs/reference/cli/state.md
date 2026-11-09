---
title: "barca state"
description: "List the metadata backups, put one back, or start the database over."
---

With a remote store (`[remote] uri` or `BARCA_REMOTE_URI`), `get` and `run` back the metadata
DB up on a one-minute checkpoint and when they end, and `serve` every minute it changed: to
`.barca/backups/<name>.db` and `<uri>/<env>/state/backups/<name>.db`, five copies kept in each,
each with a `<name>.json` sidecar (`run_id`, `rows`, `digest`, `barca_version`, `db_format`,
and from 0.29 `host` and `project`: the machine and project directory that took it). Nothing is
downloaded before a run. A store is one machine's backup: when another machine's backup is
newer than this one's own, a backup or `barca state backups` warns once, pruning keeps each
machine's newest backup, and `restore` without `--from` picks this machine's.

## Usage

```
barca state backups [--json]
barca state restore [--from N|NAME|PATH]
barca state reset [--yes]
```

```bash
barca state backups                  # newest first: # NAME WHERE SIZE ROWS RUN HOST
barca state backups --json           # {backups: [{index, name, where, size_bytes, run_id, rows, digest, agree, barca_version, db_format, host, project, mine}], local_dir, store}
barca state restore                  # put this machine's newest backup back (the newest, if it has none)
barca state restore --from 1         # the newest, whichever machine took it
barca state restore --from 2         # by index; or --from 2026-10-14T18-02-11.482Z-3f9a1c2e
barca state reset --yes              # move .barca/metadata.db aside to metadata.db.reset-<UTC>, start empty
barca state restore --from .barca/metadata.db.reset-2026-10-20T18-02-11.482Z  # undo a reset
```

`restore` checks the copy first (a whole SQLite file, integrity, barca's tables, a usable
format), the local copy before the store's, then moves the current database to
`.barca/metadata.db.pre-restore` and puts the copy in place; one line on stderr says what it
restored and from where, and `--json` adds `{restored, from, source, pre_restore, rows, run_id, host, project}`
on stdout. It never runs by itself. Exit 2: no backup, an unknown `--from`, or a copy that fails
the check (nothing is changed). Exit 3: `barca serve` is running on the environment. See
[Remote storage](/reference/remote-storage/#backups-and-restore).

`reset` (experimental) needs no store: it moves the database and its `-wal` and `-shm` aside to
`.barca/metadata.db.reset-<UTC>` in the same directory and creates an empty one, for a database
that cannot be opened or migrated or a history you want to start over. Nothing is deleted and
artifacts are left alone. It asks first in a terminal; `--yes` skips the question and is required
when stdin is not a terminal (exit 2 otherwise). `--json` prints `{database, moved_to, moved,
undo}`. Exit 3: a run is in progress on this machine, or `barca serve` is running on the
environment (use its UI's **Reset database** button or `POST /state/reset`). Undo with
`barca state restore --from <the moved file>`, which also takes any database file's path.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
