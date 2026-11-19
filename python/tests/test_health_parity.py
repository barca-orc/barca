"""One definition of health: `barca status` and a server answer from the same computation.

`barca status --json` reads the metadata database; `GET /state` reads a server's read cache,
its scheduler and its run queue. Both fill one inputs type in barca-core
(`health::Inputs`), so on one database every node's health, cause, reason and estimate are
the same on both, whichever host answers (#1056).

The fixture is built so that the definition of a typical duration matters: `orders` has two
successful runs, one near 0 s and one near 12 s. The median of an even count is the mean of
the two middle values on every surface; a server once served the lower one.
"""

import json
import time
from datetime import datetime
from pathlib import Path

from .helpers import QUICK, barca, wait_until

# One schedule that never fires during the test, so its next tick is one fixed instant.
PIPELINE = """
import time

from barca import Always, Manual, Schedule, asset, sensor


@sensor(freshness=Schedule("0 0 1 1 *"))
def inbox() -> int:
    return 1


@asset(inputs={"n": inbox}, freshness=Always)
def orders(n: int) -> dict:
    time.sleep(SLEEP)
    return {"n": n, "v": VERSION}


@asset(inputs={"o": orders}, freshness=Always)
def report(o: dict) -> dict:
    return {"o": o}


@asset(freshness=Manual)
def loose() -> dict:
    return {}
"""

# What a node says of its health and when it will be fresh: every field classification and
# the estimates fill (`barca_core::health::classify`).
FIELDS = (
    "health",
    "reason",
    "cause",
    "root",
    "blocked_by",
    "stale_since",
    "expected_fresh_at",
    "recheck_at",
    "no_estimate",
    "refresh_path",
    "schedule",
    "last_attempt",
    "typical_run_seconds",
)


def _write(root: Path, sleep: float, version: int) -> None:
    source = PIPELINE.replace("SLEEP", str(sleep)).replace("VERSION", str(version))
    (root / "pipeline.py").write_text(source)


def _ok(done) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _picked(nodes: list[dict]) -> dict[str, dict]:
    return {n["name"]: {field: n.get(field) for field in FIELDS} for n in nodes}


def _local(root: Path) -> dict[str, dict]:
    out = _ok(barca(root, "status", "pipeline.py", "--json", "--all", timeout=QUICK))
    return _picked(json.loads(out)["nodes"])


def _settled(read):
    """`read()` twice with the same answer: nothing is moving between the two surfaces."""
    seen = [read()]

    def same():
        seen.append(read())
        return seen[-1] == seen[-2] and seen[-1]

    return wait_until(same, "two equal answers in a row", interval=0.3)


def test_status_and_state_say_the_same_of_every_node(tmp_path, serve):
    # Two successful runs of `orders`: about 0 s and about 12 s.
    _write(tmp_path, 0, 1)
    _ok(barca(tmp_path, "get", "report", "pipeline.py"))
    _write(tmp_path, 12, 2)
    _ok(barca(tmp_path, "get", "report", "pipeline.py"))
    time.sleep(1.1)  # results are recorded to the second
    # A third definition nothing has run: `orders` and `report` are out of date.
    _write(tmp_path, 0, 3)

    # No scheduler anywhere: a read-only server and the CLI read the same records.
    reader = serve("--read-only")
    local = _local(tmp_path)
    assert local["orders"]["health"] != "ok", local
    # What the web UI used to work out is served, by both, from one computation (#1056): how
    # the newest attempt went, and the typical time of a run (the median of about 0 s and
    # about 12 s; none for a node that never ran).
    assert local["orders"]["last_attempt"] == "success", local
    assert local["loose"]["last_attempt"] == "never_run", local
    assert 5.5 < local["orders"]["typical_run_seconds"] < 9, local
    assert local["loose"]["typical_run_seconds"] is None, local
    served = _picked(reader.get("/state"))
    assert served == local, {
        (name, field): (served[name][field], local[name][field])
        for name in local
        for field in FIELDS
        if served[name][field] != local[name][field]
    }
    assert _picked(reader.get("/nodes")["nodes"]) == local
    reader.stop()

    # A server whose scheduler fires `inbox`: now there are estimates, made of the typical
    # durations. The CLI sees that scheduler in the records; the server holds it.
    server = serve()
    wait_until(
        lambda: _picked(server.get("/state"))["orders"]["expected_fresh_at"],
        "an estimate for orders",
    )
    served, local = _settled(lambda: (_picked(server.get("/state")), _local(tmp_path)))
    assert served == local
    assert _picked(server.get("/nodes")["nodes"]) == local

    # The estimate is the next tick plus the typical durations on the path, rounded up to
    # 5 s. `orders` ran in about 0 s and about 12 s: its typical duration is about 6 s (10 s
    # after the tick), not the lower sample (5 s after it).
    state = {n["name"]: n for n in server.get("/state")}
    typical = state["orders"]["durations"]
    assert typical["samples"] == 2, typical
    assert 5.5 < typical["median_seconds"] < 9, typical
    assert typical["p95_seconds"] > 11.5, typical
    fresh = datetime.fromisoformat(local["orders"]["expected_fresh_at"]).timestamp()
    assert fresh - state["inbox"]["next_run"] == 10, (local["orders"], typical)

    # The rollup sums the same rows: what each root costs is the same number.
    def roots(doc: dict) -> list[tuple]:
        return sorted(
            (r["id"], r["kind"], r["action"]["kind"], r["action"]["estimated_seconds"])
            for r in doc["roots"]
        )

    def both():
        out = _ok(barca(tmp_path, "status", "pipeline.py", "--rollup", "--json", timeout=QUICK))
        return roots(server.get("/state/rollup")), roots(json.loads(out))

    served_roots, local_roots = _settled(both)
    assert served_roots == local_roots and local_roots

    # What a pause would cover is one walk, the scheduler's: the CLI prints the route's answer.
    cover = server.get("/pauses?target=orders&downstream=true")["covers"]
    out = _ok(
        barca(
            tmp_path,
            "pause",
            "orders",
            "--downstream",
            "--dry-run",
            "--server",
            server.url,
            "--json",
            timeout=QUICK,
        )
    )
    assert json.loads(out)["covers"] == cover
    assert [t.rsplit(":", 1)[1] for t in cover["targets"]] == ["orders"]
    assert [d.rsplit(":", 1)[1] for d in cover["downstream"]] == ["report"]
    # `orders` is an `Always` asset below the scheduled sensor: a target of its ticks.
    assert [(s["id"].rsplit(":", 1)[1], s["relation"]) for s in cover["schedules"]] == [
        ("inbox", "target")
    ]
    # `loose` is `Manual` and nothing reads it: no tick reaches it.
    assert server.get("/pauses?target=loose&downstream=true")["covers"]["schedules"] == []
    # Nothing was paused by asking.
    assert server.get("/pauses")["paused"] == []
    assert "covers" not in server.get("/pauses")
