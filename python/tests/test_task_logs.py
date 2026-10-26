"""Task stdout/result conformance: specs/interfaces/task-output.md and barca docs logs."""

import json
import os
import re
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary

PIPELINE = """
from barca import asset, task
@asset()
def source():
    print("source line")
    return 3
@task(inputs={"count": source})
def report(count):
    print("loading 3 files")
    print()
    print("tail", end="")
    return {"rows": count}
@task()
def quiet(): return {"quiet": True}
@task()
def broken():
    print("before failure")
    print("partial failure", end="")
    raise ValueError("boom")
"""


@pytest.fixture
def project(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def cli(project, *args, code=0):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env={
            **os.environ,
            "PYTHONPATH": str(Path(__file__).resolve().parents[1]),
            "BARCA_OUTPUT": "human",
        },
        text=True,
        capture_output=True,
        timeout=60,
    )
    assert proc.returncode == code, proc.stderr
    return proc


TS = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z")


def plain(rows):
    """Recorded rows without their arrival times, which every row has."""
    assert all(TS.fullmatch(row["ts"]) for row in rows), rows
    return [{k: v for k, v in row.items() if k != "ts"} for row in rows]


def test_task_result_json_value_and_python_api(project, monkeypatch):
    run = cli(project, "run", "report", "pipeline.py", "--json")
    doc = json.loads(run.stdout)
    assert doc["final_output"] == {"rows": 3}
    assert "loading 3 files" not in run.stderr and "source line" not in run.stderr
    value = cli(project, "run", "report", "pipeline.py", "-o", "value", "--agent")
    assert json.loads(value.stdout) == {"rows": 3}
    assert "[pipeline.py:report] loading 3 files\n" in value.stderr
    assert "[barca] step:pipeline.py:report" in value.stderr
    monkeypatch.chdir(project)
    monkeypatch.setenv("PYTHONPATH", str(Path(__file__).resolve().parents[1]))
    from barca import run as barca_run

    assert barca_run("report", "pipeline.py") == {"rows": 3}


def test_logs_exact_lines_filters_and_source_free_reads(project):
    run = json.loads(cli(project, "run", "report", "pipeline.py", "--json").stdout)
    (project / "pipeline.py").unlink()
    records = json.loads(cli(project, "logs", run["run_id"], "--json").stdout)
    assert [list(row) for row in records] == [["node_id", "stream", "ts", "line"]] * 4
    assert plain(records) == [
        {"node_id": "pipeline.py:source", "stream": "stdout", "line": "source line"},
        {"node_id": "pipeline.py:report", "stream": "stdout", "line": "loading 3 files"},
        {"node_id": "pipeline.py:report", "stream": "stdout", "line": ""},
        {"node_id": "pipeline.py:report", "stream": "stdout", "line": "tail"},
    ]
    assert (
        cli(project, "logs", run["run_id"], "--step", "report", "--pretty").stdout
        == "loading 3 files\n\ntail\n"
    )
    selected = json.loads(
        cli(project, "logs", run["run_id"], "--step", "pipeline.py:source", "--json").stdout
    )
    assert selected == records[:1]
    cli(project, "logs", run["run_id"], "--step", "absent", "--json", code=2)
    cli(project, "logs", "absent-run", "--json", code=2)


def test_failed_and_silent_steps_have_accessible_logs(project):
    failed = cli(project, "run", "broken", "pipeline.py", "--json", code=1)
    doc = json.loads(failed.stdout)
    assert "before failure" not in failed.stderr and "partial failure" not in failed.stderr
    logs = json.loads(cli(project, "logs", doc["run_id"], "--json").stdout)
    stdout = [row["line"] for row in logs if row["stream"] == "stdout"]
    assert stdout == ["before failure", "partial failure"]
    # The failed attempt's traceback is the step's stderr, as Python prints it.
    stderr = [row["line"] for row in logs if row["stream"] == "stderr"]
    assert stderr[0] == "Traceback (most recent call last):"
    assert stderr[-1] == "ValueError: boom"
    assert any('raise ValueError("boom")' in line for line in stderr)
    assert all(row["node_id"] == "pipeline.py:broken" for row in logs)
    quiet = json.loads(cli(project, "run", "quiet", "pipeline.py", "--json").stdout)
    assert (
        json.loads(cli(project, "logs", quiet["run_id"], "--step", "quiet", "--json").stdout) == []
    )
    assert cli(project, "logs", quiet["run_id"], "--pretty").stdout == ""


def test_module_import_output_and_pinned_stdout_handler_follow_current_step(project):
    (project / "pipeline.py").write_text("""
import logging, sys
from barca import asset, task
print("import line")
logger = logging.getLogger("pinned")
logger.propagate = False
logger.addHandler(logging.StreamHandler(sys.stdout))
@asset()
def source(): return 3
@task(inputs={"count": source})
def report(count):
    logger.warning("handler line")
    return count
""")
    run = cli(project, "run", "report", "pipeline.py", "--json")
    assert "import line" not in run.stderr and "handler line" not in run.stderr
    doc = json.loads(run.stdout)
    rows = json.loads(cli(project, "logs", doc["run_id"], "--json").stdout)
    assert plain(rows) == [
        {"node_id": "pipeline.py:source", "stream": "stdout", "line": "import line"},
        {"node_id": "pipeline.py:report", "stream": "stdout", "line": "handler line"},
    ]


def test_environment_and_multi_target_logs(project):
    doc = json.loads(
        cli(project, "run", "report,quiet", "pipeline.py", "--env", "dev", "--json").stdout
    )
    cli(project, "logs", doc["run_id"], "--json", code=2)
    rows = json.loads(cli(project, "logs", doc["run_id"], "--env", "dev", "--json").stdout)
    assert any(row["line"] == "loading 3 files" for row in rows)


def test_ambiguous_function_names_require_exact_identity(project):
    for name in ["left", "right"]:
        (project / f"{name}.py").write_text(
            f'from barca import task\n@task()\ndef work():\n    print("{name}")\n    return "{name}"\n'
        )
    doc = json.loads(
        cli(project, "run", "left.py:work,right.py:work", "left.py", "right.py", "--json").stdout
    )
    failed = cli(project, "logs", doc["run_id"], "--step", "work", "--json", code=2)
    assert "ambiguous" in failed.stderr
    rows = json.loads(
        cli(project, "logs", doc["run_id"], "--step", "left.py:work", "--json").stdout
    )
    assert plain(rows) == [{"node_id": "left.py:work", "stream": "stdout", "line": "left"}]


def test_partition_filters_keep_exact_key_identity(project):
    (project / "pipeline.py").write_text("""
from barca import asset, partitions
@asset(partitions={"k": partitions(["a", "b"])})
def split(k):
    print(k)
    return k
""")
    doc = json.loads(cli(project, "get", "split", "pipeline.py", "--json").stdout)
    rows = json.loads(cli(project, "logs", doc["run_id"], "--step", "split", "--json").stdout)
    assert {row["line"] for row in rows} == {"a", "b"}
    selected = json.loads(
        cli(project, "logs", doc["run_id"], "--step", "pipeline.py:split[k=a]", "--json").stdout
    )
    assert plain(selected) == [
        {"node_id": "pipeline.py:split[k=a]", "stream": "stdout", "line": "a"}
    ]


def test_logs_does_not_need_the_configured_store(project, monkeypatch):
    doc = json.loads(cli(project, "run", "report", "pipeline.py", "--json").stdout)
    monkeypatch.setenv("BARCA_REMOTE_URI", "s3://unreachable-no-driver-is-needed/logs")
    rows = json.loads(cli(project, "logs", doc["run_id"], "--json").stdout)
    assert any(row["line"] == "loading 3 files" for row in rows)


def test_logs_without_a_run_id_reads_the_most_recent_run(project):
    # Nothing recorded yet: a usage error that says what to run first.
    empty = cli(project, "logs", "--pretty", code=2)
    assert "no run is recorded" in empty.stderr.lower()

    first = json.loads(cli(project, "run", "report", "pipeline.py", "--json").stdout)
    second = json.loads(cli(project, "get", "source", "pipeline.py", "--json").stdout)
    latest = cli(project, "logs", "--pretty")
    # stdout is only the recorded lines; stderr names the run it read.
    assert latest.stdout == ""  # `source` was cached: nothing printed
    note = latest.stderr.strip().splitlines()[-1]
    assert note.startswith(f"barca: logs of the most recent run: {second['run_id']} (get source,")
    assert "success" in note

    rerun = json.loads(cli(project, "run", "report", "pipeline.py", "--json").stdout)
    assert rerun["run_id"] != first["run_id"]
    lines = json.loads(cli(project, "logs", "--step", "report", "--json").stdout)
    assert [entry["line"] for entry in lines] == ["loading 3 files", "", "tail"]
    named = cli(project, "logs", "--json").stderr
    assert f"most recent run: {rerun['run_id']} (run report," in named
    # An explicit id is unchanged and prints no note.
    explicit = cli(project, "logs", first["run_id"], "--json")
    assert "most recent run" not in explicit.stderr
