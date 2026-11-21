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
    assert out.returncode == 0, out.stderr[-4000:]
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


def test_a_run_list_never_counts_fewer_runs_than_it_lists(tmp_path, serve):
    """`GET /runs` counts the runs and then lists them, and a run recorded between the two was
    listed without being counted: `total: 18` over 19 rows, `truncated: false`."""
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    server = serve("--max-runs", "4", "--timezone", "utc")
    stop = threading.Event()
    torn: list[str] = []
    samples_ready = threading.Event()
    sample_lock = threading.Lock()
    polls = 0

    def submit() -> None:
        while not stop.is_set():
            server.request(
                "POST", "/runs", {"command": "get", "targets": ["last"], "refresh_all": True}
            )

    def poll() -> None:
        nonlocal polls
        while not stop.is_set():
            status, listing = server.request("GET", "/runs?limit=1000")
            if status != 200:
                continue
            with sample_lock:
                polls += 1
                if polls > 100:
                    samples_ready.set()
            if listing["total"] < len(listing["runs"]):
                torn.append(f"total {listing['total']} over {len(listing['runs'])} rows")

    threads = [threading.Thread(target=submit) for _ in range(2)]
    threads += [threading.Thread(target=poll) for _ in range(4)]
    for thread in threads:
        thread.start()
    # Keep the original load window, extending it on slow runners to collect enough lists.
    stop.wait(8)
    samples_ready.wait(timeout=52)
    stop.set()
    for thread in threads:
        thread.join()
    assert polls > 100, polls
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
