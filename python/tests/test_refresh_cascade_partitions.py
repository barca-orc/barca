"""A refresh reaches the keys of a `partitions_from` consumer, and what it does not reach (#799).

The shape of the report: two keyed upstream assets (`drivers`, `cutoffs`) whose data changes in
place, a `partitions_from` consumer of both (`monetized`, batched or not) and a fan-in (`app`).
A `--refresh` of either or both upstreams, by name or by key, recomputes the aligned keys of the
consumer and the fan-in, on the CLI and under `barca serve` (`max_runs` 1, an artifact store,
evicted local copies). What a refresh does not reach is a consumer outside the run's cone, and
`--batch` recomputes nothing on its own: `barca docs partitions`, "When a consumer's keys are
stale".
"""

import json
import shutil

import pytest

from .helpers import clean_env
from .test_asset_cutoff import calls, get, run, step

PIPELINE = """
from pathlib import Path
from barca import asset, collect, partitions, partitions_from

KEYS = ["x", "y", "z"]

def called(name):
    with Path("calls.txt").open("a") as f:
        f.write(name + "\\n")

def data():
    return Path("data.txt").read_text().strip()

@asset(partitions={"account": partitions(KEYS)})
def drivers(account: str) -> str:
    called("drivers " + account)
    return account + ":" + data()

@asset(partitions={"account": partitions(KEYS)})
def cutoffs(account: str) -> str:
    called("cutoffs " + account)
    return account + ":" + data()

CONSUMER

@asset(inputs={"rows": collect(monetized)})
def app(rows: list) -> list:
    called("app")
    return rows
"""

UNBATCHED = """
@asset(inputs={"cutoffs": cutoffs}, partitions={"account": partitions_from(drivers)})
def monetized(account: str, drivers: str, cutoffs: str) -> str:
    called("monetized " + account)
    return drivers + "|" + cutoffs
"""

BATCHED = """
@asset(inputs={"cutoffs": cutoffs}, partitions={"account": partitions_from(drivers)}, batch=2)
def monetized(account: list[str], drivers: dict, cutoffs: dict) -> dict:
    for k in account:
        called("monetized " + k)
    return {k: drivers[k] + "|" + cutoffs[k] for k in account}
"""

KEYS = ["x", "y", "z"]


def value(project):
    proc = run(project, "get", "app", "pipeline.py", "-o", "value", "--json")
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)["final_output"]


def rows(drivers: dict, cutoffs: dict) -> list[str]:
    return [f"{k}:{drivers[k]}|{k}:{cutoffs[k]}" for k in KEYS]


@pytest.fixture(params=[False, True], ids=["unbatched", "batched"])
def project(tmp_path, request):
    consumer = BATCHED if request.param else UNBATCHED
    (tmp_path / "pipeline.py").write_text(PIPELINE.replace("CONSUMER", consumer))
    (tmp_path / "data.txt").write_text("v1")
    get(tmp_path, "app")
    calls(tmp_path)
    (tmp_path / "data.txt").write_text("v2")
    return tmp_path


@pytest.mark.parametrize(
    ("refresh", "refreshed"),
    [
        ("drivers,cutoffs", {"drivers": KEYS, "cutoffs": KEYS}),
        ("drivers", {"drivers": KEYS}),
        ("drivers[account=x],cutoffs[account=x]", {"drivers": ["x"], "cutoffs": ["x"]}),
    ],
)
def test_cli_refresh_cascades_to_consumer_keys(project, refresh, refreshed):
    result = get(project, "app", "--refresh", refresh)
    touched = sorted({k for keys in refreshed.values() for k in keys})
    expected = [f"{node} {k}" for node, keys in refreshed.items() for k in keys]
    assert calls(project) == sorted(expected + [f"monetized {k}" for k in touched] + ["app"])
    assert step(result, "monetized")["reason"] == "refresh_cascade"
    assert step(result, "monetized")["partitions"]["cached"] == 3 - len(touched)
    version = {
        node: {k: "v2" if k in refreshed.get(node, []) else "v1" for k in KEYS}
        for node in ("drivers", "cutoffs")
    }
    assert value(project) == rows(version["drivers"], version["cutoffs"])


@pytest.mark.parametrize("evict", [False, True], ids=["local", "evicted"])
def test_server_refresh_cascades_to_consumer_keys(project, tmp_path_factory, evict):
    """The report's sequence on a server with an artifact store and `max_runs` 1: the upstream
    stage refreshed on its own first, then the umbrella target with `refresh` of both."""
    from . import served

    store = tmp_path_factory.mktemp("store")
    (project / "barca.toml").write_text(f'[remote]\nuri = "{store}"\n')
    with served.Serve(project, "--max-runs", "1", env=clean_env(BARCA_POOL_SIZE="2")) as serve:
        serve.ready()
        stage = serve.post(
            "/runs", {"command": "get", "targets": ["drivers", "cutoffs"], "refresh_all": True}
        )
        assert serve.finished(stage["run_id"])["status"] == "success"
        calls(project)
        if evict:
            for artifacts in (project / ".barca").rglob("artifacts"):
                if artifacts.is_dir():
                    shutil.rmtree(artifacts)
        (project / "data.txt").write_text("v3")
        request = {"command": "get", "targets": ["app"], "refresh": ["drivers", "cutoffs"]}
        answer = serve.post("/runs", request)
        assert serve.finished(answer["run_id"])["status"] == "success"
        ran = calls(project)
        assert [line for line in ran if line.startswith("monetized")] == [
            f"monetized {k}" for k in KEYS
        ]
    assert value(project) == rows(dict.fromkeys(KEYS, "v3"), dict.fromkeys(KEYS, "v3"))


def test_refresh_outside_the_cone_and_the_batch_override(project):
    """A refresh with another target leaves the consumer cached on the old bytes; `--batch`
    alone recomputes nothing; naming the consumer in `--refresh` recomputes its keys."""
    result = get(project, "drivers", "--refresh", "drivers")
    assert step(result, "drivers")["status"] == "ran"
    assert calls(project) == ["drivers x", "drivers y", "drivers z"]
    proc = run(project, "status", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    states = {n["id"]: n["cache"]["state"] for n in json.loads(proc.stdout)["nodes"]}
    assert states["pipeline.py:monetized"] == "cached"
    assert value(project) == rows(dict.fromkeys(KEYS, "v1"), dict.fromkeys(KEYS, "v1"))
    calls(project)
    flags = ["--refresh", "monetized"]
    if "batch=2" in (project / "pipeline.py").read_text():
        get(project, "app", "--batch", "monetized=all")
        assert calls(project) == []
        flags += ["--batch", "monetized=all"]
    result = get(project, "app", *flags)
    assert step(result, "monetized")["reason"] == "refresh"
    assert calls(project) == ["app", "monetized x", "monetized y", "monetized z"]
    assert value(project) == rows(dict.fromkeys(KEYS, "v2"), dict.fromkeys(KEYS, "v1"))
