"""Synthetic commerce data for exploring the Barca UI. No external services."""

import time
from pathlib import Path

from barca import Schedule, asset, sensor, task


@sensor(freshness=Schedule("*/15 * * * *"))
def orders_version() -> tuple[bool, str]:
    print("Demo storefront: order batch 2026-10-09 is available")
    return True, "batch-2026-10-09"


@sensor(freshness=Schedule("*/5 * * * *"))
def inventory_version() -> tuple[bool, str]:
    print("Demo warehouse: inventory snapshot v3 is available")
    return True, "inventory-v3"


@sensor()
def partner_feed_version() -> tuple[bool, str]:
    return True, "partner-feed-v1"


@asset(inputs={"version": orders_version})
def orders(version: str) -> list[dict]:
    print(f"Loading synthetic orders from {version}")
    return [
        {"order_id": f"ORD-{i:04d}", "customer_id": f"CUS-{i % 12:03d}",
         "region": ["Americas", "Europe", "Asia Pacific"][i % 3],
         "product": ["Notebook", "Desk lamp", "Backpack", "Headphones"][i % 4],
         "quantity": i % 4 + 1, "unit_price": [18.0, 42.0, 65.0, 89.0][i % 4]}
        for i in range(60)
    ]


@asset()
def customers() -> list[dict]:
    return [{"customer_id": f"CUS-{i:03d}", "name": f"Demo customer {i + 1}",
             "segment": "business" if i % 3 == 0 else "consumer"} for i in range(12)]


@asset(inputs={"version": inventory_version})
def inventory(version: str) -> list[dict]:
    print(f"Reading synthetic stock: {version}")
    return [{"product": product, "in_stock": count, "warehouse": "Demo East"}
            for product, count in [("Notebook", 420), ("Desk lamp", 82),
                                   ("Backpack", 16), ("Headphones", 0)]]


@asset(inputs={"orders": orders, "customers": customers})
def enriched_orders(orders: list, customers: list) -> list[dict]:
    lookup = {c["customer_id"]: c for c in customers}
    return [{**order, "segment": lookup[order["customer_id"]]["segment"],
             "revenue": round(order["quantity"] * order["unit_price"], 2)} for order in orders]


@asset(inputs={"orders": enriched_orders}, freshness=Schedule("0 * * * *"))
def revenue_by_region(orders: list) -> list[dict]:
    return [{"region": region, "orders": sum(o["region"] == region for o in orders),
             "revenue": sum(o["revenue"] for o in orders if o["region"] == region)}
            for region in ["Americas", "Europe", "Asia Pacific"]]


@asset(inputs={"orders": enriched_orders})
def revenue_by_product(orders: list) -> list[dict]:
    return [{"product": product, "units": sum(o["quantity"] for o in orders if o["product"] == product),
             "revenue": sum(o["revenue"] for o in orders if o["product"] == product)}
            for product in ["Notebook", "Desk lamp", "Backpack", "Headphones"]]


@asset()
def growth_assumption() -> dict:
    return {"growth_rate": 0.08}  # Seed temporarily changes this to demonstrate stale assets.


@asset(inputs={"regions": revenue_by_region, "assumption": growth_assumption})
def revenue_forecast(regions: list, assumption: dict) -> list[dict]:
    return [{"region": r["region"], "forecast": round(r["revenue"] * (1 + assumption["growth_rate"]), 2)}
            for r in regions]


@asset(inputs={"stock": inventory})
def stock_alerts(stock: list) -> list[dict]:
    return [{**item, "severity": "critical" if item["in_stock"] == 0 else "low"}
            for item in stock if item["in_stock"] < 20]


@asset(inputs={"orders": enriched_orders})
def customer_cohorts(orders: list) -> dict:
    return {"customers": len({o["customer_id"] for o in orders}), "status": "demo cohort"}


@asset(inputs={"version": partner_feed_version})
def partner_orders(version: str) -> list[dict]:
    return [{"partner": "Demo marketplace", "batch": version, "orders": 24}]


@asset(inputs={"stock": inventory})
def inventory_quality_check(stock: list) -> dict:
    empty = [item["product"] for item in stock if item["in_stock"] == 0]
    print(f"Checking {len(stock)} synthetic products")
    if empty:
        raise ValueError(f"Demo quality check: out-of-stock products: {', '.join(empty)}")
    return {"status": "passed"}


@task(inputs={"regions": revenue_by_region}, freshness=Schedule("0 9 * * 1-5"))
def publish_dashboard(regions: list) -> None:
    counter = Path(".demo-publish-count")
    count = int(counter.read_text()) if counter.exists() else 0
    counter.write_text(str(count + 1))
    time.sleep(0.04 * (count % 5 + 1))
    print(f"Demo dashboard refreshed: {len(regions)} regions")
    print(f"Total revenue: ${sum(r['revenue'] for r in regions):,.2f}")


@task(inputs={"alerts": stock_alerts}, freshness=Schedule("*/30 * * * *"))
def notify_stock_team(alerts: list) -> None:
    print(f"Demo notification preview: {len(alerts)} stock alerts")
    for alert in alerts:
        print(f"{alert['product']}: {alert['in_stock']} remaining ({alert['severity']})")


@task(inputs={"orders": enriched_orders})
def validate_orders(orders: list) -> None:
    assert all(order["revenue"] > 0 for order in orders)
    print(f"Validated {len(orders)} synthetic orders")


@task(inputs={"products": revenue_by_product}, freshness=Schedule("0 6 * * *"))
def export_partner_feed(products: list) -> None:
    print(f"Preparing {len(products)} product totals for the demo partner")
    raise ConnectionError("Synthetic failure: demo partner endpoint is unavailable; no network request was made")


@task(inputs={"cohorts": customer_cohorts})
def send_weekly_digest(cohorts: dict) -> None:
    print(f"Demo digest preview for {cohorts['customers']} customers")
