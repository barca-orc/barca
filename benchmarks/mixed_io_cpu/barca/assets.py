"""Mixed I/O + CPU: realistic pipeline with I/O-bound and CPU-bound steps.
Tests overlapping I/O waits with CPU computation.

Topology:
  api_call_0 (50ms) ──┐
  api_call_1 (50ms) ──┤
  api_call_2 (50ms) ──├→ combine → heavy_compute → summarize
  api_call_3 (50ms) ──┤
  api_call_4 (50ms) ──┘
"""

import hashlib
import time

from barca import asset


# ── I/O-bound steps (simulated API calls, 50ms each) ──


@asset()
def api_call_0() -> dict:
    time.sleep(0.05)
    return {"source": "api_0", "records": list(range(200))}


@asset()
def api_call_1() -> dict:
    time.sleep(0.05)
    return {"source": "api_1", "records": list(range(200))}


@asset()
def api_call_2() -> dict:
    time.sleep(0.05)
    return {"source": "api_2", "records": list(range(200))}


@asset()
def api_call_3() -> dict:
    time.sleep(0.05)
    return {"source": "api_3", "records": list(range(200))}


@asset()
def api_call_4() -> dict:
    time.sleep(0.05)
    return {"source": "api_4", "records": list(range(200))}


# ── Fan-in merge ──


@asset(
    inputs={
        "a0": api_call_0,
        "a1": api_call_1,
        "a2": api_call_2,
        "a3": api_call_3,
        "a4": api_call_4,
    }
)
def combine(a0: dict, a1: dict, a2: dict, a3: dict, a4: dict) -> dict:
    all_records = a0["records"] + a1["records"] + a2["records"] + a3["records"] + a4["records"]
    return {"records": all_records, "count": len(all_records)}


# ── CPU-bound step (heavy computation) ──


@asset(inputs={"data": combine})
def heavy_compute(data: dict) -> dict:
    """Simulate CPU-heavy work: hash each record multiple times."""
    results = []
    for r in data["records"]:
        h = str(r)
        for _ in range(500):
            h = hashlib.sha256(h.encode()).hexdigest()
        results.append(h[:16])
    return {"hashes": results, "count": len(results)}


# ── Light post-processing ──


@asset(inputs={"data": heavy_compute})
def summarize(data: dict) -> dict:
    unique = len(set(data["hashes"]))
    return {"total_hashes": data["count"], "unique": unique}
