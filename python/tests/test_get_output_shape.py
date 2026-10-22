"""`barca get` returns artifact paths by default and every key of a partitioned target (#404,
#287): `-o path|value`, `--partition`, and `barca.get()` still returning values."""

import json
import os
import pty
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary

PIPELINE = """
from barca import asset, partitions


@asset()
def total() -> dict:
    return {"total": 6}


@asset()
def big() -> list:
    return list(range(100))


@asset(partitions={"region": partitions(["emea", "amer", "apac"])})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 100}


@asset(partitions={"region": partitions(["emea", "amer"]), "day": partitions(["d1", "d2"])})
def daily(region: str, day: str) -> dict:
    return {"region": region, "day": day}


@asset(partitions={"k": partitions(["ok1", "bad", "ok2"])})
def flaky(k: str) -> dict:
    if k == "bad":
        raise ValueError("bad key")
    return {"k": k}
"""


def cli(project: Path, *args: str) -> subprocess.CompletedProcess:
    env = {**os.environ, "BARCA_REMOTE": "off"}
    env.pop("BARCA_OUTPUT", None)
    return subprocess.run(
        [_find_binary(), *args], check=False, cwd=project, capture_output=True, text=True, env=env
    )


def doc(proc: subprocess.CompletedProcess, code: int = 0) -> dict:
    assert proc.returncode == code, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def read(project: Path, pointer: dict):
    meta = pointer["_barca_artifact"]
    return json.loads((project / meta["path"]).read_text())


@pytest.fixture
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    return tmp_path


def test_json_final_output_is_the_pointer_and_o_value_the_value(project):
    out = doc(cli(project, "get", "total", "pipeline.py"))  # piped: JSON
    meta = out["final_output"]["_barca_artifact"]
    assert set(meta) == {"path", "format", "size_bytes"} and meta["format"] == "json"
    assert read(project, out["final_output"]) == {"total": 6}
    spelled = doc(cli(project, "get", "total", "pipeline.py", "-o", "path", "--json"))
    assert spelled["final_output"] == out["final_output"]  # -o path is the default spelled out
    valued = doc(cli(project, "get", "total", "pipeline.py", "-o", "value", "--json"))
    assert valued["final_output"] == {"total": 6}


def test_o_path_prints_only_the_path_and_o_value_only_the_value(project):
    path = cli(project, "get", "total", "pipeline.py", "-o", "path")
    assert path.returncode == 0, path.stderr
    lines = path.stdout.splitlines()
    assert len(lines) == 1 and json.loads((project / lines[0]).read_text()) == {"total": 6}
    value = cli(project, "get", "total", "pipeline.py", "-o", "value")
    assert json.loads(value.stdout) == {"total": 6}
    both = cli(project, "get", "total,big", "pipeline.py", "-o", "path")
    assert [Path(p).parent.name for p in both.stdout.splitlines()] == [
        "pipeline.py--total",
        "pipeline.py--big",
    ]


def test_o_value_reads_parquet_as_rows(project):
    pytest.importorskip("pyarrow")
    pytest.importorskip("pandas")
    (project / "pipeline.py").write_text(
        PIPELINE
        + """

import pandas as pd


@asset()
def frame() -> pd.DataFrame:
    return pd.DataFrame({"n": [1, 2]})
"""
    )
    out = doc(cli(project, "get", "frame", "pipeline.py"))
    assert out["final_output"]["_barca_artifact"]["format"] == "parquet"
    rows = doc(cli(project, "get", "frame", "pipeline.py", "-o", "value", "--json"))
    assert rows["final_output"] == [{"n": 1}, {"n": 2}]


def test_a_partitioned_target_returns_every_key_sorted(project):
    out = doc(cli(project, "get", "sales", "pipeline.py"))
    assert list(out["final_output"]) == ["region=amer", "region=apac", "region=emea"]
    assert read(project, out["final_output"]["region=emea"]) == {"region": "emea", "revenue": 400}
    paths = cli(project, "get", "sales", "pipeline.py", "-o", "path").stdout.splitlines()
    assert [Path(p).parent.name for p in paths] == [
        "pipeline.py--sales_region_amer",
        "pipeline.py--sales_region_apac",
        "pipeline.py--sales_region_emea",
    ]
    values = json.loads(cli(project, "get", "sales", "pipeline.py", "-o", "value").stdout)
    assert values["region=amer"] == {"region": "amer", "revenue": 400}


def test_partition_selects_the_keys_that_run_and_are_returned(project):
    out = doc(cli(project, "get", "sales", "pipeline.py", "--partition", "region=emea,amer"))
    assert out["steps_executed"] == 2
    assert list(out["final_output"]) == ["region=amer", "region=emea"]
    assert not list((project / ".barca/artifacts").glob("*sales_region_apac*/*"))
    # The same keys again are cached; --refresh of the target recomputes only the selection.
    again = doc(cli(project, "get", "sales", "pipeline.py", "--partition", "region=emea"))
    assert again["steps_executed"] == 0 and list(again["final_output"]) == ["region=emea"]
    refreshed = doc(
        cli(
            project,
            "get",
            "sales",
            "pipeline.py",
            "--partition",
            "region=emea",
            "--refresh",
            "sales",
        )
    )
    assert refreshed["steps_executed"] == 1


def test_partition_takes_a_json_object_for_compound_keys(project):
    out = doc(
        cli(
            project,
            "get",
            "daily",
            "pipeline.py",
            "--partition",
            '{"region": "emea", "day": ["d2"]}',
        )
    )
    assert list(out["final_output"]) == ["day=d2,region=emea"]
    assert out["steps_executed"] == 1


@pytest.mark.parametrize(
    "args",
    [
        ["get", "total", "pipeline.py", "--partition", "region=emea"],  # not partitioned
        ["get", "sales", "pipeline.py", "--partition", "day=d1"],  # no such dimension
        ["get", "sales", "pipeline.py", "--partition", "region=latam"],  # no such key
        ["get", "pipeline.py", "--partition", "region=latam"],  # no target, no such key
        ["get", "sales", "pipeline.py", "--partition", "region"],  # malformed
    ],
)
def test_a_bad_partition_selection_is_a_usage_error_before_anything_runs(project, args):
    proc = cli(project, *args, "--json")
    assert proc.returncode == 2, proc.stderr
    error = json.loads(proc.stderr.strip().splitlines()[-1])
    assert error["kind"] == "usage" and "--partition" in error["error"] + error["remediation"]
    assert not (project / ".barca/artifacts").exists()


def test_failed_keys_are_absent_and_the_run_fails(project):
    out = doc(cli(project, "get", "flaky", "pipeline.py"), code=1)
    assert out["status"] == "failed" and out["failed_node"] == "pipeline.py:flaky[k=bad]"
    assert list(out["final_output"]) == ["k=ok1", "k=ok2"]
    paths = cli(project, "get", "flaky", "pipeline.py", "-o", "path")
    assert paths.returncode == 1
    assert len(paths.stdout.splitlines()) == 2


def test_a_terminal_previews_a_small_json_value_under_its_path(project):
    master, slave = pty.openpty()
    env = {**os.environ, "BARCA_REMOTE": "off"}
    env.pop("BARCA_OUTPUT", None)
    proc = subprocess.Popen(
        [_find_binary(), "get", "total", "pipeline.py"],
        cwd=project,
        stdout=slave,
        stderr=subprocess.PIPE,
        env=env,
    )
    os.close(slave)
    chunks = []
    while True:
        try:
            chunk = os.read(master, 4096)
        except OSError:
            break
        if not chunk:
            break
        chunks.append(chunk)
    proc.wait(timeout=60)
    out = b"".join(chunks).decode().replace("\r\n", "\n")
    assert "pipeline.py--total/" in out
    assert '  "total": 6' in out
    # A value longer than the preview limit shows only its path.
    proc = cli(project, "get", "big", "pipeline.py", "--pretty")
    # The path is the last line: no preview of the list's elements follows it. Checking lines
    # rather than a substring like "99" keeps a run id or duration from deciding the result.
    lines = [line.strip() for line in proc.stdout.splitlines() if line.strip()]
    assert "pipeline.py--big/" in lines[-1], proc.stdout
    assert not {"99", "99,", "[", "]"} & set(lines), proc.stdout


def test_python_get_still_returns_values(project, monkeypatch):
    import barca

    monkeypatch.chdir(project)
    monkeypatch.setenv("BARCA_REMOTE", "off")
    assert barca.get("total", "pipeline.py") == {"total": 6}
    by_key = barca.get("sales", "pipeline.py")
    assert sorted(by_key) == ["region=amer", "region=apac", "region=emea"]
    assert barca.get("sales", "pipeline.py", partition="region=emea") == {
        "region=emea": {"region": "emea", "revenue": 400}
    }
    assert list(barca.get("daily", "pipeline.py", partition={"region": "amer", "day": "d1"})) == [
        "day=d1,region=amer"
    ]
