"""`inputs=` keys and function parameters are checked when the file is read (#549).

The worker calls a step as `fn(**kwargs)`. Until 0.28 an `inputs=` key that is not a parameter,
or a parameter that nothing fills, failed as a `TypeError` in the worker after every upstream
had run, and `barca list` and `--dry-run` said nothing. Now both are a usage error (exit 2) on
every command that reads the file, before any step runs. The parser cases are in
`crates/barca-core/src/input_params.rs`; this file tests what a user sees.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary

MISSPELT = """\
from barca import asset


@asset()
def raw() -> list:
    return [1, 2]


@asset(inputs={"row": raw})
def amounts(rows: list) -> list:
    return rows
"""

FORGOTTEN = """\
from barca import asset


@asset()
def raw() -> list:
    return [1, 2]


@asset()
def amounts(
    rows: list,
) -> list:
    return rows
"""


def barca_cmd(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=cwd, check=False, capture_output=True, text=True, timeout=120
    )


def envelope(proc: subprocess.CompletedProcess) -> dict:
    return json.loads(
        next(line for line in reversed(proc.stderr.splitlines()) if line.startswith("{"))
    )


def test_a_misspelt_input_key_names_the_line_the_key_and_the_parameters(tmp_path: Path) -> None:
    (tmp_path / "pipeline.py").write_text(MISSPELT)
    proc = barca_cmd(tmp_path, "list", "pipeline.py", "--json")
    assert proc.returncode == 2, proc.stderr
    assert proc.stdout == ""
    assert envelope(proc) == {
        "code": 2,
        "kind": "usage",
        "error": "Parse error: pipeline.py:amounts (line 9): the inputs= key `row` is not a parameter: "
        "amounts() takes `rows`. Did you mean `rows`?",
        "remediation": "Rename the key to `rows`, or add a parameter `row` to amounts(). Barca passes "
        "each inputs= key to the function as a keyword argument. See `barca docs assets`.",
    }


def test_a_forgotten_input_names_the_parameter_and_its_line(tmp_path: Path) -> None:
    (tmp_path / "pipeline.py").write_text(FORGOTTEN)
    proc = barca_cmd(tmp_path, "get", "amounts", "pipeline.py", "--json")
    assert proc.returncode == 2, proc.stderr
    assert envelope(proc) == {
        "code": 2,
        "kind": "usage",
        "error": "Parse error: pipeline.py:amounts (line 11): the parameter `rows` of amounts() has no "
        "input and no default: barca passes it no arguments, so calling it would fail.",
        "remediation": 'Wire it with `inputs={"rows": <upstream>}`, give it a default (`rows=...`), '
        "or remove it. See `barca docs assets`.",
    }


@pytest.mark.parametrize("source", [MISSPELT, FORGOTTEN])
@pytest.mark.parametrize(
    "args",
    [
        ("list", "pipeline.py"),
        ("plan", "pipeline.py"),
        ("get", "pipeline.py"),
        ("get", "raw", "pipeline.py", "--dry-run"),
        ("get", "amounts", "pipeline.py"),
        ("run", "amounts", "pipeline.py"),
    ],
)
def test_every_command_that_plans_the_file_exits_2_and_runs_nothing(
    tmp_path: Path, source: str, args: tuple[str, ...]
) -> None:
    (tmp_path / "pipeline.py").write_text(source)
    proc = barca_cmd(tmp_path, *args)
    assert proc.returncode == 2, proc.stderr
    assert "pipeline.py:amounts (line" in proc.stderr
    assert "TypeError" not in proc.stderr
    assert proc.stdout == ""
    # The upstream did not run first.
    assert not (tmp_path / ".barca" / "artifacts").exists()


def test_defaults_kwargs_partitions_and_partitions_from_run(tmp_path: Path) -> None:
    (tmp_path / "pipeline.py").write_text(
        """\
from barca import asset, partitions, partitions_from, collect


@asset()
def raw() -> list:
    return [1, 2]


@asset(inputs={"anything": raw})
def loose(limit: int = 1, **rest) -> int:
    return len(rest["anything"]) * limit


@asset(partitions={"region": partitions(["us", "eu"])})
def sales(region: str) -> int:
    return len(region)


@asset(partitions={"region": partitions_from(sales)})
def margin(region: str, sales: int) -> int:
    return sales * 2


@asset(inputs={"all": collect(margin), "n": loose})
def total(all: list, n: int) -> int:
    return sum(all) + n
"""
    )
    proc = barca_cmd(tmp_path, "get", "total", "pipeline.py", "-o", "value", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == 10


def test_a_partition_dimension_that_is_not_a_parameter_is_refused(tmp_path: Path) -> None:
    (tmp_path / "pipeline.py").write_text(
        """\
from barca import asset, partitions


@asset(partitions={"regoin": partitions(["us"])})
def sales(region: str) -> int:
    return 1
"""
    )
    proc = barca_cmd(tmp_path, "plan", "pipeline.py")
    assert proc.returncode == 2, proc.stderr
    assert envelope(proc)["error"] == (
        "Parse error: pipeline.py:sales (line 4): the partitions= key `regoin` is not a parameter: "
        "sales() takes `region`. Did you mean `region`?"
    )
