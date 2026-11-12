"""`barca.on_step_start`: callbacks the worker runs before each step, with its node id and weight."""

import json
import os
import re
import textwrap
from pathlib import Path

import pytest

from barca import _hooks

from .helpers import barca

PIPELINE = """
from pathlib import Path

import barca
from barca import asset

LOG = Path(__file__).parent / "starts.txt"


@barca.on_step_start
def record(step):
    with LOG.open("a") as f:
        f.write(f"{step.node_id.split(':')[-1]} {step.weight}\\n")


@asset(weight=3)
def heavy() -> int:
    return 1


@asset
def light() -> int:
    return 2


@asset(inputs={"a": heavy, "b": light})
def total(a: int, b: int) -> int:
    return a + b
"""


def cli(root, *args, code=0, **env):
    proc = barca(
        root, *args, env={**os.environ, "BARCA_REMOTE": "off", "BARCA_CAPACITY": "4", **env}
    )
    assert proc.returncode == code, proc.stderr
    return proc


def test_every_step_starts_with_its_weight(tmp_path):
    (tmp_path / "p.py").write_text(textwrap.dedent(PIPELINE))

    got = json.loads(cli(tmp_path, "get", "total", "p.py", "-o", "value", "--json").stdout)

    assert got["final_output"] == 3
    starts = (tmp_path / "starts.txt").read_text().splitlines()
    assert sorted(starts) == ["heavy 3", "light 1", "total 1"]
    # `total` reads the other two, so it starts last.
    assert starts[-1] == "total 1"


KEYS_AND_BRANCHES = """
from functools import partial
from pathlib import Path

import barca
from barca import asset, parallel, partitions, task

LOG = Path(__file__).parent / "starts.txt"


@barca.on_step_start
def record(step):
    with LOG.open("a") as f:
        f.write(f"{step.node_id.split(':')[-1]} {step.weight}\\n")


@asset(partitions={"k": partitions(["a", "b"])}, weight=2)
def per_key(k: str) -> str:
    return k


@task()
def branch(i: int) -> int:
    return i


@task(weight=3)
def fan_out() -> list:
    return parallel(*[partial(branch, i) for i in range(2)])
"""


def test_partition_keys_and_parallel_branches_start_with_their_nodes_weight(tmp_path):
    (tmp_path / "p.py").write_text(textwrap.dedent(KEYS_AND_BRANCHES))

    cli(tmp_path, "get", "per_key", "p.py")
    assert sorted((tmp_path / "starts.txt").read_text().splitlines()) == [
        "per_key[k=a] 2",
        "per_key[k=b] 2",
    ]

    (tmp_path / "starts.txt").unlink()
    cli(tmp_path, "run", "fan_out", "p.py")
    starts = (tmp_path / "starts.txt").read_text().splitlines()
    # The caller starts first; each branch starts with the caller's weight.
    assert starts[0] == "fan_out 3"
    assert len(starts) == 3 and all(line.endswith(" 3") for line in starts)


def test_a_batched_call_starts_once_for_all_its_keys(tmp_path):
    (tmp_path / "p.py").write_text(
        textwrap.dedent(
            """
            from pathlib import Path

            import barca
            from barca import asset, partitions

            LOG = Path(__file__).parent / "starts.txt"


            @barca.on_step_start
            def record(step):
                with LOG.open("a") as f:
                    f.write(f"{step.weight}\\n")


            @asset(partitions={"k": partitions(["a", "b", "c"])}, batch="all", weight=2)
            def keys(k: list[str]) -> dict:
                return {key: key for key in k}
            """
        )
    )

    cli(tmp_path, "get", "keys", "p.py")

    assert (tmp_path / "starts.txt").read_text().splitlines() == ["2"]


def test_cached_steps_start_nothing(tmp_path):
    (tmp_path / "p.py").write_text(textwrap.dedent(PIPELINE))
    cli(tmp_path, "get", "total", "p.py")
    (tmp_path / "starts.txt").unlink()

    cli(tmp_path, "get", "total", "p.py")

    assert not (tmp_path / "starts.txt").exists()


def test_a_callback_that_raises_fails_the_step(tmp_path):
    (tmp_path / "p.py").write_text(
        textwrap.dedent(
            """
            import barca
            from barca import asset


            @barca.on_step_start
            def refuse(step):
                raise RuntimeError(f"no room for {step.node_id}")


            @asset
            def a() -> int:
                return 1
            """
        )
    )

    proc = cli(tmp_path, "get", "a", "p.py", code=1)

    assert "no room for p.py:a" in proc.stderr


def write(root, body):
    (root / "p.py").write_text(textwrap.dedent(body))


def test_callbacks_run_in_order_and_print_to_the_steps_log(tmp_path):
    write(
        tmp_path,
        """
        import barca
        from barca import asset


        @barca.on_step_start
        def first(step):
            print(f"first {step.node_id}")


        @barca.on_step_start
        def second(step):
            print(f"second {step.node_id}")


        @asset
        def a() -> int:
            return 1
        """,
    )

    cli(tmp_path, "get", "a", "p.py")
    log = cli(tmp_path, "logs", "--step", "a", "--pretty").stdout

    assert log.index("first p.py:a") < log.index("second p.py:a")


def test_the_worker_runs_its_next_step_after_a_callback_raises(tmp_path):
    write(
        tmp_path,
        """
        import os
        from pathlib import Path

        import barca
        from barca import asset

        PIDS = Path(__file__).parent / "pids.txt"


        @barca.on_step_start
        def refuse_a(step):
            with PIDS.open("a") as f:
                f.write(f"{step.node_id} {os.getpid()}\\n")
            if step.node_id.endswith(":a"):
                raise RuntimeError("no room for a")


        @asset
        def a() -> int:
            return 1


        @asset
        def b() -> int:
            return 2
        """,
    )

    proc = cli(tmp_path, "get", "a,b", "p.py", "--json", code=1, BARCA_POOL_SIZE="1")

    steps = {s["id"]: s["status"] for s in json.loads(proc.stdout)["steps"]}
    assert steps["p.py:a"] == "failed" and steps["p.py:b"] == "ran"
    pids = {line.split()[1] for line in (tmp_path / "pids.txt").read_text().splitlines()}
    assert len(pids) == 1


INPUT_ORDER = """
from pathlib import Path

import barca
from barca import asset

LOG = Path(__file__).parent / "order.txt"


def note(line):
    with LOG.open("a") as f:
        f.write(line + "\\n")


class Loaded:
    def __init__(self):
        self.rows = 1  # pickle calls __setstate__ only for a non-empty state

    def __setstate__(self, state):
        note("input read")


@barca.on_step_start
def start(step):
    note(f"start {step.node_id.split(':')[-1]}")
    if step.node_id.endswith(":down") and REFUSE:
        raise RuntimeError("no room for down")


@asset(serializer="pickle")
def up() -> Loaded:
    return Loaded()


@asset(inputs={"value": up})
def down(value: Loaded) -> int:
    return 1
"""


@pytest.mark.parametrize("refuse", [False, True])
def test_the_callback_runs_before_the_inputs_are_read(tmp_path, refuse):
    write(tmp_path, f"REFUSE = {refuse}\n" + INPUT_ORDER)

    # `up` first, in a run of its own, so `down`'s worker reads it from disk.
    cli(tmp_path, "get", "up", "p.py")
    (tmp_path / "order.txt").unlink()

    cli(tmp_path, "get", "down", "p.py", code=1 if refuse else 0)

    order = (tmp_path / "order.txt").read_text().splitlines()
    assert order == (["start down"] if refuse else ["start down", "input read"])


def test_the_steps_timeout_covers_the_callback(tmp_path):
    write(
        tmp_path,
        """
        import time

        import barca
        from barca import asset


        @barca.on_step_start
        def stall(step):
            time.sleep(5)


        @asset(timeout_seconds=1)
        def a() -> int:
            return 1
        """,
    )

    proc = cli(tmp_path, "get", "a", "p.py", code=1)

    assert "exceeded timeout" in proc.stderr


MANUAL = Path(__file__).resolve().parents[2] / "crates/barca-cli/docs/assets.md"


def test_the_manuals_example_sizes_each_step_by_its_weight(tmp_path):
    body = MANUAL.read_text().split("barca.on_step_start`", 1)[1]
    example = re.search(r"```python\n(.*?)```", body, re.S).group(1)
    probe = """
import duckdb


@asset(weight=4)
def heavy_limit() -> str:
    return duckdb.sql("select current_setting('memory_limit')").fetchall()[0][0]


@asset
def light_limit() -> str:
    return duckdb.sql("select current_setting('memory_limit')").fetchall()[0][0]
"""
    (tmp_path / "p.py").write_text(example + probe)

    heavy = json.loads(cli(tmp_path, "get", "heavy_limit", "p.py", "-o", "value", "--json").stdout)
    light = json.loads(cli(tmp_path, "get", "light_limit", "p.py", "-o", "value", "--json").stdout)

    assert heavy["final_output"] == "16.0 GiB"
    assert light["final_output"] == "4.0 GiB"


def test_registering_twice_registers_once(monkeypatch):
    monkeypatch.setattr(_hooks, "_callbacks", [])
    seen = []

    def record(step):
        seen.append((step.node_id, step.weight))

    _hooks.on_step_start(record)
    _hooks.on_step_start(record)
    _hooks.run_step_start({"node_id": "p.py:a"})
    _hooks.run_step_start({"node_id": "p.py:b", "weight": 4})

    assert seen == [("p.py:a", 1), ("p.py:b", 4)]


def test_only_a_function_registers(monkeypatch):
    monkeypatch.setattr(_hooks, "_callbacks", [])
    with pytest.raises(TypeError, match="function of one StepStart"):
        _hooks.on_step_start("not a function")
