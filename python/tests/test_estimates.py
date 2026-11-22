"""When a node went out of date and when it will be fresh again (#962).

`stale_since`, `expected_fresh_at` and `recheck_at` on every node, a queued run's
`estimated_start_at`, and a schedule's typical queue wait and run time: the same values in
`barca status --json`, a server's `GET /state` and `GET /nodes`, and `barca.nodes`.

The accuracy test runs a real server with one run slot and two schedules ticking every few
seconds, records each estimate when it is first given and then the time the thing happened,
and asserts the error stays within the bound `specs/behavior/health.md` states.
"""

import json
import time
from datetime import UTC, datetime
from pathlib import Path

import pytest

from .helpers import QUICK, barca, wait_until

# Two schedules on one run slot: `feed` ticks two seconds before `inbox` and its run takes
# three, so every tick of `inbox` waits in the run queue. `orders` and what is below it take a
# known time; `gate` is a cutoff asset, so `below_gate` cannot be decided before it runs.
PIPELINE = """
import time

from barca import Always, Manual, Schedule, asset, sensor

VERSION = 0


@sensor(freshness=Schedule("*/10 * * * * *"))
def inbox() -> int:
    return 1


@asset(inputs={"n": inbox}, freshness=Always)
def orders(n: int) -> dict:
    time.sleep(0.6)
    return {"n": n, "version": VERSION}


@asset(inputs={"o": orders}, freshness=Always)
def clean(o: dict) -> dict:
    time.sleep(0.8)
    return o


@asset(inputs={"o": orders}, freshness=Always)
def side(o: dict) -> dict:
    time.sleep(0.4)
    return o


@asset(inputs={"c": clean, "s": side}, freshness=Always)
def report(c: dict, s: dict) -> dict:
    time.sleep(0.5)
    return {"c": c, "s": s}


@asset(inputs={"o": orders}, freshness=Always, cutoff=True)
def gate(o: dict) -> dict:
    return {"n": o["n"]}


@asset(inputs={"g": gate}, freshness=Always)
def below_gate(g: dict) -> dict:
    return g


@sensor(freshness=Schedule("8/10 * * * * *"))
def feed() -> float:
    return time.time()


@asset(inputs={"f": feed}, freshness=Always)
def feed_rows(f: float) -> dict:
    time.sleep(3.0)
    return {"f": f}


@asset(freshness=Manual)
def loose() -> dict:
    return {}
"""

# What the estimates are held to (`specs/behavior/health.md`, "Times"): an estimate is rounded
# up to 5 seconds, and a run's startup (workers importing the project) is not modelled.
FRESH_ERROR_SECONDS = 8.0
START_ERROR_SECONDS = 6.0
CHAIN = ("orders", "clean", "side", "report")


def _ok(done) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _at(text: str) -> float:
    value = datetime.fromisoformat(text.replace("Z", "+00:00"))
    # Legacy materialization timestamps are UTC without an offset.
    return (value if value.tzinfo else value.replace(tzinfo=UTC)).timestamp()


def _state(server) -> dict[str, dict]:
    return {n["name"]: n for n in server.get("/state")}


def _edit(root: Path, version: int) -> float:
    """Change `orders`' code; returns when."""
    source = (root / "pipeline.py").read_text()
    before = f"VERSION = {version - 1}"
    assert before in source
    (root / "pipeline.py").write_text(source.replace(before, f"VERSION = {version}"))
    return time.time()


@pytest.mark.timeout(300)
def test_estimates_are_within_the_stated_error_on_a_real_server(project, serve):
    server = serve("--max-runs", "1")

    # Let both schedules tick a few times: typical durations, queue waits and run times.
    def warmed() -> bool:
        ticked = {s["id"]: s for s in server.get("/schedule")}
        return all(s["typical_run_seconds"] is not None for s in ticked.values()) and all(
            (n.get("durations") or {}).get("samples", 0) >= 1
            for name, n in _state(server).items()
            if name in (*CHAIN, "feed_rows")
        )

    wait_until(warmed, "both schedules to tick a few times", timeout=90)
    schedules = {s["id"].rsplit(":", 1)[1]: s for s in server.get("/schedule")}
    # The sleep gives a lower bound, not an upper bound on a loaded runner. The
    # estimates below still have to predict the recorded completions within 8s/6s.
    assert schedules["feed"]["typical_run_seconds"] >= 3, schedules
    assert schedules["inbox"]["typical_queue_wait_seconds"] is not None, schedules

    fresh_errors: list[float] = []
    start_errors: list[float] = []
    for version in (1, 2, 3):
        # Out of date from now: the next tick of `inbox` brings the chain up to date.
        wait_until(
            lambda: all(n["health"] == "ok" for name, n in _state(server).items() if name in CHAIN),
            "the chain to be fresh",
            timeout=60,
        )
        edited = _edit(project, version)
        estimates: dict[str, float] = {}
        actual: dict[str, float] = {}
        starts: dict[str, float] = {}
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and len(actual) < len(CHAIN):
            state = _state(server)
            for name in CHAIN:
                node = state[name]
                if name in estimates and node["health"] == "ok":
                    # The observer can be descheduled after the server finishes. Compare
                    # with the recorded completion, not when this polling process woke.
                    actual.setdefault(name, _at(node["last_materialization"]["created_at"]))
                elif node["health"] != "ok" and node["expected_fresh_at"]:
                    if name not in estimates:
                        estimates[name] = _at(node["expected_fresh_at"])
                        # Recheck names the next tick. A run already queued/running can
                        # make the node fresh before that tick when observation is late.
                        assert node["recheck_at"] is not None, node
                        assert node["recheck_at"] == node["refresh_path"]["next_tick"], node
                        # Out of date since the edit, to the second.
                        assert abs(_at(node["stale_since"]) - edited) <= 3, (node, edited)
            # A node below a cutoff asset that would run is undecided: rechecked, never
            # promised fresh.
            below = state["below_gate"]
            if below["health"] == "undecided":
                assert below["expected_fresh_at"] is None, below
                assert below["recheck_at"] is not None, below
                assert below["stale_since"] is None, below
            # Nothing reaches `loose`.
            assert state["loose"]["recheck_at"] is None
            assert state["loose"]["expected_fresh_at"] is None
            queue = server.get("/queue")
            for entry in queue["queued"]:
                if entry["estimated_start_at"]:
                    starts.setdefault(entry["run_id"], _at(entry["estimated_start_at"]))
                    assert entry["wait_seconds"] is not None, entry
            time.sleep(0.1)
        assert set(actual) == set(CHAIN), (estimates, actual)
        fresh_errors += [actual[name] - estimates[name] for name in CHAIN]
        # A short run can start and finish between polls. Its history retains the start;
        # do not silently drop a queued estimate merely because /queue missed it running.
        for run_id, estimate in starts.items():
            finished = server.finished(run_id)
            assert finished["status"] == "success", finished
            start_errors.append(_at(finished["started_at"]) - estimate)

    worst = max(abs(e) for e in fresh_errors)
    report = (
        f"expected_fresh_at error (actual - estimate), seconds: "
        f"{[round(e, 1) for e in fresh_errors]}; estimated_start_at: "
        f"{[round(e, 1) for e in start_errors]}"
    )
    print(report)
    assert worst <= FRESH_ERROR_SECONDS, report
    assert start_errors, "no queued run was observed starting"
    assert all(abs(e) <= START_ERROR_SECONDS for e in start_errors), report


def test_every_surface_carries_the_same_times(project, serve):
    _ok(barca(project, "get", "report", "pipeline.py"))
    time.sleep(1.1)  # results are recorded to the second
    edited = _edit(project, 1)

    def times(nodes: list[dict]) -> dict[str, tuple]:
        return {
            n["name"]: (
                n["stale_since"],
                n["expected_fresh_at"],
                n["recheck_at"],
                n["no_estimate"],
            )
            for n in nodes
        }

    doc = json.loads(_ok(barca(project, "status", "pipeline.py", "--json", timeout=QUICK)))
    local = times(doc["nodes"])
    # No scheduler here: nothing is estimated; a code change is dated by the file.
    assert all(fresh is None and recheck is None for _, fresh, recheck, _ in local.values())
    # So every node that is not fresh says why it has no estimate (#968), and a fresh one
    # says nothing: no scheduler fires the chain's schedule, and nothing refreshes `loose`.
    fresh_nodes = {n["name"] for n in doc["nodes"] if n["cause"] is None}
    assert {name for name, t in local.items() if t[3] is None} == fresh_nodes, local
    assert local["orders"][3] == local["report"][3] == "scheduler_off", local
    assert local["loose"][3] == "no_path", local
    assert abs(_at(local["orders"][0]) - edited) <= 3, local
    # What is below `orders` has been out of date as long; a fresh node has no time.
    assert local["clean"][0] == local["orders"][0] == local["report"][0], local
    assert local["inbox"][0] is None and local["feed_rows"][0] is None, local
    # Never run: since the file was last written.
    assert local["loose"][0] is not None, local

    fields = _ok(
        barca(
            project,
            "status",
            "pipeline.py",
            "--fields",
            "name,stale_since,expected_fresh_at,recheck_at,no_estimate",
        )
    )
    assert times(json.loads(fields)["nodes"]) == local
    query = "select name, stale_since, expected_fresh_at, recheck_at, no_estimate from barca.nodes"
    rows = json.loads(_ok(barca(project, "sql", query, "pipeline.py", "--json", timeout=QUICK)))
    assert times(rows["rows"]) == local

    # A server that fires nothing says the same on both routes.
    server = serve("--read-only")
    assert times(server.get("/state")) == local
    assert times(server.get("/nodes")["nodes"]) == local
    rollup = server.get("/state/rollup")
    root = rollup["entries"][0]
    # Nothing is estimated, so every node that is not fresh is unbounded.
    not_fresh = sum(1 for n in doc["nodes"] if n["health"] not in ("ok", "suspect"))
    assert (root["fresh_by"], root["unbounded"]) == (None, not_fresh), root


def test_the_table_says_fresh_only_of_an_estimate(project, serve):
    server = serve("--max-runs", "1")
    wait_until(
        lambda: all(_state(server)[name]["health"] == "ok" for name in CHAIN),
        "the first tick to finish",
        timeout=60,
    )
    _edit(project, 1)

    def shown() -> bool:
        table = barca(project, "status", "pipeline.py", "--pretty", "--server", server.url)
        lines = {row.split()[0]: row for row in table.stdout.splitlines() if row.strip()}
        rollup = server.get("/state/rollup")["entries"][0]
        return (
            "fresh ~" in lines.get("orders", "")
            and "fresh" not in lines.get("below_gate", "").split("cutoff_pending", 1)[-1]
            and rollup["fresh_by"] is not None
        )

    wait_until(shown, "the estimate to show", timeout=30)
