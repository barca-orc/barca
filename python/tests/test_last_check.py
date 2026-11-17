"""What the last tick found, where the read path cannot decide (#964).

`barca status` and a server's `GET /state` predict what a request (`barca get`) would do,
without running anything. Below a cutoff asset they predict would run, the answer depends on
bytes only a run writes, so the node's state is `unknown`. A tick does run, and reads a
paused node above the cutoff asset as it is (a request would recompute it), so tick after
tick it finds the node up to date. Every surface then says so: `last_check`, and health `ok`
with `checked_up_to_date`, until a newer check fails or something the read path can see
changes. (Below a `Manual` or pinned node the read path decides as a tick does, #985.)
"""

import json
import sqlite3
import time
from pathlib import Path

from .helpers import QUICK, barca, wait_until

PIPELINE = """
import time

from barca import Schedule, asset, sensor


@sensor(freshness=Schedule("*/5 * * * *"))
def clock() -> float:
    return time.time()


@asset(inputs={"now": clock})
def baseline(now: float) -> dict:
    return {"day": 1}


@asset(inputs={"base": baseline, "now": clock}, cutoff=True)
def window(base: dict, now: float) -> dict:
    return base


@asset(inputs={"w": window})
def report(w: dict) -> dict:
    return w
"""

REPORT = "pipeline.py:report"
CLOCK = "pipeline.py:clock"


def _ok(done) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _status(root: Path) -> dict[str, dict]:
    doc = json.loads(_ok(barca(root, "status", "pipeline.py", "--json", timeout=QUICK)))
    return {n["name"]: n for n in doc["nodes"]}


def _row(root: Path) -> dict:
    """`report` in `barca.nodes`."""
    query = (
        "select health, health_reason, last_check_at, last_check_outcome "
        "from barca.nodes where name = 'report'"
    )
    doc = json.loads(_ok(barca(root, "sql", query, "pipeline.py", "--json", timeout=QUICK)))
    return doc["rows"][0]


def _tick(root: Path, ended: float, report: str, run: str) -> None:
    """Record a fired tick of `clock` that ended at `ended` and did `report` with the report,
    as the scheduling server writes it (`schedule_ticks`, #960)."""
    targets = [[CLOCK, "rebuilt"], ["pipeline.py:window", "rebuilt"], [REPORT, report]]
    # One slot a tick: the ticks of a test are seconds apart at most.
    slot = int(ended) - 10 + int(run.rsplit("-", 1)[1])
    with sqlite3.connect(root / ".barca" / "metadata.db") as db:
        db.execute(
            "INSERT INTO schedule_ticks (schedule_id, scheduled_at, action, run_id, started_at,"
            " finished_at, run_status, queue_wait_seconds, rebuilt, up_to_date, failed,"
            " not_reached, targets) VALUES (?, ?, 'fired', ?, ?, ?, 'success', 0, 2, ?, ?, 0, ?)",
            (
                CLOCK,
                slot,
                run,
                slot + 1.0,
                ended,
                int(report == "up_to_date"),
                int(report == "failed"),
                json.dumps(targets),
            ),
        )
    db.close()


def _polled(root: Path) -> None:
    """What a tick leaves while `baseline` is paused: the sensor polled again, and `baseline`
    as it was, so a request would recompute it."""
    _ok(barca(root, "get", "clock", "pipeline.py"))


def _verdict(node: dict) -> tuple:
    check = node["last_check"]
    return (
        node["health"],
        node["health_reason"],
        check and (check["outcome"], check["schedule"]),
    )


CHECKED = ("ok", "checked_up_to_date", ("up_to_date", CLOCK))


def test_a_node_the_read_path_cannot_decide_reads_as_the_last_tick_found_it(project, serve):
    _ok(barca(project, "get", "report", "pipeline.py"))
    # What a run does: the sensor is polled, `window` runs again and writes the same bytes,
    # and `report` is a cache hit.
    steps = {
        s["id"]: s["status"]
        for s in json.loads(_ok(barca(project, "get", "report", "pipeline.py")))["steps"]
    }
    assert steps[REPORT] == "cached", steps
    assert steps["pipeline.py:window"] == "ran", steps
    _polled(project)

    # The read path predicts `baseline` would run, so it cannot decide `report`. With no
    # tick record that is all there is to say.
    before = _status(project)["report"]
    assert before["cache"]["state"] == "unknown", before
    assert before["cache"]["reason"] == "cutoff_pending", before
    assert before["health"] != "ok" and before["last_check"] is None, before
    assert _row(project)["last_check_at"] is None

    # A tick checked it and found it up to date.
    ended = time.time()
    _tick(project, ended, "up_to_date", "tick-1")
    nodes = _status(project)
    assert _verdict(nodes["report"]) == CHECKED, nodes["report"]
    at = nodes["report"]["last_check"]["at"]
    assert at == time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ended)), at
    # A node the read path decides keeps its own answer, and carries the check all the same.
    assert nodes["window"]["health"] == "behind", nodes["window"]
    assert nodes["window"]["last_check"]["outcome"] == "rebuilt"
    # A sensor's `rebuilt` means polled; it decides nothing about a sensor.
    assert nodes["clock"]["health_reason"] == "last_run_succeeded", nodes["clock"]
    assert nodes["clock"]["last_check"]["outcome"] == "rebuilt"
    assert _row(project) == {
        "health": "ok",
        "health_reason": "checked_up_to_date",
        "last_check_at": at,
        "last_check_outcome": "up_to_date",
    }
    table = _ok(barca(project, "status", "pipeline.py", "--pretty", timeout=QUICK))
    line = next(row for row in table.splitlines() if row.startswith("report"))
    assert "cutoff_pending; checked " in line and ", up to date" in line, table

    # A server that only reads says the same, on both routes.
    server = serve("--read-only")

    def served() -> tuple[dict, dict]:
        state = {n["name"]: n for n in server.get("/state")}
        listed = {n["name"]: n for n in server.get("/nodes")["nodes"]}
        return state["report"], listed["report"]

    for row in served():
        assert _verdict(row) == CHECKED, row
        assert row["last_check"]["at"] == at, row

    # A newer tick that failed it is the last check, and decides nothing.
    _tick(project, time.time(), "failed", "tick-2")
    # (`undecided`, or `behind` where an input's newer result shows; never `ok`.)
    assert _status(project)["report"]["health"] != "ok"
    assert _status(project)["report"]["last_check"]["outcome"] == "failed"
    wait_until(
        lambda: all(r["last_check"]["outcome"] == "failed" for r in served()),
        "the server to read the failed tick",
    )
    for row in served():
        assert row["health"] != "ok" and row["health_reason"] != CHECKED[1], row

    # The next tick finds it up to date again.
    _tick(project, time.time(), "up_to_date", "tick-3")
    assert _verdict(_status(project)["report"]) == CHECKED
    wait_until(
        lambda: all(_verdict(r) == CHECKED for r in served()),
        "the server to read the newer tick",
    )

    # An edit to the node is a change the read path sees: the verdict was about other code.
    source = project / "pipeline.py"
    source.write_text(source.read_text().replace("    return w\n", "    return {**w}\n"))
    edited = _status(project)["report"]
    assert edited["health"] != "ok" and edited["health_reason"] != CHECKED[1], edited
    assert edited["last_check"]["outcome"] == "up_to_date", edited
    assert _row(project)["health"] != "ok"
    wait_until(
        lambda: all(r["health"] != "ok" for r in served()),
        "the server to read the edit",
    )


def test_a_verdict_does_not_outlive_its_schedule(project):
    _ok(barca(project, "get", "report", "pipeline.py"))
    _polled(project)
    # Found up to date an hour ago by a schedule that fires every five minutes: the ticks
    # since then are missing, so the verdict no longer decides. The check is still shown.
    # (Rows above it are newer than this tick too; either reason is enough.)
    _tick(project, time.time() - 3600, "up_to_date", "tick-0")
    old = _status(project)["report"]
    assert old["health"] != "ok", old
    assert old["last_check"]["outcome"] == "up_to_date", old
    table = _ok(barca(project, "status", "pipeline.py", "--pretty", timeout=QUICK))
    assert "checked 1h ago, up to date" in table, table


def test_a_live_tick_decides_and_its_verdict_expires_once_ticks_stop(project, serve):
    source = project / "pipeline.py"
    source.write_text(source.read_text().replace("*/5 * * * *", "*/2 * * * * *"))
    _ok(barca(project, "get", "report", "pipeline.py"))
    server = serve()
    # Ticks read a paused node as it is; a request would recompute it.
    _ok(barca(project, "pause", "baseline", "--server", server.url, "--json"))

    def report() -> dict:
        return next(n for n in server.get("/state") if n["name"] == "report")

    # The scheduler's own ticks: each polls the sensor, reads `baseline` as it is, reruns
    # `window` and finds `report` up to date.
    wait_until(
        lambda: _verdict(report()) == CHECKED,
        "a tick to find report up to date",
    )
    assert report()["cache"]["state"] == "unknown"
    # While ticks keep coming the verdict holds: a tick in flight has polled the sensor and
    # rebuilt `window`, and its own verdict is on its way.
    seen = []
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        seen.append(report()["health"])
        time.sleep(0.05)
    # (Not every poll: on a loaded machine a tick can end later than its verdict allows.)
    assert seen.count("ok") >= 0.8 * len(seen), seen

    # No scheduler, no newer check: after the next tick is overdue the verdict decides
    # nothing, and the check is still there to read.
    server.stop()
    stale = wait_until(
        lambda: (n := _status(project)["report"])["health"] != "ok" and n,
        "the verdict to expire",
        interval=0.5,
    )
    assert stale["last_check"]["outcome"] == "up_to_date", stale
    assert stale["health_reason"] != "checked_up_to_date", stale
