---
title: Remote Storage
description: Share one cache across machines with S3, GCS, Azure or Cloudflare R2, configured with environment variables.
---

Point barca at a bucket and every machine that uses the same location shares results and run
history: a result computed on one machine is a cache hit on the others. Configuration is one
variable plus the credentials your cloud's tools already use. No `barca.toml` is needed.

```bash
pip install "barca[s3]"                      # or barca[gcs], barca[azure], barca[remote] (all)
export BARCA_REMOTE_URI=s3://my-bucket/barca/my-project
barca get total                              # results go to the bucket; history is shared
```

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
name one). Results and shared history always use the same credentials.

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
2. On a second machine, or a fresh clone with no `.barca/`, run the same `barca get`: it reports
   `steps_executed: 0`, served from the bucket.

Every execution checks the artifact store before starting workers, including with
`state = "off"` and runs triggered through `barca serve`. The read-only startup
probe is limited to 10 seconds, or `transfer_timeout` when shorter. Cloud stores
must allow listing the bucket/container root and its backend's existence check;
object read/write access or prefix-only listing is insufficient. This check
verifies reachability, not upload permission. A new directory store checks its
existing ancestor; the probe creates nothing.
Before the end-of-run upload wait, barca reports the artifact count, destination
and per-attempt timeout; longer waits report progress every 10 seconds. Failed
uploads report their attempt count and remain unrecorded.

A failure to reach the bucket stops the run before any step: exit 3, naming the location, with
the cloud's own error (expired login, access denied). A missing extra says which one to install.

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
`[remote.storage_options.*]` over `FSSPEC_*` variables. Every key is in the
[configuration reference](/reference/config/).

## Storage for one process

Set `BARCA_REMOTE=off` to use local artifacts and local history without editing
`barca.toml`. It overrides configured artifact/state URIs and remote options,
including environment overrides. `--env` still selects the local environment.
Unset or empty values keep the configured storage; other values are usage errors.

```bash
BARCA_REMOTE=off barca get total pipeline.py
BARCA_REMOTE=off barca run deploy pipeline.py
BARCA_REMOTE=off barca serve pipeline.py
```

Existing history is kept. Remote/shared-store results recorded in that history
are not fetched or inspected while off. Barca uses a previously recorded local
result when available, otherwise computes one locally; subsequent runs reuse it.
`barca status --json` reports local cache paths. Historical materialization paths
still describe where those earlier results were stored. User code can still
perform its own network I/O.

`BARCA_STATE=off` is the narrower existing override: keep metadata local while
continuing to share artifacts. Use it with `serve` when project configuration
normally enables optimistic shared state:

```bash
BARCA_STATE=off barca serve pipeline.py
```

`serve` continues to refuse optimistic shared metadata unless explicitly disabled.
It does not silently discard that configuration. Empty URI overrides remain
unset; `BARCA_REMOTE_URI=` does not turn off a TOML remote root.

## What barca keeps in the bucket

```
<uri>/<env>/artifacts/<node>/<run_hash>.<ext>   one file per result
<uri>/<env>/state/metadata.db                   run history, pulled at the start of a run
```

`<env>` is `default` unless you pass `--env` (`barca docs cache`). Barca reads and writes objects
and reads their metadata; it never deletes, and it lists the bucket only once, before it
recomputes a result whose artifact is missing (see "Failures"). In practice that is
`s3:GetObject`, `s3:PutObject` and `s3:ListBucket` on S3, the Storage Object User role on GCS (replacing the
history object needs delete permission there), and Storage Blob Data Contributor on Azure.

Two machines finishing runs at the same time do not lose history: the second detects the
conflict, re-reads and merges. Set `BARCA_STATE=off` to keep history on each machine while uploading artifact
files to the configured store. Another machine does not discover cache hits from those
files alone: cache discovery needs materialization rows in shared history.

The shared history is one object, replaced by every run, and barca keeps no earlier copies of
it in the bucket. Turn on object versioning for the bucket (S3, GCS and Azure all have it): it
is the backup of `state/metadata.db`. What barca does when the object is damaged, and how to
put a good one back, is under "If the shared history is damaged" below.

### The local copy of the history

Each machine works on a local copy, `.barca/metadata.db`. `barca get` and `barca run` bring it up
to the shared history when they start (a pull), publish newly recorded progress every minute,
and upload their final outcome when they end; `--dry-run` and
`barca status` pull before they look, and upload nothing. `barca history` and `barca stats` read
the local copy as it is.

After a pull the local copy is the shared history plus what was recorded only on this machine:

- **Nothing other machines uploaded is dropped by a pull**, whatever state the local copy was
  left in (a killed run, a database created by `barca history` before the first run), and
  whichever barca commands run in the project at the same time: a download that another
  command's pull or upload has overtaken is thrown away, never put in place of a newer copy.
  A barca 0.17.1 or older running in the same project at the same moment does not take part
  in this.
- **What was recorded only here is kept.** A run that was killed (`kill -9`, out of memory, a
  lost machine) before it could upload, a run whose upload failed, and runs made with
  `BARCA_STATE=off` are still in the local copy after the pull, with their finished steps and
  captured output. The pull says so on stderr, for example
  `[barca] kept 1 run and 2 finished steps recorded only on this machine (not yet in the shared history)`.
  So after a kill the next `barca get` serves the steps the killed run finished from cache
  (`barca docs cache`, "While a run is going, and after one is killed").
- **They are shared by the next upload.** A running `barca get` or `barca run` publishes
  newly recorded progress on its next healthy minute checkpoint, including history kept by
  a pull. Its final upload also shares that history. A killed run can still appear `running`
  on a machine that cannot establish that its original process has gone. `--dry-run` and `barca status` keep them locally and upload nothing, and the
  `kept` line is printed once, not by every command until then.
- **Nothing is there twice.** A run is identified by its run id and a step by its run id and
  node, so a row both copies hold appears once, however many pulls happen before an upload.
- **A step of a run made here is kept only with its result.** If its result file is no longer on
  this machine it is left out (stderr: `left out 1 step whose result file is no longer here`)
  and runs again; the run it belonged to is kept.

Every pull works the same way, whatever state the local copy is in: download the shared
history, add to the download what only the local copy has, and put the result in the local
copy's place. Nothing is remembered about the local copy from one command to the next, so it
does not matter whether it was written by a run that did not upload, deleted and created again
(`barca history`, `barca stats`, `barca get` and `barca run` create it), or replaced as a whole
file by an older or newer copy of itself (restored from a backup, or from `metadata.db.prev`).
That covers what barca writes and whole-file replacement. It does not cover another program
editing rows inside the file: finding what only the local copy has reads the end of its history
and its indexes, not the whole of it (so a long history does not make that slower), and that
relies on history only ever being added at the end, which barca's own writes keep.

Before the download is put in place it is checked, and it must be a database this version of
barca can use (the rules are under "If the shared history is damaged"). Part of that check
reads every page of the download, which takes about 6 ms per MB of history (80 ms for a 14 MB
history of 3,000 runs on a laptop). It is skipped
when the download is byte for byte the local copy, which is the usual case on a machine that
ran last; it is paid on the first pull after another machine uploaded, and by every pull
while this machine holds rows it has not uploaded.

A pull is safe while a run is going in the same project. `--dry-run`, `barca status` and a second
`barca get` or `barca run` pull as usual; the running run's row and the steps it has finished stay
in the local copy, so `barca status` shows its progress next to what other machines uploaded.
Every run finishes and uploads; one that finds the shared history changed merges as described
above. An upload sends a copy of the history taken at that moment, so other barca commands in
the project do not wait for it, however slow it is; if one of them writes to the local copy
meanwhile, the run uploads once more when the first upload is done (it reports this as a
conflict retry). Once more only: what is written during that second upload (by a run still
going in the project, say) is uploaded by that run when it ends, or with the next run from this
machine.

A download does not hold the project's lock either, with one exception. If two other barca
commands in a row replace or upload the local copy while one pull is downloading, that pull's
third download is made holding the lock, so that nothing can overtake it again; other barca
commands in the project wait for it meanwhile. That download may take 45 seconds. After that it
is stopped, the pull fails (exit 3) with the local copy untouched, and the commands that were
waiting go on; run the failed command again.

Files next to the database, all local: a download goes to
`.barca/metadata.db.pull-<host>-<pid>-<n>` and is moved into place once complete, an upload is
sent from `.barca/metadata.db.push-<host>-<pid>-<n>`, and `.barca/metadata.db.base` is a counter
that changes every time the local copy is replaced or uploaded, which is how a pull notices
that its download was overtaken. `.barca/metadata.db.prev` is the local copy as it was before
the last pull that changed it (see "Going back to the local history from before a pull"), and
`.barca/metadata.db.prev.tmp` exists for an instant while it is being replaced. Leftovers of a
killed command are removed by a later pull. You can delete any of them when no barca command is
running; nothing is concluded from the counter about what the local copy holds, and barca never
reads `.prev`. If barca is killed during a pull, the local copy is either the old one, whole,
or the new one, whole.

When something is wrong, the local copy is replaced only if it certainly holds no history:

- A downloaded history that is not a database this version of barca can use replaces nothing,
  whether or not there is a local copy: the command fails (exit 3). See "If the shared history
  is damaged".
- A local copy that another program holds open (a DB browser, a script using `sqlite3`), that
  cannot be read (permissions, I/O), whose main file is empty while its `-wal` file is not, or
  that fails for any reason barca does not recognise is left exactly as it was: barca waits up
  to 5 seconds for a lock, then the command fails (exit 3) and says what to close or check.
- A local file that certainly holds no barca history is replaced, with a warning on stderr
  that says why: it is not a database (no SQLite header, cut short, or reported corrupt when
  read), it is empty, it is a database without barca's tables, or only a `-wal` file was left.

**Resetting or rolling back the shared history.** Every machine keeps whatever the shared
history lacks, so removing history takes more than changing the shared file:

- If `state/metadata.db` is deleted, the next `barca get` or `barca run` on any machine creates
  it again from that machine's whole local copy.
- If it is replaced by an older copy, each machine keeps every run the older copy lacks at its
  next pull, and uploads them with its next run.
- To reset on purpose: with no barca command running anywhere, delete the shared file and, on
  each machine, `rm -f .barca/metadata.db .barca/metadata.db-wal .barca/metadata.db.base` (for
  a named environment the same three files under `.barca/envs/<env>/`). Result files are not
  affected.

**If the shared history is damaged.** Every pull checks what it downloaded before anything is
replaced. The download must be all of these:

1. a whole SQLite file: not empty, starting with the SQLite header, a whole number of pages
   long;
2. readable page by page: it opens, and SQLite's `PRAGMA integrity_check` answers `ok`. This is
   what catches a transfer that stopped part-way and pages overwritten inside the file;
3. a barca history: it has both the `runs` and the `materializations` table;
4. compatible with this version: the metadata schema marker must be supported before any
   schema updates or replacement. Existing unversioned histories are migrated additively in
   one transaction, preserving their rows. Every table must have the columns this version
   uses, and no required columns it cannot fill. Compatible extra nullable/defaulted columns
   are left alone. Unsupported schema versions are refused; upgrade to a
   compatible Barca release. A version mismatch never resets run history, logs or schedules.

The same version guard applies to local reads and writes, including inspection snapshots.
Read-only inspection does not migrate the source database. Older already-published Barca
binaries do not understand this new guard, so the first versioned schema remains additive
and compatible with their existing tables.

If any of these fails, the command stops before any step runs:

```
the shared history s3://my-bucket/barca/my-project/default/state/metadata.db is not a database barca can use: its integrity check found 12 problems, the first: Page 17: never used.
The local history .barca/metadata.db was left as it was, and nothing was uploaded.
To repair it, follow `barca docs remote`, section "If the shared history is damaged": ...
What the integrity check found:
  Page 17: never used
  ...
```

It exits 3 (in JSON mode the first line is the envelope's `error` and the rest its
`remediation`). The first line always says what is wrong in one line; when an integrity check
found several problems, up to 20 of them are listed after the instructions. The local copy, with everything recorded only on this machine, is exactly as
it was; nothing was written to the bucket; and every command that pulls (`barca get`,
`barca run`, `--dry-run`, `barca status`) fails the same way on every machine until the object
is repaired. Nothing repairs it automatically. `BARCA_STATE=off` runs with local history only in
the meantime, and those runs are uploaded later like any others recorded only locally.

To repair it, either put back an earlier version of the object (bucket versioning, or a
backup), after which each machine adds what that version lacks with its next run; or rebuild it
from a machine's local copy:

1. Find the machine whose local history is the most complete and intact. `barca history --all`
   reads the local copy and pulls nothing, so it works while the shared history is damaged.
   Check that copy before you upload it, because the upload is not checked:
   `sqlite3 .barca/metadata.db "PRAGMA integrity_check"` must print `ok`. If it does not, the
   local copy is damaged too; follow "Upgrading a project whose shared history was damaged by
   0.17.1 or earlier" below, which also applies to damage from any other cause.
2. With no barca command running anywhere, remove the damaged object from the bucket (or move
   it aside), with your cloud's own tool. For example:

   ```
   aws s3 mv s3://my-bucket/barca/my-project/default/state/metadata.db s3://my-bucket/barca/my-project/default/state/metadata.db.damaged
   ```

3. On that machine, run any `barca get`. It reports `no shared state yet — this run will create
   it` and uploads its whole local history as the new shared history.
4. Every other machine pulls it with its next command and keeps what only it had; those rows
   are uploaded when its next `barca get` or `barca run` ends.

**Going back to the local history from before a pull.** A pull keeps the database it replaces
as `.barca/metadata.db.prev` (for a named environment, under `.barca/envs/<env>/`): one whole
SQLite file, exactly the local copy as it was, including what had not been uploaded. One
generation is kept. It is the local copy from before the last pull that brought in a different
version of the shared history: pulls that find the shared history unchanged do not touch it,
and neither does a run's own upload. There is none until a pull has replaced a local history
(a machine's first pull has nothing to keep). Keeping it takes no time, whatever the size of
the history: the old file is kept under a second name instead of being deleted. It takes the
disk space of one more copy of the history.

You rarely need it, because a pull already keeps every run and step the shared history lacks.
It is for when the shared history that was pulled turns out to be the wrong one (replaced by
mistake, or written by a project pointed at the wrong location), and for what a pull does not
carry over (see Limitations). To look inside, copy it somewhere and open the copy
(`sqlite3 copy.db`). To go back to it, with no barca command running in the project:

```
cp .barca/metadata.db.prev .barca/metadata.db
rm -f .barca/metadata.db-wal .barca/metadata.db-shm
```

The second line matters: the `-wal` file belongs to the database you are replacing. Going
back loses what this machine recorded after that pull and has not uploaded since (runs made
with `BARCA_STATE=off`, a killed run, a run whose upload failed): those rows are only in the
file you overwrite, so copy it somewhere first if you may want them. After this:

- with `BARCA_STATE=off`, barca works on that history alone;
- with shared history on, the next pull downloads the shared history again and keeps what
  only the restored copy has, so by itself this does not undo anything in the shared history;
- to make the restored copy the shared history, remove the shared object as in step 2 above and
  run `barca get`: the run uploads the restored history. Other machines still hold the runs
  they pulled or made, and add them back with their next run; to drop those for good, reset
  those machines as described under "Resetting or rolling back the shared history".

**Upgrading a project whose shared history was damaged by 0.17.1 or earlier.** Up to 0.17.1 a
pull could leave the previous database's `-wal` file beside the one it downloaded, and the two
were then read as one database. A project where runs were killed, or where several machines
ran close together, can have been left with a damaged history, shared and local alike. Those
versions kept running on it (with errors such as `short read on page 33` now and then). From
0.18.0 a pull checks what it downloads, so on such a project every `barca get`, `barca run`,
`--dry-run` and `barca status` exits 3 with one of:

```
... is not a database barca can use: its integrity check found 4 problems, the first: Page 17: never used.
... is not a database barca can use: its integrity check found 12 problems, the first: Page 17: never used.
... is not a database barca can use: it cannot be read as a database: I/O error: short read on page 33: expected 4096 bytes, got 0.
```

(the second lists `row 29 missing from index idx_mat_run` among its problems).

A machine whose local main file is byte-for-byte identical to the download and whose
write-ahead log has no frames skips the full page-integrity scan on that pull. The
download still passes the header, schema and compatibility checks, so this is not a
guarantee that a damaged identical copy is accepted. A successful pull can update local
metadata and its retained copy; inspection is not a promise to leave those files untouched.
In the 0.17.1 reproduction, local and shared copies had the same damage. Uploading that
local copy again cannot repair the shared history. Verify a candidate backup before
restoring it, or salvage readable history. Unsupported schema versions require a
compatible release, rather than resetting history. Stop Barca commands on every machine
before the manual repair procedure below.

*Salvage what can be read.* This needs the `sqlite3` command, version 3.29 or later, built
with `.recover` (the one shipped with macOS and with current Linux distributions is).

1. On each machine, in the project directory, check the local copy:

   ```
   sqlite3 .barca/metadata.db "PRAGMA integrity_check"
   ```

   A single line `ok` means this copy is intact: leave it. Anything else, an error included,
   means it is damaged.
2. On each machine with a damaged copy, recover what can be read into a new database and put
   it in place (the damaged file is kept as `metadata.db.damaged`):

   ```
   sqlite3 .barca/metadata.db ".recover" | sqlite3 .barca/metadata.recovered.db
   sqlite3 .barca/metadata.recovered.db 'DROP TABLE IF EXISTS lost_and_found;
   INSERT OR IGNORE INTO "__turso_internal_seq___turso_internal_autoincrement_runs" VALUES (1,0,1,1,1,9223372036854775807,0);
   INSERT OR IGNORE INTO "__turso_internal_seq___turso_internal_autoincrement_materializations" VALUES (1,0,1,1,1,9223372036854775807,0);
   INSERT OR IGNORE INTO "__turso_internal_seq___turso_internal_autoincrement_logs" VALUES (1,0,1,1,1,9223372036854775807,0);
   PRAGMA integrity_check;'
   mv .barca/metadata.db .barca/metadata.db.damaged
   rm -f .barca/metadata.db-wal .barca/metadata.db-shm .barca/metadata.db.base
   mv .barca/metadata.recovered.db .barca/metadata.db
   BARCA_STATE=off barca history --all
   ```

   These lines work as they are in bash, zsh and fish (the second command is one command:
   its quoted argument spans five lines). The second command must end with `ok`, and the last
   must list the machine's runs. (The three
   `INSERT` lines put back a bookkeeping row the database engine needs, in case it was on a
   page that could not be read; they do nothing when it is there.) If either fails, this copy
   cannot be salvaged: remove it with the command under "Start the history again" and go on;
   the machine then takes the history from the others.
3. Remove the damaged shared object from the bucket, or move it aside, with your cloud's tool.
4. On the machine whose salvaged history lists the most runs, run any `barca get`. It reports
   `no shared state yet — this run will create it` and uploads its history as the shared one.
5. On every other machine, carry on: its next command pulls the new shared history and keeps
   what only its own copy has, and its next `barca get` or `barca run` uploads that.

What can be lost: rows that were on pages that could not be read. A step whose row is lost
runs once more; a run whose row is lost is missing from `barca history`. Where the damage was
only pages that nothing refers to or an index that disagreed with its table, nothing is lost.

*Start the history again.* When there is no `sqlite3`, or the salvage fails, or the history is
not worth keeping: remove the shared object from the bucket and, on each machine,

```
rm -f .barca/metadata.db .barca/metadata.db-wal .barca/metadata.db-shm .barca/metadata.db.base
```

(for a named environment, the same files under `.barca/envs/<env>/`). The next `barca get`
creates a new shared history. What is lost: all of `barca history` and `barca stats`, and every
record of what is cached, so each step runs once more on the first run that needs it (its
result file is then written again; the old files are not deleted).

*Without shared history.* A local `.barca/metadata.db` that was damaged on its own (the same
check as step 1 says so; barca may fail with `short read on page`, `database disk image is
malformed`, or stop with an internal error of the database engine) is repaired the same way:
step 2 alone, or the `rm -f` line to start again.

`barca get --json` reports a result as it does without a store: json values inline, parquet and
pickle as a pointer (`{"_barca_artifact": {"path", ...}}`) to the copy in `.barca/artifacts/`.
A final output another machine produced is downloaded first.

## What happens during a run

- The shared metadata DB is pulled before the run and pushed after it (conditional upload;
  a concurrent push from another machine is merged by replaying this run's rows).
- Steps always read and write local files under `.barca/artifacts/`. A helper process
  uploads each artifact in the background as soon as its step finishes.
- A cache hit recorded by another machine is downloaded just before the first step that
  reads it eagerly. Cached intermediates nothing reads are never downloaded, and neither are
  parquet results that are only read lazily (below).
- Before results are recorded, barca waits for every upload. The shared history never points
  at an artifact that is missing from the store.

stderr reports each part, so remote cost is visible:

```
[barca] pulled state (48.0 KB) in 0.03s
[barca] fetched 1 cached artifact (672.8 KB) in 0.1s
[barca] uploaded 2 artifacts (672.8 KB); waited 0.0s at end of run
[barca] pushed state (48.0 KB) in 0.03s
```

## How steps read inputs from the store

A result produced on this machine is already on disk, and steps read that file. For a cache hit
recorded by another machine, the input's annotation decides how many bytes move
(`barca docs types`):

- A parquet input annotated `duckdb.DuckDBPyRelation` or `pl.LazyFrame` is read in place:
  only the byte ranges the step's query touches are fetched, and nothing is downloaded. A query
  over one column of eight fetches about that column's share of the object; a selective filter
  skips row groups. This applies when every step in the phase that reads the result is lazy.
- Every other input (no annotation, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table`, json,
  pickle) is downloaded once into `.barca/artifacts/` and read from there by this run and
  later ones.

For a large upstream that a step filters, projects or aggregates, annotate the input as lazy.
DuckDB reads barca's artifacts through a `barca<protocol>://` filesystem registered on its
connection, so `s3://`, `abfss://` and `gs://` URLs in your own SQL keep using DuckDB's own
extensions and credentials.

## Checking a local copy against the store

When an artifact is uploaded, the SHA-256 of the local file is recorded with it in the shared
history. A machine uses that hash to decide whether its own copy is current:

- A copy already in `.barca/artifacts/` is hashed the first time a run reads it. If it does not
  match (edited by hand, or left from before another machine refreshed the result), it is
  replaced by the store's copy and reported as a fetch.
- A downloaded artifact is hashed too. If the store's copy does not match the recorded hash,
  the run still uses it and prints a warning naming the step:
  `[barca] warning: pipeline.py:total: the copy at <location> is not the one this result was
  recorded with (another run overwrote it, or it was changed). Using it. Recompute with
  --refresh pipeline.py:total.` This is not an error and the exit code does not change: an
  artifact's path is `<node>/<run_hash>`, which identifies the computation and not the bytes,
  so a `--refresh`, or two machines computing the same step at once, overwrites the object.
  `--refresh <file.py:name>` recomputes the step, uploads the new bytes over the object and
  records their hash, which clears the warning for every machine that shares this history.
  Until then, machines can hold different copies of that one result: a machine whose copy
  matches the recorded hash keeps it, and the others use the store's.

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
warnings only ([CLI contract](/reference/cli-contract/)).

A mismatch and a missing object are different findings and never stand in for each other: a
copy with other bytes is used and flagged, as above; an object that is not in the store at all
is computed again with `reason: "artifact_missing"` (`barca docs remote`, "Failures").

Only artifacts a run reads are hashed, once per run. Not checked: a parquet input that is read
in place (only byte ranges are fetched), and results recorded before barca stored a hash. A
`--dry-run` does not contact the store, so it never reports a mismatch.

## Ctrl-C

Ctrl-C cancels a `barca get` or `barca run` at any point: while steps run, and while barca is
uploading, downloading, or pulling or pushing the shared history. The command exits 130 with the
`cancelled` error. It makes no difference whether the terminal sent the signal to every process
of the job or something sent SIGINT to barca alone. Barca, its workers and its helper processes
print no traceback, with or without a store and from the first moment of a run: a Ctrl-C while
the workers are still starting exits as promptly as any other.

SIGTERM, which a supervisor, a CI timeout or `docker stop` sends, does what Ctrl-C does:
everything below holds for it, a second SIGTERM counts as a second Ctrl-C, and the exit code
is 130, not 143.

1. **The first Ctrl-C cancels the run.** Steps and transfers in flight are stopped, what
   finished is recorded in this machine's history, the run as `cancelled`, and the run wraps
   up: it pushes that record to the shared history, so that other machines do not compute the
   finished steps again.
2. **The wrap-up takes at most 10 seconds.** With a store that answers it takes a fraction of
   a second. If the push has not finished by then (a slow, stalled or unreachable store) it is
   stopped, and stderr says so:
   `[barca] the shared history was not updated (the upload did not finish within 10s). This run
   is recorded on this machine; the next barca get or barca run here uploads it.`
3. **A second Ctrl-C abandons the wrap-up at once** (the same line, with `stopped by a second
   interrupt`, whichever signal it was). A third changes nothing.

Stopping a helper process can take up to 2 seconds, so the command ends within a few seconds of
the last of these. The exit code is 130 in every case, never 3: a push that fails during the
wrap-up is reported in that stderr line, not as an error.

What is left behind is always consistent:

- A step is recorded only once its artifact is confirmed in the store. A step whose upload
  was still in flight is not recorded, and runs again next time.
- No partial file is left. A download, and an upload into a store that is a directory, is
  written to a temp file beside its destination and renamed when whole; the temp file is
  removed when the transfer is stopped. An object store shows an object only once its upload
  has completed, so an interrupted upload leaves the previous object, or none. The shared
  history is replaced in one step, so it is the old one or the new one.
- Nothing is lost when the wrap-up does not finish. The record is in this machine's history, a
  pull keeps what was recorded only here (see "The local copy of the history"), and the next
  `barca get` or `barca run` on this machine serves the finished steps from cache and uploads
  them with its own.
- Interrupted while the shared history is still being pulled, before anything ran, the command
  exits 130, no run is recorded and the local copy is as it was.

**What the run's record says.** `cancelled`, whenever the interrupt arrived before the record
was shared. That includes a Ctrl-C during the final push, when every step had finished: the
steps are recorded as finished, the run as `cancelled`, and that is what the wrap-up shares, so
every machine sees the same. A Ctrl-C that arrives once the push has completed is too late
to cancel anything: the run is `success` and the command exits 0.

Two narrow cases, stated exactly:

- A run with a failed step is recorded and shared as `failed`, and may then still download an
  earlier output to return. Interrupted during that download, the command exits 130 and the
  record stays `failed`, the same on every machine.
- If the interrupt arrives in the instant in which the push completes in the store but barca
  has not yet heard so, the shared history has the run as `success` and this machine marks it
  `cancelled`. The wrap-up then pushes again, which puts `cancelled` in the shared history too.
  If that wrap-up does not finish either, the two can temporarily differ. The next pull on
  this machine preserves its cancelled outcome, even if other machines have added runs to
  the shared history. The next successful upload shares that correction. Until then, another
  machine can still see the earlier outcome.

The end-of-run line (`[barca] <n>/<total> steps | done in <secs>s`) is about the steps. A Ctrl-C
that arrives after the last step finished, while artifacts upload or the history is pushed,
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

Barca's helper processes (the one that moves artifacts and the one that moves the history) do
not act on Ctrl-C themselves. They are started outside the terminal's job (in a process group
of their own), so the terminal's Ctrl-C reaches barca only, which decides what it means and
stops them. If barca itself is killed (`kill -9`, out of memory), nobody is left to stop them,
so they watch for that: each exits on its own, at once and without output, and removes the temp
file it was writing. A download of the history that was cut this way can leave
`.barca/metadata.db.pull-*`, and an upload `.barca/metadata.db.push-*`; the next pull removes
both.

## A directory where an artifact belongs

An artifact is one file. A directory at an artifact's path under `.barca/artifacts/` is not a
store problem and not an error: the result is treated as missing, the store's copy is fetched
(or the step is computed again), and the directory is moved aside to
`<run_hash>.<ext>.moved-aside`, with its contents, never deleted. An empty directory is removed;
a symlink is replaced without touching its target. If barca may not rename the directory (no
write permission on the directory that holds it), the run exits 3 and says so, naming the path.
`barca docs cache`, "A directory at an artifact's path", has the full rule.

A directory at an object's path inside a store that is a shared directory is different: barca
changes nothing in a store but its own objects, so the fetch or the upload fails with exit 3
(`IsADirectoryError`, naming the path), and the error says what works: remove or rename the
directory there. Recomputing with `--refresh-all` does not help; the upload meets the same
directory. A directory at `state/metadata.db` fails the pull with `<uri> is a directory, not
the shared history file`.

## Settings

| `[remote]` key | Env var | Default | Meaning |
|---|---|---|---|
| `transfer_concurrency` | `BARCA_TRANSFER_CONCURRENCY` | 4 | Uploads/downloads in flight at once |
| `transfer_timeout` | `BARCA_TRANSFER_TIMEOUT` | 600 | Seconds one transfer attempt may run (counted from when it starts) |
| `push_retries` | `BARCA_PUSH_RETRIES` | 5 | Conflict retries for the state push |

## Failures

Transfers are retried up to 3 times (0.5s, 1s, 2s backoff) when the error looks transient:
dropped connections, timeouts, 5xx, 408 and 429. Missing objects, permission and
authentication errors, and other 4xx responses fail on the first attempt. An attempt that
exceeds `transfer_timeout` fails as stalled and is not retried.

- **Upload failed**: the run exits 3 and names the step. The step gets a `failed` row with
  `error_type = 'UploadError'`, no artifact path, and the attempt count; it recomputes on
  the next run. `barca stats target pipeline.py` shows the failure.
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

`.barca/artifacts/` doubles as a local cache of the store and is never pruned automatically;
deleting it is safe (anything needed later is downloaded again, or computed again if it is no
longer in the store).

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

## State object paths

`BARCA_REMOTE_URI` names an artifact/state root or prefix. `BARCA_STATE_URI` instead
names the shared history file or object itself: for example,
`/shared/history/metadata.db` or `s3://bucket/history/metadata.db`. If it points to a
directory, startup exits 3 before importing user code and explains the object-path
setting. Barca leaves that directory and its contents unchanged; choose a file/object
path or set `BARCA_STATE=off` to use local history.

## Limitations

- `barca serve` does not share history yet; set `BARCA_STATE=off` for it.
- During `barca get` and `barca run`, newly recorded progress is published every minute
  in healthy operation, and the final outcome is published when the run ends. A shared
  checkpoint keeps the run `running`, with its recorded step count and no finish timestamp.
  With a remote artifact store, only confirmed uploads become reusable progress; failed or
  unfinished uploads do not. A fresh project root can reuse the shared confirmed results
  after the original process is killed. Unchanged progress does not upload another snapshot.
- A failed checkpoint leaves committed local history intact and retries on the next minute
  tick. Failed uploads or database/storage failures can delay publication beyond a minute;
  this cadence is not a guaranteed recovery point during an outage. Checkpoints replace the
  current shared object; they do not create retained snapshots or delete history.
- Carried across a pull: runs, steps and captured output. Not carried: step rows written by
  barca before 0.17 that were never uploaded (they do not say which run wrote them), and the
  timing estimates used to size batches, which are rebuilt by running.
- The shared history has no snapshots in the bucket and there is no command that restores or
  rebuilds it: repairing a damaged one is the manual procedure above, and the backup is your
  bucket's object versioning. `.barca/metadata.db.prev` is one generation, on one machine. If
  the object is deleted and the first machine to run afterwards has no local history, the
  shared history starts again from nothing, without a warning, until a machine that still has
  its local copy runs.
- Without a store (only `BARCA_STATE_URI` set), history is shared but result files are not: a
  step another machine computed is recorded with a path on that machine.
- `barca status` does not describe a remote json or pickle result larger than 16 MB, and
  `barca sql` downloads a whole artifact before querying it.
- `.barca/artifacts/` has no size cap (see Failures).

## Remote sinks

`@sink` paths accept the same URIs, whether or not a remote store is configured:

```python
from barca import asset, sink

@asset
@sink('abfss://exports@myaccount.dfs.core.windows.net/daily/report.parquet')
def report():
    return build_dataframe()
```

A sink that fails (a missing extra, rejected credentials, an account that cannot be reached)
does not fail the asset or change the exit code. It is reported on stderr as
`[barca] SINK FAILED: ...`. See `barca docs sinks`.

## Staged writes

A result is not held in memory in serialized form while it is written:

1. The serializer (json, pickle or parquet) writes to a temporary file in the destination
   directory, or in `.barca/staging/<pid>/` for a remote `@sink`. That directory is on the
   project's disk and not in `/tmp`, which is often memory-backed.
2. A local destination gets the temporary file by an atomic rename. A remote destination gets
   it by a chunked upload; object stores make the object visible only when the upload completes.
3. On failure the temporary file is removed, so the destination does not hold a partial file.
   Staging directories of workers that are no longer running are removed when a worker starts.

The transfer helper works the same way: uploads are read from disk in chunks, and a download
goes to a temporary file that is renamed into place when complete.

## Artifacts only, history local

`BARCA_ARTIFACT_URI` is the setting from 0.4.0. It puts artifacts in a store, at the URI as
written with no environment name added, and keeps the history local:

```bash
export BARCA_ARTIFACT_URI=abfss://artifacts@myaccount.dfs.core.windows.net/prod
barca get pipeline.py
```

Prefer `BARCA_REMOTE_URI` with `BARCA_STATE=off`, which does the same and keeps `--env`
separation.
