"""Debugging failed runs from the CLI: `barca history` filters, the failure on each run, and one
run in detail (`barca history RUN_ID`), on this machine and from a `barca serve` with a file://
store and a laptop checkout pointed at it (`barca docs status`, "Finding and reading a failed
run"; `barca docs server-mode`).
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

from . import served
from .test_server_ledger import SCRUB, WAIT, toml

PIPELINE = """
import os
import signal
from pathlib import Path

from barca import Schedule, asset, task


@asset()
def load() -> list:
    return [1, 2, 0]


@asset(inputs={"rows": load})
def clean(rows: list) -> list:
    return [10 / r for r in rows]


@asset(retries=3, retry_backoff=0)
def flaky() -> int:
    raise RuntimeError("still flaky")


@task(inputs={"rows": load})
def publish(rows: list) -> None:
    raise ValueError(f"publish failed for {len(rows)} rows")


@asset()
def killed() -> int:
    os.kill(os.getpid(), signal.SIGKILL)
    return 1
"""

SCHEDULED = """
from barca import Schedule, asset


@asset(freshness=Schedule("* * * * * *", max_failures=1))
def ticker() -> int:
    raise KeyError("no tick data")
"""


def _env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["BARCA_POOL_SIZE"] = "2"
    return env


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=_env(),
        timeout=WAIT,
    )


def history(cwd: Path, *args: str) -> dict:
    out = barca(cwd, "history", "--json", *args)
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout)


def run_id_of(out: subprocess.CompletedProcess) -> str:
    """The run id a failed `get`/`run --json` reports on stdout."""
    return json.loads(out.stdout)["run_id"]


def detail(cwd: Path, run_id: str, *args: str) -> dict:
    out = barca(cwd, "history", run_id, "--json", *args)
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout)


def failed_step(view: dict) -> dict:
    [step] = [s for s in view["steps"] if s["status"] == "failed"]
    return step


def check_debugging(cwd: Path, runs: dict[str, str], select: str = "") -> None:
    """What every location answers, given the run ids of the failed `clean`, `publish` and
    `flaky` runs and of the successful `load` run."""
    failed = history(cwd, "--status", "failed")
    by_id = {r["run_id"]: r for r in failed["runs"]}
    assert set(by_id) >= {runs["clean"], runs["publish"], runs["flaky"]}, failed
    assert failed["total"] == len(failed["runs"]) and failed["truncated"] is False
    assert all(r["status"] == "failed" for r in failed["runs"])
    assert by_id[runs["clean"]]["failure"] == {
        "node": "pipeline.py:clean",
        "error": "ZeroDivisionError: division by zero",
    }
    assert by_id[runs["publish"]]["failure"] == {
        "node": "pipeline.py:publish",
        "error": "ValueError: publish failed for 3 rows",
    }
    assert by_id[runs["flaky"]]["failure"]["error"] == "RuntimeError: still flaky"
    ok = history(cwd, "--status", "success")
    assert runs["load"] in {r["run_id"] for r in ok["runs"]}
    assert all(r["failure"] is None for r in ok["runs"])

    # --limit applies after the filter; total and truncated count what it keeps.
    one = history(cwd, "--status", "failed", "-l", "1")
    assert len(one["runs"]) == 1 and one["total"] == failed["total"] and one["truncated"]
    assert "hint" in one

    # --target: the run's target or job.
    clean = history(cwd, "--target", "clean")
    assert {r["run_id"] for r in clean["runs"]} == {runs["clean"]}, clean
    assert history(cwd, "--target", "pipeline.py:clean")["runs"] == clean["runs"]
    loads = {r["run_id"] for r in history(cwd, "--target", "load")["runs"]}
    assert runs["load"] in loads and runs["clean"] not in loads
    assert history(cwd, "--target", "nothing")["total"] == 0

    # --since: a duration, a time, a date; AND with the others.
    every = history(cwd)
    assert history(cwd, "--since", "1h")["total"] == every["total"]
    assert history(cwd, "--since", "2999-01-01")["total"] == 0
    assert history(cwd, "--since", "2999-01-01T00:00:00Z")["runs"] == []
    both = history(cwd, "--status", "failed", "--target", "publish", "--since", "1d")
    assert [r["run_id"] for r in both["runs"]] == [runs["publish"]]

    # The table ends a failed run's row with its step and error.
    out = barca(cwd, "history", "--status", "failed", "--pretty")
    assert out.returncode == 0, out.stderr
    rows = {line.split()[0]: line for line in out.stdout.splitlines()[2:]}
    assert rows[runs["clean"]].endswith("  clean: ZeroDivisionError: division by zero"), out.stdout
    assert rows[runs["publish"]].endswith("  publish: ValueError: publish failed for 3 rows")

    # Bad values are usage errors naming the valid ones.
    bad = barca(cwd, "history", "--status", "broke")
    assert bad.returncode == 2 and "interrupted" in bad.stderr, bad.stderr
    bad = barca(cwd, "history", "--triggered-by", "cron")
    assert bad.returncode == 2 and "schedule" in bad.stderr, bad.stderr
    bad = barca(cwd, "history", "--since", "yesterday")
    assert bad.returncode == 2 and "24h" in bad.stderr, bad.stderr

    # One run in detail: a failing asset.
    view = detail(cwd, runs["clean"])
    assert view["run"]["run_id"] == runs["clean"] and view["run"]["status"] == "failed"
    assert view["run"]["failure"]["node"] == "pipeline.py:clean"
    step = failed_step(view)
    assert step["node_id"] == "pipeline.py:clean" and step["partition_key"] is None
    assert step["error_type"] == "ZeroDivisionError"
    assert step["error_message"] == "division by zero"
    assert step["error"].startswith("ZeroDivisionError: division by zero")
    assert "10 / r" in step["error_traceback"], step
    assert step["worker_exit"] is None and step["attempts"] == 1
    assert step["run_hash"] and step["artifact_path"] is None
    hint = f"barca logs {runs['clean']} --step clean --stream stderr{select}"
    assert view["hint"] == hint

    # The hint's command prints the failed step's stderr.
    logs = barca(cwd, *hint.split()[1:])
    assert logs.returncode == 0, logs.stderr
    assert "ZeroDivisionError: division by zero" in logs.stdout

    # A failing task, with its traceback.
    step = failed_step(detail(cwd, runs["publish"]))
    assert step["error_type"] == "ValueError"
    assert "raise ValueError" in step["error_traceback"]

    # Retries: attempts on the step and on the run.
    view = detail(cwd, runs["flaky"])
    assert failed_step(view)["attempts"] == 3
    assert view["run"]["attempts"] == 3

    # A successful run lists the steps it executed, with their artifacts, and has no hint.
    view = detail(cwd, runs["load"])
    [step] = view["steps"]
    assert step["status"] == "success" and step["error"] is None
    assert step["artifact_format"] == "json" and step["artifact_size_bytes"] > 0
    assert step["artifact_path"] and step["run_hash"]
    assert "hint" not in view

    # The human form.
    out = barca(cwd, "history", runs["clean"], "--pretty")
    assert out.returncode == 0, out.stderr
    text = out.stdout
    assert text.startswith(f"run {runs['clean']}  get clean  failed"), text
    assert "STEP" in text and "RUN_HASH" in text and "ARTIFACT" in text
    assert "pipeline.py:clean failed: ZeroDivisionError: division by zero" in text
    assert "  Traceback (most recent call last):" in text
    assert f"hint: {hint}" in out.stderr

    # An unknown run id.
    out = barca(cwd, "history", "000000000000", "--json")
    assert out.returncode == 2, out.stderr
    assert "000000000000" in out.stderr


def test_debugging_failed_runs_on_this_machine(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    runs = {}
    out = barca(tmp_path, "get", "load", "--json")
    assert out.returncode == 0, out.stderr
    runs["load"] = run_id_of(out)
    for name, argv in (
        ("clean", ["get", "clean"]),
        ("publish", ["run", "publish"]),
        ("flaky", ["get", "flaky"]),
    ):
        out = barca(tmp_path, *argv, "--json")
        assert out.returncode == 1, out.stderr
        runs[name] = run_id_of(out)
    check_debugging(tmp_path, runs)
    assert {r["triggered_by"] for r in history(tmp_path)["runs"]} == {"manual"}
    assert history(tmp_path, "--triggered-by", "manual")["total"] == 4
    assert history(tmp_path, "--triggered-by", "schedule,api")["total"] == 0

    # A worker the kernel killed: how it ended.
    out = barca(tmp_path, "get", "killed", "--json")
    assert out.returncode == 1, out.stderr
    step = failed_step(detail(tmp_path, run_id_of(out)))
    assert step["error_type"] == "WorkerError"
    assert step["worker_exit"] == "SIGKILL (possibly the OOM killer)", step
    text = barca(tmp_path, "history", run_id_of(out), "--pretty").stdout
    assert "  worker exit: SIGKILL (possibly the OOM killer)" in text, text


@pytest.fixture()
def team(tmp_path):
    """`barca serve` with a file:// store and a laptop checkout configured to use it."""
    store = tmp_path / "store"
    store.mkdir()
    server_root, laptop = tmp_path / "server", tmp_path / "laptop"
    for root in (server_root, laptop):
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE)
    (server_root / "barca.toml").write_text(toml(store.as_uri(), None))
    serve = served.Serve(server_root, "--no-schedule", env=_env())
    serve.ready()
    url = f"http://127.0.0.1:{serve.port}"
    (laptop / "barca.toml").write_text(toml(store.as_uri(), url))
    yield laptop, serve, url
    serve.stop()


def test_debugging_failed_runs_from_a_server(team):
    laptop, serve, url = team
    runs = {}
    # The laptop computes `load` against the server's ledger and posts it (a client run).
    out = barca(laptop, "get", "load", "--json")
    assert out.returncode == 0, out.stderr
    [posted] = history(laptop)["runs"]
    assert posted["triggered_by"] == "client"
    runs["load"] = posted["run_id"]
    # The failures run on the server.
    for name, argv in (
        ("clean", ["get", "clean"]),
        ("publish", ["run", "publish"]),
        ("flaky", ["get", "flaky"]),
    ):
        out = barca(laptop, *argv, "--server", "--json")
        assert out.returncode == 1, out.stderr
        runs[name] = run_id_of(out)
    check_debugging(laptop, runs)
    assert history(laptop, "--triggered-by", "client")["total"] == 1
    assert history(laptop, "--triggered-by", "api")["total"] == 3
    # The server applies the filters itself.
    body = serve.get("/history?status=failed&target=clean&since=1h")
    assert [r["run_id"] for r in body["runs"]] == [runs["clean"]]
    assert body["runs"][0]["failure"]["node"] == "pipeline.py:clean"
    # The steps the server's run recorded, from the server.
    view = detail(laptop, runs["clean"])
    assert "pipeline.py:clean" in {s["node_id"] for s in view["steps"]}
    assert view["run"]["triggered_by"] == "api"
    # An explicit --server is carried into the hint.
    view = detail(laptop, runs["clean"], "--server", url)
    assert view["hint"].endswith(f"--stream stderr --server {url}"), view

    # A failed run on the laptop is not posted: the server does not have it, and the error
    # names the command that reads it here.
    out = barca(laptop, "get", "clean", "--json")
    assert out.returncode == 1, out.stderr
    here = run_id_of(out)
    assert here not in {r["run_id"] for r in history(laptop)["runs"]}
    out = barca(laptop, "history", here)
    assert out.returncode == 2, out.stderr
    assert f"this run is on this machine: barca history {here} --local" in out.stderr
    view = detail(laptop, here, "--local")
    assert failed_step(view)["error_type"] == "ZeroDivisionError"
    assert view["hint"] == f"barca logs {here} --step clean --stream stderr --local"
    local = history(laptop, "--local", "--status", "failed")
    assert [r["run_id"] for r in local["runs"]] == [here]


def test_a_scheduled_failure_from_a_server(tmp_path):
    (tmp_path / "pipeline.py").write_text(SCHEDULED)
    with served.Serve(tmp_path, env=_env()) as serve:
        url = f"http://127.0.0.1:{serve.port}"
        serve.wait(
            "a failed scheduled run",
            lambda: serve.get("/history?status=failed&triggered_by=schedule")["runs"],
        )
        out = barca(
            tmp_path, "history", "--server", url, "--triggered-by", "schedule", "--json"
        )
        assert out.returncode == 0, out.stderr
        run = json.loads(out.stdout)["runs"][-1]
        assert run["status"] == "failed" and run["schedule_id"] == "pipeline.py:ticker"
        assert run["failure"] == {"node": "pipeline.py:ticker", "error": "KeyError: 'no tick data'"}
        view = detail(tmp_path, run["run_id"], "--server", url)
        assert view["run"]["scheduled_at"] == run["scheduled_at"]
        assert failed_step(view)["error_type"] == "KeyError"
        text = barca(tmp_path, "history", run["run_id"], "--server", url, "--pretty").stdout
        assert "triggered by schedule  tick " in text, text
        assert "schedule pipeline.py:ticker" in text
