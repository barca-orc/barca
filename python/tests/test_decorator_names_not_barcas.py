"""A decorator or helper that is not positively barca's is never checked (#284).

The argument check (`test_decorator_arguments.py`) rejects arguments barca's decorators do not
define. It must not judge a `task` that is Celery's, Prefect's or the file's own: those files
listed fine on 0.18.1 and must list the same way now. A name is checked only when a
`from barca import NAME` stands at the top level of the module and nothing else binds the name
at module scope (`BarcaNames` in `crates/barca-core/src/decorator_args.rs`).

Each case below binds the name some other way and then passes it an argument barca does not
define. `EXPECTED` is what `barca list` printed for the same file on 0.18.1 (ids and kinds, in
order): barca still reads these functions as nodes, as it always has, and that is not changed
here.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

RAW = "@asset()\ndef raw():\n    return 1\n\n\n"

# name -> (source, the nodes 0.18.1 lists for it)
CASES: dict[str, tuple[str, list[tuple[str, str]]]] = {
    # The three from the review.
    "celery_assignment": (
        "from barca import asset\nfrom celery import Celery\n\napp = Celery('x')\ntask = app.task\n\n\n"
        "@task(bind=True)\ndef send(self):\n    return 1\n\n\n" + RAW,
        [("pipeline.py:raw", "asset"), ("pipeline.py:send", "task")],
    ),
    "try_import": (
        "from barca import asset\n\ntry:\n    from prefect import task\nexcept ImportError:\n"
        "    from barca import task\n\n\n"
        "@task(log_prints=True)\ndef send():\n    return 1\n",
        [("pipeline.py:send", "task")],
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
        [("pipeline.py:t", "task")],
    ),
    "augmented_assignment": (
        "from barca import task\n\ntask |= extra\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "tuple_unpacking": (
        "from barca import asset, task\n\n(asset, [task, *rest]) = make()\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n\n\n@asset(owner='me')\ndef a():\n    return 1\n",
        [("pipeline.py:a", "asset"), ("pipeline.py:t", "task")],
    ),
    "walrus": (
        "from barca import task\n\nif (task := make()) is not None:\n    pass\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "def": (
        "from barca import asset\n\n\ndef asset(**options):\n    return lambda f: f\n\n\n"
        "@asset(owner='me')\ndef a():\n    return 1\n",
        [("pipeline.py:a", "asset")],
    ),
    "class": (
        "from barca import sensor\n\n\nclass sensor:\n    def __init__(self, **kw):\n        pass\n\n\n"
        "@sensor(poll=5)\ndef s():\n    return (True, 1)\n",
        [("pipeline.py:s", "sensor")],
    ),
    "import_as": (
        "from barca import task\nimport celery_shim as task\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "import_module_of_that_name": (
        "from barca import task\nimport task.helpers\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "from_other_import": (
        "from barca import asset, task\nfrom dagster import asset\nfrom other import sink as sink\n\n\n"
        "@asset(ins={'x': 1})\n@sink('p', mode='a')\ndef a():\n    return 1\n",
        [("pipeline.py:a", "asset")],
    ),
    "from_other_import_as": (
        "from barca import task\nfrom celery import shared_task as task\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "barca_import_under_another_name": (
        "from barca import task as asset\n\n\n@asset(inputs={}, when=1)\ndef a():\n    return 1\n",
        [("pipeline.py:a", "asset")],
    ),
    "import_nested_in_if": (
        "from barca import task\nimport sys\n\nif sys.version_info >= (3, 12):\n"
        "    from newlib import task\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "import_nested_in_with": (
        "from barca import task\n\nwith ctx():\n    from newlib import task\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "barca_import_nested_in_try": (
        "try:\n    from barca import task\nexcept ImportError:\n    raise\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "global_in_a_function": (
        "from barca import task\n\n\ndef setup():\n    global task\n    task = make()\n\n\n"
        "setup()\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "for_target": (
        "from barca import task\n\nfor task in registry():\n    pass\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "with_as": (
        "from barca import task\n\nwith make() as task:\n    pass\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "except_as": (
        "from barca import task\n\ntry:\n    pass\nexcept Exception as task:\n    pass\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "match_capture": (
        "from barca import task\n\nmatch make():\n    case [task, *_]:\n        pass\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "del": (
        "from barca import task\n\ndel task\nfrom celery import *\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "star_import_after": (
        "from barca import asset, task\nfrom celery_shim import *\n\n\n"
        "@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "no_import_of_the_name": (
        "import barca\nfrom barca import asset\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "relative_module_named_barca": (
        "from .barca import task\nimport barca\n\n\n@task(bind=True)\ndef t():\n    pass\n",
        [("pipeline.py:t", "task")],
    ),
    "helper_rebound": (
        "from barca import asset, partitions, Schedule\n\npartitions = load_partitions\n"
        "Schedule = cron_lib.Schedule\n\n\n"
        "@asset(partitions={'k': partitions(kind='daily')}, freshness=Schedule(cron='0 5 * * *', tz='utc'))\n"
        "def a(k):\n    return 1\n",
        None,  # 0.18.1 exits 2 here too: it reads the cron as empty. Asserted below.
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


@pytest.mark.parametrize("case", [c for c in CASES if CASES[c][1] is not None])
def test_a_name_bound_by_anything_else_is_not_checked(tmp_path: Path, case: str) -> None:
    source, expected = CASES[case]
    proc = listed(tmp_path, source)
    assert proc.returncode == 0, proc.stderr
    assert [(n["id"], n["kind"]) for n in json.loads(proc.stdout)["nodes"]] == expected


def test_a_rebound_helper_is_not_checked(tmp_path: Path) -> None:
    """The helpers too. 0.18.1 rejects this file for another reason (it reads the cron of any
    `Schedule(...)` call, and finds none); the message must still be that one, not one about
    the arguments of a function that is not barca's."""
    proc = listed(tmp_path, CASES["helper_rebound"][0])
    assert proc.returncode == 2
    assert "invalid Schedule cron" in proc.stderr
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
