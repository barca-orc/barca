# Benchmark Results

Latest full pass: 2026-07-17, on the code that shipped as barca 0.7.0, via the Docker harness
(`benchmarks/docker/`). Each section below is dated; the summary, profile and parallel-mode
sections near the end are from v0.2.0 (June 2026).

> **Caveat (2026-10-20):** none of these numbers come from the current release (0.26.1), and
> passes on different hardware are not comparable in absolute time; only the
> barca/Dagster/Prefect ratios within one pass mean anything. #65 re-runs the suite on the
> current harness; until then, re-run `bench.sh` before quoting a number.

## Standardized re-run via Docker harness (2026-07-17)

Ran via the new `benchmarks/docker/bench.sh` (see `benchmarks/README.md#reproducible-fairness-via-docker-recommended`):
Docker Desktop's Linux VM on an Apple Silicon Mac (M4 Max host, 16 cores/64GB — VM itself
provisioned with 12 vCPU/24GB), container `arm64` (aarch64), **hard-enforced** via `docker
run --cpuset-cpus=0-3 --memory=4g`  — 4 cores, 4GiB RAM, identical ceiling for barca,
Dagster, and Prefect alike, not just reported but actually unexceedable by the container.
barca Python 3.12 (uv venv baked into the image alongside the wheel), Dagster/Prefect each
in their own `uv sync`'d venv, also baked in at image build time so no timed run touches
the network. `/proc/cpuinfo` on this VM's kernel doesn't expose a `model name` field (ARM64
convention — `CPU part`/`CPU implementer` instead), hence "cpu model: unknown" in each run's
banner; two bugs this exposed are fixed as part of this pass, see Notes.
`--warmup 1 --runs 5` per each benchmark's own `bench.sh` defaults (`--warmup 3 --runs 10`
for `trivial`), `BARCA_BENCH_MEMORY=1` requested on every run but unavailable this pass —
see Notes.

**A first attempt on this environment surfaced a real barca-core bug, not just environment
noise — fixed before the table below, not after.** A first full pass showed dagster winning
or roughly tying on 14 of the 20 rows (`deep_diamond`, `etl_duckdb`, `etl_duckdb_dataframes`,
`large_payloads`, `map_reduce`, `mixed_io_cpu`, `multi_file_discovery`, `parallel_tasks`,
`partitioned_etl`, `partitioned_fan_in`, `resilience_pileup` (~tied), `spaceflights`,
`wide_join`, `wide_layers`) — a sharp reversal from the 2026-07-16 pass, where barca won
nearly everything. Re-running `deep_diamond`, `mixed_io_cpu`, and `wide_join` a second time
in isolation reproduced the same ratios within ~2-3%, ruling out sampling noise as the
explanation. `BARCA_TRACE_TIMING=1` (the waterfall tracer already in
`crates/barca-core/src/commands.rs`, see its doc comment) pointed at the actual cause: on
`deep_diamond`, the trace showed 18/18 steps complete by ~285ms into a ~1160ms total run —
the missing ~830ms was entirely inside a single call, `pool.shutdown()`. `fan_out_500`
showed the identical ~830ms shutdown cost despite doing 500x more real work, while
`chain_100` — which never needs more than one worker, since a linear chain has no
independent-branch parallelism to spawn a second one for — paid only ~200ms. That pointed
straight at `WorkerPool::shutdown()` in `crates/barca-core/src/io_loop.rs`: it looped over
every spawned worker *sequentially*, sending SIGTERM and then unconditionally sleeping
200ms before checking whether the process had exited (`graceful_kill()`) — a flat
`pool_size * 200ms` tax on every single run (~800ms for the default 4-worker pool),
regardless of how much or little actual work the run did. This bug isn't new — it's been in
every prior pass too — but a slower CPU (the 2026-07-16 pass's Intel Xeon @ 2.80GHz
container) made Dagster's own compute-bound overhead proportionally larger, which masked
barca's flat, CPU-speed-independent shutdown tax. On the much faster cores here (Apple M4
Max), Dagster's overhead shrank while barca's fixed `sleep()`-based teardown didn't, so it
started dominating wall time on every benchmark whose DAG isn't a single long chain.
**Fixed** in this PR: `shutdown()` now signals every worker first, then polls all of them
together for one shared 200ms grace window instead of sleeping 200ms per worker in
sequence, before falling back to SIGKILL for stragglers. Confirmed via the same tracer:
`deep_diamond`'s `pool_shutdown` phase dropped from ~830ms to ~9ms, and its total run time
roughly halved. The table below is the **post-fix** re-run — barca now wins or ties on
19/20 rows, restoring (and, on several rows, beating) the 2026-07-16 pass's pattern, with
`etl_duckdb` (1.06x dagster) and `large_payloads` (1.05x dagster) the only two within noise
of a tie, consistent with those two being the pass's known cross-process-serialization edge
cases (see the 2026-07-16 section's Notes on `etl_duckdb`).

| Benchmark | barca | dagster | prefect | barca vs dagster | barca vs prefect |
|---|---:|---:|---:|---:|---:|
| trivial | 37.7ms ± 1.7ms | 521.6ms ± 6.7ms | 4.108s ± 0.047s | 13.84x faster | 109.01x faster |
| chain_100 | 123.1ms ± 6.2ms | 1.101s ± 0.017s | 4.128s ± 0.051s | 8.94x faster | 33.53x faster |
| deep_diamond | 161.2ms ± 1.1ms | 652.2ms ± 3.6ms | 4.169s ± 0.050s | 4.05x faster | 25.86x faster |
| etl_duckdb | 1.047s ± 0.077s | 992.7ms ± 13.5ms | 10.241s ± 0.088s | **~tied** (dagster 1.06x) | 9.78x faster |
| etl_duckdb_dataframes | 394.9ms ± 16.2ms | 820.2ms ± 10.7ms | 4.355s ± 0.059s | 2.08x faster | 11.03x faster |
| fan_out_500 | 903.4ms ± 58.2ms | 2.306s ± 0.007s | 4.648s ± 1.032s | 2.55x faster | 5.15x faster |
| fan_out_500_50ms | 7.620s ± 0.053s | 35.415s ± 0.095s | 10.166s ± 0.047s | 4.65x faster | 1.33x faster |
| incremental_backfill | 608.6ms ± 12.2ms | 5.904s ± 0.032s | 41.773s ± 0.918s | 9.70x faster | 68.64x faster |
| large_payloads | 660.3ms ± 9.7ms | 629.1ms ± 5.7ms | 6.193s ± 0.030s | **~tied** (dagster 1.05x) | 9.38x faster |
| map_reduce | 197.0ms ± 8.4ms | 882.1ms ± 5.3ms | 4.192s ± 0.014s | 4.48x faster | 21.28x faster |
| mixed_io_cpu | 310.1ms ± 14.0ms | 1.038s ± 0.005s | 4.465s ± 0.756s | 3.35x faster | 14.40x faster |
| multi_file_discovery | 276.7ms ± 8.7ms | 862.5ms ± 3.9ms | 4.149s ± 0.037s | 3.12x faster | 15.00x faster |
| parallel_tasks | 150.1ms ± 2.0ms | 563.2ms ± 10.2ms | 3.975s ± 0.057s | 3.75x faster | 26.49x faster |
| partitioned_chain | 288.5ms ± 5.7ms | 1.276s ± 0.015s | 3.925s ± 0.057s | 4.42x faster | 13.60x faster |
| partitioned_etl | 149.8ms ± 3.8ms | 818.5ms ± 11.4ms | 3.973s ± 0.060s | 5.46x faster | 26.53x faster |
| partitioned_fan_in | 229.7ms ± 12.4ms | 978.6ms ± 6.6ms | 3.965s ± 0.086s | 4.26x faster | 17.26x faster |
| resilience_pileup | 1.934s ± 0.009s | 2.297s ± 0.028s | 5.915s ± 0.655s | 1.19x faster | 3.06x faster |
| spaceflights | 580.2ms ± 7.9ms | 1.058s ± 0.002s | 4.013s ± 0.046s | 1.82x faster | 6.92x faster |
| wide_join | 133.4ms ± 1.3ms | 599.7ms ± 14.9ms | 3.946s ± 0.061s | 4.50x faster | 29.59x faster |
| wide_layers | 360.6ms ± 6.5ms | 905.7ms ± 2.5ms | 3.910s ± 0.058s | 2.51x faster | 10.84x faster |

### Peak memory

Unavailable this pass. `BARCA_BENCH_MEMORY=1` was set on every run, but the opt-in
whole-process-tree memory measurement (`bench_mem_peak` in `benchmarks/lib/env.sh`) needs
to create its own child cgroup with a writable `cgroup.subtree_control` inside the
already-restricted outer container, which Docker Desktop's container doesn't expose without
`--privileged` (out of scope for a "minimal" harness) — every row correctly falls back to
reporting "memory measurement unavailable" per its documented graceful-degradation path
rather than a misleading number.

**Since fixed (2026-07-20):** the harness now runs the container `--privileged` and the
entrypoint delegates the cgroup v2 memory controller at boot, which makes the cgroup
peak-memory path work under Docker Desktop — first exercised by the scheduler-overhead
pass below. A future full-suite re-run would fill this table too.

### Notes

- **The worker-pool shutdown fix** (`crates/barca-core/src/io_loop.rs`,
  `WorkerPool::shutdown()`): previously, `graceful_kill()` was called once per worker in a
  plain `for` loop — each call sent SIGTERM, then unconditionally `std::thread::sleep`'d
  200ms before its first liveness check, only moving to SIGKILL if the worker was still
  alive after that. With `pool_size` workers (default 4) all needing termination at the end
  of a run, that's up to 4 sequential 200ms sleeps — ~800ms of pure `thread::sleep`, on
  every single `barca get`/`barca run` invocation, independent of workload size. Fixed by
  sending SIGTERM (and SIGCONT-then-SIGTERM for frozen `parallel()` workers) to every
  worker up front, then polling all of them together in one shared 200ms window (5ms poll
  interval) before SIGKILL-ing any stragglers — turning an O(pool_size) cost into O(1). This
  is a real, general barca-core performance fix, not benchmark-specific: any `barca`
  invocation that spawns more than one worker pays this tax on exit, so every real-world
  multi-asset project benefits, not just this suite.
- Two smaller bugs surfaced and fixed while building `benchmarks/docker/`, both specific to
  running the existing benchmark harness on ARM64 Linux for the first time (every prior
  pass ran x86_64):
  - `bench_env_banner()` in `benchmarks/lib/env.sh` unconditionally did
    `cpu_model="$(grep -m1 'model name' /proc/cpuinfo | ...)"`; ARM64's `/proc/cpuinfo` has
    no `model name` line (it uses `CPU part`/`CPU implementer` instead), so `grep` exits 1,
    and under the calling `bench.sh`'s `set -euo pipefail` that aborted the entire benchmark
    before hyperfine ever ran. Fixed by tolerating the failure (`|| true`) for both the CPU
    model and `free -h` lookups, falling back to "unknown" — matches the function's existing
    `${cpu_model:-unknown}` display fallback, which was already written assuming the
    assignment could come back empty but not that it could kill the script outright.
  - The runtime image was also missing `free` (`procps` isn't installed by default in the
    `bookworm-slim` base) — added explicitly for the same banner.
  - Root barca venv (`/work/.venv` in the image, `.venv` in local dev) needs
    `scikit-learn`, `pandas`, and `pyarrow` installed alongside the barca wheel itself —
    `spaceflights/barca/assets.py` and `etl_duckdb_dataframes/barca/assets.py` import them
    directly, and unlike the Dagster/Prefect sides there's no separate per-benchmark barca
    venv to isolate them in. This mirrors `.github/workflows/ci.yml`'s benchmark-smoke-test
    step, which does the same `pip install scikit-learn` / `pip install pandas pyarrow`
    alongside its own wheel-installed barca. Without this, both benchmarks failed their
    first hyperfine warmup run with a barca-side import error before any timing was
    collected.
- The Prefect `.submit()` fix from the 2026-07-16 "Further note" above is included in this
  pass (it landed on `main` before this pass ran) — Prefect still loses every single row by
  a wide margin regardless, consistent with the 2026-07-16 pass's conclusion that its high
  fixed per-run overhead dominates regardless of environment or concurrency fixes.

## Standardized re-run (2026-07-16)

Ran on this container: 4 vCPU (Intel Xeon @ 2.80GHz, pinned via `taskset -c 0-3`),
15 GiB RAM, barca Rust release build, Python 3.13 (barca) / 3.12 (Dagster, Prefect).
Worker count = 4 for all three frameworks (`BARCA_BENCH_CORES` default, `--warmup 1
--runs 5` except trivial at `--warmup 3 --runs 10`). Absolute times are inflated by
container overhead (cold PyPI-installed venvs, virtualized CPU, no dedicated
hardware, shared with whatever else is on the host) — read the **relative** ratios,
not the raw ms/s, and don't compare these numbers to the Apple Silicon rows above.

| Benchmark | barca | dagster | prefect | barca vs dagster | barca vs prefect |
|---|---:|---:|---:|---:|---:|
| trivial | 339ms ± 13ms | 1.98s ± 0.15s | 10.74s ± 0.24s | 5.85x faster | 31.68x faster |
| parallel_tasks | 1.28s ± 0.02s | 2.10s ± 0.06s | 11.46s ± 0.94s | 1.64x faster | 8.92x faster |
| resilience_pileup | 2.56s ± 0.02s ⚠ | 3.61s ± 0.04s | 11.75s ± 0.21s | 1.41x faster | 4.59x faster |
| chain_100 | 765ms ± 80ms | 3.88s ± 0.09s | 11.09s ± 0.13s | 5.07x faster | 14.50x faster |
| deep_diamond | 1.49s ± 0.04s | 2.41s ± 0.15s | 11.11s ± 0.17s | 1.62x faster | 7.46x faster |
| etl_duckdb | 4.44s ± 0.10s | 3.59s ± 0.22s | 47.63s ± 0.83s | **dagster 1.24x faster** † | 13.28x faster |
| etl_duckdb_dataframes ‡ | 1.54s ± 0.03s | 2.36s ± 0.05s | 9.56s ± 0.13s | 1.53x faster | 6.19x faster |
| fan_out_500 | 2.69s ± 0.06s | 8.59s ± 0.32s | 12.95s ± 0.15s | 3.20x faster | 4.82x faster |
| fan_out_500_50ms | 9.84s ± 0.55s | 34.89s ± 0.27s | 37.92s ± 0.16s | 3.55x faster | 3.85x faster |
| incremental_backfill | 16.25s ± 0.17s | 20.47s ± 0.29s | 108.52s ± 1.47s | 1.26x faster | 6.68x faster |
| large_payloads | 3.67s ± 0.40s | 2.14s ± 0.07s | 17.70s ± 0.12s | **dagster 1.72x faster** | 8.29x faster |
| map_reduce | 2.57s ± 0.07s | 3.09s ± 0.14s | 10.48s ± 0.04s | 1.20x faster | 4.08x faster |
| mixed_io_cpu | 3.07s ± 0.08s | 3.05s ± 0.11s | 11.27s ± 0.34s | **dagster 1.01x faster** (~tied) | 3.70x faster |
| multi_file_discovery | 3.16s ± 0.51s ⚠ | 3.15s ± 0.15s | 10.73s ± 0.33s | **~tied** (1.00x) | 3.40x faster |
| partitioned_chain | 3.37s ± 0.13s | 4.32s ± 0.05s | 11.45s ± 0.50s | 1.28x faster | 3.40x faster |
| partitioned_etl | 2.96s ± 0.06s | 2.95s ± 0.17s ⚠ | 10.98s ± 0.14s | **~tied** (1.00x) | 3.72x faster |
| partitioned_fan_in | 3.43s ± 0.11s | 3.49s ± 0.07s | 10.81s ± 0.11s | 1.02x faster (~tied) | 3.15x faster |
| spaceflights | 4.34s ± 0.22s | 4.01s ± 0.20s | 12.26s ± 0.17s | **dagster 1.08x faster** | 3.06x faster |
| wide_join | 3.14s ± 0.13s | 2.19s ± 0.08s | 11.18s ± 0.26s | **dagster 1.43x faster** | 5.10x faster |
| wide_layers | 3.53s ± 0.61s ⚠ | 3.47s ± 0.10s | 11.09s ± 0.21s | **~tied** (1.02x) | 3.19x faster |

⚠ = hyperfine flagged statistical outliers for that row's barca (or, for
partitioned_etl, dagster) measurement — see variance notes below.

† = stale as of the serialization fix described in Notes — `etl_duckdb`'s two
heaviest assets now use `serializer="pickle"`, closing this to ~1.07-1.14x
dagster-faster (within noise). The row above is left as originally measured
since it's what motivated the fix; see Notes for before/after numbers. ‡ =
`etl_duckdb_dataframes` was added later as a follow-up benchmark (same
topology, DataFrame/parquet payloads instead of dict-of-rows) and measured in
isolation via `bench.sh 8 2`, not as part of this table's single full-suite
pass — see Notes before comparing its absolute times to the other rows here.

### Variance

Relative standard deviation (σ/mean) per framework, averaged across the original
19 full-suite-pass rows (excludes `etl_duckdb_dataframes`, added later and
measured separately — see the table's ‡ footnote):
**barca ~5.2%, dagster ~3.6%, prefect ~1.9%**. Barca is consistently the noisiest
of the three here, and four rows crossed hyperfine's outlier-detection threshold
(resilience_pileup, multi_file_discovery, partitioned_etl's dagster leg,
wide_layers) — worst case was wide_layers at σ = 17% of the mean. Prefect and
dagster are quieter because their per-run cost is dominated by fixed Python
import/framework overhead, which swamps scheduler jitter; barca's per-run cost is
smaller in absolute terms, so the same container noise is a larger fraction of it.
This reads as environment noise (a shared, virtualized 4-vCPU container), not a
barca-specific issue — but it does mean any individual row within ~1.3x of 1.0
(the "~tied" ones above) shouldn't be read as a confident win either direction
without more runs or a quieter machine. `benchmarks/lib/env.sh` pins cores via
`taskset`, which helps, but can't fully isolate a shared host from noisy
neighbors the way dedicated hardware would.

### Peak memory (whole process tree)

Collected separately via `BARCA_BENCH_MEMORY=1` (opt-in — see
`benchmarks/README.md#methodology` — not part of the timed hyperfine runs above,
so it isn't in the variance figures either):

| Benchmark | barca | dagster | prefect |
|---|---:|---:|---:|
| trivial | 55.8 MB | 156.7 MB | 410.5 MB |
| chain_100 | 32.2 MB | 132.9 MB | 408.8 MB |
| deep_diamond | 62.6 MB | 120.5 MB | 373.7 MB |
| etl_duckdb | **287.6 MB** | 265.8 MB | 482.4 MB |
| etl_duckdb_dataframes | 258.2 MB | 174.4 MB | 386.7 MB |
| fan_out_500 | 66.1 MB | 136.5 MB | 404.1 MB |
| fan_out_500_50ms | 64.4 MB | 137.4 MB | 460.6 MB |
| incremental_backfill | 25.1 MB | 113.4 MB | 378.0 MB |
| large_payloads | 62.2 MB | 124.6 MB | 384.9 MB |
| map_reduce | 56.2 MB | 116.9 MB | 376.3 MB |
| mixed_io_cpu | 53.3 MB | 112.2 MB | 359.4 MB |
| multi_file_discovery | 56.6 MB | 116.4 MB | 373.6 MB |
| parallel_tasks | 63.2 MB | 117.0 MB | 395.4 MB |
| partitioned_chain | 54.7 MB | 119.6 MB | 381.7 MB |
| partitioned_etl | 56.1 MB | 115.1 MB | 370.4 MB |
| partitioned_fan_in | 55.2 MB | 117.2 MB | 371.8 MB |
| resilience_pileup | 52.8 MB | 119.5 MB | 372.5 MB |
| spaceflights | **409.4 MB** | 322.5 MB | 563.7 MB |
| wide_join | 54.6 MB | 87.6 MB | 361.6 MB |
| wide_layers | 55.6 MB | 90.8 MB | 371.5 MB |
| **median** | **56.1 MB** | **119.5 MB** | **378.0 MB** |

Barca's footprint is roughly half dagster's and a sixth to an eighth of prefect's
almost everywhere — except `etl_duckdb` and `spaceflights`, which spike to match
or exceed dagster's own footprint. Those are the same two benchmarks flagged below
as barca's genuine (not noise) losses, and the memory spike is the same root cause
showing up a second way — see Notes.

### Notes

- **Barca does not universally win here, and most of that turned out to be noise
  rather than a real result — verified, not assumed.** The table above was
  generated over roughly an hour; re-running individual rows afterward, in
  isolation, showed several "dagster wins" reverse completely (`wide_layers`:
  dagster-favored in the table above → barca 2.17x faster on rerun; `wide_join`:
  dagster 1.43x faster above → barca 1.29x faster on rerun; `large_payloads`:
  dagster 1.72x faster above → ~tied, barca 1.05x faster on rerun). The tell is in
  the CPU-time breakdown hyperfine already reports: dagster's `(user+sys)/wall` is
  ≈1.00 on almost every row (it's single-process and sequential — script mode, per
  the Execution modes table below — so wall time *is* CPU time, nothing hidden).
  Barca's ratio sits at 0.3–0.5 almost everywhere: most of its wall-clock time is
  spent *waiting* (process coordination, socket IPC, DB writes), which is exactly
  what a shared, virtualized container's scheduler jitter hits hardest — a
  multi-process architecture has more "waiting" surface for noise to land on than
  a single sequential process does.
- **`etl_duckdb` is the one benchmark confirmed real, not noise** — re-run twice
  (~1.24x, then ~1.40x dagster-faster) under different container load and it held
  both times, unlike the others above. Its CPU-time pattern is also qualitatively
  different: barca's *user* time is higher than dagster's here (barca is doing
  more total compute, not just waiting), and its peak memory (287.6 MB) roughly
  matches dagster's rather than sitting far below it like every other benchmark.
  The mechanism: `etl_duckdb` generates three separate 100k-row datasets
  (`raw_orders`, `raw_customers`, `raw_products`) that flow through *parallel*
  raw→staging→intermediate branches before merging. Barca's coordinator + separate
  Python workers is genuinely multi-process, so each parallel branch's 100k-row
  payload gets serialized to a JSON artifact on disk and deserialized by the next
  worker — real, repeated cross-process I/O and duplicated in-memory copies.
  Dagster's script-mode execution is single-process and sequential, so it just
  passes Python objects by reference between steps — zero serialization cost.
  `spaceflights` (sklearn models/dataframes, also flagged above) shows the same
  memory-spike signature and is worth the same suspicion, though it wasn't
  independently re-run to confirm reproducibility the way `etl_duckdb` was.
  `large_payloads` looks superficially similar (also large payloads) but is a
  *linear* chain — one worker handles it start to finish with no cross-process
  handoff — which is consistent with it turning out to be noise, not a real cost:
  the tax shows up when large payloads *and* parallel branching combine, not from
  payload size alone.
- **`etl_duckdb`'s loss was investigated further and substantially fixed** (not
  just diagnosed). Root cause was two-layered: (1) the cross-process
  serialization cost above was real, but (2) barca's own per-step timing was
  *undercounting* it — `_materialize()` in `python/barca/_worker.py` measured
  `elapsed`/`cpu_seconds` before calling `serialize()`, so the actual artifact
  write (the expensive part for large payloads) was invisible to barca's own
  cost model, `barca stats`, and `barca history`. Fixed by moving the
  measurement to wrap `serialize()` too — confirmed via a `BARCA_TRACE_TIMING=1`
  waterfall trace (new, permanent, zero-cost-when-unset debug env var) that
  `raw_orders`'s self-reported cost jumped from a wrong 396ms to a correct
  ~1049ms, and the run's previously-"unaccounted" ~1.6s coordination gap shrank
  to ~0.37s of legitimate, understood overhead (worker spawn + DB init/persist).
  This fix benefits any barca project with large-payload assets, not just this
  benchmark. Two follow-up changes then closed most of the actual gap:
  - **Pickle quick win** (`benchmarks/etl_duckdb/barca/assets.py`): added
    `serializer="pickle"` to `raw_orders` and `stg_orders`, the two heaviest
    payloads (14.9MB/8.1MB as JSON vs 5.3MB/3.4MB as pickle — pickle is both
    smaller and much faster to (de)serialize for this list-of-small-dicts
    shape: measured `json.dump` ~635ms vs `pickle.dump` ~64ms for
    `raw_orders`). This is an existing, already-shipped `@asset()` kwarg, not
    an engine change. Result, two confirming isolated runs
    (`bench.sh 8 2`): dagster's lead narrowed from 1.24-1.40x to **1.07x, then
    1.14x** — within noise of tied. One side effect worth documenting: pickled
    `raw_orders` drops under the in-process `_ArtifactLRU`'s 8MB cache
    threshold (`python/barca/_worker.py`), making it newly eligible for
    caching — but its only consumer runs on a different worker process, so the
    cache can never hit and the `copy.deepcopy()` mutation-safety guard
    (measured ~469ms for this payload) is pure waste on every read. Left
    unfixed (out of scope, core-engine cache logic) but noted for anyone
    chasing a similar case where deepcopy cost outweighs a cache that can't be
    hit.
  - **DataFrame + parquet rewrite** (new sibling benchmark,
    `benchmarks/etl_duckdb_dataframes/`, same 11-asset topology across all
    three frameworks so the comparison isn't conflated with a framework
    difference): rewrote each asset from manual Python loops over dict-of-rows
    to vectorized pandas (`groupby`/`merge`/`assign`). Barca auto-detects
    DataFrame return values and routes them to parquet
    (`detect_format()`/`resolve_format()` in `python/barca/_artifacts.py`) —
    no code changes needed beyond returning DataFrames; confirmed via
    `.barca/artifacts/` that every asset except the two dict-of-scalars mart
    outputs serialized as `.parquet`. This is a categorically faster
    mechanism than JSON or pickle for tabular data (vectorized, typed-array
    (de)serialization via pyarrow's C++ reader/writer, not Python-level object
    traversal), not just a smaller payload. Result: barca flipped from
    *losing* to dagster to **winning 1.53-1.57x faster**, confirmed across two
    isolated timing runs (1.55x, 1.53x) plus a `BARCA_BENCH_MEMORY=1` pass
    (1.57x, peak memory 258.2 MB vs dagster's 174.4 MB — both frameworks'
    memory dropped from the dict-of-rows version, since DataFrames are more
    memory-efficient than nested dicts in both). All three frameworks produce
    identical output values on this workload (exact match, not just within
    tolerance).
  - Net: the original `etl_duckdb` loss traced to a fixable barca-side gap
    (missing format optimization for large/tabular payloads), not an inherent
    architectural ceiling — the multi-process, cross-process-serialization
    design this benchmark stresses is real, but the *format* used for that
    serialization was the actual lever, and the DataFrame/parquet path is the
    strictly better one when the workload is tabular.
- Prefect is unaffected by any of the above — it loses every benchmark by a wide
  margin regardless of environment or re-run, consistent with a much higher fixed
  per-run overhead swamping everything else (confirmed by its own peak memory,
  6-8x barca's baseline, and `(user+sys)/wall` also ≈1.0 — it's not parallelizing
  in script mode either).
- **Takeaway**: don't trust any single hyperfine table from a shared/virtualized
  host at face value — re-run rows that look surprising in isolation before
  drawing conclusions, and check whether the CPU-time ratio and (if available)
  peak memory corroborate the wall-clock story. A re-run of this whole suite on
  dedicated hardware would settle both the noise question and give a cleaner
  baseline for the `etl_duckdb`/`spaceflights` cross-process-serialization cost.
- Fixed three pre-existing bugs uncovered while running this, unrelated to the
  CPU/RAM standardization work itself:
  - `parallel_tasks/{barca,dagster,prefect}/run.sh` called bare `barca`/`python`
    instead of the pinned venv binaries (worked only if a venv happened to
    already be active on `PATH`).
  - `resilience_pileup/barca/run.sh` was missing `--no-cache`, so every run after
    the first was a cache-hit no-op (measured: 2.5s cold vs 8ms warm) — the
    historical "330ms" figure above is suspect for the same reason.
    `resilience_pileup/prefect/run.py` had a variable name collision (`t0` was
    both a `@task` and the `__main__` timer, so every run raised
    `TypeError: 'float' object is not callable`) — always broken, not a
    regression from this change.
  - A real correctness bug in `barca-core`: dynamic partitions (`partitions_from`)
    combined with explicit `inputs={}` wiring to a downstream partitioned
    consumer could silently drop the dependency edge (root cause: partition-key
    chunks get bin-packed across dispatch streams by load alone, with no
    dependency awareness, so a consumer's chunk could be visited before its
    producer's). This broke `partitioned_etl` — confirmed pre-existing via
    bisect, not a regression from the adaptive-executor merge. Fixed in
    `coordinator.rs` (two-pass dependency resolution) and covered by a new Rust
    unit test plus `tests/integration/test_partitions.sh`, both wired into CI
    along with `tests/integration/test_benchmark_examples.sh` (runs every
    benchmark's barca side as a correctness smoke test on every PR).

## Scheduler overhead (daemon mode)

Unlike every table above (one-shot cold start), `scheduler_overhead` compares the
long-running **cron scheduler daemons**: `barca serve` vs `dagster dev` vs
`prefect .serve()`. See [`scheduler_overhead/README.md`](scheduler_overhead/README.md)
for the harness and fairness rationale.

Ran 2026-07-20 via `benchmarks/docker/bench.sh scheduler_overhead`: Docker
Desktop's Linux VM on an Apple Silicon Mac (M4 Max host), container `arm64`,
enforced `--cpuset-cpus=0-3 --memory=4g` — the same environment class as the
2026-07-17 full-suite pass above. Defaults: 3 fire-boundaries sampled per
framework (the observation window spans up to 5 minute-boundaries including
boot slack, hence 4–5 recorded fires), 60s idle window, 20s cadence window.
Unlike the 2026-07-17 pass, **peak memory worked this time**: the harness now
runs the container `--privileged` and delegates the cgroup v2 memory
controller at boot (see `benchmarks/docker/bench.sh` / `entrypoint.sh`) —
without that, Docker mounts `/sys/fs/cgroup` read-only and the measurement
falls back to "unavailable", which is exactly what the 2026-07-17 pass hit.
Caveat: the host was shared with unrelated running containers during this
pass; that adds CPU noise, but the effects measured here (seconds-scale poll
floors, hundreds-of-MB footprint gaps) are far above that noise floor.

Three dimensions:

1. **Trigger latency** — measured at a **1-minute cron** (`* * * * *`), the finest
   cadence all three share, so it is apples-to-apples. Expectation from the
   architecture: barca's ≤1s tick loop vs Dagster's ~30s daemon-eval interval and
   Prefect's ~10–15s runner poll (each floors how soon a fire can follow the tick).
2. **Idle footprint** — peak memory of the daemon holding 10 idle jobs. This is
   the closest head-to-head axis: barca is a single Rust process (workers spawned
   only on fire), Dagster runs a webserver + code server + schedule daemon, and
   Prefect's `.serve()` runs its own ephemeral API in-process.
3. **Minimum cadence** — **barca schedules sub-minute (6-field cron, 1-second
   resolution); Dagster and Prefect cannot express any cron finer than 1 minute.**
   Barca fires ~10× in a 20s window where the other two fire 0–1×.

| Dimension | barca | dagster | prefect |
|---|---|---|---|
| Trigger latency (median, 1-min cron) | **0.062s** | 4.929s | 3.698s |
| Idle footprint (peak MB, 10 jobs) | **7.9 MB** | 473.7 MB | 315.7 MB |
| Minimum cadence | **1 second** | 60 seconds | 60 seconds |

Full trigger-latency distribution (seconds past the minute boundary):

| framework | fires | min | median | p95 | max |
|---|---:|---:|---:|---:|---:|
| barca serve | 5 | 0.061s | **0.062s** | 0.218s | 0.218s |
| dagster dev | 4 | 1.782s | 4.929s | 5.882s | 5.882s |
| prefect serve | 5 | 1.402s | 3.698s | 5.52s | 5.52s |

Reading the results:

- **Latency**: barca's per-second tick loop fires ~60ms after the minute
  boundary, with worst case ~220ms. Dagster and Prefect land 1.4–5.9s late —
  better than their documented worst-case poll intervals (~30s daemon eval /
  ~10–15s runner poll) would allow in the worst case, but 20–80× barca's
  median. Dagster also recorded one fewer fire in the same window (4 vs 5):
  its first minute-boundary passed during daemon boot before the schedule was
  being evaluated.
- **Idle footprint**: barca's daemon holding 10 registered schedules is a
  single idle Rust process — **7.9 MB** peak over boot + 60s idle (workers
  only spawn when something fires; these jobs never fire). Dagster boots a
  webserver + code server + schedule daemon (473.7 MB), Prefect an in-process
  ephemeral API (315.7 MB) — a ~40–60× gap, and the closest thing this
  benchmark has to a pure head-to-head axis since nothing is executing.
- **Cadence**: barca's `*/2 * * * * *` fired exactly 10× in the 20s window, as
  scheduled. This axis is a capability difference, not a race — Dagster and
  Prefect cannot express any cron finer than 1 minute (see the fairness notes
  in the README).

## Methodology

### Environment
- **Hardware**: Apple Silicon (M-series), same machine for all benchmarks
- **Barca**: Python 3.14.3 (workspace .venv), Rust release binary
- **Dagster**: Python 3.12.0 (latest compatible), dagster latest
- **Prefect**: Python 3.12.0, prefect latest
- **Airflow**: Python 3.12.0, apache-airflow latest (3.2.2)
- **Python version note**: dagster, prefect, and airflow do not yet support Python 3.14

### Architecture (v0.2.0)

Barca uses a **stateless worker pool** with **Unix domain sockets** for coordination:
- Rust owns a global ready queue and assigns one task at a time to idle workers
- Workers receive tasks, execute, report back — no pre-assigned queues
- `parallel()` uses **SIGSTOP/SIGCONT** to freeze the requesting worker, spawn a temp replacement, and dispatch children across the pool
- Nested `parallel()` works recursively (frozen processes stack, active pool stays at `pool_size`)

### Execution modes tested

| Framework | Mode | Parallelism |
|---|---|---|
| **Barca** | multi-process | Rust spawns N Python workers via UDS (pool_size = cpu_count) |
| **Dagster** (script) | `materialize()` | Sequential in-process (only mode available in script mode) |
| **Dagster** (server) | `dagster dev` + GraphQL | multiprocess_executor spawns subprocess per asset |
| **Prefect** (sequential) | direct task calls | Sequential (default) |
| **Prefect** (parallel) | `ConcurrentTaskRunner` + `.submit()` | Thread-based concurrency (max_workers=16) |
| **Airflow** (dags test) | `dags test` | Sequential in-process (testing mode) |
| **Airflow** (LocalExecutor) | scheduler + trigger | Parallel subprocesses (requires PostgreSQL) |

### Measurement
- hyperfine with --warmup 2-3 for sub-100ms benchmarks (5-10 runs)
- hyperfine with --warmup 1 for longer benchmarks (3-5 runs)
- All times are wall-clock cold start (process spawn to exit)

## Summary: script mode (cold start)

All frameworks invoked as scripts — no pre-started servers.

| # | Benchmark | barca | dagster | prefect | airflow |
|---|---|---:|---:|---:|---:|
| 1 | Trivial (1 asset) | **38ms** | 500ms | 3.6s | 3.5s |
| 2 | Chain 100 (linear) | **72ms** | 999ms | 3.7s | 83s |
| 3 | Fan-out 500 (no work) | **439ms** | 2.1s | 4.3s | — |
| 4 | Fan-out 500×50ms | **2.2s** | 31.5s | 31.2s | 461s |
| 5 | Spaceflights (sklearn) | **720ms** | 1.1s | 3.8s | — |
| 6 | Deep Diamond (18 assets) | **159ms** | 678ms | 4.0s | 17s |
| 7 | Wide Layers (63 assets) | **1.2s** | 919ms | 3.9s | — |
| 8 | Mixed I/O+CPU | **334ms** | 1.1s | 4.0s | — |
| 9 | Large Payloads (10k rows) | **210ms** | 631ms | 6.0s | — |
| 10 | Map/Reduce (1→50→1) | **443ms** | 917ms | 4.1s | — |
| 11 | Multi-file (50 files) | **60ms** | 911ms | 4.0s | — |
| 12 | ETL Pipeline (100k rows) | **500ms** | 953ms | 14.2s | — |
| 13 | Wide Join (10→1) | **267ms** | 635ms | 4.1s | — |
| 14 | Backfill (10-step × 10) | **282ms** | 6.2s | 40.1s | — |
| 15 | Partitioned Chain (3×50) | **1.2s** | — | — | — |
| 16 | Resilience Pileup | **330ms** | — | — | — |
| 17 | parallel(1000) tasks | **1.7s** | — | — | — |
| 18 | Nested parallel (3×5) | **650ms** | — | — | — |

*Rows 5-14 dagster/prefect numbers from 2026-06-03 run; barca numbers updated 2026-06-10.*

## Internal performance profile

Profiler thresholds (all pass):

| Check | Measured | Threshold |
|---|---:|---:|
| Trivial (total) | 31ms | <100ms |
| Plan 100 nodes | 5.6ms | <50ms |
| Plan 2002 nodes | 21ms | <500ms |
| Per-step overhead | 0.40ms | <1.0ms |

### Timing breakdown (trivial, 1 asset)

| Phase | Time |
|---|---:|
| Rust parse + plan | 4ms |
| Python process spawn | ~18ms |
| UDS connect + execute + serialize | ~5ms |
| DB init + write | ~5ms |
| **Total** | **32ms** |

Python process startup is the dominant fixed cost. For workloads with >10 steps, it amortizes to <0.6ms/step.

## Parallel mode comparison (fan_out_500_50ms)

500 independent tasks, each sleeping 50ms. Sequential minimum: 25.0s.

| Framework | Mode | Time | Speedup vs sequential |
|---|---|---:|---:|
| **Barca** | 16 worker processes (UDS) | **2.2s** | 11.4x |
| **Prefect** | ConcurrentTaskRunner (16 workers) | **4.2s** | 6.0x |
| Dagster | script mode (sequential) | 31.5s | 1.0x |
| Prefect | script mode (sequential) | 31.2s | 1.0x |
| Dagster | server + multiprocess_executor | 68.5s | 0.5x (slower than sequential!) |

### Dynamic parallel dispatch (v0.2.0)

`parallel()` at runtime — dispatches tasks dynamically, not known at plan time.

| N items | Time | Notes |
|---:|---:|---|
| 10 | 300ms | includes process spawn overhead |
| 200 | 530ms | previously hung in v0.1.x |
| 1000 | 1.7s | 1000 unique results, SIGSTOP/SIGCONT |
| Nested 3×5 | 650ms | 3 outer × 5 inner = 15 leaf tasks |

## Reproducing

```bash
cargo build --release && maturin develop --release

# Script-mode benchmarks:
for bench in trivial chain_100 fan_out_500 fan_out_500_50ms spaceflights deep_diamond \
             wide_layers mixed_io_cpu large_payloads map_reduce multi_file_discovery \
             etl_duckdb etl_duckdb_dataframes wide_join incremental_backfill \
             partitioned_chain partitioned_etl partitioned_fan_in resilience_pileup \
             parallel_tasks; do
  echo "=== $bench ==="
  hyperfine --warmup 1 --runs 3 benchmarks/$bench/*/run.sh
done

# Performance profiler:
python benchmarks/perf/profile.py

# Parallel tasks:
barca run fan_out_1000 /tmp/parallel_1000.py --agent
```
