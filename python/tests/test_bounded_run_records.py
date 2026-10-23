"""What a run keeps does not grow with the partition keys it served from cache (#623).

Up to 0.30.0 a run's outcome listed every key of a partitioned target: at 200k keys each
`barca serve` tick wrote about 82 MB of JSON to the metadata DB and the server kept about 400 MB
more memory per tick. Past 1000 keys a run now lists the keys it computed and counts the rest,
the server keeps finished runs under a memory budget and returns what a run freed, and
`barca history RUN_ID` / `GET /runs/{run_id}` page the steps a run recorded.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from barca.api import BarcaError, _find_binary
from barca.api import get as barca_get

from . import served

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

KEYED = """
import json
import pathlib

from barca import Schedule, asset, partitions_from, sensor

VERSIONS = pathlib.Path(__file__).with_name("versions.json")


@sensor()
def versions() -> tuple[bool, dict]:
    return True, json.loads(VERSIONS.read_text())


@asset(partitions={"retailer": partitions_from(versions)})
def cleaned(retailer: str, versions: int) -> dict:
    return {"retailer": retailer, "version": versions}


@asset(partitions={"retailer": partitions_from(cleaned)})
def scored(retailer: str, cleaned: dict) -> dict:
    return {**cleaned, "score": len(retailer)}
"""


def env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)} | {
        "BARCA_REMOTE": "off"
    }


def keyed(root: Path, n: int, cron: str | None = None) -> Path:
    source = KEYED
    if cron:
        source = source.replace("@sensor()", f'@sensor(freshness=Schedule("{cron}"))')
    (root / "pipeline.py").write_text(source)
    (root / "versions.json").write_text(json.dumps({f"r{i:05d}": 0 for i in range(n)}))
    return root


def bump(root: Path, key: str) -> None:
    path = root / "versions.json"
    versions = json.loads(path.read_text())
    versions[key] += 1
    path.write_text(json.dumps(versions))


def barca(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(
        [_find_binary(), *args], cwd=root, env=env(), capture_output=True, text=True
    )
    if check:
        assert result.returncode == 0, result.stderr
    return result


def summary_of(output: dict) -> dict:
    assert set(output) == {"_barca_partitions"}, list(output)[:5]
    return output["_barca_partitions"]


def test_a_target_of_many_keys_lists_the_keys_the_run_computed(tmp_path, monkeypatch):
    keyed(tmp_path, 1500)
    cold = json.loads(barca(tmp_path, "get", "scored", "pipeline.py", "--json").stdout)
    s = summary_of(cold["final_output"])
    # A cold run computed every key: the first 1000 are listed and the list says it is cut.
    assert (s["keys"], s["computed"], s["cached"], s["truncated"]) == (1500, 1500, 0, True)
    assert len(s["outputs"]) == 1000 and min(s["outputs"]) == "retailer=r00000"
    assert "--partition" in s["hint"]

    bump(tmp_path, "r00042")
    tick = json.loads(barca(tmp_path, "get", "scored", "pipeline.py", "--json").stdout)
    s = summary_of(tick["final_output"])
    assert (s["keys"], s["computed"], s["cached"], s["truncated"]) == (1500, 1, 1499, False)
    pointer = s["outputs"]["retailer=r00042"]["_barca_artifact"]
    assert json.loads(Path(tmp_path, pointer["path"]).read_text())["version"] == 1

    # `-o value` holds values under the same summary; `-o path` prints the computed keys' paths
    # and says on stderr what it left out.
    bump(tmp_path, "r00007")
    values = json.loads(barca(tmp_path, "get", "scored", "pipeline.py", "-o", "value").stdout)
    assert summary_of(values)["outputs"] == {
        "retailer=r00007": {"retailer": "r00007", "version": 1, "score": 6}
    }
    paths = barca(tmp_path, "get", "scored", "pipeline.py", "-o", "path")
    assert len(paths.stdout.splitlines()) == 0, "nothing was computed: no path is listed"
    assert "1500 of 1500 keys not listed (1500 cached)" in paths.stderr

    # A selection returns every key it selects, as before.
    selected = json.loads(
        barca(
            tmp_path,
            "get",
            "scored",
            "pipeline.py",
            "--json",
            "--partition",
            "retailer=r00001,r00002",
        ).stdout
    )
    assert sorted(selected["final_output"]) == ["retailer=r00001", "retailer=r00002"]

    # Python's barca.get cannot return a dict of every key: it says to select them.
    for name in list(os.environ):
        if name.startswith(SCRUB):
            monkeypatch.delenv(name)
    monkeypatch.setenv("BARCA_REMOTE", "off")
    monkeypatch.chdir(tmp_path)
    with pytest.raises(BarcaError, match="partition="):
        barca_get("scored", "pipeline.py")
    assert barca_get("scored", "pipeline.py", partition="retailer=r00003") == {
        "retailer=r00003": {"retailer": "r00003", "version": 0, "score": 6}
    }


def test_a_target_of_at_most_a_thousand_keys_lists_every_key(tmp_path):
    keyed(tmp_path, 1000)
    out = json.loads(barca(tmp_path, "get", "scored", "pipeline.py", "--json").stdout)
    assert len(out["final_output"]) == 1000
    assert "_barca_partitions" not in out["final_output"]


FAILING = """
from barca import asset, partitions


@asset(partitions={"n": partitions([str(i) for i in range(30)])})
def many(n: str) -> int:
    if n == "17":
        raise ValueError("seventeen")
    return int(n)
"""


def test_history_of_one_run_pages_its_steps_failed_ones_first(tmp_path):
    (tmp_path / "pipeline.py").write_text(FAILING)
    failed = barca(tmp_path, "get", "many", "pipeline.py", "--json", check=False)
    assert failed.returncode == 1
    run_id = json.loads(failed.stdout)["run_id"]

    page = json.loads(barca(tmp_path, "history", run_id, "--limit", "5", "--json").stdout)
    assert page["steps_page"] == {"total": 30, "truncated": True}
    keys = [s["partition_key"] for s in page["steps"]]
    assert len(keys) == 5 and "n=17" in keys, keys
    assert [s["status"] for s in page["steps"] if s["partition_key"] == "n=17"] == ["failed"]

    every = json.loads(barca(tmp_path, "history", run_id, "--all", "--json").stdout)
    assert every["steps_page"] == {"total": 30, "truncated": False}
    assert len(every["steps"]) == 30
    default = json.loads(barca(tmp_path, "history", run_id, "--json").stdout)
    assert len(default["steps"]) == 30 and not default["steps_page"]["truncated"]

    table = barca(tmp_path, "history", run_id, "--limit", "5", "--pretty")
    assert "showing 5 of 30 steps" in table.stderr

    # The server pages the same way.
    with served.Serve(tmp_path, "--no-schedule", env=env()) as serve:
        serve.ready()
        view = serve.get(f"/runs/{run_id}?limit=5")
        assert view["steps_page"] == {"total": 30, "truncated": True}
        assert len(view["steps"]) == 5
        assert len(serve.get(f"/runs/{run_id}?all=true")["steps"]) == 30
        remote = barca(
            tmp_path,
            "history",
            run_id,
            "--limit",
            "5",
            "--json",
            "--server",
            f"http://127.0.0.1:{serve.port}",
        )
        assert json.loads(remote.stdout)["steps_page"] == {"total": 30, "truncated": True}


def _rss_mb(pid: int) -> float:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024
    raise AssertionError("no VmRSS")


def _db_bytes(root: Path) -> int:
    return sum(p.stat().st_size for p in (root / ".barca").glob("metadata.db*"))


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="reads /proc for RSS")
def test_serve_ticks_over_many_keys_keep_the_db_and_memory_bounded(tmp_path):
    """10k keys, a tick a second, one key changed per tick: each tick's outcome is a few
    kilobytes, the DB grows by kilobytes per tick and the server's RSS stays flat. Before the
    fix each tick here recorded about 4 MB of outcome and the server kept it in memory."""
    keys, ticks = 10_000, 10
    keyed(tmp_path, keys, cron="* * * * * *")
    barca(tmp_path, "get", "scored", "pipeline.py")
    with served.Serve(tmp_path, env=env()) as serve:
        serve.ready()
        seen: set[str] = set()

        def next_tick() -> dict:
            def finished():
                listing = serve.get("/runs?limit=5")["runs"]
                done = [
                    r
                    for r in listing
                    if r["id"] not in seen
                    and r["status"] not in served.LIVE
                    and r["triggered_by"] == "schedule"
                ]
                return done and done[0]

            run = serve.wait("a scheduled tick", finished, timeout=120)
            seen.add(run["id"])
            return serve.finished(run["id"])

        # The ticks already in flight or done before the first change.
        next_tick()
        db_before = _db_bytes(tmp_path)
        rss, outcome_bytes = [], []
        for i in range(ticks):
            bump(tmp_path, f"r{i:05d}")
            view = next_tick()
            assert view["status"] == "success", view
            outcome_bytes.append(len(json.dumps(view["outcome"])))
            targets = view["outcome"]["targets"]
            scored = targets[next(t for t in targets if t.endswith("scored"))]
            s = summary_of(scored["final_output"])
            assert s["keys"] == keys and s["computed"] <= 2, s
            rss.append(_rss_mb(serve.proc.pid))
        db_growth = _db_bytes(tmp_path) - db_before

    assert max(outcome_bytes) < 20_000, outcome_bytes
    # The WAL takes whole pages per write, so allow for that, far under the 40 MB that ten
    # 4 MB outcomes took.
    assert db_growth < 8_000_000, db_growth
    with served.snapshot(tmp_path) as db:
        largest = db.execute(
            "select max(length(outcome_json)) from runs where triggered_by = 'schedule'"
        ).fetchone()[0]
    assert largest < 20_000, largest
    # Flat: the last ticks hold no more than the first ones, within allocator noise.
    assert max(rss[-3:]) - min(rss[:3]) < 60, rss
