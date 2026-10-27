"""Prefect: ETL pipeline — dbt-style layered transforms on synthetic 100k-row
data, using vectorized pandas DataFrames. Mirrors ../barca/assets.py so the
DataFrame-vs-dict-of-rows comparison isn't conflated with a framework
difference — see benchmarks/etl_duckdb_dataframes for context.
"""

import json
import os
import time

import numpy as np
import pandas as pd
from prefect import flow, task
from prefect.task_runners import ConcurrentTaskRunner

# Matches barca's pool_size and dagster's max_concurrent for this benchmark run
# (see benchmarks/lib/env.sh) so no framework gets more/fewer workers than another.
BENCH_WORKERS = int(os.environ.get("BARCA_BENCH_WORKERS", "16"))


# -- Raw layer: generate synthetic data --


@task
def raw_orders():
    """100k orders over 3 years, built via vectorized numpy generation."""
    rng = np.random.default_rng(42)
    n = 100_000
    months = rng.integers(1, 13, n)
    days = rng.integers(1, 29, n)
    return pd.DataFrame(
        {
            "order_id": np.arange(n),
            "customer_id": rng.integers(0, 10_000, n),
            "product_id": rng.integers(0, 500, n),
            "quantity": rng.integers(1, 21, n),
            "unit_price": np.round(rng.uniform(5.0, 500.0, n), 2),
            "order_date": [f"2022-{m:02d}-{d:02d}" for m, d in zip(months, days)],
            "status": rng.choice(["completed", "returned", "cancelled"], n, p=[0.6, 0.2, 0.2]),
        }
    )


@task
def raw_customers():
    """10k customers."""
    rng = np.random.default_rng(43)
    n = 10_000
    tiers = ["bronze", "silver", "gold", "platinum"]
    return pd.DataFrame(
        {
            "customer_id": np.arange(n),
            "name": [f"Customer_{i:05d}" for i in range(n)],
            "tier": rng.choice(tiers, n),
        }
    )


@task
def raw_products():
    """500 products."""
    rng = np.random.default_rng(44)
    n = 500
    categories = ["electronics", "clothing", "home", "sports", "books"]
    return pd.DataFrame(
        {
            "product_id": np.arange(n),
            "name": [f"Product_{i:03d}" for i in range(n)],
            "category": rng.choice(categories, n),
            "cost": np.round(rng.uniform(2.0, 200.0, n), 2),
        }
    )


# -- Staging layer: clean + transform --


@task
def stg_orders(raw):
    """Filter to completed orders, compute total."""
    df = raw[raw["status"] == "completed"].copy()
    df["total"] = (df["quantity"] * df["unit_price"]).round(2)
    df["month"] = df["order_date"].str[:7]
    return df[["order_id", "customer_id", "product_id", "quantity", "unit_price", "total", "month"]]


@task
def stg_customers(raw):
    """Normalize tiers."""
    df = raw.copy()
    df["tier"] = df["tier"].str.upper()
    return df


@task
def stg_products(raw):
    """Add margin."""
    df = raw.copy()
    df["margin"] = (df["cost"] * 0.3).round(2)
    return df


# -- Intermediate layer: aggregates --


@task
def int_order_metrics(orders):
    """Monthly order metrics."""
    grouped = orders.groupby("month")["total"].agg(order_count="count", revenue="sum")
    grouped["revenue"] = grouped["revenue"].round(2)
    grouped["avg_order_value"] = (grouped["revenue"] / grouped["order_count"]).round(2)
    return grouped.reset_index().sort_values("month")


@task
def int_customer_agg(orders, customers):
    """Per-customer lifetime metrics."""
    agg = orders.groupby("customer_id")["total"].agg(order_count="count", lifetime_value="sum")
    agg["lifetime_value"] = agg["lifetime_value"].round(2)
    agg = agg.reset_index()
    merged = agg.merge(customers[["customer_id", "tier"]], on="customer_id", how="left")
    merged["tier"] = merged["tier"].fillna("UNKNOWN")
    return merged[["customer_id", "tier", "order_count", "lifetime_value"]]


@task
def int_product_stats(orders, products):
    """Per-product sales stats."""
    agg = orders.groupby("product_id").agg(units_sold=("quantity", "sum"), revenue=("total", "sum"))
    agg["revenue"] = agg["revenue"].round(2)
    agg = agg.reset_index()
    merged = agg.merge(products[["product_id", "category"]], on="product_id", how="left")
    merged["category"] = merged["category"].fillna("unknown")
    return merged[["product_id", "category", "units_sold", "revenue"]]


# -- Mart layer --


@task
def mart_customer_orders(customers, orders, products):
    top = customers.sort_values("lifetime_value", ascending=False).head(10)
    return {
        "customer_count": len(customers),
        "months": len(orders),
        "product_count": len(products),
        "total_revenue": round(orders["revenue"].sum(), 2),
        "top_customers": top.to_dict("records"),
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
def etl_duckdb_dataframes_flow():
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
    result = etl_duckdb_dataframes_flow()
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
