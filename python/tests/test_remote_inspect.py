"""`barca status` shapes and `barca sql` over artifacts that live in remote storage (#213).

Status reads a remote artifact through the fsspec filesystem the workers use: a parquet footer
by ranged reads (never the whole object), json and pickle by a download capped in size. `barca
sql` fetches the artifacts a query names into `.barca/sql-cache/` and queries the copies.

The first half runs in-process against fsspec's memory:// filesystem. The second half runs the
real CLI against the S3 emulator the backend suite uses (MinIO) and is skipped when it is not
reachable: memory:// does not cross processes.
"""

import json
import os
import pickle
import shutil
import socket
import subprocess
import threading
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit

import pytest

from barca import _inspect, _sql, _storage
from barca.api import _find_binary

pa = pytest.importorskip("pyarrow")
pq = pytest.importorskip("pyarrow.parquet")
pytest.importorskip("fsspec")


@pytest.fixture(autouse=True)
def _clean():
    _inspect._store_down.clear()
    yield
    _inspect._store_down.clear()
    fs = _storage._fs_cache.get("memory")
    if fs is not None:
        fs.store.clear()


def _put(uri: str, data: bytes) -> None:
    fs = _storage.get_fs(uri)
    fs.makedirs(uri.rsplit("/", 1)[0], exist_ok=True)
    fs.pipe_file(uri, data)


def _parquet_bytes(table, **kw) -> bytes:
    sink = pa.BufferOutputStream()
    pq.write_table(table, sink, **kw)
    return sink.getvalue().to_pybytes()


class _CountingFile:
    """A file that counts the bytes read through it."""

    def __init__(self, f, fs):
        self._f, self._fs = f, fs

    def read(self, n=-1):
        data = self._f.read(n)
        self._fs.bytes_read += len(data)
        return data

    def __getattr__(self, name):
        return getattr(self._f, name)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self._f.close()


class _CountingFS:
    """The memory filesystem, counting what is read from it."""

    def __init__(self):
        self._fs = _storage.get_fs("memory://")
        self.bytes_read = 0
        self.downloads = 0

    def open(self, path, mode="rb", **kw):
        self.open_options = kw
        return _CountingFile(self._fs.open(path, mode), self)

    def cat_file(self, path):
        self.downloads += 1
        data = self._fs.cat_file(path)
        self.bytes_read += len(data)
        return data

    def __getattr__(self, name):
        return getattr(self._fs, name)


@pytest.fixture
def counting(monkeypatch):
    fs = _CountingFS()
    monkeypatch.setattr(_storage, "get_fs", lambda path: fs)
    return fs


# ─── status shape, in process ─────────────────────────────────────────────────


def test_remote_parquet_shape_is_rows_and_columns():
    table = pa.table({"id": [1, 2, 3], "name": ["a", "b", None]})
    _put("memory://arts/orders/h.parquet", _parquet_bytes(table))
    assert _inspect.shape("memory://arts/orders/h.parquet", "parquet") == {
        "type": "table",
        "rows": 3,
        "columns": [{"name": "id", "type": "int64"}, {"name": "name", "type": "string"}],
    }


def test_remote_parquet_is_read_from_its_footer_not_downloaded(counting):
    import random

    rng = random.Random(0)
    n = 200_000
    table = pa.table({c: [rng.random() for _ in range(n)] for c in "abcd"})
    data = _parquet_bytes(table, row_group_size=10_000, compression="none")
    assert len(data) > 6_000_000
    _put("memory://arts/big/h.parquet", data)

    shape = _inspect.shape("memory://arts/big/h.parquet", "parquet")
    assert shape["rows"] == n and len(shape["columns"]) == 4
    assert counting.downloads == 0
    assert counting.bytes_read < len(data) / 20, "the shape must come from the footer alone"
    # The drivers' default read-ahead (50 MB on S3) would fetch most of the object for a sample.
    assert counting.open_options == {"block_size": 1024 * 1024}

    counting.bytes_read = 0
    sampled = _inspect.shape("memory://arts/big/h.parquet", "parquet", sample=3)
    assert len(sampled["sample"]) == 3 and set(sampled["sample"][0]) == set("abcd")
    assert counting.downloads == 0
    assert counting.bytes_read < len(data) / 4, "a sample must not read every row group"


def test_remote_json_and_pickle_shapes_with_a_sample():
    rows = [{"id": 1, "name": "a"}, {"id": 2, "name": None}]
    _put("memory://arts/keys/h.json", json.dumps(rows).encode())
    _put("memory://arts/cfg/h.json", json.dumps({"a": 1, "b": 2}).encode())
    _put("memory://arts/blob/h.pkl", pickle.dumps({1, 2}))

    keys = _inspect.shape("memory://arts/keys/h.json", "json", sample=1)
    assert keys["type"] == "list" and keys["rows"] == 2
    assert keys["columns"] == [
        {"name": "id", "type": "int"},
        {"name": "name", "type": "str | null"},
    ]
    assert keys["sample"] == rows[:1]
    cfg = _inspect.shape("memory://arts/cfg/h.json", "json", sample=1)
    assert cfg == {"type": "dict", "keys": ["a", "b"], "sample": {"a": 1}}
    assert _inspect.shape("memory://arts/blob/h.pkl", "pickle", sample=5) == {"type": "set"}


@pytest.mark.parametrize("fmt, ext", [("json", "json"), ("pickle", "pkl")])
def test_a_remote_json_or_pickle_over_the_cap_is_not_downloaded(fmt, ext, counting, monkeypatch):
    monkeypatch.setattr(_inspect, "MAX_REMOTE_BYTES", 1024 * 1024)
    uri = f"memory://arts/big/h.{ext}"
    _put(uri, b" " * (3 * 1024 * 1024 + 1))
    shape = _inspect.shape(uri, fmt, sample=2)
    assert shape == {"note": f"remote {fmt} artifact too large to inspect: 3 MB (limit 1 MB)"}
    assert counting.downloads == 0 and counting.bytes_read == 0


def test_the_default_cap_is_sixteen_megabytes():
    assert _inspect.MAX_REMOTE_BYTES == 16 * 1024 * 1024


def test_a_missing_remote_artifact_is_a_note():
    for fmt, ext in (("parquet", "parquet"), ("json", "json"), ("pickle", "pkl")):
        assert _inspect.shape(f"memory://arts/gone/h.{ext}", fmt) == {
            "note": "artifact file not found"
        }
    assert _inspect._store_down == {}, "a missing object is not a store failure"


def test_a_missing_driver_is_a_note_naming_the_extra(monkeypatch):
    import fsspec

    def no_driver(protocol, **kw):
        raise ImportError("Install s3fs to access S3")

    monkeypatch.setattr(fsspec, "filesystem", no_driver)
    monkeypatch.delitem(_storage._fs_cache, "s3", raising=False)
    shape = _inspect.shape("s3://bucket/arts/orders/h.parquet", "parquet")
    assert shape == {
        "note": "could not read remote artifact: s3:// paths require the 's3fs' package "
        "(pip install 'barca[s3]')"
    }


def test_bad_storage_options_are_a_note(monkeypatch):
    monkeypatch.setenv("BARCA_STORAGE_OPTIONS", "{not json")
    monkeypatch.delitem(_storage._fs_cache, "memory", raising=False)
    shape = _inspect.shape("memory://arts/keys/h.json", "json")
    assert shape["note"].startswith("could not read remote artifact: BARCA_STORAGE_OPTIONS")


class _DownFS:
    """A store that cannot be reached; counts the attempts."""

    def __init__(self, error):
        self.error, self.attempts = error, 0

    def _fail(self, *a, **kw):
        self.attempts += 1
        raise self.error

    open = size = cat_file = info = get_file = _fail


def test_a_credential_or_network_failure_is_a_note_and_is_not_retried_per_artifact(monkeypatch):
    down = _DownFS(PermissionError("Forbidden\nRequestId: abc"))
    monkeypatch.setattr(_storage, "get_fs", lambda path: down)
    arts = [{"path": f"s3://b/arts/n{i}/h.json", "format": "json"} for i in range(40)]
    arts.append({"path": "s3://b/arts/t/h.parquet", "format": "parquet"})
    shapes = _inspect.shapes(arts)
    assert len(shapes) == 41
    for s in shapes:
        assert s["note"].startswith("could not read remote artifact: PermissionError: Forbidden")
        assert "RequestId" not in s["note"]
    # The first readers hit the failure; the rest report it without another round trip.
    assert down.attempts <= _inspect.MAX_REMOTE_READERS
    assert any("not retried" in s["note"] for s in shapes)


def test_a_corrupt_remote_parquet_does_not_mark_the_store_down():
    _put("memory://arts/bad/h.parquet", b"this is not parquet")
    _put("memory://arts/keys/h.json", b"[1, 2]")
    bad = _inspect.shape("memory://arts/bad/h.parquet", "parquet")
    assert bad["note"].startswith("could not read remote artifact: Arrow")
    assert _inspect.shape("memory://arts/keys/h.json", "json") == {"type": "list", "rows": 2}


def test_shapes_keep_order_and_read_remote_artifacts_concurrently(tmp_path, monkeypatch):
    local = tmp_path / "local.json"
    local.write_text('{"k": 1}')
    arts = [{"path": str(local), "format": "json"}]
    for i in range(20):
        _put(f"memory://arts/n{i}/h.json", json.dumps([i] * (i + 1)).encode())
        arts.append({"path": f"memory://arts/n{i}/h.json", "format": "json"})
    arts.append({"path": str(tmp_path / "gone.json"), "format": "json"})

    real = _storage.get_fs("memory://")
    lock = threading.Lock()
    state = {"now": 0, "peak": 0}

    class Slow:
        def size(self, path):
            with lock:
                state["now"] += 1
                state["peak"] = max(state["peak"], state["now"])
            time.sleep(0.05)
            with lock:
                state["now"] -= 1
            return real.size(path)

        def __getattr__(self, name):
            return getattr(real, name)

    slow = Slow()
    monkeypatch.setattr(_storage, "get_fs", lambda path: slow)
    start = time.monotonic()
    shapes = _inspect.shapes(arts)
    elapsed = time.monotonic() - start

    assert shapes[0] == {"type": "dict", "keys": ["k"]}
    assert [s["rows"] for s in shapes[1:21]] == list(range(1, 21))
    assert shapes[21] == {"note": "artifact file not found"}
    assert 1 < state["peak"] <= _inspect.MAX_REMOTE_READERS
    assert elapsed < 20 * 0.05, "remote shapes must not be read one after another"


# ─── barca sql, in process ────────────────────────────────────────────────────


def _request(tmp_path, query, views):
    return {"query": query, "limit": 100, "cache_dir": str(tmp_path / "sql-cache"), "views": views}


def _view(name, fmt, *uris, partitions=None):
    files = [
        {"path": u, "partition": partitions[i] if partitions else None} for i, u in enumerate(uris)
    ]
    return {"name": name, "node": f"p.py:{name}", "format": fmt, "files": files}


def _cached_files(tmp_path) -> list[str]:
    root = tmp_path / "sql-cache"
    return sorted(
        str(p.relative_to(root)) for p in root.rglob("*") if p.is_file() and p.suffix != ".version"
    )


def test_sql_fetches_the_remote_views_a_query_names_and_only_those(tmp_path):
    pytest.importorskip("duckdb")
    _put("memory://b/arts/orders/h1.parquet", _parquet_bytes(pa.table({"id": [1, 2, 3]})))
    _put("memory://b/arts/other/h2.parquet", _parquet_bytes(pa.table({"x": [1]})))
    _put("memory://b/arts/keys/h3.json", b'[{"k": "a"}, {"k": "b"}]')
    views = [
        _view("orders", "parquet", "memory://b/arts/orders/h1.parquet"),
        _view("other", "parquet", "memory://b/arts/other/h2.parquet"),
        _view("keys", "json", "memory://b/arts/keys/h3.json"),
    ]
    q = "select (select count(*) from Orders) as n, (select count(*) from keys) as k"
    out = _sql.run(_request(tmp_path, q, views))
    assert out["rows"] == [{"n": 3, "k": 2}]
    assert out["fetched"]["files"] == 2 and out["fetched"]["bytes"] > 0
    assert out["unreachable"] == {} and out["unavailable"] == {}
    # The cache mirrors the URI; the view the query does not name was not fetched.
    assert _cached_files(tmp_path) == [
        "memory/b/arts/keys/h3.json",
        "memory/b/arts/orders/h1.parquet",
    ]


def test_sql_reuses_a_fetched_copy_until_the_object_changes(tmp_path, monkeypatch):
    pytest.importorskip("duckdb")
    uri = "memory://b/arts/orders/h1.parquet"
    _put(uri, _parquet_bytes(pa.table({"id": [1, 2, 3]})))
    views = [_view("orders", "parquet", uri)]
    q = "select count(*) as n from orders"
    assert _sql.run(_request(tmp_path, q, views))["fetched"]["files"] == 1

    real = _storage.get_fs(uri)
    downloads = []

    class Spy:
        def get_file(self, src, dst):
            downloads.append(src)
            return real.get_file(src, dst)

        def __getattr__(self, name):
            return getattr(real, name)

    spy = Spy()
    monkeypatch.setattr(_storage, "get_fs", lambda path: spy)
    again = _sql.run(_request(tmp_path, q, views))
    assert again["rows"] == [{"n": 3}]
    assert again["fetched"] == {"files": 0, "bytes": 0} and downloads == []

    # `--refresh` rewrites the same content-addressed path: the copy must not go stale.
    _put(uri, _parquet_bytes(pa.table({"id": [1, 2, 3, 4, 5]})))
    refreshed = _sql.run(_request(tmp_path, q, views))
    assert refreshed["rows"] == [{"n": 5}]
    assert refreshed["fetched"]["files"] == 1 and downloads == [uri]


def test_sql_reads_a_remote_partitioned_view(tmp_path):
    pytest.importorskip("duckdb")
    for key, units in (("w1", 1), ("w2", 2)):
        _put(f"memory://b/arts/weekly_{key}/h.parquet", _parquet_bytes(pa.table({"u": [units]})))
    view = _view(
        "weekly",
        "parquet",
        "memory://b/arts/weekly_w1/h.parquet",
        "memory://b/arts/weekly_w2/h.parquet",
        partitions=["week=w1", "week=w2"],
    )
    out = _sql.run(_request(tmp_path, "select partition, u from weekly order by 1", [view]))
    assert out["rows"] == [{"partition": "week=w1", "u": 1}, {"partition": "week=w2", "u": 2}]


def test_show_tables_lists_remote_views_without_fetching_them(tmp_path):
    pytest.importorskip("duckdb")
    _put("memory://b/arts/orders/h1.parquet", _parquet_bytes(pa.table({"id": [1]})))
    views = [_view("orders", "parquet", "memory://b/arts/orders/h1.parquet")]
    out = _sql.run(_request(tmp_path, "show tables", views))
    assert out["rows"] == [{"name": "orders"}]
    assert out["fetched"] == {"files": 0, "bytes": 0} and _cached_files(tmp_path) == []


def test_sql_reports_a_remote_view_it_could_not_fetch(tmp_path, monkeypatch):
    pytest.importorskip("duckdb")
    views = [_view("orders", "parquet", "memory://b/arts/orders/gone.parquet")]
    out = _sql.run(_request(tmp_path, "select * from orders", views))
    assert out["error"]["kind"] == "missing_table" and out["error"]["table"] == "orders"
    assert out["unreachable"]["orders"]["kind"] == "fetch"
    assert "is not in the remote store" in out["unreachable"]["orders"]["reason"]

    def no_driver(path):
        raise ImportError("s3:// paths require the 's3fs' package (pip install 'barca[s3]')")

    monkeypatch.setattr(_storage, "get_fs", no_driver)
    views = [_view("orders", "parquet", "s3://b/arts/orders/h.parquet")]
    out = _sql.run(_request(tmp_path, "select * from orders", views))
    assert out["unreachable"]["orders"] == {
        "kind": "driver",
        "reason": "s3:// paths require the 's3fs' package (pip install 'barca[s3]')",
    }
    assert not [p for p in (tmp_path / "sql-cache").rglob("*.tmp")]


def test_cache_path_mirrors_the_uri_and_refuses_to_leave_the_cache():
    assert _sql.cache_path("c", "S3://bucket/proj/default/artifacts/n/h.parquet") == os.path.join(
        "c", "s3", "bucket", "proj", "default", "artifacts", "n", "h.parquet"
    )
    for bad in ("s3://bucket/../../etc/passwd", "s3://", "s3://bucket//x"):
        with pytest.raises(ValueError):
            _sql.cache_path("c", bad)


# ─── the CLI against an S3 emulator ───────────────────────────────────────────

S3_ENDPOINT = os.environ.get("BARCA_TEST_S3_ENDPOINT", "http://localhost:9100")
S3_KEY = os.environ.get("BARCA_TEST_S3_KEY", "minioadmin")
S3_SECRET = os.environ.get("BARCA_TEST_S3_SECRET", "minioadmin")
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
import pandas as pd
from barca import asset, partitions


@asset()
def orders() -> pd.DataFrame:
    return pd.DataFrame({"id": [1, 2, 3, 4], "region": ["emea", "amer", "emea", "apac"]})


@asset()
def keys() -> list:
    return [{"ca": "CA1", "residual": 0.5}, {"ca": "CA2", "residual": None}]


@asset()
def blob() -> set:
    return {1, 2}


@asset(partitions={"week": partitions(["w1", "w2"])})
def weekly(week: str) -> pd.DataFrame:
    return pd.DataFrame({"units": [1 if week == "w1" else 2]})
"""


def _reachable(url: str) -> bool:
    parts = urlsplit(url)
    try:
        with socket.create_connection((parts.hostname, parts.port), timeout=0.5):
            return True
    except OSError:
        return False


def cli(cwd: Path, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
    base = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    creds = {"AWS_ACCESS_KEY_ID": S3_KEY, "AWS_SECRET_ACCESS_KEY": S3_SECRET}
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env={**base, **creds, **(env or {})},
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def envelope(proc: subprocess.CompletedProcess) -> dict:
    return json.loads(proc.stderr.strip().splitlines()[-1])


@pytest.fixture(scope="module")
def remote_project(tmp_path_factory) -> Path:
    """A project whose results exist only in the bucket: locally there is just the metadata DB.

    The endpoint is set in barca.toml alone, so the status and sql helpers reach the bucket only
    if barca hands them the storage options it hands the workers. State stays local
    (`state = "off"`): these tests are about artifacts."""
    pytest.importorskip("pandas")
    pytest.importorskip("s3fs")
    if not _reachable(S3_ENDPOINT):
        pytest.skip(f"s3 emulator not reachable at {S3_ENDPOINT}")
    import fsspec

    bucket = f"barca-inspect-{uuid.uuid4().hex[:8]}"
    fs = fsspec.filesystem(
        "s3", key=S3_KEY, secret=S3_SECRET, endpoint_url=S3_ENDPOINT, skip_instance_cache=True
    )
    fs.mkdir(bucket)
    root = tmp_path_factory.mktemp("remote_project")
    (root / "pipeline.py").write_text(PIPELINE)
    (root / "barca.toml").write_text(
        f'[remote]\nuri = "s3://{bucket}/proj"\nstate = "off"\n\n'
        f'[remote.storage_options.s3]\nendpoint_url = "{S3_ENDPOINT}"\n'
    )
    for target in ("orders", "keys", "blob", "weekly"):
        ok(cli(root, "get", target, "--json"))
    # Workers write locally and the copies are uploaded; drop them to leave only the bucket's.
    shutil.rmtree(root / ".barca" / "artifacts")
    return root


def _shapes(proc: subprocess.CompletedProcess) -> dict:
    return {n["name"]: n["shape"] for n in ok(proc)["nodes"]}


def test_status_shows_the_shape_of_remote_artifacts(remote_project):
    status = ok(cli(remote_project, "status", "--json", "--sample", "1"))
    assert all(
        n["last_materialization"]["artifact"].startswith("s3://") for n in status["nodes"]
    ), status
    shapes = {n["name"]: n["shape"] for n in status["nodes"]}
    assert shapes["orders"]["type"] == "table" and shapes["orders"]["rows"] == 4
    assert [c["name"] for c in shapes["orders"]["columns"]] == ["id", "region"]
    assert shapes["orders"]["sample"] == [{"id": 1, "region": "emea"}]
    assert shapes["keys"]["rows"] == 2
    assert shapes["keys"]["columns"] == [
        {"name": "ca", "type": "str"},
        {"name": "residual", "type": "float | null"},
    ]
    assert shapes["keys"]["sample"] == [{"ca": "CA1", "residual": 0.5}]
    assert shapes["blob"] == {"type": "set"}
    assert shapes["weekly"]["type"] == "table" and shapes["weekly"]["rows"] == 1

    table = cli(remote_project, "status", "orders", "--pretty")
    assert table.returncode == 0, table.stderr
    assert "4 rows x 2 cols" in table.stdout


def test_status_with_bad_credentials_still_succeeds_and_says_so_in_note(remote_project):
    proc = cli(remote_project, "status", "--json", env={"AWS_SECRET_ACCESS_KEY": "wrong"})
    shapes = _shapes(proc)
    assert set(shapes) == {"orders", "keys", "blob", "weekly"}
    for shape in shapes.values():
        assert set(shape) == {"note"}
        assert shape["note"].startswith("could not read remote artifact: "), shape


def _without_s3fs(tmp_path: Path) -> dict:
    """An environment where importing s3fs fails, as if the extra were not installed."""
    stub = tmp_path / "no_s3fs" / "s3fs"
    stub.mkdir(parents=True)
    (stub / "__init__.py").write_text("raise ImportError(\"No module named 's3fs'\")\n")
    return {"PYTHONPATH": str(stub.parent)}


def test_status_without_the_driver_still_succeeds_and_names_the_extra(remote_project, tmp_path):
    proc = cli(remote_project, "status", "orders", "--json", env=_without_s3fs(tmp_path))
    note = _shapes(proc)["orders"]["note"]
    assert "pip install 'barca[s3]'" in note, note


def test_sql_queries_results_that_are_only_in_the_bucket(remote_project):
    cache = remote_project / ".barca" / "sql-cache"
    first = cli(
        remote_project, "sql", "select region, count(*) as n from orders group by 1", "--json"
    )
    rows = sorted(ok(first)["rows"], key=lambda r: r["region"])
    assert rows == [
        {"region": "amer", "n": 1},
        {"region": "apac", "n": 1},
        {"region": "emea", "n": 2},
    ]
    assert "fetched 1 remote artifact" in first.stderr and ".barca/sql-cache/" in first.stderr
    copies = [p for p in cache.rglob("*.parquet")]
    assert len(copies) == 1 and "pipeline.py--orders" in str(copies[0])
    assert str(copies[0].relative_to(cache)).startswith("s3/barca-inspect-")

    # The copy is reused; only what the next query names is fetched.
    again = cli(remote_project, "sql", "select count(*) as n from orders", "--json")
    assert ok(again)["rows"] == [{"n": 4}]
    assert "fetched" not in again.stderr

    weekly = cli(remote_project, "sql", "select partition, units from weekly order by 1", "--json")
    assert ok(weekly)["rows"] == [
        {"partition": "week=w1", "units": 1},
        {"partition": "week=w2", "units": 2},
    ]
    assert "fetched 2 remote artifacts" in weekly.stderr
    keys = cli(remote_project, "sql", "select ca from keys where residual is null", "--json")
    assert ok(keys)["rows"] == [{"ca": "CA2"}]

    pickled = cli(remote_project, "sql", "select * from blob", "--json")
    assert pickled.returncode == 2 and "pickle" in envelope(pickled)["error"]


def test_sql_that_cannot_fetch_says_why(remote_project, tmp_path):
    # A different query target than the cached `orders`, so nothing local can answer it.
    q = "select * from keys"
    for copy in (remote_project / ".barca" / "sql-cache").rglob("*.json*"):
        copy.unlink()
    denied = cli(remote_project, "sql", q, "--json", env={"AWS_SECRET_ACCESS_KEY": "wrong"})
    assert denied.returncode == 3, denied.stderr
    env = envelope(denied)
    assert env["kind"] == "infra"
    assert "'keys' is in remote storage and could not be fetched" in env["error"]
    assert "barca docs remote" in env["remediation"]

    no_driver = cli(remote_project, "sql", q, "--json", env=_without_s3fs(tmp_path))
    assert no_driver.returncode == 2, no_driver.stderr
    assert "pip install 'barca[s3]'" in envelope(no_driver)["error"]


# ─── every backend ────────────────────────────────────────────────────────────


@pytest.mark.parametrize("backend", ["s3", "gcs", "azure"])
def test_status_and_sql_read_from_each_cloud(backend, tmp_path):
    """The same reads through s3fs, gcsfs and adlfs, configured by environment variables, from a
    second machine: a fresh directory that has neither the artifacts nor (until barca pulls the
    shared history) the metadata."""
    pytest.importorskip("pandas")
    from .test_remote_env_config import CASES
    from .test_remote_env_config import _reachable as reachable

    endpoint, make = CASES[backend]
    if not reachable(endpoint):
        pytest.skip(f"{backend} emulator not reachable at {endpoint}")
    uri, env = make()
    env = {**env, "BARCA_REMOTE_URI": uri}
    machine_a, machine_b = tmp_path / "a", tmp_path / "b"
    for machine in (machine_a, machine_b):
        machine.mkdir()
        (machine / "pipeline.py").write_text(PIPELINE)
    for target in ("orders", "keys", "blob"):
        ok(cli(machine_a, "get", target, "--json", env=env))
    tmp_path = machine_b

    status = ok(cli(tmp_path, "status", "--json", "--sample", "1", env=env))
    shapes = {n["name"]: n["shape"] for n in status["nodes"]}
    scheme = uri.split("://")[0]
    assert all(
        n["last_materialization"]["artifact"].startswith(f"{scheme}://")
        for n in status["nodes"]
        if n["last_materialization"]
    )
    assert shapes["orders"]["rows"] == 4, shapes
    assert shapes["orders"]["sample"] == [{"id": 1, "region": "emea"}]
    assert shapes["keys"]["rows"] == 2 and shapes["keys"]["sample"] == [
        {"ca": "CA1", "residual": 0.5}
    ]
    assert shapes["blob"] == {"type": "set"}

    q = "select count(*) as n from orders o, keys k"
    first = cli(tmp_path, "sql", q, "--json", env=env)
    assert ok(first)["rows"] == [{"n": 8}]
    assert "fetched 2 remote artifacts" in first.stderr
    again = cli(tmp_path, "sql", q, "--json", env=env)
    assert ok(again)["rows"] == [{"n": 8}]
    assert "fetched" not in again.stderr, "an unchanged object must not be downloaded twice"
