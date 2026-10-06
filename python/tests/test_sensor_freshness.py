"""A `@sensor`'s output invalidates the assets that consume it (#151, #183).

The external-freshness pattern: a sensor returns a blob's etag / mtime, a bronze asset depends on
the sensor, so when the blob changes in place the bronze asset's run hash changes and it
re-materializes. A content hash of the sensor's serialized output is folded into each direct
consumer's run hash; everything downstream follows through the consumer's new run hash.

Sensors run in an earlier phase than their consumers, so a consumer's cache decision always sees
this run's sensor output. `--dry-run` and `barca status` execute nothing: they predict from the
sensor's last recorded output, and report a consumer as `unknown` when there is none.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

PIPELINE = """
from pathlib import Path
from barca import asset, partitions, sensor, task


@sensor()
def blob_etag() -> tuple[bool, str]:
    etag = Path("etag.txt").read_text().strip()   # stands in for an Azure/S3 blob's etag
    return True, etag


@asset(inputs={"etag": blob_etag})
def bronze(etag: str) -> dict:
    return {"etag": etag}


@asset(inputs={"b": bronze})
def silver(b: dict) -> dict:
    return {"etag": b["etag"], "layer": "silver"}


@asset()
def unrelated() -> dict:
    return {"v": 1}


@asset(inputs={"etag": blob_etag}, partitions={"region": partitions(["us", "eu"])})
def by_region(region: str, etag: str) -> dict:
    return {"region": region, "etag": etag}


@task(inputs={"s": silver, "u": unrelated})
def report(s: dict, u: dict) -> dict:
    return {"etag": s["etag"], "u": u["v"]}
"""


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "etag.txt").write_text("v1")
    return tmp_path


def run(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=project, env=os.environ, capture_output=True, text=True
    )


def barca(project: Path, *args: str) -> dict:
    proc = run(project, *args)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def steps(result: dict) -> dict:
    return {s["id"].split(":")[-1]: s for s in result["steps"]}


def set_etag(project: Path, value: str) -> None:
    (project / "etag.txt").write_text(value)


def test_the_sensor_runs_every_time_and_its_own_run_hash_depends_only_on_its_code(project):
    first = steps(barca(project, "get", "bronze", "pipeline.py"))
    set_etag(project, "v2")
    second = steps(barca(project, "get", "bronze", "pipeline.py"))
    assert first["blob_etag"]["status"] == "ran" and second["blob_etag"]["status"] == "ran"
    assert first["blob_etag"]["run_hash"] == second["blob_etag"]["run_hash"]


def test_a_changed_sensor_output_re_materializes_the_downstream_asset(project):
    barca(project, "get", "bronze", "pipeline.py")
    set_etag(project, "v2")
    assert barca(project, "get", "bronze", "pipeline.py")["final_output"] == {"etag": "v2"}


def test_a_changed_output_gives_the_consumer_a_new_run_hash_and_it_runs(project):
    first = barca(project, "get", "bronze", "pipeline.py")
    assert first["final_output"] == {"etag": "v1"}
    set_etag(project, "v2")
    second = barca(project, "get", "bronze", "pipeline.py")
    bronze = steps(second)["bronze"]
    assert bronze["status"] == "ran"
    assert bronze["reason"] == "not_materialized"
    assert bronze["run_hash"] != steps(first)["bronze"]["run_hash"]
    assert second["final_output"] == {"etag": "v2"}


def test_the_same_output_keeps_the_consumer_cached(project):
    first = barca(project, "get", "bronze", "pipeline.py")
    second = barca(project, "get", "bronze", "pipeline.py")
    assert steps(second)["blob_etag"]["status"] == "ran"
    assert steps(second)["bronze"]["status"] == "cached"
    assert steps(second)["bronze"]["run_hash"] == steps(first)["bronze"]["run_hash"]
    assert second["steps_executed"] == 1, "only the sensor ran"


def test_returning_to_an_earlier_output_serves_the_earlier_materialization(project):
    first = barca(project, "get", "bronze", "pipeline.py")
    set_etag(project, "v2")
    barca(project, "get", "bronze", "pipeline.py")
    set_etag(project, "v1")
    third = barca(project, "get", "bronze", "pipeline.py")
    assert steps(third)["bronze"]["status"] == "cached"
    assert steps(third)["bronze"]["run_hash"] == steps(first)["bronze"]["run_hash"]
    assert third["final_output"] == {"etag": "v1"}


def test_downstream_of_the_consumer_changes_transitively(project):
    barca(project, "get", "silver", "pipeline.py")
    same = steps(barca(project, "get", "silver", "pipeline.py"))
    assert same["bronze"]["status"] == "cached" and same["silver"]["status"] == "cached"

    set_etag(project, "v2")
    result = barca(project, "get", "silver", "pipeline.py")
    changed = steps(result)
    assert changed["bronze"]["status"] == "ran"
    assert changed["silver"]["status"] == "ran"
    assert result["final_output"] == {"etag": "v2", "layer": "silver"}


def test_assets_that_do_not_read_the_sensor_stay_cached(project):
    barca(project, "run", "report", "pipeline.py")
    set_etag(project, "v2")
    result = barca(project, "run", "report", "pipeline.py")
    by = steps(result)
    assert by["unrelated"]["status"] == "cached"
    assert by["bronze"]["status"] == "ran" and by["silver"]["status"] == "ran"
    assert result["final_output"] == {"etag": "v2", "u": 1}


def test_a_partitioned_consumer_re_runs_every_key_when_the_output_changes(project):
    barca(project, "get", "by_region", "pipeline.py")
    cached = steps(barca(project, "get", "by_region", "pipeline.py"))["by_region"]
    assert cached["status"] == "cached"
    assert cached["partitions"]["cached"] == 2

    set_etag(project, "v2")
    changed = steps(barca(project, "get", "by_region", "pipeline.py"))["by_region"]
    assert changed["status"] == "ran"
    assert changed["partitions"]["will_run"] == 2
    arts = sorted((project / ".barca" / "artifacts").glob("*by_region_region_us/*.json"))
    assert len(arts) == 2, "one artifact per etag for the `us` key"
    values = sorted(json.loads(a.read_text())["etag"] for a in arts)
    assert values == ["v1", "v2"]


def test_refresh_still_forces_the_consumer_and_its_downstream(project):
    barca(project, "run", "report", "pipeline.py")
    result = barca(project, "run", "report", "pipeline.py", "--refresh", "bronze")
    by = steps(result)
    assert by["bronze"]["status"] == "ran" and by["bronze"]["reason"] == "refresh"
    assert by["silver"]["reason"] == "refresh_cascade"
    assert by["unrelated"]["status"] == "cached"

    got = steps(barca(project, "get", "silver", "pipeline.py", "--refresh", "bronze"))
    assert got["bronze"]["reason"] == "refresh" and got["silver"]["reason"] == "refresh_cascade"

    everything = steps(barca(project, "run", "report", "pipeline.py", "--refresh-all"))
    assert everything["bronze"]["reason"] == "refresh_all"
    assert everything["unrelated"]["reason"] == "refresh_all"


def test_dry_run_before_the_sensor_ever_ran_reports_its_consumers_as_unknown(project):
    result = barca(project, "get", "silver", "pipeline.py", "--dry-run")
    by = steps(result)
    assert by["blob_etag"]["action"] == "run" and by["blob_etag"]["reason"] == "sensor"
    assert by["bronze"]["action"] == "unknown"
    assert by["bronze"]["reason"] == "sensor_output_unknown"
    assert "blob_etag" in by["bronze"]["detail"]
    assert by["silver"]["action"] == "unknown"
    assert by["silver"]["reason"] == "sensor_output_unknown"
    assert "bronze" in by["silver"]["detail"]
    assert result["summary"] == {"will_run": 1, "cached": 0, "unknown": 2}
    assert not (project / ".barca").exists(), "a dry run writes nothing"


def test_dry_run_predicts_from_the_last_recorded_output(project):
    barca(project, "get", "silver", "pipeline.py")
    set_etag(project, "v2")  # the dry run does not run the sensor, so it cannot see this
    dry = steps(barca(project, "get", "silver", "pipeline.py", "--dry-run"))
    assert dry["bronze"]["action"] == "cached"
    assert (
        "assumes sensor 'blob_etag' returns the same value as its last run"
        in (dry["bronze"]["detail"])
    )
    assert dry["silver"]["action"] == "cached"

    # The real run sees the new etag.
    real = steps(barca(project, "get", "silver", "pipeline.py"))
    assert real["bronze"]["status"] == "ran"

    # Observing the sensor on its own (as a scheduled sensor does) records its output, so the
    # next prediction reflects it.
    set_etag(project, "v3")
    barca(project, "get", "blob_etag", "pipeline.py")
    dry = steps(barca(project, "get", "silver", "pipeline.py", "--dry-run"))
    assert dry["bronze"]["action"] == "run"
    assert dry["bronze"]["reason"] == "not_materialized"
    assert "assumes sensor 'blob_etag'" in dry["bronze"]["detail"]
    assert dry["silver"]["action"] == "run"


def test_dry_run_will_run_matches_the_real_run(project):
    barca(project, "get", "silver", "pipeline.py")
    for etag in ("v1", "v2"):
        set_etag(project, etag)
        barca(project, "get", "blob_etag", "pipeline.py")
        dry = barca(project, "get", "silver", "pipeline.py", "--dry-run")
        real = barca(project, "get", "silver", "pipeline.py")
        assert dry["summary"]["will_run"] == real["steps_executed"], etag


def status(project: Path) -> dict:
    proc = run(project, "status", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    return {n["name"]: n for n in json.loads(proc.stdout)["nodes"]}


def test_status_follows_the_same_rule(project):
    by = status(project)
    assert by["bronze"]["cache"]["state"] == "unknown"
    assert by["bronze"]["cache"]["reason"] == "sensor_output_unknown"
    assert by["silver"]["cache"]["state"] == "unknown"

    barca(project, "get", "silver", "pipeline.py")
    by = status(project)
    assert by["blob_etag"]["cache"]["state"] == "always_runs"
    assert by["bronze"]["cache"]["state"] == "cached"
    assert "assumes sensor 'blob_etag'" in by["bronze"]["cache"]["detail"]

    # A sensor observed on its own (as a scheduled sensor is) records its new output, so status
    # shows its consumers as stale before they run.
    set_etag(project, "v2")
    barca(project, "get", "blob_etag", "pipeline.py")
    by = status(project)
    assert by["bronze"]["cache"]["state"] == "stale"
    assert by["silver"]["cache"]["state"] == "stale"


def test_a_sensor_whose_output_changes_every_run_re_runs_its_consumers_every_time(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "import time\n"
        "from barca import asset, sensor\n\n\n"
        "@sensor()\n"
        "def now() -> tuple[bool, float]:\n"
        "    return True, time.time()\n\n\n"
        "@asset(inputs={'t': now})\n"
        "def stamp(t: float) -> float:\n"
        "    return t\n"
    )
    barca(tmp_path, "get", "stamp", "pipeline.py")
    again = steps(barca(tmp_path, "get", "stamp", "pipeline.py"))
    assert again["stamp"]["status"] == "ran"


# Run hashes of a sensor-free pipeline, pinned from barca 0.11.0 (before sensor outputs were
# hashed). Pipelines without sensors must keep exactly these hashes, or every existing cache is
# invalidated on upgrade.
SENSOR_FREE = """
from barca import asset, partitions, task


@asset()
def raw() -> dict:
    return {"v": 1}


@asset(inputs={"raw": raw})
def clean(raw: dict) -> dict:
    return {"v": raw["v"] * 2}


@asset(inputs={"clean": clean}, partitions={"region": partitions(["us", "eu"])})
def by_region(region: str, clean: dict) -> dict:
    return {"region": region, "v": clean["v"]}


@task(inputs={"clean": clean})
def report(clean: dict) -> dict:
    return clean
"""

PINNED_0_11 = {
    "raw": "6978869c8574f25c01e807d2efd97cb11cd062482e0ace8c5eb0cb1e6b40c1f0",
    "clean": "f986a8ebb90d71684e85fc317f48ed1baec59fef4f10c6b01722fd052481bf32",
    "report": "4f70c9c67a6a6f31c61e2cc5563f46f5ea71aed4c72bb8eb6cdc713f6cd15f7e",
    "by_region[us]": "efe09938adf906a64c345891955b9b7463ec41ce533265c3f903c34e512d5cb4",
    "by_region[eu]": "67f5a43fb5baa9d1d187cb5a12ab2f2cc1f9b908cbf6e0739d1297cbfb724831",
}


def test_sensor_free_pipelines_keep_their_0_11_run_hashes(tmp_path):
    (tmp_path / "pipeline.py").write_text(SENSOR_FREE)
    run_steps = steps(barca(tmp_path, "run", "report", "pipeline.py"))
    barca(tmp_path, "get", "by_region", "pipeline.py")
    got = {name: run_steps[name]["run_hash"] for name in ("raw", "clean", "report")}
    for key in ("us", "eu"):
        (art,) = (tmp_path / ".barca" / "artifacts").glob(f"*by_region_region_{key}/*.json")
        got[f"by_region[{key}]"] = art.stem
    assert got == PINNED_0_11
