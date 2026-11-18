"""A writer and a read-only observer of the same anonymous 616-node project."""

import json
from datetime import datetime

from .helpers import barca, wait_until


def test_read_only_overview_tracks_writer_and_shutdown(tmp_path, serve):
    source = ["from barca import Always, Schedule, asset, sensor"]
    for i in range(20):
        source.append(f'@sensor(freshness=Schedule("0 0 1 1 *"))\ndef source_{i}(): return 1')
    for i in range(596):
        source.append(
            f'@asset(inputs={{"n": source_{i % 20}}}, freshness=Always)\n'
            f"def output_{i}(n): return n + 1"
        )
    path = tmp_path / "pipeline.py"
    path.write_text("\n\n".join(source))
    writer = serve("--max-runs", "3", "--timezone", "Pacific/Honolulu")
    reader = serve("--read-only", "--max-runs", "1")
    assert set(writer.schedule()) == set(reader.schedule())
    assert reader.get("/queue")["max_runs"] == 3

    def rollup(server):
        return server.get("/state/rollup?now=true")

    live = rollup(writer)
    observed = rollup(reader)
    assert len(reader.get("/state")) == 616

    def roots_of(view):
        return [(r["id"], r["nodes"], r["action"]) for r in view["roots"]]

    assert roots_of(observed) == roots_of(live)
    cli = barca(tmp_path, "status", "--rollup", "--json", "--now", "--server", reader.url)
    assert cli.returncode == 0, cli.stderr
    assert roots_of(json.loads(cli.stdout)) == roots_of(live)
    for server in (writer, reader):
        rows = {row["name"]: row for row in server.get("/state")}
        assert rows["output_0"]["recheck_at"] == rows["source_0"]["refresh_path"]["next_tick"]
    entry, reference = observed["entries"][0], live["entries"][0]
    assert entry["unbounded"] == reference["unbounded"]
    assert entry["fresh_by"] and reference["fresh_by"]
    assert (
        abs(
            (
                datetime.fromisoformat(entry["fresh_by"])
                - datetime.fromisoformat(reference["fresh_by"])
            ).total_seconds()
        )
        <= 8
    )
    assert abs(entry["gap_seconds"] - reference["gap_seconds"]) <= 616 * 60

    writer.stop()
    wait_until(
        lambda: rollup(reader)["roots"][0]["id"] == "scheduler_off",
        "one scheduler root",
        timeout=60,
    )
    stopped = rollup(reader)
    roots = stopped["roots"]
    assert len(roots) == 1 and roots[0]["action"]["command"] == "barca serve", roots
    assert stopped["entries"][0]["fresh_by"] is None
    cli = barca(tmp_path, "status", "--rollup", "--json", "--now", "--server", reader.url)
    assert cli.returncode == 0, cli.stderr
    assert [r["id"] for r in json.loads(cli.stdout)["roots"]] == ["scheduler_off"]


def test_observer_counts_the_upstream_steps_of_a_served_run(tmp_path, serve):
    source = """from pathlib import Path
import time
from barca import asset

@asset()
def first():
    while not Path("release").exists():
        time.sleep(0.05)
    return 1
"""
    for i in range(1, 51):
        parent = "first" if i == 1 else f"step_{i - 1}"
        source += f'\n@asset(inputs={{"n": {parent}}})\ndef step_{i}(n): return n + 1\n'
    (tmp_path / "pipeline.py").write_text(source)
    writer = serve("--no-schedule")
    reader = serve("--read-only")
    accepted = writer.submit("step_50", command="get")
    try:
        for server in (writer, reader):
            wait_until(
                lambda: sum(
                    (row.get("running") or {}).get("run_id") == accepted["run_id"]
                    for row in server.get("/state")
                )
                == 51,
                "the run to cover all 51 nodes",
            )
        wait_until(
            lambda: writer.get("/state/rollup")["entries"][0]["running"] == 1,
            "only the first step to be executing",
        )
        assert reader.get("/state/rollup")["entries"][0]["running"] == 0
    finally:
        (tmp_path / "release").touch()
    assert writer.finished(accepted["run_id"])["status"] == "success"


def test_observer_refreshes_its_graph_after_a_source_edit(tmp_path, serve):
    path = tmp_path / "pipeline.py"
    path.write_text("from barca import asset\n@asset()\ndef before(): return 1\n")
    reader = serve("--read-only")
    assert [n["name"] for n in reader.get("/state")] == ["before"]
    path.write_text(path.read_text().replace("before", "after"))
    health = reader.get("/health")
    assert "graph_stale" in health
    assert [n["name"] for n in reader.get("/state")] == ["after"]
    assert reader.get("/health")["graph_stale"] is False
