---
title: Remote Storage
description: Store artifacts and shared state in S3, GCS, Azure ADLS Gen2, or Cloudflare R2.
---

Barca can store artifacts — the serialized outputs of every asset — in a
remote object store instead of the local `.barca/artifacts/` directory, and
`@sink` destinations can point at remote URIs directly. Amazon S3, Google
Cloud Storage, and Azure ADLS Gen2 are all first-class; Cloudflare R2 rides
on the S3 backend (it speaks the S3 API).

## Install the backend

Remote backends are optional extras — the core install stays dependency-free:

| Extra | Backend | URI schemes |
|---|---|---|
| `barca[s3]` | Amazon S3 (s3fs) | `s3://`, `s3a://` |
| `barca[r2]` | Cloudflare R2 (s3fs) — S3-compatible | `s3://` + R2 endpoint |
| `barca[gcs]` | Google Cloud Storage (gcsfs + google-cloud-storage) | `gs://`, `gcs://` |
| `barca[azure]` | Azure ADLS Gen2 / Blob (adlfs) | `abfs://`, `abfss://` |
| `barca[remote]` | all of the above | |

```bash
uv add 'barca[s3]'
```

Every backend is held to the **same shared-state contract** — conditional
create, cross-machine cache hit, concurrent-writer conflict → replay — by a
backend conformance suite that runs on every PR against local emulators
(MinIO for S3/R2, fake-gcs-server for GCS, Azurite for Azure). See
[Releases](/contributing/releases/) for the guarantees each backend makes.

## Remote mode: shared state + artifacts

Point `[remote].uri` in `barca.toml` (or `BARCA_REMOTE_URI`) at an object
store prefix and barca shares **both** artifacts and materialization state
across machines:

```toml
# barca.toml
[remote]
uri = "abfss://pipelines@myaccount.dfs.core.windows.net/barca/my-project"
```

- Artifacts are stored **content-addressed** at
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
re-execution. See [Configuration](/reference/config/) for the full schema,
environment separation (`--env`), and env-var overrides.

`barca serve` does not support shared state yet — set `state = "off"` for
served projects.

### Local-first artifacts, background transfer

Workers never talk to the object store. They write every artifact to the
local artifact directory (`.barca/artifacts/`, or `.barca/envs/<env>/artifacts/`)
and read their inputs from there, so a step's critical path is local disk
only. A single helper process per run (`python -m barca._transfer`) moves
bytes between that directory and the store in the background, using the
same fsspec backends and credentials as everything else:

- **Upload** — the moment a step finishes, its artifact is queued for
  upload while downstream steps keep running against the local copy.
  Up to `transfer_concurrency` transfers run at once (default 4).
- **Fetch** — a cache hit recorded by another machine is downloaded to its
  local path just before the first step that reads it runs. Cached
  intermediates that nothing in the run reads are never downloaded — a fully
  cached `barca get` fetches only the final output.
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

### Artifacts-only mode (0.4.0 behavior)

Set `BARCA_ARTIFACT_URI` to a URI prefix to keep artifacts in a store while
metadata stays local:

```bash
export BARCA_ARTIFACT_URI=abfss://artifacts@myaccount.dfs.core.windows.net/prod
barca get pipeline.py
```

Artifacts are written locally and transferred exactly as in remote mode; only
the metadata DB is not shared.

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
   partial artifact. Stale temp files from crashed workers are swept at
   worker startup.

The transfer helper follows the same rules: uploads stream from disk in
chunks, and fetches download to a temp file that is renamed into place only
when complete.

## Credentials

Barca passes no credentials — each backend uses its native default chain:

- **S3 (s3fs)**: the standard boto chain — `AWS_ACCESS_KEY_ID`, profiles,
  instance metadata.
- **GCS**: `google.auth` application default credentials. Artifact I/O uses
  gcsfs; the shared-state path uses the `google-cloud-storage` SDK directly
  (gcsfs cannot express a generation precondition on overwrite) — both read
  the same ADC chain.
- **Azure (adlfs)**: `DefaultAzureCredential` — env vars
  (`AZURE_CLIENT_ID`/`AZURE_CLIENT_SECRET`/`AZURE_TENANT_ID`), managed
  identity, Azure CLI login, etc. `AZURE_STORAGE_ACCOUNT_NAME` /
  `AZURE_STORAGE_ACCOUNT_KEY` and connection strings also work.

For anything the default chains can't express, `BARCA_STORAGE_OPTIONS`
takes a JSON object keyed by fsspec protocol, splatted into the filesystem
constructor (equivalently, `[remote.storage_options.<protocol>]` in
`barca.toml`):

```bash
export BARCA_STORAGE_OPTIONS='{"abfs": {"account_name": "myaccount", "anon": false}}'
```

### Cloudflare R2

R2 is S3-compatible, so it uses the `s3://` schemes with the S3 backend
(`barca[r2]` or `barca[s3]`) pointed at your account's R2 endpoint. Set the
endpoint in `storage_options` under the `s3` protocol; credentials are your
R2 access key / secret via the usual boto env vars:

```toml
# barca.toml
[remote]
uri = "s3://my-bucket/barca/my-project"

[remote.storage_options.s3]
client_kwargs = { endpoint_url = "https://<account-id>.r2.cloudflarestorage.com" }
```

```bash
export AWS_ACCESS_KEY_ID=<r2-access-key-id>
export AWS_SECRET_ACCESS_KEY=<r2-secret-access-key>
```

R2 supports the same `If-Match` conditional writes barca's shared state relies
on. As with S3, the state blob must stay under the 48 MiB single-request limit
(the coordinator errors clearly if it grows past that).

## Changing stores

Cache rows record each artifact's location in the store. If you point a
project at a different store, rows recorded against the old one are used
only when the artifact is still on local disk; otherwise those steps simply
recompute. A cache row whose object has been deleted from the current store
fails the run with the missing object named — re-run with `--no-cache` to
recompute it.
