"""Load the planning-pipeline generator (benchmarks/planning_pipeline/generate.py, #250).

`write_planning_pipeline(dest)` writes the 116-node project into `dest` and returns its
`Project` (root, nodes, edges, `downstream()`); the `planning_pipeline` fixture in conftest.py
does it in a temp dir. Other suites (the `--server` parity suite, #394) reuse it from here.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

GENERATOR = Path(__file__).resolve().parents[2] / "benchmarks" / "planning_pipeline" / "generate.py"
_MODULE = "barca_planning_pipeline_generate"


def generator():
    """The generator module; skips the test when the repo checkout is not available."""
    if _MODULE in sys.modules:
        return sys.modules[_MODULE]
    if not GENERATOR.exists():  # e.g. running from an installed sdist
        pytest.skip("benchmarks/planning_pipeline is not in this checkout")
    for dep in ("duckdb", "pandas", "polars", "pyarrow", "fsspec"):
        pytest.importorskip(dep)
    spec = importlib.util.spec_from_file_location(_MODULE, GENERATOR)
    module = importlib.util.module_from_spec(spec)
    sys.modules[_MODULE] = module  # dataclasses look their module up while being created
    spec.loader.exec_module(module)
    return module


def write_planning_pipeline(dest: Path | str, *, scale: int = 1, seed: int = 250):
    """Write the planning-pipeline project into `dest` and return its `Project`."""
    return generator().write_project(dest, scale=scale, seed=seed)
