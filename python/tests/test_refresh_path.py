"""A node's refresh path: the schedule whose tick brings it up to date (#961).

`refresh_path` is the scheduler's own target walk read back per node: the schedule, its next
tick, whether it is paused, whether a scheduler fires it, and how many finished ticks had the
node as a target and did not leave it fresh. `barca status --json`, a server's `GET /state` and
`GET /nodes`, and `barca.nodes` carry the same value.
"""

import json
import sqlite3
import time
from pathlib import Path

from .helpers import QUICK, barca, wait_until

# Daily schedules: no tick falls inside a test.
PIPELINE = """
from barca import Always, Manual, Schedule, asset, sensor


@sensor(freshness=Schedule("0 6 * * *"))
def inbox() -> int:
    return 1


@asset(inputs={"n": inbox}, freshness=Always)
def orders(n: int) -> dict:
    return {"n": n}


@asset(inputs={"o": orders}, freshness=Manual)
def reviewed(o: dict) -> dict:
    return o


@asset(inputs={"r": reviewed}, freshness=Always)
def report(r: dict) -> dict:
    return r


@sensor
def feed() -> int:
    return 1


@asset(inputs={"f": feed}, freshness=Schedule("30 6 * * *"))
def summary(f: int) -> dict:
    return {"f": f}


@asset(inputs={"f": feed}, freshness=Always)
def feed_rows(f: int) -> dict:
    return {"f": f}


@asset(freshness=Always)
def loose() -> dict:
    return {}
"""

INBOX = "pipeline.py:inbox"
SUMMARY = "pipeline.py:summary"
# The schedule each node's path names: the scheduled sensor and the `Always` assets below it,
# past a `Manual` asset (which a tick reads as it is); what is below an unscheduled sensor
# follows the schedule that polls it; nothing reaches an asset with no scheduled node above.
PATHS = {
    "inbox": INBOX,
    "orders": INBOX,
    "reviewed": None,
    "report": INBOX,
    "feed": None,
    "summary": SUMMARY,
    "feed_rows": SUMMARY,
    "loose": None,
}


def _ok(done) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _status(root: Path) -> dict[str, dict]:
    doc = json.loads(_ok(barca(root, "status", "pipeline.py", "--json", timeout=QUICK)))
    return {n["name"]: n["refresh_path"] for n in doc["nodes"]}


def _rows(root: Path) -> dict[str, dict]:
    """Every node's path columns in `barca.nodes`."""
    query = "select name, refresh_schedule, next_tick, missed_ticks from barca.nodes"
    doc = json.loads(_ok(barca(root, "sql", query, "pipeline.py", "--json", timeout=QUICK)))
    return {row.pop("name"): row for row in doc["rows"]}


def _columns(paths: dict[str, dict]) -> dict[str, dict]:
    """What `barca.nodes` holds of `paths`."""
    return {
        name: {
            "refresh_schedule": path and path["schedule"],
            "next_tick": path and path["next_tick"],
            "missed_ticks": path and path["missed_ticks"],
        }
        for name, path in paths.items()
    }


def _tick(root: Path, slot: int, outcomes: dict[str, str]) -> None:
    """Record a fired tick of `inbox` that just ended, as the scheduling server writes it
    (`schedule_ticks`, #960)."""
    targets = [[f"pipeline.py:{name}", outcome] for name, outcome in outcomes.items()]
    with sqlite3.connect(root / ".barca" / "metadata.db") as db:
        db.execute(
            "INSERT INTO schedule_ticks (schedule_id, scheduled_at, action, run_id, started_at,"
            " finished_at, run_status, queue_wait_seconds, rebuilt, up_to_date, failed,"
            " not_reached, targets) VALUES (?, ?, 'fired', ?, ?, ?, 'failed', 0, 1, 0, 1, 1, ?)",
            (INBOX, slot, f"tick-{slot}", slot + 1.0, time.time(), json.dumps(targets)),
        )
    db.close()


def test_a_path_is_the_schedule_whose_tick_targets_the_node(project):
    paths = _status(project)
    assert {name: path and path["schedule"] for name, path in paths.items()} == PATHS
    # No scheduler runs here: nothing fires the schedules, and no time is estimated.
    for path in filter(None, paths.values()):
        assert path == {
            "schedule": path["schedule"],
            "next_tick": None,
            "state": "on_time",
            "paused": False,
            "live": False,
            "missed_ticks": 0,
        }
    assert _rows(project) == _columns(paths)

    # The table says what will refresh a node that is not fresh, or that nothing will.
    table = _ok(barca(project, "status", "pipeline.py", "--pretty", timeout=QUICK))
    line = {row.split()[0]: row for row in table.splitlines() if row.strip()}
    assert "inbox is not firing" in line["orders"], table
    assert "summary is not firing" in line["feed_rows"], table
    assert "no refresh path" in line["loose"], table
    assert "no refresh path" in line["reviewed"], table

    fields = _ok(barca(project, "status", "pipeline.py", "--fields", "name,refresh_path"))
    assert {n["name"]: n["refresh_path"] for n in json.loads(fields)["nodes"]} == paths


def test_a_scheduling_server_says_when_the_next_tick_is(project, serve):
    server = serve()

    def served() -> tuple[dict, dict]:
        state = {n["name"]: n for n in server.get("/state")}
        listed = {n["name"]: n for n in server.get("/nodes")["nodes"]}
        return state, listed

    state, listed = served()
    for name, schedule in PATHS.items():
        path = state[name]["refresh_path"]
        assert listed[name]["refresh_path"] == path, name
        if schedule is None:
            assert path is None, name
            continue
        # The next tick is the schedule's next slot, as its own `schedule` block reports it.
        owner = state[schedule.rsplit(":", 1)[1]]["schedule"]
        assert path == {
            "schedule": schedule,
            "next_tick": owner["next_fire"],
            "state": "on_time",
            "paused": False,
            "live": True,
            "missed_ticks": 0,
        }, name

    # A paused schedule is still the path, and says so; until resumed it has no next tick.
    server.post("/schedule/inbox/pause", {})

    def paused() -> bool:
        state, listed = served()
        return all(
            rows[name]["refresh_path"]
            == {
                "schedule": INBOX,
                "next_tick": None,
                "state": "paused",
                "paused": True,
                "live": True,
                "missed_ticks": 0,
            }
            for rows in (state, listed)
            for name in ("inbox", "orders", "report")
        )

    wait_until(paused, "the pause to show on the path")
    # The other schedule is not paused.
    assert served()[0]["feed_rows"]["refresh_path"]["next_tick"] is not None

    # A paused node is skipped by a tick: it has no path, and its schedule keeps its others.
    _ok(barca(project, "pause", "feed_rows", "--server", server.url))

    def skipped() -> bool:
        return all(
            rows["feed_rows"]["refresh_path"] is None
            and rows["summary"]["refresh_path"]["schedule"] == SUMMARY
            for rows in served()
        )

    wait_until(skipped, "the paused node to lose its path")


def test_missed_ticks_are_counted_from_the_tick_record(project, serve):
    _ok(barca(project, "status", "pipeline.py", "--json", timeout=QUICK))
    _ok(barca(project, "get", "loose", "pipeline.py"))  # creates the metadata DB
    assert _status(project)["orders"]["missed_ticks"] == 0

    # Two ticks had `orders` and `report` as targets and left neither fresh.
    # Both decisions predate the latest daily slot and its grace.
    slot = int(time.time()) - 2 * 86400
    _tick(project, slot, {"inbox": "rebuilt", "orders": "failed", "report": "not_reached"})
    _tick(project, slot + 60, {"inbox": "rebuilt", "orders": "failed", "report": "not_reached"})
    paths = _status(project)
    assert paths["orders"]["missed_ticks"] == 2, paths
    assert paths["report"]["missed_ticks"] == 2, paths
    # The sensor was polled each time; a node with no path has no count.
    assert paths["inbox"]["missed_ticks"] == 0 and paths["reviewed"] is None, paths
    assert _rows(project) == _columns(paths)
    table = _ok(barca(project, "status", "pipeline.py", "--pretty", timeout=QUICK))
    assert "inbox is not firing, missed 2 ticks" in table, table

    # A server that only reads says the same on both routes, and fires nothing.
    server = serve("--read-only")
    for rows in (server.get("/state"), server.get("/nodes")["nodes"]):
        assert {n["name"]: n["refresh_path"] for n in rows} == paths

    # A run nobody scheduled brings `orders` up to date: no tick missed a fresh node, and
    # the ticks before its success stay made good.
    time.sleep(1.1)  # results are recorded to the second
    _ok(barca(project, "get", "orders", "pipeline.py"))
    paths = _status(project)
    assert paths["orders"]["missed_ticks"] == 0, paths
    assert paths["report"]["missed_ticks"] == 2, paths

    # The next tick reaches `report`: the count starts over, whatever its state says.
    _tick(project, slot + 120, {"inbox": "rebuilt", "orders": "up_to_date", "report": "rebuilt"})
    paths = _status(project)
    assert paths["report"]["missed_ticks"] == 0, paths
    wait_until(
        lambda: all(
            {n["name"]: n["refresh_path"] for n in rows} == paths
            for rows in (server.get("/state"), server.get("/nodes")["nodes"])
        ),
        "the server to read the newer tick",
    )
    assert _rows(project) == _columns(paths)
