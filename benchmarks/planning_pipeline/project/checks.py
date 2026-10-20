"""Validation helpers: print a summary, raise on a failed check."""

import engine


def summary(name: str, rel) -> int:
    n = engine.row_count(rel)
    print(f"{name}: {n} rows, columns {', '.join(rel.columns)}")
    return n


def no_nulls(name: str, rel, keys: list[str]) -> None:
    cols = [k for k in keys if k in rel.columns]
    if not cols:
        return
    cond = " OR ".join(f"{c} IS NULL" for c in cols)
    bad = rel.filter(cond).aggregate("count(*)").fetchone()[0]
    if bad:
        raise ValueError(f"{name}: {bad} rows with a null in {', '.join(cols)}")


def validate(name: str, rel, keys: list[str]) -> None:
    summary(name, rel)
    no_nulls(name, rel, keys)


def staged_matches(name: str, rel, alias: str) -> None:
    """Check that the names `engine.staged` created hold this step's rows and no others."""
    c = engine.con()
    rows = sorted(rel.fetchall(), key=repr)
    keys = sorted(rel.distinct().fetchall(), key=repr)
    for table, own in ((f"{alias}_rows", rows), (f"{alias}_keys", keys)):
        seen = sorted(c.sql(f"SELECT * FROM {table}").fetchall(), key=repr)
        if seen != own:
            raise ValueError(
                f"{name}: {table} holds {len(seen)} rows, this step's input has {len(own)}: "
                "another step's data is visible under the same name"
            )


def frame_checks(name: str, df, weeks: int) -> None:
    """pandas checks: no null keys, weeks inside the plan."""
    keys = [c for c in ("account_id", "group_id", "item_id") if c in df.columns]
    if keys and df[keys].isna().any().any():
        raise ValueError(f"{name}: null keys")
    if "week" in df.columns and len(df) and not df["week"].between(1, weeks).all():
        raise ValueError(f"{name}: weeks outside 1..{weeks}")
    print(f"{name}: {len(df)} rows")
