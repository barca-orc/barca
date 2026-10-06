"""Several targets in one invocation: `barca run a,b pipeline.py` / `barca get a,b pipeline.py`.

The union of the targets' cones is planned once, so shared upstream materializes once. Every
named target runs even if another fails, and the exit code is non-zero if any failed. JSON output
is keyed by target only when more than one target is given; single-target output is unchanged.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset, task


@asset()
def src() -> dict:
    with open("src_runs.log", "a") as f:
        f.write("ran\\n")
    return {"n": 3}


@asset(inputs={"s": src})
def left(s: dict) -> dict:
    return {"left": s["n"]}


@asset(inputs={"s": src})
def right(s: dict) -> dict:
    return {"right": s["n"] * 2}


@asset(inputs={"l": left})
def deeper(l: dict) -> dict:
    return {"deeper": l["left"] + 1}


@task(inputs={"s": src})
def check_a(s: dict) -> dict:
    return {"a_ok": s["n"] == 3}


@task(inputs={"s": src})
def check_b(s: dict) -> dict:
    return {"b_ok": s["n"] > 0}


@task(inputs={"s": src})
def boom(s: dict) -> None:
    raise ValueError("check failed on purpose")


@asset()
def broken() -> dict:
    raise RuntimeError("broken upstream")


@task(inputs={"b": broken})
def needs_broken(b: dict) -> None:
    pass


@task(inputs={"d": deeper})
def deep_check(d: dict) -> dict:
    return {"deep_ok": d["deeper"] == 4}
"""

SINGLE_KEYS = {
    "status",
    "run_id",
    "elapsed_seconds",
    "steps_executed",
    "phases",
    "final_output",
    "steps",
}


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def barca(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=project, env=dict(os.environ), capture_output=True, text=True
    )


def last_json(proc: subprocess.CompletedProcess) -> dict:
    out = proc.stdout.strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])


def ok(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return last_json(proc)


def src_runs(project: Path) -> int:
    log = project / "src_runs.log"
    return len(log.read_text().splitlines()) if log.exists() else 0


def ids(result: dict) -> list[str]:
    return [s["id"].split(":")[-1] for s in result["steps"]]


def test_run_two_tasks_materializes_shared_upstream_once(project):
    out = ok(barca(project, "run", "check_a,check_b", "pipeline.py"))
    assert src_runs(project) == 1
    assert ids(out).count("src") == 1
    assert set(out["targets"]) == {"check_a", "check_b"}
    assert out["targets"]["check_a"] == {"status": "success", "final_output": {"a_ok": True}}
    assert out["targets"]["check_b"] == {"status": "success", "final_output": {"b_ok": True}}
    assert out["steps_executed"] == 3
    assert "final_output" not in out
    assert {"run_id", "elapsed_seconds", "steps_executed", "phases", "steps"} <= set(out)


def test_get_two_assets_then_both_are_cached(project):
    first = ok(barca(project, "get", "left,right", "pipeline.py"))
    assert first["targets"]["left"]["final_output"] == {"left": 3}
    assert first["targets"]["right"]["final_output"] == {"right": 6}
    assert first["steps_executed"] == 3
    second = ok(barca(project, "get", "left,right", "pipeline.py"))
    assert second["steps_executed"] == 0
    assert second["targets"]["right"] == {"status": "success", "final_output": {"right": 6}}
    assert src_runs(project) == 1


def test_a_failing_target_does_not_stop_the_others(project):
    proc = barca(project, "run", "boom,check_a", "pipeline.py")
    assert proc.returncode == 1, proc.stderr
    out = last_json(proc)
    assert out["targets"]["check_a"] == {"status": "success", "final_output": {"a_ok": True}}
    failed = out["targets"]["boom"]
    assert failed["status"] == "failed"
    assert failed["failed_node"] == "pipeline.py:boom"
    assert "check failed on purpose" in failed["error"]
    assert "boom" in proc.stderr
    # The run reports failure on stdout, and the error envelope (#154) is the last stderr line.
    assert out["status"] == "failed"
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "step_failed" and envelope["node"] == "pipeline.py:boom"


def test_all_targets_succeeding_reports_success(project):
    out = ok(barca(project, "run", "check_a,check_b", "pipeline.py"))
    assert out["status"] == "success"


def test_a_failed_upstream_fails_only_the_targets_that_depend_on_it(project):
    proc = barca(project, "run", "needs_broken,deep_check", "pipeline.py")
    assert proc.returncode == 1, proc.stderr
    out = last_json(proc)
    assert out["targets"]["deep_check"] == {
        "status": "success",
        "final_output": {"deep_ok": True},
    }
    failed = out["targets"]["needs_broken"]
    assert failed["status"] == "failed"
    assert failed["failed_node"] == "pipeline.py:broken"
    assert "broken upstream" in failed["error"]
    by = {s["id"].split(":")[-1]: s for s in out["steps"]}
    assert by["broken"]["status"] == "failed"
    assert by["needs_broken"]["status"] == "skipped"
    assert by["needs_broken"]["reason"] == "upstream_failed"
    assert by["deep_check"]["status"] == "ran"
    assert out["steps_executed"] == 5  # the skipped step never executed
    runs = ok(barca(project, "history", "--json"))["runs"]
    assert runs[0]["status"] == "failed"


def test_single_target_output_is_unchanged(project):
    out = ok(barca(project, "run", "check_a", "pipeline.py"))
    assert set(out) == SINGLE_KEYS
    assert out["final_output"] == {"a_ok": True}
    got = ok(barca(project, "get", "left", "pipeline.py"))
    assert set(got) == SINGLE_KEYS


def test_a_repeated_name_is_one_target(project):
    out = ok(barca(project, "get", "left,left", "pipeline.py"))
    assert set(out) == SINGLE_KEYS


def test_dry_run_with_several_targets_reports_the_union_once(project):
    dry = ok(barca(project, "run", "check_a,check_b", "pipeline.py", "--dry-run"))
    assert dry["dry_run"] is True and dry["command"] == "run"
    # Keyed by target like a real run (#180), each with its own predicted summary.
    assert list(dry["targets"]) == ["check_a", "check_b"]
    assert dry["targets"]["check_a"] == {"summary": {"will_run": 2, "cached": 0, "unknown": 0}}
    assert "target" not in dry
    assert sorted(ids(dry)) == ["check_a", "check_b", "src"]
    assert dry["summary"] == {"will_run": 3, "cached": 0, "unknown": 0}
    assert not (project / ".barca").exists()
    real = ok(barca(project, "run", "check_a,check_b", "pipeline.py"))
    assert real["steps_executed"] == 3
    assert list(real["targets"]) == list(dry["targets"])
    warm = ok(barca(project, "run", "check_a,check_b", "pipeline.py", "--dry-run"))
    assert warm["summary"] == {"will_run": 2, "cached": 1, "unknown": 0}
    assert warm["targets"]["check_b"]["summary"] == {"will_run": 1, "cached": 1, "unknown": 0}


def test_dry_run_targets_keep_the_order_given(project):
    dry = ok(barca(project, "get", "right,left", "pipeline.py", "--dry-run"))
    assert list(dry["targets"]) == ["right", "left"]


def test_single_target_dry_run_shape_is_unchanged(project):
    dry = ok(barca(project, "run", "check_a", "pipeline.py", "--dry-run"))
    assert dry["target"] == "check_a"
    assert "targets" not in dry


def test_refresh_applies_to_the_union_of_cones(project):
    ok(barca(project, "run", "check_a,deep_check", "pipeline.py"))
    out = ok(barca(project, "run", "check_a,deep_check", "pipeline.py", "--refresh", "src,left"))
    assert src_runs(project) == 2
    by = {s["id"].split(":")[-1]: s for s in out["steps"]}
    assert by["left"]["status"] == "ran" and by["left"]["reason"] == "refresh"
    bad = barca(project, "run", "check_a,check_b", "pipeline.py", "--refresh", "right")
    assert bad.returncode == 2 and "no upstream asset named 'right'" in bad.stderr


def test_value_output_is_keyed_by_target(project):
    proc = barca(project, "get", "left,right", "pipeline.py", "-o", "value")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout) == {"left": {"left": 3}, "right": {"right": 6}}


def test_every_name_is_checked_before_anything_runs(project):
    unknown = barca(project, "run", "check_a,nope", "pipeline.py")
    assert unknown.returncode == 2 and "nope" in unknown.stderr
    wrong_kind = barca(project, "get", "left,check_a", "pipeline.py")
    assert wrong_kind.returncode == 2 and "barca run" in wrong_kind.stderr
    assert src_runs(project) == 0
    empty = barca(project, "run", "check_a,,check_b", "pipeline.py")
    assert empty.returncode == 2 and "empty target name" in empty.stderr
    assert empty.stdout == ""


def test_history_records_every_target(project):
    ok(barca(project, "run", "check_a,check_b", "pipeline.py"))
    runs = ok(barca(project, "history", "--json"))["runs"]
    assert runs[0]["target"] == "check_a,check_b"
