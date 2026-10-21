"""A syntax error in a project helper module is reported when barca plans (#550).

Until 0.28 a helper that did not parse was hashed without a word; the step that imported it
failed in the worker with a traceback made of importlib frames. Now a command that plans a step
of a file importing it (directly, through other project modules, or inside a function) exits 2
with the file, line, column and source line before anything runs. Commands whose steps do not
import it are unaffected. When a worker does hit a `SyntaxError` on import (an import barca
cannot see, such as `importlib.import_module`), the traceback shows where it is.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary

HELPERS = """\
def parse_amount(s):
    return float(s.split(":")[1])

def normalize(rows)
    return [parse_amount(r) for r in rows]
"""

PIPELINE = """\
from barca import asset
from helpers import normalize


@asset()
def raw() -> list:
    return ["a:1", "b:2"]


@asset(inputs={"rows": raw})
def amounts(rows: list) -> list:
    return normalize(rows)
"""

OTHER = """\
from barca import asset


@asset()
def fine() -> int:
    return 1
"""


def barca_cmd(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([_find_binary(), *args], cwd=cwd, capture_output=True, text=True, timeout=120)


def envelope(proc: subprocess.CompletedProcess) -> dict:
    return json.loads(next(line for line in reversed(proc.stderr.splitlines()) if line.startswith("{")))


@pytest.fixture
def project(tmp_path: Path) -> Path:
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "helpers.py").write_text(HELPERS)
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "other.py").write_text(OTHER)
    return tmp_path


def test_the_error_names_the_helper_its_position_and_the_step(project: Path) -> None:
    proc = barca_cmd(project, "get", "amounts", "pipeline.py", "--json")
    assert proc.returncode == 2, proc.stderr
    assert proc.stdout == ""
    assert envelope(proc) == {
        "code": 2,
        "kind": "usage",
        "error": "Parse error: syntax error in helpers.py: line 4, column 20: Expected `:`, found "
        "newline (imported by pipeline.py, so step 'pipeline.py:raw' cannot run)",
        "remediation": "helpers.py:4:20:\n"
        "    def normalize(rows)\n"
        "                       ^\n"
        "Fix the syntax error: the worker imports helpers.py when it loads pipeline.py, so no "
        "step of pipeline.py can run until it parses.",
    }


@pytest.mark.parametrize(
    "args",
    [
        ("plan", "pipeline.py"),
        ("status", "pipeline.py"),
        ("get", "pipeline.py"),
        ("get", "amounts", "pipeline.py"),
        ("get", "amounts", "pipeline.py", "--dry-run"),
        ("run", "amounts", "pipeline.py"),
        ("get", "amounts"),
    ],
)
def test_every_command_that_plans_a_step_of_the_file_exits_2_and_runs_nothing(
    project: Path, args: tuple[str, ...]
) -> None:
    proc = barca_cmd(project, *args)
    assert proc.returncode == 2, proc.stderr
    assert "syntax error in helpers.py: line 4, column 20" in proc.stderr
    assert "importlib" not in proc.stderr
    assert proc.stdout == ""
    assert not (project / ".barca" / "artifacts").exists()


def test_commands_whose_steps_do_not_import_it_are_unaffected(project: Path) -> None:
    # `list` plans nothing.
    proc = barca_cmd(project, "list", "--json")
    assert proc.returncode == 0, proc.stderr
    # A step of another file, with the whole project read.
    proc = barca_cmd(project, "get", "fine", "-o", "value", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == 1


def test_a_helper_reached_through_another_module_inside_a_function(project: Path) -> None:
    (project / "pkg").mkdir()
    (project / "pkg" / "__init__.py").write_text("")
    (project / "pkg" / "wrap.py").write_text("from helpers import normalize\n")
    (project / "pipeline.py").write_text(
        PIPELINE.replace("from helpers import normalize\n", "").replace(
            "    return normalize(rows)", "    from pkg.wrap import normalize\n\n    return normalize(rows)"
        )
    )
    proc = barca_cmd(project, "get", "amounts", "pipeline.py", "--json")
    assert proc.returncode == 2, proc.stderr
    assert envelope(proc)["error"].endswith(
        "(imported by pipeline.py through pkg/wrap.py, so step 'pipeline.py:raw' cannot run)"
    )


def test_a_worker_import_that_fails_shows_where_not_importlib(project: Path) -> None:
    """An import barca cannot see: the worker's traceback points at the syntax error."""
    (project / "dyn.py").write_text(
        "import importlib\n\nfrom barca import asset\n\n\n"
        "@asset()\ndef loads() -> int:\n    return importlib.import_module('helpers').parse_amount('a:1')\n"
    )
    proc = barca_cmd(project, "get", "loads", "dyn.py", "--json")
    assert proc.returncode == 1, proc.stderr
    error = envelope(proc)
    assert error["error"].startswith("step 'dyn.py:loads' failed: SyntaxError: expected ':'"), error
    traceback = error["traceback"]
    assert "importlib" not in traceback.replace("importlib.import_module('helpers')", ""), traceback
    assert 'dyn.py", line 8, in loads' in traceback, traceback
    assert 'helpers.py", line 4' in traceback, traceback
    assert "    def normalize(rows)\n" in traceback, traceback
    assert traceback.rstrip().endswith("^"), traceback
