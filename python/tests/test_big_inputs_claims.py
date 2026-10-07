"""Every claim of `barca docs big-inputs` about what a step reads, measured (#236).

The manual may only state behavior that was run. `test_docs_examples.py` runs the topic's
example through the CLI and checks its results; this file checks the claims about *what is
read*, two ways:

- **bytes**: the example's steps run through the worker against the in-process object store of
  `test_remote_lazy.py`, which serves byte ranges like a real one and counts every byte fetched
  and every whole-object download. This is the remote path (`barca docs remote`). The example's
  `events` query is run at 2M rows instead of 100k so that a column is much larger than the
  reader's fixed costs (DuckDB and pyarrow read a 64 KiB tail to find the metadata, and DuckDB
  prefetches small neighbouring ranges), which would otherwise drown the measurement;
- **the reader's plan**: for a local file there is no byte counter, so the test asks the reader
  itself (DuckDB `explain`, polars `explain`) which columns it scans and which filter it pushes
  into the scan, on the same reader call the worker makes for a local artifact.

Each test's docstring quotes the sentence of the manual it proves.
"""

import re
import subprocess
from pathlib import Path

import pytest

from barca import _artifacts, _storage
from barca.api import _find_binary

from .test_docs_examples import blocks
from .test_remote_lazy import CountingStore, _run_step

duckdb = pytest.importorskip("duckdb")
pl = pytest.importorskip("polars")
pd = pytest.importorskip("pandas")
pq = pytest.importorskip("pyarrow.parquet")
pytest.importorskip("fsspec")

EVENTS = "memory://arts/events/h1.parquet"
ROWS = 2_000_000
# What a reader fetches to open a parquet object, before any column: one read of its tail.
TAIL = 64 * 1024


@pytest.fixture(scope="module")
def topic() -> str:
    out = subprocess.run(
        [_find_binary(), "docs", "big-inputs"], capture_output=True, text=True, check=True
    )
    return out.stdout


@pytest.fixture(scope="module")
def example(topic) -> str:
    """The topic's example pipeline, as printed by the binary."""
    return blocks(topic, "python")[0]


def column_bytes(path: str) -> dict[str, int]:
    """Compressed bytes of each column of a parquet file, summed over its row groups."""
    meta = pq.ParquetFile(path).metadata
    out: dict[str, int] = {}
    for g in range(meta.num_row_groups):
        for c in range(meta.num_columns):
            col = meta.row_group(g).column(c)
            out[col.path_in_schema] = out.get(col.path_in_schema, 0) + col.total_compressed_size
    return out


@pytest.fixture
def events(tmp_path, monkeypatch, example):
    """The example's `events` asset, written as barca writes it but in 10 row groups, in a
    counting object store. Yields (store, local copy, bytes per column)."""
    monkeypatch.chdir(tmp_path)
    fs = CountingStore()
    monkeypatch.setitem(_storage._fs_cache, "memory", fs)
    # The asset's own query, from the manual's example.
    query = re.search(r'duckdb\.sql\("""(.*?)"""\)', example, re.S).group(1)
    assert "range(100000)" in query
    query = query.replace("range(100000)", f"range({ROWS})")
    local = tmp_path / "events.parquet"
    duckdb.sql(f"copy ({query}) to '{local}' (format parquet, row_group_size 100000)")
    meta = pq.ParquetFile(local).metadata
    assert meta.num_rows == ROWS and meta.num_row_groups >= 10
    fs.put_file(str(local), EVENTS)
    fs.fetched = fs.downloads = 0
    yield fs, local, column_bytes(str(local))
    fs.store.clear()


def run(tmp_path, monkeypatch, source: str, function: str, param: str, frame_type: str | None):
    """Run one step of `source` on the stored `events` artifact; returns its output URI."""
    return _run_step(
        tmp_path,
        monkeypatch,
        source,
        function,
        {param: EVENTS},
        {param: frame_type} if frame_type else {},
    )


# ─── the table: what each annotation reads ───────────────────────────────────


@pytest.mark.parametrize("frame_type", [None, "pandas", "polars", "pyarrow"])
def test_an_eager_input_is_read_whole(events, tmp_path, monkeypatch, frame_type):
    """ "none, `pd.DataFrame`, `pl.DataFrame`, `pyarrow.Table` | the whole parquet file, before
    the function runs" and, remote, "downloaded whole"."""
    fs, _, _ = events
    body = """
        def shape(events):
            return [type(events).__module__.split(".")[0], len(events), len(events.columns
                    if hasattr(events, "columns") else events.column_names)]
        """
    out = run(tmp_path, monkeypatch, body, "shape", "events", frame_type)
    module, rows, columns = _artifacts.deserialize(out, "json")
    # No annotation is pandas: the claim "a parquet input that is not annotated is read with
    # pandas".
    assert (
        module
        == {None: "pandas", "pandas": "pandas", "polars": "polars", "pyarrow": "pyarrow"}[
            frame_type
        ]
    )
    assert (rows, columns) == (ROWS, 3)  # every row and column, although the step used none
    assert fs.step_downloads == 1  # the whole object, once


def test_json_and_pickle_inputs_ignore_the_annotation(tmp_path):
    """ "A json or pickle input is always deserialized whole, whatever the annotation"."""
    _artifacts.serialize({"n": [1, 2, 3]}, tmp_path / "v.json", "json")
    for frame_type in (None, "duckdb", "polars_lazy"):
        value = _artifacts.deserialize(tmp_path / "v.json", "json", frame_type=frame_type)
        assert value == {"n": [1, 2, 3]}


@pytest.mark.parametrize("frame_type", ["duckdb", "polars_lazy"])
def test_a_lazy_input_that_is_never_queried_reads_no_rows(
    events, tmp_path, monkeypatch, frame_type
):
    """ "`duckdb.DuckDBPyRelation`, `pl.LazyFrame` | no rows before the function runs" (opening
    it reads at most the file's metadata)."""
    fs, local, cols = events
    body = """
        def untouched(events):
            return type(events).__name__
        """
    out = run(tmp_path, monkeypatch, body, "untouched", "events", frame_type)
    assert _artifacts.deserialize(out, "json") in ("DuckDBPyRelation", "LazyFrame")
    assert fs.step_downloads == 0
    # One read of the file's tail for the metadata, and nothing else.
    assert 0 < fs.step_fetched <= TAIL, fs.step_fetched
    assert fs.step_fetched < 0.02 * local.stat().st_size


# ─── the example's steps ─────────────────────────────────────────────────────


def scanned(plan: str) -> str:
    """The parquet scan nodes of a DuckDB plan: what the reader is asked for."""
    assert "PARQUET_SCAN" in plan, plan
    return plan[plan.index("PARQUET_SCAN") :]


def test_per_bucket_reads_only_the_bucket_column(events, tmp_path, monkeypatch, example):
    """ "aggregate before anything is materialized: only `bucket` is read"."""
    fs, local, cols = events
    size = local.stat().st_size
    out = run(tmp_path, monkeypatch, example, "per_bucket", "events", "duckdb")
    result = _artifacts.deserialize(out.replace(".json", ".parquet"), "parquet")
    assert result["n"].tolist() == [ROWS // 10] * 10
    # Remote bytes: `id` is most of the object and none of it moved; what was fetched is the
    # order of the `bucket` column plus the tail read.
    assert fs.step_downloads == 0
    assert cols["id"] > 0.9 * size
    assert 0 < fs.step_fetched < 0.05 * size, (fs.step_fetched, size, cols)
    assert fs.step_fetched <= 2 * (cols["bucket"] + TAIL), (fs.step_fetched, cols)

    # The reader's plan, on the reader the worker uses for a local artifact and for a remote
    # one: the scan projects `bucket` and nothing else.
    for source in (local, EVENTS):
        relation = _artifacts.deserialize(source, "parquet", frame_type="duckdb")
        scan = scanned(relation.aggregate("bucket, count(*) as n").order("bucket").explain())
        assert re.findall(r"Projections:\s*(\w+)", scan) == ["bucket"], scan
        assert not re.search(r"\b(id|name)\b", scan), scan


def test_bucket_3_pushes_its_filter_and_projection_into_the_scan(
    events, tmp_path, monkeypatch, example
):
    """ "polars: filter and project on the lazy frame; the step's result is what is written"."""
    fs, local, _ = events
    out = run(tmp_path, monkeypatch, example, "bucket_3", "events", "polars_lazy")
    assert fs.step_downloads == 0  # read in place, not downloaded
    written = _artifacts.deserialize(out.replace(".json", ".parquet"), "parquet")
    assert list(written.columns) == ["id", "name"] and len(written) == ROWS // 10
    assert written["id"].min() == 3

    # Local: the filter is part of the parquet scan (SELECTION), not a FILTER node applied
    # after a full read.
    lazy = _artifacts.deserialize(local, "parquet", frame_type="polars_lazy")
    assert isinstance(lazy, pl.LazyFrame)
    plan = lazy.filter(pl.col("bucket") == 3).select("id", "name").explain()
    assert "Parquet SCAN" in plan and "FILTER" not in plan, plan
    assert re.search(r"SELECTION:.*bucket", plan), plan


def test_first_ids_converts_only_the_small_result_to_pandas(events, tmp_path, monkeypatch, example):
    """ "its `.df()` call builds a pandas DataFrame of five rows and one column, and the `name`
    column is never read"."""
    fs, local, cols = events
    # The step's own conversion, observed: the example calls `small.df()`.
    source = example.replace("small.df()", "record(small.df())")
    assert source != example
    source += (
        "\n\ndef record(frame):\n"
        "    import json, pathlib\n"
        "    pathlib.Path('df_shape.json').write_text(json.dumps(\n"
        "        [type(frame).__module__.split('.')[0], *frame.shape]))\n"
        "    return frame\n"
    )
    out = run(tmp_path, monkeypatch, source, "first_ids", "events", "duckdb")
    assert _artifacts.deserialize(out, "json") == {"ids": [3, 13, 23, 33, 43]}
    import json

    assert json.loads(Path("df_shape.json").read_text()) == ["pandas", 5, 1]
    assert fs.step_downloads == 0

    for source_path in (local, EVENTS):
        relation = _artifacts.deserialize(source_path, "parquet", frame_type="duckdb")
        plan = relation.filter("bucket = 3").order("id").limit(5).project("id").explain()
        assert not re.search(r"\bname\b", scanned(plan)), plan


def test_after_events_loads_nothing(events, tmp_path, monkeypatch, example):
    """ "ordering only: runs after `events`, never loads it"."""
    fs, _, _ = events
    source = example.replace('return {"ran": True}', 'return {"ran": True, "got": repr(_events)}')
    assert source != example
    out = run(tmp_path, monkeypatch, source, "after_events", "_events", None)
    assert _artifacts.deserialize(out, "json") == {"ran": True, "got": "None"}
    assert (fs.step_fetched, fs.step_downloads) == (0, 0)


# ─── row skipping needs clustering ───────────────────────────────────────────


def test_a_filter_skips_row_groups_only_when_the_data_is_clustered_on_it(tmp_path, monkeypatch):
    """ "Skipping *rows* by a filter only works when the data is clustered on the filtered
    column ... If the upstream is unsorted on that column, a selective filter still reads every
    row group of the columns it touches. Sort the upstream step's output"."""
    monkeypatch.chdir(tmp_path)
    fs = CountingStore()
    monkeypatch.setitem(_storage._fs_cache, "memory", fs)
    limit = ROWS // 20
    body = f"""
        import duckdb

        def few(events: duckdb.DuckDBPyRelation):
            return events.filter("k < {limit}").aggregate("count(*), sum(v)").fetchone()
        """
    fetched, both, share = {}, {}, {}
    # Same rows; only the order they are written in differs.
    for name, order in (("clustered", "k"), ("unsorted", "hash(k)")):
        local = tmp_path / f"{name}.parquet"
        duckdb.sql(
            f"""copy (select i::bigint as k, random() as v, 'pad-' || i as pad
                      from range({ROWS}) t(i) order by {order})
                to '{local}' (format parquet, row_group_size 100000)"""
        )
        meta = pq.ParquetFile(local).metadata
        assert meta.num_row_groups >= 10
        # Row groups whose min/max statistics for `k` cannot rule the filter out.
        may_match = sum(
            meta.row_group(g).column(0).statistics.min < limit for g in range(meta.num_row_groups)
        )
        share[name] = may_match / meta.num_row_groups
        uri = f"memory://arts/{name}/h1.parquet"
        fs.put_file(str(local), uri)
        fs.fetched = fs.downloads = 0
        out = _run_step(tmp_path, monkeypatch, body, "few", {"events": uri}, {"events": "duckdb"})
        count, _ = _artifacts.deserialize(out, "json")
        assert count == limit and fs.step_downloads == 0
        cols = column_bytes(str(local))
        fetched[name], both[name] = fs.step_fetched, cols["k"] + cols["v"]
        # Columns are skipped either way: `pad`, a third of the file, is never needed.
        assert cols["pad"] > 0.2 * local.stat().st_size

    # Clustered: the statistics rule out all but a couple of row groups, and the bytes follow.
    assert share["clustered"] <= 0.15, share
    assert fetched["clustered"] < 0.25 * both["clustered"], (fetched, both)
    # Unsorted: no row group can be ruled out, so all of both columns is read.
    assert share["unsorted"] == 1.0, share
    assert fetched["unsorted"] >= 0.9 * both["unsorted"], (fetched, both)
    assert fetched["unsorted"] > 4 * fetched["clustered"], fetched


# ─── the topic states no number it did not measure ───────────────────────────


def test_the_topic_quotes_no_timings(topic):
    """Timings depend on the data and the machine, and none is measured by a test, so the
    topic gives none (the figures in #236 were one user's report)."""
    assert not re.search(r"\d+(\.\d+)?\s*(s|sec|seconds|ms|x)\b", topic), "a timing or speed-up"
    assert "22M" not in topic
