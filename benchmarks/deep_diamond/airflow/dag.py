import hashlib
import random
from datetime import datetime
from airflow.decorators import dag, task


@task
def src_0():
    return {"rows": [{"id": i, "val": random.Random(0).random()} for i in range(1000)]}


@task
def src_1():
    return {"rows": [{"id": i, "val": random.Random(1).random()} for i in range(1000)]}


@task
def src_2():
    return {"rows": [{"id": i, "val": random.Random(2).random()} for i in range(1000)]}


@task
def src_3():
    return {"rows": [{"id": i, "val": random.Random(3).random()} for i in range(1000)]}


@task
def src_4():
    return {"rows": [{"id": i, "val": random.Random(4).random()} for i in range(1000)]}


@task
def prep(data):
    rows = [r for r in data["rows"] if r["val"] > 0.2]
    mx = max(r["val"] for r in rows) if rows else 1
    return {"rows": [{"id": r["id"], "val": r["val"] / mx} for r in rows]}


@task
def feat(data):
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
def merge(f0, f1, f2, f3, f4):
    all_f = f0["features"] + f1["features"] + f2["features"] + f3["features"] + f4["features"]
    return {"combined": all_f, "count": len(all_f)}


@task
def transform(data):
    sorted_data = sorted(data["combined"], key=lambda x: x["f"], reverse=True)
    return {"top_100": sorted_data[:100], "total": data["count"]}


@task
def output(data):
    avg = sum(r["f"] for r in data["top_100"]) / len(data["top_100"]) if data["top_100"] else 0
    return {"avg_top_feature": round(avg, 6), "total_rows": data["total"]}


@dag(dag_id="deep_diamond", start_date=datetime(2024, 1, 1), schedule=None, catchup=False)
def deep_diamond_dag():
    s = [src_0(), src_1(), src_2(), src_3(), src_4()]
    p = [prep(s[i]) for i in range(5)]
    f = [feat(p[i]) for i in range(5)]
    m = merge(f[0], f[1], f[2], f[3], f[4])
    t = transform(m)
    output(t)


deep_diamond_dag()
