"""Prefect: ETL pipeline — dbt-style layered transforms on synthetic 100k-row data."""

import json
import os
import random
import time
from collections import defaultdict

from prefect import flow, task
from prefect.task_runners import ConcurrentTaskRunner

# Matches barca's pool_size and dagster's max_concurrent for this benchmark run
# (see benchmarks/lib/env.sh) so no framework gets more/fewer workers than another.
BENCH_WORKERS = int(os.environ.get("BARCA_BENCH_WORKERS", "16"))


# -- Raw layer: generate synthetic data --


@task
def raw_orders():
    """100k orders over 3 years."""
    rng = random.Random(42)
    rows = []
    for i in range(100_000):
        rows.append(
            {
                "order_id": i,
                "customer_id": rng.randint(0, 9999),
                "product_id": rng.randint(0, 499),
                "quantity": rng.randint(1, 20),
                "unit_price": round(rng.uniform(5.0, 500.0), 2),
                "order_date": f"2022-{rng.randint(1, 12):02d}-{rng.randint(1, 28):02d}",
                "status": rng.choice(
                    ["completed", "completed", "completed", "returned", "cancelled"]
                ),
            }
        )
    return {"rows": rows}


@task
def raw_customers():
    """10k customers."""
    rng = random.Random(43)
    tiers = ["bronze", "silver", "gold", "platinum"]
    rows = []
    for i in range(10_000):
        rows.append(
            {
                "customer_id": i,
                "name": f"Customer_{i:05d}",
                "tier": rng.choice(tiers),
            }
        )
    return {"rows": rows}


@task
def raw_products():
    """500 products."""
    rng = random.Random(44)
    categories = ["electronics", "clothing", "home", "sports", "books"]
    rows = []
    for i in range(500):
        rows.append(
            {
                "product_id": i,
                "name": f"Product_{i:03d}",
                "category": rng.choice(categories),
                "cost": round(rng.uniform(2.0, 200.0), 2),
            }
        )
    return {"rows": rows}


# -- Staging layer: clean + transform --


@task
def stg_orders(raw):
    """Filter to completed orders, compute total."""
    rows = []
    for r in raw["rows"]:
        if r["status"] == "completed":
            rows.append(
                {
                    "order_id": r["order_id"],
                    "customer_id": r["customer_id"],
                    "product_id": r["product_id"],
                    "quantity": r["quantity"],
                    "unit_price": r["unit_price"],
                    "total": round(r["quantity"] * r["unit_price"], 2),
                    "month": r["order_date"][:7],
                }
            )
    return {"rows": rows}


@task
def stg_customers(raw):
    """Normalize tiers."""
    return {"rows": [{**r, "tier": r["tier"].upper()} for r in raw["rows"]]}


@task
def stg_products(raw):
    """Add margin."""
    return {"rows": [{**r, "margin": round(r["cost"] * 0.3, 2)} for r in raw["rows"]]}


# -- Intermediate layer: aggregates --


@task
def int_order_metrics(orders):
    """Monthly order metrics."""
    by_month = defaultdict(lambda: {"count": 0, "revenue": 0.0})
    for r in orders["rows"]:
        m = by_month[r["month"]]
        m["count"] += 1
        m["revenue"] += r["total"]
    rows = [
        {
            "month": k,
            "order_count": v["count"],
            "revenue": round(v["revenue"], 2),
            "avg_order_value": round(v["revenue"] / v["count"], 2),
        }
        for k, v in sorted(by_month.items())
    ]
    return {"rows": rows}


@task
def int_customer_agg(orders, customers):
    """Per-customer lifetime metrics."""
    cust_orders = defaultdict(lambda: {"count": 0, "total": 0.0})
    for o in orders["rows"]:
        c = cust_orders[o["customer_id"]]
        c["count"] += 1
        c["total"] += o["total"]
    cust_map = {c["customer_id"]: c for c in customers["rows"]}
    rows = []
    for cid, agg in cust_orders.items():
        cust = cust_map.get(cid, {})
        rows.append(
            {
                "customer_id": cid,
                "tier": cust.get("tier", "UNKNOWN"),
                "order_count": agg["count"],
                "lifetime_value": round(agg["total"], 2),
            }
        )
    return {"rows": rows}


@task
def int_product_stats(orders, products):
    """Per-product sales stats."""
    prod_sales = defaultdict(lambda: {"units": 0, "revenue": 0.0})
    for o in orders["rows"]:
        p = prod_sales[o["product_id"]]
        p["units"] += o["quantity"]
        p["revenue"] += o["total"]
    prod_map = {p["product_id"]: p for p in products["rows"]}
    rows = []
    for pid, agg in prod_sales.items():
        prod = prod_map.get(pid, {})
        rows.append(
            {
                "product_id": pid,
                "category": prod.get("category", "unknown"),
                "units_sold": agg["units"],
                "revenue": round(agg["revenue"], 2),
            }
        )
    return {"rows": rows}


# -- Mart layer --


@task
def mart_customer_orders(customers, orders, products):
    return {
        "customer_count": len(customers["rows"]),
        "months": len(orders["rows"]),
        "product_count": len(products["rows"]),
        "total_revenue": round(sum(r["revenue"] for r in orders["rows"]), 2),
        "top_customers": sorted(customers["rows"], key=lambda x: x["lifetime_value"], reverse=True)[
            :10
        ],
    }


@task
def mart_summary(data):
    return {
        "customer_count": data["customer_count"],
        "months_tracked": data["months"],
        "product_count": data["product_count"],
        "total_revenue": data["total_revenue"],
        "avg_revenue_per_month": round(data["total_revenue"] / max(data["months"], 1), 2),
        "top_customer_ltv": data["top_customers"][0]["lifetime_value"]
        if data["top_customers"]
        else 0,
    }


@flow(task_runner=ConcurrentTaskRunner(max_workers=BENCH_WORKERS))
def etl_duckdb_flow():
    # Raw layer (parallel)
    orders_raw = raw_orders.submit()
    customers_raw = raw_customers.submit()
    products_raw = raw_products.submit()

    # Staging layer (parallel, each depends on its raw source)
    orders_stg = stg_orders.submit(orders_raw)
    customers_stg = stg_customers.submit(customers_raw)
    products_stg = stg_products.submit(products_raw)

    # Intermediate layer (parallel, each depends on its staging inputs)
    order_metrics = int_order_metrics.submit(orders_stg)
    customer_agg = int_customer_agg.submit(orders_stg, customers_stg)
    product_stats = int_product_stats.submit(orders_stg, products_stg)

    # Mart layer
    customer_orders = mart_customer_orders.submit(customer_agg, order_metrics, product_stats)
    summary = mart_summary(customer_orders)
    return summary


if __name__ == "__main__":
    t0 = time.perf_counter()
    result = etl_duckdb_flow()
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 11,
                "result": result,
            },
            indent=2,
        )
    )
