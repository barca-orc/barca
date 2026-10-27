"""ETL pipeline: dbt-style layered transforms on synthetic 100k-row data.
raw -> staging -> intermediate -> marts. 12 assets, realistic data volume.

Topology:
  raw_orders --> stg_orders --> int_order_metrics --+
  raw_customers -> stg_customers -> int_customer_agg -+-> mart_customer_orders -> mart_summary
  raw_products --> stg_products --> int_product_stats -+
"""

import random
from collections import defaultdict

from barca import asset


# -- Raw layer: generate synthetic data --


@asset(serializer="pickle")
def raw_orders() -> dict:
    """100k orders over 3 years.

    Pickled rather than the default JSON: this payload is a list of ~100k
    small dicts, and pickle is both smaller and dramatically faster to
    (de)serialize than JSON for this shape (measured: json.dump ~635ms vs
    pickle.dump ~64ms for this exact 14.9MB/100k-row payload; json.load
    ~165ms vs pickle.load ~64ms). See benchmarks/RESULTS.md's etl_duckdb
    notes for the full investigation.
    """
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


@asset()
def raw_customers() -> dict:
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


@asset()
def raw_products() -> dict:
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


@asset(inputs={"raw": raw_orders}, serializer="pickle")
def stg_orders(raw: dict) -> dict:
    """Filter to completed orders, compute total.

    Pickled like raw_orders — this is the fan-out point (read by all three
    int_* assets below), so a cheaper format here pays off three times over.
    """
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


@asset(inputs={"raw": raw_customers})
def stg_customers(raw: dict) -> dict:
    """Normalize tiers."""
    return {"rows": [{**r, "tier": r["tier"].upper()} for r in raw["rows"]]}


@asset(inputs={"raw": raw_products})
def stg_products(raw: dict) -> dict:
    """Add margin."""
    return {"rows": [{**r, "margin": round(r["cost"] * 0.3, 2)} for r in raw["rows"]]}


# -- Intermediate layer: aggregates --


@asset(inputs={"orders": stg_orders})
def int_order_metrics(orders: dict) -> dict:
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


@asset(inputs={"orders": stg_orders, "customers": stg_customers})
def int_customer_agg(orders: dict, customers: dict) -> dict:
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


@asset(inputs={"orders": stg_orders, "products": stg_products})
def int_product_stats(orders: dict, products: dict) -> dict:
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


@asset(
    inputs={
        "customers": int_customer_agg,
        "orders": int_order_metrics,
        "products": int_product_stats,
    }
)
def mart_customer_orders(customers: dict, orders: dict, products: dict) -> dict:
    return {
        "customer_count": len(customers["rows"]),
        "months": len(orders["rows"]),
        "product_count": len(products["rows"]),
        "total_revenue": round(sum(r["revenue"] for r in orders["rows"]), 2),
        "top_customers": sorted(customers["rows"], key=lambda x: x["lifetime_value"], reverse=True)[
            :10
        ],
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
