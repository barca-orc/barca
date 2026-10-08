"""Editing a decorator re-runs a step only when the edit can change its result (#283).

Up to 0.18 the definition hash covered the decorators as text, so adding a partition key to an
inline list, a description, a comment or a reformat re-ran every key of the asset and everything
downstream. These tests edit the pipeline file between runs, the way a user does, for every way
of writing the keys, and for each decorator argument that counts and each that does not
(`barca docs cache`, "Which decorator arguments count").
"""

import json
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

# `sales` fans out over the keys; `margin` runs once per key of `sales`; `summary` collects
# every key; `report` reads both, so one `get report` shows what each kind of consumer does.
CONSUMERS = """

@asset(partitions={"region": partitions_from(sales)})
def margin(region: str, sales: dict) -> dict:
    return {"region": region, "margin": 1}


@asset(inputs={"all_sales": collect(sales)})
def summary(all_sales: list) -> dict:
    return {"regions": sorted(s["region"] for s in all_sales)}


@asset(inputs={"s": summary, "m": collect(margin)})
def report(s: dict, m: list) -> dict:
    return {"regions": s["regions"], "margins": len(m)}
"""

SALES = """
def sales(region: str) -> dict:
    return {"region": region}
"""

IMPORTS = "from barca import asset, collect, partitions, partitions_from\n"


def literal(keys: list[str]) -> str:
    return f'{IMPORTS}\n\n@asset(partitions={{"region": partitions({json.dumps(keys)})}}){SALES}'


def constant(keys: list[str]) -> str:
    return (
        f"{IMPORTS}\nREGIONS = {json.dumps(keys)}\n\n\n"
        f'@asset(partitions={{"region": partitions(REGIONS)}}){SALES}'
    )


def expression(keys: list[str]) -> str:
    listed = ", ".join(json.dumps(k) for k in keys)
    return (
        f"{IMPORTS}\n\n"
        f'@asset(partitions={{"region": partitions([r.lower() for r in ({listed},)])}}){SALES}'
    )


def function_call(keys: list[str]) -> str:
    return (
        f"{IMPORTS}\n\ndef regions():\n    return {json.dumps(keys)}\n\n\n"
        f'@asset(partitions={{"region": partitions(regions())}}){SALES}'
    )


def from_upstream(keys: list[str]) -> str:
    return (
        f"{IMPORTS}\n\n@asset()\ndef keys() -> list:\n    return {json.dumps(keys)}\n\n\n"
        f'@asset(partitions={{"region": partitions_from(keys)}}){SALES}'
    )


FORMS = {
    "literal": literal,
    "constant": constant,
    "expression": expression,
    "function_call": function_call,
    "partitions_from": from_upstream,
}


def ran(result: dict) -> dict[str, list[str] | bool]:
    """What executed: for a partitioned asset the keys that ran, for any other step `True`."""
    out: dict[str, list[str] | bool] = {}
    for step in result["steps"]:
        name = step["id"].split(":")[-1]
        if "partitions" in step:
            keys = sorted(k.split("=", 1)[1] for k in step["partitions"].get("will_run_keys", []))
            if keys:
                out[name] = keys
        elif step["status"] == "ran":
            out[name] = True
    return out


def get(project: Path, target: str = "report") -> dict:
    proc = subprocess.run(
        [_find_binary(), "get", target, "pipeline.py"], cwd=project, capture_output=True, text=True
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def write(project: Path, source: str) -> None:
    (project / "pipeline.py").write_text(source)


@pytest.fixture(params=[1, 2, 16], ids=lambda n: f"pool{n}", autouse=True)
def pool_size(request, monkeypatch) -> int:
    """Every test here runs with 1, 2 and 16 workers. What runs after an edit must not depend
    on the pool size, which defaults to the machine's core count: these tests passed on a
    16-core machine and failed on a small CI runner until #330 and #331 were fixed."""
    monkeypatch.setenv("BARCA_POOL_SIZE", str(request.param))
    return request.param


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


# ─── The set of partition keys ────────────────────────────────────────────────


@pytest.mark.parametrize("form", FORMS)
def test_adding_a_key_runs_only_the_new_key(project, form):
    source = FORMS[form]
    write(project, source(["us", "eu"]) + CONSUMERS)
    first = get(project)
    assert first["steps_executed"] == (7 if form == "partitions_from" else 6)
    assert get(project)["steps_executed"] == 0

    write(project, source(["us", "eu", "apac"]) + CONSUMERS)
    grown = get(project)
    expected = {
        "sales": ["apac"],  # the new key, and no other
        "margin": ["apac"],  # a per-key consumer runs for the new key only
        "summary": True,  # a collect consumer runs: its set of inputs changed
        "report": True,
    }
    if form == "partitions_from":
        expected["keys"] = True  # the list is the body of `keys`, which was edited
    assert ran(grown) == expected
    assert grown["final_output"] == {"regions": ["apac", "eu", "us"], "margins": 3}
    assert get(project)["steps_executed"] == 0


@pytest.mark.parametrize("form", FORMS)
def test_removing_a_key_runs_no_key(project, form):
    source = FORMS[form]
    write(project, source(["us", "eu", "apac"]) + CONSUMERS)
    get(project)

    write(project, source(["us", "apac"]) + CONSUMERS)
    shrunk = get(project)
    expected = {"summary": True, "report": True}  # the collect consumers: one input fewer
    if form == "partitions_from":
        expected["keys"] = True
    assert ran(shrunk) == expected
    assert shrunk["final_output"] == {"regions": ["apac", "us"], "margins": 2}

    # The removed key comes back from cache when it is listed again.
    write(project, source(["us", "eu", "apac"]) + CONSUMERS)
    back = ran(get(project))
    assert "sales" not in back and "margin" not in back


@pytest.mark.parametrize("form", FORMS)
def test_reordering_the_keys_runs_nothing(project, form):
    source = FORMS[form]
    write(project, source(["us", "eu", "apac"]) + CONSUMERS)
    get(project)

    write(project, source(["apac", "us", "eu"]) + CONSUMERS)
    reordered = get(project)
    # With `partitions_from(keys)` the list is the body of `keys`, so `keys` itself runs again.
    # It returns the same set of keys, and nothing that depends on them runs.
    assert ran(reordered) == ({"keys": True} if form == "partitions_from" else {})


# ─── Arguments that do not count ──────────────────────────────────────────────

BASE_DECORATOR = '@asset(partitions={"region": partitions(["us", "eu"])}, serializer="json")'

SAME_RESULT = {
    "description": '@asset(partitions={"region": partitions(["us", "eu"])}, serializer="json", description="Sales")',
    "tags": '@asset(partitions={"region": partitions(["us", "eu"])}, serializer="json", tags={"team": "data"})',
    "retries": '@asset(partitions={"region": partitions(["us", "eu"])}, serializer="json", retries=3, retry_backoff=0.5)',
    "timeout": '@asset(partitions={"region": partitions(["us", "eu"])}, serializer="json", timeout_seconds=60)',
    "freshness": '@asset(partitions={"region": partitions(["us", "eu"])}, serializer="json", freshness=Always)',
    "whitespace": '@asset(\n    partitions = { "region" : partitions( [ "us" , "eu" ] ) } ,\n    serializer = "json" ,\n)',
    "comment": '@asset(  # sales, one step per region\n    partitions={"region": partitions(["us", "eu"])},  # the regions\n    serializer="json",\n)',
    "quotes": "@asset(partitions={'region': partitions(['us', 'eu'])}, serializer='json')",
    "trailing_comma": '@asset(partitions={"region": partitions(["us", "eu",]),}, serializer="json",)',
    "keyword_order": '@asset(serializer="json", partitions={"region": partitions(["us", "eu"])})',
    "constant_in_description": '@asset(partitions={"region": partitions(["us", "eu"])}, serializer="json", description=SUMMARY)',
}


@pytest.mark.parametrize("edit", SAME_RESULT)
def test_an_edit_that_cannot_change_the_result_runs_nothing(project, edit):
    head = f"{IMPORTS}from barca import Always\n\nSUMMARY = 'Sales by region'\n\n\n"
    write(project, head + BASE_DECORATOR + SALES + CONSUMERS)
    assert get(project)["steps_executed"] == 6

    write(project, head + SAME_RESULT[edit] + SALES + CONSUMERS)
    assert ran(get(project)) == {}


def test_a_constant_used_only_in_an_argument_that_does_not_count_is_not_followed(project):
    def source(summary: str, team: str) -> str:
        return (
            f"{IMPORTS}\nSUMMARY = {summary!r}\nTAGS = {{'team': {team!r}}}\n\n\n"
            '@asset(partitions={"region": partitions(["us", "eu"])}, description=SUMMARY, tags=TAGS)'
            f"{SALES}{CONSUMERS}"
        )

    write(project, source("Sales", "data"))
    get(project)
    write(project, source("Sales by region", "finance"))
    assert ran(get(project)) == {}


def test_a_sensor_s_schedule_and_description_do_not_rerun_its_consumers(project):
    def source(sensor_arguments: str) -> str:
        return (
            "from barca import Manual, Schedule, asset, sensor\n\n\n"
            f"@sensor({sensor_arguments})\n"
            "def version() -> tuple[bool, str]:\n    return True, 'v1'\n\n\n"
            '@asset(inputs={"v": version})\n'
            "def data(v: str) -> dict:\n    return {'v': v}\n"
        )

    write(project, source(""))
    assert ran(get(project, "data")) == {"version": True, "data": True}
    for arguments in (
        'description="The data version"',
        "freshness=Manual",
        'freshness=Schedule("*/5 * * * *"), retries=2',
    ):
        write(project, source(arguments))
        # The sensor always runs. It returns the same value, and its own code has not changed.
        assert ran(get(project, "data")) == {"version": True}, arguments


def test_editing_a_sensor_s_code_reruns_its_consumers_even_with_the_same_value(project):
    """The sensor's run hash covers its code and is part of its consumers' run hashes, next to
    the hash of its output (`barca docs cache`, "External data that changes in place")."""

    def source(body: str) -> str:
        return (
            "from barca import asset, sensor\n\n\n"
            f"@sensor()\ndef version() -> tuple[bool, str]:\n    {body}\n\n\n"
            '@asset(inputs={"v": version})\n'
            "def data(v: str) -> dict:\n    return {'v': v}\n"
        )

    write(project, source("return True, 'v1'"))
    get(project, "data")
    assert ran(get(project, "data")) == {"version": True}
    write(project, source("value = 'v1'\n    return True, value"))
    assert ran(get(project, "data")) == {"version": True, "data": True}


# ─── Arguments that count ─────────────────────────────────────────────────────

CHAIN = """

@asset(inputs={"x": middle})
def end(x) -> dict:
    return {"x": x}
"""


def chain(middle_decorators: str, prelude: str = "") -> str:
    return (
        "import functools\n\nfrom barca import asset, sink\n\n"
        f"{prelude}\n\n"
        "@asset()\ndef a() -> int:\n    return 1\n\n\n"
        "@asset()\ndef b() -> int:\n    return 2\n\n\n"
        f"{middle_decorators}\n"
        "def middle(x: int = 0) -> dict:\n    return {'x': x}\n"
        f"{CHAIN}"
    )


CHANGES_THE_RESULT = {
    "serializer": ('@asset(inputs={"x": a})', '@asset(inputs={"x": a}, serializer="pickle")'),
    "inputs_upstream": ('@asset(inputs={"x": a})', '@asset(inputs={"x": b})'),
    "sink_added": ('@asset(inputs={"x": a})', '@asset(inputs={"x": a})\n@sink("out/middle.json")'),
    "sink_edited": (
        '@asset(inputs={"x": a})\n@sink("out/middle.json")',
        '@asset(inputs={"x": a})\n@sink("out/other.json")',
    ),
    "other_decorator": ('@asset(inputs={"x": a})', '@asset(inputs={"x": a})\n@functools.cache'),
    "other_decorator_argument": (
        '@asset(inputs={"x": a})\n@functools.lru_cache(maxsize=1)',
        '@asset(inputs={"x": a})\n@functools.lru_cache(maxsize=2)',
    ),
}


@pytest.mark.parametrize("edit", CHANGES_THE_RESULT)
def test_an_edit_that_can_change_the_result_reruns_the_step_and_its_downstream(project, edit):
    before, after = CHANGES_THE_RESULT[edit]
    write(project, chain(before))
    get(project, "end")
    assert get(project, "end")["steps_executed"] == 0

    write(project, chain(after))
    again = ran(get(project, "end"))
    assert again.get("middle") is True and again.get("end") is True, again
    assert "a" not in again  # upstream of the edit: cached
    assert get(project, "end")["steps_executed"] == 0


def test_an_unknown_argument_edit_is_rejected_before_using_the_cache(project):
    before = chain('@asset(inputs={"x": a})')
    write(project, before)
    get(project, "end")
    assert get(project, "end")["steps_executed"] == 0
    write(project, chain('@asset(inputs={"x": a}, mode="fast")'))
    proc = subprocess.run(
        [_find_binary(), "get", "end", "pipeline.py"], cwd=project,
        capture_output=True, text=True,
    )
    assert proc.returncode == 2
    assert proc.stdout == ""
    error = json.loads(proc.stderr.strip().splitlines()[-1])
    assert error["kind"] == "usage"
    assert "`mode` is not an argument of @asset" in error["error"]
    write(project, before)
    assert get(project, "end")["steps_executed"] == 0


def test_a_new_sink_is_written(project):
    write(project, chain('@asset(inputs={"x": a})'))
    get(project, "end")
    write(project, chain('@asset(inputs={"x": a})\n@sink("out/middle.json")'))
    get(project, "end")
    assert json.loads((project / "out" / "middle.json").read_text()) == {"x": 1}


def test_a_constant_used_only_in_a_counted_argument_is_followed(project):
    def source(size: int, unrelated: int) -> str:
        return chain(
            '@asset(inputs={"x": a})\n@functools.lru_cache(maxsize=SIZE)',
            prelude=f"SIZE = {size}\nUNRELATED = {unrelated}\n",
        )

    write(project, source(1, 0))
    get(project, "end")
    write(project, source(1, 5))  # a constant nothing uses
    assert ran(get(project, "end")) == {}
    write(project, source(2, 5))  # the constant the decorator's argument uses
    assert ran(get(project, "end")) == {"middle": True, "end": True}


def test_a_wrapper_defined_in_the_project_is_followed(project):
    def wrappers(bonus: int) -> str:
        return (
            "import functools\n\n\n"
            "def plus(fn):\n"
            "    @functools.wraps(fn)\n"
            "    def inner(*args, **kwargs):\n"
            f"        return {{'x': fn(*args, **kwargs)['x'] + {bonus}}}\n"
            "    return inner\n\n\n"
            "def unused():\n    return 0\n"
        )

    source = chain('@asset(inputs={"x": a})\n@plus', prelude="from wrappers import plus\n")
    write(project, source)
    (project / "wrappers.py").write_text(wrappers(10))
    assert get(project, "end")["final_output"] == {"x": {"x": 11}}

    (project / "wrappers.py").write_text(wrappers(10).replace("return 0", "return 1"))
    assert ran(get(project, "end")) == {}  # a function of the module the step does not use

    (project / "wrappers.py").write_text(wrappers(20))
    edited = get(project, "end")
    assert ran(edited) == {"middle": True, "end": True}
    assert edited["final_output"] == {"x": {"x": 21}}


def test_renaming_a_partition_dimension_reruns_every_key(project):
    def source(dimension: str) -> str:
        return (
            f"{IMPORTS}\n\n"
            f'@asset(partitions={{"{dimension}": partitions(["us", "eu"])}})\n'
            f"def sales({dimension}: str) -> dict:\n    return {{'key': {dimension}}}\n"
        )

    write(project, source("region"))
    get(project, "sales")
    write(project, source("area"))
    assert ran(get(project, "sales")) == {"sales": ["eu", "us"]}


def test_renaming_a_node_runs_it_once_and_nothing_downstream(project):
    """`name=` is the node's id, not part of its definition: the renamed step has no result
    under its new id and runs, with the run hash it had before, so its consumer stays cached."""

    def source(arguments: str) -> str:
        return (
            "from barca import asset\n\n\n"
            f"@asset({arguments})\ndef a() -> int:\n    return 1\n\n\n"
            '@asset(inputs={"x": a})\n'
            "def end(x: int) -> int:\n    return x + 1\n"
        )

    write(project, source(""))
    first = {s["id"]: s for s in get(project, "end")["steps"]}
    write(project, source('name="renamed"'))
    steps = {s["id"]: s for s in get(project, "end")["steps"]}
    assert steps["renamed"]["status"] == "ran"
    assert steps["renamed"]["run_hash"] == first["pipeline.py:a"]["run_hash"]
    assert steps["pipeline.py:end"]["status"] == "cached"


def test_wrapper_keyword_order_is_part_of_the_result(project):
    """A custom decorator may observe Python's insertion order of keyword arguments."""
    def source(arguments: str) -> str:
        return (
            "from barca import asset\n\n"
            "def ordered(**options):\n"
            "    def decorate(fn):\n"
            "        def wrapped():\n"
            "            return list(options)\n"
            "        return wrapped\n"
            "    return decorate\n\n"
            f"@asset()\n@ordered({arguments})\n"
            "def result():\n    return []\n"
        )
    write(project, source("first=1, second=2"))
    assert get(project, "result")["final_output"] == ["first", "second"]
    assert get(project, "result")["steps_executed"] == 0
    write(project, source("second=2, first=1"))
    changed = get(project, "result")
    assert changed["steps_executed"] == 1
    assert changed["final_output"] == ["second", "first"]
