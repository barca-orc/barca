"""Dagster: mixed I/O + CPU — 5 parallel API calls, merge, heavy compute, summarize."""

import hashlib
import json
import time

from dagster import AssetIn, asset, materialize


# ── I/O-bound steps (simulated API calls, 50ms each) ──


@asset
def api_call_0():
    time.sleep(0.05)
    return {"source": "api_0", "records": list(range(200))}


@asset
def api_call_1():
    time.sleep(0.05)
    return {"source": "api_1", "records": list(range(200))}


@asset
def api_call_2():
    time.sleep(0.05)
    return {"source": "api_2", "records": list(range(200))}


@asset
def api_call_3():
    time.sleep(0.05)
    return {"source": "api_3", "records": list(range(200))}


@asset
def api_call_4():
    time.sleep(0.05)
    return {"source": "api_4", "records": list(range(200))}


# ── Fan-in merge ──


@asset(
    ins={
        "a0": AssetIn(key="api_call_0"),
        "a1": AssetIn(key="api_call_1"),
        "a2": AssetIn(key="api_call_2"),
        "a3": AssetIn(key="api_call_3"),
        "a4": AssetIn(key="api_call_4"),
    }
)
def combine(a0, a1, a2, a3, a4):
    all_records = a0["records"] + a1["records"] + a2["records"] + a3["records"] + a4["records"]
    return {"records": all_records, "count": len(all_records)}


# ── CPU-bound step (heavy computation) ──


@asset(ins={"data": AssetIn(key="combine")})
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


@asset(ins={"data": AssetIn(key="heavy_compute")})
def summarize(data):
    unique = len(set(data["hashes"]))
    return {"total_hashes": data["count"], "unique": unique}


if __name__ == "__main__":
    all_assets = [
        api_call_0,
        api_call_1,
        api_call_2,
        api_call_3,
        api_call_4,
        combine,
        heavy_compute,
        summarize,
    ]
    t0 = time.perf_counter()
    result = materialize(all_assets)
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 8,
                "success": result.success,
            },
            indent=2,
        )
    )
