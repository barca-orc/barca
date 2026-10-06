"""One spelling per idea across the CLI's JSON and stderr (#180).

Each test pins one resolved inconsistency: `list` freshness, `stats` id, `history` files, `plan`
reason, the end-of-run and failed-step `--agent` lines, lowercase warnings, and the Python API's
refresh vocabulary.
"""

import json
import os
import subprocess
import warnings
from pathlib import Path

import pytest

import barca
from barca.api import _find_binary

PIPELINE = """
from barca import Manual, Schedule, asset, collect, partitions, sensor, task


@asset()
def src() -> dict:
    return {"n": 1}


@asset(inputs={"s": src})
def total(s: dict) -> dict:
    return {"total": s["n"]}


@asset(partitions={"k": partitions(["a", "b"])})
def per_key(k: str) -> dict:
    return {"k": k}


@asset(inputs={"parts": collect(per_key)})
def gathered(parts: list) -> int:
    return len(parts)


@sensor(freshness=Manual)
def poll() -> str:
    return "etag"


@task(freshness=Schedule("0 6 * * *"))
def nightly() -> None:
    pass
"""

FAILING = """
from barca import asset


@asset()
def ok_first() -> int:
    return 1


@asset(inputs={"x": ok_first})
def broken(x: int) -> int:
    raise ValueError("broken on purpose")
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "failing.py").write_text(FAILING)
    return tmp_path


def barca_cli(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={k: v for k, v in os.environ.items() if k != "BARCA_OUTPUT"},
        capture_output=True,
        text=True,
    )


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout.strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])


def test_list_freshness_is_a_lowercase_string_like_kind(project):
    nodes = {
        n["id"].split(":")[-1]: n
        for n in ok(barca_cli(project, "list", "pipeline.py", "--json"))["nodes"]
    }
    assert nodes["src"]["freshness"] == "always"
    assert nodes["poll"]["freshness"] == "manual"
    assert nodes["nightly"]["freshness"] == "schedule"
    assert nodes["nightly"]["schedule"] == "0 6 * * *"
    assert "next_fire" in nodes["nightly"]
    assert "schedule" not in nodes["src"]
    trimmed = ok(barca_cli(project, "list", "pipeline.py", "--fields", "id,schedule"))
    assert {k for n in trimmed["nodes"] for k in n} <= {"id", "schedule"}


def test_stats_names_the_node_id(project):
    ok(barca_cli(project, "get", "total", "pipeline.py", "--json"))
    stats = ok(barca_cli(project, "stats", "total", "pipeline.py", "--json"))
    assert stats["id"] == "pipeline.py:total"
    assert "node_id" not in stats


def test_history_files_is_an_array(project):
    ok(barca_cli(project, "get", "total", "pipeline.py", "--json"))
    runs = ok(barca_cli(project, "history", "--json"))["runs"]
    assert runs[0]["files"] == ["pipeline.py"]


def test_plan_reason_is_a_structured_object(project):
    plan = ok(barca_cli(project, "plan", "pipeline.py"))
    reasons = [p["reason"] for p in plan["phases"]]
    assert reasons[0] == {"type": "initial"}
    fan_in = [r for r in reasons if r["type"] == "fan_in"]
    assert fan_in and fan_in[0]["node_id"].endswith(":gathered"), reasons
    # `--env` was a no-op on plan and is gone.
    assert barca_cli(project, "plan", "pipeline.py", "--env", "dev").returncode == 2


@pytest.mark.parametrize("agent", [True, False])
def test_end_of_run_line_is_one_format(project, agent):
    args = ["get", "total", "pipeline.py", "--json"] + (["--agent"] if agent else [])
    proc = barca_cli(project, *args)
    assert proc.returncode == 0, proc.stderr
    if agent:
        assert "steps | done in" in proc.stderr, proc.stderr
    assert "steps done in" not in proc.stderr


def test_a_failed_agent_run_names_the_step_and_does_not_say_done(project):
    proc = barca_cli(project, "get", "broken", "failing.py", "--json", "--agent")
    assert proc.returncode == 1
    lines = proc.stderr.splitlines()
    assert any(
        line.startswith("[barca] step:failing.py:broken failed: ") and "broken on purpose" in line
        for line in lines
    ), proc.stderr
    summary = [line for line in lines if " steps | " in line]
    assert summary and "failed in" in summary[-1] and "done" not in summary[-1], summary
    assert "[barca] run failed: step 'failing.py:broken' failed (exit 1)" in proc.stderr


def test_warnings_are_lowercase(project):
    proc = barca_cli(project, "get", "total", "pipeline.py", "--json", "--no-cache")
    assert "[barca] warning: " in proc.stderr
    assert "[barca] Warning" not in proc.stderr


def test_python_api_get_shares_the_refresh_vocabulary(project, monkeypatch):
    monkeypatch.chdir(project)
    assert barca.get("total", "pipeline.py") == {"total": 1}
    assert barca.get("total", "pipeline.py", refresh=["src"]) == {"total": 1}
    assert barca.get("total", "pipeline.py", refresh_all=True) == {"total": 1}
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        assert barca.get("total", "pipeline.py", no_cache=True) == {"total": 1}
    assert any(issubclass(w.category, DeprecationWarning) for w in caught)
    assert barca.stats("total", "pipeline.py")["id"] == "pipeline.py:total"
    assert barca.history(1)[0]["files"] == ["pipeline.py"]
