"""Half of the project the web UI's unit-test fixtures are generated from.

`crates/barca-core/src/ui_fixture_tests.rs` copies this directory into a scratch project,
records a history for it with a fixed clock, and writes what a server would answer about it
to `ui/src/lib/testdata/served/`. Nothing here is executed: only the definitions are read.
Each node is in one state the UI shows, named in its docstring. Changing a definition changes
the generated files: `BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca-core ui_fixture`.
"""

from barca import Manual, Schedule, asset, group, partitions, sensor, task


@sensor(freshness=Schedule("*/15 * * * *"))
def orders_feed() -> tuple[bool, str]:
    """A schedule on time: its last tick ran, its next is ahead."""
    return True, "batch-2026-11-17"


@asset(inputs={"version": orders_feed})
def orders(version: str) -> list[dict]:
    """Fresh: a cached result for this code and these inputs."""
    return [{"order_id": 1, "customer_id": 7, "amount": 42.0, "version": version}]


@asset()
def customers() -> list[dict]:
    """Fresh, and paused (`barca pause customers`)."""
    return [{"customer_id": 7, "segment": "business"}]


@asset(inputs={"orders": orders, "customers": customers})
def orders_enriched(orders: list, customers: list) -> list[dict]:
    """Stale with a cause: its code changed since its last run, and a queued run covers it."""
    segments = {c["customer_id"]: c["segment"] for c in customers}
    return [{**o, "segment": segments[o["customer_id"]]} for o in orders]


@asset(inputs={"orders": orders_enriched}, freshness=Schedule("0 * * * *"))
def revenue_hourly(orders: list) -> dict:
    """On an hourly schedule whose 14:00 tick waits for a run slot; its input is stale."""
    return {"revenue": sum(o["amount"] for o in orders)}


# The keys are written out: literal keys are read without importing this file.
@asset(
    partitions={
        "day": partitions(["2026-11-13", "2026-11-14", "2026-11-15", "2026-11-16", "2026-11-17"])
    }
)
def sales_by_day(day: str) -> dict:
    """Partitioned: three keys cached, one failed, one never computed."""
    return {"day": day, "units": 3}


@asset(
    inputs={"sales": sales_by_day},
    partitions={
        "day": partitions(["2026-11-13", "2026-11-14", "2026-11-15", "2026-11-16", "2026-11-17"])
    },
)
def sales_report(day: str, sales: dict) -> dict:
    """Partitioned, below `sales_by_day`: a key waits on the key of it that failed."""
    return {"day": day, "report": sales["units"]}


@asset(freshness=Manual)
def fx_rates() -> dict:
    """Pinned (`barca pin fx_rates`): held at the version it has."""
    return {"EUR": 1.08}


@asset()
def forecast() -> dict:
    """Never run."""
    return {"growth": 0.08}


@task(inputs={"revenue": revenue_hourly})
def publish_dashboard(revenue: dict) -> None:
    """A task that ran: `always_runs`."""
    print("published", revenue)


@task(freshness=Schedule("0 6 * * *"))
def nightly_export() -> None:
    """A schedule that is late: it fired yesterday, and nothing decided today's 06:00 slot."""
    print("exported")


reporting = group("Reporting", members=[revenue_hourly, publish_dashboard], output=revenue_hourly)
