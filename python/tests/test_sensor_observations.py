"""Sensor change signals and optional values have independent meanings.

Contract: specs/behavior/freshness.md (INV-OBS-01, INV-OBS-02, INV-OBS-03).
"""

import json
import os

import pytest

from .helpers import QUICK, barca


@pytest.mark.parametrize("observation", ["changed", "(changed, None)"])
def test_signal_without_data_keeps_asset_chain_fresh(tmp_path, observation):
    (tmp_path / "pipeline.py").write_text(
        f"""from pathlib import Path
from barca import sensor, asset

@sensor()
def source():
    changed = Path("changed").read_text() == "yes"
    return {observation}

@asset(inputs={{"_source": source}})
def bronze(_source):
    return int(Path("value").read_text())

@asset(inputs={{"bronze": bronze}})
def silver(bronze):
    return bronze * 2
"""
    )
    env = os.environ.copy()
    for name in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(name, None)

    def get(changed, value):
        (tmp_path / "changed").write_text("yes" if changed else "no")
        (tmp_path / "value").write_text(str(value))
        result = barca(
            tmp_path,
            "get",
            "silver",
            "pipeline.py",
            "--json",
            "-o",
            "value",
            env=env,
            timeout=QUICK,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        return json.loads(result.stdout)

    assert get(True, 1)["final_output"] == 2
    unchanged = get(False, 2)
    assert unchanged["final_output"] == 2
    assert unchanged["steps_executed"] == 1
    assert get(True, 2)["final_output"] == 4
    assert get(True, 3)["final_output"] == 6


def test_unchanged_signal_preserves_data_for_a_new_consumer(tmp_path):
    pipeline = """from pathlib import Path
from barca import sensor, asset

@sensor()
def source():
    if Path("changed").read_text() == "yes":
        return True, {"value": 7}
    return False

@asset(inputs={"source": source})
def report(source):
    return source["value"] * FACTOR
"""
    env = {**os.environ, "BARCA_OUTPUT": "value"}
    for name in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV"):
        env.pop(name, None)

    def get(changed, factor):
        (tmp_path / "pipeline.py").write_text(pipeline.replace("FACTOR", str(factor)))
        (tmp_path / "changed").write_text("yes" if changed else "no")
        result = barca(
            tmp_path,
            "get",
            "report",
            "pipeline.py",
            "--json",
            "-o",
            "value",
            env=env,
            timeout=QUICK,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        return json.loads(result.stdout)

    assert get(True, 2)["final_output"] == 14
    assert get(False, 3)["final_output"] == 21
    assert get(False, 4)["final_output"] == 28


@pytest.mark.parametrize("kind", ["asset", "task"])
def test_explicit_change_refreshes_even_with_the_same_payload(tmp_path, kind):
    (tmp_path / "pipeline.py").write_text(
        """from pathlib import Path
from barca import sensor, asset, task

@sensor()
def source():
    return True, "same payload"

@asset(inputs={"source": source})
def report(source):
    return int(Path("value").read_text())
""".replace("@asset(inputs=", f"@{kind}(inputs=")
    )
    env = os.environ.copy()
    for name in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(name, None)
    for value in (1, 2, 3):
        (tmp_path / "value").write_text(str(value))
        result = barca(
            tmp_path,
            "get" if kind == "asset" else "run",
            "report",
            "pipeline.py",
            "--json",
            *(["-o", "value"] if kind == "asset" else []),
            env=env,
            timeout=QUICK,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert json.loads(result.stdout)["final_output"] == value


@pytest.mark.parametrize(
    "observation, warned",
    [
        ("(True, 'etag')", True),
        ("(True, None)", False),
        ("'etag'", False),
        ("(False, 'etag')", False),
    ],
)
def test_explicit_changed_value_warning(tmp_path, observation, warned):
    (tmp_path / "pipeline.py").write_text(
        f"from barca import sensor\n@sensor()\ndef source(): return {observation}\n"
    )
    result = barca(tmp_path, "get", "pipeline.py", "--json")
    assert result.returncode == 0, result.stderr
    assert ("always counts as changed" in result.stderr) is warned
    if warned:
        assert "pipeline.py:source" in result.stderr
        assert "value alone" in result.stderr


def test_serve_warns_once_per_node_across_polls(serve, tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import sensor\n"
        "@sensor()\ndef first(): return True, 'etag'\n"
        "@sensor()\ndef second(): return True, 'etag'\n"
    )
    server = serve("--no-schedule")
    for node in ("first", "first", "second", "second"):
        run_id = server.submit(node, command="get")["run_id"]
        server.wait(
            "sensor poll", lambda: server.run(run_id)["status"] not in ("pending", "running")
        )
        assert server.run(run_id)["status"] == "success"
    assert server.stderr.count("always counts as changed") == 2
    assert server.stderr.count("sensor pipeline.py:first returned") == 1
    assert server.stderr.count("sensor pipeline.py:second returned") == 1
