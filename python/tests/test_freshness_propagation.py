"""Freshness propagation (RFC-0008), end to end.

Under `barca serve` a tick of a scheduled node brings that node and every `Always` node below
it up to date in one run. `Manual` pins itself: it is never recomputed by a tick or by a
request for something below it, which reads its current result. A failed node retries at every
tick, up to `Schedule(..., max_failures=N)`. `barca get` reads a stale `Manual` upstream as it
is, with a note, and refuses (exit 2) when it has never been computed.
"""

import json
import os
import subprocess
import time
from pathlib import Path

from barca.api import _find_binary

from . import served
from .served import rows as _rows

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


def _env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}


def _write(root: Path, name: str, value: str) -> None:
    """Replace the file in one step, so a sensor poll never reads it half-written."""
    tmp = root / f"{name}.tmp"
    tmp.write_text(value)
    os.replace(tmp, root / name)


def _lines(path: Path) -> list[str]:
    return path.read_text().splitlines() if path.exists() else []


def Serve(root: Path, *args: str) -> served.Serve:
    """`barca serve` on a free port in `root`. While it runs, a test reads what it recorded
    through its HTTP API, never from the database file (#537)."""
    return served.Serve(root, *args, env=_env())


def _barca(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=root, env=_env(), capture_output=True, text=True
    )


def test_a_tick_brings_the_always_nodes_below_a_sensor_up_to_date_in_one_run(tmp_path):
    (tmp_path / "pipeline.py").write_text(SHARED)
    _write(tmp_path, "inbox.txt", "a")
    sent = tmp_path / "sent.log"
    with Serve(tmp_path) as serve:
        # A new task fires at the first tick that reaches it.
        serve.wait("first value", lambda: _lines(sent) == ["a"])
        rows = serve.settle()
        # The sensor returns the same value: nothing below it runs again.
        assert rows.get("orders") == 1 and rows.get("summary") == 1, rows
        assert _lines(sent) == ["a"], _lines(sent)

        # A new value: one run computes `orders` once, then both consumers.
        _write(tmp_path, "inbox.txt", "b")
        serve.wait("second value", lambda: _lines(sent) == ["a", "b"])
        rows = serve.settle()
        assert rows.get("orders") == 2 and rows.get("summary") == 2, rows
        assert _lines(sent) == ["a", "b"]

        jobs = serve.schedule()
        assert set(jobs) == {"inbox"}, jobs
        assert jobs["inbox"]["failures"] == 0 and jobs["inbox"]["failing"] is False
        # The last tick's run may still be going (one starts every second).
        run = serve.finished(jobs["inbox"]["last_run"])
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
        serve.wait("a few sensor polls", lambda: serve.count().get("prices", 0) >= 2)
        rows = serve.count()
        assert "baseline" not in rows and "report" not in rows, rows
        # A tick starts every second: read one that has ended.
        jobs = serve.wait(
            "a tick that has ended",
            lambda: (j := serve.schedule())["prices"]["last_status"] not in served.LIVE and j,
        )
        assert jobs["prices"]["last_status"] == "complete", jobs
        run = serve.finished(jobs["prices"]["last_run"])
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
        serve.wait("report and publish", lambda: _lines(published) == ["p1"])
        serve.wait("report", lambda: serve.count().get("report") == 1)

        # The sensor changes: `report` is recomputed reading the pinned baseline; `baseline`
        # is not recomputed, and `publish` (whose only input is baseline) does not fire.
        _write(tmp_path, "prices.txt", "p2")
        serve.wait("report on p2", lambda: serve.count().get("report") == 2)
        rows = serve.settle()
        assert rows.get("baseline") == 1 and rows.get("report") == 2, rows
        assert _lines(published) == ["p1"]
        value = json.loads(
            _barca(tmp_path, "get", "report", "pipeline.py", "-o", "value", "--json").stdout
        )
        assert value["final_output"] == {"baseline": "p1", "etag": "p2"}, value

        # A refresh of the Manual node: the next tick recomputes `report` and fires `publish`.
        done = _barca(tmp_path, "get", "baseline", "pipeline.py", "--refresh", "baseline")
        assert done.returncode == 0, done.stderr
        serve.wait("publish after refresh", lambda: _lines(published) == ["p1", "p2"])
        serve.wait("report after refresh", lambda: serve.count().get("report") == 3)
        rows = serve.settle()
        assert rows.get("baseline") == 2 and rows.get("report") == 3, rows
        assert _lines(published) == ["p1", "p2"]


def test_a_failing_node_retries_each_tick_up_to_max_failures(tmp_path):
    (tmp_path / "pipeline.py").write_text(FLAKY)
    _write(tmp_path, "signal.txt", "s1")
    (tmp_path / "fail").write_text("")
    with Serve(tmp_path) as serve:
        serve.wait("failing", lambda: serve.schedule()["signal"]["failing"])
        job = serve.schedule()["signal"]
        assert job["failures"] == 2 and job["max_failures"] == 2, job
        # Skipped past the cap: no more attempts while nothing changes.
        serve.settle()
        assert serve.count("failed").get("flaky") == 2, serve.count("failed")
        polls = serve.count().get("signal")
        time.sleep(2)
        assert serve.count().get("signal") == polls, "a failing job was still polled"

        # An explicit request that succeeds resets the count; the next tick runs again.
        (tmp_path / "fail").unlink()
        done = _barca(tmp_path, "run", "flaky", "pipeline.py")
        assert done.returncode == 0, done.stderr
        serve.wait("reset", lambda: not serve.schedule()["signal"]["failing"])
        serve.wait("ticks again", lambda: serve.count().get("signal", 0) > polls)
        assert serve.schedule()["signal"]["failures"] == 0
    assert "failing: 2 consecutive failed ticks" in serve.err, serve.err


def test_a_restart_does_not_refire_a_task_that_is_up_to_date(tmp_path):
    (tmp_path / "pipeline.py").write_text(SHARED)
    _write(tmp_path, "inbox.txt", "a")
    sent = tmp_path / "sent.log"
    with Serve(tmp_path) as serve:
        # Recorded, not only run: a stop before the record would cancel the run.
        serve.wait("first fire", lambda: serve.count().get("send_report") == 1)
        assert _lines(sent) == ["a"]
    with Serve(tmp_path) as serve:
        serve.wait("ticks after restart", lambda: serve.count().get("inbox", 0) >= 4)
        time.sleep(1)
    assert _lines(sent) == ["a"]


def test_a_watch_edit_to_an_always_node_is_picked_up_at_the_next_tick(tmp_path):
    (tmp_path / "pipeline.py").write_text(SHARED)
    _write(tmp_path, "inbox.txt", "a")
    with Serve(tmp_path, "--watch") as serve:
        serve.wait("summary", lambda: serve.count().get("summary") == 1)
        (tmp_path / "pipeline.py").write_text(
            SHARED.replace('{"summary": orders["files"]}', '{"summary": orders["files"] * 2}')
        )
        serve.wait("summary after the edit", lambda: serve.count().get("summary") == 2)
        assert serve.count().get("orders") == 1


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
        run = serve.finished(run_id)
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


# ─── A refreshed Manual node reaches what reads it (#548) ────────────────────

# A `Manual` reference table that reads external data: its code and inputs never change, so a
# refresh gives the same run hash and (when the data changed) new bytes.
REFERENCE = """
from pathlib import Path

from barca import asset, partitions, partitions_from, sensor, Manual, Schedule


@asset(freshness=Manual)
def fx() -> float:
    return float(Path("ref.txt").read_text())


@asset(inputs={"rate": fx})
def price_eur(rate: float) -> float:
    return round(100 * rate, 2)


@asset(inputs={"rate": fx}, partitions={"k": partitions(["a", "b"])})
def keyed(k: str, rate: float) -> str:
    return f"{k}:{rate}"


@asset(partitions={"r": partitions(["a", "b"])}, freshness=Manual)
def table(r: str) -> str:
    return Path(f"{r}.txt").read_text()


@asset(partitions={"r": partitions_from(table)})
def mapped(r: str, table: str) -> str:
    return table.upper()


@sensor(freshness=Schedule("* * * * * *"))
def clock() -> tuple[bool, str]:
    return True, Path("clock.txt").read_text()


@asset(inputs={"rate": fx, "tick": clock})
def priced(rate: float, tick: str) -> float:
    return round(100 * rate, 2)
"""


def _value(root: Path, *args: str):
    done = _barca(root, "get", *args, "-o", "value", "--json")
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout)["final_output"]


def _status(root: Path) -> dict[str, dict]:
    done = _barca(root, "status", "pipeline.py", "--json")
    assert done.returncode == 0, done.stderr
    return {n["name"]: n["cache"] for n in json.loads(done.stdout)["nodes"]}


def _dry_run(root: Path, target: str) -> dict[str, str]:
    done = _barca(root, "get", target, "pipeline.py", "--dry-run", "--json")
    assert done.returncode == 0, done.stderr
    return {s["id"].rsplit(":", 1)[1]: s["action"] for s in json.loads(done.stdout)["steps"]}


def _reference(root: Path) -> None:
    (root / "pipeline.py").write_text(REFERENCE)
    _write(root, "ref.txt", "1.08")
    _write(root, "clock.txt", "c1")
    for key in ("a", "b"):
        _write(root, f"{key}.txt", f"{key}1")


def test_a_refreshed_manual_node_reaches_what_reads_it(tmp_path):
    _reference(tmp_path)
    assert _value(tmp_path, "fx", "pipeline.py", "--refresh", "fx") == 1.08
    assert _value(tmp_path, "price_eur", "pipeline.py") == 108.0
    assert _status(tmp_path)["price_eur"]["state"] == "cached"

    # The reference data changes and the Manual node is refreshed by hand, alone.
    _write(tmp_path, "ref.txt", "1.20")
    assert _value(tmp_path, "fx", "pipeline.py", "--refresh", "fx") == 1.2
    assert _rows(tmp_path).get("fx") == 2

    # What reads it is stale relative to the new result: status and the dry run say so.
    cache = _status(tmp_path)["price_eur"]
    assert cache["state"] == "stale", cache
    assert _dry_run(tmp_path, "price_eur") == {"fx": "cached", "price_eur": "run"}

    # The next get recomputes it from the new result; fx itself is read as it is.
    assert _value(tmp_path, "price_eur", "pipeline.py") == 120.0
    rows = _rows(tmp_path)
    assert rows.get("fx") == 2 and rows.get("price_eur") == 2, rows
    assert _status(tmp_path)["price_eur"]["state"] == "cached"

    # A refresh that gives the same bytes changes nothing below it.
    assert _value(tmp_path, "fx", "pipeline.py", "--refresh", "fx") == 1.2
    assert _dry_run(tmp_path, "price_eur") == {"fx": "cached", "price_eur": "cached"}
    assert _value(tmp_path, "price_eur", "pipeline.py") == 120.0
    assert _rows(tmp_path).get("price_eur") == 2


def test_a_refresh_with_the_cascade_leaves_what_reads_it_up_to_date(tmp_path):
    # `--refresh fx` with a target below it recomputes both in one run; the next get of the
    # target serves it from cache (its run hash took fx's new result into account).
    _reference(tmp_path)
    assert _value(tmp_path, "price_eur", "pipeline.py", "--refresh", "fx") == 108.0
    _write(tmp_path, "ref.txt", "1.20")
    assert _value(tmp_path, "price_eur", "pipeline.py", "--refresh", "fx") == 120.0
    assert _rows(tmp_path).get("price_eur") == 2
    assert _dry_run(tmp_path, "price_eur") == {"fx": "cached", "price_eur": "cached"}
    assert _value(tmp_path, "price_eur", "pipeline.py") == 120.0
    assert _rows(tmp_path).get("price_eur") == 2


def test_a_refreshed_manual_node_reaches_selected_partition_keys(tmp_path):
    _reference(tmp_path)
    assert _value(tmp_path, "fx", "pipeline.py", "--refresh", "fx") == 1.08
    assert _value(tmp_path, "keyed", "pipeline.py", "--partition", "k=a") == {"k=a": "a:1.08"}
    _write(tmp_path, "ref.txt", "1.20")
    assert _value(tmp_path, "fx", "pipeline.py", "--refresh", "fx") == 1.2
    assert _value(tmp_path, "keyed", "pipeline.py", "--partition", "k=a") == {"k=a": "a:1.2"}


def test_a_refreshed_key_of_a_partitioned_manual_node_reaches_what_reads_it(tmp_path):
    _reference(tmp_path)
    assert _value(tmp_path, "mapped", "pipeline.py") == {"r=a": "A1", "r=b": "B1"}
    _write(tmp_path, "a.txt", "a2")
    _value(tmp_path, "table", "pipeline.py", "--partition", "r=a", "--refresh", "table")
    assert _dry_run(tmp_path, "mapped")["mapped"] == "partial"
    assert _value(tmp_path, "mapped", "pipeline.py") == {"r=a": "A2", "r=b": "B1"}
    assert _rows(tmp_path).get("mapped") == 3


def test_a_tick_below_a_refreshed_manual_node_reads_its_new_result(tmp_path):
    _reference(tmp_path)
    assert _value(tmp_path, "fx", "pipeline.py", "--refresh", "fx") == 1.08
    with Serve(tmp_path) as serve:
        serve.wait("priced", lambda: serve.count().get("priced") == 1)
        rows = serve.settle()
        assert rows.get("priced") == 1 and rows.get("fx") == 1, rows

        # Refreshed by hand while the service runs: the next tick recomputes `priced`.
        _write(tmp_path, "ref.txt", "1.20")
        done = _barca(tmp_path, "get", "fx", "pipeline.py", "--refresh", "fx")
        assert done.returncode == 0, done.stderr
        serve.wait("priced after refresh", lambda: serve.count().get("priced") == 2)
        rows = serve.settle()
        # Once: the ticks after it serve `priced` from cache, and never recompute fx.
        assert rows.get("priced") == 2 and rows.get("fx") == 2, rows
    assert _value(tmp_path, "priced", "pipeline.py") == 120.0
