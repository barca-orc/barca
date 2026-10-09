"""Qualified/aliased static recognition and real execution share Barca semantics."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary


def invoke(root, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["BARCA_POOL_SIZE"] = "2"
    return subprocess.run(
        [_find_binary(), *args], cwd=root, env=env, capture_output=True, text=True, timeout=30
    )


@pytest.mark.parametrize(
    "imports,asset,parts,collect",
    [
        ("import barca", "barca.asset", "barca.partitions", "barca.collect"),
        ("import barca as b", "b.asset", "b.partitions", "b.collect"),
        ("from barca import asset as a, partitions as p, collect as c", "a", "p", "c"),
    ],
)
def test_aliases_plan_without_import_and_run_partitioned_results(
    tmp_path, imports, asset, parts, collect
):
    source = (
        f"{imports}\nfrom pathlib import Path\nPath('imported').touch()\n"
        f"@{asset}(partitions={{'key': {parts}(['a', 'b'])}})\n"
        "def part(key):\n    return {'key': key}\n"
        f"@{asset}(inputs={{'rows': {collect}(part)}}, retries=1)\n"
        "def summary(rows):\n    return {'keys': sorted(r['key'] for r in rows)}\n"
    )
    (tmp_path / "pipeline.py").write_text(source)
    (tmp_path / "barca.toml").write_text("")
    listed = invoke(tmp_path, "list", "pipeline.py", "--json")
    assert listed.returncode == 0, listed.stderr
    assert [n["id"] for n in json.loads(listed.stdout)["nodes"]] == [
        "pipeline.py:part",
        "pipeline.py:summary",
    ]
    planned = invoke(tmp_path, "plan", "pipeline.py")
    assert planned.returncode == 0, planned.stderr
    assert not (tmp_path / "imported").exists()
    result = invoke(tmp_path, "get", "summary", "pipeline.py")
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["final_output"] == {"keys": ["a", "b"]}
    assert (tmp_path / "imported").exists()
    (tmp_path / "pipeline.py").write_text(source.replace("retries=1", "retries=3"))
    cached = invoke(tmp_path, "get", "summary", "pipeline.py")
    assert cached.returncode == 0, cached.stderr
    assert json.loads(cached.stdout)["steps_executed"] == 0


@pytest.mark.parametrize(
    "imports,decorator",
    [("import barca", "barca.asset"), ("from barca import task as asset", "asset")],
)
def test_alias_arguments_fail_before_import(tmp_path, imports, decorator):
    (tmp_path / "pipeline.py").write_text(
        f"{imports}\nfrom pathlib import Path\nPath('imported').touch()\n"
        f"@{decorator}(input={{}})\ndef value(): return 1\n"
    )
    out = invoke(tmp_path, "list", "pipeline.py", "--json")
    assert out.returncode == 2
    assert "`input` is not an argument" in out.stderr
    assert not (tmp_path / "imported").exists()


def test_foreign_wrapper_stacked_on_genuine_node_still_executes(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset as actual_asset\n"
        "def asset(**options):\n    return lambda fn: lambda: options['value']\n"
        "@actual_asset()\n@asset(value=7)\ndef value(): return 0\n"
    )
    (tmp_path / "barca.toml").write_text("")
    out = invoke(tmp_path, "get", "value", "pipeline.py")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["final_output"] == 7
