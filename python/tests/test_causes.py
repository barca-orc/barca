"""Why a node is not fresh, what explains it, and what clears it (#963, #973).

`cause`, `root` and `gap_seconds` on every node, the same values in `barca status --json`, a
server's `GET /state` and `GET /nodes`, and `barca.nodes`; the ranked roots with their actions
in `barca status --rollup` and `GET /state/rollup`.

The queue test runs a real server with one run slot and two schedules whose runs together
take longer than they are apart: status names the run queue as the cause, with the wait.
"""

import json
import time
from pathlib import Path

from .helpers import QUICK, barca, wait_until

# Daily schedules: no tick falls inside a test.
PIPELINE = """
from barca import Always, Manual, Schedule, asset, sensor

VERSION = 0


@sensor(freshness=Schedule("0 6 * * *"))
def inbox() -> int:
    return 1


@asset(inputs={"n": inbox}, freshness=Always)
def orders(n: int) -> dict:
    return {"n": n, "version": VERSION}


@asset(inputs={"o": orders}, freshness=Always)
def clean(o: dict) -> dict:
    return o


@asset(inputs={"c": clean}, freshness=Manual)
def reviewed(c: dict) -> dict:
    return c


@asset(freshness=Always)
def ledger() -> dict:
    return {}


@asset(inputs={"l": ledger}, freshness=Always)
def totals(l: dict) -> dict:
    return l
"""

FIELDS = ("cause", "root", "gap_seconds")
CAUSES = {"catching_up", "queued", "no_refresh_path", "overdue", "failing", "undecided"}


def _ok(done) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _edit(root: Path, version: int) -> None:
    """Change `orders`' code."""
    source = (root / "pipeline.py").read_text()
    before = f"VERSION = {version - 1}"
    assert before in source
    (root / "pipeline.py").write_text(source.replace(before, f"VERSION = {version}"))


def _why(nodes: list[dict]) -> dict[str, tuple]:
    """`(cause, root id)` of every node, by name."""
    return {n["name"]: (n["cause"], n["root"] and n["root"]["id"]) for n in nodes}


def _held(rollup: dict, nodes: list[dict]) -> None:
    """INV-HEALTH-01 and INV-HEALTH-04 over one answer: a cause and a root exactly on the nodes
    that are not fresh, each under one root of the list, and the roots adding up."""
    behind = [n for n in nodes if n["health"] not in ("ok", "suspect")]
    for node in nodes:
        fresh = node["health"] in ("ok", "suspect")
        assert (node["cause"] is None) == fresh, node["name"]
        assert (node["root"] is None) == fresh, node["name"]
        if fresh:
            assert node["gap_seconds"] == 0
            continue
        assert node["cause"] in CAUSES
        root = node["root"]
        assert root["id"] == f"{root['kind']}:{root['node']}"
        assert [r["id"] for r in rollup["roots"]].count(root["id"]) == 1
    total = rollup["entries"][0]
    assert sum(total["causes"].values()) == len(behind)
    assert sum(r["nodes"] for r in rollup["roots"]) == len(behind)
    assert sum(r["gap_seconds"] for r in rollup["roots"]) == total["gap_seconds"]
    assert sum(r["unbounded"] for r in rollup["roots"]) == total["unbounded"]
    # The distance from fresh (#1028): a fresh node weighs 1, a node that is not fresh its
    # cause's weight; the list is ranked by what each root holds of it.
    fresh = total["nodes"] - len(behind)
    assert total["weight"] == sum(total["weights"].values())
    assert total["weights"]["fresh"] == fresh
    assert sum(r["weight"] for r in rollup["roots"]) == total["weight"] - fresh
    weights = [r["weight"] for r in rollup["roots"]]
    assert weights == sorted(weights, reverse=True)
    assert [h["root"] for h in total["held_by"]] == [r["id"] for r in rollup["roots"]]
    for root in rollup["roots"]:
        causes = root["causes"]
        acted = causes["failing"] + causes["overdue"] + causes["no_refresh_path"]
        assert root["weight"] == 6 * acted + 2 * (root["nodes"] - acted)
        assert sum(root["causes"].values()) == root["nodes"]
        assert set(root["action"]) == {"kind", "command", "estimated_seconds"}


def test_status_says_why_each_node_is_not_fresh_and_what_explains_it(project):
    _ok(barca(project, "get", "reviewed", "pipeline.py", "--refresh", "reviewed"))
    _edit(project, 1)
    doc = json.loads(_ok(barca(project, "status", "pipeline.py", "--json", timeout=QUICK)))
    # No scheduler runs here and no tick is on record: nothing fires `inbox`'s schedule, so
    # what its ticks would bring up to date hangs on that, not on the code that changed. A
    # `Manual` node is where someone has to run it; a chain no schedule reaches hangs on its
    # topmost node.
    assert _why(doc["nodes"]) == {
        "inbox": (None, None),
        "orders": ("no_refresh_path", "scheduler_off:pipeline.py:inbox"),
        "clean": ("no_refresh_path", "scheduler_off:pipeline.py:inbox"),
        "reviewed": ("no_refresh_path", "no_schedule:pipeline.py:reviewed"),
        "ledger": ("no_refresh_path", "no_schedule:pipeline.py:ledger"),
        "totals": ("no_refresh_path", "no_schedule:pipeline.py:ledger"),
    }
    # Beside `summary`: the instant of classification and the scope summed.
    assert doc["as_of"].endswith("Z")
    total = doc["rollup"]
    assert (total["id"], total["nodes"], total["assets"]) == ("root", 6, 5)
    assert total["causes"] == {
        "catching_up": 0,
        "queued": 0,
        "no_refresh_path": 5,
        "overdue": 0,
        "failing": 0,
        "undecided": 0,
    }
    assert total["asset_causes"] == total["causes"]
    assert total["unbounded"] == 5 and total["fresh_by"] is None
    assert total["gap_seconds"] == sum(n["gap_seconds"] for n in doc["nodes"])

    # `--fields` selects them; `barca.nodes` has them as columns.
    fields = _ok(barca(project, "status", "pipeline.py", "--fields", "name," + ",".join(FIELDS)))
    assert json.loads(fields)["nodes"] == [
        {"name": n["name"], **{key: n[key] for key in FIELDS}} for n in doc["nodes"]
    ]
    query = "select name, cause, root_id, root_kind, root_node from barca.nodes"
    rows = json.loads(_ok(barca(project, "sql", query, "pipeline.py", "--json", timeout=QUICK)))
    assert {r["name"]: r for r in rows["rows"]} == {
        n["name"]: {
            "name": n["name"],
            "cause": n["cause"],
            "root_id": n["root"] and n["root"]["id"],
            "root_kind": n["root"] and n["root"]["kind"],
            "root_node": n["root"] and n["root"]["node"],
        }
        for n in doc["nodes"]
    }
    gap = "select sum(gap_seconds) as gap, count(*) filter (where cause is not null) as n from barca.nodes"
    summed = json.loads(_ok(barca(project, "sql", gap, "pipeline.py", "--json", timeout=QUICK)))
    assert summed["rows"][0]["n"] == 5

    # The table says how far the project is from fresh in one line.
    table = _ok(barca(project, "status", "pipeline.py", "--pretty", timeout=QUICK))
    assert "Gap: 5 no refresh path (nodes); 5 with no estimate" in table, table

    # The rollup lists what holds them, with what clears each.
    rollup = json.loads(
        _ok(barca(project, "status", "pipeline.py", "--rollup", "--json", timeout=QUICK))
    )
    _held(rollup, doc["nodes"])
    roots = {r["id"]: r for r in rollup["roots"]}
    assert [(r["id"], r["nodes"]) for r in rollup["roots"]] == [
        ("no_schedule:pipeline.py:ledger", 2),
        ("scheduler_off:pipeline.py:inbox", 2),
        ("no_schedule:pipeline.py:reviewed", 1),
    ]
    off = roots["scheduler_off:pipeline.py:inbox"]
    assert off["detail"] == "no scheduler is firing inbox's schedule"
    assert off["action"]["kind"] == "start_server" and off["action"]["command"] == "barca serve"
    assert roots["no_schedule:pipeline.py:ledger"]["action"] == {
        "kind": "add_schedule",
        "command": None,
        "estimated_seconds": None,
    }
    # A `Manual` node is run by hand, named in `--refresh`: the command computes it and what
    # is above it.
    manual = roots["no_schedule:pipeline.py:reviewed"]
    assert manual["detail"] == "reviewed is Manual: no tick computes it"
    assert (manual["action"]["kind"], manual["action"]["command"]) == (
        "run",
        "barca get reviewed --refresh reviewed",
    )
    assert manual["action"]["estimated_seconds"] is not None
    printed = _ok(barca(project, "status", "pipeline.py", "--rollup", "--pretty", timeout=QUICK))
    assert "What holds the 5 nodes that are not fresh, and what clears it:" in printed, printed
    assert "1. no schedule: ledger" in printed, printed
    assert "do:   run: barca get reviewed --refresh reviewed" in printed, printed
    assert "do:   start a server: barca serve" in printed, printed

    # The command clears its root: everything above `reviewed` is computed with it.
    _ok(barca(project, *manual["action"]["command"].split()[1:], "pipeline.py"))
    after = json.loads(_ok(barca(project, "status", "pipeline.py", "--json", timeout=QUICK)))
    assert _why(after["nodes"]) == {
        "inbox": (None, None),
        "orders": (None, None),
        "clean": (None, None),
        "reviewed": (None, None),
        "ledger": ("no_refresh_path", "no_schedule:pipeline.py:ledger"),
        "totals": ("no_refresh_path", "no_schedule:pipeline.py:ledger"),
    }


def test_a_server_roots_a_stale_chain_at_the_change_on_every_route(project, serve):
    _ok(barca(project, "get", "reviewed", "pipeline.py", "--refresh", "reviewed"))
    _edit(project, 1)
    server = serve()

    def served() -> tuple[list[dict], list[dict], dict]:
        return server.get("/state"), server.get("/nodes")["nodes"], server.get("/state/rollup")

    # A scheduler fires `inbox`: its next tick computes `orders` and `clean`, and the change
    # that started it is `orders`' code.
    expected = {
        "inbox": (None, None),
        "orders": ("catching_up", "code:pipeline.py:orders"),
        "clean": ("catching_up", "code:pipeline.py:orders"),
        "reviewed": ("no_refresh_path", "no_schedule:pipeline.py:reviewed"),
        "ledger": ("no_refresh_path", "no_schedule:pipeline.py:ledger"),
        "totals": ("no_refresh_path", "no_schedule:pipeline.py:ledger"),
    }
    wait_until(lambda: _why(served()[0]) == expected, "the server to read the edit")
    state, listed, rollup = served()
    assert _why(listed) == expected
    # INV-HEALTH-02: one answer on every route, to the field.
    by_name = {n["name"]: n for n in listed}
    for node in state:
        for key in FIELDS:
            assert node[key] == by_name[node["name"]][key], (node["name"], key)
    _held(rollup, state)
    nodes = server.get("/nodes")
    assert nodes["as_of"].endswith("Z") and nodes["rollup"]["causes"]["catching_up"] == 2
    code = next(r for r in rollup["roots"] if r["id"] == "code:pipeline.py:orders")
    assert code["detail"] == "the code of orders changed"
    assert code["causes"]["catching_up"] == 2 and code["unbounded"] == 0
    assert code["action"]["kind"] == "wait" and code["action"]["command"] is None
    # Catching up has an estimate, so a gap ahead: the next tick is tomorrow at the latest.
    assert code["gap_seconds"] > 0
    # A command a server's answer names goes to the server.
    manual = next(r for r in rollup["roots"] if r["id"] == "no_schedule:pipeline.py:reviewed")
    assert manual["action"]["command"] == "barca get reviewed --refresh reviewed --server"

    # The CLI prints the server's list.
    printed = _ok(barca(project, "status", "--rollup", "--pretty", "--server", server.url))
    assert "code change: orders" in printed and "do:   wait" in printed, printed

    # A paused schedule: nothing will refresh what its ticks reach, and resuming it is what
    # clears that.
    server.post("/schedule/inbox/pause", {})
    paused = ("no_refresh_path", "schedule_paused:pipeline.py:inbox")
    wait_until(lambda: _why(served()[0])["clean"] == paused, "the pause to show as the cause")
    root = next(r for r in served()[2]["roots"] if r["kind"] == "schedule_paused")
    assert root["detail"] == "inbox's schedule is paused"
    assert (root["nodes"], root["action"]["kind"]) == (2, "resume_schedule")
    assert root["action"]["command"] == "barca resume --schedule inbox --server"
    # The command clears the root.
    _ok(barca(project, *root["action"]["command"].split()[1:], server.url))
    wait_until(lambda: _why(served()[0]) == expected, "the resumed schedule to catch up again")

    # A paused node is its own root, with the command that resumes it.
    _ok(barca(project, "pause", "orders", "--server", server.url))
    wait_until(
        lambda: _why(served()[0])["orders"]
        == ("no_refresh_path", "schedule_paused:pipeline.py:orders"),
        "the paused node to root at itself",
    )
    root = next(r for r in served()[2]["roots"] if r["kind"] == "schedule_paused")
    assert root["detail"] == "orders is paused: every tick skips it"
    assert root["action"]["command"] == "barca resume orders --server"
    _ok(barca(project, *root["action"]["command"].split()[1:], server.url))
    wait_until(lambda: _why(served()[0]) == expected, "the resumed node to catch up again")


# One run slot. `feed`'s run takes three seconds and both schedules tick every two, so every
# tick of `pulse` waits for the slot longer than its ticks are apart.
QUEUE = """
import time

from barca import Always, Schedule, asset, sensor

VERSION = 0


@sensor(freshness=Schedule("*/2 * * * * *"))
def pulse() -> int:
    return 1


@asset(inputs={"p": pulse}, freshness=Always)
def beat(p: int) -> dict:
    return {"p": p, "version": VERSION}


@sensor(freshness=Schedule("1/2 * * * * *"))
def feed() -> float:
    return time.time()


@asset(inputs={"f": feed}, freshness=Always)
def feed_rows(f: float) -> dict:
    time.sleep(3.0)
    return {"f": f}
"""


def test_ticks_waiting_on_one_run_slot_name_the_queue_as_the_cause(tmp_path, serve):
    (tmp_path / "pipeline.py").write_text(QUEUE)
    server = serve("--max-runs", "1")

    def schedule(name: str) -> dict:
        # Empty until the scheduler has published its jobs.
        jobs = (s for s in server.get("/schedule") if s["id"] == f"pipeline.py:{name}")
        return next(jobs, {})

    # A few ticks in, `pulse` is queue-bound: its typical wait exceeds its interval.
    wait_until(lambda: schedule("pulse").get("queue_bound"), "pulse's ticks to wait", timeout=90)
    assert schedule("pulse")["typical_queue_wait_seconds"] >= 2

    # Make `beat` out of date, and read why before the tick that waits computes it. An edit
    # may be picked up by a run already starting; another edit gives the next window.
    seen: dict = {}

    def queued() -> bool:
        seen["version"] = seen.get("version", 0) + 1
        source = (tmp_path / "pipeline.py").read_text()
        (tmp_path / "pipeline.py").write_text(
            source.replace(f"VERSION = {seen['version'] - 1}", f"VERSION = {seen['version']}")
        )
        until = time.time() + 4
        while time.time() < until:
            rollup = server.get("/state/rollup")
            beat = next(n for n in server.get("/state") if n["name"] == "beat")
            root = next((r for r in rollup["roots"] if r["id"] == "queue:pipeline.py:pulse"), None)
            if beat["cause"] == "queued" and root is not None:
                seen.update(beat=beat, root=root)
                return True
            time.sleep(0.1)
        return False

    wait_until(queued, "status to name the queue", timeout=60)
    beat, root = seen["beat"], seen["root"]
    assert beat["root"] == {
        "id": "queue:pipeline.py:pulse",
        "kind": "queue",
        "node": "pipeline.py:pulse",
    }
    # The root says the wait, the interval and the setting, and what to do about it.
    assert root["detail"].startswith("pulse's ticks wait about "), root
    assert root["detail"].endswith(" for a run slot, every 2s: max_runs is 1"), root
    assert root["action"]["kind"] == "raise_max_runs" and root["action"]["command"] is None
    assert root["causes"]["queued"] >= 1

    # A server that only reads fires nothing itself, and reads in the tick record that a
    # scheduler is firing: it does not say every schedule is off.
    reader = serve("--read-only")

    def read() -> bool:
        seen["version"] += 1
        source = (tmp_path / "pipeline.py").read_text()
        (tmp_path / "pipeline.py").write_text(
            source.replace(f"VERSION = {seen['version'] - 1}", f"VERSION = {seen['version']}")
        )
        until = time.time() + 4
        while time.time() < until:
            beat = next(n for n in reader.get("/state") if n["name"] == "beat")
            if beat["cause"] is not None:
                seen["read"] = beat
                return True
            time.sleep(0.1)
        return False

    wait_until(read, "the read-only server to see beat out of date", timeout=60)
    beat = seen["read"]
    assert beat["refresh_path"]["live"] is False, beat
    assert beat["cause"] == "queued" and beat["root"]["kind"] == "queue", beat
