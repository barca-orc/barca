# Metadata backups and restore

This topic owns the backups of the metadata database: when they are taken, what one contains,
what `barca state backups` lists and `barca state restore` does, and how a server's backups
differ. The store they go to is `barca docs remote`.

Each machine works on its own `.barca/metadata.db`, and it is the truth for that machine:
nothing is ever downloaded before a run. With a store configured by `[remote] uri` or
`BARCA_REMOTE_URI`, the database is backed up:

- by `barca get` and `barca run`, on a routine checkpoint when the run recorded something
  since the last backup, and once when the run ends. A run that recorded no new result takes no
  backup at its end: nothing failed or was uploaded, and every step it ran was a sensor whose
  value the store already held (a warm `get` whose data did not change). Its run row goes with
  the next backup;
- by `barca serve`, at its routine interval when the database changed, and once when it shuts down
  (not with `--read-only`).

Routine checkpoints default to one minute. Set `[remote] backup_interval = "1h"` or
`BARCA_BACKUP_INTERVAL=1h` to limit routine work to once an hour per database. The environment
variable overrides TOML. Values are positive whole seconds, optionally suffixed `s`, `m`,
`h` or `d`, or `off`; surrounding whitespace is ignored. Empty, zero, negative, fractional,
compound, overflowing or unknown durations, and non-string TOML values, are usage errors
(exit 2). Restart the server after changing the setting.

Server timer ticks, recorder checkpoints and the end of server-owned runs share one limit
before copying, hashing or uploading. Attempts consume the interval even on failure; skipped
requests retain dirty progress for a later checkpoint. Missed ticks are skipped. A server
restart retains the previous attempt time. `off` disables routine copies and retries, while
standalone commands' final backups and the server's graceful-shutdown backup remain enabled.
Final attempts bypass the interval and update its timestamp. An abrupt kill cannot promise
a final backup, and graceful shutdown retains its bounded backup budget.

**0.36.0 upgrade:** rapid consecutive served-run completions no longer force separate backups.
A completed served run can be recorded locally without yet being in the remote metadata
backup. Standalone final backups retain the behavior described above.

Backups remain whole uncompressed database copies, including whole-copy hashing and upload.
At 1.7 GB, changed-state routine uploads can approach 102 GB/hour at `1m`, or 1.7 GB/hour at
`1h`, plus final backups and retries. Five local copies occupy about 8.5 GB in addition to
the live database, WAL and temporary copy. These are byte-volume estimates, not throughput
measurements. A longer interval increases the recovery window.

A machine with a server configured (`[server] url` or `BARCA_SERVER`) takes no backups, with or
without `--local`: the server is the ledger and backs up its own database ("With a server" below).

A backup whose bytes would equal the newest one is not taken. None is taken under
`BARCA_REMOTE=off`, or when only `BARCA_ARTIFACT_URI` / `[remote] artifacts_uri` name the store.

A backup is the database's main file, copied right after its write-ahead log is folded in,
so a write in progress never produces a torn copy. Only the fold holds the database's lock: a read
of the folded file, started before the lock is released, keeps every checkpoint out of the main
file until the copy is hashed and written, so other barca processes (the CLI beside a `barca
serve` that is backing up) keep reading and recording meanwhile. It is written
locally first, to `.barca/backups/<name>.db` (for a named environment
`.barca/envs/<env>/backups/`), named like `2026-10-14T18-02-11.482Z-3f9a1c2e.db`: the UTC time
to the millisecond and a suffix from this machine's host name, the database's path and the
copy's digest, so directories that share a store location never replace each other's copies.
Its sidecar is `2026-10-14T18-02-11.482Z-3f9a1c2e.json`. Names barca 0.26.0 wrote, to the second
(`2026-10-14T18-02-11Z.db`), are still listed, restored and pruned. The sidecar:

```json
{"run_id": "7f30b3e9", "rows": 412, "digest": "<sha256>", "barca_version": "0.29.0", "db_format": 1,
 "host": "laptop", "project": "/home/me/analytics"}
```

`run_id` is the run that took it (`null` for `barca serve`), `rows` the run and
materialization rows in the copy, `db_format` its metadata format, `host` and `project` the
machine and the project directory that took it (absent in a sidecar barca 0.28 or earlier
wrote; such a backup is still listed and restored, with no host). Then both files are
uploaded to `<uri>/<env>/state/backups/` (one `python -m barca._state backup` per backup). Each
place keeps the five newest backups, pruned after a successful write; only files named like a
backup are ever removed. In the store, the newest backup of every *other* host and project
directory taken in the last week is kept as well (at most five of those), so a second machine
writing to the prefix cannot rotate out the first one's last backup. At the end of a run stderr says
`[barca] backed up the metadata DB (49.2 KB) to <uri>/<env>/state/backups/<name> in 0.31s`.

A backup that does not reach the store never fails the run. At the end of a run it is a warning,
`[barca] warning: the metadata backup was not uploaded: backup to <store> failed: <why>; the run
is recorded on this machine, a copy is in .barca/backups, and the next backup uploads it`, the
exit code stays 0 and the run JSON's `uploads.error` says `backup to <store> failed: ...`. The
local copy carries a `<name>.unsent` marker until the store confirms it, and the next backup
uploads it even if nothing changed. Minute checkpoints that fail print one warning per run, after
three in a row.

List the backups, newest first, local and in the store merged:

```bash
barca state backups
barca state backups --json
```

```
#  NAME                                  WHERE  SIZE     ROWS  RUN       HOST
1  2026-10-14T18-02-11.482Z-3f9a1c2e.db  both   49.2 KB  412   7f30b3e9  laptop (this machine)
2  2026-10-14T18-01-10.907Z-3f9a1c2e.db  both   48.6 KB  401   7f30b3e9  laptop (this machine)
```

`where` is `local`, `store` or `both`; `agree` (JSON) says whether both copies have the same
size and digest (`null` with one copy). `host` and `project` (JSON) say who took it, and
`mine` whether it was this host and project directory (`null` for a backup from barca 0.28 or
earlier). The `HOST` column shows another project directory on this host as `host:/path`. A store that cannot be listed is a warning on stderr and
`store_error` in JSON; the local copies are still listed.

Put one back, only when you ask:

```bash
barca state restore                  # this machine's newest (the newest, if it has none)
barca state restore --from 1         # the newest, whichever machine took it
barca state restore --from 2         # by index in barca state backups
barca state restore --from 2026-10-14T18-01-10.907Z-3f9a1c2e
```

Stop `barca get` and `barca run` in the project first. `restore` copies the chosen backup next
to the database (the local copy when there is one, else the store's) and checks it before
touching anything: a whole SQLite file, `PRAGMA integrity_check` answering `ok`, barca's `runs`
and `materializations` tables, and a metadata format this barca can use (an older one is
migrated). Then the current database, its write-ahead log folded in, moves to
`.barca/metadata.db.pre-restore` (replacing an older one) and the copy takes its place. One line
on stderr says what happened:
`[barca] restored 2026-10-14T18-01-10.907Z-3f9a1c2e.db from the local copy (.barca/backups/...), 401 rows,
taken by run 7f30b3e9; the database it replaced is .barca/metadata.db.pre-restore`.
With `--json`, stdout also gets `{restored, from, source, pre_restore, rows, run_id, host,
project}`.

Without `--from`, `restore` takes this machine's newest backup (same host and project
directory), and says so when newer ones came from elsewhere:
`[barca] restoring this machine's newest backup, 2026-10-14T18-01-10.907Z-3f9a1c2e.db (#3); the
2 newer ones were taken by desktop. barca state restore --from 1 restores the newest`. With
none of its own (a fresh machine taking over), it takes the newest and names who took it.

- No backup at all, an unknown `--from`, or a copy that fails the check: exit 2, nothing changed.
  A copy that fails names the next newest (`barca state restore --from 3`).
- A copy written by a newer barca in a format this one cannot use: exit 2; upgrade.
- `barca serve` running on this environment (it holds `.barca/serve.lock`): exit 3, naming the
  server's pid, host and address; stop it first.

To undo a restore, with no barca command running, put the kept file back:

```
mv .barca/metadata.db.pre-restore .barca/metadata.db
rm -f .barca/metadata.db-wal .barca/metadata.db-shm
```

**Start over (`barca state reset`, experimental).** When the metadata database cannot be used
(the UI says `Can't load pipeline: Database error: ...`, a format this barca cannot migrate, a
corrupt file), or you want an empty history, reset it. Nothing is deleted:

```bash
barca state reset                    # asks first in a terminal
barca state reset --yes              # no question; required when stdin is not a terminal
barca state reset --yes --json       # {database, moved_to, moved, undo}
```

The database and its `-wal` and `-shm` move to `.barca/metadata.db.reset-<UTC>` (and
`<that>-wal`, `<that>-shm`) in the same directory, and an empty database takes its place. Run
history, cache records, partition keys and schedule pauses start over, so the next `get`
recomputes what it needs. A database that cannot be opened moves as it is. One line on stderr
says what moved and how to undo it:
`[barca] reset the metadata database: the old one is .barca/metadata.db.reset-2026-10-20T18-02-11.482Z, .barca/metadata.db is new and empty. Undo: barca state restore --from .barca/metadata.db.reset-2026-10-20T18-02-11.482Z`.

Artifacts are left on disk, and a second line says how much:
`[barca] the 129 KB of artifacts under .barca/artifacts stay on disk, and no record names them now: the cache size limit evicts them first, barca gc --max-size off deletes them now, and until then the undo above brings them back`.
No record names them, so they are the first thing the cache size limit evicts (`barca docs disk`); `barca gc --max-size off` deletes them now. The same holds after `barca
state restore` for the artifacts the restored copy does not name. The moved-aside databases
themselves are never deleted: remove a `metadata.db.reset-<UTC>` yourself when you no longer
need it (`barca gc` lists them under `held` as `db_copies`).

- Not in a terminal without `--yes`: exit 2, nothing changed.
- A run in progress on this machine (`running` in `barca history`, its process alive): exit 3,
  naming the run.
- `barca serve` running on this environment: exit 3. Reset from its UI instead (the **Reset
  database** button, on the error panel of a database it cannot use), or `POST /state/reset`: the server
  refuses it with 409 while one of its runs is pending or running, and with 403 under
  `--read-only`.

To undo, with `barca serve` stopped, restore the moved file: `barca state restore --from
.barca/metadata.db.reset-<UTC>` checks it and puts it back (the empty database it replaces moves
to `metadata.db.pre-restore`). `--from` takes the path of any barca database file.

**A wiped laptop or a fresh container.** Clone the project, set the same `BARCA_REMOTE_URI`
and credentials, and run `barca state restore`: it lists the store's backups (shown as `store`)
and fetches the chosen one. That is the only time a database comes down from the store. After
it, `barca get` serves cached results from the bucket.

**Two machines on one store with no server** is not supported. Each backs up to the same prefix
and the five-copy retention interleaves their histories; nothing coordinates them. Use a server
(RFC-0007) to share state, or give each machine its own `[remote] uri`. barca notices: when a
backup finds that another host (or another project directory) took a backup after this one's
previous backup, or `barca state backups` lists one newer than this machine's newest, stderr
gets one warning per command:
`[barca] warning: <store> also holds backups from desktop (the newest 2026-10-14T18-05-00.120Z-9c1d2e3f.db), taken after this machine's own: two machines are backing up to one store, and nothing coordinates them. barca state restore picks this machine's newest backup unless given --from. To share state between machines, use a server (barca docs server-mode); otherwise give each machine its own [remote] uri`.
A machine with no backup of its own yet (one restoring another's) is not warned; once both
write, both are. Pruning keeps each machine's newest backup, and `restore` without `--from`
picks this machine's, so neither loses its last copy to the other.

**Upgrading from the shared history.** barca 0.26.0 removed `optimistic` mode: `[remote] state`,
`[remote] state_uri`, `[remote] push_retries`, `BARCA_STATE`, `BARCA_STATE_URI`,
`BARCA_PUSH_RETRIES`, the pull before each run and the conditional push after it, and the
`metadata.db.prev` copy a pull kept. A `barca.toml` or environment that still sets one of them
is a usage error (exit 2) naming the setting, even under `BARCA_REMOTE=off`: remove it. A single
machine gets the same protection from backups with no setting. A team that shared history
through the bucket moves to a server. Leftover local files from those versions
(`metadata.db.base`, `metadata.db.prev`, `metadata.db.pull-*`, `metadata.db.push-*`) are not
used and can be deleted.

`barca get --json` reports a result as it does without a store: a pointer
(`{"_barca_artifact": {"path", ...}}`) to the copy in `.barca/artifacts/`, and `-o value` reads
that copy. A final output that is only in the store is downloaded first.

## With a server

The server backs up its database to the store (`<uri>/<env>/state/backups`, `barca docs
remote`); the machines that use it take no backups, with or without `--local`, and print no
`backed up the metadata DB` line. A client's history that matters is on the server, and a
client's copies in the server's backup set would rotate the server's own out of it and be
candidates for a restore of the server. Backups barca 0.29.1 and older took on a client stay
in the store (pruning keeps every machine's newest; `barca state backups` lists them with their
`host` and `project`), and you can delete them there. `barca state restore` without `--from`
on the server takes the server's own newest and names any newer ones a client took; on a server
with no backup of its own (a new host) it takes the newest and names who took it, so check
`barca state backups` and pass `--from` with the server's.

See also: `barca docs remote`, `barca docs server-mode`, `barca docs layout`, `barca docs contract`.
