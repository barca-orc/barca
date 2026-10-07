"""A declared data input the step never uses is reported at plan time (#231).

The rule is in `barca docs assets`, "Unused inputs"; the JSON shape in `barca docs contract`,
"Plan warnings". This file tests what a user sees: which commands report the warning, where
(stderr line and the `warnings` array, the same list on both), for which steps (the planned cone
only, once per step input), and that it never changes an exit code. The static rule itself
(what counts as a use, what is never reported) is unit-tested in
`crates/barca-core/src/unused_inputs.rs`; the cases a user is most likely to meet are repeated
here through the real binary.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PREFIX = "[barca] warning: "

# `report` ignores `other`; nothing else in the file has an unused input.
PIPELINE = """
from barca import asset, task


@asset()
def raw() -> dict:
    return {"n": 1}


@asset()
def other() -> dict:
    return {"m": 2}


@asset(inputs={"raw": raw, "other": other})
def report(raw: dict, other: dict) -> int:
    return raw["n"]


@asset(inputs={"raw": raw})
def unrelated(raw: dict) -> int:
    return raw["n"] + 1


@task(inputs={"report": report})
def publish(report: int) -> int:
    return report


@task(inputs={"report": report})
def fails(report: int) -> None:
    raise ValueError(f"no good: {report}")
"""

REPORT_OTHER = ("pipeline.py:report", "other")

# One step per way of being reported.
REPORTED = """
import polars as pl
from barca import asset, parallel, partitions, task


@asset()
def raw() -> pl.DataFrame:
    return pl.DataFrame({"n": [1, 2, 3]})


@asset(inputs={"raw": raw})
def dropped(raw: pl.DataFrame) -> int:
    del raw
    return 1


@asset(inputs={"raw": raw})
def lazy_polars(raw: pl.LazyFrame) -> int:
    return 2


@asset(inputs={"raw": raw}, partitions={"k": partitions(["a", "b", "c"])})
def per_key(raw: pl.DataFrame, k: str) -> str:
    return k


@task()
def child() -> int:
    return 1


@task(inputs={"raw": raw})
def fan(raw: pl.DataFrame) -> list:
    return parallel(child)
"""

# Inputs read by name from a string (SQL, a pandas expression): the Python name never appears in
# code, the step returns the right value, and nothing may be reported. The last four mention the
# input only where it does not count, and are reported.
BY_NAME = """
import duckdb
import pandas as pd
import polars as pl
from barca import asset

QUERY = "select sum(amount) from orders"


@asset()
def frame() -> pd.DataFrame:
    return pd.DataFrame({"amount": [1, 2, 3, 4]})


@asset()
def limits() -> dict:
    return {"min": 2}


@asset(inputs={"orders": frame})
def duckdb_over_pandas(orders: pd.DataFrame) -> int:
    return int(duckdb.sql("select sum(amount) from orders").fetchone()[0])


@asset(inputs={"orders": frame})
def duckdb_over_polars(orders: pl.DataFrame) -> int:
    return int(duckdb.sql("select sum(amount) from orders").fetchone()[0])


@asset(inputs={"orders": frame})
def duckdb_unannotated(orders) -> int:
    return int(duckdb.sql("select sum(amount) from orders").fetchone()[0])


@asset(inputs={"orders": frame})
def polars_sql(orders: pl.LazyFrame) -> int:
    return int(pl.sql("select sum(amount) as total from orders").collect()["total"][0])


@asset(inputs={"orders": frame, "threshold": limits})
def pandas_query(orders: pd.DataFrame, threshold: dict) -> int:
    return int(orders.query("amount > @threshold['min']")["amount"].sum())


@asset(inputs={"orders": frame})
def query_from_a_constant(orders: pd.DataFrame) -> int:
    return int(duckdb.sql(QUERY).fetchone()[0])


@asset(inputs={"orders": frame})
def docstring_only(orders: pd.DataFrame) -> int:
    \"\"\"Sums the amount column of orders.\"\"\"
    return 10


@asset(inputs={"orders": frame})
def comment_only(orders: pd.DataFrame) -> int:
    # sums the amount column of orders
    return 10


@asset(inputs={"orders": frame})
def longer_name(orders: pd.DataFrame) -> str:
    return "select sum(amount) from reorders, orders_v2"


@asset(inputs={"orders": frame})
def another_name(orders: pd.DataFrame) -> str:
    return "select sum(amount) from customers"
"""

# Nothing here may be reported: each step is one documented exception or one kind of use.
NOT_REPORTED = """
import builtins
import inspect

import duckdb
from barca import asset, sensor, task


@asset()
def raw() -> dict:
    return {"n": 1}


@asset()
def orders() -> duckdb.DuckDBPyRelation:
    return duckdb.sql("select 1 as n")


@sensor()
def etag() -> tuple[bool, str]:
    return True, "v1"


def helper(value):
    return value["n"]


@asset(inputs={"raw": raw})
def via_helper(raw: dict) -> int:
    return helper(raw)


@asset(inputs={"raw": raw})
def via_closure(raw: dict) -> int:
    def inner():
        return raw["n"]

    return inner()


@asset(inputs={"raw": raw})
def via_locals(raw: dict) -> int:
    return locals()["raw"]["n"]


@asset(inputs={"raw": raw})
def via_builtins_locals(raw: dict) -> int:
    return builtins.locals()["raw"]["n"]


@asset(inputs={"raw": raw})
def via_frame(raw: dict) -> int:
    return inspect.currentframe().f_locals["raw"]["n"]


@asset(inputs={"raw": raw})
def via_eval(raw: dict) -> int:
    return eval("raw")["n"]


@asset(inputs={"raw": raw})
def via_kwargs(**kwargs) -> int:
    return kwargs["raw"]["n"]


@asset(inputs={"orders": orders})
def via_sql(orders: duckdb.DuckDBPyRelation) -> int:
    # Read through the view barca binds under the parameter's name, not the Python name.
    return duckdb.sql("select count(*) from orders").fetchone()[0]


@asset(inputs={"etag": etag})
def cache_trigger(etag: str) -> int:
    return 1


@asset(inputs={"_raw": raw})
def ordering_only(_raw) -> int:
    return 1


@task(inputs={"raw": raw})
def stub(raw: dict) -> None:
    ...


@task(inputs={"raw": raw})
def docstring_only(raw: dict) -> None:
    \"\"\"Not written yet.\"\"\"


@task(inputs={"raw": raw})
def gate(raw: dict) -> None:
    raise RuntimeError("gate closed")
"""


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["BARCA_PROGRESS_SECS"] = "0"
    return subprocess.run(
        [_find_binary(), *args], cwd=cwd, env=env, capture_output=True, text=True, timeout=120
    )


def write(tmp_path: Path, source: str) -> Path:
    (tmp_path / "pipeline.py").write_text(source)
    return tmp_path


def stderr_warnings(proc: subprocess.CompletedProcess) -> list[str]:
    """The text of each `[barca] warning: ...` line on stderr."""
    return [ln[len(PREFIX) :] for ln in proc.stderr.splitlines() if ln.startswith(PREFIX)]


def json_warnings(proc: subprocess.CompletedProcess) -> list[dict]:
    return json.loads(proc.stdout)["warnings"]


def pairs(warnings: list[dict]) -> list[tuple[str, str]]:
    return sorted((w["node"], w["param"]) for w in warnings)


# ─── what is reported ────────────────────────────────────────────────────────


def test_the_warning_names_the_step_the_input_and_the_three_fixes(tmp_path):
    proc = barca(write(tmp_path, PIPELINE), "plan", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    [w] = json_warnings(proc)
    assert set(w) == {"kind", "node", "param", "message"}
    assert (w["kind"], w["node"], w["param"]) == ("unused_input", *REPORT_OTHER)
    assert w["message"] == (
        "pipeline.py:report never uses its input `other`. It is still loaded in full each time "
        "the step runs, and it counts toward the step's cache key. Use it, remove it from "
        "inputs=, or rename the parameter `_other` if it is there for ordering only (a `_` input "
        "is not loaded and never flagged)"
    )
    # The stderr line is the same text, once.
    assert stderr_warnings(proc) == [w["message"]]


def test_each_reported_case_once_per_step_input(tmp_path):
    proc = barca(write(tmp_path, REPORTED), "plan", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    warnings = json_warnings(proc)
    assert pairs(warnings) == [
        ("pipeline.py:dropped", "raw"),  # `del raw` is not a use
        ("pipeline.py:fan", "raw"),  # a parallel() caller is judged like any body
        ("pipeline.py:lazy_polars", "raw"),
        ("pipeline.py:per_key", "raw"),  # three partition keys, one warning
    ]
    assert sorted(stderr_warnings(proc)) == sorted(w["message"] for w in warnings)


def test_a_lazy_input_is_not_said_to_be_loaded(tmp_path):
    proc = barca(write(tmp_path, REPORTED), "get", "lazy_polars", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    [w] = json_warnings(proc)
    assert "loaded in full" not in w["message"]
    assert "annotated as a lazy input (polars_lazy)" in w["message"]
    assert "opened but not read" in w["message"]
    # The eager one in the same file is.
    [eager] = json_warnings(barca(tmp_path, "get", "dropped", "pipeline.py", "--json"))
    assert "loaded in full each time the step runs" in eager["message"]


def test_nothing_is_reported_for_uses_the_analysis_cannot_see_and_documented_exceptions(tmp_path):
    proc = barca(write(tmp_path, NOT_REPORTED), "plan", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    assert json_warnings(proc) == []
    assert stderr_warnings(proc) == []
    # And they all really run: the dynamic ones do read the input.
    for target, value in [
        ("via_helper", 1),
        ("via_closure", 1),
        ("via_locals", 1),
        ("via_builtins_locals", 1),
        ("via_frame", 1),
        ("via_eval", 1),
        ("via_kwargs", 1),
        ("via_sql", 1),
        ("cache_trigger", 1),
        ("ordering_only", 1),
    ]:
        run = barca(tmp_path, "get", target, "pipeline.py", "--json")
        assert run.returncode == 0, (target, run.stderr)
        out = json.loads(run.stdout)
        assert (out["final_output"], out["warnings"]) == (value, []), target
        assert stderr_warnings(run) == [], target


def test_an_input_read_by_name_from_a_string_is_used(tmp_path):
    """SQL and expression strings read variables by name: DuckDB replacement scans, `pl.sql`,
    pandas `query("@name")`. Each step returns the right value and nothing is printed."""
    cwd = write(tmp_path, BY_NAME)
    for target, value in [
        ("duckdb_over_pandas", 10),
        ("duckdb_over_polars", 10),
        ("duckdb_unannotated", 10),
        ("polars_sql", 10),
        ("pandas_query", 7),
        ("query_from_a_constant", 10),  # the query is not in the body: barca cannot tell
    ]:
        run = barca(cwd, "get", target, "pipeline.py", "--json")
        assert run.returncode == 0, (target, run.stderr)
        out = json.loads(run.stdout)
        assert (out["final_output"], out["warnings"]) == (value, []), target
        assert stderr_warnings(run) == [], target


def test_the_string_rule_does_not_hide_an_input_that_is_only_described(tmp_path):
    """A docstring, a comment, a longer identifier or another name is not a mention."""
    proc = barca(write(tmp_path, BY_NAME), "plan", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    assert pairs(json_warnings(proc)) == [
        ("pipeline.py:another_name", "orders"),
        ("pipeline.py:comment_only", "orders"),
        ("pipeline.py:docstring_only", "orders"),
        ("pipeline.py:longer_name", "orders"),
    ]
    assert len(stderr_warnings(proc)) == 4


def test_the_underscore_prefix_declares_an_input_unused_on_purpose(tmp_path):
    """The documented way to keep an input you do not read: the warning goes away, the step
    still runs after the upstream, receives None, and still re-runs when the upstream changes."""
    source = PIPELINE.replace('"other": other}', '"_other": other}').replace(
        'def report(raw: dict, other: dict) -> int:\n    return raw["n"]',
        'def report(raw: dict, _other) -> int:\n    return [raw["n"], _other]',
    )
    assert source != PIPELINE
    first = barca(write(tmp_path, source), "get", "report", "pipeline.py", "--json")
    assert first.returncode == 0, first.stderr
    out = json.loads(first.stdout)
    assert out["warnings"] == [] and stderr_warnings(first) == []
    assert out["final_output"] == [1, None]
    assert [s["id"] for s in out["steps"]].index("pipeline.py:other") < len(out["steps"]) - 1

    write(tmp_path, source.replace('{"m": 2}', '{"m": 3}'))
    second = json.loads(barca(tmp_path, "get", "report", "pipeline.py", "--json").stdout)
    status = {s["id"]: s["status"] for s in second["steps"]}
    assert status["pipeline.py:other"] == "ran"
    assert status["pipeline.py:report"] == "ran"  # still a cache dependency
    assert status["pipeline.py:raw"] == "cached"


# ─── where it is reported: the planned cone, every planning command, same place ──────────────


def test_a_target_whose_cone_has_no_unused_input_reports_nothing(tmp_path):
    cwd = write(tmp_path, PIPELINE)
    for args in (
        ["get", "unrelated", "pipeline.py", "--json"],
        ["get", "unrelated", "pipeline.py", "--dry-run", "--json"],
        ["get", "unrelated,raw", "pipeline.py", "--json"],
        ["get", "unrelated", "pipeline.py", "--agent", "--json"],
    ):
        proc = barca(cwd, *args)
        assert proc.returncode == 0, proc.stderr
        assert json_warnings(proc) == [], args  # the key is there, empty
        assert stderr_warnings(proc) == [], args


@pytest.mark.parametrize(
    "args",
    [
        ["plan", "pipeline.py"],
        ["get", "report", "pipeline.py", "--json"],
        ["get", "report", "pipeline.py", "--dry-run", "--json"],
        ["get", "pipeline.py", "--json"],  # no target: every asset
        ["get", "pipeline.py", "--dry-run", "--json"],
        ["get", "report,unrelated", "pipeline.py", "--json"],
        ["get", "report,unrelated", "pipeline.py", "--dry-run", "--json"],
        ["get", "report", "pipeline.py", "--agent", "--json"],
        ["get", "report", "pipeline.py", "--json", "--fields", "id"],
        ["run", "publish", "pipeline.py", "--json"],  # upstream of the task
        ["run", "publish", "pipeline.py", "--dry-run", "--json"],
        ["run", "publish", "pipeline.py", "--agent", "--json"],
    ],
    ids=lambda a: " ".join(a),
)
def test_every_planning_command_reports_the_same_list_in_both_places(tmp_path, args):
    proc = barca(write(tmp_path, PIPELINE), *args)
    assert proc.returncode == 0, proc.stderr
    warnings = json_warnings(proc)
    assert pairs(warnings) == [REPORT_OTHER]
    assert stderr_warnings(proc) == [w["message"] for w in warnings]


@pytest.mark.parametrize(
    "args",
    [
        ["get", "report", "pipeline.py", "--pretty"],
        ["get", "report", "pipeline.py", "--dry-run", "--pretty"],
        ["get", "report", "pipeline.py", "-o", "value"],
        ["run", "publish", "pipeline.py", "--pretty"],
    ],
    ids=lambda a: " ".join(a),
)
def test_human_output_gets_the_stderr_line_and_stdout_is_unchanged(tmp_path, args):
    proc = barca(write(tmp_path, PIPELINE), *args)
    assert proc.returncode == 0, proc.stderr
    assert len(stderr_warnings(proc)) == 1
    assert "never uses its input `other`" in stderr_warnings(proc)[0]
    assert "never uses" not in proc.stdout


def test_a_cached_step_is_reported_on_every_run(tmp_path):
    """The list depends on the source and the target only, never on what is cached."""
    cwd = write(tmp_path, PIPELINE)
    first = barca(cwd, "get", "report", "pipeline.py", "--json")
    second = barca(cwd, "get", "report", "pipeline.py", "--json")
    assert json.loads(second.stdout)["steps_executed"] == 0
    assert json_warnings(second) == json_warnings(first) != []
    assert stderr_warnings(second) == stderr_warnings(first)


# ─── it is only a warning ────────────────────────────────────────────────────


def test_a_failed_run_keeps_exit_1_and_still_carries_the_warnings(tmp_path):
    cwd = write(tmp_path, PIPELINE)
    proc = barca(cwd, "run", "fails", "pipeline.py", "--json")
    assert proc.returncode == 1
    out = json.loads(proc.stdout)
    assert out["status"] == "failed" and out["failed_node"] == "pipeline.py:fails"
    assert pairs(out["warnings"]) == [REPORT_OTHER]
    assert len(stderr_warnings(proc)) == 1
    # The error envelope is still the last stderr line, and has no warnings in it.
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "step_failed" and "warnings" not in envelope

    multi = barca(cwd, "run", "publish,fails", "pipeline.py", "--json")
    assert multi.returncode == 1
    out = json.loads(multi.stdout)
    assert out["status"] == "failed"
    assert pairs(out["warnings"]) == [REPORT_OTHER]


def test_a_usage_error_keeps_exit_2_and_prints_no_warning(tmp_path):
    proc = barca(write(tmp_path, PIPELINE), "get", "nope", "pipeline.py", "--json")
    assert proc.returncode == 2
    assert stderr_warnings(proc) == []
    assert proc.stdout == ""


def test_commands_that_do_not_plan_a_run_report_nothing(tmp_path):
    cwd = write(tmp_path, PIPELINE)
    for args in (["list", "pipeline.py", "--json"], ["status", "pipeline.py", "--json"]):
        proc = barca(cwd, *args)
        assert proc.returncode == 0, proc.stderr
        assert stderr_warnings(proc) == []
        assert "warnings" not in json.loads(proc.stdout)


def manual_list(topic: str, label: str) -> list[str]:
    line = next(ln for ln in topic.splitlines() if ln.strip().startswith(label))
    return [n.strip("` .") for n in line.split(":", 1)[1].split(",")]


def test_the_manual_lists_exactly_the_names_the_check_treats_specially(tmp_path):
    """The lists live in one place, `unused_inputs.rs`; the manual repeats them. Each name
    alone silences the check, and the same step without it is reported."""
    topic = barca(tmp_path, "docs", "assets").stdout
    dynamic = manual_list(topic, "Dynamic access names:")
    assert dynamic == [
        "locals",
        "vars",
        "eval",
        "exec",
        "currentframe",
        "_getframe",
        "f_locals",
        "f_back",
        "getargvalues",
        "inspect.stack",
    ]
    entry_points = manual_list(topic, "Query entry points:")
    assert entry_points == [
        "sql",
        "execute",
        "executemany",
        "query",
        "from_query",
        "read_sql",
        "read_sql_query",
        "SQLContext",
    ]
    bodies = [f"return {n}" if "." in n else f"return thing.{n}" for n in dynamic]
    bodies += [f"return thing.{n}(text)" for n in entry_points]
    steps = "".join(
        f'\n@asset(inputs={{"raw": raw}})\ndef s{i}(raw):\n    {body}\n'
        for i, body in enumerate(bodies)
    )
    source = "import inspect\nfrom barca import asset\n\n@asset()\ndef raw():\n    return 1\n"
    proc = barca(write(tmp_path, source + steps), "plan", "pipeline.py")
    assert proc.returncode == 0, proc.stderr
    assert json_warnings(proc) == []
    # Without the name, or with a query barca can read, the same steps are reported.
    changed = steps.replace("thing.locals", "thing.other").replace(
        "thing.sql(text)", 'thing.sql("select 1")'
    )
    proc = barca(write(tmp_path, source + changed), "plan", "pipeline.py")
    assert pairs(json_warnings(proc)) == [("pipeline.py:s0", "raw"), ("pipeline.py:s10", "raw")]
