"""One stored artifact read by a lazy step and an eager step, end to end (#248).

`dispatch::lazily_read_inputs` decides, per phase, which inputs are read in place from the
artifact store: those every reader in the phase takes through a lazy type. One eager reader
needs the whole file, so the artifact is downloaded once and every reader uses that copy
(`barca docs big-inputs`, "Remote artifacts").

test_remote_lazy_local_first.py pins this for an unpartitioned input on an S3 emulator. Here
the input is partitioned, so its readers name it by base id while its artifacts are per key,
and the store is a plain directory, so the tests run without an emulator.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

pytest.importorskip("pandas")
pytest.importorskip("pyarrow")
pytest.importorskip("duckdb")
pytest.importorskip("polars")

PIPELINE = """
import duckdb
import pandas as pd
import polars as pl
from barca import asset, collect, partitions, partitions_from


@asset(partitions={"k": partitions(["a", "b"])})
def parts(k: str) -> pd.DataFrame:
    start = {"a": 0, "b": 1000}[k]
    return pd.DataFrame({"n": range(start, start + 100), "k": [k] * 100})


@asset(inputs={"p": parts}, partitions={"k": partitions_from(parts)})
def lazy_each(k: str, p: duckdb.DuckDBPyRelation) -> int:
    return int(p.aggregate("sum(n)").fetchone()[0])


@asset(inputs={"p": parts}, partitions={"k": partitions_from(parts)})
def eager_each(k: str, p: pd.DataFrame) -> int:
    return int(p["n"].sum())


@asset(inputs={"xs": collect(lazy_each)})
def lazy_report(xs: list) -> list:
    return sorted(xs)


@asset(inputs={"xs": collect(eager_each)})
def eager_report(xs: list) -> list:
    return sorted(xs)


@asset(inputs={"ps": collect(parts)})
def eager_all(ps: list[pd.DataFrame]) -> int:
    return int(sum(p["n"].sum() for p in ps))


@asset(inputs={"ps": collect(parts)})
def lazy_all(ps: list[pl.LazyFrame]) -> int:
    return int(pl.concat(ps).select(pl.col("n").sum()).collect().item())
"""

EACH = [sum(range(100)), sum(range(1000, 1100))]
ALL = sum(EACH)


def cli(root: Path, store: Path, *args: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env={**env, "BARCA_REMOTE_URI": str(store)},
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


@pytest.fixture
def machine(tmp_path):
    """Factory for working directories that share one store; `parts` is already in it."""
    store = tmp_path / "store"

    def make(name: str) -> Path:
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        return root

    produced = cli(make("producer"), store, "get", "parts", "--json")
    assert produced.returncode == 0, produced.stderr
    assert len(list(store.glob("default/artifacts/*parts*/*.parquet"))) == 2
    return store, make


def local_copies(root: Path) -> list[Path]:
    return sorted((root / ".barca" / "artifacts").glob("*parts*/*.parquet"))


def targets(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    doc = json.loads(proc.stdout)
    if "targets" in doc:
        return {name: t["final_output"] for name, t in doc["targets"].items()}
    return {"": doc["final_output"]}


def test_lazy_readers_alone_read_every_partition_in_place(machine):
    store, make = machine
    root = make("lazy")
    proc = cli(root, store, "get", "lazy_report", "--json")
    assert targets(proc) == {"": EACH}
    assert "fetched" not in proc.stderr, proc.stderr
    assert local_copies(root) == []


def test_an_eager_reader_in_the_same_phase_downloads_each_partition_once_for_both(machine):
    store, make = machine
    root = make("mixed")
    proc = cli(root, store, "get", "lazy_report,eager_report", "--json")
    assert targets(proc) == {"lazy_report": EACH, "eager_report": EACH}
    assert "fetched 2 cached artifacts" in proc.stderr, proc.stderr
    assert len(local_copies(root)) == 2


def test_an_eager_fan_in_in_a_later_phase_downloads_what_the_lazy_step_read_in_place(machine):
    # A collect() consumer runs in its own phase, after the per-partition readers: the lazy
    # step reads in place, then the eager one downloads the partitions for itself.
    store, make = machine
    root = make("later")
    proc = cli(root, store, "get", "lazy_report,eager_all", "--json")
    assert targets(proc) == {"lazy_report": EACH, "eager_all": ALL}
    assert "fetched 2 cached artifacts" in proc.stderr, proc.stderr
    assert len(local_copies(root)) == 2


def test_a_lazy_fan_in_is_handed_local_copies(machine):
    # Known limit: only single inputs are read in place; a collect() of lazy frames is
    # downloaded like an eager one (crates/barca-core/src/commands.rs `read_in_place`).
    store, make = machine
    root = make("fan-in")
    proc = cli(root, store, "get", "lazy_all", "--json")
    assert targets(proc) == {"": ALL}
    assert "fetched 2 cached artifacts" in proc.stderr, proc.stderr
    assert len(local_copies(root)) == 2


def test_the_same_readers_agree_without_a_store(tmp_path):
    root = tmp_path / "local"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    proc = subprocess.run(
        [_find_binary(), "get", "lazy_report,eager_report,eager_all,lazy_all", "--json"],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )
    assert targets(proc) == {
        "lazy_report": EACH,
        "eager_report": EACH,
        "eager_all": ALL,
        "lazy_all": ALL,
    }
