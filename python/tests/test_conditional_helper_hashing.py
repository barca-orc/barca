"""Conditional project imports invalidate real cached results without executing the plan."""

import json
import os
import subprocess
from pathlib import Path

import pytest
from barca.api import _find_binary


def invoke(project: Path, *args: str):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=project,
        capture_output=True,
        text=True,
        env=os.environ.copy(),
        timeout=60,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


@pytest.fixture(params=[1, 2], autouse=True)
def worker_pool(request, monkeypatch):
    monkeypatch.setenv("BARCA_POOL_SIZE", str(request.param))


@pytest.mark.parametrize(
    ("bindings", "expression"),
    [
        (
            "try:\n    from helpers import value\nexcept ImportError:\n    def value(): return -1",
            "value()",
        ),
        (
            "if True:\n    from helpers import value\nelse:\n    from fallback import value",
            "value()",
        ),
        (
            "try:\n    import helpers as selected\nexcept ImportError:\n    import fallback as selected",
            "selected.value()",
        ),
        ("from fallback import value\nif False:\n    from helpers import value", "value()"),
    ],
)
def test_conditional_import_cache_tracks_possible_helpers(tmp_path, bindings, expression):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "helpers.py").write_text("def value(): return 11\n")
    (tmp_path / "fallback.py").write_text("def value(): return 7\n")
    (tmp_path / "p.py").write_text(
        "from barca import asset\nfrom pathlib import Path\n"
        "Path('imported').touch()\n"
        f"{bindings}\n@asset()\ndef result(): return {expression}\n"
        "@asset()\ndef unaffected(): return 100\n"
    )
    invoke(tmp_path, "plan", "p.py")
    assert not (tmp_path / "imported").exists()
    assert not (tmp_path / ".barca").exists()
    first = invoke(tmp_path, "get", "result,unaffected", "p.py")
    old_hash = next(step["run_hash"] for step in first["steps"] if step["id"].endswith(":result"))
    assert first["targets"]["result"]["final_output"] == (7 if "if False" in bindings else 11)
    assert invoke(tmp_path, "get", "result,unaffected", "p.py")["steps_executed"] == 0
    (tmp_path / "helpers.py").write_text("def value(): return 22\n")
    changed = invoke(tmp_path, "get", "result,unaffected", "p.py")
    assert changed["steps_executed"] == 1
    step = next(step for step in changed["steps"] if step["id"].endswith(":result"))
    assert step["run_hash"] != old_hash
    assert changed["targets"]["result"]["final_output"] == (7 if "if False" in bindings else 22)
    assert invoke(tmp_path, "get", "result,unaffected", "p.py")["steps_executed"] == 0


def test_conditional_reexport_in_package_tracks_relative_import(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    package = tmp_path / "pkg"
    package.mkdir()
    (package / "__init__.py").write_text("")
    (package / "helpers.py").write_text("def value(): return 11\n")
    (package / "selected.py").write_text(
        "try:\n    from .helpers import value\nexcept ImportError:\n    def value(): return -1\n"
    )
    (tmp_path / "p.py").write_text(
        "from barca import asset\nfrom pkg.selected import value\n"
        "@asset()\ndef result(): return value()\n"
    )
    first = invoke(tmp_path, "get", "result", "p.py")
    assert first["final_output"] == 11
    assert invoke(tmp_path, "get", "result", "p.py")["steps_executed"] == 0
    (package / "helpers.py").write_text("def value(): return 22\n")
    changed = invoke(tmp_path, "get", "result", "p.py")
    assert changed["steps_executed"] == 1
    assert changed["final_output"] == 22
    assert changed["steps"][0]["run_hash"] != first["steps"][0]["run_hash"]


def test_missing_primary_helper_then_available_changes_cached_fallback(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "fallback.py").write_text("def value(): return 7\n")
    (tmp_path / "p.py").write_text(
        "from barca import asset\ntry:\n    from helpers import value\n"
        "except ImportError:\n    from fallback import value\n"
        "@asset()\ndef result(): return value()\n"
    )
    first = invoke(tmp_path, "get", "result", "p.py")
    assert first["final_output"] == 7
    assert invoke(tmp_path, "get", "result", "p.py")["steps_executed"] == 0
    (tmp_path / "helpers.py").write_text("def value(): return 11\n")
    changed = invoke(tmp_path, "get", "result", "p.py")
    assert changed["final_output"] == 11
    assert changed["steps_executed"] == 1
    assert changed["steps"][0]["run_hash"] != first["steps"][0]["run_hash"]
