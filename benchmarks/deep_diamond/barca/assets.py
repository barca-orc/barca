"""Deep diamond: 5 parallel 3-step pipelines → merge → 2-step post-process.
18 assets total, 5-wide parallelism, 6 levels deep.
Each step does light compute (list processing) to make it non-trivial."""

import hashlib
import random

from barca import asset


# ── 5 independent sources ──


@asset()
def src_0() -> dict:
    rng = random.Random(0)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset()
def src_1() -> dict:
    rng = random.Random(1)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset()
def src_2() -> dict:
    rng = random.Random(2)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset()
def src_3() -> dict:
    rng = random.Random(3)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


@asset()
def src_4() -> dict:
    rng = random.Random(4)
    return {"rows": [{"id": i, "val": rng.random()} for i in range(1000)]}


# ── 5 prep steps (filter + normalize) ──


@asset(inputs={"data": src_0})
def prep_0(data: dict) -> dict:
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(inputs={"data": src_1})
def prep_1(data: dict) -> dict:
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(inputs={"data": src_2})
def prep_2(data: dict) -> dict:
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(inputs={"data": src_3})
def prep_3(data: dict) -> dict:
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@asset(inputs={"data": src_4})
def prep_4(data: dict) -> dict:
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


# ── 5 feature engineering steps ──


@asset(inputs={"data": prep_0})
def feat_0(data: dict) -> dict:
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


@asset(inputs={"data": prep_1})
def feat_1(data: dict) -> dict:
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


@asset(inputs={"data": prep_2})
def feat_2(data: dict) -> dict:
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


@asset(inputs={"data": prep_3})
def feat_3(data: dict) -> dict:
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


@asset(inputs={"data": prep_4})
def feat_4(data: dict) -> dict:
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


@asset(inputs={"f0": feat_0, "f1": feat_1, "f2": feat_2, "f3": feat_3, "f4": feat_4})
def merge(f0: dict, f1: dict, f2: dict, f3: dict, f4: dict) -> dict:
    all_features = (
        f0["features"] + f1["features"] + f2["features"] + f3["features"] + f4["features"]
    )
    return {"combined": all_features, "count": len(all_features)}


# ── Post-processing chain ──


@asset(inputs={"data": merge})
def transform(data: dict) -> dict:
    sorted_data = sorted(data["combined"], key=lambda x: x["f"], reverse=True)
    return {"top_100": sorted_data[:100], "total": data["count"]}


@asset(inputs={"data": transform})
def output(data: dict) -> dict:
    avg = sum(r["f"] for r in data["top_100"]) / len(data["top_100"]) if data["top_100"] else 0
    return {
        "avg_top_feature": round(avg, 6),
        "total_rows": data["total"],
        "top_count": len(data["top_100"]),
    }
