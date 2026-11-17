"""`barca serve` shows a `barca get` from a shell on the same project as running (#534).

A CLI run streams nothing to the server; its `running` row in `.barca/metadata.db`, with a live
owner, is what `GET /state` and `GET /runs?status=running` report. A killed CLI run is not shown.
"""

import json
import os
import re
import signal
import subprocess
import urllib.request
from pathlib import Path

from barca.api import _find_binary

from .helpers import QUICK, RUN, clean_env, wait_until
from .served import Serve

PIPELINE = """
import os
import time
from pathlib import Path

from barca import asset


@asset()
def upstream() -> int:
    return 1


@asset(inputs={"upstream": upstream})
def slow(upstream: int) -> dict:
    Path("cli.started").write_text("")
    deadline = time.time() + 120
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return {"n": upstream}
"""


def _get(port: int, path: str):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5) as resp:
            return json.loads(resp.read())
    except OSError:
        return None


def _running_of(port: int, name: str):
    nodes = _get(port, "/state")
    if nodes is None:
        return None
    return next(n for n in nodes if n["name"] == name)["running"]


def _start(root: Path, *args: str, log: str) -> subprocess.Popen:
    """A `barca` CLI command in a session of its own, its stderr in `log`."""
    return subprocess.Popen(
        [_find_binary(), *args],
        cwd=root,
        env=clean_env(),
        stdout=subprocess.DEVNULL,
        stderr=(root / log).open("w"),
        start_new_session=True,
    )


def _serve(root: Path) -> Serve:
    """`barca serve --no-schedule` in a session of its own, as `_start` starts the CLI."""
    return Serve(root, "--no-schedule", env=clean_env(), new_session=True)


def _stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        os.killpg(proc.pid, signal.SIGKILL)
    proc.wait(timeout=QUICK)


def _stop_server(server: Serve) -> None:
    server.send(signal.SIGKILL)
    server.exited()


def _project(root: Path) -> None:
    (root / "pipeline.py").write_text(PIPELINE)
    (root / "barca.toml").write_text("")


def test_a_cli_run_on_the_same_project_shows_as_running_until_it_ends(tmp_path):
    _project(tmp_path)
    serve = _serve(tmp_path)
    port = serve.port
    cli = None
    try:
        cli = _start(tmp_path, "get", "slow", log="cli.log")
        wait_until(
            lambda: (tmp_path / "cli.started").exists() or cli.poll() is not None, "the CLI run"
        )
        assert cli.poll() is None, (tmp_path / "cli.log").read_text()

        # Within one poll: the CLI run's target is running; what it already finished is not.
        running = wait_until(
            lambda: _running_of(port, "slow"), "slow to show as running", timeout=10
        )
        assert running["triggered_by"] == "manual", running
        assert running["step"] == "running", running
        assert running["partitions"] is None, running
        assert _running_of(port, "upstream") is None
        detail = _get(port, "/assets/slow")
        assert detail["running"]["run_id"] == running["run_id"], detail
        [run] = _get(port, "/runs?status=running")["runs"]
        assert run["id"] == running["run_id"], run
        assert run["status"] == "running", run
        assert [s["node_id"].split(":")[-1] for s in run["active_steps"]] == ["slow"], run

        (tmp_path / "release").write_text("")
        assert cli.wait(timeout=RUN) == 0, (tmp_path / "cli.log").read_text()
        wait_until(
            lambda: _get(port, "/state") and _running_of(port, "slow") is None,
            "slow to clear",
            timeout=10,
        )
        assert _get(port, "/runs?status=running")["runs"] == []
    finally:
        (tmp_path / "release").write_text("")
        if cli is not None:
            _stop(cli)
        _stop_server(serve)


def test_a_killed_cli_run_is_not_shown_as_running(tmp_path):
    _project(tmp_path)
    cli = _start(tmp_path, "get", "slow", log="cli.log")
    wait_until(lambda: (tmp_path / "cli.started").exists() or cli.poll() is not None, "the CLI run")
    os.killpg(cli.pid, signal.SIGKILL)
    cli.wait(timeout=QUICK)
    serve = _serve(tmp_path)
    port = serve.port
    try:
        assert _running_of(port, "slow") is None
        assert _get(port, "/runs?status=running")["runs"] == []
    finally:
        _stop_server(serve)


def _history(root: Path, *args: str) -> subprocess.CompletedProcess:
    """`barca history ...` in `root`, as an agent runs it."""
    return subprocess.run(
        [_find_binary(), "history", *args],
        cwd=root,
        env=clean_env(),
        capture_output=True,
        text=True,
        timeout=RUN,
    )


def _in_flight(root: Path, *args: str) -> list[dict]:
    """The runs of `barca history --status running --json`."""
    out = _history(root, "--status", "running", "--json", *args)
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout)["runs"]


def test_history_says_what_a_run_in_flight_is_working_on_here_and_through_a_server(tmp_path):
    """#991: `now` of `GET /runs?status=running` from the CLI, locally and with `--server`."""
    _project(tmp_path)
    serve = _serve(tmp_path)
    server = ["--server", f"http://127.0.0.1:{serve.port}"]
    cli = None
    try:
        # Nothing in flight: no run, and so no `now`.
        assert _in_flight(tmp_path) == []
        cli = _start(tmp_path, "get", "slow", log="cli.log")
        wait_until(
            lambda: (tmp_path / "cli.started").exists() or cli.poll() is not None, "the CLI run"
        )
        assert cli.poll() is None, (tmp_path / "cli.log").read_text()

        # On this machine, from what the run recorded: its target has no result yet.
        [here] = _in_flight(tmp_path)
        now = here["now"]
        assert [n["node_id"] for n in now["running"]] == ["pipeline.py:slow"], now
        assert (now["queued"], now["keys"], now["groups"]) == (0, None, []), now
        assert now["line"] == "executing 1: slow", now
        # The server reads the same run by the same rule, and its route says the same.
        [there] = wait_until(lambda: _in_flight(tmp_path, *server), "the server to list the run")
        assert there["run_id"] == here["run_id"]
        assert there["now"] == now
        [route] = _get(serve.port, "/runs?status=running")["runs"]
        assert route["now"] == now
        # The table prints it under the run's row; `--fields` selects it.
        table = _history(tmp_path, "--status", "running", "--pretty")
        assert "\n  now: executing 1: slow\n" in table.stdout, table.stdout
        fields = _in_flight(tmp_path, "--fields", "run_id,now")
        assert fields == [{"run_id": here["run_id"], "now": now}]
        # Not asked for the runs in progress: the list stays as it was.
        listed = json.loads(_history(tmp_path, "--json").stdout)["runs"]
        assert all("now" not in run for run in listed), listed

        (tmp_path / "release").write_text("")
        assert cli.wait(timeout=RUN) == 0, (tmp_path / "cli.log").read_text()
        assert _in_flight(tmp_path) == []
        assert _in_flight(tmp_path, *server) == []
    finally:
        (tmp_path / "release").write_text("")
        if cli is not None:
            _stop(cli)
        _stop_server(serve)


def _post(port: int, path: str, body: dict) -> dict:
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=10) as resp:
        return json.loads(resp.read())


def test_a_server_run_in_flight_and_the_days_activity_read_the_same_from_the_cli(tmp_path):
    """#991: a run the server started, followed by its events; and `--activity`."""
    _project(tmp_path)
    serve = _serve(tmp_path)
    port = serve.port
    server = ["--server", f"http://127.0.0.1:{port}"]
    try:
        # A finished run first, then one that waits in its slow step. The first computed
        # `upstream`, which the second serves from cache: a cached step reports nothing until
        # the run ends, and does not read as executing (#969).
        done = _post(port, "/runs", {"command": "get", "targets": ["upstream"]})["run_id"]
        wait_until(
            lambda: _get(port, f"/runs/{done}")["status"] == "success", "the first run to finish"
        )
        slow = _post(port, "/runs", {"command": "get", "targets": ["slow"]})["run_id"]
        wait_until(lambda: (tmp_path / "cli.started").exists(), "the slow step to start")

        # `upstream` is cached, `slow` is executing.
        def listed():
            runs = _in_flight(tmp_path, *server)
            executing = [n["node_id"] for n in runs[0]["now"]["running"]] if runs else []
            return runs if executing == ["pipeline.py:slow"] else None

        [run] = wait_until(listed, "the run in flight in barca history --server")
        assert run["run_id"] == slow, run
        assert run["now"]["line"] == "executing 1: slow", run
        [route] = _get(port, "/runs?status=running")["runs"]
        assert run["now"] == route["now"]

        # The activity: counted by the server over the window, not over the runs listed.
        out = _history(tmp_path, "--activity", "--json", *server)
        assert out.returncode == 0, out.stderr
        activity = json.loads(out.stdout)["activity"]
        one = _get(port, "/runs?limit=1")
        assert len(one["runs"]) == 1 and one["total"] == 2, one
        assert activity == one["activity"]
        assert len(activity["hours"]) == 24
        total = activity["total"]
        assert (total["runs"], total["ok"], total["in_progress"]) == (2, 1, 1), total
        assert total["by_trigger"]["api"] == 2, total
        assert activity["hours"][-1]["runs"] == 2, "both started in the current hour"
        assert re.fullmatch(
            r"2 runs in the last 24 hours · 1 in progress · median \d+\.\ds · 2 api",
            activity["summary"],
        ), activity["summary"]
        # The hour's median is of its one finished run; nothing went wrong in it.
        assert total["median_seconds"] == _get(port, f"/runs/{done}")["elapsed_seconds"]
        assert total["failure"] is None and activity["excluded"] is None, activity
        # The same database read without the server gives the same counts.
        local = json.loads(_history(tmp_path, "--activity", "--json", "--local").stdout)
        assert local["activity"]["total"] == total
        table = _history(tmp_path, "--activity", "--pretty", *server)
        assert table.stdout.startswith(activity["summary"] + "\nHOUR"), table.stdout

        # Without the schedule's runs: these two. Only the schedule's: none.
        others = ["--triggered-by", "manual,api,client"]
        kept = json.loads(_history(tmp_path, "--activity", "--json", *others, *server).stdout)
        assert kept["activity"]["total"]["runs"] == 2
        assert kept["activity"]["triggered_by"] == ["manual", "api", "client"]
        assert _get(port, "/runs?triggered_by=manual,api,client")["total"] == 2
        scheduled = _get(port, "/runs?triggered_by=schedule")
        assert (scheduled["runs"], scheduled["total"]) == ([], 0), scheduled
        assert scheduled["activity"]["total"]["runs"] == 0
        assert scheduled["activity"]["summary"] == "0 runs in the last 24 hours"
        # Left out, not kept: the same two runs, and what the exclusion left out is said.
        hidden = json.loads(
            _history(
                tmp_path, "--activity", "--not-triggered-by", "schedule", "--json", *server
            ).stdout
        )["activity"]
        assert hidden["total"]["runs"] == 2 and hidden["excluded"]["runs"] == 0, hidden
        assert hidden == _get(port, "/runs?limit=1&not_triggered_by=schedule")["activity"]
        # By status and by text: the run in progress, counted by the server.
        live = json.loads(
            _history(
                tmp_path, "--activity", "--status", "running", "--search", "SLOW", "--json", *server
            ).stdout
        )["activity"]
        assert (live["total"]["runs"], live["statuses"], live["search"]) == (1, ["running"], "SLOW")
        # The window is the activity's own: a filter on when a run started is refused.
        refused = _history(tmp_path, "--activity", "--since", "1h", *server)
        assert refused.returncode == 2, refused.stderr

        # A run in progress leads the list whatever `limit` lists (#941): two runs finish
        # after `slow` started, and a page of one still has it.
        newer = ""
        for _ in range(2):
            newer = _post(port, "/runs", {"command": "get", "targets": ["upstream"]})["run_id"]
            wait_until(
                lambda run=newer: _get(port, f"/runs/{run}")["status"] == "success",
                "a newer run to finish",
            )
        page = _get(port, "/runs?limit=1")
        assert [r["id"] for r in page["runs"]] == [slow], page["runs"]
        assert (page["total"], page["truncated"]) == (4, True), page
        assert [r["id"] for r in _get(port, "/runs?limit=2")["runs"]] == [slow, newer]
        only = _get(port, "/runs?limit=1&status=success")
        assert [r["id"] for r in only["runs"]] == [newer] and only["total"] == 3, only

        (tmp_path / "release").write_text("")
        wait_until(lambda: _get(port, f"/runs/{slow}")["status"] == "success", "the run to finish")
        assert _in_flight(tmp_path, *server) == []
        after = json.loads(_history(tmp_path, "--activity", "--json", *server).stdout)
        assert after["activity"]["total"]["in_progress"] == 0
        assert after["activity"]["total"]["ok"] == 4
    finally:
        (tmp_path / "release").write_text("")
        _stop_server(serve)
