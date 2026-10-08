"""What a `parallel()` branch returns reaches the step that called it (#285).

A branch's return value travels like a step's output: its worker writes it as an artifact
(json, pickle or parquet, by type) and the caller reads it from there. Before, the coordinator
read the artifact and passed the value inline, which it could do for JSON only: a set, a date
or a DataFrame reached the caller as `None`, with no error and a successful run.

Every test runs the real binary. The pipeline describes each value it received (type, repr,
whether it equals what the branch built), so the assertions are about what the calling step
saw, not about what a file contains.
"""

import json
import os
import signal
import subprocess
import textwrap
from pathlib import Path

import pytest

from barca.api import _find_binary

pytest.importorskip("duckdb")
pytest.importorskip("numpy")
pytest.importorskip("pandas")
pytest.importorskip("polars")
pytest.importorskip("pyarrow")

PIPELINE = '''
import dataclasses
import datetime
import decimal
from functools import partial

from barca import ParallelError, asset, parallel, parallel_map, task


@dataclasses.dataclass
class Point:
    x: int
    y: int


class Custom:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, Custom) and other.v == self.v


def make(case):
    if case == "set":
        return {1, 2, 3}
    if case == "frozenset":
        return frozenset({1, 2})
    if case == "date":
        return datetime.date(2026, 1, 2)
    if case == "datetime":
        return datetime.datetime(2026, 1, 2, 3, 4, 5)
    if case == "decimal":
        return decimal.Decimal("1.10")
    if case == "bytes":
        return b"\\x00\\x01abc"
    if case == "tuple":
        return (1, "a", 2.5)
    if case == "tuple_in_set":
        return {(1, 2), (3, 4)}
    if case == "intkeys":
        return {1: "a", 2: "b"}
    if case == "dataclass":
        return Point(1, 2)
    if case == "custom":
        return Custom(7)
    if case == "numpy":
        import numpy as np

        return np.arange(6).reshape(2, 3)
    if case == "pandas":
        import pandas as pd

        return pd.DataFrame({"a": [1, 2], "b": ["x", "y"]})
    if case == "polars":
        import polars as pl

        return pl.DataFrame({"a": [1, 2], "b": ["x", "y"]})
    if case == "polars_lazy":
        import polars as pl

        return pl.LazyFrame({"a": [1, 2], "b": ["x", "y"]})
    if case == "pyarrow":
        import pyarrow as pa

        return pa.table({"a": [1, 2], "b": ["x", "y"]})
    if case == "duckdb":
        import duckdb

        return duckdb.sql("select 1 as a, 'x' as b union all select 2, 'y' order by a")
    if case == "none":
        return None
    if case == "nan":
        return float("nan")
    if case == "bigint":
        return 10**30
    if case == "keyorder":
        return {"b": 1, "a": 2}
    if case == "dict":
        return {"i": 1}
    if case == "large":
        return list(range(400_000))
    raise KeyError(case)


CASES = [
    "set", "frozenset", "date", "datetime", "decimal", "bytes", "tuple", "tuple_in_set",
    "intkeys", "dataclass", "custom", "numpy", "pandas", "polars", "polars_lazy", "pyarrow",
    "duckdb", "none", "nan", "bigint", "keyorder", "dict", "large",
]


def same(got, case):
    """Whether `got` equals what the branch built, type included."""
    want = make(case)
    if case == "numpy":
        return type(got) is type(want) and bool((got == want).all())
    if case in ("pandas", "polars", "pyarrow"):
        return type(got) is type(want) and bool(got.equals(want))
    if case == "polars_lazy":
        return type(got) is type(want) and bool(got.collect().equals(want.collect()))
    if case == "duckdb":
        return type(got) is type(want) and got.order("a").fetchall() == want.fetchall()
    if case == "nan":
        return isinstance(got, float) and got != got
    if case == "keyorder":
        return got == want and list(got) == list(want)
    return type(got) is type(want) and got == want


def describe(got, case):
    r = repr(got)
    return {
        "case": case,
        "type": f"{type(got).__module__}.{type(got).__qualname__}".replace("_barca_", ""),
        "repr": r if len(r) < 40 else r[:37] + "...",
        "same": same(got, case),
    }


@task()
def branch(case: str):
    return make(case)


@task()
def boom(case: str):
    raise ValueError("boom")


@task()
def nested(case: str):
    return parallel(partial(branch, case))[0]


def fan(mode):
    if mode == "map":
        results = parallel_map(branch, CASES)
    elif mode == "nested":
        results = parallel(*(partial(nested, c) for c in CASES))
    else:
        results = parallel(*(partial(branch, c) for c in CASES))
    return {c: describe(r, c) for r, c in zip(results, CASES)}


@task()
def task_parallel() -> dict:
    return fan("parallel")


@task()
def task_map() -> dict:
    return fan("map")


@task()
def task_nested() -> dict:
    return fan("nested")


@asset()
def asset_parallel() -> dict:
    return fan("parallel")


@asset()
def asset_map() -> dict:
    return fan("map")


@task()
def none_and_failure() -> list:
    ok, failed = parallel(partial(branch, "none"), partial(boom, "x"))
    return [ok is None, isinstance(failed, ParallelError), str(failed).splitlines()[0]]
'''

# Read from disk by the next step, a JSON-serializable value is what JSON gives back.
JSON_ROUND_TRIP = {
    "tuple": ("builtins.list", "[1, 'a', 2.5]"),
    "intkeys": ("builtins.dict", "{'1': 'a', '2': 'b'}"),
}


def run(root: Path, command: str, target: str, *extra: str, pool: int = 4, store: bool = False):
    """`barca <command> <target> <file>` as a job of its own, stopped as a whole if it hangs."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["BARCA_POOL_SIZE"] = str(pool)
    if store:
        (root / "store").mkdir(exist_ok=True)
        env["BARCA_REMOTE_URI"] = str(root / "store")
    proc = subprocess.Popen(
        [_find_binary(), command, target, *extra],
        cwd=root,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        out, err = proc.communicate(timeout=180)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
        raise AssertionError(f"barca did not finish\n{err}") from None
    return proc.returncode, out, err


@pytest.fixture
def root(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def received(root: Path, target: str, **how) -> dict:
    code, out, err = run(
        root, "run" if target.startswith("task") else "get", target, "pipeline.py", **how
    )
    assert code == 0, err
    return json.loads(out)["final_output"]


def assert_intact(seen: dict) -> None:
    assert sorted(seen) == sorted(CASES)
    for case, got in seen.items():
        if case in JSON_ROUND_TRIP:
            assert (got["type"], got["repr"]) == JSON_ROUND_TRIP[case], got
        else:
            assert got["same"] is True, got
        assert got["type"] != "builtins.NoneType" or case == "none", got


@pytest.mark.parametrize("pool", [1, 4])
@pytest.mark.parametrize(
    "target", ["task_parallel", "task_map", "task_nested", "asset_parallel", "asset_map"]
)
def test_a_branch_may_return_whatever_a_step_may_return(root, target, pool):
    """From a task and from an asset, through parallel and parallel_map, one level down and
    two, with one worker and with several: every value arrives equal and of the type the
    branch returned. Two JSON-serializable values arrive as JSON gives them back."""
    assert_intact(received(root, target, pool=pool))


@pytest.mark.parametrize("target", ["task_parallel", "asset_map"])
def test_branch_results_arrive_with_a_remote_store_configured(root, target):
    """RFC-0005 §4.3 said the caller gets `null` results with a warning under a remote
    store. Branch artifacts are local whatever the store, and are read where they are."""
    code, out, err = run(root, "run" if target.startswith("task") else "get", target,
                         "pipeline.py", store=True)  # fmt: skip
    assert code == 0, err
    assert_intact(json.loads(out)["final_output"])
    assert "null" not in err and "warning" not in err.lower(), err
    # Branch artifacts are not uploaded: only the caller's own result is a step of the run.
    stored = [p.name for p in (root / "store").rglob("*") if "_branch_" in p.name]
    assert stored == []


def test_none_is_a_value_and_a_failed_branch_is_a_parallel_error(root):
    assert received(root, "task_parallel")["none"]["same"] is True
    code, out, err = run(root, "run", "none_and_failure", "pipeline.py")
    assert code == 0, err
    assert json.loads(out)["final_output"] == [True, True, "ValueError: boom"]


HANDOFF = """
import polars as pl

from barca import asset
from pipeline import CASES, describe, make

"""


def handoff_module() -> str:
    """One producer and one consumer asset per case, and a consumer annotated for polars."""
    src = HANDOFF
    for case in json.loads(json.dumps(CASES)):
        src += textwrap.dedent(
            f"""
            @asset()
            def p_{case}():
                return make("{case}")


            @asset(inputs={{"x": p_{case}}})
            def c_{case}(x) -> dict:
                return describe(x, "{case}")

            """
        )
    src += textwrap.dedent(
        """
        @asset(inputs={"x": p_polars})
        def c_polars_annotated(x: pl.DataFrame) -> dict:
            return describe(x, "polars")
        """
    )
    return src


CASES = [
    "set", "frozenset", "date", "datetime", "decimal", "bytes", "tuple", "tuple_in_set",
    "intkeys", "dataclass", "custom", "numpy", "pandas", "polars", "polars_lazy", "pyarrow",
    "duckdb", "none", "nan", "bigint", "keyorder", "dict", "large",
]  # fmt: skip


def test_a_branch_result_arrives_like_a_step_input_read_from_disk(root):
    """The reference is what a normal hand-off does: an asset reading another asset's output
    from its artifact. (Each consumer is run on its own against a cached producer, so it reads
    the file: a consumer that runs in the worker that produced the value gets that worker's
    in-memory copy instead, which is not a round trip.)

    Branch results are the same, value for value. The one difference is by design: a step
    picks its frame reader from its parameter's annotation (pandas without one), and the
    caller of parallel() has no annotation for a result, so it gets the frame type the branch
    returned."""
    (root / "handoff.py").write_text(handoff_module())
    consumers = [f"c_{case}" for case in CASES] + ["c_polars_annotated"]
    code, _, err = run(root, "get", ",".join(f"p_{case}" for case in CASES), "handoff.py")
    assert code == 0, err

    from_disk = {}
    for consumer in consumers:
        code, out, err = run(root, "get", consumer, "handoff.py")
        assert code == 0, err
        doc = json.loads(out)
        # The producer was served from cache: the consumer read its artifact.
        assert doc["steps_executed"] == 1, doc["steps"]
        from_disk[consumer[2:]] = doc["final_output"]

    branch = received(root, "task_parallel")
    frames_by_annotation = {"polars", "polars_lazy", "pyarrow", "duckdb"}
    for case in CASES:
        step, got = from_disk[case], branch[case]
        if case in frames_by_annotation:
            # Without an annotation a step reads any parquet artifact as pandas.
            assert step["type"] == "pandas.DataFrame", step
            assert got["same"] is True, got
        else:
            assert (got["type"], got["same"]) == (step["type"], step["same"]), (step, got)
            if case in JSON_ROUND_TRIP:
                assert got["repr"] == step["repr"] == JSON_ROUND_TRIP[case][1]
            else:
                assert got["same"] is True, got
    # Annotated for polars, the step gets what the branch's caller gets.
    assert from_disk["polars_annotated"]["type"] == branch["polars"]["type"]
    assert from_disk["polars_annotated"]["same"] is True


UNPASSABLE = """
from functools import partial

from barca import asset, parallel, task


@task()
def handle(i: int):
    return open(__file__)


@task()
def function(i: int):
    return lambda x: x


@task()
def fine(i: int):
    return {1, 2}


def _explode():
    raise ValueError("cannot be rebuilt here")


class WritesButDoesNotRead:
    def __reduce__(self):
        return (_explode, ())


@task()
def unreadable(i: int):
    return WritesButDoesNotRead()


@task()
def task_handle() -> list:
    return [repr(r) for r in parallel(partial(fine, 0), partial(handle, 1))]


@task()
def task_two() -> list:
    return [repr(r) for r in parallel(partial(function, 0), partial(fine, 1), partial(handle, 2))]


@asset()
def asset_handle() -> list:
    return [repr(r) for r in parallel(partial(fine, 0), partial(handle, 1))]


@task()
def task_unreadable() -> list:
    return [repr(r) for r in parallel(partial(fine, 0), partial(unreadable, 1))]
"""


def failed_with(root: Path, target: str, pool: int = 4) -> str:
    (root / "pipeline.py").write_text(UNPASSABLE)
    code, out, err = run(
        root, "run" if target.startswith("task") else "get", target, "pipeline.py", pool=pool
    )
    assert code == 1, (code, out, err)
    doc = json.loads(out)
    assert doc["status"] == "failed" and "final_output" not in doc, doc
    assert doc["failed_node"] == f"pipeline.py:{target}"
    envelope = json.loads(err.strip().splitlines()[-1])
    assert envelope["kind"] == "step_failed" and envelope["node"] == f"pipeline.py:{target}"
    assert doc["error"] in envelope["error"]
    return doc["error"]


@pytest.mark.parametrize("pool", [1, 4])
@pytest.mark.parametrize("target", ["task_handle", "asset_handle"])
def test_a_value_that_cannot_be_written_fails_the_calling_step(root, target, pool):
    """Not `None`, and not a successful run: the caller raises, and the run reports the
    calling step as failed with the branch, the type and the reason."""
    error = failed_with(root, target, pool)
    assert error.startswith("BranchResultError: parallel() branch 1: pipeline.py:handle"), error
    assert "returned a _io.TextIOWrapper" in error
    assert "TypeError: cannot pickle" in error


def test_the_first_unpassable_branch_is_named_and_the_others_counted(root):
    error = failed_with(root, "task_two")
    assert "parallel() branch 0: pipeline.py:function returned a builtins.function" in error
    assert error.endswith("(and 1 more)")


def test_a_value_that_cannot_be_read_back_fails_the_calling_step(root):
    error = failed_with(root, "task_unreadable")
    assert error.startswith("BranchResultError: parallel() branch 1: the value "), error
    assert "pipeline.py:unreadable returned" in error
    assert "could not be read back: ValueError: cannot be rebuilt here" in error
    assert ".pkl (pickle)" in error


def test_a_class_from_a_module_the_caller_imports_by_name(tmp_path):
    """The branch's worker loads the branch's file under barca's own module name, and a
    pickled object names that module. The calling worker knows the file as `helpers`."""
    (tmp_path / "helpers.py").write_text(
        textwrap.dedent(
            """
            from barca import task


            class Thing:
                def __init__(self, v):
                    self.v = v


            @task()
            def make_thing(v: int):
                return Thing(v)
            """
        )
    )
    (tmp_path / "pipeline.py").write_text(
        textwrap.dedent(
            """
            from functools import partial

            from barca import parallel, task
            from helpers import make_thing


            @task()
            def caller() -> list:
                things = parallel(partial(make_thing, 1), partial(make_thing, 2))
                return [[type(t).__name__, t.v] for t in things]
            """
        )
    )
    code, out, err = run(tmp_path, "run", "caller", "pipeline.py")
    assert code == 0, err
    assert json.loads(out)["final_output"] == [["Thing", 1], ["Thing", 2]]


# ─── the calling worker's side, without a coordinator ────────────────────────


def test_collect_reads_artifacts_and_raises_for_a_lost_value(tmp_path):
    from barca import BranchResultError, ParallelError, _branches
    from barca._artifacts import serialize

    serialize({1, 2}, tmp_path / "a.pkl", "pickle")
    serialize({"b": 1, "a": 2}, tmp_path / "b.json", "json")
    items = [{"fn_ref": "/p/pipeline.py:work"}] * 3
    ok = [
        {"status": "ok", "artifact": {"path": str(tmp_path / "a.pkl"), "format": "pickle"}},
        {"status": "ok", "artifact": {"path": str(tmp_path / "b.json"), "format": "json"}},
        {"status": "error", "error": "ValueError: boom"},
    ]
    a, b, failed = _branches.collect(ok, items)
    assert a == {1, 2} and list(b) == ["b", "a"]
    assert isinstance(failed, ParallelError) and failed.error == "ValueError: boom"

    lost = [ok[0], {"status": "error", "error": f"{_branches.UNRETURNABLE}: work returned X\n tb"}]
    with pytest.raises(BranchResultError, match=r"^parallel\(\) branch 1: work returned X$"):
        _branches.collect(lost, items)

    missing = [{"status": "ok", "artifact": {"path": str(tmp_path / "no.json"), "format": "json"}}]
    with pytest.raises(BranchResultError, match="branch 0: the value pipeline.py:work returned"):
        _branches.collect(missing, items)


def test_collect_accepts_inline_values_from_an_older_coordinator():
    """A barca binary from before this change answers with JSON values inline."""
    from barca import _branches

    answer = [{"status": "ok", "result": {"i": 1}}, {"status": "ok", "result": None}]
    assert _branches.collect(answer, [{"fn_ref": "p.py:f"}] * 2) == [{"i": 1}, None]


def test_the_submit_message_asks_for_artifact_results(monkeypatch):
    from barca import _runtime

    sent = []
    monkeypatch.setattr(_runtime, "send_message", sent.append)
    monkeypatch.setattr(
        _runtime, "recv_message", lambda: {"type": "parallel_response", "results": []}
    )
    assert _runtime.submit_and_wait([]) == []
    assert sent == [{"type": "submit", "items": [], "artifact_results": True}]
