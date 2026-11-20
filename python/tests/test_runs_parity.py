"""One definition of a run and of the run list: `barca history` and a server answer alike.

`barca history RUN_ID --json` reads the metadata database; `GET /runs/{run_id}` reads a
server's snapshot of it and adds what only the server holds (its runs in memory, the events a
run streamed). Both go through `barca_service::views::run_view`, and the run list through
`queries::history_at`, so on one database a recorded run is the same run whichever host
answers (#1056).

The fixture has what the definitions disagree on when they are written twice: a run that
failed (its first failure, its counts of failed steps), a partitioned node (the per-node
summary of a run's steps), a run that computed nothing (its planned nodes, cached ones
included) and a run a server started (it stored an outcome, which a CLI run does not).
"""

import json
from pathlib import Path

from .helpers import QUICK, barca, wait_until

PIPELINE = """
from barca import Always, asset, partitions


@asset(freshness=Always)
def orders() -> dict:
    return {"n": 1}


@asset(inputs={"o": orders}, freshness=Always)
def report(o: dict) -> dict:
    return {"o": o}


@asset(partitions={"key": partitions(["a", "b", "c"])}, freshness=Always)
def shard(key: str) -> dict:
    return {"key": key}


@asset(freshness=Always)
def broken() -> dict:
    raise ValueError("no rows for today")
"""

# What only a server's answer has: the row as `GET /runs` words it for a page (its `line`),
# and the run a server holds in memory.
LIST_SERVER_ONLY = {"line", "now"}


def _ok(done) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _history(root: Path, *args: str) -> dict:
    return json.loads(_ok(barca(root, "history", "--json", *args, timeout=QUICK)))


def _detail(root: Path, run_id: str, *args: str) -> dict:
    out = _ok(barca(root, "history", run_id, "--json", "--all", *args, timeout=QUICK))
    return json.loads(out)


def test_history_says_the_same_of_every_run_here_and_through_a_server(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    _ok(barca(tmp_path, "get", "report", "pipeline.py"))
    _ok(barca(tmp_path, "get", "shard", "pipeline.py"))
    failed = barca(tmp_path, "get", "broken", "pipeline.py")
    assert failed.returncode == 1, failed.stderr
    # Computes nothing: every step of it is cached.
    _ok(barca(tmp_path, "get", "report", "pipeline.py"))

    # A run the server starts: it stores the run's outcome, which `barca get` does not.
    writer = serve("--no-schedule")
    started = writer.post("/runs", {"command": "get", "targets": ["report"]})
    wait_until(
        lambda: writer.get(f"/runs/{started['run_id']}")["status"] == "success",
        "the server's run to finish",
    )
    writer.stop()

    # A read-only server holds no run in memory: it and the CLI read the same records.
    reader = serve("--read-only")
    local = _history(tmp_path, "--local")
    served = _history(tmp_path, "--server", reader.url)
    assert len(local["runs"]) == 5, local
    assert {r["status"] for r in local["runs"]} == {"success", "failed"}, local

    def rows(doc: dict) -> list[dict]:
        return [{k: v for k, v in r.items() if k not in LIST_SERVER_ONLY} for r in doc["runs"]]

    assert rows(served) == rows(local)
    assert served["total"] == local["total"] == 5
    # The words a server adds to a row are computed from the row's own fields.
    assert all("line" in r for r in served["runs"])

    # `GET /runs` lists the same runs in the same order, under its own keys.
    listed = reader.get("/runs")
    assert [r["id"] for r in listed["runs"]] == [r["run_id"] for r in local["runs"]]
    assert listed["total"] == local["total"]
    shared = ("command", "target", "status", "triggered_by", "steps_total", "steps_executed")
    for row, run in zip(listed["runs"], local["runs"], strict=True):
        assert {k: row[k] for k in shared} == {k: run[k] for k in shared}, (row, run)

    # Each run in full: its row, its steps, the per-node summary and its explanation.
    for run in local["runs"]:
        run_id = run["run_id"]
        here = _detail(tmp_path, run_id, "--local")
        there = _detail(tmp_path, run_id, "--server", reader.url)
        # A stored time reads `YYYY-MM-DD HH:MM:SS` from the database and RFC 3339 from a
        # server: the same instant, in two spellings (left as they are, #1056 B3).
        # The hint is the command that reads the failed step's output where the run was read
        # from: it names the host (`--local`, `--server URL`).
        hint, served_hint = here.pop("hint", ""), there.pop("hint", "")
        assert hint.replace("--local", "") == served_hint.replace(f"--server {reader.url}", "")
        for doc in (here, there):
            for key in ("started_at", "finished_at"):
                value = doc["run"].get(key)
                if value:
                    doc["run"][key] = value.replace("T", " ").removesuffix("Z").split(".")[0]
        assert there == here, run_id
        assert here["steps_page"]["total"] == len(here["steps"]), here["steps_page"]

    # What the definitions were written twice for.
    by_target = {r["target"]: r for r in local["runs"]}
    broke = _detail(tmp_path, by_target["broken"]["run_id"], "--local")
    assert broke["run"]["failure"]["node"].endswith(":broken"), broke["run"]
    assert "no rows for today" in broke["run"]["failure"]["error"], broke["run"]
    shard = _detail(tmp_path, by_target["shard"]["run_id"], "--local")
    nodes = {n["node_id"].split(":")[-1]: n for n in shard["steps_page"]["nodes"]}
    assert nodes["shard"]["keys"] == 3, shard["steps_page"]
    # A page of its steps says where the next one starts, wherever it is read (it once said so
    # only through a server).
    run_id = by_target["shard"]["run_id"]
    page = json.loads(_ok(barca(tmp_path, "history", run_id, "--json", "-l", "2", "--local")))
    there = json.loads(
        _ok(barca(tmp_path, "history", run_id, "--json", "-l", "2", "--server", reader.url))
    )
    assert page["steps_page"] == there["steps_page"]
    assert len(page["steps"]) == 2 and page["steps_page"]["truncated"], page["steps_page"]
    assert page["steps_page"]["next_offset"] == 2, page["steps_page"]
    # The server's own run: its outputs are read from the outcome it stored, on both paths.
    own = _detail(tmp_path, started["run_id"], "--local")
    assert own["explanation"]["outputs"], own["explanation"]
