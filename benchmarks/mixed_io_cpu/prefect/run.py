"""Prefect: mixed I/O + CPU — 5 parallel API calls, merge, heavy compute, summarize."""

import hashlib
import json
import os
import time

from prefect import flow, task
from prefect.task_runners import ConcurrentTaskRunner

# Matches barca's pool_size and dagster's max_concurrent for this benchmark run
# (see benchmarks/lib/env.sh) so no framework gets more/fewer workers than another.
BENCH_WORKERS = int(os.environ.get("BARCA_BENCH_WORKERS", "16"))


# ── I/O-bound steps (simulated API calls, 50ms each) ──


@task
def api_call_0():
    time.sleep(0.05)
    return {"source": "api_0", "records": list(range(200))}


@task
def api_call_1():
    time.sleep(0.05)
    return {"source": "api_1", "records": list(range(200))}


@task
def api_call_2():
    time.sleep(0.05)
    return {"source": "api_2", "records": list(range(200))}


@task
def api_call_3():
    time.sleep(0.05)
    return {"source": "api_3", "records": list(range(200))}


@task
def api_call_4():
    time.sleep(0.05)
    return {"source": "api_4", "records": list(range(200))}


# ── Fan-in merge ──


@task
def combine(a0, a1, a2, a3, a4):
    all_records = a0["records"] + a1["records"] + a2["records"] + a3["records"] + a4["records"]
    return {"records": all_records, "count": len(all_records)}


# ── CPU-bound step (heavy computation) ──


@task
def heavy_compute(data):
    """Simulate CPU-heavy work: hash each record multiple times."""
    results = []
    for r in data["records"]:
        h = str(r)
        for _ in range(500):
            h = hashlib.sha256(h.encode()).hexdigest()
        results.append(h[:16])
    return {"hashes": results, "count": len(results)}


# ── Light post-processing ──


@task
def summarize(data):
    unique = len(set(data["hashes"]))
    return {"total_hashes": data["count"], "unique": unique}


@flow(task_runner=ConcurrentTaskRunner(max_workers=BENCH_WORKERS))
def mixed_io_cpu_flow():
    # I/O-bound (parallel)
    a0 = api_call_0.submit()
    a1 = api_call_1.submit()
    a2 = api_call_2.submit()
    a3 = api_call_3.submit()
    a4 = api_call_4.submit()

    # Fan-in merge
    combined = combine.submit(a0, a1, a2, a3, a4)

    # CPU-bound
    computed = heavy_compute.submit(combined)

    # Post-processing
    result = summarize(computed)
    return result


if __name__ == "__main__":
    t0 = time.perf_counter()
    result = mixed_io_cpu_flow()
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 8,
                "result": result,
            },
            indent=2,
        )
    )
