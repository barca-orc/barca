"""The IR round-trip fixture (`crates/barca-core/src/ir_roundtrip_tests.rs`): every kind of
declaration the graph carries.

Planned and never run: the bodies only have to parse and to read what they declare. One file,
because the test loads it from a temporary directory outside the project root.
"""

import os

from barca import (
    Manual,
    Schedule,
    asset,
    batches,
    batches_by,
    collect,
    group,
    partitions,
    partitions_from,
    sensor,
    sink,
    task,
)

REGIONS = ["emea", "amer", "apac"]


def region_keys() -> list:
    return [region for region in REGIONS]


@sensor(freshness=Schedule("*/5 * * * *", max_failures=3, timeout_seconds=60))
def inbox() -> tuple:
    return True, {"etag": "1"}


@asset(freshness=Manual, description="Reference rates, refreshed by hand", tags={"team": "data"})
def rates() -> dict:
    return {"usd": 1.0}


@asset(
    cutoff=True,
    env=["FIXTURE_MARKET"],
    retries=3,
    retry_backoff=1.5,
    timeout_seconds=30,
    weight=2,
)
def tickers() -> list:
    # FIXTURE_UNDECLARED is read and not declared: an undeclared-env finding.
    return [os.environ.get("FIXTURE_MARKET", "us"), os.environ.get("FIXTURE_UNDECLARED", "")]


@asset(partitions={"region": partitions(["emea", "amer", "apac"])}, serializer="json")
def sales(region: str) -> dict:
    return {"region": region, "amount": 1}


@asset(partitions={"region": partitions(region_keys())}, deterministic=False)
def stock(region: str) -> dict:
    return {"region": region, "units": 2}


def continent(region: str) -> str:
    return "europe" if region == "emea" else "other"


# Derived from a partitioned asset: the keys of `sales`, read key for key.
@asset(partitions={"region": partitions_from(sales)}, batch=2)
def margin(region: list, sales: dict) -> dict:
    return {"regions": region, "sales": sales}


# Reads a `Manual` asset that was never computed: its dry run is a usage error.
@asset(inputs={"rates": rates})
def priced(rates: dict) -> dict:
    return rates


# Derived from the list an unpartitioned asset returns: keys known only after it runs.
@asset(partitions={"ticker": partitions_from(tickers)})
def quote(ticker: str) -> dict:
    return {"ticker": ticker}


@asset(
    inputs={"stock": stock},
    partitions={"region": partitions(["emea", "amer", "apac"])},
    batch=batches([["region=emea"], ["region=amer", "region=apac"]]),
)
def restock(region: list, stock: dict) -> dict:
    return {"regions": region, "stock": stock}


@asset(partitions={"region": partitions_from(margin)}, batch=batches_by(continent, max_size=2))
def score(region: list, margin: dict) -> dict:
    return {"regions": region, "margin": margin}


@asset(
    partitions={"region": partitions(["emea", "amer"]), "tier": partitions([1, 2])},
    batch="all",
)
def grid(region: list, tier: list) -> dict:
    return {"regions": region, "tiers": tier}


@asset(inputs={"margins": collect(margin), "etag": inbox, "unused": tickers})
@sink("exports/report.json")
@sink("exports/report.parquet", serializer="parquet")
def report(margins: list, etag: dict, unused: list) -> dict:
    return {"margins": margins, "etag": etag}


@asset(
    inputs={"scores": collect(score)},
    freshness=Schedule("0 6 * * *", batch="all"),
    name="daily_scores",
)
@sink("exports/scores/", partition_by=["region"], rows_per_file=1000)
def scores(scores: list) -> list:
    return scores


@task(inputs={"report": report, "scores": scores})
def publish(report: dict, scores: list) -> None:
    print(report, scores)


@task(freshness=Schedule("30 6 * * 1-5"))
def notify() -> None:
    print("sent")


inputs_group = group("Inputs", members=[rates, tickers, sales, stock], output=sales)
scoring = group(
    "Scoring",
    members=[margin, score, scores],
    output=scores,
    description="Margins and what is computed from them",
)
group("Reporting", members=[report, publish], output=report)
