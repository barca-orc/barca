"""A target name selects exactly the node with that name, never a node whose name merely ends with it.

Before this fix, a target matched any node id that ended with the name, and the first match in
topological order won. So `barca run deploy` on a file with only `prod_deploy` ran `prod_deploy`
and exited 0, and `barca get margin_all` could run `dyn_margin_all`.
"""

import json
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary


def _barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args, "--json"] if args[0] != "list" else [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=120,
    )


def _result(r: subprocess.CompletedProcess) -> dict:
    return json.loads(r.stdout.strip().splitlines()[-1])


def _envelope(r: subprocess.CompletedProcess) -> dict:
    return json.loads(r.stderr.strip().splitlines()[-1])


ONLY_PREFIXED = """
from barca import task

@task()
def prod_deploy() -> str:
    return "prod"
"""

BOTH_PREFIXED_FIRST = """
from barca import task

@task()
def prod_deploy() -> str:
    return "prod"

@task()
def deploy() -> str:
    return "plain"
"""

ASSETS_SUFFIX = """
from barca import asset

@asset()
def dyn_margin_all() -> str:
    return "dyn"

@asset()
def margin_all() -> str:
    return "plain"
"""


def test_suffix_only_match_is_not_found(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(ONLY_PREFIXED)
    r = _barca(tmp_path, "run", "deploy", "p.py")
    assert r.returncode == 2, r.stdout + r.stderr
    assert r.stdout.strip() == "" or "prod" not in r.stdout
    env = _envelope(r)
    assert env["kind"] == "usage"
    assert "deploy" in env["error"]


@pytest.mark.parametrize("source", [BOTH_PREFIXED_FIRST])
def test_exact_name_wins_over_suffix_match(tmp_path: Path, source: str) -> None:
    (tmp_path / "p.py").write_text(source)
    r = _barca(tmp_path, "run", "deploy", "p.py")
    assert r.returncode == 0, r.stderr
    assert _result(r)["final_output"] == "plain"
    ran = [s["id"] for s in _result(r)["steps"]]
    assert ran == ["p.py:deploy"]


def test_asset_exact_name_wins(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(ASSETS_SUFFIX)
    r = _barca(tmp_path, "get", "margin_all", "p.py")
    assert r.returncode == 0, r.stderr
    assert _result(r)["final_output"] == "plain"


def test_status_and_stats_use_exact_names(tmp_path: Path) -> None:
    (tmp_path / "p.py").write_text(ASSETS_SUFFIX)
    assert _barca(tmp_path, "get", "margin_all", "p.py").returncode == 0
    status = _barca(tmp_path, "status", "margin_all", "p.py")
    assert status.returncode == 0, status.stderr
    ids = [n["id"] for n in json.loads(status.stdout)["nodes"]]
    assert ids == ["p.py:margin_all"]
    stats = _barca(tmp_path, "stats", "margin_all", "p.py")
    assert stats.returncode == 0, stats.stderr
    assert json.loads(stats.stdout)["id"] == "p.py:margin_all"


def test_same_name_in_two_files_is_ambiguous(tmp_path: Path) -> None:
    (tmp_path / "a.py").write_text(
        "from barca import asset\n\n@asset()\ndef total() -> int:\n    return 1\n"
    )
    (tmp_path / "b.py").write_text(
        "from barca import asset\n\n@asset()\ndef total() -> int:\n    return 2\n"
    )
    r = _barca(tmp_path, "get", "total", "a.py", "b.py")
    assert r.returncode == 2, r.stdout + r.stderr
    env = _envelope(r)
    assert env["kind"] == "usage"
    assert "a.py:total" in env["error"] + env.get("remediation", "")
    assert "b.py:total" in env["error"] + env.get("remediation", "")
    # The full id disambiguates.
    r = _barca(tmp_path, "get", "b.py:total", "a.py", "b.py")
    assert r.returncode == 0, r.stderr
    assert _result(r)["final_output"] == 2


def test_full_id_and_path_suffix_still_work(tmp_path: Path) -> None:
    sub = tmp_path / "sub"
    sub.mkdir()
    (sub / "p.py").write_text(ASSETS_SUFFIX)
    r = _barca(tmp_path, "get", "sub/p.py:margin_all", "sub/p.py")
    assert r.returncode == 0, r.stderr
    assert _result(r)["final_output"] == "plain"
    # `p.py:margin_all` names the same node at a path boundary.
    r = _barca(tmp_path, "get", "p.py:margin_all", "sub/p.py")
    assert r.returncode == 0, r.stderr
    assert _result(r)["final_output"] == "plain"
