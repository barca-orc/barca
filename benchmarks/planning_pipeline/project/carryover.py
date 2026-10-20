"""Carryover logic, in polars: prior-year volume carried into the plan year."""

import polars as pl

OPT_OUT_RATE = 0.3


def account_group(ledger_prior: pl.DataFrame, registry: pl.DataFrame) -> pl.DataFrame:
    opted_out = registry.filter(pl.col("rate") < OPT_OUT_RATE).select("account_id").unique()
    return (
        ledger_prior.filter(pl.col("group_id").is_not_null())
        .join(opted_out, on="account_id", how="anti")
        .group_by("account_id", "group_id", "week")
        .agg(pl.col("units").sum().cast(pl.Float64).alias("value"))
        .sort("account_id", "group_id", "week")
    )


def period_deltas(tbl: pl.DataFrame) -> pl.DataFrame:
    return tbl.sort("account_id", "group_id", "week").with_columns(
        (pl.col("value") - pl.col("value").shift(1).over("account_id", "group_id"))
        .fill_null(0.0)
        .alias("value")
    )


def to_items(deltas: pl.DataFrame, shares: pl.DataFrame, items: pl.DataFrame) -> pl.DataFrame:
    keep = items.select("account_id", "item_id", "week").unique()
    return (
        deltas.join(shares.select("item_id", "group_id", "share"), on="group_id")
        .with_columns((pl.col("value") * pl.col("share") * 0.1).alias("value"))
        .join(keep, on=["account_id", "item_id", "week"], how="semi")
        .select("account_id", "group_id", "item_id", "week", "value")
    )


def check(carryover: pl.DataFrame, driver: pl.DataFrame) -> None:
    print(f"carryover: {carryover.height} account-group-weeks, driver: {driver.height} rows")
    if driver.height and driver["value"].is_nan().any():
        raise ValueError("carryover driver has NaN values")
