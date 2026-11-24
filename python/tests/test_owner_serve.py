"""A real `barca serve` as the long-lived holder of the metadata connection (#1056, A7 and A8).

`tests/simulation/owner_serve.py` holds a server up while its schedules, backups, size checks
and read routes run and other barca processes come and go, then stops and kills it and restores
a backup beside CLI processes; it fails on any database error, lost run, torn read or bad
backup. The long run is a person's (`--seconds 600`); CI's full tier runs the short one here
(about a minute), as it runs `test_simulation.py`.
"""

import json
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest
from barca.api import _find_binary

from .helpers import SERVE

SCRIPT = Path(__file__).resolve().parents[2] / "tests/simulation/owner_serve.py"

PIPELINE = """
import time

from barca import Schedule, asset, task


@asset()
def base() -> dict:
    return {"n": 1}


@asset(inputs={"base": base})
def last(base: dict) -> dict:
    return {"n": base["n"] + 1}


@task(freshness=Schedule("* * * * * *"))
def tick() -> None:
    print("tick")
"""


@pytest.mark.timeout(300)
def test_a_server_holds_its_connection_through_backups_trims_kills_and_a_restore(tmp_path):
    report = tmp_path / "report.json"
    out = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--barca",
            _find_binary(),
            "--seed",
            "1",
            "--seconds",
            "20",
            "--wait-for-coverage",
            "--kills",
            "1",
            "--limit-mb",
            "6",
            "--quiet",
            "--keep",
            str(tmp_path),
            "--report",
            str(report),
            # A shared CI runner stalls for longer than the bar now and then: the long run
            # holds the read routes to it, this one reports them.
            "--no-latency-bar",
        ],
        capture_output=True,
        text=True,
        timeout=SERVE,
        check=False,
    )
    assert out.returncode == 0, out.stderr[-4000:] + "\n" + out.stdout[-6000:]
    done = json.loads(report.read_text())
    assert done["errors"] == [], done["errors"]
    counts = done["counts"]
    # The run did what it is for: backups, size checks with a trim, kills, restores.
    assert counts["backups"] >= 3, counts
    assert counts["trims"] >= 1, counts
    assert counts["kills"] == 1 and counts["restores"] == 3, counts
    assert done["backups"]["backups checked"] == counts["backups"], done["backups"]
    assert done["history"]["recorded steps"] >= 200, done["history"]
    assert [stop["run status"] for stop in done["stops"]] == [
        "success",
        "cancelled",
        "interrupted",
    ], done["stops"]
    # A clean stop leaves the main file alone: no log for the next process to recover.
    assert set(done["clean stop"]["left on disk"]) == {""}, done["clean stop"]
    assert done["clean stop"]["warnings from the next commands"] == []


@pytest.mark.timeout(180)
def test_a_run_list_never_counts_fewer_runs_than_it_lists(tmp_path, serve):
    """`GET /runs` counts the runs and then lists them, and a run recorded between the two was
    listed without being counted: `total: 18` over 19 rows, `truncated: false`."""
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    server = serve("--max-runs", "4", "--timezone", "utc")
    stop = threading.Event()
    torn: list[str] = []
    sample_lock = threading.Lock()
    polls = 0
    completed = 0
    errors: list[str] = []

    def enough() -> None:
        # Both reads and writes must make progress; a fast reader alone is no coverage.
        if polls > 100 and completed >= 20:
            stop.set()

    def submit() -> None:
        nonlocal completed
        while not stop.is_set():
            status, body = server.request(
                "POST", "/runs", {"command": "get", "targets": ["last"], "refresh_all": True}
            )
            assert status == 202, body
            # Backpressure: two live producers, not an unbounded queue whose size depends
            # on machine speed (the old loop accumulated >11,000 pending runs).
            done = server.finished(body["run_id"])
            assert done["run"]["status"] == "success", done
            with sample_lock:
                completed += 1
                enough()

    def poll() -> None:
        nonlocal polls
        while not stop.is_set():
            status, listing = server.request("GET", "/runs?limit=1000")
            assert status == 200, listing
            with sample_lock:
                polls += 1
                if listing["total"] < len(listing["runs"]):
                    torn.append(f"total {listing['total']} over {len(listing['runs'])} rows")
                enough()

    def checked(work) -> None:
        try:
            work()
        except Exception as error:  # Propagate worker failures to the test, not a warning.
            with sample_lock:
                errors.append(repr(error))
            stop.set()

    threads = [threading.Thread(target=checked, args=(submit,)) for _ in range(2)]
    threads += [threading.Thread(target=checked, args=(poll,)) for _ in range(4)]
    for thread in threads:
        thread.start()
    try:
        stop.wait(60)
    finally:
        stop.set()
        for thread in threads:
            thread.join(timeout=65)  # server.finished has a bounded 60-second wait.
    assert not any(thread.is_alive() for thread in threads), "traffic did not stop"
    assert not errors, errors
    assert polls > 100 and completed >= 20, (polls, completed)
    assert not torn, f"{len(torn)} of {polls} lists: {torn[:3]}"


def test_a_run_list_stays_prompt_beside_the_runs_a_server_remembers(tmp_path, serve):
    """A server remembers the runs it finished in the last hour, and `GET /runs` asked the
    database about each of them: 0.8 s for a page of 50 beside 2,000 finished runs, on a
    server doing nothing else. Read routes answer within 500 ms (#921)."""
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    server = serve("--no-schedule", "--max-runs", "8")
    for _ in range(2000):
        status, _ = server.request("POST", "/runs", {"command": "get", "targets": ["last"]})
        assert status == 202
    server.wait(
        "the runs to finish",
        lambda: (q := server.get("/queue")) and not q["running"] and not q["queued"],
        timeout=SERVE,
    )
    took = []
    for _ in range(15):
        began = time.monotonic()
        listing = server.get("/runs?limit=50")
        took.append(time.monotonic() - began)
    assert (listing["total"], len(listing["runs"])) == (2000, 50)
    median = sorted(took)[len(took) // 2]
    assert median < 0.5, f"GET /runs?limit=50 took {median * 1000:.0f} ms beside 2000 finished runs"


@pytest.mark.parametrize("phase", ["", "kill"])
def test_a_broken_event_stream_is_only_expected_during_a_stop(tmp_path, monkeypatch, phase):
    """An abruptly killed chunked response raises HTTPException, not OSError."""
    import http.client
    import random
    import runpy
    import urllib.request
    from argparse import Namespace

    harness_type = runpy.run_path(str(SCRIPT))["Harness"]
    harness = harness_type(
        Namespace(barca=_find_binary(), limit_mb=6, backup_secs=2, trace=False, quiet=True),
        tmp_path,
    )
    monkeypatch.setattr(harness, "submit", lambda *args, **kwargs: "run")
    monkeypatch.setattr(harness, "follow", lambda *args, **kwargs: None)

    def broken(*args, **kwargs):
        raise http.client.IncompleteRead(b"partial event")

    monkeypatch.setattr(urllib.request, "urlopen", broken)
    if phase:
        harness.enter(phase)
    # Random(4)'s second draw takes the event-stream path.
    harness.api_runs(random.Random(4))
    if phase:
        assert not harness.problems
        assert sum(harness.explained.values()) == 1
    else:
        assert len(harness.problems) == 1
        assert "IncompleteRead" in harness.problems[0]["detail"]
        assert harness.problems[0]["class"] == "unexplained"


@pytest.mark.parametrize("reason,errors", [("refreshed_by_this_run", 0), ("cache_hit", 1)])
def test_local_refresh_check_requires_evidence_for_reused_steps(reason, errors):
    """The stress check counts post-acceptance reuse, but rejects ordinary old cache hits."""
    import random
    import runpy
    from types import SimpleNamespace

    harness_type = runpy.run_path(str(SCRIPT))["Harness"]
    harness = object.__new__(harness_type)
    harness.pause_writers = threading.Event()
    harness.lock = threading.Lock()
    harness.local_runs = {}
    harness.now = lambda: 0
    problems = []
    harness.problem = lambda *args: problems.append(args)
    done = {
        "run_id": "local-refresh",
        "status": "success",
        "steps_executed": 12,
        "steps": [
            {"id": node, "status": "cached", "reason": reason}
            for node in ("pipeline.py:base", "pipeline.py:total", "pipeline.py:last")
        ],
    }
    harness.cli = lambda *args: SimpleNamespace(stdout=json.dumps(done), stderr="")
    harness.local_gets(random.Random(1))  # selects the whole 15-step pipeline
    assert len(problems) == errors, problems
