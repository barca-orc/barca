"""Which steps run, and every run hash, do not depend on the worker pool size.

The pool size (`BARCA_POOL_SIZE`, by default the machine's core count) decides how a phase's work
is split across workers. It must not decide anything else:

- #330: with chained `partitions_from`, a small pool re-ran keys of the second asset and its
  fan-in after the key source ran again, because their run hashes had been computed from
  whichever upstream keys happened to be hashed first;
- #331: with one worker, the first run of a `collect()` consumer failed with a `TypeError`;
- #325: the same failure at any pool size when the collected asset has one key.

Every test here runs at several pool sizes, including 1 and one larger than any key count used.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

POOLS = [1, 2, 3, 5, 16]


def barca(project: Path, pool: int, *args: str) -> dict:
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, "BARCA_POOL_SIZE": str(pool)},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"pool {pool}: {' '.join(args)}\n{proc.stderr}"
    return json.loads(proc.stdout.strip().splitlines()[-1])


def ran(result: dict) -> dict:
    """What executed: for a partitioned asset the sorted keys that ran, else `True`."""
    out: dict = {}
    for step in result["steps"]:
        name = step["id"].split(":")[-1]
        if "partitions" in step:
            keys = sorted(k.split("=", 1)[1] for k in step["partitions"].get("will_run_keys", []))
            if keys:
                out[name] = keys
        elif step["status"] == "ran":
            out[name] = True
    return out


def write(project: Path, source: str) -> None:
    (project / "barca.toml").write_text("")
    (project / "pipeline.py").write_text(source)


# ─── #330: chained partitions_from ────────────────────────────────────────────


def chained(keys: list[str], note: str = "") -> str:
    """`keys -> sales = partitions_from(keys) -> margin = partitions_from(sales)`, with a
    fan-in over each. `note` edits the body of `keys` without changing what it returns."""
    return f"""
from barca import asset, collect, partitions_from


@asset()
def keys() -> list:
    {note or "pass"}
    return {json.dumps(keys)}


@asset(partitions={{"region": partitions_from(keys)}})
def sales(region: str) -> dict:
    return {{"region": region}}


@asset(partitions={{"region": partitions_from(sales)}})
def margin(region: str, sales: dict) -> dict:
    return {{"region": region, "margin": 1}}


@asset(inputs={{"all_sales": collect(sales)}})
def summary(all_sales: list) -> dict:
    return {{"regions": sorted(s["region"] for s in all_sales)}}


@asset(inputs={{"s": summary, "m": collect(margin)}})
def report(s: dict, m: list) -> dict:
    return {{"regions": s["regions"], "margins": len(m)}}
"""


@pytest.mark.parametrize("pool", POOLS)
def test_rerunning_the_key_source_with_the_same_keys_runs_nothing_else(tmp_path, pool):
    write(tmp_path, chained(["us", "eu", "apac"]))
    first = barca(tmp_path, pool, "get", "report", "pipeline.py")
    assert first["steps_executed"] == 9
    assert barca(tmp_path, pool, "get", "report", "pipeline.py")["steps_executed"] == 0

    write(tmp_path, chained(["us", "eu", "apac"], note="x = 1"))
    again = barca(tmp_path, pool, "get", "report", "pipeline.py")
    assert ran(again) == {"keys": True}  # its code changed; it returned the same keys


@pytest.mark.parametrize("pool", POOLS)
def test_removing_a_key_from_the_source_runs_no_key(tmp_path, pool):
    write(tmp_path, chained(["us", "eu", "apac"]))
    barca(tmp_path, pool, "get", "report", "pipeline.py")

    write(tmp_path, chained(["us", "apac"]))
    shrunk = barca(tmp_path, pool, "get", "report", "pipeline.py")
    # The two fan-ins have one input fewer. No key of `sales` or `margin` runs.
    assert ran(shrunk) == {"keys": True, "summary": True, "report": True}
    assert shrunk["final_output"] == {"regions": ["apac", "us"], "margins": 2}


@pytest.mark.parametrize("pool", POOLS)
def test_reordering_the_keys_of_the_source_runs_nothing_else(tmp_path, pool):
    write(tmp_path, chained(["us", "eu", "apac"]))
    barca(tmp_path, pool, "get", "report", "pipeline.py")

    write(tmp_path, chained(["apac", "us", "eu"]))
    assert ran(barca(tmp_path, pool, "get", "report", "pipeline.py")) == {"keys": True}


@pytest.mark.parametrize("pool", POOLS)
def test_adding_a_key_to_the_source_runs_only_the_new_key(tmp_path, pool):
    write(tmp_path, chained(["us", "eu"]))
    barca(tmp_path, pool, "get", "report", "pipeline.py")

    write(tmp_path, chained(["us", "eu", "apac"]))
    grown = barca(tmp_path, pool, "get", "report", "pipeline.py")
    assert ran(grown) == {
        "keys": True,
        "sales": ["apac"],
        "margin": ["apac"],
        "summary": True,
        "report": True,
    }
    assert grown["final_output"] == {"regions": ["apac", "eu", "us"], "margins": 3}


# ─── #331, #325: a collect() consumer on a cold run ───────────────────────────


def fan_in(keys: list[str]) -> str:
    return f"""
from barca import asset, collect, partitions


@asset(partitions={{"region": partitions({json.dumps(keys)})}})
def sales(region: str) -> dict:
    return {{"region": region}}


@asset(inputs={{"all_sales": collect(sales)}})
def summary(all_sales: list) -> dict:
    return {{"regions": sorted(s["region"] for s in all_sales)}}
"""


@pytest.mark.parametrize("pool", POOLS)
@pytest.mark.parametrize("keys", [["us"], ["us", "eu"], ["us", "eu", "apac", "latam"]], ids=len)
def test_a_collect_consumer_runs_cold(tmp_path, pool, keys):
    write(tmp_path, fan_in(keys))
    first = barca(tmp_path, pool, "get", "summary", "pipeline.py")
    assert first["steps_executed"] == len(keys) + 1
    # One key gives the consumer a list of one, like any other number of keys.
    assert first["final_output"] == {"regions": sorted(keys)}
    assert barca(tmp_path, pool, "get", "summary", "pipeline.py")["steps_executed"] == 0


@pytest.mark.parametrize("pool", POOLS)
def test_a_collect_consumer_of_a_collect_consumer_runs_cold(tmp_path, pool):
    """Every fan-in waits for its own upstream, also when the plan is one stream wide."""
    write(
        tmp_path,
        fan_in(["us", "eu"])
        + """

@asset(partitions={"shard": partitions(["a"])}, inputs={"s": summary})
def shard(shard: str, s: dict) -> dict:
    return {"shard": shard, "n": len(s["regions"])}


@asset(inputs={"shards": collect(shard)})
def total(shards: list) -> int:
    return sum(x["n"] for x in shards)
""",
    )
    first = barca(tmp_path, pool, "get", "total", "pipeline.py")
    assert first["final_output"] == 2
    assert first["steps_executed"] == 5


# ─── Property: the same outcome at every pool size ────────────────────────────

SENSOR_ABOVE = """
from pathlib import Path

from barca import asset, collect, partitions, partitions_from, sensor


@sensor()
def version() -> tuple[bool, str]:
    return True, Path("version.txt").read_text().strip()


@asset(inputs={"v": version}, partitions={"ticker": partitions(KEYS)})
def prices(ticker: str, v: str) -> dict:
    return {"ticker": ticker, "v": v}


@asset(partitions={"ticker": partitions_from(prices)})
def signal(ticker: str, prices: dict) -> dict:
    return {"ticker": ticker, "buy": len(ticker) > 3}


@asset(inputs={"signals": collect(signal)})
def report(signals: list) -> dict:
    return {"n": len(signals), "buys": sorted(s["ticker"] for s in signals if s["buy"])}
"""

STATIC_CHAIN = """
from barca import asset, collect, partitions, partitions_from


@asset()
def base() -> int:
    return BASE


@asset(inputs={"b": base}, partitions={"k": partitions(KEYS)})
def first(k: str, b: int) -> dict:
    return {"k": k, "v": b}


@asset(partitions={"k": partitions_from(first)})
def second(k: str, first: dict) -> dict:
    return {"k": k, "v": first["v"] + 1}


@asset(partitions={"k": partitions_from(second)})
def third(k: str, second: dict) -> dict:
    return {"k": k, "v": second["v"] + 1}


@asset(inputs={"rows": collect(third), "firsts": collect(first)})
def total(rows: list, firsts: list) -> int:
    return sum(r["v"] for r in rows) + len(firsts)


@asset(inputs={"t": total})
def final(t: int) -> dict:
    return {"total": t}
"""


def with_keys(template: str, keys: list[str], base: int = 1) -> str:
    return f"KEYS = {json.dumps(keys)}\nBASE = {base}\n" + template


# Each shape: the target, then the successive states of the project. A state is the pipeline
# source and the files beside it; the first is the cold run, the rest are edits.
SHAPES = {
    "chained_partitions_from": (
        "report",
        [
            (chained(["a", "b", "c", "d", "e"]), {}),
            (chained(["a", "b", "c", "d", "e"], note="x = 1"), {}),  # source runs, same keys
            (chained(["a", "b", "c", "d", "e", "f", "g"]), {}),  # keys added
            (chained(["g", "a", "c", "e"]), {}),  # keys removed and reordered
            (chained(["a"]), {}),  # one key left
        ],
    ),
    "sensor_above": (
        "report",
        [
            (with_keys(SENSOR_ABOVE, ["AAPL", "MSFT", "GOOG"]), {"version.txt": "v1"}),
            (with_keys(SENSOR_ABOVE, ["AAPL", "MSFT", "GOOG"]), {"version.txt": "v2"}),
            (with_keys(SENSOR_ABOVE, ["AAPL", "MSFT", "GOOG", "NVDA", "TSM"]), {}),
            (with_keys(SENSOR_ABOVE, ["NVDA", "AAPL"]), {}),
            (with_keys(SENSOR_ABOVE, ["NVDA", "AAPL"]), {"version.txt": "v1"}),
        ],
    ),
    "static_chain_of_three": (
        "final",
        [
            (with_keys(STATIC_CHAIN, ["p", "q", "r", "s"]), {}),
            (with_keys(STATIC_CHAIN, ["p", "q", "r", "s"], base=5), {}),  # unpartitioned input
            (with_keys(STATIC_CHAIN, ["p", "q", "r", "s", "t", "u", "v"], base=5), {}),
            (with_keys(STATIC_CHAIN, ["v", "p"], base=5), {}),
            (with_keys(STATIC_CHAIN, ["p"], base=5), {}),
        ],
    ),
    "fan_in": (
        "summary",
        [
            (fan_in(["us", "eu", "apac"]), {}),
            (fan_in(["us"]), {}),
            (fan_in(["us", "eu", "apac", "latam", "anz", "mena"]), {}),
        ],
    ),
}


def outcome(project: Path, pool: int, target: str) -> dict:
    """Everything about one run that must not depend on the pool size: what a dry run
    predicts, what the run reports per step, its output, and the run hash of every result
    in the project (an artifact's path is `<node and key>/<run hash>.<ext>`)."""
    predicted = barca(project, pool, "get", target, "pipeline.py", "--dry-run", "--json")
    result = barca(project, pool, "get", target, "pipeline.py")
    steps = {}
    for step in result["steps"]:
        entry = {"status": step["status"], "run_hash": step.get("run_hash")}
        if "partitions" in step:
            parts = step["partitions"]
            entry["partitions"] = {
                "total": parts["total"],
                "cached": parts["cached"],
                "ran": sorted(parts.get("will_run_keys", [])),
            }
        steps[step["id"]] = entry
    artifacts = sorted(
        str(p.relative_to(project / ".barca" / "artifacts"))
        for p in (project / ".barca" / "artifacts").rglob("*")
        if p.is_file()
    )
    return {
        "predicted": predicted["summary"],
        "steps": steps,
        "steps_executed": result["steps_executed"],
        "final_output": result["final_output"],
        "artifacts": artifacts,
    }


@pytest.mark.parametrize("shape", SHAPES)
def test_the_same_steps_run_with_the_same_run_hashes_at_every_pool_size(tmp_path, shape):
    target, states = SHAPES[shape]
    pools = [1, 2, 16]
    for pool in pools:
        (tmp_path / str(pool)).mkdir()
    for number, (source, files) in enumerate(states):
        outcomes = {}
        for pool in pools:
            project = tmp_path / str(pool)
            write(project, source)
            for name, text in files.items():
                (project / name).write_text(text)
            outcomes[pool] = outcome(project, pool, target)
        for pool in pools[1:]:
            assert outcomes[pool] == outcomes[pools[0]], (
                f"state {number}: pool {pool} differs from pool {pools[0]}"
            )
        # And a second run at each pool size is served from cache (sensors aside).
        for pool in pools:
            again = barca(tmp_path / str(pool), pool, "get", target, "pipeline.py")
            assert set(ran(again)) <= {"version"}, f"state {number}, pool {pool}"
