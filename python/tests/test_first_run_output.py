"""What a first run prints to a person (#496): no worker import notice without `--agent`,
timestamps marked UTC in the human tables, `barca docs --pretty` accepted, and `barca stats`
naming what its rate measures."""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset


@asset()
def src() -> dict:
    return {"v": 1}
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def barca(project: Path, *args: str, code: int = 0) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    proc = subprocess.run(
        [_find_binary(), *args], cwd=project, env=env, capture_output=True, text=True, timeout=120
    )
    assert proc.returncode == code, (proc.returncode, proc.stdout, proc.stderr)
    return proc


def test_the_import_notice_is_printed_only_under_agent(project):
    quiet = barca(project, "get", "src", "pipeline.py", "--json")
    assert "importing" not in quiet.stderr, quiet.stderr
    agent = barca(project, "get", "src", "pipeline.py", "--json", "--agent", "--refresh-all")
    assert f"[barca] importing {(project / 'pipeline.py').resolve()}" in agent.stderr, agent.stderr


def test_human_tables_mark_their_timestamps_utc(project):
    barca(project, "get", "src", "pipeline.py", "--json")
    status = barca(project, "status", "pipeline.py", "--pretty").stdout
    assert " UTC " in status.splitlines()[1], status
    history = barca(project, "history", "--pretty").stdout
    assert history.splitlines()[2].rstrip().endswith(" UTC"), history
    stats = barca(project, "stats", "src", "pipeline.py", "--pretty").stdout
    assert stats.rstrip().endswith(" UTC"), stats
    # JSON keeps the stored value as it was.
    doc = json.loads(barca(project, "history", "--json").stdout)
    assert "UTC" not in doc["runs"][0]["started_at"], doc


def test_docs_accepts_pretty(project):
    piped = barca(project, "docs", "cache", "--pretty").stdout
    assert piped == barca(project, "docs", "cache").stdout
    assert piped.startswith("#"), piped[:80]
    barca(project, "docs", "--pretty", "--json", code=2)


def test_stats_counts_re_executions_not_cache_hits(project):
    barca(project, "get", "src", "pipeline.py", "--json")
    barca(project, "get", "src", "pipeline.py", "--json")  # a cache hit: nothing executes
    stats = json.loads(barca(project, "stats", "src", "pipeline.py", "--json").stdout)
    assert stats["total_runs"] == 1 and stats["cache_hit_rate"] == 0, stats
    for _ in range(2):  # the same code and inputs, executed again
        barca(project, "get", "src", "pipeline.py", "--json", "--refresh-all")
    stats = json.loads(barca(project, "stats", "src", "pipeline.py", "--json").stdout)
    assert stats["total_runs"] == 3, stats
    assert stats["cache_hit_rate"] == pytest.approx(2 / 3), stats
    text = barca(project, "stats", "src", "pipeline.py", "--pretty").stdout
    assert "Re-executed with unchanged inputs: 66.7%" in text, text
    assert "Cache hit rate" not in text, text
