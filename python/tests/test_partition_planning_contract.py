"""Partition plans and previews describe work independently of pool chunking (#337)."""

import json
import os
import sqlite3
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary


def invoke(project: Path, pool: int | None, *args: str):
    env = dict(os.environ)
    env.pop("BARCA_POOL_SIZE", None)
    if pool is not None:
        env["BARCA_POOL_SIZE"] = str(pool)
    return subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        env=env,
        capture_output=True,
        text=True,
        timeout=90,
        check=False,
    )


def result(project: Path, pool: int | None, *args: str):
    proc = invoke(project, pool, *args)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def test_cold_warm_reports_and_plan_use_execution_pool(tmp_path):
    keys = [f"k{i:02}" for i in reversed(range(30))]
    source = f"""from barca import asset, collect, partitions
from pathlib import Path
Path("imported").touch()

@asset()
def seed(): return 1

@asset(inputs={{"seed": seed}}, partitions={{"k": partitions({keys!r})}})
def part(k, seed): return {{"k": k}}

@asset(inputs={{"part": part}}, partitions={{"k": partitions({keys!r})}})
def double(k, part): return {{"k": k}}

@asset(inputs={{"parts": collect(part), "doubles": collect(double)}})
def total(parts, doubles): return len(parts) + len(doubles)
"""
    previews = []
    warnings = []
    hashes = []
    for pool in (1, 2, None):
        project = tmp_path / str(pool)
        project.mkdir()
        (project / "barca.toml").write_text("")
        (project / "pipeline.py").write_text(source)
        plan = result(project, pool, "plan", "pipeline.py")
        assert not (project / "imported").exists()
        assert not (project / ".barca").exists()
        streams = max(len(phase["streams"]) for phase in plan["phases"])
        if pool is not None:
            assert streams == pool  # both have enough independent keys to fill the pool
        cold = result(project, pool, "get", "total", "pipeline.py", "--dry-run")
        first = result(project, pool, "get", "total", "pipeline.py")
        assert first["steps_executed"] == 62
        assert first["final_output"] == 60
        expected = [f"k=k{i:02}" for i in range(20)]
        for report in (cold, first):
            by = {step["id"].rsplit(":", 1)[-1]: step for step in report["steps"]}
            for name in ("part", "double"):
                assert by[name]["partitions"]["will_run_keys"] == expected
                assert by[name]["partitions"]["will_run"] == 30
        previews.append([s["partitions"] for s in cold["steps"] if "partitions" in s])
        warnings.append(cold["warnings"])
        assert cold["warnings"] == plan["warnings"] == first["warnings"]
        assert [(w["node"], w["param"]) for w in cold["warnings"]] == sorted(
            (w["node"], w["param"]) for w in cold["warnings"]
        )
        warm = result(project, pool, "get", "total", "pipeline.py", "--dry-run")
        second = result(project, pool, "get", "total", "pipeline.py")
        assert second["steps_executed"] == 0
        assert second["final_output"] == 60
        assert warm["summary"]["will_run"] == 0
        with sqlite3.connect(project / ".barca" / "metadata.db") as db:
            rows = db.execute(
                "SELECT node_id, run_hash FROM materializations ORDER BY node_id"
            ).fetchall()
        db.close()
        assert len(rows) == 62
        hashes.append(rows)
        forced = result(project, pool, "get", "total", "pipeline.py", "--dry-run", "--refresh-all")
        for step in forced["steps"]:
            if "partitions" in step:
                assert step["partitions"]["will_run_keys"] == expected
        files = list((project / ".barca" / "artifacts").glob("*--part_k_*/*"))
        assert len(files) == 30
        for artifact in files:
            artifact.unlink()
        missing = result(project, pool, "get", "part", "pipeline.py", "--dry-run")
        part = next(s for s in missing["steps"] if s["id"].endswith(":part"))
        assert part["partitions"]["will_run_keys"] == expected
        assert part["partitions"]["will_run"] == 30
        rebuilt = result(project, pool, "get", "part", "pipeline.py")
        assert rebuilt["steps_executed"] == 30
        part = next(s for s in rebuilt["steps"] if s["id"].endswith(":part"))
        assert part["partitions"]["will_run_keys"] == expected
    assert previews[0] == previews[1] == previews[2]
    assert warnings[0] == warnings[1] == warnings[2]
    assert hashes[0] == hashes[1] == hashes[2]


@pytest.mark.parametrize("command", ["plan", "get", "run"])
@pytest.mark.parametrize(
    "dimension", ['partitions(["a"])', "partitions([str(i) for i in range(2)])"]
)
def test_mixed_dimensions_fail_before_import_or_run(tmp_path, command, dimension):
    (tmp_path / "barca.toml").write_text("")
    (
        tmp_path / "pipeline.py"
    ).write_text(f"""from barca import asset, task, collect, partitions, partitions_from
from pathlib import Path
Path("imported").touch()

@asset()
def keys(): return ["x"]

@asset(partitions={{"k": partitions_from(keys), "tier": {dimension}}})
def mixed(k, tier): return k + tier

@task(inputs={{"values": collect(mixed)}})
def finish(values): return values
""")
    args = ("finish", "pipeline.py") if command == "run" else ("pipeline.py",)
    proc = invoke(tmp_path, 1, command, *args)
    assert proc.returncode == 2, proc.stderr
    envelope = json.loads(proc.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "usage"
    assert "mixing partitions() and partitions_from()" in envelope["error"]
    assert "partitions([...])" in envelope["remediation"]
    assert not (tmp_path / "imported").exists()
    assert not (tmp_path / ".barca").exists()


@pytest.mark.parametrize("pool", [1, 2, None])
@pytest.mark.parametrize(
    "file", ["pipeline[old].py", "folder[old]/pipeline.py", "folder[old]/pipeline[old].py"]
)
def test_complete_hash_boundary_preserves_bracketed_source_paths(tmp_path, pool, file):
    (tmp_path / "barca.toml").write_text("")
    source = tmp_path / file
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text(
        """from barca import asset, task
from pathlib import Path
Path("imported").touch()
@asset()
def producer() -> int:
    return 41
@asset(inputs={"value": producer})
def answer(value: int) -> int:
    return value + 1
@task(inputs={"value": answer})
def publish(value: int) -> None:
    assert value == 42
"""
    )
    cold = result(tmp_path, pool, "get", "answer", file, "--dry-run")
    assert cold["summary"]["will_run"] == 2
    assert not (tmp_path / "imported").exists()
    first = result(tmp_path, pool, "get", "answer", file)
    assert first["final_output"] == 42
    assert first["steps_executed"] == 2
    warm = result(tmp_path, pool, "get", "answer", file)
    assert warm["final_output"] == 42
    assert warm["steps_executed"] == 0
    assert [(s["id"], s["run_hash"]) for s in first["steps"]] == [
        (s["id"], s["run_hash"]) for s in warm["steps"]
    ]
    assert result(tmp_path, pool, "get", "answer", file, "--dry-run")["summary"] == {
        "cached": 2,
        "unknown": 0,
        "will_run": 0,
    }
    assert result(tmp_path, pool, "run", "publish", file, "--dry-run")["summary"]["unknown"] == 0
    assert result(tmp_path, pool, "run", "publish", file)["status"] == "success"
