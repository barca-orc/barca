"""Batched partitions (RFC-0013): `batch=` groups the keys of a partitioned step that need to
run into fewer calls of its function.

The function receives the list of its call's keys, a partitioned input as a dict of key to
value (or one lazy table), and returns a dict of key to value or a table with the dimension as
a column. Each key keeps the run hash, artifact and materialization it has without batching.

Contract: specs/behavior/execution.md (INV-BATCH-01, INV-BATCH-02, INV-BATCH-03).
"""

import json
import os
import subprocess
import textwrap
from pathlib import Path

import pytest

from barca.api import _find_binary

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

HEADER = """
import json
import pathlib

from barca import Failed, asset, batches, batches_by, collect, partitions, partitions_from, sensor

HERE = pathlib.Path(__file__).parent
CALLS = HERE / "calls.txt"


def _called(name, keys):
    with CALLS.open("a") as f:
        f.write(json.dumps([name, list(keys)]) + "\\n")


@sensor()
def versions() -> tuple[bool, dict]:
    return True, json.loads((HERE / "versions.json").read_text())


@asset(partitions={"retailer": partitions_from(versions)})
def cleaned(retailer: str, versions: str) -> dict:
    return {"retailer": retailer, "version": versions, "amount": int(retailer[1:])}
"""


def env() -> dict[str, str]:
    # Five keys over three workers make unequal chunks; grouping must not follow the
    # bin-packer's stream order (the production CI pool exposed this).
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)} | {
        "BARCA_POOL_SIZE": "3"
    }


def run(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=project, env=env(), capture_output=True, text=True
    )


def get(project: Path, target: str, *flags: str) -> dict:
    proc = run(project, "get", target, "pipeline.py", "--json", *flags)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def value(project: Path, target: str, *flags: str) -> object:
    proc = run(project, "get", target, "pipeline.py", "-o", "value", *flags)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def calls(project: Path) -> list:
    """The calls made since the last look: [name, keys], in order."""
    path = project / "calls.txt"
    lines = path.read_text().splitlines() if path.exists() else []
    path.write_text("")
    return [json.loads(line) for line in lines]


def set_versions(project: Path, versions: dict) -> None:
    (project / "versions.json").write_text(json.dumps(versions))


def write(project: Path, body: str) -> Path:
    (project / "pipeline.py").write_text(HEADER + textwrap.dedent(body))
    return project


def step(result: dict, name: str) -> dict:
    return next(s for s in result["steps"] if s["id"] == f"pipeline.py:{name}")


def outputs(project: Path, result: dict) -> dict:
    """Each key's artifact path (relative: it names the run hash) and bytes."""
    out = {}
    for key, pointer in result["final_output"].items():
        path = pointer["_barca_artifact"]["path"]
        out[key] = (path, (project / path).read_bytes())
    return out


SCORED_DICT = """
def region_of(retailer):
    return "even" if int(retailer[1:]) % 2 == 0 else "odd"


@asset(partitions={"retailer": partitions_from(cleaned)}, batch=BATCH)
def scored(retailer: list[str], cleaned: dict[str, dict]) -> dict:
    _called("scored", retailer)
    return {r: {"retailer": r, "score": cleaned[r]["amount"] * 2} for r in retailer}
"""


@pytest.fixture()
def keys5(tmp_path) -> Path:
    set_versions(tmp_path, {f"r{i}": "a" for i in range(1, 6)})
    return tmp_path


def scored(project: Path, batch: str) -> Path:
    return write(project, SCORED_DICT.replace("BATCH", batch))


# ─── Priority 1: the call ────────────────────────────────────────────────────


def test_one_call_receives_every_key_and_each_key_is_its_own_step(keys5):
    scored(keys5, '"all"')
    res = get(keys5, "scored")
    assert calls(keys5) == [["scored", ["r1", "r2", "r3", "r4", "r5"]]]
    assert value(keys5, "scored")["retailer=r3"] == {"retailer": "r3", "score": 6}
    # One materialization per key, each with its own run hash.
    assert step(res, "scored")["partitions"]["will_run"] == 5
    paths = {p["_barca_artifact"]["path"] for p in res["final_output"].values()}
    assert len(paths) == 5

    # History: one call, its keys under it, all with the call's batch id.
    detail = json.loads(run(keys5, "history", res["run_id"], "--json").stdout)
    keys = [s for s in detail["steps"] if s["node_id"].startswith("pipeline.py:scored[")]
    assert len(keys) == 5 and len({s["batch"] for s in keys}) == 1
    pretty = run(keys5, "history", res["run_id"], "--pretty").stdout
    assert "pipeline.py:scored[5 keys]" in pretty


def test_a_run_reports_calls_and_where_their_time_went(keys5):
    scored(keys5, "2")
    done = run(keys5, "get", "scored", "pipeline.py", "--json", "--agent")
    assert done.returncode == 0, done.stderr
    result = json.loads(done.stdout)
    steps = {s["id"]: s for s in result["steps"]}
    assert "batch_execution" not in steps["pipeline.py:versions"]
    assert "batch_execution" not in steps["pipeline.py:cleaned"]
    calls = steps["pipeline.py:scored"]["batch_execution"]
    assert (calls["calls"], calls["keys"]) == (3, 5)
    for phase in ("import", "load", "compute", "split", "write"):
        assert calls[f"{phase}_seconds"] >= 0
    assert calls["write_seconds"] > 0
    assert result["metadata_seconds"] > 0

    # One line for the node beside its per-key lines, kept in the run's logs too.
    lines = done.stderr.splitlines()
    summary = [line for line in lines if line.startswith("[barca] batch:")]
    assert len(summary) == 1
    assert summary[0].startswith("[barca] batch:pipeline.py:scored 3 calls, 5 keys | import ")
    end_of_run = next(line for line in lines if " steps | done " in line)
    assert lines.index(summary[0]) < lines.index(end_of_run)
    logs = run(keys5, "logs", result["run_id"])
    assert summary[0] in logs.stdout + logs.stderr
    assert "[barca] metadata " in logs.stdout + logs.stderr

    # Nothing ran in a call: no totals, no lines.
    again = run(keys5, "get", "scored", "pipeline.py", "--json", "--agent")
    assert "[barca] batch:" not in again.stderr
    assert "metadata_seconds" not in json.loads(again.stdout)


FAILING_KEYS = """
@asset(partitions={"retailer": partitions_from(cleaned)}, batch=2)
def scored(retailer: list[str], cleaned: dict[str, dict]) -> dict:
    bad = (HERE / "bad.txt").exists()
    return {r: Failed(f"no plan for {r}") if bad and r in ("r2", "r4") else {"r": r} for r in retailer}
"""


def test_a_partitioned_node_reports_its_failed_keys_and_its_runs(keys5):
    from . import served
    from .test_http_openapi_client import validate

    write(keys5, FAILING_KEYS)
    assert run(keys5, "get", "scored", "pipeline.py").returncode == 0
    (keys5 / "bad.txt").touch()
    # New inputs: every key has to run again, and r2 and r4 have no result for them.
    set_versions(keys5, {f"r{i}": "b" for i in range(1, 6)})
    failing = run(keys5, "get", "scored", "pipeline.py", "--json")
    assert failing.returncode == 1, failing.stderr

    stats = json.loads(run(keys5, "stats", "scored", "pipeline.py", "--json").stdout)
    assert stats["total_runs"] == 2
    [latest, first] = stats["recent_runs"]
    assert (latest["status"], latest["keys"], latest["failed_keys"]) == ("failed", 5, 2)
    assert (first["status"], first["keys"], first["failed_keys"]) == ("success", 5, 0)

    with served.Serve(keys5, "--no-schedule", env=env()) as serve:
        serve.ready()
        state = {n["name"]: n for n in serve.get("/state")}
        partitions = state["scored"]["partitions"]
        assert partitions["failed"] == 2
        assert partitions["failed_keys"] == ["retailer=r2", "retailer=r4"]
        assert "failed" not in state["cleaned"]["partitions"]

        failed = serve.get("/assets/scored/partitions?status=failed")
        validate({"$ref": "#/components/schemas/PartitionKeys"}, failed)
        assert [k["key"] for k in failed["keys"]] == ["retailer=r2", "retailer=r4"]
        assert (failed["total"], failed["recorded"], failed["failed"]) == (2, 5, 2)
        # The key's own value does not split one problem into a group per key.
        assert failed["errors"] == [{"error": "BatchKeyFailed: no plan for <key>", "keys": 2}]
        found = serve.get("/assets/scored/partitions?q=R3&limit=1")
        assert [k["key"] for k in found["keys"]] == ["retailer=r3"] and found["keys"][0][
            "artifact_path"
        ]
        page = serve.get("/assets/scored/partitions?limit=2&offset=4")
        assert [k["key"] for k in page["keys"]] == ["retailer=r5"] and page["total"] == 5

        view = serve.get(f"/runs/{latest['run_id']}")
        validate({"$ref": "#/components/schemas/RunView"}, view)
        # Failed nodes first, the rest of the run after it.
        summary, *others = view["steps_page"]["nodes"]
        assert {n["node_id"] for n in others} == {"pipeline.py:versions", "pipeline.py:cleaned"}
        assert summary["node_id"] == "pipeline.py:scored"
        assert (summary["keys"], summary["failed"], summary["calls"]) == (5, 2, 3)

    # The fix: only the failed keys run, and nothing is reported failed any more.
    (keys5 / "bad.txt").unlink()
    fixed = get(keys5, "scored")
    assert fixed["steps_executed"] == 3, "the versions sensor and the two failed keys"
    status = json.loads(run(keys5, "status", "pipeline.py", "--json").stdout)
    nodes = status["nodes"] if isinstance(status, dict) else status
    [node] = [n for n in nodes if n["name"] == "scored"]
    assert "failed" not in node["partitions"]


def test_only_stale_keys_are_grouped(keys5):
    scored(keys5, '"all"')
    get(keys5, "scored")
    calls(keys5)
    set_versions(keys5, {"r1": "a", "r2": "b", "r3": "a", "r4": "b", "r5": "a"})
    res = get(keys5, "scored")
    assert calls(keys5) == [["scored", ["r2", "r4"]]]
    assert step(res, "scored")["partitions"]["will_run_keys"] == ["retailer=r2", "retailer=r4"]
    # Nothing changed: no call at all.
    get(keys5, "scored")
    assert calls(keys5) == []


@pytest.mark.parametrize("source", ["static", "dict"])
def test_batch_boundaries_follow_declared_order_across_worker_chunks(keys5, source):
    order = ["r5", "r1", "r4", "r2", "r3"]
    set_versions(keys5, dict.fromkeys(order, "a"))
    declaration = f"partitions({order!r})" if source == "static" else "partitions_from(cleaned)"
    write(
        keys5,
        f"""
        @asset(partitions={{"retailer": {declaration}}}, batch=2)
        def scored(retailer: list[str], cleaned: dict = None):
            _called("scored", retailer)
            return {{r: cleaned[r] if cleaned else r for r in retailer}}
    """,
    )
    get(keys5, "scored")
    assert sorted(keys for _, keys in calls(keys5)) == sorted([order[:2], order[2:4], order[4:]])


@pytest.mark.parametrize(
    "batch, groups",
    [
        ("2", [["r1", "r2"], ["r3", "r4"], ["r5"]]),
        ('"all"', [["r1", "r2", "r3", "r4", "r5"]]),
        ('batches([["r5", "r1"], ["r2", "r3"]])', [["r1", "r5"], ["r2", "r3"], ["r4"]]),
        ("batches_by(region_of)", [["r1", "r3", "r5"], ["r2", "r4"]]),
        ("batches_by(region_of, max_size=2)", [["r1", "r3"], ["r5"], ["r2", "r4"]]),
        (
            'batches_by(lambda r: 1 if r in ["r1", "r2"] else "1")',
            [["r1", "r2"], ["r3", "r4", "r5"]],
        ),
    ],
)
def test_batching_never_changes_a_cached_result(tmp_path, batch, groups):
    """Each form: computed with batching, every key is a cache hit without it (the same run
    hash and bytes), and the other way round."""
    batched = tmp_path / "batched"
    single = tmp_path / "single"
    for p in (batched, single):
        p.mkdir()
        set_versions(p, {f"r{i}": "a" for i in range(1, 6)})
        scored(p, batch)

    with_batch = get(batched, "scored")
    made = sorted(sorted(keys) for _, keys in calls(batched))
    assert made == sorted(groups)
    # Without batching: every key cached, no call.
    again = get(batched, "scored", "--batch", "none")
    assert calls(batched) == []
    assert step(again, "scored")["partitions"]["cached"] == 5
    assert outputs(batched, again) == outputs(batched, with_batch)

    # The reverse: one key per call first, then batched: every key cached.
    without = get(single, "scored", "--batch", "none")
    assert sorted(calls(single)) == sorted(["scored", [f"r{i}"]] for i in range(1, 6))
    get(single, "scored")
    assert calls(single) == []
    # And the two projects agree key by key: same run hash (the path) and same bytes.
    assert outputs(single, without) == outputs(batched, with_batch)


def test_dict_results_must_cover_the_call_and_nothing_else(keys5):
    write(
        keys5,
        """
        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all")
        def missing(retailer: list[str], cleaned: dict) -> dict:
            return {r: r for r in retailer if r != "r2"}

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all")
        def extra(retailer: list[str], cleaned: dict) -> dict:
            return {**{r: r for r in retailer}, "r99": "x"}

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all")
        def failing(retailer: list[str], cleaned: dict) -> dict:
            return {r: Failed(f"{r} has no data") if r == "r4" else r for r in retailer}
        """,
    )
    proc = run(keys5, "get", "missing", "pipeline.py", "--json")
    assert proc.returncode == 1
    assert "batch returned no result for retailer=r2" in proc.stderr
    assert set(json.loads(proc.stdout)["final_output"]) == {
        "retailer=r1",
        "retailer=r3",
        "retailer=r4",
        "retailer=r5",
    }

    proc = run(keys5, "get", "extra", "pipeline.py", "--json")
    assert proc.returncode == 1
    assert "retailer=r99, which is not one of its keys" in proc.stderr
    detail = json.loads(run(keys5, "history", "--limit", "1", "--json").stdout)["runs"][0]
    rows = json.loads(run(keys5, "history", detail["run_id"], "--json").stdout)["steps"]
    assert {s["status"] for s in rows if "extra[" in s["node_id"]} == {"failed"}

    proc = run(keys5, "get", "failing", "pipeline.py", "--json")
    assert proc.returncode == 1
    assert "r4 has no data" in proc.stderr
    # The other keys succeeded and are cached: only r4 runs again.
    res = json.loads(run(keys5, "get", "failing", "pipeline.py", "--json").stdout)
    assert step(res, "failing")["partitions"]["will_run_keys"] == ["retailer=r4"]


def test_a_call_that_raises_fails_every_key_and_a_retry_reruns_only_failed_keys(keys5):
    write(
        keys5,
        """
        FLAG = HERE / "fail_once.txt"

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all", retries=2)
        def flaky(retailer: list[str], cleaned: dict) -> dict:
            _called("flaky", retailer)
            if FLAG.exists():
                FLAG.unlink()
                return {r: Failed("first attempt") if r in ("r2", "r5") else r for r in retailer}
            return {r: r for r in retailer}

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all")
        def broken(retailer: list[str], cleaned: dict) -> dict:
            print("about to fail")
            raise ValueError("boom")
        """,
    )
    (keys5 / "fail_once.txt").write_text("")
    res = get(keys5, "flaky")
    assert calls(keys5) == [
        ["flaky", ["r1", "r2", "r3", "r4", "r5"]],
        ["flaky", ["r2", "r5"]],
    ]
    assert step(res, "flaky")["attempts"] == 2

    proc = run(keys5, "get", "broken", "pipeline.py", "--json")
    assert proc.returncode == 1
    run_id = json.loads(proc.stdout)["run_id"]
    rows = json.loads(run(keys5, "history", run_id, "--json").stdout)["steps"]
    failed = [s for s in rows if "broken[" in s["node_id"]]
    assert len(failed) == 5 and all(s["status"] == "failed" for s in failed)
    assert all("boom" in s["error_message"] for s in failed)
    # The call's output is recorded once and found from any of its keys.
    logs = run(keys5, "logs", run_id, "--step", "broken[retailer=r3]", "--pretty")
    assert logs.returncode == 0, logs.stderr
    assert "about to fail" in logs.stdout
    assert failed[0]["output_tail"] == ["about to fail"]


# ─── Failed keys block their own keys (#764) ─────────────────────────────────

CHAIN = (
    FAILING_KEYS
    + """
@asset()
def prices() -> int:
    return 2

# A second input ends the planner's chain: `promo` and `orders` are a phase after `scored`.
@asset(partitions={"retailer": partitions_from(scored)}, batch="all", inputs={"prices": prices})
def promo(retailer: list[str], scored: dict, prices: int) -> dict:
    _called("promo", retailer)
    return {r: {"promo": scored[r]["r"] * prices} for r in retailer}

@asset(partitions={"retailer": partitions_from(promo)})
def orders(retailer: str, promo: dict) -> dict:
    return {"orders": promo["promo"]}

@asset(inputs={"orders": collect(orders)})
def summary(orders: list) -> dict:
    return {"n": len(orders)}
"""
)


def skipped_keys(result: dict) -> list[tuple[str, str]]:
    return sorted(
        (s["id"], s["detail"]) for s in result["steps"] if s.get("reason") == "upstream_key_failed"
    )


def test_a_failed_key_blocks_only_its_own_key_down_the_chain_and_a_fan_in_whole(keys5):
    from . import served
    from .test_http_openapi_client import validate

    write(keys5, CHAIN)
    (keys5 / "bad.txt").touch()
    proc = run(keys5, "get", "summary", "pipeline.py", "--json")
    assert proc.returncode == 1, proc.stderr
    res = json.loads(proc.stdout)
    # Two calls of two keys fail; which one is reported first depends on the workers.
    assert res["failed_node"] in {
        "pipeline.py:scored[retailer=r2]",
        "pipeline.py:scored[retailer=r4]",
    }
    assert step(res, "scored")["status"] == "failed"
    # The other three keys of the consumers ran, in one call; the two blocked keys are steps of
    # their own, naming what they read.
    assert step(res, "promo")["status"] == "ran"
    assert step(res, "promo")["partitions"]["will_run"] == 3
    assert step(res, "orders")["status"] == "ran"
    assert calls(keys5) == [["promo", ["r1", "r3", "r5"]]]
    assert skipped_keys(res) == [
        ("pipeline.py:orders[retailer=r2]", "depends on 'promo[retailer=r2]', which was skipped"),
        ("pipeline.py:orders[retailer=r4]", "depends on 'promo[retailer=r4]', which was skipped"),
        ("pipeline.py:promo[retailer=r2]", "depends on 'scored[retailer=r2]', which failed"),
        ("pipeline.py:promo[retailer=r4]", "depends on 'scored[retailer=r4]', which failed"),
    ]
    # The fan-in reads every key: skipped whole, as before.
    summary = step(res, "summary")
    assert (summary["status"], summary["reason"]) == ("skipped", "upstream_failed")
    assert summary["detail"] == "depends on 'scored', which failed"
    assert "5 downstream steps skipped (exit 1)" in proc.stderr

    # The consumer alone: its computed keys are cached, the blocked ones skipped again.
    proc = run(keys5, "get", "promo", "pipeline.py", "--json")
    assert proc.returncode == 1, proc.stderr
    res = json.loads(proc.stdout)
    assert step(res, "promo")["status"] == "cached"
    assert sorted(res["final_output"]) == ["retailer=r1", "retailer=r3", "retailer=r5"]
    assert [k for k, _ in skipped_keys(res)] == [
        "pipeline.py:promo[retailer=r2]",
        "pipeline.py:promo[retailer=r4]",
    ]
    assert calls(keys5) == []

    # Status: the consumers are blocked, not stale because of their upstream.
    status = json.loads(run(keys5, "status", "pipeline.py", "--json").stdout)
    nodes = {n["name"]: n for n in status["nodes"]}
    assert (nodes["promo"]["cache"]["state"], nodes["promo"]["cache"]["reason"]) == (
        "blocked",
        "upstream_failed",
    )
    assert (
        nodes["promo"]["cache"]["detail"]
        == "2 of 2 missing keys wait on 'scored': the same keys of it failed"
    )
    assert nodes["orders"]["cache"]["state"] == "blocked"
    assert "scored" in nodes["orders"]["cache"]["detail"]
    assert nodes["scored"]["partitions"]["failed"] == 2
    assert nodes["summary"]["cache"]["state"] == "never_run"
    assert status["summary"]["blocked"] == 2
    only = json.loads(run(keys5, "status", "pipeline.py", "--state", "blocked", "--json").stdout)
    assert {n["name"] for n in only["nodes"]} == {"promo", "orders"}
    table = run(keys5, "status", "pipeline.py", "--pretty").stdout
    assert "2 of 2 missing keys wait on 'scored'" in table and "2 blocked," in table
    with served.Serve(keys5, "--no-schedule", env=env()) as serve:
        serve.ready()
        state = serve.get("/state")
        validate({"type": "array", "items": {"$ref": "#/components/schemas/NodeState"}}, state)
        by_name = {n["name"]: n for n in state}
        assert by_name["promo"]["cache"]["state"] == "blocked"
        assert by_name["promo"]["partitions"]["missing"] == 2

    # The fix is upstream: only the blocked keys run below it.
    (keys5 / "bad.txt").unlink()
    res = get(keys5, "summary")
    assert step(res, "scored")["partitions"]["will_run"] == 2
    assert step(res, "promo")["partitions"]["will_run"] == 2
    assert calls(keys5) == [["promo", ["r2", "r4"]]]
    assert step(res, "summary")["status"] == "ran"
    assert skipped_keys(res) == []
    status = json.loads(run(keys5, "status", "pipeline.py", "--json").stdout)
    assert status["summary"]["blocked"] == 0


def test_within_one_phase_a_skipped_key_is_its_own_step(tmp_path):
    # Static keys: the planner fuses the chain into one phase, and the coordinator skips the key
    # that reads the failed one while the others run.
    write(
        tmp_path,
        """
        @asset(partitions={"k": partitions(["a", "b", "c"])})
        def first(k: str) -> dict:
            if k == "b":
                raise ValueError("no b")
            return {"k": k}

        @asset(partitions={"k": partitions(["a", "b", "c"])}, inputs={"first": first})
        def second(k: str, first: dict) -> dict:
            return {"k": first["k"]}
        """,
    )
    proc = run(tmp_path, "get", "second", "pipeline.py", "--json")
    assert proc.returncode == 1, proc.stderr
    res = json.loads(proc.stdout)
    assert step(res, "first")["status"] == "failed"
    assert step(res, "second")["status"] == "ran"
    assert sorted(res["final_output"]) == ["k=a", "k=c"]
    assert skipped_keys(res) == [
        ("pipeline.py:second[k=b]", "depends on 'first[k=b]', which failed")
    ]
    assert "1 downstream steps skipped (exit 1)" in proc.stderr


# ─── Priority 2 and 3: tables ────────────────────────────────────────────────

TABLES = """
import duckdb
import pandas as pd
import polars as pl
import pyarrow as pa
import pyarrow.dataset as ds


@asset(partitions={"retailer": partitions_from(versions)})
def rows(retailer: str) -> pd.DataFrame:
    n = int(retailer[1:])
    return pd.DataFrame({"retailer": [retailer] * n, "amount": list(range(n))})


@asset(partitions={"retailer": partitions_from(rows)}, batch="all")
def by_duckdb(retailer: list[str], rows: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    _called("by_duckdb", retailer)
    return rows.aggregate("retailer, sum(amount)::BIGINT AS total, count(*) AS n")


@asset(partitions={"retailer": partitions_from(rows)}, batch="all")
def by_polars(retailer: list[str], rows: pl.LazyFrame) -> pl.DataFrame:
    return rows.group_by("retailer").agg(pl.col("amount").sum().alias("total")).collect()


@asset(partitions={"retailer": partitions_from(rows)}, batch="all")
def by_arrow(retailer: list[str], rows: ds.Dataset) -> pa.Table:
    return rows.to_table().group_by("retailer").aggregate([("amount", "sum")])


@asset(partitions={"retailer": partitions_from(rows)}, batch="all")
def by_pandas(retailer: list[str], rows: dict[str, pd.DataFrame]) -> pd.DataFrame:
    frame = pd.concat([rows[r] for r in retailer])
    return frame[frame["amount"] > 0].groupby("retailer", as_index=False)["amount"].sum()


@asset(partitions={"retailer": partitions_from(rows)}, batch="all")
def extra_rows(retailer: list[str], rows: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return duckdb.sql("SELECT 'r99' AS retailer, 1 AS x")


@asset(partitions={"retailer": partitions_from(rows)}, batch="all")
def no_column(retailer: list[str], rows: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return rows.aggregate("sum(amount) AS total")


@asset(partitions={"retailer": partitions_from(versions)})
def not_a_table(retailer: str) -> dict:
    return {"x": 1}


@asset(partitions={"retailer": partitions_from(not_a_table)}, batch="all")
def lazy_over_json(retailer: list[str], not_a_table: duckdb.DuckDBPyRelation) -> dict:
    return {r: 1 for r in retailer}
"""


@pytest.fixture()
def tables(tmp_path) -> Path:
    set_versions(tmp_path, {"r1": "a", "r2": "a", "r3": "a"})
    return write(tmp_path, TABLES)


def test_table_outputs_are_split_by_the_dimension_column(tables):
    assert value(tables, "by_duckdb") == {
        "retailer=r1": [{"retailer": "r1", "total": 0, "n": 1}],
        "retailer=r2": [{"retailer": "r2", "total": 1, "n": 2}],
        "retailer=r3": [{"retailer": "r3", "total": 3, "n": 3}],
    }
    assert calls(tables) == [["by_duckdb", ["r1", "r2", "r3"]]]
    assert value(tables, "by_polars")["retailer=r3"] == [{"retailer": "r3", "total": 3}]
    assert value(tables, "by_arrow")["retailer=r2"] == [{"retailer": "r2", "amount_sum": 1}]
    by_pandas = value(tables, "by_pandas")
    # r1 has no row with amount > 0: an empty table with the schema.
    assert by_pandas["retailer=r1"] == []
    assert by_pandas["retailer=r3"] == [{"retailer": "r3", "amount": 3}]
    # Each key's artifact is a table a one-key consumer reads like any other.
    res = get(tables, "by_duckdb")
    path = res["final_output"]["retailer=r2"]["_barca_artifact"]["path"]
    assert path.endswith(".parquet")


def test_a_table_with_rows_for_another_key_or_no_dimension_column_fails_the_call(tables):
    proc = run(tables, "get", "extra_rows", "pipeline.py")
    assert proc.returncode == 1
    assert "retailer=r99, which is not one of its keys" in proc.stderr
    proc = run(tables, "get", "no_column", "pipeline.py")
    assert proc.returncode == 1
    assert "has no column retailer" in proc.stderr


def test_a_lazy_input_over_a_non_tabular_upstream_names_the_dict_form(tables):
    proc = run(tables, "get", "lazy_over_json", "pipeline.py")
    assert proc.returncode == 1
    assert "Annotate it dict[str, T]" in proc.stderr


# ─── Priority 4: groups ──────────────────────────────────────────────────────


@pytest.mark.parametrize("group_expression", ["GROUPS", '[[FIRST, "r2"], ["r3"]]'])
def test_strict_groups_and_evaluated_groups(keys5, group_expression):
    write(
        keys5,
        """
        GROUPS = [["r1", "r2"], ["r3"]]
        FIRST = "r1"

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch=batches(GROUPS))
        def evaluated(retailer: list[str], cleaned: dict) -> dict:
            _called("evaluated", retailer)
            return {r: r for r in retailer}

        @asset(
            partitions={"retailer": partitions_from(cleaned)},
            batch=batches([["r1", "r2"]], strict=True),
        )
        def strict(retailer: list[str], cleaned: dict) -> dict:
            return {r: r for r in retailer}
        """.replace("batches(GROUPS)", f"batches({group_expression})"),
    )
    get(keys5, "evaluated")
    assert sorted(k for _, k in calls(keys5)) == [["r1", "r2"], ["r3"], ["r4"], ["r5"]]
    proc = run(keys5, "get", "strict", "pipeline.py")
    assert proc.returncode == 2
    assert "strict=True" in proc.stderr and "retailer=r3" in proc.stderr
    last = json.loads(run(keys5, "history", "--limit", "1", "--json").stdout)["runs"][0]
    assert last["status"] == "failed"


def test_a_grouping_that_cannot_be_evaluated_is_a_load_error_naming_the_node(keys5):
    write(
        keys5,
        """
        def explode():
            raise RuntimeError("no groups today")

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch=batches(explode()))
        def scored(retailer: list[str], cleaned: dict) -> dict:
            return {r: r for r in retailer}
        """,
    )
    proc = run(keys5, "get", "scored", "pipeline.py")
    assert proc.returncode == 2
    assert "pipeline.py:scored" in proc.stderr and "no groups today" in proc.stderr

    write(
        keys5,
        """
        @asset(
            partitions={"retailer": partitions_from(cleaned)},
            batch=batches_by(lambda r: None),
        )
        def scored(retailer: list[str], cleaned: dict) -> dict:
            return {r: r for r in retailer}
        """,
    )
    proc = run(keys5, "get", "scored", "pipeline.py")
    assert proc.returncode == 2
    assert "a label must be a string or an integer" in proc.stderr


@pytest.mark.parametrize(
    "decorator, says",
    [
        ('batch=0, partitions={"retailer": partitions(["a"])}', "at least 1"),
        ('batch=-2, partitions={"retailer": partitions(["a"])}', "at least 1"),
        ('batch="many", partitions={"retailer": partitions(["a"])}', "many"),
        ('batch="all"', "declares no partitions="),
        ('batch=batches([[]]), partitions={"retailer": partitions(["a"])}', "empty group"),
        (
            'batch=batches([["a"], ["a"]]), partitions={"retailer": partitions(["a"])}',
            "in two groups",
        ),
    ],
)
def test_invalid_batch_values_are_usage_errors(tmp_path, decorator, says):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, batches, partitions\n\n"
        f"@asset({decorator})\n"
        "def scored(retailer: list[str] = None) -> dict:\n    return {}\n"
    )
    proc = run(tmp_path, "get", "scored", "pipeline.py")
    assert proc.returncode == 2, proc.stderr
    assert says in proc.stderr


def test_a_scalar_annotation_and_a_sensor_are_refused(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, partitions, sensor\n\n"
        '@asset(partitions={"retailer": partitions(["a"])}, batch=2)\n'
        "def scored(retailer: str) -> dict:\n    return {}\n"
    )
    proc = run(tmp_path, "get", "scored", "pipeline.py")
    assert proc.returncode == 2
    assert "annotate it `list[str]`" in proc.stderr
    (tmp_path / "pipeline.py").write_text(
        "from barca import partitions, sensor\n\n"
        '@sensor(partitions={"retailer": partitions(["a"])}, batch=2)\n'
        "def polled(retailer: list[str]) -> tuple[bool, dict]:\n    return True, {}\n"
    )
    proc = run(tmp_path, "get", "polled", "pipeline.py")
    assert proc.returncode == 2
    assert "batch" in proc.stderr


# ─── Priority 5: the override ────────────────────────────────────────────────


def test_the_per_run_override(keys5):
    scored(keys5, "2")
    get(keys5, "scored", "--batch", "all", "--refresh", "scored")
    assert calls(keys5) == [["scored", ["r1", "r2", "r3", "r4", "r5"]]]
    get(keys5, "scored", "--batch", "scored=3", "--refresh", "scored")
    assert sorted(k for _, k in calls(keys5)) == [["r1", "r2", "r3"], ["r4", "r5"]]
    get(keys5, "scored", "--batch", "all", "--batch", "scored=none", "--refresh", "scored")
    assert len(calls(keys5)) == 5

    proc = run(keys5, "get", "scored", "pipeline.py", "--batch", "cleaned=all")
    assert proc.returncode == 2
    assert "'cleaned' does not declare batch=" in proc.stderr
    proc = run(keys5, "get", "scored", "pipeline.py", "--batch", "nope=all")
    assert proc.returncode == 2
    proc = run(keys5, "get", "scored", "pipeline.py", "--batch", "0")
    assert proc.returncode == 2

    # The Python API passes it on.
    import barca

    cwd = os.getcwd()
    os.chdir(keys5)
    try:
        barca.get("scored", "pipeline.py", refresh=["scored"], batch={"scored": "all"})
    finally:
        os.chdir(cwd)
    assert calls(keys5) == [["scored", ["r1", "r2", "r3", "r4", "r5"]]]


# ─── Priority 6: observability ───────────────────────────────────────────────


def test_the_dry_run_says_how_the_keys_are_grouped(keys5):
    scored(keys5, "2")
    get(keys5, "cleaned")
    proc = run(keys5, "get", "scored", "pipeline.py", "--dry-run", "--json")
    assert proc.returncode == 0, proc.stderr
    s = step(json.loads(proc.stdout), "scored")
    assert s["batch"] == {"grouping": "batch=2", "keys": 5, "calls": 3}
    assert s["detail"].endswith("5 keys in 3 calls (batch=2)")
    proc = run(keys5, "get", "scored", "pipeline.py", "--dry-run", "--batch", "all")
    assert "5 keys in 1 call (batch=all)" in proc.stdout


# ─── Priority 7: composition ─────────────────────────────────────────────────


def test_partition_narrows_then_groups(keys5):
    scored(keys5, '"all"')
    get(keys5, "scored", "--partition", "retailer=r2,r4")
    assert calls(keys5) == [["scored", ["r2", "r4"]]]


def test_cutoff_sinks_and_collect_stay_per_key(keys5):
    write(
        keys5,
        """
        import pandas as pd

        from barca import sink

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all", cutoff=True)
        def stable(retailer: list[str], cleaned: dict) -> dict:
            _called("stable", retailer)
            return {r: {"retailer": r} for r in retailer}

        @asset(partitions={"retailer": partitions_from(stable)}, batch="all")
        def downstream(retailer: list[str], stable: dict[str, dict]) -> dict:
            _called("downstream", retailer)
            return {r: stable[r]["retailer"] for r in retailer}

        @sink("out/")
        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all")
        def table(retailer: list[str], cleaned: dict) -> pd.DataFrame:
            return pd.DataFrame({"retailer": retailer, "x": [1] * len(retailer)})

        @asset(inputs={"all_rows": collect(downstream)})
        def summary(all_rows: list) -> list:
            return sorted(all_rows)
        """,
    )
    assert value(keys5, "summary") == ["r1", "r2", "r3", "r4", "r5"]
    calls(keys5)
    # A dataset sink writes one directory per key.
    get(keys5, "table")
    assert sorted(p.name for p in (keys5 / "out").iterdir() if p.is_dir()) == [
        f"retailer=r{i}" for i in range(1, 6)
    ]
    # cutoff: r3's value changes, `stable` reruns r3 with the same bytes, downstream stays cached.
    set_versions(keys5, {"r1": "a", "r2": "a", "r3": "b", "r4": "a", "r5": "a"})
    get(keys5, "summary")
    assert calls(keys5) == [["stable", ["r3"]]]


SCHEDULED = """
import json
import pathlib

from barca import Schedule, asset, partitions_from, sensor

HERE = pathlib.Path(__file__).parent
CALLS = HERE / "calls.txt"


def _called(name, keys):
    with CALLS.open("a") as f:
        f.write(json.dumps([name, list(keys)]) + "\\n")


@sensor(freshness=Schedule("* * * * * *", batch=2))
def versions() -> tuple[bool, dict]:
    return True, json.loads((HERE / "versions.json").read_text())


@asset(partitions={"retailer": partitions_from(versions)}, batch="all")
def scored(retailer: list[str], versions: dict[str, str]) -> dict:
    _called("scored", retailer)
    return {r: {"retailer": r, "version": versions[r]} for r in retailer}
"""


def test_under_serve_a_schedule_and_a_run_request_override_the_grouping(tmp_path):
    from . import served
    from .test_http_openapi_client import validate

    (tmp_path / "pipeline.py").write_text(SCHEDULED)
    set_versions(tmp_path, {f"r{i}": "a" for i in range(1, 6)})
    with served.Serve(tmp_path, env=env()) as serve:

        def scored_keys():
            rows = serve.rows()
            return sum(n for k, n in rows.items() if k.startswith("scored["))

        serve.wait("the first tick", lambda: scored_keys() >= 5)
        serve.settle(2)
        # Schedule(..., batch=2) overrides batch="all" for its ticks.
        assert sorted(k for _, k in calls(tmp_path)) == [["r1", "r2"], ["r3", "r4"], ["r5"]]
        set_versions(tmp_path, {**{f"r{i}": "a" for i in range(1, 6)}, "r4": "b"})
        serve.wait("the tick after the change", lambda: scored_keys() >= 6)
        serve.settle(2)
        assert calls(tmp_path) == [["scored", ["r4"]]]

        # A run request's batch.
        answer = serve.post(
            "/runs",
            {"command": "get", "targets": ["scored"], "refresh": ["scored"], "batch": ["all"]},
        )
        view = serve.finished(answer["run_id"])
        assert view["status"] == "success", view
        validate({"$ref": "#/components/schemas/RunView"}, view)
        made = [c for c in calls(tmp_path) if c[1] == ["r1", "r2", "r3", "r4", "r5"]]
        assert made, "the refresh ran as one call"
        keys = [s for s in view["steps"] if s["node_id"].startswith("pipeline.py:scored[")]
        assert len(keys) == 5 and len({s["batch"] for s in keys}) == 1
        request = {
            "command": "get",
            "targets": ["scored"],
            "refresh": ["scored"],
            "batch": ["2"],
            "dry_run": True,
        }
        validate({"$ref": "#/components/schemas/RunRequest"}, request)
        predicted = serve.post("/runs", request)
        validate({"$ref": "#/components/schemas/Explain"}, predicted)
        assert step(predicted, "scored")["batch"]["grouping"] == "batch=2"
        # A bad value is a 400.
        import urllib.error

        with pytest.raises(urllib.error.HTTPError) as bad:
            serve.post("/runs", {"command": "get", "targets": ["scored"], "batch": ["0"]})
        assert bad.value.code == 400


LEDGER = """
from barca import asset, partitions


@asset(partitions={"retailer": partitions(["r1", "r2", "r3", "r4"])}, batch="all")
def scored(retailer: list[str]) -> dict:
    return {r: {"retailer": r} for r in retailer}
"""


def test_with_a_server_the_ledger_stays_per_key(tmp_path):
    """A laptop computes in one call; another laptop, one key per call, hits every key in the
    server's ledger: lookups and posts are per key."""
    from .test_server_ledger import Server, _free_port, toml

    store = tmp_path / "store"
    store.mkdir()
    port = _free_port()
    url = f"http://127.0.0.1:{port}"
    roots = {name: tmp_path / name for name in ("server", "a", "b")}
    for name, root in roots.items():
        root.mkdir()
        (root / "pipeline.py").write_text(LEDGER)
        (root / "barca.toml").write_text(toml(store, None if name == "server" else url))
    server = Server(roots["server"], port)
    try:
        a = run(roots["a"], "get", "scored", "pipeline.py", "--json")
        assert a.returncode == 0, a.stderr
        assert step(json.loads(a.stdout), "scored")["partitions"]["will_run"] == 4
        b = run(roots["b"], "get", "scored", "pipeline.py", "--json", "--batch", "none")
        assert b.returncode == 0, b.stderr
        result = json.loads(b.stdout)
        assert result["steps_executed"] == 0, result
        assert step(result, "scored")["partitions"]["cached"] == 4
    finally:
        server.stop()


def test_compound_keys_receive_aligned_lists_and_return_tuple_keys(tmp_path):
    (tmp_path / "pipeline.py").write_text("""
from barca import asset, partitions, batches_by

def region(key):
    return key["region"]

@asset(partitions={"region": partitions(["eu", "us"]),
                   "month": partitions(["jan", "feb"])}, batch=batches_by(region))
def scored(region: list[str], month: list[str]):
    assert len(set(region)) == 1
    return {(m, r): m + ":" + r for m, r in zip(month, region)}
""")
    assert value(tmp_path, "scored") == {
        "month=jan,region=eu": "jan:eu",
        "month=feb,region=eu": "feb:eu",
        "month=jan,region=us": "jan:us",
        "month=feb,region=us": "feb:us",
    }
    result = get(tmp_path, "scored", "--batch", "none")
    assert result["steps_executed"] == 0


def test_table_keys_preserve_reserved_looking_and_escaped_values(tmp_path):
    (tmp_path / "pipeline.py").write_text("""
import duckdb
from barca import asset, partitions

@asset(partitions={"retailer": partitions(["NULL", "a/b", "r=1", "café"])}, batch="all")
def scored(retailer: list[str]):
    return duckdb.sql("SELECT unnest(?) AS retailer", params=[retailer])
""")
    result = value(tmp_path, "scored")
    assert {k: rows[0]["retailer"] for k, rows in result.items()} == {
        f"retailer={k}": k for k in ["NULL", "a/b", "r=1", "café"]
    }


@pytest.mark.parametrize("kind", ["duckdb", "polars", "arrow"])
def test_compound_lazy_inputs_keep_every_dimension(tmp_path, kind):
    annotation, body = {
        "duckdb": ("duckdb.DuckDBPyRelation", "return cleaned"),
        "polars": ("pl.LazyFrame", "return cleaned.collect()"),
        "arrow": ("ds.Dataset", "return cleaned.to_table()"),
    }[kind]
    (tmp_path / "pipeline.py").write_text(f"""
import duckdb
import pandas as pd
import polars as pl
import pyarrow.dataset as ds
from barca import asset, partitions

@asset(partitions={{"region": partitions(["eu", "us"]), "month": partitions(["jan", "feb"])}})
def cleaned(region, month):
    return pd.DataFrame({{"amount": [len(region + month)]}})

@asset(inputs={{{"cleaned"!r}: cleaned}}, partitions={{"region": partitions(["eu", "us"]), "month": partitions(["jan", "feb"])}}, batch="all")
def scored(region: list[str], month: list[str], cleaned: {annotation}):
    {body}
""")
    result = value(tmp_path, "scored")
    assert len(result) == 4
    for key, rows in result.items():
        assert len(rows) == 1
        row = rows[0]
        assert key == f"month={row['month']},region={row['region']}"
        assert row["amount"] == 5


def test_worker_death_retries_every_unfinished_key_of_the_call(keys5):
    write(
        keys5,
        """
        import os

        @asset(partitions={"retailer": partitions_from(cleaned)}, batch="all", retries=2)
        def scored(retailer: list[str], cleaned: dict):
            _called("scored", retailer)
            marker = HERE / "died"
            if not marker.exists():
                marker.touch()
                os._exit(7)
            return {r: cleaned[r] for r in retailer}
    """,
    )
    result = get(keys5, "scored")
    assert calls(keys5) == [["scored", ["r1", "r2", "r3", "r4", "r5"]]] * 2
    assert step(result, "scored")["attempts"] == 2
    assert len(result["final_output"]) == 5


def test_weight_is_charged_per_call_and_limits_parallel_calls(tmp_path):
    (tmp_path / "barca.toml").write_text("[execution]\ncapacity = 2\npool_size = 2\n")
    (tmp_path / "pipeline.py").write_text("""
import fcntl
import json
import time
from pathlib import Path
from barca import asset, partitions

def record(delta):
    with Path("concurrency").open("a+") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        f.seek(0)
        active, peak = json.loads(f.read() or "[0, 0]")
        active += delta
        f.seek(0)
        f.truncate()
        f.write(json.dumps([active, max(peak, active)]))

@asset(partitions={"retailer": partitions(["a", "b", "c", "d"])}, batch=2, weight=2)
def scored(retailer: list[str]):
    record(1)
    time.sleep(0.2)
    record(-1)
    return {r: r for r in retailer}
""")
    result = get(tmp_path, "scored")
    assert len(result["final_output"]) == 4
    assert json.loads((tmp_path / "concurrency").read_text()) == [0, 1]


@pytest.mark.parametrize("table", [False, True])
def test_batched_tasks_work_through_run_python_and_server_capacity(tmp_path, monkeypatch, table):
    import barca
    from .served import Serve

    (tmp_path / "pipeline.py").write_text(
        """
import pandas as pd
from barca import task, partitions

@task(partitions={"retailer": partitions(["a", "b", "c"])}, batch="all")
def job(retailer: list[str]):
    return {r: r.upper() for r in retailer}
""".replace(
            "return {r: r.upper() for r in retailer}",
            'return pd.DataFrame({"retailer": retailer, "value": [r.upper() for r in retailer]})'
            if table
            else "return {r: r.upper() for r in retailer}",
        )
    )
    cli = run(tmp_path, "run", "job", "pipeline.py", "--json")
    assert cli.returncode == 0, cli.stderr
    assert len(json.loads(cli.stdout)["final_output"]) == 3
    monkeypatch.chdir(tmp_path)
    expected = {
        f"retailer={k}": ([{"retailer": k, "value": k.upper()}] if table else k.upper())
        for k in ("a", "b", "c")
    }
    assert barca.run("job", "pipeline.py", batch={"job": 1}) == expected
    with Serve(tmp_path, "--no-schedule", "--capacity", "1", env=env()) as serve:
        serve.ready()
        # Three keys need only one capacity admission: the call is the unit of weight.
        assert (
            barca.run("job", "pipeline.py", batch="all", server=f"http://127.0.0.1:{serve.port}")
            == expected
        )
