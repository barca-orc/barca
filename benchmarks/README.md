# Benchmarks

Compares Barca, Dagster, and Prefect on orchestration overhead and parallel throughput.

All measurements use [hyperfine](https://github.com/sharkdp/hyperfine). See [RESULTS.md](RESULTS.md) for the latest numbers.

## Methodology

### What's controlled
- **Same machine**: all benchmarks run on the same hardware
- **Same workload**: each benchmark implements identical logic across all three frameworks
- **Process-level cold start**: every timed run execs a fresh process (no warm interpreter,
  no in-memory caches carried between samples). Note this is *not* a full state wipe: each
  framework's own run-history/metadata store (barca's `.barca/metadata.db`, Dagster's and
  Prefect's SQLite run storage under `DAGSTER_HOME`/`PREFECT_HOME`) persists and grows
  across hyperfine's repeated samples within one `bench.sh` invocation. This is symmetric
  across all three frameworks (none of them get a full wipe between samples either), so it
  isn't a between-framework fairness issue, but it does mean a single `bench.sh` invocation
  isn't a set of fully independent cold-start measurements.
- **Parallel where applicable**: benchmarks whose steps have genuinely independent branches
  (no dependency on each other, so they could run concurrently) enable real concurrency in
  barca and Prefect, both capped to the same worker count (see "Worker count" below).
  Dagster is a deliberate exception — see the dedicated note below.
- **CPU affinity**: every measured process is pinned to the same fixed set of cores via
  `taskset` (see `benchmarks/lib/env.sh`), so the OS scheduler can't migrate work mid-run
  and load on unrelated cores doesn't leak into the measurement
- **Worker count**: Barca's `pool_size` and Prefect's `max_workers` are set to the *same*
  value for a given run on every benchmark that has real parallel branches to exploit, via
  `BARCA_BENCH_WORKERS`/`BARCA_POOL_SIZE` (see `benchmarks/lib/env.sh`) — neither is over- or
  under-subscribed relative to the other. Dagster does not participate in this — see below.
- **RAM**: total system RAM and CPU model are captured and printed in every `bench.sh`
  run's banner for transparency; no benchmark here is memory-bound enough to warrant
  an artificial memory ceiling
- **Peak memory (opt-in)**: `BARCA_BENCH_MEMORY=1 benchmarks/<name>/bench.sh` adds an
  extra untimed pass per framework that reports peak memory for the *whole process
  tree* (parent + every child it spawns), via a fresh cgroup per run — falls back to
  `/usr/bin/time -v` (single-process only, clearly labeled) if no cgroup memory
  controller is writable. This matters because barca is multi-process (Rust
  coordinator + N Python workers): a naive single-process wrapper would only see the
  coordinator and make barca look artificially light next to Dagster/Prefect's single
  process. Off by default since it re-runs each framework once more and adds
  wall-clock, especially for slow-starting frameworks.

### Reproducible fairness via Docker (recommended)

The native path above (`benchmarks/<name>/bench.sh`, `benchmarks/lib/env.sh`) pins CPU
affinity via `taskset` but only *reports* RAM — there's no enforced memory ceiling, and
`taskset` itself isn't available everywhere (e.g. macOS has no `taskset`, and no writable
cgroup memory controller outside a Linux container). That's fine for fast local iteration,
but it means the CPU/memory budget barca, Dagster, and Prefect actually run under can
silently vary by host.

`benchmarks/docker/bench.sh` is the standard, documented way to get a genuinely
**enforced**, reproducible CPU + memory ceiling shared identically across all three
frameworks, on any Docker-capable machine (including Apple Silicon Macs, via Docker
Desktop's Linux VM):

```bash
# Build the image and run the full 20-benchmark suite
benchmarks/docker/bench.sh

# Or a subset, for quick iteration
benchmarks/docker/bench.sh trivial chain_100

# Override the shared resource ceiling (defaults: 4 cores, 4g memory)
BARCA_BENCH_CORES=8 BARCA_BENCH_MEM=8g benchmarks/docker/bench.sh
```

What it does differently from the native path:
- **Enforced CPU ceiling**: `docker run --cpuset-cpus` hard-restricts which host cores the
  whole container (barca's coordinator + worker pool, Dagster, and Prefect alike) can ever
  schedule onto — not just an advisory pin inside the process, but a limit the container
  cannot exceed regardless of host load. `BARCA_BENCH_CORES` reuses the same env var name
  and convention as `benchmarks/lib/env.sh` (default 4), translated into a
  `--cpuset-cpus=0-N-1` range.
- **Enforced memory ceiling**: `docker run --memory` gives all three frameworks a real,
  identical, kernel-enforced RAM budget (default `4g`, generous relative to the largest
  peak-memory figure ever recorded in `RESULTS.md` — ~560MB for Prefect on
  `spaceflights` — but a genuine ceiling rather than "no limit"), overridable via
  `BARCA_BENCH_MEM`.
- **Fully offline, reproducible timed runs**: `benchmarks/docker/Dockerfile` builds the
  barca wheel from source, then bakes every benchmark's Dagster/Prefect `uv sync` venv at
  *image build time* — so the timed portion of a run never touches the network, and one
  image can be reused across repeated runs without first-run cold-cache variance.
  `benchmarks/docker/entrypoint.sh` (baked into the image) then loops over all 20
  benchmarks (or a subset passed as args), running each with
  `BARCA_BENCH_MEMORY=1 ./bench.sh` and collecting logs + `results.md` under
  `benchmarks/docker/out/` on the host (gitignored — raw output, not committed).

The native path stays documented and unchanged for fast local iteration without a Docker
image build; the Docker path is what to reach for when the numbers need to be defensible
across machines (e.g. for a `RESULTS.md` update).

### Why Dagster is excluded from the worker-count fix

Dagster's `materialize()`/`job.execute_in_process()` — what every `dagster/run.py` here
calls — **hard-ignores `executor_def`** regardless of configuration; it's documented
behavior ("The executor_def on the Job will be ignored, and replaced with the in-process
executor") and confirmed empirically against the installed package. Dagster's *only* way to
get real multiprocess parallelism is the out-of-process `execute_job(reconstructable(...),
instance=...)` API, which spawns a brand-new OS subprocess **per step** — not a reused pool.
Measured directly: 12 zero-work steps at `max_concurrent=4` took 8.5s wall time, i.e. roughly
0.5–1.5s of pure interpreter-boot/import overhead per step. Every task in this benchmark
suite does ≤50ms of real work, so switching any benchmark to Dagster's real multiprocess
executor would make Dagster slower, not more comparable — trading the original bug (Dagster
never parallelizing genuinely parallel work) for a different distortion (Dagster measured on
subprocess-spawn cost that swamps the actual task). Given the current suite's task sizes,
there is no benchmark where the out-of-process executor would be a net win, so `dagster/run.py`
is deliberately left on the default in-process (sequential) executor everywhere, and Dagster's
numbers on parallel-shaped benchmarks should be read as "Dagster's realistic default local-run
behavior for this workload size," not as "Dagster's best possible parallel throughput." If a
future benchmark's individual steps are heavy enough (roughly >500ms each, per the measured
spawn cost above) to amortize that per-step subprocess cost, `execute_job`/`reconstructable`
is the right mechanism to revisit this — see `dagster_server/`/`prefect_server` in a few
benchmark directories for a partially-built alternative (a warm `dagster dev` daemon + GraphQL
`launchRun`, which amortizes the spawn cost differently). Those aren't wired into `bench.sh`
today.

### What differs
- **Python version**: Barca uses Python 3.14 (from workspace .venv). Dagster and Prefect use Python 3.12 (latest compatible with both)
- **Parallelism model**: Barca uses multi-process (Rust spawns N persistent Python workers,
  reused across tasks). Prefect uses `ConcurrentTaskRunner`, capped to the same worker count
  (see above). Dagster uses its default in-process executor (sequential) everywhere in this
  suite — see "Why Dagster is excluded" above for why its multiprocess executor isn't a fair
  comparison point here.
- **DB persistence**: Barca writes to Turso/libSQL. Dagster and Prefect use their own internal SQLite stores

### Parallelism configuration

`benchmarks/lib/env.sh` exports `BARCA_POOL_SIZE` and `BARCA_BENCH_WORKERS` (both
default to `BARCA_BENCH_CORES`, default `4`) for every framework to read:

- **Barca**: `pool_size` — set via the `BARCA_POOL_SIZE` env var (falls back to `cpu_count` if unset)
- **Prefect**: `ConcurrentTaskRunner(max_workers=BARCA_BENCH_WORKERS)` — wired on every
  `prefect/run.py` whose workload has independent branches to parallelize (falls back to 16 if
  unset, matching Barca's own cpu_count fallback and Dagster's historical default). Benchmarks
  that are a strict single chain (no step ever has an independent sibling ready at the same
  time) are intentionally left on Prefect's plain sequential calls, since there's nothing to
  parallelize either way.
- **Dagster**: not wired to `BARCA_BENCH_WORKERS` — see "Why Dagster is excluded" above.

In practice, only `deep_diamond/prefect/run.py` and `mixed_io_cpu/prefect/run.py`
(plus the `*_server` variants, which aren't invoked by `bench.sh`) actually read
`BARCA_BENCH_WORKERS` today — the rest of the script-mode Dagster/Prefect files
call `materialize()` / `execute_in_process()` / `@flow` with framework defaults.
For those, cross-framework worker parity comes from the shared `taskset` CPU pin
alone (same core budget), not a matched worker count. Override the pin with
`BARCA_BENCH_CORES=8 benchmarks/trivial/bench.sh`.

For the `*_server` benchmarks (persistent `dagster dev` / long-running Prefect
processes), source `benchmarks/lib/env.sh` in the shell that runs `start.sh` — the
worker count is read once, at server startup, from the environment.

## Benchmark suite

### Overhead & scaling

| Benchmark | Assets | Topology | Parallelism |
|---|---|---|---|
| `trivial` | 1 | single node | N/A |
| `chain_100` | 100 | linear chain | None (sequential) |
| `fan_out_500` | 500 | independent | All frameworks parallel |
| `fan_out_500_50ms` | 500 | independent + 50ms I/O | All frameworks parallel |
| `incremental_backfill` | 10 × 10 | linear chain × 10 runs | None (sequential) |
| `multi_file_discovery` | 98 | independent (50 files) | All frameworks parallel |

### DAG topologies

| Benchmark | Assets | Topology | Parallelism |
|---|---|---|---|
| `deep_diamond` | 18 | 5-wide → merge → chain | Parallel fan-out |
| `wide_layers` | 63 | 3 × 20 + aggregation | Parallel within layers |
| `map_reduce` | 52 | 1 → 50 → 1 | Parallel mappers |
| `wide_join` | 11 | 10 dims → 1 fact | Parallel dims |

### Real workloads

| Benchmark | Assets | Topology | Parallelism |
|---|---|---|---|
| `spaceflights` | 10 | diamond + sklearn | Parallel sources |
| `mixed_io_cpu` | 8 | 5 API calls → merge → compute | Parallel API calls |
| `etl_duckdb` | 12 | raw → staging → marts | Parallel sources |
| `etl_duckdb_dataframes` | 12 | raw → staging → marts (DataFrame/parquet payloads) | Parallel sources |
| `large_payloads` | 5 | linear chain, 10k rows/step | None (sequential) |

### Partitioned workloads

| Benchmark | Steps | Topology | Parallelism |
|---|---|---|---|
| `partitioned_chain` | 150 | 3 assets × 50 partitions | Parallel partitions |
| `partitioned_etl` | — | ETL with partitioning | Parallel partitions |
| `partitioned_fan_in` | 100 | 50 partitions × 2 assets, 1:1 partition-aligned chain | Parallel partitions |
| `collect_fan_in` | 51 | 50 partitions → 1 (`collect()`, many-to-one gather) | Parallel partitions |
| `partitioned_10k` | ~10k | Docker-based cross-framework | Parallel partitions |

### Dynamic dispatch & resilience (v0.2.0)

| Benchmark | Steps | What it tests |
|---|---|---|
| `parallel_tasks` | N (param) | `parallel()` runtime dispatch — SIGSTOP/SIGCONT, temp workers |
| `resilience_pileup` | 18 | Failure/retry behavior — one flaky chain shouldn't stall healthy work |

### Artifact hash check

Not a `hyperfine` run: `python benchmarks/transfer_hash/bench.py [MB] [--max-seconds-per-gb N]`
times the SHA-256 a remote-backed run takes of each local artifact it reads (about 0.5 s/GB with a
warm page cache), so a regression in `python/barca/_transfer.py` shows up as a number.

### Scheduler / daemon

The odd one out: not a `hyperfine` cold-start measurement but a comparison of the
long-running **cron scheduler daemons** (`barca serve` vs `dagster dev` vs
`prefect .serve()`). See [`scheduler_overhead/README.md`](scheduler_overhead/README.md).

| Benchmark | What it tests |
|---|---|
| `scheduler_overhead` | Trigger latency (fair, pinned to 1-minute cron), idle-daemon memory footprint, and minimum achievable cadence — **barca's 1-second / 6-field sub-minute cron vs Dagster's & Prefect's 1-minute floor** |

**Methodology note.** Latency is measured with a 1-minute cron (`* * * * *`) —
the finest cadence all three frameworks share — so it is apples-to-apples;
Dagster (daemon eval ~30s) and Prefect (runner poll ~10–15s) land up to one poll
interval after the tick regardless of workload. Barca's ability to schedule
*sub-minute* (6-field cron, 1-second resolution), which the other two cannot
express at all, is reported on its own "minimum cadence" axis rather than folded
into the latency comparison. This benchmark is **slow** — it waits on real minute
boundaries (~`(FIRES+1)` min per framework) — and starts real daemons rather than
one-shot processes.

## Running benchmarks

```bash
# Prerequisites: Rust toolchain, uv, hyperfine, taskset (util-linux)
cargo build --release && maturin develop --release

# Run individual benchmark (sets up venvs on first run):
benchmarks/trivial/bench.sh 10

# Override the pinned core count / worker count (default: 4 cores):
BARCA_BENCH_CORES=8 benchmarks/trivial/bench.sh 10

# Or run manually with hyperfine, pinned and worker-matched the same way bench.sh does:
source benchmarks/lib/env.sh
hyperfine --warmup 3 --runs 10 \
  "$(bench_pin benchmarks/trivial/barca/run.sh)" \
  "$(bench_pin benchmarks/trivial/dagster/run.sh)" \
  "$(bench_pin benchmarks/trivial/prefect/run.sh)"
```

Each benchmark directory contains:
- `barca/` — barca implementation + `run.sh`
- `dagster/` — equivalent Dagster implementation + `run.sh`
- `prefect/` — equivalent Prefect implementation + `run.sh`
