"""Keyed partition sources: `partitions_from(source)` where `source` returns a dict.

The dict's keys are the partition keys; each key's call receives that key's value, and each
key's run hash folds in its own value only. So one sensor call that returns
`{retailer: version}` decides both which retailers exist and which changed: only a changed or
new key runs, through a chain of `partitions_from` assets.

Up to 0.29.5 an unpartitioned source that returned a dict was not read for keys: the consumer
ran once with no arguments and failed with `TypeError: keyed() missing 2 required positional
arguments`.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

from . import served

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
import json
import pathlib

from barca import asset, collect, partitions_from, sensor

VERSIONS = pathlib.Path(__file__).with_name("versions.json")
CALLS = pathlib.Path(__file__).with_name("calls.txt")


def _called(what):
    with CALLS.open("a") as f:
        f.write(what + "\\n")


@sensor()
def versions() -> tuple[bool, dict]:
    return True, json.loads(VERSIONS.read_text())


@asset(partitions={"retailer": partitions_from(versions)})
def cleaned(retailer: str, versions: str) -> dict:
    _called(f"cleaned {retailer}")
    return {"retailer": retailer, "version": versions}


@asset(partitions={"retailer": partitions_from(cleaned)})
def scored(retailer: str, cleaned: dict) -> dict:
    _called(f"scored {retailer}")
    return {**cleaned, "score": len(retailer)}


@asset(inputs={"rows": collect(scored)})
def summary(rows: list) -> list:
    _called("summary")
    return sorted([r["retailer"], r["version"]] for r in rows)


# The value under another name: the `inputs=` key that names the source.
@asset(inputs={"v": versions}, partitions={"retailer": partitions_from(versions)})
def renamed(retailer: str, v: str) -> str:
    return f"{retailer}={v}"


# A function that does not take the value gets the keys only.
@asset(partitions={"retailer": partitions_from(versions)})
def keys_only(retailer: str) -> str:
    return retailer
"""


def env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}


def run(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=project, env=env(), capture_output=True, text=True
    )


def get(project: Path, target: str, *flags: str) -> dict:
    proc = run(project, "get", target, "pipeline.py", "--json", *flags)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def value(project: Path, target: str) -> object:
    proc = run(project, "get", target, "pipeline.py", "-o", "value")
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def calls(project: Path) -> list[str]:
    path = project / "calls.txt"
    lines = path.read_text().splitlines() if path.exists() else []
    path.write_text("")
    return sorted(lines)


def set_versions(project: Path, versions: dict) -> None:
    (project / "versions.json").write_text(json.dumps(versions))


def step(result: dict, name: str) -> dict:
    return next(s for s in result["steps"] if s["id"] == f"pipeline.py:{name}")


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    set_versions(tmp_path, {"r1": "a", "r2": "a", "r3": "a"})
    return tmp_path


def test_only_the_key_whose_value_changed_runs_through_the_chain(project):
    get(project, "summary")
    assert calls(project) == [
        "cleaned r1",
        "cleaned r2",
        "cleaned r3",
        "scored r1",
        "scored r2",
        "scored r3",
        "summary",
    ]
    assert value(project, "cleaned") == {
        "retailer=r1": {"retailer": "r1", "version": "a"},
        "retailer=r2": {"retailer": "r2", "version": "a"},
        "retailer=r3": {"retailer": "r3", "version": "a"},
    }
    calls(project)

    # Nothing changed: the sensor runs, every key is served from cache.
    res = get(project, "summary")
    assert calls(project) == []
    assert res["steps_executed"] == 1

    # One value changed: that key runs, and the fan-in over it.
    set_versions(project, {"r1": "a", "r2": "b", "r3": "a"})
    res = get(project, "summary")
    assert calls(project) == ["cleaned r2", "scored r2", "summary"]
    assert res["steps_executed"] == 4  # the sensor, two keys, the fan-in
    assert step(res, "cleaned")["partitions"]["will_run_keys"] == ["retailer=r2"]
    assert value(project, "summary") == [["r1", "a"], ["r2", "b"], ["r3", "a"]]

    # Changing the value back is a cache hit again: each key is addressed by its own value.
    set_versions(project, {"r1": "a", "r2": "a", "r3": "a"})
    get(project, "summary")
    assert calls(project) == []


def test_a_new_key_runs_alone_and_a_removed_key_runs_nothing(project):
    get(project, "summary")
    calls(project)

    set_versions(project, {"r1": "a", "r2": "a", "r3": "a", "r4": "z"})
    get(project, "summary")
    assert calls(project) == ["cleaned r4", "scored r4", "summary"]
    assert value(project, "summary")[-1] == ["r4", "z"]

    set_versions(project, {"r1": "a", "r3": "a", "r4": "z"})
    res = get(project, "scored")
    assert calls(project) == []
    assert sorted(res["final_output"]) == ["retailer=r1", "retailer=r3", "retailer=r4"]
    # The fan-in runs again: its set of inputs changed.
    get(project, "summary")
    assert calls(project) == ["summary"]
    assert value(project, "summary") == [["r1", "a"], ["r3", "a"], ["r4", "z"]]


def test_the_value_is_passed_under_the_inputs_name_and_only_when_taken(project):
    assert value(project, "renamed") == {
        "retailer=r1": "r1=a",
        "retailer=r2": "r2=a",
        "retailer=r3": "r3=a",
    }
    assert value(project, "keys_only") == {
        "retailer=r1": "r1",
        "retailer=r2": "r2",
        "retailer=r3": "r3",
    }
    # Named in inputs=, the key still folds in its own value only.
    set_versions(project, {"r1": "a", "r2": "b", "r3": "a"})
    res = get(project, "renamed")
    assert step(res, "renamed")["partitions"]["will_run_keys"] == ["retailer=r2"]


def test_the_dry_run_and_status_say_which_key_changed_and_why(project):
    get(project, "scored")
    set_versions(project, {"r1": "a", "r2": "b", "r3": "a", "r9": "new"})
    # The sensor's new value is recorded by running it alone (a scheduled sensor does this).
    assert run(project, "get", "versions", "pipeline.py").returncode == 0

    proc = run(project, "get", "scored", "pipeline.py", "--dry-run", "--json")
    assert proc.returncode == 0, proc.stderr
    dry = json.loads(proc.stdout)
    cleaned = step(dry, "cleaned")
    assert cleaned["action"] == "partial"
    assert cleaned["partitions"]["will_run_keys"] == ["retailer=r2", "retailer=r9"]
    # r2 ran before with another value; r9 is new and has nothing to compare with.
    assert cleaned["partitions"]["changed"] == {"retailer=r2": ["value pipeline.py:versions"]}
    assert cleaned["changed"] == ["value pipeline.py:versions"]
    assert "assumes sensor 'versions' returns the same value" in cleaned["detail"]
    scored = step(dry, "scored")
    assert scored["partitions"]["changed"] == {"retailer=r2": ["input pipeline.py:cleaned"]}

    proc = run(project, "status", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    nodes = {n["id"]: n for n in json.loads(proc.stdout)["nodes"]}
    part = nodes["pipeline.py:cleaned"]["partitions"]
    assert part["missing_keys"] == ["retailer=r2", "retailer=r9"]
    assert part["changed"] == {"retailer=r2": ["value pipeline.py:versions"]}


def test_collect_reruns_when_its_inputs_change_and_only_then(project):
    get(project, "summary")
    calls(project)
    get(project, "summary")
    assert calls(project) == []
    set_versions(project, {"r1": "a", "r2": "a", "r3": "c"})
    get(project, "summary")
    assert calls(project) == ["cleaned r3", "scored r3", "summary"]
    assert value(project, "summary") == [["r1", "a"], ["r2", "a"], ["r3", "c"]]


def test_integer_keys_are_their_strings(project):
    (project / "pipeline.py").write_text(
        "from barca import asset, partitions_from, sensor\n\n"
        "@sensor()\ndef ids() -> tuple[bool, dict]:\n    return True, {1: 'a', 20: 'b'}\n\n"
        "@asset(partitions={'id': partitions_from(ids)})\n"
        "def per(id: str, ids: str) -> list:\n    return [id, ids]\n"
    )
    assert value(project, "per") == {"id=1": ["1", "a"], "id=20": ["20", "b"]}


@pytest.mark.parametrize(
    ("returned", "words"),
    [
        (
            "{('a', 1): 'x'}",
            "returned a dict with the key ('a', 1) (tuple); keys must be strings or integers",
        ),
        ("{True: 'x'}", "returned a dict with the key True (bool)"),
        ("{1: 'x', '1': 'y'}", "returned the keys 1 and '1', which are both the partition key '1'"),
        ("{'ok': 1, 'bad': {1, 2}}", "the value for key 'bad' (set) cannot be written as JSON"),
        ("{'nan': float('nan')}", "the value for key 'nan' (float) cannot be written as JSON"),
        (
            "'r1,r2'",
            "partitions_from source 'source' returned str; it must return a list of keys or a dict of key -> value",
        ),
        ("None", "returned NoneType; it must return a list of keys or a dict"),
    ],
)
def test_a_source_barca_cannot_read_keys_from_fails_naming_why(project, returned, words):
    (project / "pipeline.py").write_text(
        "from barca import asset, partitions_from, sensor\n\n"
        f"@sensor()\ndef source() -> tuple:\n    return True, {returned}\n\n"
        "@asset(partitions={'k': partitions_from(source)})\n"
        "def per(k: str, source) -> str:\n    return k\n"
    )
    proc = run(project, "get", "per", "pipeline.py", "--json")
    assert proc.returncode == 1, proc.stderr
    assert words in proc.stderr, proc.stderr
    assert "missing 2 required positional arguments" not in proc.stderr


def test_an_unpartitioned_sensor_returning_a_dict_no_longer_crashes(project):
    """The report: `@sensor()` returning `(True, {...})` into `partitions_from`."""
    (project / "pipeline.py").write_text(
        "import json, pathlib\n"
        "from barca import asset, sensor, partitions_from\n\n"
        "VERS = pathlib.Path(__file__).with_name('versions.json')\n\n"
        "@sensor()\ndef all_versions() -> tuple[bool, dict]:\n"
        "    return True, json.loads(VERS.read_text())\n\n"
        "@asset(partitions={'retailer': partitions_from(all_versions)})\n"
        "def keyed(retailer: str, all_versions: dict) -> str:\n"
        "    return f'{retailer}:{all_versions}'\n"
    )
    assert value(project, "keyed") == {
        "retailer=r1": "r1:a",
        "retailer=r2": "r2:a",
        "retailer=r3": "r3:a",
    }


def test_an_empty_dict_has_no_keys(project):
    set_versions(project, {})
    res = get(project, "summary")
    assert calls(project) == ["summary"]
    assert res["steps_executed"] == 2  # the sensor and the fan-in, over no key
    assert value(project, "summary") == []


def test_the_list_form_is_unchanged(project):
    (project / "pipeline.py").write_text(
        "from barca import asset, partitions_from\n\n"
        "@asset()\ndef keys() -> list:\n    return ['p', 'q']\n\n"
        "@asset(partitions={'k': partitions_from(keys)})\n"
        "def part(k: str) -> str:\n    return k.upper()\n"
    )
    assert value(project, "part") == {"k=p": "P", "k=q": "Q"}


SCHEDULED = """
import json
import pathlib

from barca import Schedule, asset, partitions_from, sensor

VERSIONS = pathlib.Path(__file__).with_name("versions.json")
CALLS = pathlib.Path(__file__).with_name("calls.txt")


def _called(what):
    with CALLS.open("a") as f:
        f.write(what + "\\n")


@sensor(freshness=Schedule("* * * * * *"))
def versions() -> tuple[bool, dict]:
    return True, json.loads(VERSIONS.read_text())


@asset(partitions={"retailer": partitions_from(versions)})
def cleaned(retailer: str, versions: str) -> dict:
    _called(f"cleaned {retailer}")
    return {"retailer": retailer, "version": versions}


@asset(partitions={"retailer": partitions_from(cleaned)})
def scored(retailer: str, cleaned: dict) -> dict:
    _called(f"scored {retailer}")
    return {**cleaned, "score": len(retailer)}
"""


def keys_of(rows: dict[str, int], node: str) -> int:
    """Successful materializations of `node`'s keys (`scored[retailer=r1]`, ...)."""
    return sum(n for k, n in rows.items() if k.startswith(f"{node}["))


def test_a_scheduled_tick_runs_only_the_changed_keys(tmp_path):
    (tmp_path / "pipeline.py").write_text(SCHEDULED)
    set_versions(tmp_path, {f"r{i}": "a" for i in range(1, 6)})
    with served.Serve(tmp_path, env=env()) as serve:
        serve.wait("the first tick", lambda: keys_of(serve.rows(), "scored") >= 5)
        serve.settle(2)
        assert calls(tmp_path) == sorted(
            [f"cleaned r{i}" for i in range(1, 6)] + [f"scored r{i}" for i in range(1, 6)]
        )
        # Ticks with nothing changed compute nothing below the sensor.
        serve.settle(2)
        assert calls(tmp_path) == []
        set_versions(tmp_path, {**{f"r{i}": "a" for i in range(1, 6)}, "r2": "b"})
        serve.wait("the tick after the change", lambda: keys_of(serve.rows(), "scored") >= 6)
        serve.settle(2)
        assert calls(tmp_path) == ["cleaned r2", "scored r2"]
        # The run history of that tick: three steps ran (the sensor and one key of each
        # asset), the other eight keys were served from cache.
        runs = serve.get("/runs?limit=1000")["runs"]
        tick = next(r for r in runs if r.get("steps_executed") == 3)
        assert tick["steps_cached"] == 8, tick
        assert all(
            r.get("steps_executed") == 1
            for r in runs
            if r is not tick and r.get("steps_cached") == 10
        )


def test_datadog_reports_one_span_per_node_and_a_child_per_key_that_ran(project):
    """A tick over many cached keys is not noisy: one `barca.step` per node with the counts,
    and a `barca.partition` child only for the keys that ran."""
    from .test_telemetry_datadog import FakeAgent, datadog

    agent = FakeAgent()
    try:
        set_versions(project, {f"r{i}": "a" for i in range(20)})
        get(project, "scored")
        set_versions(project, {**{f"r{i}": "a" for i in range(20)}, "r7": "b"})
        proc = subprocess.run(
            [_find_binary(), "get", "scored", "pipeline.py", "--json"],
            cwd=project,
            env={**env(), **datadog(agent)},
            capture_output=True,
            text=True,
        )
        assert proc.returncode == 0, proc.stderr
        spans = agent.spans()
    finally:
        agent.close()
    (root,) = [s for s in spans if s["name"] == "barca.run"]
    assert root["metrics"]["barca.steps.executed"] == 3
    assert root["metrics"]["barca.steps.cached"] == 38
    steps = {s["resource"]: s for s in spans if s["name"] == "barca.step"}
    for node in ("cleaned", "scored"):
        assert steps[node]["meta"]["barca.outcome"] == "ran"
        assert steps[node]["metrics"]["barca.partitions.executed"] == 1
        assert steps[node]["metrics"]["barca.partitions.cached"] == 19
    keys = [s for s in spans if s["name"] == "barca.partition"]
    assert len(keys) == 2, keys
    assert all(k["meta"].get("barca.partition") == "retailer=r7" for k in keys), keys
