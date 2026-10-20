"""Registry-driven drivers, in polars: each is a sparse adjustment at item x week grain."""

import polars as pl

ITEM_WEEK = ["account_id", "group_id", "item_id", "week", "value"]


def from_registry(registry: pl.DataFrame, items: pl.DataFrame, sign: float) -> pl.DataFrame:
    """Items named by the registry, from the registry's week on, scaled by its rate."""
    reg = registry.filter(pl.col("item_id").is_not_null()).select(
        "item_id", pl.col("week").alias("from_week"), "rate"
    )
    return (
        items.join(reg, on="item_id")
        .filter(pl.col("week") >= pl.col("from_week"))
        .with_columns((pl.col("value") * pl.col("rate") * sign).alias("value"))
        .select(ITEM_WEEK)
    )


def displacement(registry: pl.DataFrame, items: pl.DataFrame) -> pl.DataFrame:
    launched = registry.filter(pl.col("kind") == "displacement").select("item_id", "rate")
    return (
        items.join(launched, on="item_id")
        .group_by("account_id", "group_id", "week")
        .agg((pl.col("value") * pl.col("rate") * -0.1).sum().alias("value"))
    )


def item_week_rates(registry: pl.DataFrame) -> pl.DataFrame:
    return (
        registry.filter(pl.col("item_id").is_not_null())
        .group_by("item_id", "week")
        .agg((1.0 + pl.col("rate")).max().alias("factor"))
    )


def group_week_rates(registry: pl.DataFrame, items: pl.DataFrame) -> pl.DataFrame:
    groups = items.select("item_id", "group_id").unique()
    return (
        registry.join(groups, on="item_id")
        .group_by("group_id", "week")
        .agg((1.0 + pl.col("rate")).mean().alias("factor"))
    )
