"""The code a worker runs is exactly the source on disk, never a stale `__pycache__` .pyc (#176).

Python treats a timestamp .pyc as fresh when the source's mtime (whole seconds) and size
match. Barca hashes source text at plan time, so an edit that keeps the file's size and
mtime (an edit within the same second, or a tool that pins mtimes: Nix, Bazel, `touch -t`,
`rsync -t`) used to run the old bytecode under the new run hash and cache the wrong value.

Every test here writes a file, pins its mtime, runs it (which compiles it), then makes a
same-size edit and pins the same mtime again: exactly the case a timestamp .pyc cannot see.
"""

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

from barca.api import _find_binary

PINNED = 1767225600  # 2026-01-01T00:00:00Z


def write_pinned(path: Path, code: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(code).lstrip("\n"))
    os.utime(path, (PINNED, PINNED))


def same_size_edit(path: Path, old: str, new: str) -> None:
    assert len(old) == len(new), "the edit must keep the file size"
    before = path.read_text()
    assert old in before
    after = before.replace(old, new)
    assert len(after) == len(before)
    path.write_text(after)
    os.utime(path, (PINNED, PINNED))


def barca(project: Path, *args: str) -> dict:
    env = {k: v for k, v in os.environ.items() if k != "PYTHONDONTWRITEBYTECODE"}
    proc = subprocess.run(
        [_find_binary(), *args], cwd=project, env=env, capture_output=True, text=True
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def steps(result: dict) -> dict:
    return {s["id"].split(":")[-1]: s for s in result["steps"]}


# Helper tests name the pipeline by its bare filename, the way most people type it. That form
# used to skip the helper scan, so helper edits never reached the run hash (#178); it now covers
# that fix too.
PIPELINE = "p.py"


@pytest.fixture()
def project(tmp_path: Path) -> Path:
    return tmp_path


def test_asset_in_the_target_file(project):
    p = project / "p.py"
    write_pinned(
        p,
        """
        from barca import asset

        @asset()
        def val() -> int:
            return 1
        """,
    )
    first = barca(project, "get", "val", "p.py", "--json")
    assert first["final_output"] == 1

    same_size_edit(p, "return 1", "return 2")
    second = barca(project, "get", "val", "p.py", "--json")
    assert steps(second)["val"]["run_hash"] != steps(first)["val"]["run_hash"]
    assert steps(second)["val"]["status"] == "ran"
    assert second["final_output"] == 2

    # The value cached under the new run hash is the new code's.
    third = barca(project, "get", "val", "p.py", "--json")
    assert steps(third)["val"]["status"] == "cached"
    assert third["final_output"] == 2


def test_helper_module_imported_by_the_pipeline(project):
    write_pinned(
        project / "helpers.py",
        """
        def compute() -> int:
            return 1
        """,
    )
    p = project / "p.py"
    write_pinned(
        p,
        """
        from barca import asset
        from helpers import compute

        @asset()
        def val() -> int:
            return compute()
        """,
    )
    first = barca(project, "get", "val", PIPELINE, "--json")
    assert first["final_output"] == 1

    same_size_edit(project / "helpers.py", "return 1", "return 2")
    second = barca(project, "get", "val", PIPELINE, "--json")
    # The helper is in the asset's dependency cone, so the edit changes the run hash...
    assert steps(second)["val"]["run_hash"] != steps(first)["val"]["run_hash"]
    # ...and the worker must run the edited helper, not its cached bytecode.
    assert second["final_output"] == 2


def test_package_helper_with_relative_import(project):
    write_pinned(project / "lib" / "__init__.py", "from .core import compute\n")
    write_pinned(
        project / "lib" / "core.py",
        """
        def compute() -> int:
            return 1
        """,
    )
    p = project / "p.py"
    write_pinned(
        p,
        """
        from barca import asset
        from lib import compute

        @asset()
        def val() -> int:
            return compute()
        """,
    )
    first = barca(project, "get", "val", PIPELINE, "--json")
    assert first["final_output"] == 1

    same_size_edit(project / "lib" / "core.py", "return 1", "return 2")
    second = barca(project, "get", "val", PIPELINE, "--json")
    assert steps(second)["val"]["run_hash"] != steps(first)["val"]["run_hash"]
    assert second["final_output"] == 2


def test_parallel_children(project):
    p = project / "p.py"
    write_pinned(
        p,
        """
        from functools import partial
        from barca import task, parallel

        @task()
        def child(x: int) -> int:
            return x + 1

        @task()
        def fan() -> list:
            return parallel(partial(child, 10), partial(child, 20))
        """,
    )
    first = barca(project, "run", "fan", "p.py", "--json")
    assert first["final_output"] == [11, 21]

    same_size_edit(p, "return x + 1", "return x + 2")
    second = barca(project, "run", "fan", "p.py", "--json")
    assert second["final_output"] == [12, 22]


def test_sensor(project):
    p = project / "p.py"
    write_pinned(
        p,
        """
        from barca import asset, sensor

        @sensor()
        def ping() -> tuple[bool, int]:
            return True, 1

        @asset(inputs={"v": ping})
        def val(v: int) -> int:
            return v
        """,
    )
    first = barca(project, "get", "val", "p.py", "--json")
    assert first["final_output"] == 1

    same_size_edit(p, "return True, 1", "return True, 2")
    second = barca(project, "get", "val", "p.py", "--json")
    assert second["final_output"] == 2


def test_dynamic_partition_values(project):
    """`partitions(<expr>)` is evaluated by importing the file at plan time."""
    p = project / "p.py"
    write_pinned(
        p,
        """
        from barca import asset, collect, partitions

        KEYS = ["a", "b"]

        @asset(partitions={"k": partitions([x for x in KEYS])})
        def part(k: str) -> str:
            return k

        @asset(inputs={"parts": collect(part)})
        def summary(parts: list) -> list:
            return sorted(parts)
        """,
    )
    first = barca(project, "get", "summary", "p.py", "--json")
    assert first["final_output"] == ["a", "b"]

    same_size_edit(p, '["a", "b"]', '["a", "c"]')
    second = barca(project, "get", "summary", "p.py", "--json")
    assert second["final_output"] == ["a", "c"]


def _run_batch_mode(project: Path, source: Path) -> int:
    """Run one step through `python -m barca._worker <batch.json>` and return its value."""
    art = project / "artifacts"
    batch = {
        "stream_id": "test-w0",
        "artifact_dir": str(art),
        "provided_inputs": {},
        "steps": [
            {
                "node_id": "val",
                "kind": "asset",
                "function_name": "val",
                "source_file": str(source),
                "inputs": {},
            }
        ],
    }
    batch_file = project / "batch.json"
    batch_file.write_text(json.dumps(batch))
    env = {
        k: v for k, v in os.environ.items() if k not in ("PYTHONDONTWRITEBYTECODE", "BARCA_SOCKET")
    }
    proc = subprocess.run(
        [sys.executable, "-m", "barca._worker", str(batch_file)],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    results = [
        json.loads(line[len("BARCA:2:") :])
        for line in proc.stderr.splitlines()
        if line.startswith("BARCA:2:")
    ]
    result = next(m for m in results if m.get("type") == "result")
    return json.loads(Path(result["artifact"]["path"]).read_text())


def test_batch_mode_worker(project):
    write_pinned(
        project / "helpers.py",
        """
        def compute() -> int:
            return 10
        """,
    )
    p = project / "p.py"
    write_pinned(
        p,
        """
        from helpers import compute

        def val() -> int:
            return compute() + 1
        """,
    )
    assert _run_batch_mode(project, p) == 11

    same_size_edit(p, "compute() + 1", "compute() + 2")
    assert _run_batch_mode(project, p) == 12

    same_size_edit(project / "helpers.py", "return 10", "return 20")
    assert _run_batch_mode(project, p) == 22


# ─── The loader itself ───────────────────────────────────────────────────────

LOADER_PROBE = r"""
import json, os, sys
from barca._source_import import SourceHashLoader, load_source_module

root = sys.argv[1]
compiled = []
_orig = SourceHashLoader.source_to_code
def counting(self, data, path, **kw):
    compiled.append(os.path.basename(path))
    return _orig(self, data, path, **kw)
SourceHashLoader.source_to_code = counting

mod = load_source_module(os.path.join(root, "p.py"), "_barca_p")
import vendored  # lives in the project's own .venv site-packages
print(json.dumps({
    "value": mod.val(),
    "compiled": compiled,
    "file": mod.__file__,
    "spec_name": mod.__spec__.name,
    "registered": sys.modules["_barca_p"] is mod,
    "helper_loader": type(sys.modules["helpers"].__loader__).__name__,
    "vendored_loader": type(vendored.__loader__).__name__,
}))
"""


def _probe(project: Path) -> dict:
    venv_pkgs = project / ".venv" / "lib" / "site-packages"
    env = {k: v for k, v in os.environ.items() if k != "PYTHONDONTWRITEBYTECODE"}
    env["PYTHONPATH"] = os.pathsep.join(filter(None, [str(venv_pkgs), env.get("PYTHONPATH")]))
    proc = subprocess.run(
        [sys.executable, "-c", LOADER_PROBE, str(project)],
        env=env,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def test_loader_reuses_bytecode_only_for_identical_source(project):
    write_pinned(project / "helpers.py", "def compute() -> int:\n    return 1\n")
    write_pinned(
        project / "p.py", "from helpers import compute\n\ndef val():\n    return compute()\n"
    )
    write_pinned(project / ".venv" / "lib" / "site-packages" / "vendored.py", "X = 1\n")

    first = _probe(project)
    assert first["value"] == 1
    assert sorted(first["compiled"]) == ["helpers.py", "p.py"]
    assert first["file"] == str((project / "p.py").resolve())
    assert first["spec_name"] == "_barca_p" and first["registered"]
    assert first["helper_loader"] == "SourceHashLoader"
    # Site-packages, even a .venv inside the project directory, keeps the default loader.
    assert first["vendored_loader"] == "SourceFileLoader"

    # Unchanged source: both modules come from the hash-checked bytecode cache.
    assert _probe(project)["compiled"] == []

    # Same-size edit under a pinned mtime: only the edited module recompiles.
    same_size_edit(project / "helpers.py", "return 1", "return 2")
    third = _probe(project)
    assert third["value"] == 2
    assert third["compiled"] == ["helpers.py"]
