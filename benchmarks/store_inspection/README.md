# Store inspection cost

Run from an owned worktree with both private binaries available:

```bash
.venv/bin/python benchmarks/store_inspection/bench.py \
  --before /path/to/pre-change/barca --after .venv/bin/barca \
  --workdir target-local/store-cost --megabytes 1024
```

The work directory must not exist. The script creates a real materialization, replaces its
local/store parquet copies with matching synthetic files, and updates that fixture's recorded
digest/size. Each copy has about 1 GiB of uncompressed data plus parquet metadata. It measures
one cold and three warm CLI statuses, then three external metadata writes followed by real
`GET /state` requests. Every state result must expose the new materialization duration, so
returning old cached state does not count as a successful invalidated response. The server is
stopped in `finally` and the synthetic files remain under the worktree for teardown.

The same current Python helpers serve both native binaries: the old native request selects
the unchanged strict helper path, and the new request opts into inspection evidence. The
source/binary fingerprints, dependencies, CPU affinity, load, worker count and sample counts
are in [the final raw result](2026-10-30-settled-fast-local.json).
The benchmark copies each native binary without modifying its bytes and selects the same
benchmark Python interpreter through PATH for both helpers. This is a warm local-directory-store fast
profile comparison, not a cloud-network, TLS, release-wheel, cold-filesystem or 8.6 GB
whole-deployment measurement. It does not resolve the broader #655/#656 scaling gaps.

## 2026-10-30 measurement

Each artifact copy is 1,074,624,287 bytes. Shapes describe 134,217,728 rows of one int64 column.

| Operation | Before | After |
|---|---:|---:|
| Cold CLI status | 3.06 s | 1.14 s |
| Warm CLI status, three samples | 2.61–2.64 s | 82–94 ms |
| Metadata-invalidated `/state`, three samples | 2.52–4.50 s | 41–78 ms |
| Python artifact-hash bytes over warm CLI samples | 3,223,872,861 | 0 |
| Python artifact-hash bytes over invalidated state samples | 4,298,497,148 | 0 |

The Python hash probe excludes the old native local-file SHA-256 path, so before-byte counts
are conservative rather than total filesystem traffic. The after cold request records both
Python local/store hashes (2,149,248,574 bytes); warm/invalidated requests record neither.
Parquet footer/metadata reads and the bounded evidence-cache JSON are not artifact hashing.
Cold verification remains proportional to selected artifact bytes.

The [first prototype failed](2026-10-29-prototype-failure.json): CLI reuse worked but three
DB refreshes rehashed both copies, because evidence lived beside the server's transient DB
read view. The fix keys the cache to the stable project/environment instead. The real-server
[regression](../../python/tests/test_store_inspection_cost.py) locks down that lifetime.
[Cache controls](../../python/tests/test_inspection_cache.py) cover file replacement,
mtime-preserving edits, fixed remote expiry, backwards clocks, storage scope, strict-ledger
bypass, corruption, bounded retention, concurrent writers/readers and nonblocking write locks.

The [initial measurement](2026-10-29-fast-local.json) predates transfer invalidation and
verified-local-shape corrections. The [transfer-corrected measurement](2026-10-30-confirmed-transfers-fast-local.json)
predates the timestamp settling guard. The final run includes all three corrections and
waits 1.05 seconds after fixture writes before measuring cold verification. Recently changed
files bypass evidence lookup and retention; a settled observation is required before reuse.
This avoids keeping stale evidence when inode and timestamp fields collide within one tick.
