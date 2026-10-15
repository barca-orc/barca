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
        [_find_binary(), *args],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
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


@pytest.mark.parametrize(
    "imports,decorator",
    [
        ("import barca as c", "c.asset"),
        ("from barca import asset as b\nimport barca as c", "c.asset"),
        ("from barca import asset as a", "a"),
        ("from barca import asset", "asset"),
    ],
)
def test_module_export_write_invalidates_other_aliases_and_later_imports(
    tmp_path, imports, decorator
):
    (tmp_path / "pipeline.py").write_text(
        "import barca as b\nimport barca as c\n"
        "def foreign(**options): return lambda fn: fn\n"
        f"b.asset = foreign\n{imports}\n"
        f"@{decorator}(input={{}})\ndef hidden(): return 1\n"
    )
    out = invoke(tmp_path, "list", "pipeline.py", "--json")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["nodes"] == []


def test_mutated_module_wrapper_executes_and_invalidates_cache(tmp_path):
    source = (
        "import barca as b\nimport barca as c\n"
        "def foreign(path, *, value):\n    return lambda fn: lambda: value\n"
        "b.sink = foreign\n"
        "@c.asset()\n@c.sink('ignored.json', value=7)\ndef value(): return 0\n"
    )
    (tmp_path / "pipeline.py").write_text(source)
    (tmp_path / "barca.toml").write_text("")
    first = invoke(tmp_path, "get", "value", "pipeline.py")
    assert first.returncode == 0, first.stderr
    assert json.loads(first.stdout)["final_output"] == 7
    assert not (tmp_path / "ignored.json").exists()
    (tmp_path / "pipeline.py").write_text(source.replace("lambda: value", "lambda: value + 2"))
    changed = invoke(tmp_path, "get", "value", "pipeline.py")
    assert changed.returncode == 0, changed.stderr
    assert json.loads(changed.stdout)["steps_executed"] == 1
    assert json.loads(changed.stdout)["final_output"] == 9


def test_noncolliding_project_star_import_preserves_nodes_and_groups(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "shared.py").write_text(
        "__all__ = ['_MODEL_YEAR']\n_MODEL_YEAR = 2026\n"
        "raise RuntimeError('planning must not import shared')\n"
    )
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, group\nfrom shared import *\n"
        "@asset\ndef foo(): return _MODEL_YEAR\n"
        "g = group('G', members=[foo], output=foo)\n"
    )
    out = invoke(tmp_path, "list", "pipeline.py", "--json")
    assert out.returncode == 0, out.stderr
    data = json.loads(out.stdout)
    assert [node["id"] for node in data["nodes"]] == ["pipeline.py:foo"]
    groups = invoke(tmp_path, "list", "pipeline.py", "--groups", "--json")
    assert groups.returncode == 0, groups.stderr
    assert len(json.loads(groups.stdout)["groups"]) == 1


@pytest.mark.parametrize(
    "exports,expected",
    [
        ("__all__ = ['YEAR']", ["foo", "job"]),
        ("__all__ = ()", ["foo", "job"]),
        ("__all__: list[str] = ['YEAR']", ["foo", "job"]),
        ("__all__ = ['asset']", ["job"]),
        ("__all__ = ['task']", ["foo"]),
        ("__all__ = ['a']", ["job"]),
        ("__all__ = ['YEAR']\n__all__.append('asset')", []),
        ("__all__ = ['YEAR']\n__all__ += ['asset']", []),
        ("__all__ = make_exports()", []),
        ("__all__ = ['YEAR']\nif True: __all__ = ['asset']", []),
        ("__all__ = ['YEAR']\nfrom other import *", []),
        ("__all__ = ['YEAR']\nfrom builtins import globals as g\ng()['__all__'] = ['asset']", []),
        ("__all__ = ['YEAR']\nexec('anything')", []),
        ("__all__ = ['YEAR']\ntry: pass\nexcept Exception as __all__: pass", []),
        ("__all__ = ['YEAR']\nmatch value:\n    case __all__: pass", []),
        ("YEAR = 2026", []),
    ],
)
def test_star_exports_only_invalidate_affected_bindings(tmp_path, exports, expected):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "shared.py").write_text(exports + "\nraise RuntimeError('must not import')\n")
    # Exercise aliases as well as the canonical export name.
    alias = "asset as a" if "['a']" in exports else "asset"
    decorator = "a" if alias.endswith("as a") else "asset"
    (tmp_path / "pipeline.py").write_text(
        f"from barca import {alias}, task\nfrom shared import *\n"
        f"@{decorator}()\ndef foo(): return 1\n"
        "@task()\ndef job(): return 2\n"
    )
    result = invoke(tmp_path, "list", "pipeline.py", "--json")
    assert result.returncode == 0, result.stderr
    assert sorted(n["id"].split(":")[-1] for n in json.loads(result.stdout)["nodes"]) == sorted(
        expected
    )


@pytest.mark.parametrize("import_line", ["from .shared import *", "from pkg.shared import *"])
def test_package_star_exports_match_worker_import_path(tmp_path, import_line):
    (tmp_path / "barca.toml").write_text("")
    package = tmp_path / "pkg"
    package.mkdir()
    (package / "__init__.py").write_text("")
    (package / "shared.py").write_text("__all__ = ['YEAR']\nYEAR = 2026\n")
    (tmp_path / "shared.py").write_text("__all__ = ['asset']\n")
    (package / "pipeline.py").write_text(
        f"from barca import asset\n{import_line}\n@asset()\ndef foo(): return YEAR\n"
    )
    listed = invoke(tmp_path, "list", "pkg/pipeline.py", "--json")
    assert listed.returncode == 0, listed.stderr
    assert json.loads(listed.stdout)["total"] == 1
    first = invoke(tmp_path, "get", "foo", "pkg/pipeline.py", "--json")
    assert first.returncode == 0, first.stderr
    assert json.loads(first.stdout)["final_output"] == 2026
    (package / "shared.py").write_text("__all__ = ['YEAR']\nYEAR = 2027\n")
    again = invoke(tmp_path, "get", "foo", "pkg/pipeline.py", "--json")
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout)["final_output"] == 2027
    assert json.loads(again.stdout)["steps_executed"] == 1


@pytest.mark.parametrize(
    "import_line,decorator",
    [
        ("from barca import asset as a", "a"),
        ("import barca as b", "b.asset"),
    ],
)
def test_safe_star_import_retains_argument_validation(tmp_path, import_line, decorator):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "shared.py").write_text("__all__ = ['YEAR']\nYEAR = 2026\n")
    (tmp_path / "pipeline.py").write_text(
        f"{import_line}\nfrom shared import *\n@{decorator}(input={{}})\ndef foo(): return YEAR\n"
    )
    result = invoke(tmp_path, "list", "pipeline.py", "--json")
    assert result.returncode == 2
    assert "`input` is not an argument" in result.stderr


def test_colliding_group_export_leaves_nodes_discoverable(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "shared.py").write_text("__all__ = ['group']\n")
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset, group\nfrom shared import *\n"
        "@asset()\ndef foo(): return 1\ng = group('G', members=[foo], output=foo)\n"
    )
    result = invoke(tmp_path, "list", "pipeline.py", "--json")
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["total"] == 1
    groups = invoke(tmp_path, "list", "pipeline.py", "--groups", "--json")
    assert groups.returncode == 0, groups.stderr
    assert json.loads(groups.stdout)["groups"] == []
