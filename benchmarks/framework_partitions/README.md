# Partition execution across frameworks

A local invocation polls a JSON map of versions, changes one key, and runs that key through
`cleaned -> scored`. Both functions return small dictionaries; `scored` doubles the input
version. Every measured update must run exactly those two functions plus one poll and return
the new score. An unchanged invocation must run only the poll. There is no simulated sleep.

This is a **fresh-process, warm-data** benchmark, not a measurement of warmed daemons,
distributed scheduling, throughput, or feature equivalence. Prefect's ephemeral local API
starts and stops with each process; its startup and shutdown are included. Dagster runs
assets with `materialize()` in process. Barca runs the ordinary cache-aware `get` command.
Do not extrapolate the speed ratios to a persistent Prefect/Dagster deployment.

## Deliberately favorable routing for competitors

Barca receives the full source snapshot and automatically determines which asset partitions
need refreshing. Its entire keyed DAG is seeded and validated before measurement, including
artifacts and metadata for every key. The seed and proof pass are excluded from results.

Dagster and Prefect use the adapter in `worker.py`: it compares the current map with the
previous successful snapshot and explicitly routes **only changed keys** to the framework.
Dagster uses two statically partitioned assets and a persistent local instance/IO manager;
Prefect uses a flow, persisted JSON task results, and `INPUTS + TASK_SOURCE` caching for the
asset tasks. The poll itself is not cached. This gives competitors change selection in
application code rather than asking them to inspect every cached task or run. Their durable
history contains only the affected key, not materializations for the entire domain. Barca
therefore carries more historical state. This is a dispatch comparison with known change
routing, not evidence that all three provide identical automatic freshness behavior.

The workload changes an existing key's integer version. It does not exercise additions,
removals, failures, concurrent runs, or changed code. These are correctness boundaries of
Barca's own tests, not claims this cross-framework harness checks.

## Measurement

- Python 3.13 for all three; exact versions and installed dependency sets are recorded in
  `environment.json`. The Barca binary is built from the recorded source revision.
- Four identical CPU cores via `taskset`, inherited by all descendants. Frameworks run
  sequentially, without simultaneous builds, installs, or tests.
- Each framework/domain gets separate persistent local history and snapshot files. One
  update warms its local state before three measured updates. One unchanged check follows.
  Python imports are **not** amortized across invocations; OS/filesystem caches are warm.
- Wall time is process launch through exit, including imports, polling/comparison, durable
  writes, and cleanup. Dependency installation, data generation, Barca's full seed/proof,
  and the harness's result validation are excluded.
- Memory is the maximum **sum of live RSS across the entire process tree**, sampled every
  5 ms. It includes Barca's Python worker and Prefect's API process. This is not the earlier
  native-only Barca memory figure. Shared pages are counted in each process, and a sampler
  can miss short-lived peaks; these are sampled aggregate RSS values, not physical-memory
  accounting or PSS. Do not compare them directly with `/usr/bin/time` single-process RSS.
- The harness verifies changed outputs and actual function counts. Raw records include
  warm-up/proof cases; use only `case=one_key` for update statistics. Report median wall
  time and maximum sampled tree RSS across the three samples.
- 1k and 50k are the main cases. The 200k domain is a stress case: Dagster
  [recommends at most 100,000 partitions per asset](https://docs.dagster.io/guides/build/partitions-and-backfills/partitioning-assets)
  because larger counts may slow UI loading. This benchmark does not load a UI, but its
  200k row must still be labeled as above that recommendation.

API references: [Dagster partitioned assets](https://docs.dagster.io/guides/build/partitions-and-backfills/partitioning-assets),
[Prefect cache policies and result persistence](https://docs.prefect.io/v3/concepts/caching).

## Reproduce on Linux

Inside a repository worktree, build with a private target directory:

```bash
uv venv --python 3.13 .venv
uv pip install --python .venv/bin/python maturin
CARGO_TARGET_DIR="$PWD/target-local" .venv/bin/maturin build --release --locked \
  --compatibility linux --out target-local/wheels
uv pip install --python .venv/bin/python target-local/wheels/*.whl
uv venv --python 3.13 target-local/dagster-env
uv venv --python 3.13 target-local/prefect-env
uv pip install --python target-local/dagster-env/bin/python dagster==1.13.26
uv pip install --python target-local/prefect-env/bin/python prefect==3.9.0

BARCA_BIN="$PWD/target-local/release/barca" \
BARCA_PYTHON="$PWD/.venv/bin/python" \
DAGSTER_PYTHON="$PWD/target-local/dagster-env/bin/python" \
PREFECT_PYTHON="$PWD/target-local/prefect-env/bin/python" \
PROJECTS_DIR="$PWD/target-local/framework-fixtures" \
KEYS=1000,50000,200000 REPEATS=3 \
.venv/bin/python benchmarks/framework_partitions/bench.py > measurements.jsonl
```

Use a fresh `PROJECTS_DIR` for each full comparison. Retained logs and per-framework state
live there. The native release build used here has no bundled UI; no measured route serves
UI assets. No framework contacts a remote control plane or artifact store.

## Cold comparisons

`cold.py` uses a new directory and empty framework history for **every sample**; there is
no framework warm-up or seeded asset cache. Input JSON files are fixture data, not asset
results. Dependency installation and imports' OS page cache are not cleared between samples.

Two workloads distinguish startup from first materialization:

- Default: request the `r0` chain from a 1/1k/50k/200k-key domain. Barca uses explicit
  `--partition retailer=r0`; Dagster/Prefect use the same known-change adapter as above.
  The adapter receives two version snapshots as input. All execute one poll and two asset
  functions, with new durable results and a newly initialized orchestration database.
- `COLD_MODE=full`: materialize **every** key from empty state, at 1/100/1k keys. All execute
  one poll plus two functions per key and persist individual per-key outputs. Barca uses
  `batch=none`, so it does not get a vectorized-call advantage. Dagster uses dynamic mapped
  ops in one in-process job (not one run per partition); only routing values use its memory
  IO manager, while the source and both mapped stages use its filesystem IO manager.
  Prefect uses task mapping with four threads and persisted JSON derived results; the
  successful source snapshot is persisted by the adapter checkpoint after the flow. Dagster's default
  per-step filesystem format is pickle; Barca and Prefect use JSON. Both derived outputs
  remain separate durable objects per key. These are analogous durable work units, not
  identical asset catalogs or resume/retry contracts. The harness validates all scores and
  `1 + 2N` domain function calls. Full-materialization results have one sample per case.

Use the same environment variables as the warm command, replacing its final command with:

```bash
# Three independent cold selected-chain samples per framework/domain:
KEYS=1,1000,50000,200000 REPEATS=3 .venv/bin/python benchmarks/framework_partitions/cold.py
# One full first materialization, with per-key functions/artifacts:
COLD_MODE=full KEYS=1,100,1000 REPEATS=1 .venv/bin/python benchmarks/framework_partitions/cold.py
# Native cold-selection regression budget, excluding Python memory:
FRAMEWORKS=barca KEYS=200000 REPEATS=1 NATIVE_MAX_KIB=19531 \
.venv/bin/python benchmarks/framework_partitions/cold.py
```

Each command needs a **different fresh `PROJECTS_DIR`**. The native budget checks the
coordinator's self-reported high-water mark and is distinct from aggregate RSS in the table.
`cold-probe.jsonl` records the initial probe before the cold-selection fix; final results
use the source revision and dependency sets in `cold-environment.json`.
