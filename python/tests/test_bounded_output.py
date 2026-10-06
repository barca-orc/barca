"""Bounded list output: `--limit`, `--all`, truncation reporting, and `--fields`.

`list` and `history` print at most a default number of items. JSON output is an envelope that
says whether it was truncated (`truncated`, `total`, `hint`); the human table prints a one-line
note on stderr instead. `--fields a,b` keeps only those keys on each item of any JSON output, and
an unknown field is a usage error (exit 2) that lists the valid ones.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

SMALL = """
from barca import asset, task


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"nums": numbers})
def total(nums: list) -> dict:
    return {"total": sum(nums)}


@task(inputs={"t": total})
def report(t: dict) -> dict:
    return t
"""


def many_assets(n: int) -> str:
    lines = ["from barca import asset", ""]
    for i in range(n):
        lines += ["", "@asset()", f"def a{i:03d}() -> int:", f"    return {i}", ""]
    return "\n".join(lines)


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(SMALL)
    (tmp_path / "big.py").write_text(many_assets(120))
    return tmp_path


def barca(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env=os.environ.copy(),
        capture_output=True,
        text=True,
    )


def ok(proc: subprocess.CompletedProcess):
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


# ─── list ─────────────────────────────────────────────────────────────────────


def test_list_json_is_an_envelope_and_small_pipelines_are_not_truncated(project):
    out = ok(barca(project, "list", "pipeline.py", "--json"))
    assert out["total"] == 3 and out["truncated"] is False
    assert "hint" not in out
    assert [n["id"].split(":")[-1] for n in out["nodes"]] == ["numbers", "total", "report"]


def test_list_default_limit_truncates_large_pipelines_and_says_so(project):
    out = ok(barca(project, "list", "big.py", "--json"))
    assert len(out["nodes"]) == 100
    assert out["total"] == 120 and out["truncated"] is True
    assert "--limit" in out["hint"] and "--all" in out["hint"]


def test_list_limit_and_all(project):
    five = ok(barca(project, "list", "big.py", "--json", "--limit", "5"))
    assert len(five["nodes"]) == 5 and five["truncated"] is True and five["total"] == 120
    every = ok(barca(project, "list", "big.py", "--json", "--all"))
    assert len(every["nodes"]) == 120 and every["truncated"] is False


def test_list_table_truncation_note_goes_to_stderr(project):
    # Piped stdout picks JSON (#153); ask for the table.
    proc = barca(project, "list", "big.py", "-l", "3", "--pretty")
    assert proc.returncode == 0, proc.stderr
    rows = proc.stdout.strip().splitlines()
    assert len(rows) == 2 + 3  # header, rule, three rows
    assert "3 of 120" in proc.stderr and "--all" in proc.stderr
    untruncated = barca(project, "list", "pipeline.py", "--pretty")
    assert untruncated.stderr == ""


def test_list_limit_and_all_conflict(project):
    proc = barca(project, "list", "big.py", "--limit", "5", "--all")
    assert proc.returncode == 2


# ─── history ──────────────────────────────────────────────────────────────────


def test_history_json_envelope_reports_truncation(project):
    for _ in range(3):
        ok(barca(project, "get", "total", "pipeline.py"))
    two = ok(barca(project, "history", "--json", "-l", "2"))
    assert len(two["runs"]) == 2 and two["total"] == 3 and two["truncated"] is True
    assert "--all" in two["hint"]
    every = ok(barca(project, "history", "--json", "--all"))
    assert len(every["runs"]) == 3 and every["truncated"] is False and "hint" not in every

    table = barca(project, "history", "-l", "1", "--pretty")
    assert table.returncode == 0
    assert "1 of 3" in table.stderr


def test_history_on_an_empty_project(project):
    out = ok(barca(project, "history", "--json"))
    assert out == {"runs": [], "total": 0, "truncated": False}


# ─── --fields ─────────────────────────────────────────────────────────────────


def test_fields_trims_list_items_and_implies_json(project):
    out = ok(barca(project, "list", "pipeline.py", "--fields", "id,kind"))
    assert out["total"] == 3
    assert all(set(n) == {"id", "kind"} for n in out["nodes"])


def test_fields_trims_history_items(project):
    ok(barca(project, "get", "total", "pipeline.py"))
    out = ok(barca(project, "history", "--fields", "run_id,status"))
    assert out["runs"][0].keys() == {"run_id", "status"}


def test_fields_trims_get_and_run_steps(project):
    out = ok(barca(project, "get", "total", "pipeline.py", "--fields", "id,status"))
    assert out["final_output"] == {"total": 6}  # the envelope is untouched
    assert all(set(s) == {"id", "status"} for s in out["steps"])
    dry = ok(barca(project, "run", "report", "pipeline.py", "--dry-run", "--fields", "id,action"))
    assert all(set(s) == {"id", "action"} for s in dry["steps"])


def test_fields_trims_stats_recent_runs(project):
    ok(barca(project, "get", "total", "pipeline.py"))
    out = ok(barca(project, "stats", "total", "pipeline.py", "--fields", "status"))
    assert out["id"] == "pipeline.py:total"
    assert out["recent_runs"] and all(set(r) == {"status"} for r in out["recent_runs"])


def test_fields_trims_docs_json(project):
    index = ok(barca(project, "docs", "--fields", "name"))
    assert all(set(t) == {"name"} for t in index["topics"])
    one = ok(barca(project, "docs", "types", "--fields", "name,summary"))
    assert set(one) == {"name", "summary"}


def test_unknown_field_is_a_usage_error_listing_valid_fields(project):
    proc = barca(project, "list", "pipeline.py", "--fields", "id,bogus")
    assert proc.returncode == 2
    assert proc.stdout == ""
    assert "bogus" in proc.stderr and "freshness" in proc.stderr and "inputs" in proc.stderr

    proc = barca(project, "get", "total", "pipeline.py", "--fields", "nope")
    assert proc.returncode == 2
    assert "status" in proc.stderr
    assert not (project / ".barca").exists(), "a usage error must not run anything"


def test_fields_with_non_json_output_is_a_usage_error(project):
    proc = barca(project, "get", "total", "pipeline.py", "-o", "pretty", "--fields", "id")
    assert proc.returncode == 2
    assert "--fields" in proc.stderr


# ─── combined with TTY-aware output (#153) ────────────────────────────────────


def test_python_history_reads_the_envelope(project, monkeypatch):
    """barca.history() asks for JSON (#153), which is now the {runs, ...} envelope (#155)."""
    import barca as api

    for _ in range(3):
        ok(barca(project, "get", "total", "pipeline.py"))
    monkeypatch.chdir(project)
    runs = api.history(2)
    assert isinstance(runs, list) and len(runs) == 2
    assert {"run_id", "status", "steps_executed"} <= set(runs[0])


def test_fields_implies_json_on_get(project):
    out = ok(barca(project, "get", "total", "pipeline.py", "--fields", "id"))
    assert all(set(s) == {"id"} for s in out["steps"])


def test_fields_with_pretty_is_a_usage_error(project):
    for args in (
        ("get", "total", "pipeline.py", "--pretty", "--fields", "id"),
        ("list", "pipeline.py", "--pretty", "--fields", "id"),
    ):
        proc = barca(project, *args)
        assert proc.returncode == 2, (args, proc.stderr)
        assert "--fields applies to JSON output" in proc.stderr
