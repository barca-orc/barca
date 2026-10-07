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

## What barca keeps in the bucket

```
<uri>/<env>/artifacts/<node>/<run_hash>.<ext>   one file per result
<uri>/<env>/state/metadata.db                   run history, pulled at the start of a run
```

`<env>` is `default` unless you pass `--env` (`barca docs cache`). Barca reads and writes objects
and reads their metadata; it never deletes or lists. In practice that is `s3:GetObject`,
`s3:PutObject` and `s3:ListBucket` on S3, the Storage Object User role on GCS (replacing the
history object needs delete permission there), and Storage Blob Data Contributor on Azure.

Two machines finishing runs at the same time do not lose history: the second detects the
conflict, re-reads and merges. Set `BARCA_STATE=off` to keep history on each machine and share
only results.

### The local copy of the history

Each machine works on a local copy, `.barca/metadata.db`. `barca get` and `barca run` bring it up
to the shared history when they start (a pull) and upload it when they end; `--dry-run` and
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
- **They are shared by the next upload.** When the next `barca get` or `barca run` on this
  machine ends, those runs and steps are in the shared history; a killed run shows there as
  `interrupted`. `--dry-run` and `barca status` keep them locally and upload nothing, and the
  `kept` line is printed once, not by every command until then.
- **Nothing is there twice.** A run is identified by its run id and a step by its run id and
  node, so a row both copies hold appears once, however many pulls happen before an upload.
- **A step of a run made here is kept only with its result.** If its result file is no longer on
  this machine it is left out (stderr: `left out 1 step whose result file is no longer here`)
  and runs again; the run it belonged to is kept.

Every pull works the same way, whatever state the local copy is in: download the shared
history, add to the download what only the local copy has, and put the result in the local
copy's place. Nothing is assumed about the local copy, so it does not matter whether it was
written by a run that did not upload, deleted and created again (`barca history`, `barca stats`,
`barca get` and `barca run` create it), restored from a backup, or changed by another program.
Finding what only the local copy has reads the end of its history and its indexes, not the whole
of it, so a long history does not make a pull slower.

A pull is safe while a run is going in the same project. `--dry-run`, `barca status` and a second
`barca get` or `barca run` pull as usual; the running run's row and the steps it has finished stay
in the local copy, so `barca status` shows its progress next to what other machines uploaded.
Every run finishes and uploads; one that finds the shared history changed merges as described
above. An upload sends a copy of the history taken at that moment, so other barca commands in
the project do not wait for it, however slow it is; if one of them writes to the local copy
meanwhile, the run uploads once more when the first upload is done (it reports this as a
conflict retry).

Files next to the database, all local: a download goes to
`.barca/metadata.db.pull-<host>-<pid>-<n>` and is moved into place once complete, an upload is
sent from `.barca/metadata.db.push-<host>-<pid>-<n>`, and `.barca/metadata.db.base` is a counter
that changes every time the local copy is replaced or uploaded, which is how a pull notices
that its download was overtaken. Leftovers of a killed command are removed by a later pull. You
can delete any of them when no barca command is running; nothing is concluded from the counter
about what the local copy holds. If barca is killed during a pull, the local copy is either the
old one, whole, or the new one, whole.

When something is wrong, the local copy is replaced only if it certainly holds no history:

- A downloaded history that is not a database does not replace an existing local copy: the
  command fails (exit 3).
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

`barca get --json` reports a result as it does without a store: json values inline, parquet and
pickle as a pointer (`{"_barca_artifact": {"path", ...}}`) to the copy in `.barca/artifacts/`.
A final output another machine produced is downloaded first.

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

## Limitations

- `barca serve` does not share history yet; set `BARCA_STATE=off` for it.
- The shared history is updated once, when a run ends. A run records each finished step in the
  local copy as it goes (`barca docs cache`, "While a run is going, and after one is killed"), but
  other machines see none of it until the run ends. A run that was killed is seen by other
  machines only after another `barca get` or `barca run` on the same machine has ended; if that
  machine never runs again, they never see it. With a remote artifact store a run records
  nothing early: a step is recorded once its upload is confirmed, when the run ends, so a
  killed run leaves its run row and no steps.
- Carried across a pull: runs, steps and captured output. Not carried: step rows written by
  barca before 0.17 that were never uploaded (they do not say which run wrote them), and the
  timing estimates used to size batches, which are rebuilt by running.
- Without a store (only `BARCA_STATE_URI` set), history is shared but result files are not: a
  step another machine computed is recorded with a path on that machine.
- `barca status` does not describe a remote json or pickle result larger than 16 MB, and
  `barca sql` downloads a whole artifact before querying it.
- The local artifact directory has no size cap (see "Local-first artifacts" below).

## How shared history works

- Artifacts are written to `{uri}/{env}/artifacts/{node}/{run_hash}{ext}`,
  addressed by the hash of the step's code and inputs, so a cache hit on one
  machine is valid on every machine. A `--refresh`, or two machines computing
  the same step at once, overwrites the object.
- The metadata DB (the turso/SQLite file that records materializations and
  run history) lives as a single blob at `{uri}/{env}/state/metadata.db`.
  Each run **pulls** it first — so cache checks see every machine's
  materializations — and **pushes** it back at the end with an
  etag/generation-conditional upload. If another machine pushed first, barca
  re-pulls and replays this run's rows, so nothing is lost (bounded by
  `push_retries`).
- Before upload the WAL is checkpointed into the main file, so the blob is
  always a complete standalone SQLite database — you can download it and
  open it with stock `sqlite3`.
- A run that pulls successfully but crashes mid-way uploads nothing; its
  local rows are discarded by the next pull and those steps recompute.

The result: a run on VM-B hits artifacts materialized by VM-A with zero
re-execution.

Every backend is held to the **same shared-state contract** — conditional
create, cross-machine cache hit, concurrent-writer conflict → replay — by a
backend conformance suite that runs on every PR against local emulators
(MinIO for S3/R2, fake-gcs-server for GCS, Azurite for Azure), and the
environment-variable setup above runs end to end against the same emulators
(a second machine must get a cache hit). See
[Releases](/contributing/releases/) for the guarantees each backend makes.

### Local-first artifacts, background transfer

Workers never write to the object store. They write every artifact to the
local artifact directory (`.barca/artifacts/`, or `.barca/envs/<env>/artifacts/`)
and read their inputs from there (lazy parquet inputs excepted, below), so a
step's critical path is local disk. A single helper process per run (`python -m barca._transfer`) moves
bytes between that directory and the store in the background, using the
same fsspec backends and credentials as everything else:

- **Upload** — the moment a step finishes, its artifact is queued for
  upload while downstream steps keep running against the local copy.
  Up to `transfer_concurrency` transfers run at once (default 4).
- **Fetch** — a cache hit recorded by another machine is downloaded to its
  local path just before the first step that reads it eagerly runs. Cached
  intermediates that nothing in the run reads are never downloaded — a fully
  cached `barca get` fetches only the final output. A parquet result that
  every reader in a phase takes as `duckdb.DuckDBPyRelation` or `pl.LazyFrame`
  is not downloaded either: those steps read it in place (see "How steps read
  inputs from the store" above).
- **Drain** — before the run is recorded and the state blob pushed, barca
  waits for every upload. A step whose upload fails gets no success row (it
  recomputes next run) and the run exits with an error naming it, so the
  shared metadata never points at an artifact missing from the store.

**Retries and timeouts.** Each transfer is retried up to 3 times with
exponential backoff (0.5s, 1s, 2s) when the error looks transient — dropped
connections, timeouts, 5xx, 408 and 429 responses. Errors no retry can fix fail
on the first attempt: missing objects, permission and authentication errors, and
any other 4xx response (SDK errors are judged by the HTTP status they carry).
The cloud SDKs also retry internally — Azure's backs off for up to ~15s on
dropped connections — so the end-of-run wait can exceed barca's own backoff. An attempt
that runs longer than `transfer_timeout` seconds (default 600, counted from
when the attempt starts, not while it waits its turn) is failed as stalled and
not retried — raise the limit if single artifacts take longer than that to
move over your link.

A failed upload is recorded as a `failed` row for that step with
`error_type = 'UploadError'`, no artifact path, the number of attempts made,
and the store error as `error_message` (`upload to <location> failed: …`);
the run's status is `failed`. `barca stats <asset>` shows it like any other
failure.

The local artifact directory doubles as a cache of the store: a second run on
the same machine reads from it without downloading anything. Nothing is
evicted automatically — delete `.barca/artifacts/` to reclaim space; anything
needed later is fetched again.

Remote I/O is reported on stderr, so its cost is visible:

```
[barca] pulled state (48.0 KB) in 0.03s
[barca] fetched 1 cached artifact (672.8 KB) in 0.1s
[barca] 2/2 steps done in 0.2s
[barca] uploaded 2 artifacts (672.8 KB); waited 0.0s at end of run
[barca] pushed state (48.0 KB) in 0.03s
```

The "waited" figure is the only upload time the run paid for — the rest
overlapped with execution. `BARCA_TRACE_TIMING=1` adds per-transfer timings.

Using a GCS emulator such as fake-gcs-server with gcsfs 2026.10 or later? Set
`GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT=false`: gcsfs's experimental mode makes a
gRPC call the emulator doesn't serve, and transfers stall until
`transfer_timeout`. Real GCS is unaffected.

`[remote].uri` may also be a plain directory (a shared or network mount)
instead of a URI; transfers are then local file copies.

## Checking a local copy against the store

When an artifact is uploaded, the SHA-256 of the local file is recorded with it in the shared
history. A machine uses that hash to decide whether its own copy is current:

- A copy already in `.barca/artifacts/` is hashed the first time a run reads it. If it does not
  match (edited by hand, or left from before another machine refreshed the result), it is
  replaced by the store's copy and reported as a fetch.
- A downloaded artifact is hashed too. If the store's copy does not match the recorded hash,
  the run still uses it and prints a warning naming the step. This is not an error: an
  artifact's path is `<node>/<run_hash>`, which identifies the computation and not the bytes,
  so a `--refresh`, or two machines computing the same step at once, overwrites the object.
  The warning names the step; `--refresh <file.py:name>` recomputes it, which clears the
  warning for every machine that shares this history. Until then, machines can hold
  different copies of that one result: a machine whose copy matches the recorded hash keeps
  it, and the others use the store's.

Only artifacts a run reads are hashed, once per run. Not checked: a parquet input that is read
in place (only byte ranges are fetched), and results recorded before barca stored a hash.

## Remote sinks

`@sink` paths accept the same URIs, independent of where the artifact store
lives:

```python
from barca import asset, sink

@asset
@sink('abfss://exports@myaccount.dfs.core.windows.net/daily/report.parquet')
def report():
    return build_dataframe()
```

A sink failure (missing extra, bad credentials, unreachable account) never
fails the parent asset — it is reported as `[barca] SINK FAILED: ...` and
recorded in the run's metadata.

## Staged writes

Serialized payloads are never buffered fully in memory — important when
assets are multi-hundred-MB DataFrames or pickled models:

1. The serializer (json/pickle/parquet) streams to a temp file in the
   destination directory (or `.barca/staging/` for a remote `@sink` —
   deliberately on project disk, not `/tmp`, which is often RAM-backed
   tmpfs).
2. Local: the temp file is atomically renamed into place (`os.replace`).
   Remote: the temp file is uploaded with a chunked `put_file`; object
   stores commit the object only when the upload completes.
3. On any failure the temp file is removed — the destination never holds a
   partial artifact. The staging directories of workers that are no longer
   running are swept at worker startup; a live worker's files are never touched.

The transfer helper follows the same rules: uploads stream from disk in
chunks, and fetches download to a temp file that is renamed into place only
when complete.

## Artifacts only, history local (0.4.0 behavior)

Set `BARCA_ARTIFACT_URI` to a URI prefix to keep artifacts in a store while
metadata stays local:

```bash
export BARCA_ARTIFACT_URI=abfss://artifacts@myaccount.dfs.core.windows.net/prod
barca get pipeline.py
```

Artifacts are written locally and transferred exactly as in remote mode; only
the metadata DB is not shared.

Prefer `BARCA_REMOTE_URI` with `BARCA_STATE=off`, which also keeps `--env` separation.

## Changing stores

Cache rows record each artifact's location in the store. If you point a
project at a different store, rows recorded against the old one are used
only when the artifact is still on local disk; otherwise those steps simply
recompute. A cache row whose object has been deleted from the current store
fails the run with the missing object named — re-run with `--refresh-all` to
recompute it.
