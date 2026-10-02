# Remote storage

Share artifacts and cache state between machines through an object store (S3, Azure Blob,
GCS, Cloudflare R2) or a shared directory. Configure it in `barca.toml`:

```toml
[remote]
uri = "s3://my-bucket/barca/my-project"
```

Install the matching extra: `pip install "barca[s3]"`, `[azure]`, `[gcs]`, `[r2]` or
`[remote]`. Credentials come from each SDK's default chain; extra options go under
`[remote.storage_options.<protocol>]`. Full reference: https://barca.sh/reference/remote-storage/

## What happens during a run

- The shared metadata DB is pulled before the run and pushed after it (conditional upload;
  a concurrent push from another machine is merged by replaying this run's rows).
- Steps always read and write local files under `.barca/artifacts/`. A helper process
  uploads each artifact in the background as soon as its step finishes.
- A cache hit recorded by another machine is downloaded just before the first step that
  reads it. Cached intermediates nothing reads are never downloaded.
- Before results are recorded, barca waits for every upload. The shared state never points
  at an artifact that is missing from the store.

stderr reports each part, so remote cost is visible:

```
[barca] pulled state (48.0 KB) in 0.03s
[barca] fetched 1 cached artifact (672.8 KB) in 0.1s
[barca] uploaded 2 artifacts (672.8 KB); waited 0.0s at end of run
[barca] pushed state (48.0 KB) in 0.03s
```

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

- **Upload failed**: the run exits 1 and names the step. The step gets a `failed` row with
  `error_type = 'UploadError'`, no artifact path, and the attempt count; it recomputes on
  the next run. `barca stats target pipeline.py` shows the failure.
- **Cached artifact missing from the store** (deleted, or a different bucket): the run exits
  1 with `could not fetch ... cached artifact(s)`. Recompute with
  `barca get target pipeline.py --no-cache`.
- **Stalled store**: lower `transfer_timeout` to fail faster; raise it if single artifacts
  legitimately take longer than 10 minutes to move.

`.barca/artifacts/` doubles as a local cache of the store and is never pruned automatically;
deleting it is safe (anything needed later is downloaded again).

Using a GCS emulator (e.g. fake-gcs-server) with gcsfs 2026.10 or later: set
`GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT=false`. gcsfs's experimental mode calls a gRPC API the
emulator doesn't serve, and transfers stall until `transfer_timeout`.
