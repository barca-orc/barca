"""`barca.on_step_start`: callbacks the worker runs before each step, with its node id and weight."""

import json
import os
import textwrap

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


def cli(root, *args, code=0):
    proc = barca(root, *args, env={**os.environ, "BARCA_REMOTE": "off", "BARCA_CAPACITY": "4"})
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
