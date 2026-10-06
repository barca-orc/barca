"""examples/basic_app runs: every node it defines can be materialized (assets, sensors) or run
(tasks) with the current CLI.

The example went stale before (#189): an unsupported `[project]` section in barca.toml, a
sensor consumer unpacking a tuple, a `collect()` consumer reading a dict, and an implicit
"partition inheritance" that `partitions_from` replaces. `wide_asset` (10,000 partitions) is
skipped to keep CI fast; it is the same shape as `fetch_prices` with more keys.
"""

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

EXAMPLE = Path(__file__).resolve().parents[2] / "examples" / "basic_app"
PIPELINE = "example_project/assets.py"
SKIP = {"wide_asset"}


@pytest.fixture(scope="module")
def project(tmp_path_factory) -> Path:
    dest = tmp_path_factory.mktemp("basic_app") / "basic_app"
    shutil.copytree(EXAMPLE, dest, ignore=shutil.ignore_patterns(".barca", "tmp", ".venv"))
    return dest


def barca(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([_find_binary(), *args], cwd=project, capture_output=True, text=True)


def result(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, f"exit {proc.returncode}\n{proc.stderr}"
    assert "SINK FAILED" not in proc.stderr, proc.stderr
    out = proc.stdout.strip()
    try:
        return json.loads(out)  # pretty-printed (list)
    except json.JSONDecodeError:
        return json.loads(out.splitlines()[-1])  # get/run: task prints may precede the JSON


def nodes(project: Path) -> dict[str, str]:
    listing = result(barca(project, "list", PIPELINE, "--json", "--all"))
    return {n["id"].split(":")[-1]: n["kind"] for n in listing["nodes"]}


def test_list_discovers_every_node(project):
    found = nodes(project)
    assert len(found) == 18
    assert found["normalised_price"] == "asset"
    assert found["heartbeat_sensor"] == "sensor"
    assert found["notify"] == "task"


def test_every_node_except_wide_asset_runs(project):
    for name, kind in sorted(nodes(project).items()):
        if name in SKIP:
            continue
        command = "run" if kind == "task" else "get"
        out = result(barca(project, command, name, PIPELINE, "--json"))
        assert out["status"] == "success", (name, out)


def test_partitioned_workflow_outputs(project):
    summary = result(barca(project, "get", "price_summary", PIPELINE, "--json"))
    assert summary["final_output"] == {"tickers": ["AAPL", "GOOG", "MSFT"], "total": 1200}
    result(barca(project, "get", "normalised_price", PIPELINE, "--json"))
    arts = project / ".barca" / "artifacts"
    for ticker in ("AAPL", "MSFT", "GOOG"):
        (art,) = (arts / f"example_project__assets.py--normalised_price_ticker_{ticker}").glob(
            "*.json"
        )
        assert json.loads(art.read_text()) == {"ticker": ticker, "normalized": 4.0}

    result(barca(project, "get", "greeting_for_world", PIPELINE, "--json", "--refresh-all"))
    assert (project / "tmp" / "greeting.json").exists()
    assert (project / "tmp" / "greeting.pkl").exists()


def test_sensor_consumer_reads_the_output(project):
    out = result(barca(project, "get", "last_heartbeat_seen", PIPELINE, "--json"))
    assert out["final_output"]["healthy"] is True
    assert isinstance(out["final_output"]["last_ts"], float)
