"""`barca status` decides a node by the rules a run follows (#985).

A run (`get`, `run`, a scheduled tick) reads a `Manual` node, and an asset pinned with
`barca pin`, as it is: it never recomputes it. Status decides the held node on its own, so it
reads `stale` once its inputs move on, and decides everything below it against the result a
run reads. So status, `barca get --dry-run`, a real `get` and a tick's recorded outcome say
the same thing for every node below a held one, with and without a cutoff asset in between.

A paused node is held by ticks only: a request recomputes it, so status (and the dry run)
say what a request would do, and the tick's outcome is in `last_check`.
"""

import json
from pathlib import Path

from .helpers import QUICK, barca, wait_until

PIPELINE = """
from pathlib import Path

from barca import Manual, Schedule, asset, sensor

FEED = Path(__file__).parent / "feed.txt"


@sensor(freshness=Schedule("0 0 1 1 *"))
def feed() -> int:
    return int(FEED.read_text())


@asset(inputs={"n": feed}, freshness=Manual)
def baseline(n: int) -> dict:
    return {"n": n}


@asset(inputs={"base": baseline})
def direct(base: dict) -> dict:
    return base


@asset(inputs={"d": direct})
def below(d: dict) -> dict:
    return d


@asset(inputs={"base": baseline}, cutoff=True)
def gate(base: dict) -> dict:
    return {"k": 1}


@asset(inputs={"g": gate})
def after_gate(g: dict) -> dict:
    return g


@asset(inputs={"n": feed})
def plain(n: int) -> dict:
    return {"n": n}


@asset(inputs={"p": plain})
def under_plain(p: dict) -> dict:
    return p
"""

# The nodes below the `Manual` node: read directly, through another asset, and through a
# cutoff asset.
BELOW = ["direct", "below", "gate", "after_gate"]
EVERY_SECOND = 'Schedule("* * * * * *")'


def _ok(done) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _status(root: Path) -> dict[str, dict]:
    doc = json.loads(_ok(barca(root, "status", "pipeline.py", "--json", timeout=QUICK)))
    return {n["name"]: n for n in doc["nodes"]}


def _steps(root: Path, target: str, *flags: str) -> dict[str, dict]:
    """The steps of `barca get TARGET` (a dry run with `--dry-run`), by node name."""
    doc = json.loads(_ok(barca(root, "get", target, "pipeline.py", "--json", *flags)))
    return {s["id"].rsplit(":", 1)[1]: s for s in doc["steps"]}


def _feed(root: Path, value: int) -> None:
    (root / "feed.txt").write_text(str(value))


def _computed(root: Path) -> None:
    """Everything computed once, against the first value of the feed."""
    _feed(root, 1)
    _ok(barca(root, "get", "baseline", "pipeline.py", "--refresh", "baseline"))
    for target in ("below", "after_gate", "under_plain"):
        _ok(barca(root, "get", target, "pipeline.py"))


def test_below_a_stale_manual_node_status_says_what_a_run_finds(project):
    _computed(project)
    # The feed moves on and is polled: `baseline` is stale, and stays as it is.
    _feed(project, 2)
    _ok(barca(project, "get", "below", "pipeline.py"))

    nodes = _status(project)
    # The `Manual` node itself is stale: only a person refreshes it.
    held = nodes["baseline"]["cache"]
    assert (held["state"], held["reason"]) == ("stale", "changed"), held
    assert nodes["baseline"]["health"] == "behind", nodes["baseline"]
    # Everything below it is up to date against its current result, and says which node is
    # held back and how to recompute it.
    for name in BELOW:
        cache = nodes[name]["cache"]
        assert (cache["state"], cache["reason"]) == ("cached", "materialized"), (name, cache)
        assert "up to date against 'baseline' as it is" in cache["detail"], cache
        assert "barca get baseline --refresh baseline" in cache["detail"], cache
        assert nodes[name]["health"] == "ok", nodes[name]

    # The dry run and the run say the same of every node below, with and without the cutoff.
    for target in ("below", "after_gate"):
        dry = _steps(project, target, "--dry-run")
        ran = _steps(project, target)
        assert "Manual and stale" in dry["baseline"]["detail"], dry
        for name in set(BELOW) & set(dry):
            assert dry[name]["action"] == "cached", (name, dry)
            assert ran[name]["status"] == "cached", (name, ran)
            assert nodes[name]["cache"]["run_hash"] == ran[name]["run_hash"], name
    # A status of one target decides the same, and still lists what is above the held node.
    scoped = json.loads(_ok(barca(project, "status", "below", "pipeline.py", "--json")))
    states = {n["name"]: n["cache"]["state"] for n in scoped["nodes"]}
    assert states == {
        "feed": "always_runs",
        "baseline": "stale",
        "direct": "cached",
        "below": "cached",
    }, states

    # Refreshed by hand, the `Manual` node has new bytes: what reads it is stale because its
    # input changed, and a `get` computes it, as before. Below the cutoff asset nothing can
    # be told until it has run.
    _ok(barca(project, "get", "baseline", "pipeline.py", "--refresh", "baseline"))
    nodes = _status(project)
    assert nodes["baseline"]["cache"]["state"] == "cached", nodes["baseline"]
    for name in ("direct", "gate"):
        cache = nodes[name]["cache"]
        assert (cache["state"], cache["reason"]) == ("stale", "changed"), (name, cache)
    assert nodes["below"]["cache"]["reason"] == "upstream_stale", nodes["below"]
    assert nodes["after_gate"]["cache"]["reason"] == "cutoff_pending", nodes["after_gate"]
    dry = _steps(project, "below", "--dry-run")
    assert [dry[n]["action"] for n in ("baseline", "direct", "below")] == ["cached", "run", "run"]
    assert _steps(project, "after_gate", "--dry-run")["after_gate"]["action"] == "unknown"
    ran = _steps(project, "below")
    assert [ran[n]["status"] for n in ("direct", "below")] == ["ran", "ran"], ran
    assert all(
        n["cache"]["state"] == "cached" for n in _status(project).values() if n["name"] in BELOW[:2]
    )


def test_a_manual_node_never_computed_holds_nothing(project):
    _feed(project, 1)
    nodes = _status(project)
    # Nothing to read as it is: a run stops at it, and status says nothing ran yet.
    for name in ("baseline", *BELOW):
        assert nodes[name]["cache"]["state"] in {"never_run", "unknown"}, nodes[name]
    done = barca(project, "get", "below", "pipeline.py", "--dry-run")
    assert done.returncode == 2 and "has never been computed" in done.stderr, done.stderr


def test_below_a_pinned_asset_status_says_what_a_run_finds(project):
    _computed(project)
    versions = json.loads(
        _ok(barca(project, "pin", "plain", "pipeline.py", "--versions", "--json"))
    )
    run_hash = versions["versions"][0]["run_hash"]
    _ok(barca(project, "pin", "plain", "pipeline.py", "--run-hash", run_hash, "--json"))
    # The feed moves on and is polled: the pinned asset's own code and inputs give another
    # result, which no run computes while it is pinned.
    _feed(project, 2)
    # (A run for `under_plain` does not walk above the pin; this one polls the feed.)
    _ok(barca(project, "get", "below", "pipeline.py"))
    ran = _steps(project, "under_plain")
    assert (ran["plain"]["status"], ran["plain"]["reason"]) == ("cached", "pinned"), ran
    assert ran["under_plain"]["status"] == "cached", ran

    nodes = _status(project)
    pinned = nodes["plain"]["cache"]
    assert (pinned["state"], pinned["reason"]) == ("stale", "changed"), pinned
    cache = nodes["under_plain"]["cache"]
    assert (cache["state"], cache["reason"]) == ("cached", "materialized"), cache
    assert "'plain' is pinned" in cache["detail"] and "barca unpin plain" in cache["detail"], cache
    assert cache["run_hash"] == ran["under_plain"]["run_hash"], cache
    dry = _steps(project, "under_plain", "--dry-run")
    assert dry["under_plain"]["action"] == "cached", dry

    # Unpinned, it is an asset like any other again.
    _ok(barca(project, "unpin", "plain", "pipeline.py", "--json"))
    nodes = _status(project)
    assert nodes["under_plain"]["cache"]["reason"] == "upstream_stale", nodes["under_plain"]
    assert _steps(project, "under_plain", "--dry-run")["under_plain"]["action"] == "run"


def test_status_agrees_with_what_each_tick_records(project, serve):
    source = project / "pipeline.py"
    source.write_text(source.read_text().replace('Schedule("0 0 1 1 *")', EVERY_SECOND))
    _computed(project)
    server = serve()

    def state() -> dict[str, dict]:
        return {n["name"]: n for n in server.get("/state")}

    def checked(names) -> dict[str, dict] | None:
        """The state, once a tick that polled the feed's new value has checked `names`."""
        nodes = state()
        seen = nodes["baseline"]["cache"]["state"] == "stale" and all(
            (nodes[n]["last_check"] or {}).get("outcome") == "up_to_date" for n in names
        )
        return nodes if seen else None

    # The feed moves on. Each tick polls it, reads `baseline` as it is and finds everything
    # below it up to date: so does the status the server answers.
    _feed(project, 2)
    nodes = wait_until(lambda: checked(BELOW), "a tick to check what is below baseline")
    assert nodes["baseline"]["last_check"] is None, nodes["baseline"]
    for name in BELOW:
        assert nodes[name]["cache"]["state"] == "cached", nodes[name]
        assert (nodes[name]["health"], nodes[name]["health_reason"]) == ("ok", "cached")
        assert "'baseline'" in nodes[name]["cache"]["detail"], nodes[name]
    # `barca status` on the same project reads the same.
    local = _status(project)
    assert {n: local[n]["cache"]["state"] for n in BELOW} == dict.fromkeys(BELOW, "cached")

    # A paused node is read as it is by ticks only. A request would recompute it, so status
    # and the dry run say `stale` for what is below it, and the tick's outcome is the check.
    _ok(barca(project, "pause", "plain", "--server", server.url, "--json"))
    _feed(project, 3)

    def paused() -> dict[str, dict] | None:
        nodes = state()
        check = nodes["under_plain"]["last_check"] or {}
        stale = nodes["plain"]["cache"]["state"] == "stale"
        return nodes if stale and check.get("outcome") == "up_to_date" else None

    nodes = wait_until(paused, "a tick to read the paused node as it is")
    cache = nodes["under_plain"]["cache"]
    assert (cache["state"], cache["reason"]) == ("stale", "upstream_stale"), cache
    dry = _steps(project, "under_plain", "--dry-run")
    assert [dry[n]["action"] for n in ("plain", "under_plain")] == ["run", "run"], dry
