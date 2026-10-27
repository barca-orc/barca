"""Dagster: ETL pipeline — dbt-style layered transforms on synthetic 100k-row
data, using vectorized pandas DataFrames. Mirrors ../barca/assets.py so the
DataFrame-vs-dict-of-rows comparison isn't conflated with a framework
difference — see benchmarks/etl_duckdb_dataframes for context.
"""

import json
import time

import numpy as np
import pandas as pd
from dagster import AssetIn, asset, materialize


# -- Raw layer: generate synthetic data --


@asset
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


@asset
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


@asset
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


@asset(ins={"raw": AssetIn(key="raw_orders")})
def stg_orders(raw):
    """Filter to completed orders, compute total."""
    df = raw[raw["status"] == "completed"].copy()
    df["total"] = (df["quantity"] * df["unit_price"]).round(2)
    df["month"] = df["order_date"].str[:7]
    return df[["order_id", "customer_id", "product_id", "quantity", "unit_price", "total", "month"]]


@asset(ins={"raw": AssetIn(key="raw_customers")})
def stg_customers(raw):
    """Normalize tiers."""
    df = raw.copy()
    df["tier"] = df["tier"].str.upper()
    return df


@asset(ins={"raw": AssetIn(key="raw_products")})
def stg_products(raw):
    """Add margin."""
    df = raw.copy()
    df["margin"] = (df["cost"] * 0.3).round(2)
    return df


# -- Intermediate layer: aggregates --


@asset(ins={"orders": AssetIn(key="stg_orders")})
def int_order_metrics(orders):
    """Monthly order metrics."""
    grouped = orders.groupby("month")["total"].agg(order_count="count", revenue="sum")
    grouped["revenue"] = grouped["revenue"].round(2)
    grouped["avg_order_value"] = (grouped["revenue"] / grouped["order_count"]).round(2)
    return grouped.reset_index().sort_values("month")


@asset(ins={"orders": AssetIn(key="stg_orders"), "customers": AssetIn(key="stg_customers")})
def int_customer_agg(orders, customers):
    """Per-customer lifetime metrics."""
    agg = orders.groupby("customer_id")["total"].agg(order_count="count", lifetime_value="sum")
    agg["lifetime_value"] = agg["lifetime_value"].round(2)
    agg = agg.reset_index()
    merged = agg.merge(customers[["customer_id", "tier"]], on="customer_id", how="left")
    merged["tier"] = merged["tier"].fillna("UNKNOWN")
    return merged[["customer_id", "tier", "order_count", "lifetime_value"]]


@asset(ins={"orders": AssetIn(key="stg_orders"), "products": AssetIn(key="stg_products")})
def int_product_stats(orders, products):
    """Per-product sales stats."""
    agg = orders.groupby("product_id").agg(units_sold=("quantity", "sum"), revenue=("total", "sum"))
    agg["revenue"] = agg["revenue"].round(2)
    agg = agg.reset_index()
    merged = agg.merge(products[["product_id", "category"]], on="product_id", how="left")
    merged["category"] = merged["category"].fillna("unknown")
    return merged[["product_id", "category", "units_sold", "revenue"]]


# -- Mart layer --


@asset(
    ins={
        "customers": AssetIn(key="int_customer_agg"),
        "orders": AssetIn(key="int_order_metrics"),
        "products": AssetIn(key="int_product_stats"),
    }
)
def mart_customer_orders(customers, orders, products):
    top = customers.sort_values("lifetime_value", ascending=False).head(10)
    return {
        "customer_count": len(customers),
        "months": len(orders),
        "product_count": len(products),
        "total_revenue": round(orders["revenue"].sum(), 2),
        "top_customers": top.to_dict("records"),
    }


@asset(ins={"data": AssetIn(key="mart_customer_orders")})
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


if __name__ == "__main__":
    all_assets = [
        raw_orders,
        raw_customers,
        raw_products,
        stg_orders,
        stg_customers,
        stg_products,
        int_order_metrics,
        int_customer_agg,
        int_product_stats,
        mart_customer_orders,
        mart_summary,
    ]
    t0 = time.perf_counter()
    result = materialize(all_assets)
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 11,
                "success": result.success,
            },
            indent=2,
        )
    )
