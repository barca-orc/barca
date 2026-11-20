"""The other half of the UI fixture project (see `orders.py`): a failure and what it blocks."""

from barca import asset, task


@asset()
def invoices() -> list[dict]:
    """Failing: it succeeded once, and its last two attempts failed."""
    rows = [{"invoice": 1, "total": -3.0}]
    if any(r["total"] < 0 for r in rows):
        raise ValueError("3 rows with a negative total")
    return rows


@asset(inputs={"invoices": invoices})
def invoice_totals(invoices: list) -> dict:
    """Blocked by a failed upstream: `invoices`."""
    return {"total": sum(r["total"] for r in invoices)}


@asset(inputs={"totals": invoice_totals})
def ledger(totals: dict) -> dict:
    """Blocked two steps below the failure, never run."""
    return {"balance": totals["total"]}


@task(inputs={"invoices": invoices})
def check__invoices(invoices: list) -> None:
    """A task whose last run failed."""
    assert all(r["total"] >= 0 for r in invoices), "3 rows with a negative total"
