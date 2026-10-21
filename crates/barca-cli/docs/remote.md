# Remote storage: this machine's backup and read-through cache

Point barca at a bucket and every result is uploaded there, and the metadata database (run
history and what is cached) is backed up there. The store is this machine's backup and
read-through cache: a lost laptop or a wiped container gets everything back with
`barca state restore`. Configuration is one variable plus the credentials your cloud's tools
already use. No `barca.toml` is needed.

```bash
pip install "barca[s3]"                      # or barca[gcs], barca[azure], barca[remote] (all)
export BARCA_REMOTE_URI=s3://my-bucket/barca/my-project
barca get total                              # results and a history backup go to the bucket
```

Sharing state between machines means a server (RFC-0007), not a shared bucket. Up to 0.25,
`optimistic` mode pulled a shared history from the bucket before each run and pushed it after;
barca 0.26.0 removed it (see "Upgrading from the shared history" below).

## Each cloud

Amazon S3, and S3-compatible stores (Cloudflare R2, MinIO):

```bash
export BARCA_REMOTE_URI=s3://my-bucket/barca/my-project
export AWS_ACCESS_KEY_ID=...                 # or AWS_PROFILE, or the machine's instance role
export AWS_SECRET_ACCESS_KEY=...
export FSSPEC_S3_ENDPOINT_URL=https://<account-id>.r2.cloudflarestorage.com   # R2/MinIO only
```

Google Cloud Storage:

```bash
export BARCA_REMOTE_URI=gs://my-bucket/barca/my-project
export GOOGLE_APPLICATION_CREDENTIALS=/path/to/service-account.json   # or: gcloud auth application-default login
```

To give barca its own key instead of the machine's default credentials, use
`FSSPEC_GCS_TOKEN=/path/to/key.json` (and `FSSPEC_GCS_PROJECT=my-project` if the key does not
name one). Results and backups always use the same credentials.

Azure Blob Storage / ADLS Gen2:

```bash
export BARCA_REMOTE_URI=abfs://my-container/barca/my-project
export AZURE_STORAGE_CONNECTION_STRING="DefaultEndpointsProtocol=https;AccountName=myaccount;AccountKey=...;EndpointSuffix=core.windows.net"
```

Instead of a connection string: `AZURE_STORAGE_ACCOUNT_NAME` with `AZURE_STORAGE_ACCOUNT_KEY` or
`AZURE_STORAGE_SAS_TOKEN`; or `AZURE_STORAGE_ACCOUNT_NAME` alone, which signs in with
`DefaultAzureCredential` (`az login`, a managed identity, or `AZURE_CLIENT_ID` /
`AZURE_CLIENT_SECRET` / `AZURE_TENANT_ID`). `abfss://my-container@myaccount.dfs.core.windows.net/...`
names the account in the URI instead.

## Any other storage option

Every option of the underlying filesystem (s3fs, gcsfs, adlfs) can be set as an environment
variable, `FSSPEC_<PROTOCOL>_<OPTION>=<value>`, with protocol `S3`, `GCS` or `ABFS`:
`FSSPEC_S3_ENDPOINT_URL`, `FSSPEC_GCS_PROJECT`, `FSSPEC_ABFS_ACCOUNT_NAME`. This is fsspec's own
convention, so other fsspec tools on the machine read the same settings.

## Check that it works

1. `barca get <asset>`, then `barca status <asset> --json`: each `cache.artifact` starts with
   your URI.
2. `barca state backups` lists the history backup the run took, in `both` places.
3. In a fresh clone with no `.barca/`, `barca state restore` fetches that backup from the bucket;
   the same `barca get` then reports `steps_executed: 0`, served from the bucket.

Every execution checks the artifact store before starting workers, including runs triggered
through `barca serve`, unless the store was checked in the last 5 minutes (see "How often the
store is checked"). The read-only startup
probe is limited to 10 seconds, or `transfer_timeout` when shorter. Cloud stores
must allow listing the bucket/container root and its backend's existence check;
object read/write access or prefix-only listing is insufficient. This check
verifies reachability, not upload permission. A new directory store checks its
existing ancestor; the probe creates nothing.
Before the end-of-run upload wait, barca reports the artifact count, destination
and per-attempt timeout; longer waits report progress every 10 seconds. Failed
uploads report their attempt count and remain unrecorded.

A failure to reach the bucket stops the run before any step: exit 3, naming the location, with
the cloud's own error (expired login, access denied, a bucket that does not exist), and the
remediation lists what to check: the URI, that the bucket exists (barca never creates it), the
endpoint, the credentials, and `BARCA_REMOTE=off` to run one command without the store. A missing extra says which one to install.

A warning that a storage library repeats on every operation (aiohttp's `Could not parse .netrc
file` under adlfs, when `~/.netrc` is malformed) is printed once per run, then counted:
`[barca] 79 more: Could not parse .netrc file`. This applies while logging is unconfigured; if
the project configures logging, every record is printed. See `barca docs agents`, "Repeated
warnings".

## In barca.toml instead

The same settings can live in the project, so everyone who clones it gets them:

```toml
[remote]
uri = "s3://my-bucket/barca/my-project"

[remote.storage_options.s3]       # any s3fs option; [...gcs] for gcsfs, [...abfs] for adlfs
endpoint_url = "https://<account-id>.r2.cloudflarestorage.com"
```

Keep secrets out of it: credentials still come from the environment. Precedence, highest first:
`BARCA_REMOTE_URI` over `[remote].uri`; `BARCA_STORAGE_OPTIONS` (JSON keyed by protocol) over
`[remote.storage_options.*]` over `FSSPEC_*` variables. Every key is in the configuration
reference: https://barca.sh/reference/config/

## Storage for one process

Set `BARCA_REMOTE=off` to use local artifacts and local history without editing
`barca.toml`. It overrides configured artifact URIs and remote options, including environment
overrides, and no backup is taken. `--env` still selects the local environment. Unset or empty
values keep the configured storage; other values are usage errors.

```bash
BARCA_REMOTE=off barca get total pipeline.py
BARCA_REMOTE=off barca run deploy pipeline.py
BARCA_REMOTE=off barca serve pipeline.py
```

Existing history is kept. Remote results recorded in that history are not fetched or inspected
while off. Barca uses a previously recorded local result when available, otherwise computes one
locally; subsequent runs reuse it. `barca status --json` reports local cache paths. Historical
materialization paths still describe where those earlier results were stored. User code can
still perform its own network I/O. Empty URI overrides remain unset; `BARCA_REMOTE_URI=` does
not turn off a TOML remote root.

## What barca keeps in the bucket

```
<uri>/<env>/artifacts/<node>/<run_hash>.<ext>   one file per result
<uri>/<env>/state/backups/<name>.db              the five newest backups of the metadata DB
<uri>/<env>/state/backups/<name>.json            what each backup holds (its sidecar)
```

`<env>` is `default` unless you pass `--env` (`barca docs cache`). Barca reads and writes
objects and reads their metadata. It lists the bucket before it recomputes a result whose
artifact is missing (see "Failures") and when it lists or prunes backups, and it deletes only
backups older than the five newest. In practice that is `s3:GetObject`, `s3:PutObject`,
`s3:DeleteObject` and `s3:ListBucket` on S3, the Storage Object User role on GCS, and Storage
Blob Data Contributor on Azure.

A `{uri}/{env}/state/metadata.db` object left by barca 0.25 or earlier (the shared history) is
ignored: never read, never deleted. Remove it yourself once you no longer need it.

## Backups and restore

Each machine works on its own `.barca/metadata.db`, and it is the truth for that machine:
nothing is ever downloaded before a run. With a store configured by `[remote] uri` or
`BARCA_REMOTE_URI`, the database is backed up:

- by `barca get` and `barca run`, on a one-minute checkpoint when the run recorded something
  since the last backup, and once when the run ends. A run that recorded no new result takes no
  backup at its end: nothing failed or was uploaded, and every step it ran was a sensor whose
  value the store already held (a warm `get` whose data did not change). Its run row goes with
  the next backup;
- by `barca serve`, every minute in which the database changed, and once when it shuts down
  (not with `--read-only`).

A backup whose bytes would equal the newest one is not taken. None is taken under
`BARCA_REMOTE=off`, or when only `BARCA_ARTIFACT_URI` / `[remote] artifacts_uri` name the store.

A backup is the database's main file, copied under the database's lock right after its
write-ahead log is folded in, so a write in progress never produces a torn copy. It is written
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
No record names them, so they are the first thing the cache size limit evicts (`barca docs
cache`, "Disk use"); `barca gc --max-size off` deletes them now. The same holds after `barca
state restore` for the artifacts the restored copy does not name. The moved-aside databases
themselves are never deleted: remove a `metadata.db.reset-<UTC>` yourself when you no longer
need it (`barca gc` lists them under `held` as `db_copies`).

- Not in a terminal without `--yes`: exit 2, nothing changed.
- A run in progress on this machine (`running` in `barca history`, its process alive): exit 3,
  naming the run.
- `barca serve` running on this environment: exit 3. Reset from its UI instead (the **Reset
  database** button, on the error panel and in the sidebar), or `POST /state/reset`: the server
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

## What happens during a run

- Nothing is downloaded before the run: the local metadata DB is used as it is.
- Steps always read and write local files under `.barca/artifacts/`. A helper process
  uploads each artifact in the background as soon as its step finishes. A sensor that returned
  what the store already holds at that location, by this machine's record of a confirmed upload
  (its data did not change), is not uploaded again; an asset is uploaded whenever it runs.
- A cache hit whose file is only in the store is downloaded just before the first step that
  reads it eagerly. Cached intermediates nothing reads are never downloaded, and neither are
  parquet results that are only read lazily (below).
- Before results are recorded, barca waits for every upload. A result whose upload failed is
  still recorded, and kept on this machine as a pending upload (see "When an upload fails
  after the steps succeeded"); every other row points at an object that is in the store.
- The metadata DB is backed up on the minute checkpoint and at the end, unless the run
  recorded no new result (see "Backups and restore").
- Within 5 minutes of a successful startup check of the same store, a run that uploads and
  fetches nothing starts no transfer helper at all (next section).

stderr reports each part, so remote cost is visible. The first line names the environment
and the store, printed once at startup by `get`, `run` and `serve` whenever a store is
configured (`barca docs cache`, "Environments"):

```
[barca] env default (built-in default); artifact store s3://bucket/proj/default/artifacts
[barca] fetched 1 cached artifact (688.9 KB) in 0.1s
[barca] uploaded 2 artifacts (688.9 KB); waited 0.0s at end of run
[barca] backed up the metadata DB (49.2 KB) to s3://my-bucket/barca/my-project/default/state/backups/2026-10-14T18-02-11.482Z-3f9a1c2e.db in 0.31s
```

## How often the store is checked

A run checks the store at startup (`[barca] checking artifact store`), and a `get` asks it about
each cached target ("`cached` and `unsynced` with a store"). Both need the transfer helper,
whose start costs a few hundred milliseconds even when nothing moves. A successful startup
check is remembered for 5 minutes, per store URI and environment, in `.barca/gc-state.json`. A
`get` or `run` within that window:

- prints no `checking artifact store` line, and starts no transfer helper until it has
  something to upload or fetch. A warm `get` whose targets are cached here, or whose sensors
  returned what the store already holds, starts none and does not ask the store about its
  targets;
- once it uploads or fetches, starts the helper then, and a `get` asks the store about its
  cached targets as usual. Before its first fetch it makes the startup check (the `checking
  artifact store` line), so a store that is gone fails the run (exit 3) rather than reading as
  missing results to compute again. Uploads do not wait for a check: a failed one is kept
  pending ("When an upload fails after the steps succeeded").

These always check the store at startup: the first run after the window ends, a different
store URI, environment or storage options, a run with results pending upload from an earlier
run, `--refresh`, `--refresh-all` and `get -o url`. `barca doctor` and `barca gc` look at the
store whenever they run. A failed transfer ends the window, so the next run checks. Only a
startup check starts a window; runs within it do not extend it.

So a store object deleted or changed behind barca's back is noticed within 5 minutes, by the
first run that checks, rather than by the very next run. `barca state reset` and
`barca state restore` end the window too (they clear `.barca/gc-state.json`).

## When an upload fails after the steps succeeded

The work is done when the steps are. If every step succeeded and only an upload to the store
failed (permission denied, an expired token, a store that went away mid-run), or only the
upload of the history backup did, the run is a success:

- it exits 0 with the usual result, and stderr has one warning naming what was not published
  and why:
  `[barca] warning: 2 results not uploaded to s3://my-bucket/barca/my-project/default/artifacts
  (pipeline.py:numbers, pipeline.py:total): upload to ... failed (1 attempt): PermissionError: ...;
  every step succeeded, the results are kept on this machine (`unsynced`, local only) and the
  next command that uses the store uploads them`.
  A failed backup upload prints `[barca] warning: the metadata backup was not uploaded: ...`;
  the next backup uploads it;
- the JSON result gains `"uploads": {"pending": 2, "error": "...", "nodes": [...]}` (absent
  when nothing is pending);
- the results are recorded, with their store locations, and are cache hits on this machine,
  served from `.barca/artifacts/`. `barca status` shows them `unsynced` with WHY `local only`
  (`"state": "unsynced"` and `"cache": {"reason": "local_only"}` in JSON), and
  `barca get --dry-run` lists them;
- the next `barca get` or `barca run` that opens the store uploads them before its own work:
  `[barca] uploading 2 pending results to <store> from an earlier run`, then
  `[barca] uploaded 2 pending results`. If they fail again the run goes on, still exits 0 if
  its steps succeed, and reports them again. Afterwards `barca status` shows them `cached`.

Pending uploads are kept in the metadata database, so they survive the process and a reboot;
only the machine that computed them retries them, from its own `.barca/artifacts/`. There is no flag to give up on one. A pending result whose local file was
deleted is dropped with a warning (nothing is left to upload); its result is then missing like
any deleted artifact and is computed again when something needs it.

A store that fails before any step runs is still an error, exit 3: the startup check
(`[barca] checking artifact store`), a cached artifact that cannot be fetched. Nothing has been computed then, so there is nothing to keep.

## `cached` and `unsynced` with a store

With a store, `barca status` calls an unpartitioned node `cached` only when this machine's copy,
the store's object and the digest recorded with the result all agree. It asks the store about
every such node with one `python -m barca._state stat`. Anything else is `unsynced`, with the
reason in `cache.reason` (WHY in the table):

| `cache.reason` | WHY | Meaning | What `barca get <node>` does |
|---|---|---|---|
| `local_only` | local only | here, not in the store (or its upload is pending) | uploads this machine's copy |
| `store_only` | store only | in the store, not here | fetches the store's copy |
| `mismatch` | mismatch | both are there and differ, or this machine's copy is not the recorded one | when this machine's copy is the recorded one, uploads it again; otherwise fetches the store's copy and records its digest ("Checking a local copy against the store") |

A result in neither place is `stale` with reason `artifact_missing`. A store that cannot be
asked leaves the states as they are, with a note in `cache.detail`.

`barca get` repairs an `unsynced` target without running a step and without changing the exit
code (a `get` within 5 minutes of a store check that transfers nothing does not ask; see "How
often the store is checked"). For each unpartitioned target served from cache whose local copy has the recorded digest,
the transfer helper asks the store for the object: when it is missing, stderr says
`[barca] pipeline.py:total: not in the artifact store; uploading this machine's copy to <path>`;
when it has another size (or other bytes, for a store that is a plain directory),
`[barca] pipeline.py:total: the artifact store holds different bytes; uploading ...`, followed by
`[barca] warning: pipeline.py:total produced different bytes for the same code and inputs; the
function is not deterministic` and `steps[].bytes_changed: true` on that step. A `store_only`
target is fetched as its output is returned. A local copy that is not the recorded one is
replaced by the store's when it is read ("Checking a local copy against the store"). A repair
upload that fails is a pending upload like any other.

Known limits: object stores give no content checksum, so a store object of the same size with
other bytes is not detected there (a plain-directory store is compared by SHA-256). A partitioned
node is `unsynced` only when one of its keys has a pending upload. `get` repairs its targets,
not their whole upstream cone.

## How steps read inputs from the store

A result produced on this machine is already on disk, and steps read that file. For a cache hit
whose file is only in the store, the input's annotation decides how many bytes move
(`barca docs types`):

- A parquet input annotated `duckdb.DuckDBPyRelation` or `pl.LazyFrame` is read in place:
  only the byte ranges the step's query touches are fetched, and nothing is downloaded. A query
  over one column of eight fetches about that column's share of the object; a selective filter
  skips row groups. This applies when every step in the phase that reads the result is lazy.
  The ranges fetched are kept in `.barca/range-cache/<run_hash>-<version>/`, so a later run on
  this machine that reads the same ranges fetches nothing, and one that reads more fetches only
  the ranges no earlier read fetched. The directory is keyed by the result's run hash and the
  store object's version (its ETag or generation, size and modified time), so a rewritten
  object is never served from the old one's ranges. The cache size limit counts these
  directories and evicts them first, least recently used first (`barca docs cache`, "Disk
  use"); a range that is gone is fetched again.
- Every other input (no annotation, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table`, json,
  pickle) is downloaded once into `.barca/artifacts/` and read from there by this run and
  later ones.

For a large upstream that a step filters, projects or aggregates, annotate the input as lazy.
DuckDB reads barca's artifacts through a `barca<protocol>://` filesystem registered on its
connection, so `s3://`, `abfss://` and `gs://` URLs in your own SQL keep using DuckDB's own
extensions and credentials.

## Checking a local copy against the store

When an artifact is uploaded, the SHA-256 of the local file is recorded with it in the
metadata DB. Barca uses that hash to decide whether a local copy is current:

- A copy already in `.barca/artifacts/` is hashed the first time a run reads it. If it does not
  match (edited by hand, or left from before a refresh), it is
  replaced by the store's copy and reported as a fetch.
- A downloaded artifact is hashed too. If the store's copy does not match the recorded hash,
  the run still uses it and prints a warning naming the step:
  `[barca] warning: pipeline.py:total: the copy at <location> is not the one this result was
  recorded with (another run overwrote it, or it was changed). Using it. Recompute with
  --refresh pipeline.py:total.` This is not an error and the exit code does not change: an
  artifact's path is `<node>/<run_hash>`, which identifies the computation and not the bytes,
  so a `--refresh`, or another project writing to the same store, overwrites the object.
  Without a server, the run then records the store copy's hash as the result's: the store's
  copy wins, so the warning is printed by this run only, and the next `barca get` and
  `barca status` find the result `cached`. That is also how a `--refresh` killed after its
  upload and before its result was recorded settles: the store and this machine hold the new
  bytes, the record names the old ones, and the next `get` that returns or reads the result
  records the new ones. `--refresh <file.py:name>` recomputes the step instead, uploads the new
  bytes over the object and records their hash. With a server, its ledger keeps its record
  (`barca docs server-mode`).

The JSON result carries the same finding, so a script or agent does not have to read stderr:

```bash
barca get total --json --fields id,status,artifact_mismatch
```

```json
{"steps": [{"id": "pipeline.py:numbers", "status": "cached", "artifact_mismatch": true},
           {"id": "pipeline.py:total", "status": "ran", "artifact_mismatch": true}], "...": "..."}
```

`steps[].artifact_mismatch` is `true` on the step the artifact belongs to and on every step that
read it as an input in this run; on every other step the key is absent. `steps[].warning` has
the text. It is reported per step and not in the top-level `warnings` array, which holds plan
warnings only (`barca docs contract`).

A mismatch and a missing object are different findings and never stand in for each other: a
copy with other bytes is used and flagged, as above; an object that is not in the store at all
is computed again with `reason: "artifact_missing"` (see "Failures").

Only artifacts a run reads are hashed, once per run. Not checked: a parquet input that is read
in place (only byte ranges are fetched), and results recorded before barca stored a hash. A
`--dry-run` does not contact the store, so it never reports a mismatch.

## Ctrl-C

Ctrl-C cancels a `barca get` or `barca run` at any point: while steps run, and while barca is
uploading, downloading, or backing up the history. The command exits 130 with the
`cancelled` error. It makes no difference whether the terminal sent the signal to every process
of the job or something sent SIGINT to barca alone. Barca, its workers and its helper processes
print no traceback, with or without a store and from the first moment of a run: a Ctrl-C while
the workers are still starting exits as promptly as any other.

SIGTERM, which a supervisor, a CI timeout or `docker stop` sends, does what Ctrl-C does:
everything below holds for it, a second SIGTERM counts as a second Ctrl-C, and the exit code
is 130, not 143.

1. **The first Ctrl-C cancels the run.** Steps and transfers in flight are stopped, what
   finished is recorded in this machine's history, the run as `cancelled`, and the run wraps
   up: with a store, it backs that history up.
2. **The wrap-up takes at most 10 seconds.** With a store that answers it takes a fraction of
   a second. If the backup has not finished by then (a slow, stalled or unreachable store) it
   is stopped, and stderr says so:
   `[barca] the metadata backup did not finish (not within 10s); the run is recorded on this
   machine and the next backup takes it`.
3. **A second Ctrl-C abandons the wrap-up at once** (the same line, with `stopped by a second
   interrupt`, whichever signal it was). A third changes nothing.

Stopping a helper process can take up to 2 seconds, so the command ends within a few seconds of
the last of these. The exit code is 130 in every case, never 3: a backup that fails during the
wrap-up is reported on stderr, not as an error.

What is left behind is always consistent:

- A step is recorded only once its artifact is confirmed in the store. A step whose upload
  was still in flight is not recorded, and runs again next time.
- No partial file is left. A download, and an upload into a store that is a directory, is
  written to a temp file beside its destination and renamed when whole; the temp file is
  removed when the transfer is stopped. An object store shows an object only once its upload
  has completed, so an interrupted upload leaves the previous object, or none. A local backup is
  written beside its final name and renamed when whole.
- Nothing is lost when the wrap-up does not finish. The record is in this machine's history,
  and the next `barca get` or `barca run` serves the finished steps from cache and backs the
  history up.

**What the run's record says.** `cancelled`, whenever the interrupt arrived before the run
ended. That includes a Ctrl-C during the end-of-run backup, when every step had finished: the
steps are recorded as finished and the run as `cancelled`. A Ctrl-C that arrives once the backup
has completed is too late to cancel anything: the run is `success` and the command exits 0.
A run with a failed step is recorded as `failed`, and may then still download an earlier output
to return; interrupted during that download, the command exits 130 and the record stays `failed`.

The end-of-run line (`[barca] <n>/<total> steps | done in <secs>s`) is about the steps. A Ctrl-C
that arrives after the last step finished, while artifacts upload or the history is backed up,
therefore follows a `done` line; the exit code and the error still say `cancelled`.

Workers are part of the terminal's job, so a Ctrl-C reaches them too. A worker acts on it only
while it runs a step: the step's code gets a `KeyboardInterrupt`, and the step is left out of
the cancelled run (it is neither recorded nor reported as failed). Barca stops the worker
right after, so do not rely on a step's `except KeyboardInterrupt` or `finally` code having
run. While a worker imports a step's module or waits for its next step, a Ctrl-C does nothing
in it, and barca stops it. While a worker is still starting it is not in the terminal's job at
all (it joins once it is ready), so a Ctrl-C does not reach it.

If barca itself was started with SIGINT ignored (`nohup barca ...`, a background job of a
script, cron, some process supervisors), its workers inherit that and keep it: a step does not
get a `KeyboardInterrupt`, as in any Python program started that way. A SIGINT sent to barca
still cancels the run, with exit 130, and barca stops the workers.

Barca's helper processes (the one that moves artifacts and the one that uploads backups) do
not act on Ctrl-C themselves. They are started outside the terminal's job (in a process group
of their own), so the terminal's Ctrl-C reaches barca only, which decides what it means and
stops them. If barca itself is killed (`kill -9`, out of memory), nobody is left to stop them,
so they watch for that: each exits on its own, at once and without output, and removes the temp
file it was writing.

## Settings

| `[remote]` key | Env var | Default | Meaning |
|---|---|---|---|
| `transfer_concurrency` | `BARCA_TRANSFER_CONCURRENCY` | 4 | Uploads/downloads in flight at once |
| `transfer_timeout` | `BARCA_TRANSFER_TIMEOUT` | 600 | Seconds one transfer attempt may run (counted from when it starts) |

## Failures

Transfers are retried up to 3 times (0.5s, 1s, 2s backoff) when the error looks transient:
dropped connections, timeouts, 5xx, 408 and 429. Missing objects, permission and
authentication errors, and other 4xx responses fail on the first attempt. An attempt that
exceeds `transfer_timeout` fails as stalled and is not retried.

- **Upload failed** after the step succeeded: a warning, not a failed run (exit 0). The result
  is recorded and kept here as a pending upload, and the next command that opens the store
  uploads it (see "When an upload fails after the steps succeeded"). Versions before this
  one recorded a `failed` row with `error_type = 'UploadError'` and exited 3.
- **Cached artifact missing from the store** (the object was deleted, and there is no copy in
  `.barca/artifacts/` either): if something needs to read it, its step is computed again and
  uploaded, with `reason: "artifact_missing"` and a warning on stderr; the run does not fail.
  If nothing reads it, it stays cached and is not recomputed (`barca docs cache`, "A cached
  result whose artifact is missing"). A local copy is enough: a store that is unavailable does
  not lose a cache hit whose file is in `.barca/artifacts/`.
- **The store itself is gone** (the bucket or container was deleted, its name is misspelled,
  the directory is not mounted, or the endpoint answers "not found" to everything): every
  object then looks missing, so before computing anything again barca lists the bucket,
  container or store directory, once per run. If that fails the run exits 3 with
  `could not fetch N cached artifact(s) from the artifact store: the store at <uri> is not
  there or cannot be listed`. Nothing is recomputed, nothing is uploaded, and no bucket,
  container or directory is created.
- **Credentials that can read and write but not list**: that listing is refused, and the same
  error says so instead of "not found": `listing '<bucket>' is not permitted ... barca needs
  s3:ListBucket on the bucket` (on GCS `storage.objects.list`, on Azure the Storage Blob Data
  Reader or Contributor role). Exit 3; grant the permission, or recompute with `--refresh-all`.
- **Cached artifact that cannot be fetched** for any other reason (permission or
  authentication errors, a store that cannot be reached, a stalled transfer): the run exits 3
  with `could not fetch ... cached artifact(s)`, naming each one and the store's error. Fix
  the access, or recompute with `barca get target pipeline.py --refresh-all`.
- **How long an unreachable store takes to fail.** A refused or dropped connection counts as
  transient, so a transfer gets up to 4 attempts, and inside each attempt the cloud SDK
  retries on its own first: measured with default settings, about 12 seconds per attempt for
  S3, 90 seconds for Azure and 4 minutes for GCS. An attempt that reaches `transfer_timeout`
  (default 600 seconds) is abandoned and not retried. So the wait is at most 4 times the
  smaller of those two, and never unbounded; with `transfer_timeout = 5` an unreachable store
  fails a run in about 5 seconds on every backend. Lower it where a fast failure matters.
- **Stalled store**: lower `transfer_timeout` to fail faster; raise it if single artifacts
  legitimately take longer than 10 minutes to move.

- **A directory where a local copy belongs**: not a store problem and not an error. The store's
  copy is fetched and the directory is moved aside, never deleted (`barca docs cache`, "A
  directory at an artifact's path"). If barca may not rename it (no write permission on the
  directory that holds it), the run exits 3 and says so, naming the path.
- **A directory at an object's path in the store** (a store that is a shared directory): barca
  changes nothing in a store but its own objects, so the transfer fails with
  `IsADirectoryError`, naming the path, and the message says what works: remove or rename the
  directory there. A fetch that meets one exits 3. An upload that meets one is a pending
  upload like any other (exit 0, a warning with the same advice), uploaded by the next command
  once the directory is gone. Recomputing with `--refresh-all` does not help; the upload meets
  the same directory.

`.barca/artifacts/` doubles as a local cache of the store, bounded by the cache size limit
(`[cache] max_size`, default 10% of the filesystem; `barca docs cache`, "Disk use"). Past the
limit, local copies of results the store holds are evicted before any result that would have to
be computed again, and are downloaded again when something reads them. Deleting the directory by
hand is safe too (anything needed later is downloaded again, or computed again if it is no
longer in the store); the directories barca moved out of an artifact's way (`*.moved-aside*`)
go with it. The store itself is never pruned by barca: bound it with your bucket's lifecycle
rules.

Using a GCS emulator (e.g. fake-gcs-server) with gcsfs 2026.10 or later: set
`GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT=false`. gcsfs's experimental mode calls a gRPC API the
emulator doesn't serve, and transfers stall until `transfer_timeout`.

## Looking at results in the bucket

Nothing has to be downloaded by hand or re-run to inspect a remote result:

```bash
barca status total --json --sample 2     # rows, columns and sample rows, read from the bucket
barca sql "select * from total"          # downloads `total` into .barca/sql-cache/ and queries it
```

`barca status` reads a parquet footer by ranged requests and downloads json or pickle results of
up to 16 MB; a store it cannot read is a `note` on each shape, not a failed command
(`barca docs status`). `barca sql` downloads the artifacts of the views a query names and reuses
the copies while the objects are unchanged (`barca docs sql`).

## Sharing a result: signed URLs

Experimental. A signed URL is a time-limited HTTPS link to one object in the bucket. Anyone who
has it can download the object, without cloud credentials, until it expires: a stakeholder who
wants the CSV, a BI tool that loads parquet over HTTPS, a link in a chat message.

```bash
barca get total pipeline.py -o url                    # one link per result, valid for 1 hour
barca get total pipeline.py -o url --expires 7d       # 30m, 12h, 7d or seconds; at most 7d
barca get sales pipeline.py -o url --partition region=emea   # one link per selected key
barca get total pipeline.py -o url --json             # each _barca_artifact gains url, url_expires_at
```

`-o url` resolves results exactly as `-o path` (the target is brought up to date first) and
prints one URL per line, in the same order. The link downloads as `<node>.<ext>` (it sets
`Content-Disposition: attachment; filename="total.json"`). In Python, any object can be signed,
not only results:

```python
import barca

url = barca.signed_url("s3://reports/weekly/2026-10-19.csv")                 # 1 hour
url = barca.signed_url("abfss://out@acct.dfs.core.windows.net/x.parquet", expires=86400)
```

`expires` is seconds or a `datetime.timedelta`. `barca.signed_url` checks that the object
exists and raises `barca.BarcaError` naming the URI and the reason. It needs the store's extra,
not the barca binary.

Signing uses the credentials barca already uses for the store. What each cloud needs:

| Cloud | How the link is signed | What the identity needs |
|---|---|---|
| S3, R2, MinIO | SigV4 presigned GET; R2 and MinIO through the `endpoint_url` in the s3 storage options | `s3:GetObject` on the object. A link signed with temporary credentials (SSO, an instance role) stops working when they expire; `url_expires_at` then reports the earlier time |
| GCS | V4 signed URL. With a service-account key it is signed locally; with workload identity or the metadata server, through IAM `signBlob` as that service account | without a key: `iam.serviceAccounts.signBlob` on the service account (`roles/iam.serviceAccountTokenCreator`). User credentials (`gcloud auth application-default login`) cannot sign: log in with `--impersonate-service-account=SA_EMAIL` |
| Azure | with an account key or connection string, a service SAS; otherwise a user delegation SAS from `DefaultAzureCredential` | without a key: `Microsoft.Storage/storageAccounts/blobServices/generateUserDelegationKey/action` (in Storage Blob Delegator and Storage Blob Data Contributor) and read access to the blob. A SAS token in the storage options cannot sign new links |

Know before sharing:

- **A link cannot be revoked before it expires**, except by rotating the key that signed it (an
  S3 access key, an Azure account key) or revoking the Azure user delegation keys. That is why
  the default is one hour and the maximum is 7 days.
- **An old link keeps serving the old bytes.** Artifacts are stored as `{node}/{run_hash}{ext}`,
  so a newer result is a new object. A link made before a refresh still downloads the result it
  was made for, until it expires.
- **The URL is printed to stdout only.** It is a bearer credential: barca does not write it to
  stderr, the metadata database, run history, `barca logs` or telemetry.
- `-o url` needs a cloud store. With no store, a shared directory or `file://`, it is a usage
  error (exit 2), as is `--expires` over 7d or without `-o url`. A `local only` result (its
  upload failed after the step succeeded) is uploaded before it is signed, and a cached result
  missing from the store is uploaded from this machine's copy. A result that still is not in the
  store, or credentials that cannot sign, is exit 3 naming the node; `barca doctor` shows which
  identity barca uses.
- Sinks are not signed by `-o url`; sign the URI a sink wrote with `barca.signed_url`.

## Limitations

- Backups are this machine's: two machines on one store without a server interleave them, with
  a warning (see "Backups and restore"). Sharing state means a server (RFC-0007). A machine is
  its host name and project directory: a container whose host name changes at every start is a
  new machine each time.
- Retention is fixed at five backups in each place. A backup is a whole copy of the database,
  so the store holds up to five times its size.
- With `BARCA_ARTIFACT_URI` or `[remote] artifacts_uri` alone (no `[remote] uri`), results go to
  the store but no backup is taken.
- `barca status` compares a store object's size, not its bytes, except in a store that is a
  plain directory; partitioned nodes are checked only for pending uploads.
- A store object deleted behind barca's back is noticed within 5 minutes, not by the next run
  (see "How often the store is checked").
- `barca status` does not describe a remote json or pickle result larger than 16 MB, and
  `barca sql` downloads a whole artifact before querying it.
- `.barca/artifacts/` has no size cap (see Failures).

## Diagnose configuration

Run `barca doctor --pretty` before a run to check configured stores and credential identity.
See `barca docs doctor` for check limits and a copyable local-only debugging profile.

See also: `barca docs cache`, `barca docs server-mode`, `barca docs doctor`.
