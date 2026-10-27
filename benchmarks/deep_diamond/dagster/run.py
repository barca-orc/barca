"""Dagster: deep diamond — 5 parallel 3-step pipelines, merge, 2-step post-process."""

import hashlib
import json
import random
import time

from dagster import AssetIn, asset, materialize


# ── 5 independent sources ──


@asset
def src_0():
    rng = random.Random(0)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset
def src_1():
    rng = random.Random(1)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset
def src_2():
    rng = random.Random(2)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset
def src_3():
    rng = random.Random(3)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset
def src_4():
    rng = random.Random(4)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


# ── 5 prep steps (filter + normalize) ──


@asset(ins={"data": AssetIn(key="src_0")})
def prep_0(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(ins={"data": AssetIn(key="src_1")})
def prep_1(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(ins={"data": AssetIn(key="src_2")})
def prep_2(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(ins={"data": AssetIn(key="src_3")})
def prep_3(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(ins={"data": AssetIn(key="src_4")})
def prep_4(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


# ── 5 feature engineering steps ──


@asset(ins={"data": AssetIn(key="prep_0")})
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


@asset(ins={"data": AssetIn(key="prep_1")})
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


@asset(ins={"data": AssetIn(key="prep_2")})
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


@asset(ins={"data": AssetIn(key="prep_3")})
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


@asset(ins={"data": AssetIn(key="prep_4")})
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


@asset(
    ins={
        "f0": AssetIn(key="feat_0"),
        "f1": AssetIn(key="feat_1"),
        "f2": AssetIn(key="feat_2"),
        "f3": AssetIn(key="feat_3"),
        "f4": AssetIn(key="feat_4"),
    }
)
def merge(f0, f1, f2, f3, f4):
    all_features = (
        f0["features"] + f1["features"] + f2["features"] + f3["features"] + f4["features"]
    )
    return {"combined": all_features, "count": len(all_features)}


# ── Post-processing chain ──


@asset(ins={"data": AssetIn(key="merge")})
def transform(data):
    sorted_data = sorted(data["combined"], key=lambda x: x["f"], reverse=True)
    return {"top_100": sorted_data[:100], "total": data["count"]}


@asset(ins={"data": AssetIn(key="transform")})
def final_output(data):
    avg = sum(r["f"] for r in data["top_100"]) / len(data["top_100"]) if data["top_100"] else 0
    return {
        "avg_top_feature": round(avg, 6),
        "total_rows": data["total"],
        "top_count": len(data["top_100"]),
    }


if __name__ == "__main__":
    all_assets = [
        src_0,
        src_1,
        src_2,
        src_3,
        src_4,
        prep_0,
        prep_1,
        prep_2,
        prep_3,
        prep_4,
        feat_0,
        feat_1,
        feat_2,
        feat_3,
        feat_4,
        merge,
        transform,
        final_output,
    ]
    t0 = time.perf_counter()
    result = materialize(all_assets)
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 18,
                "success": result.success,
            },
            indent=2,
        )
    )
