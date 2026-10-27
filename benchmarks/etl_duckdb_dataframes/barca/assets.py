"""ETL pipeline: dbt-style layered transforms on synthetic 100k-row data,
using vectorized pandas DataFrames instead of dict-of-rows structures.

Same topology and workload as ../etl_duckdb, rewritten to return DataFrames.
Barca auto-detects DataFrame return values and serializes them as parquet
(see detect_format() in python/barca/_artifacts.py) — no serializer= needed.
This is a distinct, faster mechanism than JSON or pickle for tabular data:
vectorized, typed-array (de)serialization via pyarrow's C++ reader/writer,
not Python-level object graph traversal. See benchmarks/RESULTS.md's
etl_duckdb notes for the investigation this benchmark completes.

Topology:
  raw_orders --> stg_orders --> int_order_metrics --+
  raw_customers -> stg_customers -> int_customer_agg -+-> mart_customer_orders -> mart_summary
  raw_products --> stg_products --> int_product_stats -+
"""

import numpy as np
import pandas as pd

from barca import asset


# -- Raw layer: generate synthetic data --


@asset()
def raw_orders() -> pd.DataFrame:
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


@asset()
def raw_customers() -> pd.DataFrame:
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


@asset()
def raw_products() -> pd.DataFrame:
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


@asset(inputs={"raw": raw_orders})
def stg_orders(raw: pd.DataFrame) -> pd.DataFrame:
    """Filter to completed orders, compute total."""
    df = raw[raw["status"] == "completed"].copy()
    df["total"] = (df["quantity"] * df["unit_price"]).round(2)
    df["month"] = df["order_date"].str[:7]
    return df[["order_id", "customer_id", "product_id", "quantity", "unit_price", "total", "month"]]


@asset(inputs={"raw": raw_customers})
def stg_customers(raw: pd.DataFrame) -> pd.DataFrame:
    """Normalize tiers."""
    df = raw.copy()
    df["tier"] = df["tier"].str.upper()
    return df


@asset(inputs={"raw": raw_products})
def stg_products(raw: pd.DataFrame) -> pd.DataFrame:
    """Add margin."""
    df = raw.copy()
    df["margin"] = (df["cost"] * 0.3).round(2)
    return df


# -- Intermediate layer: aggregates --


@asset(inputs={"orders": stg_orders})
def int_order_metrics(orders: pd.DataFrame) -> pd.DataFrame:
    """Monthly order metrics."""
    grouped = orders.groupby("month")["total"].agg(order_count="count", revenue="sum")
    grouped["revenue"] = grouped["revenue"].round(2)
    grouped["avg_order_value"] = (grouped["revenue"] / grouped["order_count"]).round(2)
    return grouped.reset_index().sort_values("month")


@asset(inputs={"orders": stg_orders, "customers": stg_customers})
def int_customer_agg(orders: pd.DataFrame, customers: pd.DataFrame) -> pd.DataFrame:
    """Per-customer lifetime metrics."""
    agg = orders.groupby("customer_id")["total"].agg(order_count="count", lifetime_value="sum")
    agg["lifetime_value"] = agg["lifetime_value"].round(2)
    agg = agg.reset_index()
    merged = agg.merge(customers[["customer_id", "tier"]], on="customer_id", how="left")
    merged["tier"] = merged["tier"].fillna("UNKNOWN")
    return merged[["customer_id", "tier", "order_count", "lifetime_value"]]


@asset(inputs={"orders": stg_orders, "products": stg_products})
def int_product_stats(orders: pd.DataFrame, products: pd.DataFrame) -> pd.DataFrame:
    """Per-product sales stats."""
    agg = orders.groupby("product_id").agg(units_sold=("quantity", "sum"), revenue=("total", "sum"))
    agg["revenue"] = agg["revenue"].round(2)
    agg = agg.reset_index()
    merged = agg.merge(products[["product_id", "category"]], on="product_id", how="left")
    merged["category"] = merged["category"].fillna("unknown")
    return merged[["product_id", "category", "units_sold", "revenue"]]


# -- Mart layer --


@asset(
    inputs={
        "customers": int_customer_agg,
        "orders": int_order_metrics,
        "products": int_product_stats,
    }
)
def mart_customer_orders(
    customers: pd.DataFrame, orders: pd.DataFrame, products: pd.DataFrame
) -> dict:
    top = customers.sort_values("lifetime_value", ascending=False).head(10)
    return {
        "customer_count": len(customers),
        "months": len(orders),
        "product_count": len(products),
        "total_revenue": round(orders["revenue"].sum(), 2),
        "top_customers": top.to_dict("records"),
    }


@asset(inputs={"data": mart_customer_orders})
def mart_summary(data: dict) -> dict:
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
