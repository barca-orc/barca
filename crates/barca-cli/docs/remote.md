# Remote storage: one cache shared by every machine

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
`[remote.storage_options.*]` over `FSSPEC_*` variables. Every key is in the configuration
reference: https://barca.sh/reference/config/

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
conflict, re-reads and merges. Set `BARCA_STATE=off` to keep history on each machine and share
only results.

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
- Before results are recorded, barca waits for every upload. The shared state never points
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
warnings only (`barca docs contract`).

A mismatch and a missing object are different findings and never stand in for each other: a
copy with other bytes is used and flagged, as above; an object that is not in the store at all
is computed again with `reason: "artifact_missing"` (see "Failures").

Only artifacts a run reads are hashed, once per run. Not checked: a parquet input that is read
in place (only byte ranges are fetched), and results recorded before barca stored a hash. A
`--dry-run` does not contact the store, so it never reports a mismatch.

## Ctrl-C

Ctrl-C cancels a run at any point, including while barca is uploading, downloading or pushing
the shared history: the command exits 130 with the `cancelled` error, and the run is recorded
as `cancelled`. What is left behind is always consistent:

- A step is recorded only once its artifact is confirmed in the store. A step whose upload
  was still in flight is not recorded, and runs again next time.
- No partial file is left. A download, and an upload into a store that is a directory, is
  written to a temp file beside its destination and renamed when whole; the temp file is
  removed when the run is cancelled. An object store shows an object only once its upload has
  completed, so an interrupted upload leaves the previous object, or none.
- Interrupted while the shared history is pushed, the run's artifacts are in the store but the
  shared history does not have the run. Its steps are recorded on this machine only, and the
  next run starts from the shared history as every run does: if one exists, those steps are
  computed again (and their objects overwritten); if none existed yet, they are cache hits and
  that run pushes them.
- Interrupted while the shared history is still being pulled, before anything ran, the command
  exits 130 and no run is recorded.

The end-of-run line (`[barca] <n>/<total> steps | done in <secs>s`) is about the steps. A Ctrl-C
that arrives after the last step finished, while artifacts upload or the history is pushed,
therefore follows a `done` line; the exit code and the error still say `cancelled`.

Barca's helper processes do not act on Ctrl-C themselves: the terminal sends it to every process
of the job, and the coordinator alone decides what it means and stops the helpers. They print
no `KeyboardInterrupt` traceback.

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

- **A directory where a local copy belongs**: not a store problem and not an error. The store's
  copy is fetched and the directory is moved aside, never deleted (`barca docs cache`, "A
  directory at an artifact's path"). A directory at an object's path inside a store that is a
  shared directory is different: barca changes nothing in the store except its own objects, so
  the transfer fails with exit 3 (`IsADirectoryError`, naming the path) until it is removed.

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

## Limitations

- `barca serve` does not share history yet; set `BARCA_STATE=off` for it.
- The shared history is updated once, when a run ends. A run records each finished step in the
  local copy as it goes (`barca docs cache`, "While a run is going, and after one is killed"), but
  other machines see none of it until the run ends, and never see a run that was killed. With a
  remote artifact store a run records nothing early: a step is recorded once its upload is
  confirmed, when the run ends.
- On the machine a run is on, `barca status` during the run and resuming after `kill -9` can be
  relied on only while no other machine updates the shared history in the meantime:
  every `barca get`, `barca run`, `--dry-run` and `barca status` starts by replacing the local
  copy with the shared one. After a run is killed, the next run on that machine can also
  overwrite history other machines added in the meantime (result files are not touched;
  those steps run again). If a run was killed and other machines are active, delete
  `.barca/metadata.db` and `.barca/metadata.db-wal` on that machine before the next run: it
  then starts from the shared history alone, and recomputes what the killed run had finished.
- `barca status` does not describe a remote json or pickle result larger than 16 MB, and
  `barca sql` downloads a whole artifact before querying it.
- `.barca/artifacts/` has no size cap (see Failures).
