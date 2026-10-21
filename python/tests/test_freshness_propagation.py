"""Freshness propagation (RFC-0008), end to end.

Under `barca serve` a tick of a scheduled node brings that node and every `Always` node below
it up to date in one run. `Manual` pins itself: it is never recomputed by a tick or by a
request for something below it, which reads its current result. A failed node retries at every
tick, up to `Schedule(..., max_failures=N)`. `barca get` reads a stale `Manual` upstream as it
is, with a note, and refuses (exit 2) when it has never been computed.
"""

import json
import os
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.request
from pathlib import Path

from barca.api import _find_binary

# Settings from the developer's shell that would point the service at a real store.
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

SHARED = """
from pathlib import Path

from barca import asset, sensor, task, Manual, Schedule


@sensor(freshness=Schedule("* * * * * *"))
def inbox() -> tuple[bool, str]:
    return True, Path("inbox.txt").read_text()


@asset(inputs={"files": inbox})
def orders(files: str) -> dict:
    return {"files": files}


@asset(inputs={"orders": orders})
def summary(orders: dict) -> dict:
    return {"summary": orders["files"]}


@task(inputs={"orders": orders})
def send_report(orders: dict) -> None:
    with open("sent.log", "a") as f:
        f.write(orders["files"] + "\\n")
"""

PINNED = """
from pathlib import Path

from barca import asset, sensor, task, Manual, Schedule


@sensor(freshness=Schedule("* * * * * *"))
def prices() -> tuple[bool, str]:
    return True, Path("prices.txt").read_text()


@asset(inputs={"etag": prices}, freshness=Manual)
def baseline(etag: str) -> dict:
    return {"baseline": etag}


@asset(inputs={"baseline": baseline, "etag": prices})
def report(baseline: dict, etag: str) -> dict:
    return {"baseline": baseline["baseline"], "etag": etag}


@task(inputs={"baseline": baseline})
def publish(baseline: dict) -> None:
    with open("published.log", "a") as f:
        f.write(baseline["baseline"] + "\\n")
"""

FLAKY = """
from pathlib import Path

from barca import sensor, task, Schedule


@sensor(freshness=Schedule("* * * * * *", max_failures=2))
def signal() -> tuple[bool, str]:
    return True, Path("signal.txt").read_text()


@task(inputs={"value": signal})
def flaky(value: str) -> None:
    if Path("fail").exists():
        raise RuntimeError("asked to fail")
"""


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}


def _write(root: Path, name: str, value: str) -> None:
    """Replace the file in one step, so a sensor poll never reads it half-written."""
    tmp = root / f"{name}.tmp"
    tmp.write_text(value)
    os.replace(tmp, root / name)


def _rows(root: Path, status: str = "success") -> dict[str, int]:
    """Materializations per node with `status`; empty while the database is not there yet."""
    try:
        db = sqlite3.connect(root / ".barca" / "metadata.db")
        try:
            return {
                node.rsplit(":", 1)[1]: count
                for node, count in db.execute(
                    "select node_id, count(*) from materializations"
                    " where status = ? group by node_id",
                    (status,),
                )
            }
        finally:
            db.close()
    except sqlite3.Error:
        return {}


def _wait(what: str, check, timeout: float = 60):
    deadline = time.monotonic() + timeout
    while True:
        value = check()
        if value:
            return value
        if time.monotonic() > deadline:
            raise AssertionError(f"timed out waiting for {what}")
        time.sleep(0.2)


def _lines(path: Path) -> list[str]:
    return path.read_text().splitlines() if path.exists() else []


class Serve:
    """`barca serve` on a free port in `root`, stopped on exit."""

    def __init__(self, root: Path, *args: str):
        self.root = root
        self.port = _free_port()
        self.proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--port", str(self.port), *args],
            cwd=root,
            env=_env(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.err = ""

    def __enter__(self) -> "Serve":
        return self

    def __exit__(self, *_exc) -> None:
        self.stop()

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
        try:
            _, self.err = self.proc.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            _, self.err = self.proc.communicate()

    def get(self, path: str):
        with urllib.request.urlopen(f"http://127.0.0.1:{self.port}{path}", timeout=5) as r:
            return json.loads(r.read())

    def post(self, path: str, body: dict):
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=5) as r:
            return json.loads(r.read())

    def ready(self) -> None:
        def health():
            try:
                return self.get("/health")
            except OSError:
                return None

        _wait("GET /health", health)

    def schedule(self) -> dict:
        def fetch():
            try:
                return self.get("/schedule")
            except OSError:
                return None

        return {job["id"].rsplit(":", 1)[1]: job for job in _wait("GET /schedule", fetch)}


def _barca(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=root, env=_env(), capture_output=True, text=True
    )


def _settle(root: Path, seconds: float = 3) -> dict[str, int]:
    """Let several ticks pass and return the rows after them."""
    time.sleep(seconds)
    return _rows(root)


def test_a_tick_brings_the_always_nodes_below_a_sensor_up_to_date_in_one_run(tmp_path):
    (tmp_path / "pipeline.py").write_text(SHARED)
    _write(tmp_path, "inbox.txt", "a")
    sent = tmp_path / "sent.log"
    with Serve(tmp_path) as serve:
        # A new task fires at the first tick that reaches it.
        _wait("first value", lambda: _lines(sent) == ["a"])
        rows = _settle(tmp_path)
        # The sensor returns the same value: nothing below it runs again.
        assert rows.get("orders") == 1 and rows.get("summary") == 1, rows
        assert _lines(sent) == ["a"], _lines(sent)

        # A new value: one run computes `orders` once, then both consumers.
        _write(tmp_path, "inbox.txt", "b")
        _wait("second value", lambda: _lines(sent) == ["a", "b"])
        rows = _settle(tmp_path)
        assert rows.get("orders") == 2 and rows.get("summary") == 2, rows
        assert _lines(sent) == ["a", "b"]

        jobs = serve.schedule()
        assert set(jobs) == {"inbox"}, jobs
        assert jobs["inbox"]["failures"] == 0 and jobs["inbox"]["failing"] is False
        run = serve.get(f"/runs/{jobs['inbox']['last_run']}")
        ids = {step["id"].rsplit(":", 1)[1] for step in run["outcome"]["steps"]}
        assert ids == {"inbox", "orders", "summary", "send_report"}, run
        # One run with several targets, recorded as the schedule's tick (#469).
        assert run["triggered_by"] == "schedule", run
        assert run["schedule_id"].endswith(":inbox") and run["scheduled_at"], run
        targets = {t.rsplit(":", 1)[1] for t in run["outcome"]["targets"]}
        assert targets == {"inbox", "orders", "summary", "send_report"}, run
    # Each value was computed once, not once per consumer (#253).
    assert _rows(tmp_path).get("orders") == 2


def test_manual_pins_itself_and_a_refresh_lets_the_next_tick_through(tmp_path):
    (tmp_path / "pipeline.py").write_text(PINNED)
    _write(tmp_path, "prices.txt", "p1")
    published = tmp_path / "published.log"
    with Serve(tmp_path) as serve:
        # `baseline` was never computed: what reads it is blocked, the tick is not an error.
        _wait("a few sensor polls", lambda: _rows(tmp_path).get("prices", 0) >= 2)
        rows = _rows(tmp_path)
        assert "baseline" not in rows and "report" not in rows, rows
        jobs = serve.schedule()
        assert jobs["prices"]["last_status"] == "complete", jobs
        run = serve.get(f"/runs/{jobs['prices']['last_run']}")
        assert run["status"] == "success", run
        blocked = {
            s["id"].rsplit(":", 1)[1]
            for s in run["outcome"]["steps"]
            if s.get("reason") == "blocked"
        }
        assert blocked == {"report", "publish"}, run
        assert {
            name.rsplit(":", 1)[1]
            for name, t in run["outcome"]["targets"].items()
            if t["status"] == "blocked"
        } == {"report", "publish"}, run

        # Computing it by hand lets the next tick reach what reads it.
        done = _barca(tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline")
        assert done.returncode == 0, done.stderr
        _wait("report and publish", lambda: _lines(published) == ["p1"])
        _wait("report", lambda: _rows(tmp_path).get("report") == 1)

        # The sensor changes: `report` is recomputed reading the pinned baseline; `baseline`
        # is not recomputed, and `publish` (whose only input is baseline) does not fire.
        _write(tmp_path, "prices.txt", "p2")
        _wait("report on p2", lambda: _rows(tmp_path).get("report") == 2)
        rows = _settle(tmp_path)
        assert rows.get("baseline") == 1 and rows.get("report") == 2, rows
        assert _lines(published) == ["p1"]
        value = json.loads(
            _barca(tmp_path, "get", "report", "pipeline.py", "-o", "value", "--json").stdout
        )
        assert value["final_output"] == {"baseline": "p1", "etag": "p2"}, value

        # A refresh of the Manual node: the next tick recomputes `report` and fires `publish`.
        done = _barca(tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline")
        assert done.returncode == 0, done.stderr
        _wait("publish after refresh", lambda: _lines(published) == ["p1", "p2"])
        _wait("report after refresh", lambda: _rows(tmp_path).get("report") == 3)
        rows = _settle(tmp_path)
        assert rows.get("baseline") == 2 and rows.get("report") == 3, rows
        assert _lines(published) == ["p1", "p2"]


def test_a_failing_node_retries_each_tick_up_to_max_failures(tmp_path):
    (tmp_path / "pipeline.py").write_text(FLAKY)
    _write(tmp_path, "signal.txt", "s1")
    (tmp_path / "fail").write_text("")
    with Serve(tmp_path) as serve:
        _wait("failing", lambda: serve.schedule()["signal"]["failing"])
        job = serve.schedule()["signal"]
        assert job["failures"] == 2 and job["max_failures"] == 2, job
        # Skipped past the cap: no more attempts while nothing changes.
        _settle(tmp_path)
        assert _rows(tmp_path, "failed").get("flaky") == 2, _rows(tmp_path, "failed")
        polls = _rows(tmp_path).get("signal")
        time.sleep(2)
        assert _rows(tmp_path).get("signal") == polls, "a failing job was still polled"

        # An explicit request that succeeds resets the count; the next tick runs again.
        (tmp_path / "fail").unlink()
        done = _barca(tmp_path, "run", "flaky", "pipeline.py")
        assert done.returncode == 0, done.stderr
        _wait("reset", lambda: not serve.schedule()["signal"]["failing"])
        _wait("ticks again", lambda: _rows(tmp_path).get("signal", 0) > polls)
        assert serve.schedule()["signal"]["failures"] == 0
    assert "failing: 2 consecutive failed ticks" in serve.err, serve.err


def test_a_restart_does_not_refire_a_task_that_is_up_to_date(tmp_path):
    (tmp_path / "pipeline.py").write_text(SHARED)
    _write(tmp_path, "inbox.txt", "a")
    sent = tmp_path / "sent.log"
    with Serve(tmp_path):
        # Recorded, not only run: a stop before the record would cancel the run.
        _wait("first fire", lambda: _rows(tmp_path).get("send_report") == 1)
        assert _lines(sent) == ["a"]
    with Serve(tmp_path):
        _wait("ticks after restart", lambda: _rows(tmp_path).get("inbox", 0) >= 4)
        time.sleep(1)
    assert _lines(sent) == ["a"]


def test_a_watch_edit_to_an_always_node_is_picked_up_at_the_next_tick(tmp_path):
    (tmp_path / "pipeline.py").write_text(SHARED)
    _write(tmp_path, "inbox.txt", "a")
    with Serve(tmp_path, "--watch"):
        _wait("summary", lambda: _rows(tmp_path).get("summary") == 1)
        (tmp_path / "pipeline.py").write_text(
            SHARED.replace('{"summary": orders["files"]}', '{"summary": orders["files"] * 2}')
        )
        _wait("summary after the edit", lambda: _rows(tmp_path).get("summary") == 2)
        assert _rows(tmp_path).get("orders") == 1


def test_post_runs_reads_a_manual_upstream_as_barca_get_does(tmp_path):
    # The run contract (`POST /runs`, what `--server` sends) runs what `barca get` would run
    # here: a stale `Manual` upstream is read as it is, not recomputed.
    (tmp_path / "pipeline.py").write_text(PINNED)
    _write(tmp_path, "prices.txt", "p1")
    assert _barca(tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline").returncode == 0
    _write(tmp_path, "prices.txt", "p2")
    with Serve(tmp_path, "--no-schedule") as serve:
        serve.ready()
        run_id = serve.post(
            "/runs", {"command": "get", "targets": ["report"], "values": True}
        )["run_id"]
        run = _wait(
            "the run",
            lambda: (r := serve.get(f"/runs/{run_id}"))["status"]
            in ("success", "failed", "cancelled", "error")
            and r,
        )
        assert run["status"] == "success", run
        assert run["outcome"]["final_output"] == {"baseline": "p1", "etag": "p2"}, run
        detail = {s["id"].rsplit(":", 1)[1]: s.get("detail") for s in run["outcome"]["steps"]}
        assert (detail["baseline"] or "").startswith("Manual"), run
    assert _rows(tmp_path).get("baseline") == 1


# ─── barca get ────────────────────────────────────────────────────────────────


def test_get_behind_a_never_computed_manual_node_is_a_usage_error(tmp_path):
    (tmp_path / "pipeline.py").write_text(PINNED)
    _write(tmp_path, "prices.txt", "p1")
    done = _barca(tmp_path, "get", "report", "pipeline.py")
    assert done.returncode == 2, done.stderr
    assert "'baseline' is Manual and has never been computed" in done.stderr
    assert "barca get baseline pipeline.py --refresh baseline" in done.stderr
    assert "report" not in _rows(tmp_path)


def test_get_reads_a_stale_manual_upstream_as_it_is_with_a_note(tmp_path):
    (tmp_path / "pipeline.py").write_text(PINNED)
    _write(tmp_path, "prices.txt", "p1")
    assert _barca(tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline").returncode == 0

    _write(tmp_path, "prices.txt", "p2")
    done = _barca(tmp_path, "get", "report", "pipeline.py", "-o", "value", "--json")
    assert done.returncode == 0, done.stderr
    assert json.loads(done.stdout)["final_output"] == {"baseline": "p1", "etag": "p2"}
    assert (
        "[barca] note: baseline is Manual and stale; "
        "barca get baseline pipeline.py --refresh baseline to recompute"
    ) in done.stderr
    steps = {s["id"].rsplit(":", 1)[1]: s for s in json.loads(done.stdout)["steps"]}
    assert steps["baseline"]["status"] == "cached"
    assert steps["baseline"]["detail"].startswith("Manual and stale"), steps
    assert _rows(tmp_path).get("baseline") == 1

    # The dry run predicts the same.
    dry = _barca(tmp_path, "get", "report", "pipeline.py", "--dry-run", "--json")
    assert dry.returncode == 0, dry.stderr
    predicted = {s["id"].rsplit(":", 1)[1]: s for s in json.loads(dry.stdout)["steps"]}
    assert predicted["baseline"]["action"] == "cached", predicted

    # Naming it in --refresh recomputes it, and what reads it.
    done = _barca(
        tmp_path, "get", "report", "pipeline.py", "--refresh", "baseline", "-o", "value", "--json"
    )
    assert done.returncode == 0, done.stderr
    assert json.loads(done.stdout)["final_output"] == {"baseline": "p2", "etag": "p2"}
    assert "is Manual and stale" not in done.stderr
    assert _rows(tmp_path).get("baseline") == 2


def test_get_of_a_fresh_manual_upstream_prints_no_note(tmp_path):
    (tmp_path / "pipeline.py").write_text(PINNED)
    _write(tmp_path, "prices.txt", "p1")
    assert _barca(tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline").returncode == 0
    done = _barca(tmp_path, "get", "report", "pipeline.py")
    assert done.returncode == 0, done.stderr
    assert "Manual and stale" not in done.stderr


def test_get_of_a_manual_target_reads_it_as_it_is_unless_refresh_names_it(tmp_path):
    (tmp_path / "pipeline.py").write_text(PINNED)
    _write(tmp_path, "prices.txt", "p1")
    # Never computed, and not named in --refresh: the same usage error as for an upstream.
    never = _barca(tmp_path, "get", "baseline", "pipeline.py")
    assert never.returncode == 2, never.stderr
    assert "barca get baseline pipeline.py --refresh baseline" in never.stderr
    assert "baseline" not in _rows(tmp_path)

    first = _barca(
        tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline", "-o", "value", "--json"
    )
    assert json.loads(first.stdout)["final_output"] == {"baseline": "p1"}

    # Stale, asked for by name without --refresh: its current result, with the note.
    _write(tmp_path, "prices.txt", "p2")
    done = _barca(tmp_path, "get", "baseline", "pipeline.py", "-o", "value", "--json")
    assert done.returncode == 0, done.stderr
    assert json.loads(done.stdout)["final_output"] == {"baseline": "p1"}
    assert (
        "[barca] note: baseline is Manual and stale; "
        "barca get baseline pipeline.py --refresh baseline to recompute"
    ) in done.stderr
    assert _rows(tmp_path).get("baseline") == 1

    done = _barca(
        tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline", "-o", "value", "--json"
    )
    assert json.loads(done.stdout)["final_output"] == {"baseline": "p2"}
    assert _rows(tmp_path).get("baseline") == 2


def test_run_of_a_manual_task_runs_it(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        PINNED.replace('@task(inputs={"baseline": baseline})', '@task(inputs={"baseline": baseline}, freshness=Manual)')
    )
    _write(tmp_path, "prices.txt", "p1")
    assert _barca(tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline").returncode == 0
    done = _barca(tmp_path, "run", "publish", "pipeline.py")
    assert done.returncode == 0, done.stderr
    assert _lines(tmp_path / "published.log") == ["p1"]
