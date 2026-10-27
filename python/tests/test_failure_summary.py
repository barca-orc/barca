"""Failure summaries retain the machine fan-out and count blocked future phases."""

import json
import os
import subprocess

import pytest

from barca.api import _find_binary

PIPELINE = """
from barca import asset, sensor, partitions_from, collect

@sensor()
def source():
    raise ValueError("credentials missing\\ncontact the owner")

@asset(partitions={"k": partitions_from(source)})
def part(k):
    return k

@asset(inputs={"parts": collect(part)})
def combined(parts):
    return len(parts)

@asset()
def independent():
    return 42
"""


@pytest.mark.parametrize(
    "target,count", [("combined", 2), ("combined,independent", 2), ("source", 0)]
)
@pytest.mark.parametrize(
    "mode", [["--json"], ["--json", "--agent"], ["-o", "pretty"], ["-o", "value"]]
)
def test_final_summary_and_complete_blocked_fanout(tmp_path, target, count, mode):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    proc = subprocess.run(
        [_find_binary(), "get", target, "pipeline.py", *mode],
        cwd=tmp_path,
        env={**os.environ, "BARCA_REMOTE": "off"},
        text=True,
        capture_output=True,
        timeout=30,
    )
    assert proc.returncode == 1, proc.stderr
    lines = proc.stderr.splitlines()
    assert lines[-1] == (
        "[barca] run failed: step 'pipeline.py:source' failed: ValueError: credentials missing; "
        f"{count} downstream steps skipped (exit 1)"
    )
    assert sum(line.startswith("[barca] run failed:") for line in lines) == 1
    assert not any("error: target" in line for line in lines)
    assert not any("skipped" in line for line in lines[:-1])
    if "--json" in mode:
        result = json.loads(proc.stdout)
        assert result["status"] == "failed"
        skipped = {s["id"] for s in result["steps"] if s["status"] == "skipped"}
        assert skipped == ({"pipeline.py:part", "pipeline.py:combined"} if count else set())
        if "," in target:
            pointer = result["targets"]["independent"]["final_output"]["_barca_artifact"]
            assert json.loads((tmp_path / pointer["path"]).read_text()) == 42
            failed = result["targets"]["combined"]
        else:
            failed = result
        assert failed["failed_node"] == "pipeline.py:source"
        envelope = json.loads(next(line for line in reversed(lines) if line.startswith("{")))
        assert envelope["kind"] == "step_failed"
        assert "contact the owner" in envelope["error"]
        assert "raise ValueError" in envelope["traceback"]
