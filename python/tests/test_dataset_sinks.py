"""Dataset sinks (#621), conformance for specs/interfaces/artifact-formats.md "Dataset sinks".

`@sink("prefix/")` writes a Hive-partitioned dataset.

What `barca docs sinks` promises, end to end through the real CLI:

- one directory level per partition dimension of the asset, then one per `partition_by`
  column; partition columns are in the path, not in the files;
- the DuckDB, polars and pyarrow writers produce the same layout, read back with DuckDB's
  `hive_partitioning`; parquet, csv and json; `rows_per_file`;
- a cached key writes nothing, a changed key rewrites only its own directory, a removed key's
  directory is deleted;
- the prefix is owned: one with files barca did not write is refused;
- a crash between staging and the swap leaves the old data readable;
- a failed sink fails the step (exit 1, a `step_failed` envelope);
- the size warnings;
- the same against the s3, gcs and azure emulators.
"""

import json
import os
import socket
import subprocess
import textwrap
import uuid
from pathlib import Path
from urllib.parse import urlsplit

import duckdb
import pytest

from barca.api import _find_binary

from . import emulators

WRITERS = ("pandas", "arrow", "polars", "polars_lazy", "duckdb")


def run(cwd: Path, *args: str, env: dict | None = None, check: bool = True):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
        timeout=300,
    )
    if check:
        assert proc.returncode == 0, proc.stdout + proc.stderr
    return proc


def get(cwd: Path, target: str, env: dict | None = None, check: bool = True):
    return run(cwd, "get", target, "pipeline.py", "--json", env=env, check=check)


def envelope(proc) -> dict:
    lines = [line for line in proc.stderr.splitlines() if line.startswith("{")]
    assert lines, proc.stderr
    return json.loads(lines[-1])


def layout(root: Path) -> list[str]:
    """Every file of a dataset, relative to its prefix, the marker left out."""
    return sorted(
        p.relative_to(root).as_posix()
        for p in root.rglob("*")
        if p.is_file() and p.name != "_barca_sink.json"
    )


def read_back(prefix: str, fmt: str, columns: str = "*") -> list[tuple]:
    reader = {"parquet": "read_parquet", "csv": "read_csv", "json": "read_json"}[fmt]
    ext = {"json": "jsonl"}.get(fmt, fmt)
    con = duckdb.connect()
    return con.sql(
        f"select {columns} from {reader}('{prefix}/**/*.{ext}', hive_partitioning = true) "
        "order by all"
    ).fetchall()


# ─── One layout for every writer ─────────────────────────────────────────────

WRITER_PIPELINE = '''
import datetime

import barca
import duckdb
import pandas as pd
import polars as pl
import pyarrow as pa
from barca import asset, partitions, sink

ROWS = {
    "r1": [(1, "2026-10-21", 1.5), (2, "2026-10-22", 2.5), (3, "2026-10-22", 3.5),
           (4, "2026-10-22", 4.5), (5, "2026-10-22", 5.5)],
    "r2": [(6, "2026-10-21", 6.5)],
}


def rows(retailer):
    return [
        {"id": i, "order_date": datetime.date.fromisoformat(d), "amount": a}
        for i, d, a in ROWS[retailer]
    ]


def frame(writer, retailer):
    data = rows(retailer)
    if writer == "pandas":
        return pd.DataFrame(data)
    if writer == "arrow":
        return pa.Table.from_pylist(data)
    if writer == "polars":
        return pl.DataFrame(data)
    if writer == "polars_lazy":
        return pl.DataFrame(data).lazy()
    con = barca.duckdb_connection()
    values = ", ".join(f"({r['id']}, DATE '{r['order_date']}', {r['amount']})" for r in data)
    return con.sql(
        f"select * from (values {values}) as t(id, order_date, amount)"
    )
'''


def writer_assets(sink_args: str, partitioned: bool) -> str:
    out = []
    for writer in WRITERS:
        if partitioned:
            decorator = '@asset(partitions={"retailer": partitions(["r1", "r2"])})'
            signature, call = "retailer: str", "retailer"
        else:
            decorator, signature, call = "@asset()", "", '"r1"'
        out.append(
            f"{decorator}\n@sink(\"out/{writer}/\"{sink_args})\n"
            f"def {writer}_out({signature}):\n    return frame({writer!r}, {call})\n"
        )
    return "\n\n".join(out)


CASES = {
    # name: (sink arguments, partitioned, format)
    "parquet": ("", True, "parquet"),
    "parquet_partition_by": (', partition_by=["order_date"]', True, "parquet"),
    "parquet_rows_per_file": (', partition_by=["order_date"], rows_per_file=2', True, "parquet"),
    "parquet_unpartitioned": (', partition_by=["order_date"]', False, "parquet"),
    "parquet_unpartitioned_rows": (", rows_per_file=2", False, "parquet"),
    "csv": (', serializer="csv"', True, "csv"),
    "csv_partition_by": (', serializer="csv", partition_by=["order_date"]', True, "csv"),
    "json": (', serializer="json"', True, "json"),
    "json_partition_by": (', serializer="json", partition_by=["order_date"]', False, "json"),
}

EXPECTED_LAYOUT = {
    "parquet": ["retailer=r1/part-0.parquet", "retailer=r2/part-0.parquet"],
    "parquet_partition_by": [
        "retailer=r1/order_date=2026-10-21/part-0.parquet",
        "retailer=r1/order_date=2026-10-22/part-0.parquet",
        "retailer=r2/order_date=2026-10-21/part-0.parquet",
    ],
    "parquet_rows_per_file": [
        "retailer=r1/order_date=2026-10-21/part-0.parquet",
        "retailer=r1/order_date=2026-10-22/part-0.parquet",
        "retailer=r1/order_date=2026-10-22/part-1.parquet",
        "retailer=r2/order_date=2026-10-21/part-0.parquet",
    ],
    "parquet_unpartitioned": [
        "order_date=2026-10-21/part-0.parquet",
        "order_date=2026-10-22/part-0.parquet",
    ],
    "parquet_unpartitioned_rows": ["part-0.parquet", "part-1.parquet", "part-2.parquet"],
    "csv": ["retailer=r1/part-0.csv", "retailer=r2/part-0.csv"],
    "csv_partition_by": [
        "retailer=r1/order_date=2026-10-21/part-0.csv",
        "retailer=r1/order_date=2026-10-22/part-0.csv",
        "retailer=r2/order_date=2026-10-21/part-0.csv",
    ],
    "json": ["retailer=r1/part-0.jsonl", "retailer=r2/part-0.jsonl"],
    "json_partition_by": ["order_date=2026-10-21/part-0.jsonl", "order_date=2026-10-22/part-0.jsonl"],
}


@pytest.mark.parametrize("case", list(CASES))
def test_every_writer_writes_the_same_dataset(tmp_path, case):
    sink_args, partitioned, fmt = CASES[case]
    (tmp_path / "pipeline.py").write_text(
        WRITER_PIPELINE + "\n\n" + writer_assets(sink_args, partitioned)
    )
    for writer in WRITERS:
        get(tmp_path, f"{writer}_out")

    layouts = {w: layout(tmp_path / "out" / w) for w in WRITERS}
    assert layouts["pandas"] == EXPECTED_LAYOUT[case], layouts
    for writer in WRITERS:
        assert layouts[writer] == layouts["pandas"], (writer, layouts)

    # One shared check: each dataset read back as one table, with the partition values
    # recovered from the path.
    columns = "id, order_date::varchar, amount" + (", retailer" if partitioned else "")
    expected = read_back(str(tmp_path / "out" / "pandas"), fmt, columns)
    want = 6 if partitioned else 5
    assert len(expected) == want, expected
    for writer in WRITERS:
        assert read_back(str(tmp_path / "out" / writer), fmt, columns) == expected, writer
    if partitioned:
        assert {row[3] for row in expected} == {"r1", "r2"}
    assert {row[1] for row in expected} >= {"2026-10-21", "2026-10-22"}

    if fmt == "parquet":
        # Partition columns live in the path and are left out of the files.
        import pyarrow.parquet as pq

        for writer in WRITERS:
            for rel in layouts[writer]:
                names = pq.read_schema(tmp_path / "out" / writer / rel).names
                assert "retailer" not in names, (writer, rel, names)
                if "order_date=" in rel:
                    assert "order_date" not in names, (writer, rel, names)
    marker = json.loads((tmp_path / "out" / "duckdb" / "_barca_sink.json").read_text())
    assert marker["asset"] == "pipeline.py:duckdb_out"
    assert marker["dims"] == (["retailer"] if partitioned else [])


def test_a_column_named_like_the_dimension_is_left_out_of_the_files(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        textwrap.dedent(
            """
            import pandas as pd
            from barca import asset, partitions, sink


            @asset(partitions={"retailer": partitions(["r1"])})
            @sink("out/")
            def scored(retailer: str) -> pd.DataFrame:
                return pd.DataFrame({"retailer": [retailer], "n": [1]})
            """
        )
    )
    get(tmp_path, "scored")
    import pyarrow.parquet as pq

    assert pq.read_schema(tmp_path / "out/retailer=r1/part-0.parquet").names == ["n"]
    assert read_back(str(tmp_path / "out"), "parquet") == [(1, "r1")]


# ─── Writes only when a key re-runs; removed keys are deleted ────────────────

KEYED_PIPELINE = """
import json

import pandas as pd
from barca import asset, partitions, sensor, sink


@sensor(partitions={{"retailer": partitions({keys})}})
def retailer_version(retailer: str) -> int:
    with open("versions.json") as f:
        return json.load(f)[retailer]


@asset(inputs={{"version": retailer_version}}, partitions={{"retailer": partitions({keys})}})
@sink("exports/scored/", partition_by=["order_date"])
def scored(retailer: str, version: int) -> pd.DataFrame:
    return pd.DataFrame({{
        "order_date": ["2026-10-21", "2026-10-22"],
        "amount": [1.0 * version, 2.0 * version],
    }})
"""


def write_keyed(root: Path, keys: list[str], versions: dict) -> None:
    (root / "pipeline.py").write_text(KEYED_PIPELINE.format(keys=json.dumps(keys)))
    (root / "versions.json").write_text(json.dumps(versions))


def key_inodes(root: Path) -> dict[str, int]:
    return {p.name: p.stat().st_ino for p in sorted(root.glob("retailer=*"))}


def test_only_changed_keys_rewrite_and_removed_keys_are_deleted(tmp_path):
    keys = ["r1", "r2", "r3"]
    write_keyed(tmp_path, keys, {"r1": 1, "r2": 1, "r3": 1})
    get(tmp_path, "scored")
    dataset = tmp_path / "exports/scored"
    assert layout(dataset) == [
        f"retailer={k}/order_date={d}/part-0.parquet"
        for k in keys
        for d in ("2026-10-21", "2026-10-22")
    ]
    files = {p: p.stat().st_mtime_ns for p in dataset.rglob("*.parquet")}
    first = key_inodes(dataset)

    # Nothing changed: every key is cached and nothing is written.
    proc = get(tmp_path, "scored")
    assert json.loads(proc.stdout)["steps_executed"] == 3  # the three sensor keys
    assert {p: p.stat().st_mtime_ns for p in dataset.rglob("*.parquet")} == files
    assert key_inodes(dataset) == first

    # One key changed: only its directory is swapped in.
    (tmp_path / "versions.json").write_text(json.dumps({"r1": 1, "r2": 5, "r3": 1}))
    get(tmp_path, "scored")
    after = key_inodes(dataset)
    assert after["retailer=r1"] == first["retailer=r1"]
    assert after["retailer=r3"] == first["retailer=r3"]
    assert after["retailer=r2"] != first["retailer=r2"]
    for p, mtime in files.items():
        assert (p.stat().st_mtime_ns == mtime) == ("retailer=r2" not in str(p)), p
    totals = duckdb.sql(
        f"select retailer, sum(amount)::int from read_parquet('{dataset}/**/*.parquet', "
        "hive_partitioning = true) group by all order by all"
    ).fetchall()
    assert totals == [
        ("r1", 3),
        ("r2", 15),
        ("r3", 3),
    ]

    # A key removed from the asset: its directory is deleted, the others stay as they are.
    write_keyed(tmp_path, ["r1", "r2"], {"r1": 1, "r2": 5})
    proc = get(tmp_path, "scored")
    assert "dataset sink exports/scored/: deleted 1 key its asset no longer has" in proc.stderr
    assert sorted(key_inodes(dataset)) == ["retailer=r1", "retailer=r2"]
    assert key_inodes(dataset)["retailer=r1"] == first["retailer=r1"]
    # Nothing is left behind next to the dataset (staging) either.
    assert sorted(p.name for p in (tmp_path / "exports").iterdir()) == ["scored"]

    # The keys are as the last prune left them: the next run starts no prune.
    proc = get(tmp_path, "scored")
    assert "deleted" not in proc.stderr


@pytest.mark.parametrize("boundary", ["scan", "mkdir"])
def test_finishing_writer_cannot_remove_another_writers_stage_parent(tmp_path, monkeypatch, boundary):
    from barca import _sinks

    finishing = _sinks._LocalTarget(str(tmp_path / "out"))
    starting = _sinks._LocalTarget(str(tmp_path / "out"))
    previous = finishing.new_stage()
    interrupted = False
    original_scan = _sinks._clean_dead_stages
    original_mkdir = Path.mkdir

    def finish():
        nonlocal interrupted
        interrupted = True
        finishing.drop_stage(previous)
        assert not finishing.staging.exists()

    def scan(path):
        if boundary == "scan" and not interrupted:
            finish()
        return original_scan(path)

    def mkdir(path, *args, **kwargs):
        if boundary == "mkdir" and path.parent == starting.staging and not interrupted:
            finish()
        return original_mkdir(path, *args, **kwargs)

    monkeypatch.setattr(_sinks, "_clean_dead_stages", scan)
    monkeypatch.setattr(Path, "mkdir", mkdir)
    stage = starting.new_stage()
    assert interrupted
    (stage / "data.json").write_text('{"value": 1}')
    assert (stage / "data.json").read_text() == '{"value": 1}'
    starting.drop_stage(stage)
    assert not starting.staging.exists()


def test_a_narrowed_run_deletes_no_key(tmp_path):
    write_keyed(tmp_path, ["r1", "r2"], {"r1": 1, "r2": 1})
    get(tmp_path, "scored")
    proc = run(tmp_path, "get", "scored", "pipeline.py", "--partition", "retailer=r1", "--json")
    assert "deleted" not in proc.stderr
    assert sorted(key_inodes(tmp_path / "exports/scored")) == ["retailer=r1", "retailer=r2"]


def test_an_unpartitioned_dataset_is_replaced_whole(tmp_path):
    (tmp_path / "dates.json").write_text(json.dumps(["2026-10-21", "2026-10-22"]))
    (tmp_path / "pipeline.py").write_text(
        textwrap.dedent(
            """
            import json

            import pyarrow as pa
            from barca import asset, sensor, sink


            @sensor()
            def dates() -> list:
                with open("dates.json") as f:
                    return json.load(f)


            @asset(inputs={"dates": dates})
            @sink("out/orders/", partition_by=["order_date"])
            def orders(dates: list) -> pa.Table:
                return pa.table({"order_date": dates, "n": list(range(len(dates)))})
            """
        )
    )
    get(tmp_path, "orders")
    dataset = tmp_path / "out/orders"
    assert layout(dataset) == [
        "order_date=2026-10-21/part-0.parquet",
        "order_date=2026-10-22/part-0.parquet",
    ]
    (tmp_path / "dates.json").write_text(json.dumps(["2026-10-23"]))
    get(tmp_path, "orders")
    assert layout(dataset) == ["order_date=2026-10-23/part-0.parquet"]
    assert (dataset / "_barca_sink.json").exists()


# ─── Ownership ───────────────────────────────────────────────────────────────

SIMPLE = """
import pandas as pd
from barca import asset, partitions, sink


@asset(partitions={"retailer": partitions(["r1"])})
@sink("out/")
def scored(retailer: str) -> pd.DataFrame:
    return pd.DataFrame({"n": [1]})
"""


def test_a_prefix_with_files_barca_did_not_write_is_refused(tmp_path):
    (tmp_path / "pipeline.py").write_text(SIMPLE)
    (tmp_path / "out").mkdir()
    (tmp_path / "out/report.csv").write_text("theirs\n")
    proc = get(tmp_path, "scored", check=False)
    assert proc.returncode == 1, proc.stderr
    env = envelope(proc)
    assert env["kind"] == "step_failed" and env["code"] == 1, env
    assert "SinkError" in env["error"], env
    assert "holds files barca did not write" in env["error"], env
    assert "report.csv" in env["error"], env
    assert sorted(p.name for p in (tmp_path / "out").iterdir()) == ["report.csv"]
    assert (tmp_path / "out/report.csv").read_text() == "theirs\n"


def test_two_assets_cannot_share_a_prefix(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        SIMPLE
        + textwrap.dedent(
            """

            @asset()
            @sink("out/")
            def other() -> pd.DataFrame:
                return pd.DataFrame({"n": [2]})
            """
        )
    )
    get(tmp_path, "scored")
    proc = get(tmp_path, "other", check=False)
    assert proc.returncode == 1
    assert "belongs to the asset pipeline.py:scored" in envelope(proc)["error"]
    assert layout(tmp_path / "out") == ["retailer=r1/part-0.parquet"]


# ─── A crash mid-write leaves the old data readable ──────────────────────────

CRASH = """
import os

import pandas as pd
from barca import _sinks, asset, partitions, sink

with open("value.txt") as f:
    VALUE = int(f.read())


def crash():
    if os.path.exists("crash"):
        os._exit(1)  # the worker dies after staging, before the swap


_sinks._crash_point = crash


@asset(partitions={"retailer": partitions(["r1"])})
@sink("out/")
def scored(retailer: str) -> pd.DataFrame:
    return pd.DataFrame({"n": [VALUE]})
"""


def test_a_crash_between_staging_and_the_swap_leaves_the_old_data(tmp_path):
    (tmp_path / "pipeline.py").write_text(CRASH.replace("VALUE = int(f.read())", "VALUE = 1"))
    (tmp_path / "value.txt").write_text("1")
    get(tmp_path, "scored")
    assert read_back(str(tmp_path / "out"), "parquet") == [(1, "r1")]

    (tmp_path / "pipeline.py").write_text(CRASH)
    (tmp_path / "value.txt").write_text("2")
    (tmp_path / "crash").write_text("")
    proc = get(tmp_path, "scored", check=False)
    assert proc.returncode != 0, proc.stderr
    # The old key is still there, whole, and readable.
    assert read_back(str(tmp_path / "out"), "parquet") == [(1, "r1")]
    staging = tmp_path / "out/../.out.barca-staging"
    assert staging.exists() and any(staging.iterdir()), "the crashed write's stage is left"

    # The next write cleans up what the crashed one left and swaps the new data in.
    (tmp_path / "crash").unlink()
    get(tmp_path, "scored")
    assert read_back(str(tmp_path / "out"), "parquet") == [(2, "r1")]
    assert not staging.exists()


# ─── Sink failures fail the step ─────────────────────────────────────────────


@pytest.mark.parametrize(
    "sink,body,message",
    [
        # A file sink: the behaviour that changed (it used to be a warning on stderr).
        ('"out.parquet"', "return {'not': 'a table'}", "cannot be written as parquet"),
        ('"out/", partition_by=["missing"]', "return pd.DataFrame({'n': [1]})", "'missing'"),
    ],
)
def test_a_failed_sink_fails_the_step(tmp_path, sink, body, message):
    (tmp_path / "pipeline.py").write_text(
        f"import pandas as pd\nfrom barca import asset, sink\n\n\n@asset()\n@sink({sink})\n"
        f"def exported():\n    {body}\n"
    )
    proc = get(tmp_path, "exported", check=False)
    assert proc.returncode == 1, proc.stderr
    env = envelope(proc)
    assert env["kind"] == "step_failed" and env["node"] == "pipeline.py:exported", env
    assert "SinkError" in env["error"] and message in env["error"], env
    assert json.loads(proc.stdout)["status"] == "failed"
    # The step is not recorded as a success: the next run tries it again.
    proc = get(tmp_path, "exported", check=False)
    assert proc.returncode == 1


def test_dataset_arguments_on_a_file_sink_are_a_usage_error(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, sink\n\n\n@asset()\n"
        '@sink("out.parquet", partition_by=["d"])\ndef exported():\n    return 1\n'
    )
    proc = get(tmp_path, "exported", check=False)
    assert proc.returncode == 2, proc.stderr
    assert "applies to a dataset sink" in proc.stderr


# ─── Size warnings ───────────────────────────────────────────────────────────


def test_size_warnings(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        textwrap.dedent(
            """
            import pyarrow as pa
            from barca import asset, sink


            @asset()
            @sink("out/by_id/", partition_by=["id"])
            def by_id() -> pa.Table:
                return pa.table({"id": list(range(1001)), "n": [1] * 1001})


            @asset()
            @sink("out/tiny/", rows_per_file=1)
            def tiny() -> pa.Table:
                return pa.table({"n": list(range(120))})


            @asset()
            @sink("out/fine/", partition_by=["g"])
            def fine() -> pa.Table:
                return pa.table({"g": [1, 2], "n": [1, 2]})
            """
        )
    )
    proc = get(tmp_path, "by_id")
    assert "[barca] warning: sink out/by_id/: one write created 1001 directories" in proc.stderr
    assert "How to choose partition columns" in proc.stderr
    proc = get(tmp_path, "tiny")
    assert "[barca] warning: sink out/tiny/: one write created 120 files averaging" in (
        proc.stderr
    )
    proc = get(tmp_path, "fine")
    assert "warning" not in proc.stderr, proc.stderr


# ─── Object stores ───────────────────────────────────────────────────────────

S3_ENDPOINT = os.environ.get("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
GCS_ENDPOINT = os.environ.get("BARCA_TEST_GCS_ENDPOINT", "http://localhost:9200")
AZURITE_HOST = os.environ.get("BARCA_TEST_AZURITE_HOST", "127.0.0.1:9210")
AZURITE_CONN = (
    "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;"
    "AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/"
    f"KBHBeksoGMGw==;BlobEndpoint=http://{AZURITE_HOST}/devstoreaccount1;"
)


def _reachable(url_or_host: str) -> bool:
    parts = urlsplit(url_or_host if "://" in url_or_host else f"http://{url_or_host}")
    try:
        with socket.create_connection((parts.hostname, parts.port), timeout=0.5):
            return True
    except OSError:
        return False


def _s3():
    import fsspec

    bucket = f"barca-ds-{uuid.uuid4().hex[:8]}"
    fs = fsspec.filesystem(
        "s3", key="minioadmin", secret="minioadmin", endpoint_url=S3_ENDPOINT,
        skip_instance_cache=True,
    )
    fs.mkdir(bucket)
    env = {
        "AWS_ACCESS_KEY_ID": "minioadmin",
        "AWS_SECRET_ACCESS_KEY": "minioadmin",
        "FSSPEC_S3_ENDPOINT_URL": S3_ENDPOINT,
    }
    return f"s3://{bucket}", fs, env


def _gcs():
    import fsspec
    from google.auth.credentials import AnonymousCredentials
    from google.cloud import storage

    bucket = f"barca-ds-{uuid.uuid4().hex[:8]}"
    storage.Client(
        project="test",
        credentials=AnonymousCredentials(),
        client_options={"api_endpoint": GCS_ENDPOINT},
    ).create_bucket(bucket)
    fs = fsspec.filesystem(
        "gs", endpoint_url=GCS_ENDPOINT, token="anon", project="test", skip_instance_cache=True
    )
    env = {
        "FSSPEC_GCS_ENDPOINT_URL": GCS_ENDPOINT,
        "FSSPEC_GCS_TOKEN": "anon",
        "FSSPEC_GCS_PROJECT": "test",
        "GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT": "false",
    }
    return f"gs://{bucket}", fs, env


def _azure():
    import fsspec
    from azure.storage.blob import BlobServiceClient

    container = f"barca-ds-{uuid.uuid4().hex[:8]}"
    BlobServiceClient.from_connection_string(AZURITE_CONN).create_container(container)
    fs = fsspec.filesystem("abfs", connection_string=AZURITE_CONN, skip_instance_cache=True)
    return f"abfs://{container}", fs, {"AZURE_STORAGE_CONNECTION_STRING": AZURITE_CONN}


STORES = {"s3": (S3_ENDPOINT, _s3), "gcs": (GCS_ENDPOINT, _gcs), "azure": (AZURITE_HOST, _azure)}

REMOTE_PIPELINE = KEYED_PIPELINE.replace('"exports/scored/"', '"{uri}/scored/"')


def objects(fs, root: str) -> list[str]:
    fs.invalidate_cache()
    base = fs._strip_protocol(root)
    try:
        found = fs.find(root)
    except FileNotFoundError:
        return []
    return sorted(p[len(base) :].lstrip("/") for p in found)


@pytest.mark.parametrize("store", list(STORES))
def test_dataset_sinks_on_object_stores(tmp_path, store):
    endpoint, make = STORES[store]
    emulators.require(store, _reachable(endpoint), endpoint)
    uri, fs, env = make()

    def write(keys, versions):
        text = REMOTE_PIPELINE.replace("{uri}", uri).format(keys=json.dumps(keys))
        (tmp_path / "pipeline.py").write_text(text)
        (tmp_path / "versions.json").write_text(json.dumps(versions))

    write(["r1", "r2", "r3"], {"r1": 1, "r2": 1, "r3": 1})
    get(tmp_path, "scored", env=env)
    root = f"{uri}/scored"
    expected = ["_barca_sink.json"] + [
        f"retailer={k}/order_date={d}/part-0.parquet"
        for k in ("r1", "r2", "r3")
        for d in ("2026-10-21", "2026-10-22")
    ]
    assert objects(fs, root) == expected

    def modified(path):
        fs.invalidate_cache()
        info = fs.info(f"{root}/{path}")
        return info.get("LastModified") or info.get("updated") or info.get("last_modified")

    before = {p: modified(p) for p in expected[1:]}
    # One key changed: only its files are written.
    write(["r1", "r2", "r3"], {"r1": 1, "r2": 7, "r3": 1})
    import time

    time.sleep(1.1)
    get(tmp_path, "scored", env=env)
    for p, when in before.items():
        assert (modified(p) == when) == ("retailer=r2" not in p), p

    # A removed key is deleted.
    write(["r1", "r2"], {"r1": 1, "r2": 7})
    proc = get(tmp_path, "scored", env=env)
    assert "deleted 1 key" in proc.stderr, proc.stderr
    assert objects(fs, root) == [p for p in expected if "retailer=r3" not in p]

    # Read back as one dataset, through fsspec.
    import pyarrow.dataset as pds

    table = pds.dataset(
        fs._strip_protocol(root), filesystem=fs, format="parquet", partitioning="hive",
        exclude_invalid_files=True,
    ).to_table()
    totals = {}
    for row in table.to_pylist():
        totals[row["retailer"]] = totals.get(row["retailer"], 0) + row["amount"]
    assert totals == {"r1": 3.0, "r2": 21.0}

    # A foreign prefix is refused.
    fs.pipe(f"{uri}/theirs/data.csv", b"x\n")
    text = (tmp_path / "pipeline.py").read_text().replace("/scored/", "/theirs/")
    (tmp_path / "pipeline.py").write_text(text)
    proc = get(tmp_path, "scored", env=env, check=False)
    assert proc.returncode == 1
    assert "holds files barca did not write" in envelope(proc)["error"]
    assert objects(fs, f"{uri}/theirs") == ["data.csv"]
