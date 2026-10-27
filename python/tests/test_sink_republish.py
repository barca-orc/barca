"""A cache hit republishes a sink that holds another run hash's output (#662).

Conformance for `barca docs sinks` ("Cache hits") and specs/interfaces/artifact-formats.md
"Sink publication": barca records the run hash each sink last published, per partition key; a
cache hit whose run hash differs writes the sink from the cached artifact without running the
step, and one that matches writes nothing. Code A -> B -> A, for a file sink, a partitioned file
sink, an unpartitioned and a partitioned dataset sink, with and without an artifact store.
"""

import json
import os
import shutil
import sqlite3
import subprocess
from contextlib import closing
from pathlib import Path

import duckdb
import pytest
from barca.api import _find_binary


def get(cwd: Path, target: str, check: bool = True):
    proc = subprocess.run(
        [_find_binary(), "get", target, "pipeline.py", "--json"],
        cwd=cwd,
        env=os.environ.copy(),
        capture_output=True,
        text=True,
        timeout=300,
    )
    if check:
        assert proc.returncode == 0, proc.stdout + proc.stderr
    return proc


def executed(proc) -> int:
    return json.loads(proc.stdout)["steps_executed"]


def mtimes(root: Path) -> dict[str, int]:
    files = [root] if root.is_file() else [p for p in root.rglob("*") if p.is_file()]
    return {str(p): p.stat().st_mtime_ns for p in files if p.name != "_barca_sink.json"}


def read_dataset(prefix: Path) -> list[tuple]:
    return duckdb.sql(
        f"select * from read_parquet('{prefix}/**/*.parquet', hive_partitioning = true) "
        "order by all"
    ).fetchall()


FILE = """
from barca import asset, sink


@asset()
@sink("out.json")
def exported() -> dict:
    return {"version": "VERSION"}
"""

PARTITIONED_FILE = """
from barca import asset, partitions, sink


@asset(partitions={"region": partitions(["eu", "us"])})
@sink("out.json")
def exported(region: str) -> dict:
    return {"region": region, "version": "VERSION"}
"""

DATASET = """
import pandas as pd
from barca import asset, sink


@asset()
@sink("out/", partition_by=["day"])
def exported() -> pd.DataFrame:
    return pd.DataFrame({"day": ["d1", "d2"], "version": ["VERSION", "VERSION"]})
"""

PARTITIONED_DATASET = """
import pandas as pd
from barca import asset, partitions, sink


@asset(partitions={"region": partitions(["eu", "us"])})
@sink("out/")
def exported(region: str) -> pd.DataFrame:
    return pd.DataFrame({"version": ["VERSION"]})
"""


def write(project: Path, pipeline: str, version: str) -> None:
    (project / "pipeline.py").write_text(pipeline.replace("VERSION", version))


def file_contents(project: Path) -> list:
    return [json.loads(p.read_text()) for p in sorted(project.glob("out*.json"))]


CASES = {
    "file": (
        FILE,
        lambda p: file_contents(p),
        lambda v: [{"version": v}],
    ),
    "partitioned_file": (
        PARTITIONED_FILE,
        lambda p: file_contents(p),
        lambda v: [{"region": "eu", "version": v}, {"region": "us", "version": v}],
    ),
    "dataset": (
        DATASET,
        lambda p: read_dataset(p / "out"),
        lambda v: [(v, "d1"), (v, "d2")],
    ),
    "partitioned_dataset": (
        PARTITIONED_DATASET,
        lambda p: read_dataset(p / "out"),
        lambda v: [(v, "eu"), (v, "us")],
    ),
}


def sink_files(project: Path) -> dict[str, int]:
    out = {}
    for p in project.glob("out*"):
        out.update(mtimes(p))
    return out


@pytest.mark.parametrize("case", sorted(CASES))
@pytest.mark.parametrize("store", [False, True], ids=["local", "store"])
def test_a_to_b_to_a_republishes_the_sink_from_cache(tmp_path, case, store):
    pipeline, contents, expected = CASES[case]
    keys = 2 if case.startswith("partitioned") else 1
    if store:
        (tmp_path / "barca.toml").write_text(f'[remote]\nuri = "{tmp_path / "store"}"\n')
    write(tmp_path, pipeline, "A")
    assert executed(get(tmp_path, "exported")) == keys
    assert contents(tmp_path) == expected("A")
    write(tmp_path, pipeline, "B")
    assert executed(get(tmp_path, "exported")) == keys
    assert contents(tmp_path) == expected("B")

    # Back to A: a cache hit, and the sink holds A again. Nothing runs.
    write(tmp_path, pipeline, "A")
    if store:
        # This machine's copy is gone: the cached result is fetched from the store.
        shutil.rmtree(tmp_path / ".barca" / "artifacts")
    proc = get(tmp_path, "exported")
    assert executed(proc) == 0, proc.stderr
    assert contents(tmp_path) == expected("A")
    assert "[barca] sink out" in proc.stderr and "republished" in proc.stderr, proc.stderr

    # Nothing changed: the sink already holds A and is not written.
    before = sink_files(tmp_path)
    proc = get(tmp_path, "exported")
    assert executed(proc) == 0
    assert "republished" not in proc.stderr
    assert sink_files(tmp_path) == before


KEYED = """
import json

from barca import asset, partitions, sensor, sink


@sensor(partitions={"region": partitions(["eu", "us"])})
def version(region: str) -> int:
    with open("versions.json") as f:
        return json.load(f)[region]


@asset(inputs={"version": version}, partitions={"region": partitions(["eu", "us"])})
@sink("out.json")
@sink("out/")
def exported(region: str, version: int) -> list:
    return [{"region": region, "version": version}]
"""


def test_only_the_keys_that_hold_another_hash_are_republished(tmp_path):
    """Per key: `eu` goes 1 -> 2 -> 1 and is republished; `us` stays and is not written."""
    (tmp_path / "pipeline.py").write_text(KEYED)
    versions = tmp_path / "versions.json"
    for eu in (1, 2):
        versions.write_text(json.dumps({"eu": eu, "us": 1}))
        get(tmp_path, "exported")
    us = {**mtimes(tmp_path / "out_region_us.json"), **mtimes(tmp_path / "out" / "region=us")}
    versions.write_text(json.dumps({"eu": 1, "us": 1}))
    proc = get(tmp_path, "exported")
    # The two sensor keys run; both asset keys are cache hits.
    assert executed(proc) == 2, proc.stderr
    assert "[barca] sink out.json: republished 1 cached key of exported" in proc.stderr
    assert "[barca] sink out/: republished 1 cached key of exported" in proc.stderr
    assert json.loads((tmp_path / "out_region_eu.json").read_text()) == [
        {"region": "eu", "version": 1}
    ]
    eu_rows = (tmp_path / "out" / "region=eu" / "part-0.jsonl").read_text()
    assert json.loads(eu_rows) == [{"region": "eu", "version": 1}]
    assert {
        **mtimes(tmp_path / "out_region_us.json"),
        **mtimes(tmp_path / "out" / "region=us"),
    } == us


def test_a_sink_that_cannot_be_republished_fails_the_step(tmp_path):
    write(tmp_path, FILE, "A")
    get(tmp_path, "exported")
    write(tmp_path, FILE, "B")
    get(tmp_path, "exported")
    write(tmp_path, FILE, "A")
    # The sink path is now a directory: writing it fails.
    (tmp_path / "out.json").unlink()
    (tmp_path / "out.json").mkdir()
    proc = get(tmp_path, "exported", check=False)
    assert proc.returncode == 1, proc.stderr
    envelope = json.loads([line for line in proc.stderr.splitlines() if line.startswith("{")][-1])
    assert envelope["kind"] == "step_failed", envelope
    assert envelope["node"] == "pipeline.py:exported", envelope
    assert "SinkError: sink out.json:" in envelope["error"], envelope
    assert envelope["phase"] == "sink", envelope
    assert json.loads(proc.stdout)["status"] == "failed"
    # Nothing was recorded as published: once the path is free, the next run writes it.
    (tmp_path / "out.json").rmdir()
    proc = get(tmp_path, "exported")
    assert executed(proc) == 0
    assert file_contents(tmp_path) == [{"version": "A"}]


def test_a_database_from_before_the_table_writes_nothing_for_a_current_sink(tmp_path):
    """Upgrading: without a recorded publication, the step's last run that wrote sinks says
    what the sink holds."""
    write(tmp_path, FILE, "A")
    get(tmp_path, "exported")
    db_path = tmp_path / ".barca" / "metadata.db"
    with closing(sqlite3.connect(db_path)) as db:
        db.execute("DROP TABLE sink_publications")
        db.commit()
    before = mtimes(tmp_path / "out.json")
    proc = get(tmp_path, "exported")
    assert executed(proc) == 0
    assert "republished" not in proc.stderr
    assert mtimes(tmp_path / "out.json") == before

    # A -> B -> A recorded before the table: the history says the sink holds B.
    write(tmp_path, FILE, "B")
    get(tmp_path, "exported")
    with closing(sqlite3.connect(db_path)) as db:
        db.execute("DROP TABLE sink_publications")
        db.commit()
    write(tmp_path, FILE, "A")
    proc = get(tmp_path, "exported")
    assert "republished the cached result of exported" in proc.stderr
    assert file_contents(tmp_path) == [{"version": "A"}]
