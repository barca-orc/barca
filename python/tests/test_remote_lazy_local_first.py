"""Which inputs a machine downloads from the artifact store, end to end against an S3 emulator.

A result another machine produced is read in place when every reader in the phase takes it
lazily, and downloaded once into the local artifact dir when any reader takes it eagerly.
"""

import uuid
from pathlib import Path

import pytest

from .test_remote_inspect import S3_ENDPOINT, S3_KEY, S3_SECRET, _reachable, cli, ok

PIPELINE = """
import duckdb
import pandas as pd
from barca import asset


@asset()
def big() -> pd.DataFrame:
    return pd.DataFrame({"a": range(1000), "b": ["x"] * 1000})


@asset(inputs={"big": big})
def lazy_sum(big: duckdb.DuckDBPyRelation) -> int:
    return int(big.aggregate("sum(a)").fetchone()[0])


@asset(inputs={"big": big})
def eager_sum(big: pd.DataFrame) -> int:
    return int(big["a"].sum())
"""

TOTAL = sum(range(1000))


@pytest.fixture
def machine(tmp_path):
    """Factory for working directories that share one bucket; `big` is already in it."""
    pytest.importorskip("pandas")
    pytest.importorskip("duckdb")
    pytest.importorskip("s3fs")
    if not _reachable(S3_ENDPOINT):
        pytest.skip(f"s3 emulator not reachable at {S3_ENDPOINT}")
    import fsspec

    bucket = f"barca-lazy-{uuid.uuid4().hex[:8]}"
    fsspec.filesystem(
        "s3", key=S3_KEY, secret=S3_SECRET, endpoint_url=S3_ENDPOINT, skip_instance_cache=True
    ).mkdir(bucket)

    def make(name: str) -> Path:
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
        (root / "barca.toml").write_text(
            f'[remote]\nuri = "s3://{bucket}/proj"\n\n'
            f'[remote.storage_options.s3]\nendpoint_url = "{S3_ENDPOINT}"\n'
        )
        return root

    ok(cli(make("producer"), "get", "big", "--json"))
    return make


def _local_copies(root: Path) -> list[Path]:
    return [p for p in (root / ".barca" / "artifacts").glob("*big*/**/*") if p.is_file()]


def test_a_lazy_reader_does_not_download_the_input(machine):
    root = machine("lazy")
    proc = cli(root, "get", "lazy_sum", "--json")
    assert str(TOTAL) in ok(proc).__repr__(), proc.stdout
    assert "fetched" not in proc.stderr, proc.stderr
    assert _local_copies(root) == []


def test_an_eager_reader_downloads_it_once_and_later_runs_reuse_the_copy(machine):
    root = machine("eager")
    proc = cli(root, "get", "eager_sum", "--json")
    assert str(TOTAL) in ok(proc).__repr__(), proc.stdout
    assert "fetched 1 cached artifact" in proc.stderr, proc.stderr
    assert len(_local_copies(root)) == 1

    again = cli(root, "get", "lazy_sum", "--json")
    assert str(TOTAL) in ok(again).__repr__(), again.stdout
    assert "fetched" not in again.stderr, again.stderr


def test_one_eager_reader_in_the_phase_downloads_it_for_both(machine):
    root = machine("mixed")
    proc = cli(root, "get", "lazy_sum,eager_sum", "--json")
    assert ok(proc).__repr__().count(str(TOTAL)) == 2, proc.stdout
    assert "fetched 1 cached artifact" in proc.stderr, proc.stderr
    assert len(_local_copies(root)) == 1
