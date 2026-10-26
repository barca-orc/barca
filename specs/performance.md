# Coordinator performance scope

Specification version: 1. Status: measured regression budgets and approved targets, separated below.
Scope: orchestration resource cost, excluding application computation and Python interpreter cost.
[Results and methodology](../benchmarks/RESULTS.md) own recorded measurements; this document
states what a measurement can establish. These are controlled workload budgets, not a
universal latency or memory guarantee on arbitrary hardware.

## Implemented workload budgets

| Workload | Native memory budget | Native CPU budget | Evidence |
| --- | --- | --- | --- |
| Validated warm, unchanged or one changed key, aligned three-node 50k/200k chain | <20,000,000 bytes | <20 ms | `incremental_dag/bench.py` with explicit limits |
| Same chain, 17 dispersed changed keys batched | <20,000,000 bytes | <40 ms | Same harness with `CHANGED_KEYS=17` |
| Small 1k target returning every reference | <20 MiB | <40 ms | Separate reference-listing measurements |
| Cold explicitly selected chain in 200k domain | <20,000,000 bytes in measured samples | No 20 ms claim | Framework partition cold harness and native peak records |

The first two cover ordinary DAG execution after evidence is validated. They do not cover
full bootstrap, proof-format upgrade, explicit refresh, compound partitions, large source
deltas that fall back to full validation, full cache surveys, or local execution against a
remote server ledger. Sensors that enumerate all source keys still pay discovery/comparison
cost. A changed fan-in reads all applicable inputs. Returning all references is separate work.

Reproducer, from an owned worktree with a matching binary and Python helpers:

```bash
KEYS=50000,200000 REPEATS=3 COORDINATOR_MAX_KIB=19531 \
  COORDINATOR_MAX_CPU_MS=20 python benchmarks/incremental_dag/bench.py
```

Set `BARCA_BIN` and `PATH` as the [harness](../benchmarks/incremental_dag/bench.py) requires.
19,531 KiB is a conservative executable threshold below 20,000,000 bytes. A budget is
enforced only when the harness receives its limit variables; the existence of a benchmark
script alone is not a CI gate. CPU measurements include native work, not worker CPU;
whole-process-tree RSS includes Python and must not be presented as native footprint.
Record binary/source identity, Python/dependency versions, machine, worker count, affinity,
warm-up/seed treatment, sample count and concurrent load. Preserve failed samples.

- **INV-PERF-01:** A warm one-key update executes only the sensor and the two affected keys;
  an unchanged run executes the sensor alone. Assert work counts alongside timings.
- **INV-PERF-02:** A no-store ordinary run reuses its pool; partition count does not imply
  process count. Nested `parallel()` and transfer helpers require separate process budgets.

## Known gaps and approved targets

| Gap | Approved direction | Required evidence before claiming implemented |
| --- | --- | --- |
| [#655](https://github.com/barca-orc/barca/issues/655): full bootstrap/validation retains whole-domain state | Bounded admission, completion buffers and persistence; release completed per-key state | Cold and refresh scaling, peak/retained native memory, queue/window sizes, same per-key results and failure recovery |
| [#656](https://github.com/barca-orc/barca/issues/656): expensive cold/post-run inspection | Maintained summaries and explicit checking state | Cold, invalidated and cached requests; unchanged-run invalidation; concurrent responsiveness; long-run retained memory |
| [#654](https://github.com/barca-orc/barca/issues/654): duplicate matching work | Shared in-flight asset ownership | Real overlapping requests, identical/different hashes, partial key overlap, cancellation, failure and restart |

The published 0.33.0 [audit on PR #658](https://github.com/barca-orc/barca/pull/658)
reports full 200k bootstrap at 68 s / 2.75 GiB native and post-run inspection around 2.7 s /
1.22 GiB retained native RSS. These are identified gaps, not acceptable substitutes for the
bounded warm budgets. Exact raw data and later fixed-build limits are in that PR; it is not
merged into this specification's baseline. No Dagster/Prefect comparison or TLA+ model
establishes these resource targets for the implementation.
