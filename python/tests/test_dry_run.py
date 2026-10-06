"""`--dry-run` shows what a command would do, and real runs report what each step did.

A dry run executes nothing and changes nothing (no `.barca` directory, no history record). It
must predict reality: the number of steps it says will run equals `steps_executed` of the real
command, across refresh / no-cache / partition scenarios.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
import time
from barca import asset, task


@asset()
def src() -> dict:
    return {"t": time.time()}


@asset(inputs={"s": src})
def mid(s: dict) -> dict:
    return {"t": s["t"]}


@task(inputs={"m": mid})
def report(m: dict) -> dict:
    return {"t": m["t"]}
"""

PARTITIONED = """
import os
from barca import asset, collect, partitions

KEYS = os.environ.get("BARCA_TEST_KEYS", "a,b,c").split(",")


@asset(partitions={"k": partitions([x for x in KEYS])})
def part(k: str) -> dict:
    return {"k": k}


@asset(inputs={"parts": collect(part)})
def summary(parts: list) -> dict:
    return {"keys": sorted(p["k"] for p in parts)}
"""

DYNAMIC = """
from barca import asset, collect, partitions, partitions_from


@asset()
def keys() -> list:
    return ["x", "y"]


@asset(partitions={"k": partitions_from(keys)})
def part(k: str) -> dict:
    return {"k": k}
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "parts.py").write_text(PARTITIONED)
    (tmp_path / "dynamic.py").write_text(DYNAMIC)
    return tmp_path


def barca(project: Path, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={**os.environ, **(env or {})},
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


def steps_by_name(result: dict) -> dict:
    return {s["id"].split(":")[-1]: s for s in result["steps"]}


def will_run(result: dict) -> int:
    return result["summary"]["will_run"]


def test_a_dry_run_on_a_fresh_project_changes_nothing(project):
    result = ok(barca(project, "run", "report", "pipeline.py", "--dry-run"))
    assert result["dry_run"] is True
    by = steps_by_name(result)
    assert by["src"]["action"] == "run" and by["src"]["reason"] == "not_materialized"
    assert by["mid"]["action"] == "run" and by["mid"]["reason"] == "not_materialized"
    assert by["report"]["action"] == "run" and by["report"]["reason"] == "task"
    assert result["summary"] == {"will_run": 3, "cached": 0, "unknown": 0}
    assert not (project / ".barca").exists(), "a dry run must not create .barca"


def test_a_dry_run_after_a_real_run_shows_what_is_cached(project):
    ok(barca(project, "run", "report", "pipeline.py"))
    result = ok(barca(project, "run", "report", "pipeline.py", "--dry-run"))
    by = steps_by_name(result)
    assert by["src"]["action"] == "cached" and by["mid"]["action"] == "cached"
    assert by["src"]["run_hash"] and by["src"]["artifact"]
    assert by["report"]["action"] == "run" and by["report"]["reason"] == "task"
    assert result["summary"] == {"will_run": 1, "cached": 2, "unknown": 0}


def test_a_dry_run_leaves_no_history_record(project):
    ok(barca(project, "run", "report", "pipeline.py"))
    before = ok(barca(project, "history", "--json"))
    barca(project, "run", "report", "pipeline.py", "--dry-run")
    barca(project, "run", "report", "pipeline.py", "--dry-run", "--refresh", "src")
    assert ok(barca(project, "history", "--json")) == before


def test_dry_run_with_refresh_shows_the_cascade(project):
    ok(barca(project, "run", "report", "pipeline.py"))
    result = ok(barca(project, "run", "report", "pipeline.py", "--dry-run", "--refresh", "src"))
    by = steps_by_name(result)
    assert by["src"]["action"] == "run" and by["src"]["reason"] == "refresh"
    assert by["mid"]["action"] == "run" and by["mid"]["reason"] == "refresh_cascade"
    assert "src" in by["mid"]["detail"]
    assert "warning" not in by["mid"]
    assert result["summary"] == {"will_run": 3, "cached": 0, "unknown": 0}


def test_dry_run_with_no_cascade_names_the_reason_and_warns_about_stale_downstream(project):
    ok(barca(project, "run", "report", "pipeline.py"))
    result = ok(
        barca(
            project, "run", "report", "pipeline.py", "--dry-run", "--refresh", "src", "--no-cascade"
        )
    )
    by = steps_by_name(result)
    assert by["src"]["action"] == "run" and by["src"]["reason"] == "refresh"
    assert by["mid"]["action"] == "cached"
    assert "depends on refreshed 'src'" in by["mid"]["warning"]
    assert "--refresh src,mid" in by["mid"]["warning"]


def test_dry_run_with_refresh_all_on_get_and_run(project):
    ok(barca(project, "run", "report", "pipeline.py"))
    refreshed = steps_by_name(
        ok(barca(project, "run", "report", "pipeline.py", "--dry-run", "--refresh-all"))
    )
    assert (
        refreshed["src"]["reason"] == "refresh_all" and refreshed["mid"]["reason"] == "refresh_all"
    )
    # One spelling on both commands (#180).
    forced = steps_by_name(
        ok(barca(project, "get", "mid", "pipeline.py", "--dry-run", "--refresh-all"))
    )
    assert forced["src"]["reason"] == "refresh_all" and forced["mid"]["reason"] == "refresh_all"


@pytest.mark.parametrize("command,target", [("get", "mid"), ("run", "report")])
def test_no_cache_is_a_deprecated_spelling_of_refresh_all(project, command, target):
    ok(barca(project, "run", "report", "pipeline.py"))
    proc = barca(project, command, target, "pipeline.py", "--dry-run", "--no-cache")
    by = steps_by_name(ok(proc))
    assert by["src"]["reason"] == "refresh_all"
    assert "[barca] warning: --no-cache is deprecated" in proc.stderr
    assert "--refresh-all" in proc.stderr
    # Hidden from the --help options list: one canonical spelling is shown.
    options = barca(project, command, "--help").stdout.splitlines()
    assert not [o for o in options if o.startswith("  ") and o.strip().startswith("--no-cache")]


def test_get_takes_refresh_with_cascade_like_run(project):
    ok(barca(project, "get", "mid", "pipeline.py"))
    by = steps_by_name(
        ok(barca(project, "get", "mid", "pipeline.py", "--dry-run", "--refresh", "src"))
    )
    assert by["src"]["reason"] == "refresh" and by["mid"]["reason"] == "refresh_cascade"
    no_cascade = steps_by_name(
        ok(
            barca(
                project,
                "get",
                "mid",
                "pipeline.py",
                "--dry-run",
                "--refresh",
                "src",
                "--no-cascade",
            )
        )
    )
    assert no_cascade["src"]["action"] == "run" and no_cascade["mid"]["action"] == "cached"
    # A get target is an asset, so it may name itself.
    itself = steps_by_name(
        ok(barca(project, "get", "mid", "pipeline.py", "--dry-run", "--refresh", "mid"))
    )
    assert itself["mid"]["reason"] == "refresh" and itself["src"]["action"] == "cached"
    # A real run does what the dry run said.
    real = ok(barca(project, "get", "mid", "pipeline.py", "--refresh", "src"))
    assert real["steps_executed"] == 2


def test_get_rejects_an_unknown_refresh_name(project):
    proc = barca(project, "get", "mid", "pipeline.py", "--refresh", "nope")
    assert proc.returncode == 2 and "no upstream asset named 'nope'" in proc.stderr


def test_dry_run_rejects_an_unknown_refresh_name_like_a_real_run(project):
    proc = barca(project, "run", "report", "pipeline.py", "--dry-run", "--refresh", "nope")
    assert proc.returncode == 2 and "no upstream asset named 'nope'" in proc.stderr


SCENARIOS = {
    "cold": [],
    "warm": [],
    "refresh_src": ["--refresh", "src"],
    "refresh_src_no_cascade": ["--refresh", "src", "--no-cascade"],
    "refresh_mid": ["--refresh", "mid"],
    "refresh_both": ["--refresh", "src,mid"],
    "refresh_all": ["--refresh-all"],
}


@pytest.mark.parametrize("name", list(SCENARIOS))
def test_the_dry_run_prediction_matches_the_real_run(project, name):
    flags = SCENARIOS[name]
    if name != "cold":
        ok(barca(project, "run", "report", "pipeline.py"))
    predicted = will_run(ok(barca(project, "run", "report", "pipeline.py", "--dry-run", *flags)))
    actual = ok(barca(project, "run", "report", "pipeline.py", *flags))["steps_executed"]
    assert predicted == actual, f"{name}: dry run said {predicted}, the real run executed {actual}"


def test_partitions_are_reported_per_key_and_match_reality(project):
    keys = {"BARCA_TEST_KEYS": "a,b,c"}
    ok(barca(project, "get", "summary", "parts.py", env=keys))
    grown = {"BARCA_TEST_KEYS": "a,b,c,d"}
    dry = ok(barca(project, "get", "summary", "parts.py", "--dry-run", env=grown))
    part = steps_by_name(dry)["part"]
    assert part["action"] == "partial"
    assert part["partitions"]["total"] == 4 and part["partitions"]["cached"] == 3
    assert part["partitions"]["will_run"] == 1 and part["partitions"]["will_run_keys"] == ["k=d"]
    real = ok(barca(project, "get", "summary", "parts.py", env=grown))
    assert will_run(dry) == real["steps_executed"]


def test_dynamic_partitions_are_unknown_until_their_source_runs(project):
    cold = ok(barca(project, "get", "part", "dynamic.py", "--dry-run"))
    part = steps_by_name(cold)["part"]
    assert part["action"] == "unknown"
    assert "keys" in part["detail"] and "output" in part["detail"]
    ok(barca(project, "get", "part", "dynamic.py"))
    warm = steps_by_name(ok(barca(project, "get", "part", "dynamic.py", "--dry-run")))
    assert warm["part"]["action"] == "cached" and warm["keys"]["action"] == "cached"


def test_real_runs_report_what_each_step_did(project):
    first = ok(barca(project, "run", "report", "pipeline.py"))
    by = steps_by_name(first)
    assert {s["status"] for s in first["steps"]} == {"ran"}
    assert by["src"]["reason"] == "not_materialized" and by["report"]["reason"] == "task"
    second = ok(barca(project, "run", "report", "pipeline.py"))
    by = steps_by_name(second)
    assert by["src"]["status"] == "cached" and by["mid"]["status"] == "cached"
    assert by["report"]["status"] == "ran"
    cascaded = steps_by_name(ok(barca(project, "run", "report", "pipeline.py", "--refresh", "src")))
    assert cascaded["src"]["status"] == "ran" and cascaded["src"]["reason"] == "refresh"
    assert cascaded["mid"]["status"] == "ran" and cascaded["mid"]["reason"] == "refresh_cascade"
    refreshed = steps_by_name(
        ok(barca(project, "run", "report", "pipeline.py", "--refresh", "src", "--no-cascade"))
    )
    assert refreshed["src"]["status"] == "ran" and refreshed["src"]["reason"] == "refresh"
    assert (
        refreshed["mid"]["status"] == "cached" and "does not reflect" in refreshed["mid"]["warning"]
    )


def test_agent_mode_lists_cached_steps_too(project):
    ok(barca(project, "run", "report", "pipeline.py"))
    proc = barca(project, "run", "report", "pipeline.py", "--agent")
    assert "step:pipeline.py:src cached" in proc.stderr
    assert "step:pipeline.py:mid cached" in proc.stderr


def test_dry_run_pretty_output_is_a_readable_table(project):
    ok(barca(project, "run", "report", "pipeline.py"))
    proc = barca(project, "run", "report", "pipeline.py", "--dry-run", "-o", "pretty")
    assert proc.returncode == 0
    assert "cached" in proc.stdout and "will run" in proc.stdout.lower()
    assert "src" in proc.stdout and "report" in proc.stdout
