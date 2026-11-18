"""A file sink in its artifact's format is a copy of the artifact's bytes (#1005).

Conformance for specs/interfaces/artifact-formats.md ("Sinks") and `barca docs sinks`: a file
sink whose format (`serializer=`, else its extension, else the artifact's) is the artifact's
holds the artifact's bytes, and the step's result is not encoded a second time, so a returned
DuckDB relation runs its query once. Republishing a cached step's sink copies the cached bytes
without reading the value. Any other sink (another format, a dataset) encodes the result as
before, and a value that cannot be parquet still fails a `.parquet` sink.

Sink time is reported beside each step's own time (#1004): `sink_seconds` on the step and
`seconds` on each sink, never inside `elapsed_seconds`.
"""

import json
import os
import shutil
import time
from pathlib import Path

import pytest

from .helpers import SERVE, barca

duckdb = pytest.importorskip("duckdb")
pq = pytest.importorskip("pyarrow.parquet")


def get(cwd: Path, target: str = "exported", *args: str, check: bool = True):
    proc = barca(
        cwd, "get", target, "pipeline.py", "--json", *args, env=os.environ.copy(), timeout=SERVE
    )
    if check:
        assert proc.returncode == 0, proc.stdout + proc.stderr
    return proc


def write(project: Path, source: str) -> None:
    (project / "pipeline.py").write_text(source)


def artifacts(project: Path, node: str = "exported") -> list[Path]:
    """The node's artifact files (a partition key's are in a directory of their own), oldest
    first; hidden staging files excluded."""
    root = project / ".barca" / "artifacts"
    files = [
        p
        for d in root.glob(f"pipeline.py--{node}*")
        for p in d.iterdir()
        if p.is_file() and not p.name.startswith(".")
    ]
    return sorted(files, key=lambda p: p.stat().st_mtime_ns)


def no_leftovers(project: Path) -> None:
    """The private link a sink is copied from is gone once the step is reported."""
    leftovers = [p for p in (project / ".barca").rglob("*") if ".sink-" in p.name]
    assert leftovers == []


def calls(project: Path) -> int:
    path = project / "calls.txt"
    return len(path.read_text().splitlines()) if path.exists() else 0


def store(project: Path) -> None:
    (project / "barca.toml").write_text(f'[remote]\nuri = "{project / "store"}"\n')


# A relation whose query writes a line to calls.txt for each row it computes: a query run once
# for its artifact and again for its sink writes twice as many lines.
RELATION = """
import pathlib

import duckdb
from barca import asset, sink

HERE = pathlib.Path(__file__).parent


def tick(i: int) -> int:
    with open(HERE / "calls.txt", "a") as f:
        f.write(f"{i}\\n")
    return i


@asset()
@sink("SINK")
def exported() -> duckdb.DuckDBPyRelation:
    con = duckdb.connect()
    con.create_function("tick", tick, ["BIGINT"], "BIGINT", side_effects=True)
    # An aggregate: its row order is not fixed, so a second run can write other bytes.
    return con.sql("select tick(i) % 3 as k, count(*) as n from range(5) t(i) group by 1")
"""


@pytest.mark.parametrize("with_store", [False, True], ids=["local", "store"])
@pytest.mark.parametrize(
    "serializer,sink_path",
    [("", "out.parquet"), ('serializer="csv"', "out.csv"), ("", "out")],
    ids=["parquet", "csv", "no-extension"],
)
def test_a_relation_runs_once_and_its_sink_is_the_artifact(
    tmp_path, serializer, sink_path, with_store
):
    if with_store:
        store(tmp_path)
    write(
        tmp_path, RELATION.replace("@asset()", f"@asset({serializer})").replace("SINK", sink_path)
    )
    proc = get(tmp_path)
    assert calls(tmp_path) == 5, "the relation's query ran more than once"
    (artifact,) = artifacts(tmp_path)
    assert (tmp_path / sink_path).read_bytes() == artifact.read_bytes()
    no_leftovers(tmp_path)
    step = json.loads(proc.stdout)["steps"][0]
    assert step["sink_seconds"] >= 0


def test_a_sink_in_another_format_still_encodes_the_result(tmp_path):
    """Not a copy: a `.csv` sink of a parquet artifact runs the relation again (as before)."""
    write(tmp_path, RELATION.replace("SINK", "out.csv"))
    get(tmp_path)
    assert calls(tmp_path) == 10
    rows = duckdb.sql(f"select * from read_csv('{tmp_path / 'out.csv'}') order by k").fetchall()
    assert rows == [(0, 2), (1, 2), (2, 1)]


# Pickling a `Counted` writes a line to dumps.txt and unpickling it one to loads.txt, each naming
# the barca module the process runs (`barca._worker`, `barca._sinks`, ...): a sink copied from the
# artifact is pickled once by its worker (`barca get` itself reads the final value to print it),
# and republishing it never unpickles it.
COUNTED = """
import pathlib
import sys

from barca import asset, sink

HERE = pathlib.Path(__file__).parent


def _process():
    spec = getattr(sys.modules["__main__"], "__spec__", None)
    return spec.name if spec else sys.argv[0]


def _loaded():
    with open(HERE / "loads.txt", "a") as f:
        f.write(_process() + "\\n")
    return Counted()


class Counted:
    def __reduce__(self):
        # The worker's input cache copies a value with copy.deepcopy: not an encode.
        if sys._getframe(1).f_globals.get("__name__") != "copy":
            with open(HERE / "dumps.txt", "a") as f:
                f.write(_process() + "\\n")
        return (_loaded, ())


@asset()
@sink("SINK")
def exported() -> Counted:
    # VERSION
    return Counted()
"""


def lines(project: Path, name: str, process: str) -> int:
    path = project / name
    if not path.exists():
        return 0
    return sum(1 for line in path.read_text().splitlines() if line == process)


def test_a_pickle_sink_is_pickled_once(tmp_path):
    write(tmp_path, COUNTED.replace("SINK", "out.pkl"))
    get(tmp_path)
    assert lines(tmp_path, "dumps.txt", "barca._worker") == 1
    (artifact,) = artifacts(tmp_path)
    assert (tmp_path / "out.pkl").read_bytes() == artifact.read_bytes()


def test_a_value_that_fell_back_to_pickle_still_fails_a_parquet_sink(tmp_path):
    write(tmp_path, COUNTED.replace("SINK", "out.parquet"))
    proc = get(tmp_path, check=False)
    assert proc.returncode == 1
    assert "cannot be written as parquet" in proc.stdout + proc.stderr
    assert not (tmp_path / "out.parquet").exists()


def test_an_explicit_artifact_serializer_in_another_format_encodes_the_sink(tmp_path):
    write(
        tmp_path,
        """
import pandas as pd
from barca import asset, sink


@asset(serializer="pickle")
@sink("out.parquet")
def exported() -> pd.DataFrame:
    return pd.DataFrame({"a": [1, 2, 3]})
""",
    )
    get(tmp_path)
    (artifact,) = artifacts(tmp_path)
    assert artifact.suffix == ".pkl"
    assert pq.read_table(tmp_path / "out.parquet").column("a").to_pylist() == [1, 2, 3]


def test_a_json_sink_and_a_dataset_sink_of_one_step(tmp_path):
    """The json file sink is the artifact's bytes; the dataset beside it is written as before."""
    write(
        tmp_path,
        """
from barca import asset, sink


@asset()
@sink("out.json")
@sink("rows/")
def exported() -> list:
    return [{"a": 1}, {"a": 2}]
""",
    )
    get(tmp_path)
    (artifact,) = artifacts(tmp_path)
    assert (tmp_path / "out.json").read_bytes() == artifact.read_bytes()
    # A value that is not a table is one JSON line, written by the dataset writer as before.
    rows = (tmp_path / "rows" / "part-0.jsonl").read_text().splitlines()
    assert [json.loads(line) for line in rows] == [[{"a": 1}, {"a": 2}]]


def test_partitioned_keys_copy_their_own_artifacts(tmp_path):
    write(
        tmp_path,
        """
import pandas as pd
from barca import asset, partitions, sink


@asset(partitions={"region": partitions(["eu", "us"])})
@sink("out.parquet")
def exported(region: str) -> pd.DataFrame:
    return pd.DataFrame({"region": [region]})
""",
    )
    get(tmp_path)
    by_region = {
        pq.read_table(p).column("region")[0].as_py(): p.read_bytes() for p in artifacts(tmp_path)
    }
    assert (tmp_path / "out_region_eu.parquet").read_bytes() == by_region["eu"]
    assert (tmp_path / "out_region_us.parquet").read_bytes() == by_region["us"]
    no_leftovers(tmp_path)


def test_a_symlinked_sink_path_is_followed_and_kept(tmp_path):
    write(tmp_path, RELATION.replace("SINK", "out.parquet"))
    target = tmp_path / "elsewhere" / "real.parquet"
    target.parent.mkdir()
    target.write_bytes(b"old")
    (tmp_path / "out.parquet").symlink_to(target)
    get(tmp_path)
    assert (tmp_path / "out.parquet").is_symlink()
    (artifact,) = artifacts(tmp_path)
    assert target.read_bytes() == artifact.read_bytes()
    assert calls(tmp_path) == 5


def test_a_batched_key_copies_its_artifact(tmp_path):
    write(
        tmp_path,
        """
import barca
import duckdb
from barca import asset, partitions, sink


@asset(partitions={"region": partitions(["eu", "us"])}, batch="all")
@sink("out.parquet")
@sink("out.csv")
def exported(region: list[str]) -> duckdb.DuckDBPyRelation:
    return barca.duckdb_connection().sql(
        "select unnest(?::varchar[]) as region, 1 as n", params=[region]
    )
""",
    )
    get(tmp_path)
    by_region = {
        pq.read_table(p).column("region")[0].as_py(): p.read_bytes() for p in artifacts(tmp_path)
    }
    assert sorted(by_region) == ["eu", "us"]
    for region, data in by_region.items():
        assert (tmp_path / f"out_region_{region}.parquet").read_bytes() == data
        # The csv sink is not a copy: it is encoded from the key's rows, as before.
        assert (tmp_path / f"out_region_{region}.csv").read_text().splitlines() == [
            "region,n",
            f"{region},1",
        ]
    no_leftovers(tmp_path)


@pytest.mark.parametrize("with_store", [False, True], ids=["local", "store"])
def test_republish_copies_the_cached_bytes_without_reading_the_value(tmp_path, with_store):
    if with_store:
        store(tmp_path)
    source = COUNTED.replace("SINK", "out.pkl")
    write(tmp_path, source.replace("VERSION", "A"))
    get(tmp_path)
    first_sink = (tmp_path / "out.pkl").read_bytes()
    write(tmp_path, source.replace("VERSION", "B"))
    get(tmp_path)
    write(tmp_path, source.replace("VERSION", "A"))
    if with_store:
        shutil.rmtree(tmp_path / ".barca" / "artifacts")
    proc = get(tmp_path)
    assert "republished" in proc.stderr, proc.stderr
    assert lines(tmp_path, "dumps.txt", "barca._worker") == 2
    assert lines(tmp_path, "loads.txt", "barca._sinks") == 0, "republish read the cached value"
    assert (tmp_path / "out.pkl").read_bytes() == first_sink


SLOW_SINK = """
import pathlib
import time

from barca import asset, sink

HERE = pathlib.Path(__file__).parent


@asset()
@sink("out.json")
def first() -> dict:
    time.sleep(0.3)
    return {"payload": "x" * 1000}


@asset(inputs={"first": first})
@sink("second.json")
def exported(first: dict) -> dict:
    return first
"""


def test_sink_time_is_reported_beside_the_step(tmp_path, monkeypatch):
    """A slow sink: the run's wall time is covered by step time plus sink time (#1004)."""
    write(tmp_path, SLOW_SINK)
    # Slow every sink write down by 1 s, in the worker, without touching the step.
    (tmp_path / "sitecustomize.py").write_text(
        "import time\n"
        "from barca import _artifacts\n"
        "_copy = _artifacts.copy_file\n"
        "def slow(source, dest):\n"
        "    time.sleep(1.0)\n"
        "    return _copy(source, dest)\n"
        "_artifacts.copy_file = slow\n"
        "import barca._sinks as _s\n"
        "_s.copy_file = slow\n"
    )
    env = os.environ.copy()
    env["PYTHONPATH"] = f"{tmp_path}{os.pathsep}{env.get('PYTHONPATH', '')}"
    started = time.perf_counter()
    proc = barca(tmp_path, "get", "exported", "pipeline.py", "--json", env=env, timeout=SERVE)
    wall = time.perf_counter() - started
    assert proc.returncode == 0, proc.stdout + proc.stderr
    out = json.loads(proc.stdout)
    steps = out["steps"]
    assert len(steps) == 2
    for step in steps:
        assert step["sink_seconds"] >= 1.0, step
        assert step["elapsed_seconds"] < 1.0, step
    covered = sum(s["elapsed_seconds"] + s["sink_seconds"] for s in steps)
    # What is left is the coordinator's own work and the process start-up, not the sinks.
    assert out["elapsed_seconds"] - covered < 1.0, (out["elapsed_seconds"], covered)
    assert wall >= covered

    history = barca(tmp_path, "history", out["run_id"], "--json", env=env, timeout=SERVE)
    assert history.returncode == 0, history.stderr
    recorded = {s["node_id"]: s for s in json.loads(history.stdout)["steps"]}
    for step in steps:
        assert recorded[step["id"]]["sink_seconds"] == pytest.approx(step["sink_seconds"])
