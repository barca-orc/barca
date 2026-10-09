"""Foreign/rebound decorators do not define Barca nodes (#316).

Barca used to recognize foreign functions named asset/sensor/task as nodes. Static
provenance now prevents that, while validation only judges proven Barca calls.
Bare names without competing bindings retain the legacy source-snippet behavior.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

RAW = "@asset()\ndef raw():\n    return 1\n\n\n"

# name -> (source, expected statically recognized nodes)
CASES: dict[str, tuple[str, list[tuple[str, str]]]] = {
    # The three from the review.
    "celery_assignment": (
        "from barca import asset\nfrom celery import Celery\n\napp = Celery('x')\ntask = app.task\n\n\n"
        "@task(bind=True)\ndef send(self):\n    return 1\n\n\n" + RAW,
        [("pipeline.py:raw", "asset")],
    ),
    "try_import": (
        "from barca import asset\n\ntry:\n    from prefect import task\nexcept ImportError:\n"
        "    from barca import task\n\n\n"
        "@task(log_prints=True)\ndef send():\n    return 1\n",
        [],
    ),
    "local_class_helper": (
        "from barca import asset, collect\n\n\nclass collect:\n"
        "    def __init__(self, thing, flatten=False):\n        self.thing = thing\n\n\n"
        + RAW
        + "@asset(inputs={'raw': collect(raw, flatten=True)})\ndef uses(raw):\n    return raw\n",
        [("pipeline.py:raw", "asset"), ("pipeline.py:uses", "asset")],
    ),
    # Every other way a module-level name gets bound.
    "annotated_assignment": (
        "from barca import task\n\ntask: object = make()\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "augmented_assignment": (
        "from barca import task\n\ntask |= extra\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "tuple_unpacking": (
        "from barca import asset, task\n\n(asset, [task, *rest]) = make()\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n\n\n@asset(owner='me')\ndef a():\n    return 1\n",
        [],
    ),
    "walrus": (
        "from barca import task\n\nif (task := make()) is not None:\n    pass\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "def": (
        "from barca import asset\n\n\ndef asset(**options):\n    return lambda f: f\n\n\n"
        "@asset(owner='me')\ndef a():\n    return 1\n",
        [],
    ),
    "class": (
        "from barca import sensor\n\n\nclass sensor:\n    def __init__(self, **kw):\n        pass\n\n\n"
        "@sensor(poll=5)\ndef s():\n    return (True, 1)\n",
        [],
    ),
    "import_as": (
        "from barca import task\nimport celery_shim as task\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "import_module_of_that_name": (
        "from barca import task\nimport task.helpers\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "from_other_import": (
        "from barca import asset, task\nfrom dagster import asset\nfrom other import sink as sink\n\n\n"
        "@asset(ins={'x': 1})\n@sink('p', mode='a')\ndef a():\n    return 1\n",
        [],
    ),
    "from_other_import_as": (
        "from barca import task\nfrom celery import shared_task as task\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "barca_import_under_another_name": (
        "from barca import task as asset\n\n\n@asset(inputs={}, when=1)\ndef a():\n    return 1\n",
        [],
    ),
    "import_nested_in_if": (
        "from barca import task\nimport sys\n\nif sys.version_info >= (3, 12):\n"
        "    from newlib import task\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "import_nested_in_with": (
        "from barca import task\n\nwith ctx():\n    from newlib import task\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "barca_import_nested_in_try": (
        "try:\n    from barca import task\nexcept ImportError:\n    raise\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "global_in_a_function": (
        "from barca import task\n\n\ndef setup():\n    global task\n    task = make()\n\n\n"
        "setup()\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "for_target": (
        "from barca import task\n\nfor task in registry():\n    pass\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "with_as": (
        "from barca import task\n\nwith make() as task:\n    pass\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "except_as": (
        "from barca import task\n\ntry:\n    pass\nexcept Exception as task:\n    pass\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "match_capture": (
        "from barca import task\n\nmatch make():\n    case [task, *_]:\n        pass\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "del": (
        "from barca import task\n\ndel task\nfrom celery import *\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "star_import_after": (
        "from barca import asset, task\nfrom celery_shim import *\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "no_import_of_the_name": (
        "import barca\nfrom barca import asset\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "relative_module_named_barca": (
        "from .barca import task\nimport barca\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [],
    ),
    "helper_rebound": (
        "from barca import asset, partitions, Schedule\n\npartitions = load_partitions\n"
        "Schedule = cron_lib.Schedule\n\n\n"
        "@asset(partitions={'k': partitions(kind='daily')}, freshness=Schedule(cron='0 5 * * *', tz='utc'))\n"
        "def a(k):\n    return 1\n",
        None,  # Helper semantics are asserted separately below.
    ),
}


def listed(tmp_path: Path, source: str) -> subprocess.CompletedProcess:
    (tmp_path / "pipeline.py").write_text(source)
    return subprocess.run(
        [_find_binary(), "list", "pipeline.py", "--json"],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=60,
    )


@pytest.mark.parametrize(
    "case",
    [
        c
        for c in CASES
        if CASES[c][1] is not None
        and c not in ("barca_import_under_another_name", "local_class_helper")
    ],
)
def test_a_name_bound_by_anything_else_is_not_checked(tmp_path: Path, case: str) -> None:
    source, expected = CASES[case]
    proc = listed(tmp_path, source)
    assert proc.returncode == 0, proc.stderr
    assert [(n["id"], n["kind"]) for n in json.loads(proc.stdout)["nodes"]] == expected


def test_a_rebound_helper_is_not_checked(tmp_path: Path) -> None:
    """A rebound helper has neither Barca argument checks nor Barca cron semantics."""
    proc = listed(tmp_path, CASES["helper_rebound"][0])
    assert proc.returncode == 0, proc.stderr
    assert [(n["id"], n["kind"]) for n in json.loads(proc.stdout)["nodes"]] == [
        ("pipeline.py:a", "asset")
    ]
    assert "is not an argument of" not in proc.stderr


@pytest.mark.parametrize(
    "imports",
    [
        "from barca import task",
        "from barca import asset, task, sink",
        "from barca import (\n    asset,\n    task,\n)",
        "from barca import *",
        "import os\nfrom barca import task\nfrom os import path",
        # Bound before the barca import by something else is still a rebinding: not checked
        # is the safe answer, checked would also be right. Only the forms above are promised.
    ],
)
def test_a_name_imported_from_barca_and_nothing_else_is_checked(
    tmp_path: Path, imports: str
) -> None:
    proc = listed(tmp_path, f"{imports}\n\n\n@task(when=1)\ndef t():\n    pass\n")
    assert proc.returncode == 2
    assert "`when` is not an argument of @task" in proc.stderr


def test_local_names_inside_functions_do_not_turn_the_check_off(tmp_path: Path) -> None:
    """A parameter, a local variable or a comprehension variable called `task` is another
    scope: the module's `task` is still barca's."""
    source = (
        "from barca import task\n\n\n"
        "def helper(task, asset=None):\n    collect = [task for task in asset or []]\n"
        "    for sink in collect:\n        pass\n    return collect\n\n\n"
        "class Registry:\n    task = None\n\n    def sensor(self):\n        return self.task\n\n\n"
        "names = [task for task in ('a', 'b')]\n\n\n"
        "@task(when=1)\ndef t():\n    pass\n"
    )
    proc = listed(tmp_path, source)
    assert proc.returncode == 2
    assert "pipeline.py:t (line 21): `when` is not an argument of @task" in proc.stderr


@pytest.mark.parametrize(
    "rebinding",
    [
        "def install(x=(asset := custom)): pass",
        "@((asset := custom)())\ndef install(): pass",
        "class Install((asset := custom) and object): pass",
        "@((asset := custom)())\nclass Install: pass",
    ],
)
def test_definition_time_rebinding_allows_foreign_arguments(tmp_path, rebinding):
    source = (
        "from barca import asset\n"
        "def custom(**kwargs):\n"
        '    return lambda fn: lambda: kwargs["mode"]\n'
        f"{rebinding}\n"
        '@asset(mode="custom")\n'
        "def value():\n"
        "    return 0\n"
    )
    proc = listed(tmp_path, source)
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["nodes"] == []


def test_imported_alias_receives_its_original_signature(tmp_path):
    proc = listed(tmp_path, CASES["barca_import_under_another_name"][0])
    assert proc.returncode == 2
    assert "`when` is not an argument of @task" in proc.stderr


def test_foreign_collect_does_not_receive_barca_fan_in_semantics(tmp_path):
    proc = listed(tmp_path, CASES["local_class_helper"][0])
    assert proc.returncode == 2
    assert "unknown upstream 'collect'" in proc.stderr
