"""The run list's filters are the database's (#969).

`GET /runs` and `barca history` take a status, a trigger kept or left out, a window of start
times and a text, and the list, its `total` and the last day's `activity` are of the runs they
keep, over the whole history and not the runs a page holds. Checked over a seeded history
(`run_history.py`): two days of every-minute ticks, with a few runs someone started, some of
them failed, and one from before barca recorded what started a run.
"""

import json
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path
from urllib.parse import urlencode

import pytest
from barca.api import _find_binary

from .helpers import RUN, clean_env
from .run_history import Run, project, seed, stamp
from .served import Serve, get

#: The middle of an hour, a few hours back: every seeded run is inside the activity's window
#: whenever the test runs, and none is in the hour still filling.
NOW = datetime.now(UTC).replace(minute=30, second=0, microsecond=0) - timedelta(hours=3)
HOUR = NOW.replace(minute=0)


def _history_of() -> list[Run]:
    """An hour of ticks (two failed) around the runs people started, oldest first."""
    runs = [
        Run(
            f"tick{minute:03d}",
            HOUR + timedelta(minutes=minute),
            status="failed" if minute in (7, 8) else "success",
            seconds=1.0 if minute % 2 else 3.0,
            failure=("pipeline.py:poll_inbox", "TimeoutError", "inbox timed out")
            if minute in (7, 8)
            else None,
        )
        for minute in range(60)
    ]
    return [
        # The hour before: one run someone started, and one from an older barca, written the
        # way a posted run is.
        Run("early", HOUR - timedelta(minutes=30), triggered_by="manual", target="orders"),
        Run(
            "legacy", HOUR - timedelta(minutes=20), triggered_by=None, target="orders", rfc3339=True
        ),
        *runs,
        Run(
            "report_bad",
            HOUR + timedelta(minutes=40, seconds=30),
            status="failed",
            triggered_by="api",
            target="daily_report",
            seconds=40.0,
            failure=("pipeline.py:orders[day=1]", "KeyError", "'region'"),
        ),
        Run(
            "report_ok",
            HOUR + timedelta(minutes=50, seconds=30),
            triggered_by="api",
            target="daily_report",
            seconds=20.0,
        ),
        Run(
            "stopped",
            HOUR + timedelta(minutes=55, seconds=30),
            status="cancelled",
            triggered_by="manual",
            target="orders",
        ),
    ]


@pytest.fixture(scope="module")
def served(tmp_path_factory):
    """A server on the seeded project: `(root, port, the real run's id)`."""
    root = tmp_path_factory.mktemp("runs")
    db = project(root, _find_binary())
    seed(db, _history_of())
    server = Serve(root, "--no-schedule", env=clean_env())
    try:
        [real] = [
            r
            for r in get(server.port, "/runs?search=daily&limit=10")["runs"]
            if r["status"] == "success" and not r["id"].startswith("report")
        ]
        yield root, server.port, real["id"]
    finally:
        server.stop()


def _runs(port: int, **query: str | int) -> dict:
    return get(port, f"/runs?{urlencode(query)}")


def _ids(listing: dict) -> list[str]:
    return [run["id"] for run in listing["runs"]]


def _history(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), "history", *args],
        cwd=root,
        env=clean_env(),
        capture_output=True,
        text=True,
        timeout=RUN,
    )


def test_hiding_the_schedules_runs_keeps_a_run_that_recorded_no_trigger(served):
    root, port, real = served
    every = _runs(port, limit=5)
    assert every["total"] == 66 and every["truncated"]
    assert every["activity"]["excluded"] is None
    kept = _runs(port, not_triggered_by="schedule")
    assert _ids(kept) == [real, "stopped", "report_ok", "report_bad", "legacy", "early"]
    assert (kept["total"], kept["truncated"]) == (6, False)
    # Kept by inclusion, the run with no trigger is lost: that is what the page used to ask.
    assert "legacy" not in _ids(_runs(port, triggered_by="manual,api,client"))
    # The activity is of the same runs, and says what the exclusion left out of the window.
    activity = kept["activity"]
    assert activity["not_triggered_by"] == ["schedule"]
    assert activity["total"]["runs"] == 6
    assert activity["total"]["by_trigger"] == {
        "schedule": 0, "api": 2, "manual": 3, "client": 0, "unknown": 1,
    }  # fmt: skip
    hidden = activity["excluded"]
    assert (hidden["runs"], hidden["not_successful"]) == (60, 2), hidden
    assert hidden["by_trigger"]["schedule"] == 60
    assert hidden["failure"] == {"text": "poll_inbox: TimeoutError: inbox timed out", "runs": 2}
    assert hidden["median_seconds"] == 2.0


def test_an_hour_of_the_activity_lists_its_runs_and_says_its_median_and_failure(served):
    root, port, real = served
    activity = _runs(port, limit=1)["activity"]
    [hour] = [h for h in activity["hours"] if h["at"] == stamp(HOUR, True)]
    assert (hour["runs"], hour["ok"], hour["not_successful"]) == (63, 59, 4), hour
    assert hour["until"] == stamp(HOUR + timedelta(hours=1), True)
    # 60 ticks of 1s and 3s, and runs of 40s, 20s and 1s: the median of 63.
    assert hour["median_seconds"] == 3.0
    # Two ticks timed out, one report failed on a key, one run was cancelled.
    assert hour["failure"] == {"text": "poll_inbox: TimeoutError: inbox timed out", "runs": 2}
    listed = _runs(port, since=hour["at"], until=hour["until"], limit=1000)
    assert listed["total"] == 63 and len(listed["runs"]) == 63
    assert "legacy" not in _ids(listed) and "early" not in _ids(listed)
    # The hour before holds the run written with a `T`; the activity stays the whole day.
    before = _runs(port, since=stamp(HOUR - timedelta(hours=1), True), until=hour["at"])
    assert _ids(before) == ["legacy", "early"]
    assert before["activity"]["total"]["runs"] == 66
    # Without the schedule's runs, the hour's leading failure is the report's: a partitioned
    # node's key is said under the node's name.
    mine = _runs(port, not_triggered_by="schedule", limit=1)["activity"]
    [hour] = [h for h in mine["hours"] if h["at"] == stamp(HOUR, True)]
    assert hour["failure"] == {"text": "cancelled", "runs": 1}, "the latest on a tie"
    assert hour["median_seconds"] == 20.0


def test_status_and_search_are_counted_over_every_run_not_the_page(served):
    root, port, real = served
    bad = _runs(port, status="not_successful", limit=2)
    assert _ids(bad) == ["stopped", "report_bad"]
    assert (bad["total"], bad["truncated"]) == (4, True)
    assert bad["activity"]["total"]["runs"] == 4
    assert bad["activity"]["statuses"] == ["failed", "error", "cancelled", "interrupted"]
    assert _runs(port, status="failed")["total"] == 3
    # Text in the run id, the target or the job, whatever its case.
    assert _ids(_runs(port, search="DAILY_rep")) == [real, "report_ok", "report_bad"]
    assert _runs(port, search="tick00")["total"] == 10
    assert _runs(port, search="pipeline.py:orders")["total"] == 3
    assert _runs(port, search="%")["total"] == 0
    # Together: what went wrong in the runs someone started, about the report.
    both = _runs(port, status="not_successful", not_triggered_by="schedule", search="report")
    assert _ids(both) == ["report_bad"]
    assert both["activity"]["total"]["failure"] == {"text": "orders: KeyError: 'region'", "runs": 1}


@pytest.mark.parametrize(
    "flags",
    [
        ["--not-triggered-by", "schedule"],
        ["--status", "not_successful"],
        ["--search", "daily_rep"],
        ["--status", "failed", "--not-triggered-by", "schedule", "--search", "REPORT"],
        [
            "--since",
            stamp(HOUR, True),
            "--until",
            stamp(HOUR + timedelta(hours=1), True),
            "-l",
            "5",
        ],
        ["--target", "orders", "--until", stamp(HOUR, True)],
        ["--activity", "--not-triggered-by", "schedule"],
        ["--activity", "--status", "not_successful", "--search", "report"],
    ],
)
def test_the_cli_reads_the_same_here_and_through_the_server(served, flags):
    """`barca history` with each new flag: this machine's database and the server's answer."""
    root, port, real = served
    local = _history(root, *flags, "--json", "--local")
    remote = _history(root, *flags, "--json", "--server", f"http://127.0.0.1:{port}")
    assert local.returncode == 0, local.stderr
    assert remote.returncode == 0, remote.stderr
    assert json.loads(local.stdout) == json.loads(remote.stdout)
    # And as tables.
    local = _history(root, *flags, "--pretty", "--local")
    remote = _history(root, *flags, "--pretty", "--server", f"http://127.0.0.1:{port}")
    assert local.stdout == remote.stdout
    assert local.stdout.strip()


def test_the_cli_flags_keep_what_the_route_keeps(served):
    root, port, real = served
    out = json.loads(_history(root, "--not-triggered-by", "schedule", "--json", "--local").stdout)
    # The same runs; the route orders them by start, the CLI as they were recorded (the real
    # run was recorded first and started last).
    kept = _ids(_runs(port, not_triggered_by="schedule"))
    assert sorted(r["run_id"] for r in out["runs"]) == sorted(kept)
    assert out["total"] == 6
    table = _history(root, "--activity", "--not-triggered-by", "schedule", "--pretty", "--local")
    assert "left out: 60 schedule runs, 2 not successful" in table.stdout, table.stdout
    assert "MEDIAN  FAILURE" in table.stdout
    # A value the flags refuse is a usage error naming the flag; the route names its parameter.
    refused = _history(root, "--not-triggered-by", "cron", "--local")
    assert refused.returncode == 2 and "--not-triggered-by 'cron'" in refused.stderr
    refused = _history(root, "--until", "soon", "--local")
    assert refused.returncode == 2 and "--until 'soon'" in refused.stderr
    with pytest.raises(Exception) as bad:
        _runs(port, not_triggered_by="cron")
    assert "400" in str(bad.value)
