"""pandas QA checks: rows on one side whose key is missing on the other."""

import pandas as pd


def missing(left: pd.DataFrame, right: pd.DataFrame, keys: list[str]) -> pd.DataFrame:
    lk = left[keys].dropna().drop_duplicates()
    rk = right[keys].dropna().drop_duplicates()
    merged = lk.merge(rk, on=keys, how="left", indicator=True)
    return merged[merged["_merge"] == "left_only"][keys].reset_index(drop=True)


def with_group(forecast: pd.DataFrame, items: pd.DataFrame) -> pd.DataFrame:
    return forecast.merge(items[["item_id", "group_id"]], on="item_id", how="inner")


def null_rows(df: pd.DataFrame, column: str) -> pd.DataFrame:
    return df[df[column].isna()].reset_index(drop=True)
