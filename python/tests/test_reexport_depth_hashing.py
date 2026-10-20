"""Plain-module re-exports and deep import chains participate in the dependency cone (#295)."""

import json
import os
import subprocess

import pytest
from barca.api import _find_binary

# Each case: the extra project modules, and the asset body that reaches `helpers.compute`.
REEXPORTS = {
    "from_wrapper": (
        {"wrapper.py": "import helpers\n"},
        "from wrapper import helpers\n@asset()\ndef value():\n    return helpers.compute()\n",
    ),
    "wrapper_attribute": (
        {"wrapper.py": "import helpers\n"},
        "import wrapper\n@asset()\ndef value():\n    return wrapper.helpers.compute()\n",
    ),
    "aliased": (
        {"wrapper.py": "import helpers as h\n"},
        "from wrapper import h\n@asset()\ndef value():\n    return h.compute()\n",
    ),
    "reexport_of_reexport": (
        {"inner.py": "import helpers\n", "wrapper.py": "from inner import helpers\n"},
        "from wrapper import helpers\n@asset()\ndef value():\n    return helpers.compute()\n",
    ),
    "local_import": (
        {"wrapper.py": "import helpers\n"},
        "@asset()\ndef value():\n    from wrapper import helpers\n    return helpers.compute()\n",
    ),
    "module_as_value": (
        {"wrapper.py": "import helpers\n"},
        "from wrapper import helpers\n@asset()\ndef value():\n"
        "    return getattr(helpers, 'compute')()\n",
    ),
    # `wrapper` and `helpers` import each other: planning terminates and still follows.
    "import_cycle": (
        {"wrapper.py": "import helpers\n\nNAME = 'wrapper'\n"},
        "from wrapper import helpers\n@asset()\ndef value():\n    return helpers.compute()\n",
    ),
}


def chain(depth):
    """m1.f calls m2.f ... m{depth}.f, which calls helpers.compute."""
    modules = {}
    for i in range(1, depth + 1):
        nxt = f"from m{i + 1} import f as g\n" if i < depth else "import helpers\n"
        call = "g()" if i < depth else "helpers.compute()"
        modules[f"m{i}.py"] = f"{nxt}\ndef f():\n    return {call}\n"
    return modules


def ring(size):
    """m1 ... m{size} import each other in a cycle; m{size}.f reaches helpers.compute."""
    modules = {}
    for i in range(1, size + 1):
        nxt = i % size + 1
        if i == size:
            body = f"    return helpers.compute() if n == 0 else m{nxt}.f(n - 1)\n"
            head = f"import helpers\nimport m{nxt}\n"
        else:
            body = "    return g(n)\n"
            head = f"import m{nxt}\n\ndef g(n):\n    return m{nxt}.f(n)\n"
        modules[f"m{i}.py"] = f"{head}\ndef f(n):\n{body}"
    return modules


CASES = {
    **REEXPORTS,
    "chain_depth_10": (
        chain(10),
        "from m1 import f\n@asset()\ndef value():\n    return f()\n",
    ),
    "cycle_of_10": (
        ring(10),
        "from m1 import f\n@asset()\ndef value():\n    return f(0)\n",
    ),
}


def cli(root, *args):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env={**os.environ, "BARCA_REMOTE": "off", "PYTHONDONTWRITEBYTECODE": "1"},
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


@pytest.mark.parametrize("case", sorted(CASES))
def test_helper_edit_invalidates_only_its_consumers(tmp_path, case):
    modules, body = CASES[case]
    (tmp_path / "helpers.py").write_text(
        "import wrapper\n\ndef compute():\n    return 1\n"
        if case == "import_cycle"
        else "def compute():\n    return 1\n"
    )
    for name, src in modules.items():
        (tmp_path / name).write_text(src)
    (tmp_path / "p.py").write_text(
        "from barca import asset\n" + body + "\n@asset()\ndef unrelated():\n    return 7\n"
    )
    first = cli(tmp_path, "get", "p.py", "--json")
    assert first["steps_executed"] == 2
    assert cli(tmp_path, "get", "value", "p.py", "--json")["final_output"] == 1
    assert cli(tmp_path, "get", "p.py", "--json")["steps_executed"] == 0
    helpers = (tmp_path / "helpers.py").read_text()
    (tmp_path / "helpers.py").write_text(helpers.replace("return 1", "return 222"))
    changed = cli(tmp_path, "get", "p.py", "--json")
    assert changed["steps_executed"] == 1
    assert {s["id"]: s["status"] for s in changed["steps"]} == {
        "p.py:value": "ran",
        "p.py:unrelated": "cached",
    }
    assert cli(tmp_path, "get", "value", "p.py", "--json")["final_output"] == 222
    assert cli(tmp_path, "get", "p.py", "--json")["steps_executed"] == 0
