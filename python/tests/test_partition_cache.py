"""Partitioned steps are cached per key.

Each partition has its own run hash, so a re-run serves unchanged keys from cache and executes
only keys whose hash has no successful materialization (new keys, or a changed upstream).
Downstream unpartitioned nodes, including a `collect` fan-in, are cached the usual way.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

# Keys come from an env var so a test can change the key set without changing any source (the
# definition hash must stay the same for cached keys to remain valid).
PIPELINE = """
import os
from barca import asset, collect, partitions

KEYS = os.environ.get("BARCA_TEST_KEYS", "a,b,c").split(",")


@asset(partitions={"k": partitions([x for x in KEYS])})
def part(k: str) -> dict:
    return {"k": k}


@asset(inputs={"parts": collect(part)})
def summary(parts: list) -> dict:
    return {"keys": sorted(p["k"] for p in parts)}
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def get(project: Path, *flags: str, keys: str = "a,b,c") -> dict:
    proc = subprocess.run(
        [_find_binary(), "get", "summary", "pipeline.py", *flags],
        cwd=project,
        env={**os.environ, "BARCA_TEST_KEYS": keys},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def test_second_run_is_fully_cached(project):
    first = get(project)
    assert first["steps_executed"] == 4  # three partitions + the fan-in
    assert first["final_output"] == {"keys": ["a", "b", "c"]}
    second = get(project)
    assert second["steps_executed"] == 0
    assert second["final_output"] == {"keys": ["a", "b", "c"]}


def test_a_new_key_runs_only_that_key(project):
    get(project, keys="a,b,c")
    grown = get(project, keys="a,b,c,d")
    # part[d] is new; the fan-in's inputs changed so it re-runs. a, b, c come from cache.
    assert grown["steps_executed"] == 2
    assert grown["final_output"] == {"keys": ["a", "b", "c", "d"]}
    again = get(project, keys="a,b,c,d")
    assert again["steps_executed"] == 0


def test_a_removed_key_is_not_resurrected(project):
    get(project, keys="a,b,c,d")
    shrunk = get(project, keys="a,b")
    assert shrunk["final_output"] == {"keys": ["a", "b"]}
    assert shrunk["steps_executed"] == 1  # only the fan-in; both remaining keys were cached


def test_no_cache_still_reruns_every_partition(project):
    get(project)
    forced = get(project, "--refresh-all")
    assert forced["steps_executed"] == 4


def test_cached_partitions_keep_their_artifacts(project):
    get(project)
    get(project)
    arts = project / ".barca" / "artifacts"
    for key in ("a", "b", "c"):
        files = list((arts / f"pipeline.py--part_k_{key}").glob("*.json"))
        assert len(files) == 1, f"{key}: expected one artifact, got {files}"
