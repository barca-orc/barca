"""A run records why it executed what it did (#925): `run.plan_reasons` of
`barca history RUN_ID --json`, by category and subject, with node and step counts.

Each case changes one thing a real run can tell apart and checks the reason the run recorded
for it: never run, a changed helper and the input it changed downstream, new partition keys,
the retry of a failed step, `--refresh-all`, and a run served from cache recording none.
"""

import json
import os
import subprocess

from barca.api import _find_binary

COMMON = """
def bump(x):
    return x + {step}
"""

PIPELINE = """
from barca import asset, partitions
from common import bump


@asset()
def raw() -> dict:
    return {{"n": bump(1)}}


@asset(inputs={{"raw": raw}})
def clean(raw: dict) -> dict:
    return {{"n": raw["n"] + 1}}


KEYS = [f"k{{i}}" for i in range({keys})]


@asset(inputs={{"clean": clean}}, partitions={{"k": partitions(KEYS)}})
def by_key(clean: dict, k: str) -> dict:
    if k == "k{fail}":
        raise ValueError("bad key")
    return {{"n": clean["n"], "k": k}}
"""


def write(path, step=1, keys=4, fail=-1):
    (path / "common.py").write_text(COMMON.format(step=step))
    (path / "pipeline.py").write_text(PIPELINE.format(keys=keys, fail=fail))


def run(path, *args):
    proc = subprocess.run(
        [_find_binary(), "get", "by_key", "pipeline.py", "--json", *args],
        cwd=path,
        capture_output=True,
        text=True,
    )
    history = subprocess.run(
        [_find_binary(), "history", "--json", "--limit", "1"],
        cwd=path,
        capture_output=True,
        text=True,
        check=True,
    )
    run_id = json.loads(history.stdout)["runs"][0]["run_id"]
    detail = subprocess.run(
        [_find_binary(), "history", run_id, "--json"],
        cwd=path,
        capture_output=True,
        text=True,
        check=True,
    )
    return proc, json.loads(detail.stdout)["run"]


def reasons(run):
    """{(category, subject): (nodes, steps)}."""
    return {
        (r["category"], r["subject"]): (r["nodes"], r["steps"])
        for r in run["plan_reasons"]["reasons"]
    }


def test_a_first_run_is_never_run_and_says_how_many(tmp_path):
    write(tmp_path)
    proc, first = run(tmp_path)
    assert proc.returncode == 0, proc.stderr
    assert reasons(first) == {("never_run", None): (3, 6)}
    assert first["plan_reasons"]["nodes"] == 3
    assert first["plan_reasons"]["steps"] == 6
    assert first["plan_reasons"]["omitted"] == 0
    assert first["retries"] == 0
    assert first["plan_reasons"]["reasons"][0]["example"] == "pipeline.py:raw"


def test_a_changed_helper_names_it_and_the_inputs_it_changed(tmp_path):
    write(tmp_path)
    run(tmp_path)
    write(tmp_path, step=2)
    proc, changed = run(tmp_path)
    assert proc.returncode == 0, proc.stderr
    got = reasons(changed)
    assert ("code_changed", "helper common.py:bump") in got
    assert got[("inputs_changed", "input pipeline.py:raw")] == (1, 1)
    assert got[("inputs_changed", "input pipeline.py:clean")] == (1, 4)
    assert changed["plan_reasons"]["steps"] == 6
    # Every executed step is counted under some reason.
    assert changed["plan_reasons"]["steps"] == changed["steps_executed"]


def test_new_keys_are_new_keys_not_never_run(tmp_path):
    write(tmp_path)
    run(tmp_path)
    write(tmp_path, keys=7)
    proc, grown = run(tmp_path)
    assert proc.returncode == 0, proc.stderr
    assert reasons(grown) == {("new_keys", None): (1, 3)}


def test_a_failed_step_retried_unchanged_is_failed(tmp_path):
    write(tmp_path, fail=2)
    proc, _ = run(tmp_path)
    assert proc.returncode == 1
    proc, again = run(tmp_path)
    assert proc.returncode == 1
    assert reasons(again) == {("failed", None): (1, 1)}


def test_refresh_all_and_a_cached_run(tmp_path):
    write(tmp_path)
    run(tmp_path)
    _, cached = run(tmp_path)
    assert cached["steps_executed"] == 0
    assert "plan_reasons" not in cached
    _, refreshed = run(tmp_path, "--refresh-all")
    assert reasons(refreshed) == {("refresh_all", None): (3, 6)}


def test_the_table_prints_the_reasons(tmp_path):
    write(tmp_path)
    run(tmp_path)
    history = subprocess.run(
        [_find_binary(), "history", "--json", "--limit", "1"],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=True,
    )
    run_id = json.loads(history.stdout)["runs"][0]["run_id"]
    table = subprocess.run(
        [_find_binary(), "history", run_id],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=True,
        env={**os.environ, "BARCA_OUTPUT": "pretty"},
    )
    assert "why: 6 steps of 3 nodes were not cache hits" in table.stdout
    assert "never_run" in table.stdout
