"""Editing a project helper module invalidates every step that uses it (#178).

Whichever way the pipeline file is named on the command line (bare `p.py`, `./p.py`, an absolute
path, or `sub/p.py` from the parent directory) and whichever import style the step uses
(`from helpers import compute`, `import helpers` + `helpers.compute()`,
`import pkg.mod as m` + `m.f()`), the run hash covers the helper and is the same for every
spelling of the path. Node ids are relative to the project root, whatever the spelling (#202).
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINES = {
    "from_import": """
from barca import asset
from helpers import compute


@asset()
def val() -> int:
    return compute()
""",
    "module_attribute": """
import helpers
from barca import asset


@asset()
def val() -> int:
    return helpers.compute()
""",
    "aliased_dotted_module": """
import pkg.mod as m
from barca import asset


@asset()
def val() -> int:
    return m.compute()
""",
}

# The helper changes size between versions, so a stale .pyc can never mask the edit (#176).
HELPER = "def compute():\n    return {v}\n"


def write_project(root: Path, style: str, v: int = 1) -> Path:
    sub = root / "sub"
    (sub / "pkg").mkdir(parents=True, exist_ok=True)
    (sub / "p.py").write_text(PIPELINES[style])
    (sub / "helpers.py").write_text(HELPER.format(v=v))
    (sub / "pkg" / "__init__.py").write_text("")
    (sub / "pkg" / "mod.py").write_text(HELPER.format(v=v))
    return sub


def set_helper(sub: Path, v: int) -> None:
    (sub / "helpers.py").write_text(HELPER.format(v=v))
    (sub / "pkg" / "mod.py").write_text(HELPER.format(v=v))


def barca(cwd: Path, *args: str) -> dict:
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout.strip()
    try:
        return json.loads(out)  # `status --json` is one pretty-printed document
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])  # `get` prints JSON on its last line


def forms(sub: Path) -> list[tuple[Path, str, str]]:
    """(cwd, file argument, expected node id) for each way of naming the pipeline."""
    return [
        (sub, "p.py", "p.py:val"),
        (sub, "./p.py", "p.py:val"),
        (sub, str(sub / "p.py"), "p.py:val"),
        (sub.parent, "sub/p.py", "sub/p.py:val"),
    ]


def run_hash(cwd: Path, file_arg: str) -> tuple[str, str]:
    status = barca(cwd, "status", "val", file_arg, "--json")
    (node,) = [n for n in status["nodes"] if n["id"].endswith(":val")]
    return node["id"], node["cache"]["run_hash"]


@pytest.mark.parametrize("style", sorted(PIPELINES))
def test_every_path_spelling_has_the_same_run_hash_and_a_root_relative_id(tmp_path, style):
    sub = write_project(tmp_path, style)
    seen = {}
    for cwd, arg, expected_id in forms(sub):
        node_id, h = run_hash(cwd, arg)
        assert node_id == expected_id, "node ids are relative to the project root (the cwd here)"
        seen[arg] = h
    assert len(set(seen.values())) == 1, seen

    # The hash covers the helper: editing it moves the hash, for every spelling alike.
    set_helper(sub, 22)
    edited = {arg: run_hash(cwd, arg)[1] for cwd, arg, _ in forms(sub)}
    assert len(set(edited.values())) == 1, edited
    assert set(edited.values()) != set(seen.values())


@pytest.mark.parametrize("style", sorted(PIPELINES))
@pytest.mark.parametrize("form", ["bare", "dot_slash", "absolute", "from_parent"])
def test_helper_edit_reruns_the_step(tmp_path, style, form):
    sub = write_project(tmp_path, style)
    cwd, arg, _ = dict(zip(["bare", "dot_slash", "absolute", "from_parent"], forms(sub)))[form]

    first = barca(cwd, "get", "val", arg)
    assert first["steps_executed"] == 1
    assert first["final_output"] == 1

    assert barca(cwd, "get", "val", arg)["steps_executed"] == 0

    set_helper(sub, 22)
    after = barca(cwd, "get", "val", arg)
    assert after["steps_executed"] == 1, "a helper edit must invalidate the cached result"
    assert after["final_output"] == 22
