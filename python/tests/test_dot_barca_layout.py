"""Conformance for specs/interfaces/dot-barca-layout.md: barca writes no top-level entry under
`.barca/` that the spec's table does not name."""

import json
import os
import re
from pathlib import Path

from .helpers import barca as run_barca

SPEC = Path(__file__).resolve().parents[2] / "specs" / "interfaces" / "dot-barca-layout.md"

PIPELINE = """
from functools import partial

import pandas as pd
from barca import asset, parallel, partitions, task


@task
def square(x: int) -> int:
    return x * x


@asset
def squares() -> list:
    return parallel(*[partial(square, i) for i in range(3)])


@asset
def frame() -> pd.DataFrame:
    return pd.DataFrame({"a": [1, 2]})


@asset(partitions={"k": partitions(["a", "b"])})
def keyed(k: str) -> str:
    return k
"""


def spec_entries() -> list[re.Pattern]:
    """The table's entries as patterns: `<...>` matches any part of a name."""
    section = SPEC.read_text().split("## Top-level entries", 1)[1].split("\n## ", 1)[0]
    patterns = []
    for line in section.splitlines():
        match = re.match(r"\| `([^`]+)` \|", line)
        if match:
            name = match.group(1).rstrip("/")
            parts = re.split(r"<[^>]+>", name)
            patterns.append(re.compile(".+".join(re.escape(p) for p in parts) + r"\Z"))
    assert len(patterns) >= 15, "the spec's table is missing"
    return patterns


def barca(project: Path, env: dict, *args: str) -> dict:
    proc = run_barca(project, *args, "--json", env=env)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def test_top_level_entries_are_in_the_spec(tmp_path):
    project = tmp_path / "project"
    project.mkdir()
    (project / "p.py").write_text(PIPELINE)
    store = tmp_path / "store"
    store.mkdir()
    env = {**os.environ, "BARCA_REMOTE_URI": str(store)}
    barca(project, env, "get", "squares", "p.py")
    barca(project, env, "get", "frame", "p.py")
    barca(project, env, "get", "keyed", "p.py")
    # Evict the local copy so `barca sql` reads through the store.
    for f in (project / ".barca" / "artifacts").glob("p.py--frame/*"):
        f.unlink()
    barca(project, env, "sql", "select * from frame", "p.py")
    barca(project, env, "status", "frame", "p.py", "--sample", "1")
    barca(project, env, "history")
    barca(project, env, "gc")
    barca(project, env, "get", "keyed", "p.py", "--env", "dev")

    patterns = spec_entries()
    found = sorted(p.name for p in (project / ".barca").iterdir())
    unnamed = [name for name in found if not any(p.match(name) for p in patterns)]
    assert not unnamed, f"not in {SPEC.name}: {unnamed} (found {found})"
    # The run wrote what the spec says such a run writes.
    assert {"metadata.db", "artifacts", "backups", "envs", ".gitignore"} <= set(found)
