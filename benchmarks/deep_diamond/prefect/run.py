"""Prefect: deep diamond — 5 parallel 3-step pipelines, merge, 2-step post-process."""

import hashlib
import json
import os
import random
import time

from prefect import flow, task
from prefect.task_runners import ConcurrentTaskRunner

# Matches barca's pool_size and dagster's max_concurrent for this benchmark run
# (see benchmarks/lib/env.sh) so no framework gets more/fewer workers than another.
BENCH_WORKERS = int(os.environ.get("BARCA_BENCH_WORKERS", "16"))


# ── 5 independent sources ──


@task
def src_0():
    rng = random.Random(0)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@task
def src_1():
    rng = random.Random(1)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@task
def src_2():
    rng = random.Random(2)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@task
def src_3():
    rng = random.Random(3)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@task
def src_4():
    rng = random.Random(4)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


# ── 5 prep steps (filter + normalize) ──


@task
def prep_0(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@task
def prep_1(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@task
def prep_2(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@task
def prep_3(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@task
def prep_4(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


# ── 5 feature engineering steps ──


@task
def feat_0(data):
    return {
        "features": [
            {
                "id": r["id"],
                "f": r["val"] ** 2,
                "h": hashlib.md5(str(r["id"]).encode()).hexdigest()[:8],
            }
            for r in data["rows"]
        ]
    }


@task
def feat_1(data):
    return {
        "features": [
            {
                "id": r["id"],
                "f": r["val"] ** 2,
                "h": hashlib.md5(str(r["id"]).encode()).hexdigest()[:8],
            }
            for r in data["rows"]
        ]
    }


@task
def feat_2(data):
    return {
        "features": [
            {
                "id": r["id"],
                "f": r["val"] ** 2,
                "h": hashlib.md5(str(r["id"]).encode()).hexdigest()[:8],
            }
            for r in data["rows"]
        ]
    }


@task
def feat_3(data):
    return {
        "features": [
            {
                "id": r["id"],
                "f": r["val"] ** 2,
                "h": hashlib.md5(str(r["id"]).encode()).hexdigest()[:8],
            }
            for r in data["rows"]
        ]
    }


@task
def feat_4(data):
    return {
        "features": [
            {
                "id": r["id"],
                "f": r["val"] ** 2,
                "h": hashlib.md5(str(r["id"]).encode()).hexdigest()[:8],
            }
            for r in data["rows"]
        ]
    }


# ── Merge (fan-in) ──


@task
def merge(f0, f1, f2, f3, f4):
    all_features = (
        f0["features"] + f1["features"] + f2["features"] + f3["features"] + f4["features"]
    )
    return {"combined": all_features, "count": len(all_features)}


# ── Post-processing chain ──


@task
def transform(data):
    sorted_data = sorted(data["combined"], key=lambda x: x["f"], reverse=True)
    return {"top_100": sorted_data[:100], "total": data["count"]}


@task
def output(data):
    avg = sum(r["f"] for r in data["top_100"]) / len(data["top_100"]) if data["top_100"] else 0
    return {
        "avg_top_feature": round(avg, 6),
        "total_rows": data["total"],
        "top_count": len(data["top_100"]),
    }


@flow(task_runner=ConcurrentTaskRunner(max_workers=BENCH_WORKERS))
def deep_diamond_flow():
    # Sources (parallel)
    s0 = src_0.submit()
    s1 = src_1.submit()
    s2 = src_2.submit()
    s3 = src_3.submit()
    s4 = src_4.submit()

    # Prep (parallel, each depends on its source)
    p0 = prep_0.submit(s0)
    p1 = prep_1.submit(s1)
    p2 = prep_2.submit(s2)
    p3 = prep_3.submit(s3)
    p4 = prep_4.submit(s4)

    # Features (parallel, each depends on its prep)
    f0 = feat_0.submit(p0)
    f1 = feat_1.submit(p1)
    f2 = feat_2.submit(p2)
    f3 = feat_3.submit(p3)
    f4 = feat_4.submit(p4)

    # Merge (fan-in)
    merged = merge.submit(f0, f1, f2, f3, f4)

    # Post-processing chain
    transformed = transform.submit(merged)
    result = output(transformed)
    return result


if __name__ == "__main__":
    t0 = time.perf_counter()
    result = deep_diamond_flow()
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 18,
                "result": result,
            },
            indent=2,
        )
    )
