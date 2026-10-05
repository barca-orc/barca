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
and reads their metadata; it never deletes or lists. In practice that is `s3:GetObject`,
`s3:PutObject` and `s3:ListBucket` on S3, the Storage Object User role on GCS (replacing the
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
  the run still uses it and prints a warning naming the step. This is not an error: an
  artifact's path is `<node>/<run_hash>`, which identifies the computation and not the bytes,
  so a `--refresh`, or two machines computing the same step at once, overwrites the object.
  The warning names the step; `--refresh <file.py:name>` recomputes it, which clears the
  warning for every machine that shares this history. Until then, machines can hold
  different copies of that one result: a machine whose copy matches the recorded hash keeps
  it, and the others use the store's.

Only artifacts a run reads are hashed, once per run. Not checked: a parquet input that is read
in place (only byte ranges are fetched), and results recorded before barca stored a hash.

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
- **Cached artifact missing from the store** (deleted, or a different bucket): the run exits
  3 with `could not fetch ... cached artifact(s)`. Recompute with
  `barca get target pipeline.py --refresh-all`.
- **Stalled store**: lower `transfer_timeout` to fail faster; raise it if single artifacts
  legitimately take longer than 10 minutes to move.

`.barca/artifacts/` doubles as a local cache of the store and is never pruned automatically;
deleting it is safe (anything needed later is downloaded again).

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
- `barca status` does not describe a remote json or pickle result larger than 16 MB, and
  `barca sql` downloads a whole artifact before querying it.
- `.barca/artifacts/` has no size cap (see Failures).
