"""`barca pin` / `barca unpin` (`barca docs pins`): an asset pinned to an earlier
materialization is read as that materialization by every run (what reads it, `barca get`,
`barca.get()`, a scheduled tick) and never computed, even when refreshed, until it is unpinned.
"""

import json
import subprocess
import sys
import time

from .helpers import barca, clean_env
from .served import Serve

PIPELINE = """
import os

from barca import asset, task, Schedule


@asset(env=["FEED"], freshness=Schedule("* * * * * *"))
def raw() -> dict:
    return {"v": int(os.environ["FEED"])}


@asset(inputs={"raw": raw})
def doubled(raw: dict) -> dict:
    return {"v": raw["v"] * 2}


@task(inputs={"raw": raw})
def publish(raw: dict) -> None:
    pass
"""


def _get(project, target, feed, *more):
    r = barca(project, "get", target, "-o", "value", *more, env=clean_env(FEED=feed))
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)["v"]


def _json(project, *args, code=0, feed="5"):
    r = barca(project, *args, "--json", env=clean_env(FEED=feed))
    assert r.returncode == code, (r.stdout, r.stderr)
    return json.loads(r.stdout) if code == 0 else json.loads(r.stderr.strip().splitlines()[-1])


def _two_versions(project):
    assert _get(project, "doubled", "1") == 2
    assert _get(project, "doubled", "5") == 10


def test_a_pin_holds_what_reads_it_and_unpin_restores_it(project):
    _two_versions(project)
    versions = _json(project, "pin", "raw", "--versions")
    assert versions["pinnable"] is True and versions["pin"] is None
    assert [v["current"] for v in versions["versions"]] == [True, False]
    good = versions["versions"][1]["run_hash"]

    pinned = _json(project, "pin", "raw", "--previous", "--reason", "bad feed")
    assert pinned["pin"]["run_hash"] == good and pinned["pin"]["reason"] == "bad feed"
    assert pinned["replaced"] is None
    assert _json(project, "pin")["pins"][0]["node_id"].endswith(":raw")

    # What reads it computes from the pinned result; the asset itself is not run.
    result = _json(project, "get", "doubled")
    steps = {s["id"].rsplit(":", 1)[1]: s for s in result["steps"]}
    assert steps["raw"]["status"] == "cached" and steps["raw"]["reason"] == "pinned"
    assert _get(project, "doubled", "5") == 2
    assert _get(project, "raw", "5") == 1
    # Not a refresh, not a refresh of everything.
    assert _get(project, "raw", "5", "--refresh", "raw") == 1
    assert _get(project, "doubled", "5", "--refresh-all") == 2
    dry = _json(project, "get", "doubled", "--dry-run")
    raw_step = next(s for s in dry["steps"] if s["id"].endswith(":raw"))
    assert raw_step["action"] == "cached" and raw_step["reason"] == "pinned", dry

    # barca.get() reads it too.
    code = "import barca, json; print(json.dumps(barca.get('doubled')))"
    out = subprocess.run(
        [sys.executable, "-c", code],
        cwd=project,
        env=clean_env(FEED="5"),
        capture_output=True,
        text=True,
        check=True,
    )
    assert json.loads(out.stdout.strip().splitlines()[-1]) == {"v": 2}

    removed = _json(project, "unpin", "raw")
    assert removed["removed"]["run_hash"] == good
    assert _json(project, "unpin", "raw")["removed"] is None
    assert _get(project, "doubled", "5") == 10


def test_pinning_refuses_what_cannot_be_pinned(project):
    assert _get(project, "doubled", "1") == 2
    err = _json(project, "pin", "raw", "--previous", code=2)
    assert "no previous one" in err["error"], err
    err = _json(project, "pin", "raw", "--run-hash", "000000", code=2)
    assert "no successful materialization" in err["error"], err
    err = _json(project, "pin", "publish", "--previous", code=2)
    assert "cannot be pinned" in err["error"], err
    r = barca(project, "pin", "raw", env=clean_env(FEED="1"))
    assert r.returncode == 2 and "--run-hash" in r.stderr, r.stderr
    # A unique prefix of a run hash pins that version.
    run_hash = _json(project, "pin", "raw", "--versions")["versions"][0]["run_hash"]
    assert _json(project, "pin", "raw", "--run-hash", run_hash[:8])["pin"]["run_hash"] == run_hash


def test_a_scheduled_tick_does_not_compute_a_pinned_asset(project):
    _two_versions(project)
    _json(project, "pin", "raw", "--previous")
    before = _json(project, "pin", "raw", "--versions")["versions"]
    # FEED=7 would be a new version of `raw` at the next tick, were it not pinned.
    with Serve(project, "--no-schedule", env=clean_env(FEED="7"), wait="health") as idle:
        status, pins = idle.request("GET", "/pins")
        assert status == 200 and len(pins["pins"]) == 1
    with Serve(project, env=clean_env(FEED="7")) as served:
        run_id = served.wait(
            "a finished tick of raw",
            lambda: (j := served.schedule()["raw"]).get("last_run")
            and j["last_status"] == "complete"
            and j["last_run"],
        )
        run = served.finished(run_id)
        assert run["triggered_by"] == "schedule", run
        step = next(s for s in run["outcome"]["steps"] if s["id"].endswith(":raw"))
        assert step["status"] == "cached" and step["reason"] == "pinned", run
        time.sleep(1.5)
        status, versions = served.request("GET", "/assets/raw/versions")
        assert status == 200
        assert [v["run_hash"] for v in versions["versions"]] == [v["run_hash"] for v in before]
        assert versions["pin"]["run_hash"] == before[1]["run_hash"]

        # Unpinned over HTTP, the next tick computes it again.
        status, removed = served.request("DELETE", "/assets/raw/pin")
        assert status == 200 and removed["removed"] is not None, removed
        served.wait(
            "a new version of raw",
            lambda: len(served.request("GET", "/assets/raw/versions")[1]["versions"]) == 3,
        )
