"""Generate the planning-pipeline fixture (#250): a 116-node barca project with synthetic data.

The graph is `skeleton.txt`, the anonymised shape of a real pipeline: 74 assets and 42 tasks,
262 edges, duckdb relations and pandas frames as inputs and outputs, ten nodes that hop through
polars, a task that collects 14 inputs through `**frames`. This script turns it into
`project/assets.py` and writes a runnable project with seeded, deterministic data.

    python benchmarks/planning_pipeline/generate.py DEST [--scale N] [--seed S]
    python benchmarks/planning_pipeline/generate.py --update   # rewrite project/assets.py

`write_project(dest)` does the same from Python; the pytest fixture `planning_pipeline` in
`python/tests/conftest.py` calls it. The helper modules in `project/` are hand-written; only
`assets.py` and the data are generated.

What the project reproduces (see the issue for why each matters):

- roots return a lazy `read_parquet` over a URI that `sources.py` resolves from the environment,
  after probing it with fsspec; the forecast root's path holds a module-level snapshot date;
- `assets.py` loads `.env` at import, before importing `settings`, whose class attributes are
  fixed from the environment at import; none of these variables is declared to barca;
- five registries load from a CSV, a stubbed registry API (a JSON file with a version counter,
  cached per worker process), a published parquet, or an empty typed placeholder; one of them
  (`carryover_registry`) is not a root;
- steps stage inputs under shared names on DuckDB's default connection (a view, a registered
  frame, a temp table: `engine.staged`), and the registry validators raise if the names hold
  another step's rows;
- 28 assets run `fetchone()` on an aggregate of a lazy input and return a typed empty relation
  when it has no rows; one asset returns its input unchanged under a second name.

`PLANNING_SOURCE_MODE` selects where the roots read: `local` (the project's `data/`), `store`
or `hybrid` (an object store; `hybrid` is the name for keeping barca's own artifacts local).
`python/tests/test_planning_pipeline_remote.py` runs all three against the S3 emulator.

`--scale` multiplies the row counts: 1 is the CI size (a few hundred rows per table), 67 is the
large size of `bench.sh` (about 22M forecast rows, #233's scale).
"""

from __future__ import annotations

import argparse
import json
import random
import re
import shutil
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
SKELETON = HERE / "skeleton.txt"
PROJECT = HERE / "project"
HELPERS = sorted(p.name for p in PROJECT.glob("*.py") if p.name != "assets.py")

FORECAST_SNAPSHOT = "2026-10-05"
PLAN_YEAR = 2026

# ─── The skeleton ──────────────────────────────────────────────────────────────

_LINE = re.compile(
    r"(?P<kind>asset|task)\s+(?P<name>\w+)\((?P<params>.*)\)\s*(?P<kwargs>\*\*kwargs)?"
    r"\s*->\s*(?P<ret>\S+)(?P<polars>\s*\[polars\])?$"
)
_PARAM = re.compile(r"(\w+):([RD])=(\w+)")


@dataclass
class Node:
    kind: str
    name: str
    params: list[tuple[str, str, str]]  # (parameter, R|D, upstream)
    returns: str  # R, D or -
    kwargs: bool
    polars: bool


def parse_skeleton(text: str | None = None) -> list[Node]:
    nodes = []
    for line in (text if text is not None else SKELETON.read_text()).strip().splitlines():
        m = _LINE.match(line.strip())
        if not m:
            raise ValueError(f"unparseable skeleton line: {line}")
        params = [_PARAM.fullmatch(p.strip()) for p in m["params"].split(",") if p.strip()]
        nodes.append(
            Node(
                kind=m["kind"],
                name=m["name"],
                params=[(p[1], p[2], p[3]) for p in params],
                returns=m["ret"],
                kwargs=bool(m["kwargs"]),
                polars=bool(m["polars"]),
            )
        )
    return nodes


# ─── Asset bodies ──────────────────────────────────────────────────────────────
# SQL names the inputs: barca binds every duckdb-typed input as a view named after its
# parameter, on the default connection `duckdb.sql` uses.

ROOTS = {
    "dim_item_catalog": 'sources.read("dims/item_catalog.parquet")',
    "dim_group": 'sources.read("dims/group.parquet")',
    "dim_group_pack_sizes": 'sources.read("dims/group_pack_sizes.parquet")',
    "dim_account": 'sources.read("dims/account.parquet")',
    "ledger_source": 'sources.read("ledger/ledger.parquet")',
    "forecast_source": 'sources.read(f"forecast/snapshot={FORECAST_SNAPSHOT}/forecast.parquet")',
    "campaign": 'sources.read("activity/campaign.parquet")',
    "channel": 'sources.read("activity/channel.parquet")',
    "app_dim_group": 'sources.read("app/group.parquet")',
    "app_dim_account": 'sources.read("app/account.parquet")',
    "account_exit_registry": 'registries.load("account_exit")',
    "retirement_registry": 'registries.load("retirement")',
    "launch_registry": 'registries.load("launch")',
    "rollout_registry": 'registries.load("rollout")',
}

IW = "account_id, group_id, item_id, week"
GW = "account_id, group_id, week"


def _sql(query: str) -> str:
    return f'duckdb.sql(\n    """\n{_indent(query.strip(), 4)}\n    """\n)'


def _indent(text: str, n: int) -> str:
    pad = " " * n
    return "\n".join(pad + line if line else line for line in text.splitlines())


def _union(drivers: list[str], cols: str = f"{IW}, value", label: bool = False) -> str:
    parts = []
    for d in drivers:
        extra = f",\n       '{d}' AS driver" if label else ""
        parts.append(f"SELECT {cols}{extra}\nFROM {d}")
    return "\nUNION ALL\n".join(parts)


DRIVERS = [
    "rollout",
    "launch",
    "displacement",
    "retirement",
    "carryover",
    "campaign",
    "channel",
    "trend",
    "account_exit",
]

SQL = {
    "ledger": f"""
SELECT account_id, group_id, week, units, revenue FROM source WHERE year = {PLAN_YEAR}
""",
    "ledger_prior_year": f"""
SELECT account_id, group_id, week, units, revenue FROM source WHERE year = {PLAN_YEAR - 1}
""",
    "forecast": "SELECT account_id, item_id, week, units FROM source WHERE units >= 0",
    "dim_item": """
SELECT c.item_id, c.group_id,
       coalesce(
           sum(f.units) / nullif(sum(sum(f.units)) OVER (PARTITION BY c.group_id), 0), 0
       ) AS share
FROM catalog c LEFT JOIN forecast f USING (item_id)
GROUP BY c.item_id, c.group_id
""",
    "forecast_group_level": f"""
SELECT {GW}, sum(units)::DOUBLE AS value
FROM forecast JOIN catalog USING (item_id)
GROUP BY ALL
""",
    "ledger_forecast_joined": f"""
SELECT {GW}, l.units AS ledger_units,
       coalesce(g.value, 0) AS forecast_units
FROM ledger l LEFT JOIN forecast_group g USING ({GW})
WHERE group_id IS NOT NULL
""",
    "forecast_reconciled": f"""
WITH f AS (
    SELECT account_id, group_id, item_id, week, units::DOUBLE AS units
    FROM forecast JOIN catalog USING (item_id)
),
g AS (SELECT {GW}, sum(units) AS fu FROM f GROUP BY ALL),
l AS (
    SELECT {GW}, sum(units) AS lu FROM ledger
    WHERE group_id IS NOT NULL
    GROUP BY ALL
)
SELECT {IW},
       units * coalesce(lu / nullif(fu, 0), 1.0) AS value
FROM f
JOIN g USING ({GW})
LEFT JOIN l USING ({GW})
""",
    "forecast_group_reconciled": f"""
SELECT {GW}, sum(value) AS value FROM forecast_item GROUP BY ALL
""",
    "ledger_reconciled": """
SELECT account_id, group_id, week, units, revenue FROM ledger
WHERE group_id IN (SELECT group_id FROM forecast_group)
""",
    "ledger_item_baseline": f"""
SELECT {IW}, l.units * s.share AS value
FROM ledger l JOIN item_shares s USING (group_id)
""",
    "unit_normalization_policy_group": """
SELECT group_id, coalesce(max(p.pack_size), 1)::DOUBLE AS factor
FROM (SELECT DISTINCT group_id FROM ledger) l LEFT JOIN pack_sizes p USING (group_id)
GROUP BY group_id
""",
    "ledger_base_units": f"""
SELECT {GW}, units * factor AS units, revenue
FROM ledger JOIN policy USING (group_id)
""",
    "forecast_group_reconciled_base_units": f"""
SELECT {GW}, value * factor AS value
FROM forecast_group JOIN policy USING (group_id)
""",
    "forecast_reconciled_base_units": f"""
SELECT {IW}, value * factor AS value
FROM forecast_item JOIN policy USING (group_id)
""",
    "conversion_factors_group": """
SELECT group_id, coalesce(sum(revenue) / nullif(sum(units), 0), 0) AS factor
FROM ledger GROUP BY group_id
""",
    "conversion_factors_item": """
SELECT c.item_id, coalesce(g.factor, 0) AS factor
FROM catalog c LEFT JOIN group_factors g USING (group_id)
""",
    "conversion_factors_item_week": """
SELECT DISTINCT item_id, week, 1.0 + (week % 4) * 0.01 AS factor FROM forecast_item
""",
    "channel_account_group": f"""
SELECT {GW}, c.value
FROM channel c
JOIN "group" USING (group_id)
JOIN accounts USING (account_id)
WHERE EXISTS (
    SELECT 1 FROM ledger l WHERE l.account_id = c.account_id AND l.group_id = c.group_id
)
""",
    "channel_account": """
SELECT account_id, sum(value) AS value FROM channel_account_group GROUP BY account_id
""",
    "driver_channel_item": f"""
SELECT {IW}, f.value * c.value * k.factor * 0.01 AS value
FROM forecast_item f
JOIN channel_account_group c USING ({GW})
JOIN conversion k USING (group_id)
""",
    "campaign_account_group": f"""
SELECT {GW}, c.value
FROM campaign c JOIN "group" USING (group_id)
WHERE EXISTS (
    SELECT 1 FROM ledger l WHERE l.account_id = c.account_id AND l.group_id = c.group_id
)
""",
    "exited_accounts": "SELECT account_id, min(week) AS week FROM registry GROUP BY account_id",
    "driver_displacement_item": """
SELECT f.account_id, f.group_id, f.item_id, f.week, g.value * s.share AS value
FROM group_driver g
JOIN item_shares s USING (group_id)
JOIN forecast_item f
  ON f.item_id = s.item_id AND f.account_id = g.account_id AND f.week = g.week
""",
    "trend_driver_item": f"""
SELECT {IW}, f.value * 0.02 * s.share AS value
FROM forecast_item f JOIN item_shares s USING (item_id, group_id)
WHERE group_id IN (SELECT group_id FROM forecast)
""",
    "driver_campaign_item": f"""
SELECT {IW}, f.value * c.value * k.factor * 0.01 AS value
FROM forecast_item f
JOIN campaign_account_group c USING ({GW})
JOIN conversion k USING (group_id)
""",
    "driver_account_exit_item": """
SELECT f.account_id, f.group_id, f.item_id, f.week, -f.value AS value
FROM forecast_item f JOIN exited e USING (account_id)
WHERE f.week >= e.week
""",
    "driver_floor_patch_item": f"""
WITH d AS (
{_indent(_union(DRIVERS), 4)}
),
s AS (SELECT {IW}, sum(value) AS adj FROM d GROUP BY ALL)
SELECT {IW},
       greatest(0, -(f.value + coalesce(s.adj, 0))) AS value
FROM forecast_item f LEFT JOIN s USING ({IW})
WHERE account_id NOT IN (SELECT account_id FROM exited)
""",
    "plan_drivers_long_item": f"""
SELECT * FROM (
{_indent(_union(DRIVERS + ["floor_patch"], label=True), 4)}
)
WHERE account_id NOT IN (SELECT account_id FROM exited) OR driver = 'account_exit'
""",
    "plan_drivers_monetized_item": """
SELECT l.*, l.value * coalesce(lo.factor, ro.factor, c.factor, 1.0) AS amount
FROM drivers_long l
LEFT JOIN conversion c USING (item_id, week)
LEFT JOIN launch_rate_override_week lo USING (item_id, week)
LEFT JOIN rollout_rate_override_group_week ro USING (group_id, week)
WHERE l.item_id IN (SELECT item_id FROM forecast_item)
""",
    "plan_driver_toplines_year": """
SELECT driver, sum(value) AS value, sum(amount) AS amount FROM monetized GROUP BY driver
""",
    "projected_item_week": f"""
WITH d AS (
{_indent(_union(DRIVERS + ["floor_patch"]), 4)}
),
s AS (SELECT {IW}, sum(value) AS adj FROM d GROUP BY ALL)
SELECT {IW}, f.value + coalesce(s.adj, 0) AS value
FROM forecast_item f LEFT JOIN s USING ({IW})
WHERE account_id NOT IN (SELECT account_id FROM exited)
   OR f.week < (SELECT min(week) FROM exited WHERE account_id = f.account_id)
""",
    "projected_item_year": """
SELECT w.account_id, w.group_id, w.item_id, sum(w.value) AS value,
       coalesce(any_value(m.amount), 0) AS amount
FROM projected_week w
LEFT JOIN (
    SELECT account_id, item_id, sum(amount) AS amount FROM monetized GROUP BY ALL
) m USING (account_id, item_id)
GROUP BY ALL
""",
    "plan_year_summary": """
SELECT driver, value, amount, (SELECT sum(value) FROM forecast_item) AS base FROM toplines
""",
    "revenue_ratios_group": """
SELECT group_id, sum(revenue) / (SELECT sum(revenue) FROM ledger) AS factor
FROM ledger GROUP BY group_id
""",
    "reconciled_model_metrics": """
SELECT 'ledger_units' AS metric, sum(units)::DOUBLE AS value FROM ledger
UNION ALL
SELECT 'forecast_units', sum(value) FROM forecast_group
""",
}

POLARS = {
    "carryover_account_group": (
        "carryover.account_group(ledger_prior.pl(), opt_out_registry.pl())"
    ),
    "carryover_period_deltas": "carryover.period_deltas(carryover_tbl.pl())",
    "driver_carryover_item": (
        "carryover.to_items(period_deltas.pl(), item_shares.pl(), forecast_item.pl())"
    ),
    "driver_displacement_group_week": "drivers.displacement(registry.pl(), forecast_item.pl())",
    "launch_item_week_rate_override": "drivers.item_week_rates(registry.pl())",
    "rollout_group_week_rate_override": (
        "drivers.group_week_rates(registry.pl(), forecast_item.pl())"
    ),
}

# Registry-driven drivers stage their registry under the same names on the shared connection.
STAGED_DRIVERS = {
    "driver_retirement_item": -1.0,
    "driver_rollout_item": 1.0,
    "driver_launch_item": 1.0,
}

# The assets that check a lazy input for rows (a `fetchone()` on an aggregate) and return a
# typed empty relation when it has none: (input parameter, schema of the empty result).
GUARDS = {
    "ledger_forecast_joined": ("forecast_group", None),
    "forecast_reconciled": ("forecast", "ITEM_WEEK"),
    "forecast_group_reconciled": ("ledger", "GROUP_WEEK"),
    "ledger_reconciled": ("forecast_group", "LEDGER"),
    "ledger_item_baseline": ("item_shares", "ITEM_WEEK"),
    "ledger_base_units": ("policy", "LEDGER"),
    "forecast_group_reconciled_base_units": ("policy", "GROUP_WEEK"),
    "forecast_reconciled_base_units": ("policy", "ITEM_WEEK"),
    "conversion_factors_item": ("ledger", None),
    "channel_account_group": ("channel", "GROUP_WEEK"),
    "channel_account": ("channel", None),
    "driver_channel_item": ("channel_account_group", "ITEM_WEEK"),
    "campaign_account_group": ("campaign", "GROUP_WEEK"),
    "exited_accounts": ("registry", "EXITED"),
    "carryover_account_group": ("ledger_prior", "GROUP_WEEK"),
    "carryover_period_deltas": ("carryover_tbl", "GROUP_WEEK"),
    "driver_carryover_item": ("period_deltas", "ITEM_WEEK"),
    "driver_retirement_item": ("registry", "ITEM_WEEK"),
    "driver_rollout_item": ("registry", "ITEM_WEEK"),
    "driver_launch_item": ("registry", "ITEM_WEEK"),
    "driver_displacement_group_week": ("registry", "GROUP_WEEK"),
    "driver_displacement_item": ("group_driver", "ITEM_WEEK"),
    "trend_driver_item": ("forecast", "ITEM_WEEK"),
    "driver_campaign_item": ("campaign_account_group", "ITEM_WEEK"),
    "driver_account_exit_item": ("exited", "ITEM_WEEK"),
    "launch_item_week_rate_override": ("registry", "ITEM_WEEK_FACTOR"),
    "rollout_group_week_rate_override": ("registry", "GROUP_WEEK_FACTOR"),
    "plan_drivers_monetized_item": ("conversion", None),
}

# Columns a generic validator checks for nulls (only those the relation has are checked).
KEYS = ["account_id", "group_id", "item_id", "week"]
VALIDATE_KEYS = {
    # The raw ledger keeps rows without a group (the QA assets report them).
    "validate_ledger": ["account_id", "week"],
}

# QA assets: pandas in, pandas out.
QA = {
    "qa_ledger_group_not_in_dim_group": 'qa.missing(ledger, dim_group_df, ["group_id"])',
    "qa_forecast_item_not_in_dim_item": 'qa.missing(forecast, items, ["item_id"])',
    "qa_dim_item_not_in_forecast": 'qa.missing(items, forecast, ["item_id"])',
    "qa_dim_group_not_in_forecast": (
        'qa.missing(items, qa.with_group(forecast, items), ["group_id"])'
    ),
    "qa_ledger_account_not_in_dim_account": 'qa.missing(ledger, accounts, ["account_id"])',
    "qa_forecast_account_not_in_dim_account": 'qa.missing(forecast, accounts, ["account_id"])',
    "qa_dim_account_not_in_ledger": 'qa.missing(accounts, ledger, ["account_id"])',
    "qa_dim_account_not_in_forecast": 'qa.missing(accounts, forecast, ["account_id"])',
    "qa_account_in_forecast_not_in_ledger": 'qa.missing(forecast, ledger, ["account_id"])',
    "qa_account_in_ledger_not_in_forecast": 'qa.missing(ledger, forecast, ["account_id"])',
    "qa_group_account_in_forecast_not_in_ledger": (
        'qa.missing(qa.with_group(forecast, items), ledger, ["account_id", "group_id"])'
    ),
    "qa_group_account_in_ledger_not_in_forecast": (
        'qa.missing(ledger, qa.with_group(forecast, items), ["account_id", "group_id"])'
    ),
    "qa_ledger_group_not_in_dim_item": 'qa.missing(ledger, items, ["group_id"])',
    "qa_ledger_rows_null_group_id": 'qa.null_rows(ledger, "group_id")',
}


def _annotation(t: str) -> str:
    return {"R": "duckdb.DuckDBPyRelation", "D": "pd.DataFrame", "-": "None"}[t]


def _asset_body(node: Node) -> str:
    name = node.name
    if name in ROOTS:
        return f"return {ROOTS[name]}"
    if name in QA:
        return f"return {QA[name]}"
    lines = []
    if name in GUARDS:
        param, schema = GUARDS[name]
        empty = f"engine.empty(schemas.{schema})" if schema else f"{param}.limit(0)"
        lines += [f"if engine.row_count({param}) == 0:", f"    return {empty}"]
    if name == "carryover_registry":
        lines += [
            'external = registries.load("carryover")',
            "derived = duckdb.sql(",
            '    "SELECT DISTINCT account_id, NULL::BIGINT AS item_id, week, 0.5 AS rate, "',
            "    \"'carryover' AS kind \"",
            '    "FROM ledger_prior WHERE units > 50"',
            ")",
            "if engine.row_count(external) == 0:",
            "    return derived",
            "return external.union(derived)",
        ]
    elif name == "carryover_opt_out_registry":
        lines += ["return registry"]
    elif name in STAGED_DRIVERS:
        lines += [
            'with engine.staged(registry, "registry") as con:',
            f'    checks.staged_matches("{name}", registry, "registry")',
            '    staged = con.sql("SELECT * FROM registry_rows").pl()',
            f"out = drivers.from_registry(staged, forecast_item.pl(), {STAGED_DRIVERS[name]})",
            "return engine.from_polars(out)",
        ]
    elif name in POLARS:
        call = f"return engine.from_polars({POLARS[name]})"
        if len(call) > 96:
            call = f"return engine.from_polars(\n    {POLARS[name]}\n)"
        lines += [call]
    elif name in SQL:
        lines += [f"return {_sql(SQL[name])}"]
    else:
        raise KeyError(f"no body for asset {name}")
    return "\n".join(lines)


def _task_body(node: Node) -> str:
    name = node.name
    if name == "summarize_qa":
        return "\n".join(
            [
                "for check, frame in sorted(frames.items()):",
                '    print(f"{check}: {len(frame)} rows")',
                f"if len(frames) != {len(node.params)}:",
                f'    raise ValueError(f"expected {len(node.params)} QA frames, '
                + 'got {len(frames)}")',
            ]
        )
    if name == "validate_plan_grid":
        return "\n".join(
            f'checks.frame_checks("{name}.{p}", {p}, Settings.WEEKS)' for p, _, _ in node.params
        )
    if name == "validate_carryover_pipeline":
        # The `carryover` parameter shadows the module, so the check is imported by name.
        return "check_carryover(carryover.pl(), df.pl())"
    first = node.params[0][0]
    if first == "registry":
        with_items = any(p == "forecast_item" for p, _, _ in node.params)
        lines = [
            'with engine.staged(registry, "registry")' + (" as con:" if with_items else ":"),
            f'    checks.staged_matches("{name}", registry, "registry")',
        ]
        if with_items:
            lines += [
                "    unknown = con.sql(",
                '        "SELECT count(*) FROM registry_rows WHERE item_id IS NOT NULL "',
                '        "AND item_id NOT IN (SELECT item_id FROM forecast_item)"',
                "    ).fetchone()[0]",
                f'print(f"{name}: {{unknown}} registry items not in the forecast")',
            ]
        else:
            lines += [f'    checks.summary("{name}", registry)']
        return "\n".join(lines)
    keys = json.dumps(VALIDATE_KEYS[name]) if name in VALIDATE_KEYS else "KEYS"
    lines = [f'checks.validate("{name}", {first}, {keys})']
    for p, _, _ in node.params[1:]:
        lines.append(f'checks.summary("{name}.{p}", {p})')
    return "\n".join(lines)


ASSETS_HEADER = '''"""Planning pipeline. Generated by benchmarks/planning_pipeline/generate.py.

{nodes} nodes ({assets} assets, {tasks} tasks), {edges} edges. See the generator's docstring for
the behaviours this file reproduces.
"""

from pathlib import Path

import envfile

# Load the project's .env before `settings` fixes its attributes from the environment.
envfile.load(Path(__file__).resolve().parent / ".env")

import carryover
import checks
import drivers
import duckdb
import engine
import pandas as pd
import qa
import registries
import schemas
import sources
from barca import asset, task
from carryover import check as check_carryover
from settings import Settings

# A new upstream drop means editing this date.
FORECAST_SNAPSHOT = "{snapshot}"

# Key columns the validators check for nulls, where a relation has them.
KEYS = {keys}
'''


def _wrap(head: str, items: list[str], tail: str, limit: int = 100) -> str:
    """`head(a, b)tail` on one line if it fits, else one item per line (ruff's layout)."""
    line = f"{head}{', '.join(items)}{tail}"
    if len(line) <= limit:
        return line
    return head + "\n" + "".join(f"    {i},\n" for i in items) + tail


def _render_node(node: Node) -> str:
    if node.params:
        inputs = [f'"{p}": {up}' for p, _, up in node.params]
        decorator = f"@{node.kind}(inputs={{{', '.join(inputs)}}})"
        if len(decorator) > 100:
            lines = "".join(f"        {i},\n" for i in inputs)
            decorator = f"@{node.kind}(\n    inputs={{\n{lines}    }}\n)"
    else:
        decorator = f"@{node.kind}()"
    if node.kwargs:
        params = ["**frames: pd.DataFrame"]
    else:
        params = [f"{p}: {_annotation(t)}" for p, t, _ in node.params]
    signature = _wrap(f"def {node.name}(", params, f") -> {_annotation(node.returns)}:")
    body = _asset_body(node) if node.kind == "asset" else _task_body(node)
    return f"{decorator}\n{signature}\n{_indent(body, 4)}\n"


def render_assets(nodes: list[Node] | None = None) -> str:
    nodes = nodes if nodes is not None else parse_skeleton()
    edges = sum(len(n.params) for n in nodes)
    out = [
        ASSETS_HEADER.format(
            nodes=len(nodes),
            assets=sum(n.kind == "asset" for n in nodes),
            tasks=sum(n.kind == "task" for n in nodes),
            edges=edges,
            snapshot=FORECAST_SNAPSHOT,
            keys=json.dumps(KEYS),
        )
    ]
    for node in nodes:
        out.append("\n\n" + _render_node(node))
    text = "".join(out)
    if node_lines := [line for line in text.splitlines() if len(line) > 100]:
        raise ValueError("generated lines too long:\n" + "\n".join(node_lines))
    return text


# ─── Data ──────────────────────────────────────────────────────────────────────


@dataclass
class Sizes:
    accounts: int = 4
    groups: int = 3
    items_per_group: int = 3
    weeks: int = 6

    @classmethod
    def scaled(cls, scale: int) -> Sizes:
        return cls(accounts=4 * scale, groups=3 + scale - 1, items_per_group=3 * scale, weeks=6)


def _write_parquet(path: Path, rows: list[dict], types: dict) -> None:
    import pyarrow as pa
    import pyarrow.parquet as pq

    arrow = {"BIGINT": pa.int64(), "DOUBLE": pa.float64(), "VARCHAR": pa.string()}
    schema = pa.schema([(c, arrow[t]) for c, t in types.items()])
    path.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(pa.Table.from_pylist(rows, schema), path)


# Above this many forecast rows the forecast is drawn with numpy instead of row by row (the
# large size, about 22M rows, would take minutes and gigabytes as Python dicts). Every scale
# up to 20 stays on the row-by-row path, so the small size's bytes do not depend on numpy.
LARGE_ROWS = 1_000_000


def _write_large_forecast(path: Path, accounts, items, weeks, odd: list[dict], seed: int):
    """The forecast table, vectorized: one row per account x item x week, in that order."""
    import numpy as np
    import pyarrow as pa
    import pyarrow.parquet as pq

    gen = np.random.default_rng(seed)
    item_ids = np.array([i for i, _ in items], dtype=np.int64)
    n_a, n_i, n_w = len(accounts), len(item_ids), len(weeks)
    columns = {
        "account_id": np.repeat(np.array(accounts, dtype=np.int64), n_i * n_w),
        "item_id": np.tile(np.repeat(item_ids, n_w), n_a),
        "week": np.tile(np.array(weeks, dtype=np.int64), n_a * n_i),
        "units": gen.integers(1, 41, size=n_a * n_i * n_w, dtype=np.int64),
    }
    columns = {
        c: np.concatenate([values, np.array([row[c] for row in odd], dtype=np.int64)])
        for c, values in columns.items()
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(pa.table(columns), path, row_group_size=1_000_000)


def write_data(root: Path, sizes: Sizes, seed: int) -> None:
    """Seeded synthetic sources under `root/data`, plus the registry CSV and API stub."""
    rng = random.Random(seed)
    data = root / "data"
    accounts = list(range(1, sizes.accounts + 1))
    groups = list(range(1, sizes.groups + 1))
    # Item ids are <group><index>: 101, 102, ... for group 1. From 100 items a group on (scale
    # 34 and up) the index takes more digits, so ids stay unique across groups.
    width = 100 if sizes.items_per_group < 100 else 10 ** len(str(sizes.items_per_group))
    items = [(g * width + i, g) for g in groups for i in range(1, sizes.items_per_group + 1)]
    weeks = list(range(1, sizes.weeks + 1))
    B, D, V = "BIGINT", "DOUBLE", "VARCHAR"

    _write_parquet(
        data / "dims/item_catalog.parquet",
        [{"item_id": i, "group_id": g, "pack_size": rng.randint(1, 12)} for i, g in items],
        {"item_id": B, "group_id": B, "pack_size": B},
    )
    _write_parquet(
        data / "dims/group.parquet",
        [{"group_id": g, "group_name": f"group {g}"} for g in groups],
        {"group_id": B, "group_name": V},
    )
    _write_parquet(  # the last group has no pack size: the policy falls back to 1
        data / "dims/group_pack_sizes.parquet",
        [{"group_id": g, "pack_size": rng.randint(2, 6)} for g in groups[:-1]],
        {"group_id": B, "pack_size": B},
    )
    # Not every account is in the dimension, and one dimension account has no data.
    dim_accounts = accounts[:-1] + [sizes.accounts + 1]
    _write_parquet(
        data / "dims/account.parquet",
        [{"account_id": a, "region": rng.choice(["north", "south"])} for a in dim_accounts],
        {"account_id": B, "region": V},
    )
    _write_parquet(
        data / "app/group.parquet",
        [{"group_id": g, "group_name": f"group {g}"} for g in groups],
        {"group_id": B, "group_name": V},
    )
    _write_parquet(
        data / "app/account.parquet",
        [{"account_id": a, "region": "all"} for a in accounts],
        {"account_id": B, "region": V},
    )

    ledger = []
    for year in (PLAN_YEAR - 1, PLAN_YEAR):
        for a in accounts:
            for g in groups:
                for w in weeks:
                    units = float(rng.randint(10, 100))
                    ledger.append(
                        {
                            "year": year,
                            "account_id": a,
                            "group_id": g,
                            "week": w,
                            "units": units,
                            "revenue": units * rng.uniform(2, 5),
                        }
                    )
    # Rows without a group, and a group that no dimension knows.
    for w in weeks[:2]:
        ledger.append(
            {
                "year": PLAN_YEAR,
                "account_id": 1,
                "group_id": None,
                "week": w,
                "units": 5.0,
                "revenue": 10.0,
            }
        )
    ledger.append(
        {
            "year": PLAN_YEAR,
            "account_id": 2,
            "group_id": sizes.groups + 1,
            "week": 1,
            "units": 7.0,
            "revenue": 14.0,
        }
    )
    _write_parquet(
        data / "ledger/ledger.parquet",
        ledger,
        {"year": B, "account_id": B, "group_id": B, "week": B, "units": D, "revenue": D},
    )

    forecast_path = data / f"forecast/snapshot={FORECAST_SNAPSHOT}/forecast.parquet"
    forecast_types = {"account_id": B, "item_id": B, "week": B, "units": B}
    # An item outside the catalog, and a negative row the forecast asset drops.
    odd = [
        {"account_id": 1, "item_id": 999, "week": 1, "units": 3},
        {"account_id": 1, "item_id": items[0][0], "week": 1, "units": -1},
    ]
    if len(accounts) * len(items) * len(weeks) > LARGE_ROWS:
        _write_large_forecast(forecast_path, accounts, items, weeks, odd, seed)
    else:
        forecast = [
            {"account_id": a, "item_id": i, "week": w, "units": rng.randint(1, 40)}
            for a in accounts
            for i, _ in items
            for w in weeks
        ]
        _write_parquet(forecast_path, forecast + odd, forecast_types)

    for name in ("campaign", "channel"):
        rows = [
            {"account_id": a, "group_id": g, "week": w, "value": round(rng.uniform(0, 0.2), 4)}
            for a in accounts
            for g in groups
            for w in weeks
            if rng.random() < 0.4
        ]
        _write_parquet(
            data / f"activity/{name}.parquet",
            rows,
            {"account_id": B, "group_id": B, "week": B, "value": D},
        )

    # Registries, one per tier. Each registry names items of its own group, so a step that
    # saw another registry's rows would produce items outside its group.
    reg_types = {"account_id": B, "item_id": B, "week": B, "rate": D, "kind": V}

    def registry_rows(group: int, kind: str) -> list[dict]:
        return [
            {
                "account_id": None,
                "item_id": i,
                "week": rng.randint(2, sizes.weeks),
                "rate": round(rng.uniform(0.1, 0.5), 3),
                "kind": kind,
            }
            for i, g in items
            if g == group
        ]

    (root / "registries").mkdir(exist_ok=True)
    exit_week = sizes.weeks - 1
    (root / "registries/account_exit.csv").write_text(
        f"account_id,item_id,week,rate,kind\n{accounts[-1]},,{exit_week},1.0,exit\n"
    )
    launch = registry_rows(groups[0], "launch")
    launch[0] = {**launch[0], "kind": "displacement"}
    api = {
        "version": 1,
        "registries": {"launch": launch, "rollout": registry_rows(groups[1], "rollout")},
    }
    (root / "registry_api.json").write_text(json.dumps(api, indent=2) + "\n")
    _write_parquet(
        data / "registries/retirement.parquet",
        registry_rows(groups[2], "retirement"),
        reg_types,
    )
    # `carryover` has no external source at all: it falls through to the empty placeholder.

    (root / ".env").write_text(
        "\n".join(
            [
                "# Loaded by assets.py at import, before settings.py reads the environment.",
                "PLANNING_SOURCE_MODE=local",
                "PLANNING_LOCAL_SOURCE_ROOT=data",
                f"PLANNING_PLAN_YEAR={PLAN_YEAR}",
                f"PLANNING_WEEKS={sizes.weeks}",
                "ACCOUNT_EXIT_REGISTRY_CSV=registries/account_exit.csv",
                "REGISTRY_API_URL=file:registry_api.json",
                "REGISTRY_API_KEY=stub",
                "",
            ]
        )
    )


# ─── The project ───────────────────────────────────────────────────────────────


@dataclass
class Project:
    """A written planning-pipeline project and what its graph should look like."""

    root: Path
    nodes: list[Node] = field(repr=False)

    @property
    def assets_py(self) -> Path:
        return self.root / "assets.py"

    @property
    def edges(self) -> list[tuple[str, str]]:
        return [(up, n.name) for n in self.nodes for _, _, up in n.params]

    def names(self, kind: str | None = None) -> list[str]:
        return [n.name for n in self.nodes if kind is None or n.kind == kind]

    def leaves(self) -> list[str]:
        used = {up for up, _ in self.edges}
        return [n.name for n in self.nodes if n.name not in used]

    def downstream(self, names: set[str]) -> set[str]:
        """`names` and every node that depends on one of them."""
        out = set(names)
        changed = True
        while changed:
            changed = False
            for up, down in self.edges:
                if up in out and down not in out:
                    out.add(down)
                    changed = True
        return out


def write_project(dest: Path | str, *, scale: int = 1, seed: int = 250) -> Project:
    """Write the planning-pipeline project into `dest` (created if missing) and return it.

    `scale` multiplies the row counts (1 is a few hundred rows per table, for CI). The same
    `seed` always writes the same data.
    """
    root = Path(dest)
    root.mkdir(parents=True, exist_ok=True)
    nodes = parse_skeleton()
    for helper in HELPERS:
        shutil.copyfile(PROJECT / helper, root / helper)
    (root / "assets.py").write_text(render_assets(nodes))
    write_data(root, Sizes.scaled(scale), seed)
    return Project(root=root, nodes=nodes)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dest", nargs="?", help="directory to write the project into")
    parser.add_argument("--scale", type=int, default=1, help="row-count multiplier")
    parser.add_argument("--seed", type=int, default=250, help="random seed for the data")
    parser.add_argument(
        "--update", action="store_true", help="rewrite the checked-in project/assets.py"
    )
    args = parser.parse_args()
    if args.update:
        (PROJECT / "assets.py").write_text(render_assets())
        print(PROJECT / "assets.py")
        return
    if not args.dest:
        parser.error("DEST is required unless --update is given")
    print(write_project(args.dest, scale=args.scale, seed=args.seed).root)


if __name__ == "__main__":
    main()
