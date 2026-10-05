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
<uri>/<env>/artifacts/<node>/<run_hash>.<ext>   one immutable file per result
<uri>/<env>/state/metadata.db                   run history, pulled at the start of a run
```

`<env>` is `default` unless you pass `--env` (`barca docs cache`). Barca reads and writes objects
and reads their metadata; it never deletes or lists. In practice that is `s3:GetObject`,
`s3:PutObject` and `s3:ListBucket` on S3, the Storage Object User role on GCS (replacing the
history object needs delete permission there), and Storage Blob Data Contributor on Azure.

Two machines finishing runs at the same time do not lose history: the second detects the
conflict, re-reads and merges. Set `BARCA_STATE=off` to keep history on each machine and share
only results.

From a remote store, `barca get --json` reports every result as a pointer
(`{"_barca_artifact": {"path", ...}}`), json ones included; `barca.get()` in Python loads it.

## How steps read remote inputs

The input's annotation decides how many bytes a step moves (`barca docs types`):

- A parquet input annotated `duckdb.DuckDBPyRelation` or `pl.LazyFrame` is read in place:
  only the byte ranges the step's query touches are fetched. A query over one column of eight
  fetches about that column's share of the object; a selective filter skips row groups.
- Every other input (no annotation, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table`, json,
  pickle) is downloaded whole to `.barca/staging/{pid}/`, loaded, and the file removed.

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

- Keys from `partitions_from(<asset returning a list>)` are read from local disk: with a remote
  store the step errors.
- `parallel()` return values come back as `null` with a remote store (a warning says so).
- `barca serve` does not share history yet; set `BARCA_STATE=off` for it.
- `barca status` does not describe a remote json or pickle result larger than 16 MB, and
  `barca sql` downloads a whole artifact before querying it.

## How shared history works

- Artifacts are written **content-addressed** to
  `{uri}/{env}/artifacts/{node}/{run_hash}{ext}` — immutable objects, so a
  cache hit on one machine is valid on every machine.
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

1. The serializer (json/pickle/parquet) streams to a temp file — in the
   destination directory for local writes, in `.barca/staging/{pid}/` (one
   directory per worker process) for remote ones (deliberately on project
   disk, not `/tmp`, which is often RAM-backed tmpfs).
2. Local: the temp file is atomically renamed into place (`os.replace`).
   Remote: the temp file is uploaded with a chunked `put_file`; object
   stores commit the object only when the upload completes.
3. On any failure the temp file is removed — the destination never holds a
   partial artifact. The staging directories of workers that are no longer
   running are swept at worker startup; a live worker's files are never touched.

Eager remote reads are symmetric: the input is downloaded to the worker's
staging directory, deserialized, and the temp file removed. A parquet input typed
`duckdb.DuckDBPyRelation` or `pl.LazyFrame` is not downloaded: it is read in
place, fetching only the byte ranges the step's query touches (see "How steps
read remote inputs" above).

## Artifacts only, history local (0.4.0 behavior)

Set `BARCA_ARTIFACT_URI` to a URI prefix and every materialized asset is
written there instead of `.barca/artifacts/`, while metadata stays local:

```bash
export BARCA_ARTIFACT_URI=abfss://artifacts@myaccount.dfs.core.windows.net/prod
barca get pipeline.py
```

Downstream steps download eager inputs to a local staging file on demand and
read lazy (`duckdb.DuckDBPyRelation`, `pl.LazyFrame`) parquet inputs in place.

Prefer `BARCA_REMOTE_URI` with `BARCA_STATE=off`, which also keeps `--env` separation.
