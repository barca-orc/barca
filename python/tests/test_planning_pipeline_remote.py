"""The planning pipeline (#250) with its sources and artifacts in an object store (MinIO).

- The three source modes compute the same results at the small size.
- Machine A runs the whole graph in `store` mode with barca's client going through a counting
  proxy (`counting_proxy.py`), so every byte barca reads from the store is measured. Its peak
  RSS is bounded.
- Machine B is a fresh checkout after A. Since 0.26 a store holds no shared history: a machine
  gets A's records with `barca state restore` (`barca docs remote`, "A wiped laptop or a fresh
  container"), and sharing between people means a server. After the restore B executes nothing
  and fetches only what it reads.
- Bytes fetched for `forecast_reconciled`, the upstream with 34 consumers: a lazy
  (`DuckDBPyRelation`) consumer fetches a fraction of the object, a pandas consumer fetches it
  whole once per machine, and once it is on the machine no later run fetches it.

The remote world runs at scale 8 (about 46k forecast rows): at scale 1 the central object is
3 KB, smaller than one ranged read, so "a fraction of the object" cannot be measured there. The
whole graph takes a few seconds at either size. The large size is a benchmark
(`benchmarks/planning_pipeline/bench.sh`), not run here.

Skipped without the S3 emulator, failed when `BARCA_TEST_S3_ENDPOINT` declares it provided.
"""

from __future__ import annotations

import collections

import pytest

from .counting_proxy import CountingProxy
from .planning_pipeline_store import S3_ENDPOINT, Bucket, Machine, new_machine

pd = pytest.importorskip("pandas")

REMOTE_SCALE = 8
CENTRAL = "forecast_reconciled"  # 34 consumers
LAZY_CONSUMER = "validate_conversion_factors_item_week"  # counts the input's rows
PANDAS_CONSUMER = "validate_plan_grid"  # takes it as pd.DataFrame
# Peak resident memory of A's full run (4 workers, each importing duckdb, pandas, polars and
# pyarrow), measured at about 1.6 GB for the tree and 450 MB for the largest process. The
# bounds leave room for runner noise and catch a worker that holds frames it should not (#232).
TREE_RSS_BOUND = 3 * 1024**3
PROCESS_RSS_BOUND = 1024**3


@pytest.fixture(scope="module")
def bucket():
    b = Bucket()
    yield b
    b.remove()


@pytest.fixture(scope="module")
def proxy():
    p = CountingProxy(S3_ENDPOINT)
    yield p
    p.close()


def short(node_id: str) -> str:
    return node_id.split(":", 1)[1]


def frame(machine: Machine, node: dict) -> pd.DataFrame:
    """An asset's result, sorted, so results of different runs compare equal."""
    df = pd.read_parquet(machine.local_copy(node))
    return df.sort_values(list(df.columns), ignore_index=True)


def run_everything(machine: Machine, project) -> None:
    tasks = ",".join(project.names("task"))
    cold = machine.ok("run", tasks, "assets.py", "--json")
    assert cold["status"] == "success"
    machine.ok("get", "assets.py", "--json")


# ─── Source modes ─────────────────────────────────────────────────────────────


def test_three_source_modes_compute_the_same_results(tmp_path, bucket):
    """local directory, object store, and "store for sources, local for barca", small size."""
    results = {}
    for mode in ("local", "store", "hybrid"):
        machine, project = new_machine(tmp_path / mode, mode, bucket)
        run_everything(machine, project)
        nodes = machine.status()
        assets = {n: node for n, node in nodes.items() if node["kind"] == "asset"}
        assert {node["cache"]["state"] for node in assets.values()} == {"cached"}, mode
        locations = {node["cache"]["artifact"].split("://")[0] for node in assets.values()}
        if mode == "store":
            assert locations == {"s3"}
            stored = bucket.fs.find(machine.store_uri + "/default/artifacts")
            assert len({p.rsplit("/", 2)[-2] for p in stored}) >= len(assets)
        else:
            assert all(not loc.startswith("s3") for loc in locations), mode
        results[mode] = {name: frame(machine, node) for name, node in assets.items()}

    # The registry from the published parquet came from the bucket in store and hybrid modes
    # (`data/` was deleted), and every asset agrees across the three modes.
    assert len(results["store"]["retirement_registry"]) > 0
    for mode in ("store", "hybrid"):
        for name, df in results["local"].items():
            pd.testing.assert_frame_equal(results[mode][name], df, check_exact=False, obj=name)


# ─── Machine A, then B ────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def machine_a(tmp_path_factory, bucket, proxy):
    """Machine A: the whole graph in store mode, measured. Yields (machine, project, rss)."""
    root = tmp_path_factory.mktemp("a") / "project"
    machine, project = new_machine(
        root, "store", bucket, scale=REMOTE_SCALE, store_endpoint=proxy.endpoint
    )
    tasks = ",".join(project.names("task"))
    cold, tree_peak, process_peak = machine.measured("run", tasks, "assets.py", "--json")
    assert cold["status"] == "success"
    machine.ok("get", "assets.py", "--json")
    yield machine, project, (tree_peak, process_peak)


def test_peak_rss_of_the_full_run_in_remote_mode(machine_a):
    _, _, (tree_peak, process_peak) = machine_a
    assert 0 < process_peak <= tree_peak or tree_peak == 0
    assert process_peak < PROCESS_RSS_BOUND, f"largest process {process_peak / 2**20:.0f} MiB"
    assert tree_peak < TREE_RSS_BOUND, f"process tree {tree_peak / 2**20:.0f} MiB"


def fresh_machine(machine_a, tmp_path, name: str) -> Machine:
    """Another machine: the same checkout and store, no `.barca/`, A's records restored."""
    a, _, _ = machine_a
    b = a.clone(tmp_path / name)
    restored = b.ok("state", "restore", "--json")
    assert restored["from"] == "store", restored
    return b


def central(machine: Machine) -> tuple[str, int]:
    """The path part (as the proxy sees it) and the size of the central upstream's object."""
    node = machine.status()[CENTRAL]
    path = "/" + node["cache"]["artifact"].split("://", 1)[1]
    return path, node["last_materialization"]["size_bytes"]


def test_machine_b_executes_nothing_and_fetches_only_what_it_reads(machine_a, tmp_path, proxy):
    b = fresh_machine(machine_a, tmp_path, "b")
    target = b.status()["plan_year_summary"]
    proxy.reset()
    out = b.ok("get", "plan_year_summary", "assets.py", "--json")
    assert out["steps_executed"] == 0
    assert {s["status"] for s in out["steps"]} == {"cached"}
    # Only the target's object came down, whole, once; none of its 50-odd upstreams.
    path = "/" + target["cache"]["artifact"].split("://", 1)[1]
    assert proxy.served("/artifacts/") == {path: target["last_materialization"]["size_bytes"]}
    assert b.local_copy(target).exists()


def test_bytes_fetched_for_the_34_consumer_upstream(machine_a, tmp_path, proxy):
    _, project, _ = machine_a
    consumers = [down for up, down in project.edges if up == CENTRAL]
    assert len(consumers) == 34
    # The assets among them are cached on B and do not run; the 16 tasks always run.
    tasks = sorted(set(consumers) & set(project.names("task")))
    assert len(tasks) == 16 and LAZY_CONSUMER in tasks and PANDAS_CONSUMER in tasks

    b = fresh_machine(machine_a, tmp_path, "b")
    path, size = central(b)
    local = b.local_copy(b.status()[CENTRAL])

    # A lazy consumer reads the object in place: a fraction of it, and no local copy. Nothing
    # keeps a ranged read, so running it again on the same machine reads the ranges again
    # (today's behaviour, #577).
    for _ in range(2):
        proxy.reset()
        b.ok("run", LAZY_CONSUMER, "assets.py", "--json")
        fetched = proxy.served(path).get(path, 0)
        assert 0 < fetched < size / 4, f"lazy consumer fetched {fetched} of {size} bytes"
        assert not local.exists()

    # A pandas consumer downloads it whole, once.
    proxy.reset()
    b.ok("run", PANDAS_CONSUMER, "assets.py", "--json")
    assert proxy.responses(path) == [size]
    assert local.stat().st_size == size

    # Now that it is on this machine, no consumer fetches it again.
    proxy.reset()
    out = b.ok("run", ",".join(tasks), "assets.py", "--json")
    assert collections.Counter(s["status"] for s in out["steps"] if short(s["id"]) in tasks) == {
        "ran": 16
    }
    assert proxy.served(path) == {}


def test_consumers_of_the_upstream_in_one_run_fetch_it_whole_once(machine_a, tmp_path, proxy):
    """All 16 consumer tasks in one run on a fresh machine: one whole download, plus the ranged
    reads of lazy consumers that ran before it; a second run fetches nothing."""
    _, project, _ = machine_a
    tasks = sorted(
        {down for up, down in project.edges if up == CENTRAL} & set(project.names("task"))
    )
    c = fresh_machine(machine_a, tmp_path, "c")
    path, size = central(c)

    proxy.reset()
    c.ok("run", ",".join(tasks), "assets.py", "--json")
    reads = proxy.responses(path)
    assert reads.count(size) == 1, reads
    assert all(n < size / 4 for n in reads if n != size), reads

    proxy.reset()
    c.ok("run", ",".join(tasks), "assets.py", "--json")
    assert proxy.served(path) == {}
