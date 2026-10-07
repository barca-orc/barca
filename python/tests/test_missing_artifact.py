"""A cached result whose artifact is gone is computed again when something needs it (#252).

The rule (`barca docs cache`, "A cached result whose artifact is missing"):

- an artifact is needed when a step that is going to run reads it, when a `partitions_from`
  step is expanded from it, or when it is an output the command was asked for (its targets;
  with no target, every asset nothing else in the plan reads);
- a needed artifact that is neither on disk nor in the artifact store has its step run again,
  reported with reason `artifact_missing`;
- an artifact nothing reads is not looked at: a pruned intermediate stays cached.

`--dry-run` and `barca status` predict the same thing.
"""

import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import time
from pathlib import Path

import pytest

from barca.api import _find_binary

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
from barca import asset, task


@asset()
def model() -> dict:
    return {"v": 1}


@asset(inputs={"model": model})
def report(model: dict) -> dict:
    return {"from": model["v"]}


@task(inputs={"model": model})
def publish(model: dict) -> None:
    print("publish", model["v"])
"""

PARTITIONED = """
from barca import asset, collect, partitions


@asset(partitions={"k": partitions(["a", "b", "c"])})
def part(k: str) -> dict:
    return {"k": k}


@asset(inputs={"p": part}, partitions={"k": partitions(["a", "b", "c"])})
def double(p: dict, k: str) -> dict:
    return {"k": p["k"] * 2}


@asset(inputs={"parts": collect(part)})
def summary(parts: list) -> dict:
    return {"keys": sorted(p["k"] for p in parts)}
"""

DYNAMIC = """
from barca import asset, partitions_from


@asset()
def keys() -> list:
    return ["x", "y"]


@asset(partitions={"k": partitions_from(keys)})
def per_key(k: str) -> dict:
    return {"k": k}
"""

SCHEDULED = """
from barca import asset, task, Schedule


@asset()
def model() -> dict:
    return {"v": 1}


@task(freshness=Schedule("* * * * * *"), inputs={"model": model})
def publish(model: dict) -> None:
    print("publish", model["v"])
"""


def clean_env(**env: str) -> dict[str, str]:
    """This process's environment without anything that would point barca at a real store."""
    return {**{k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}, **env}


def cli(cwd: Path, *args: str, **env: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        env=clean_env(**env),
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )


def result(proc: subprocess.CompletedProcess) -> dict:
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def by_name(doc: dict, key: str) -> dict[str, str | None]:
    """`key` of every step (or status node), by function name."""
    return {s["id"].rsplit(":", 1)[1]: s.get(key) for s in doc["steps"]}


def statuses(proc: subprocess.CompletedProcess) -> dict[str, str | None]:
    return by_name(result(proc), "status")


def project(tmp_path: Path, source: str = PIPELINE) -> Path:
    (tmp_path / "pipeline.py").write_text(source)
    return tmp_path


def artifacts(root: Path, node: str) -> list[Path]:
    return sorted((root / ".barca" / "artifacts").glob(f"*--{node}/*"))


def drop(root: Path, node: str) -> None:
    """Delete the one artifact file of an unpartitioned node."""
    (path,) = artifacts(root, node)
    path.unlink()


def key_artifacts(root: Path, node: str) -> list[Path]:
    """The artifact files of a node partitioned by `k`: one directory per key."""
    return sorted((root / ".barca" / "artifacts").glob(f"*--{node}_k_*/*"))


def drop_key(root: Path, node: str, key: str) -> None:
    """Delete the artifact of partition `k=<key>` of `node`."""
    (path,) = (root / ".barca" / "artifacts").glob(f"*--{node}_k_{key}/*")
    path.unlink()


# ─── On the command line, without a store ────────────────────────────────────


def test_a_task_recomputes_an_upstream_whose_artifact_was_deleted(tmp_path):
    root = project(tmp_path)
    assert statuses(cli(root, "run", "publish", "--json")) == {"model": "ran", "publish": "ran"}
    drop(root, "model")

    again = cli(root, "run", "publish", "--json")
    doc = result(again)
    assert by_name(doc, "status") == {"model": "ran", "publish": "ran"}
    # It says why, in the step and on stderr.
    assert by_name(doc, "reason")["model"] == "artifact_missing"
    assert "pipeline.py:model: the artifact of its cached result is missing" in again.stderr
    assert "publish 1" in again.stderr
    assert doc["steps_executed"] == 2

    # The artifact is back where it was, so it is a plain cache hit from then on.
    assert len(artifacts(root, "model")) == 1
    assert statuses(cli(root, "run", "publish", "--json"))["model"] == "cached"


def test_a_deleted_target_artifact_is_recomputed_and_its_upstream_stays_cached(tmp_path):
    root = project(tmp_path)
    assert statuses(cli(root, "get", "report", "--json")) == {"model": "ran", "report": "ran"}
    drop(root, "report")
    doc = result(cli(root, "get", "report", "--json"))
    assert by_name(doc, "status") == {"model": "cached", "report": "ran"}
    assert by_name(doc, "reason")["report"] == "artifact_missing"
    assert doc["final_output"] == {"from": 1}


def test_a_pruned_intermediate_that_nothing_reads_is_not_recomputed(tmp_path):
    root = project(tmp_path)
    assert result(cli(root, "get", "report", "--json"))["steps_executed"] == 2
    drop(root, "model")

    doc = result(cli(root, "get", "report", "--json"))
    assert doc["steps_executed"] == 0
    assert by_name(doc, "status") == {"model": "cached", "report": "cached"}
    assert doc["final_output"] == {"from": 1}
    assert artifacts(root, "model") == [], "the pruned artifact was written again"

    # The dry run and status say the same.
    plan = result(cli(root, "get", "report", "--dry-run", "--json"))
    assert by_name(plan, "action") == {"model": "cached", "report": "cached"}
    assert plan["summary"] == {"will_run": 0, "cached": 2, "unknown": 0}
    status = result(cli(root, "status", "report", "pipeline.py", "--json"))
    assert {n["name"]: n["cache"]["state"] for n in status["nodes"]} == {
        "model": "cached",
        "report": "cached",
    }


def test_a_recomputed_step_recomputes_its_own_missing_input(tmp_path):
    root = project(tmp_path)
    assert result(cli(root, "get", "report", "--json"))["steps_executed"] == 2
    drop(root, "model")
    drop(root, "report")

    doc = result(cli(root, "get", "report", "--json"))
    # `report` was asked for and is gone; computing it reads `model`, which is gone too.
    assert by_name(doc, "status") == {"model": "ran", "report": "ran"}
    assert by_name(doc, "reason") == {"model": "artifact_missing", "report": "artifact_missing"}
    assert doc["final_output"] == {"from": 1}


def test_without_a_target_the_ends_of_the_pipeline_are_recomputed(tmp_path):
    root = project(tmp_path)
    assert result(cli(root, "get", "pipeline.py", "--json"))["steps_executed"] == 2
    shutil.rmtree(root / ".barca" / "artifacts")

    doc = result(cli(root, "get", "pipeline.py", "--json"))
    # `report` is what the pipeline produces; `model` is read to compute it.
    assert by_name(doc, "status") == {"model": "ran", "report": "ran"}
    assert doc["final_output"] == {"from": 1}
    assert result(cli(root, "get", "pipeline.py", "--json"))["steps_executed"] == 0


def test_several_targets_each_get_their_artifact_back(tmp_path):
    root = project(tmp_path)
    assert result(cli(root, "get", "model,report", "--json"))["steps_executed"] == 2
    drop(root, "model")

    doc = result(cli(root, "get", "model,report", "--json"))
    # `model` is a target here, so its artifact is needed although `report` is cached.
    assert by_name(doc, "status") == {"model": "ran", "report": "cached"}
    assert doc["targets"]["model"] == {"status": "success", "final_output": {"v": 1}}
    assert doc["targets"]["report"] == {"status": "success", "final_output": {"from": 1}}


# ─── --dry-run and status ────────────────────────────────────────────────────


def test_a_dry_run_reports_a_needed_missing_artifact_as_a_run_with_the_reason(tmp_path):
    root = project(tmp_path)
    assert cli(root, "run", "publish", "--json").returncode == 0
    drop(root, "model")

    plan = result(cli(root, "run", "publish", "--dry-run", "--json"))
    assert by_name(plan, "action") == {"model": "run", "publish": "run"}
    assert by_name(plan, "reason") == {"model": "artifact_missing", "publish": "task"}
    assert "artifact file is missing" in by_name(plan, "detail")["model"]
    assert plan["summary"] == {"will_run": 2, "cached": 0, "unknown": 0}
    # The dry run wrote nothing, and predicted what the run then does.
    assert artifacts(root, "model") == []
    assert result(cli(root, "run", "publish", "--json"))["steps_executed"] == 2


def test_status_reports_a_needed_missing_artifact_as_stale_with_the_reason(tmp_path):
    root = project(tmp_path)
    assert cli(root, "run", "publish", "--json").returncode == 0
    drop(root, "model")

    status = result(cli(root, "status", "publish", "pipeline.py", "--json"))
    cache = {n["name"]: n["cache"] for n in status["nodes"]}
    assert (cache["model"]["state"], cache["model"]["reason"]) == ("stale", "artifact_missing")
    assert "artifact file is missing" in cache["model"]["detail"]
    assert "artifact" not in cache["model"], "a missing artifact is not one a get would serve"
    assert status["summary"]["stale"] == 1


# ─── Partitions ──────────────────────────────────────────────────────────────


def test_only_the_partition_key_whose_artifact_is_missing_is_recomputed(tmp_path):
    root = project(tmp_path, PARTITIONED)
    assert result(cli(root, "get", "part", "--json"))["steps_executed"] == 3
    drop_key(root, "part", "b")

    plan = result(cli(root, "get", "part", "--dry-run", "--json"))
    (line,) = plan["steps"]
    assert (line["action"], line["reason"]) == ("partial", "artifact_missing")
    assert line["partitions"] == {
        "total": 3,
        "cached": 2,
        "will_run": 1,
        "will_run_keys": ["k=b"],
    }

    doc = result(cli(root, "get", "part", "--json"))
    (line,) = doc["steps"]
    assert (line["status"], line["reason"]) == ("partial", "artifact_missing")
    assert line["partitions"]["will_run_keys"] == ["k=b"]
    assert doc["steps_executed"] == 1
    assert len(key_artifacts(root, "part")) == 3
    assert result(cli(root, "get", "part", "--json"))["steps_executed"] == 0


def test_a_pruned_partition_that_nothing_reads_is_not_recomputed(tmp_path):
    root = project(tmp_path, PARTITIONED)
    assert result(cli(root, "get", "double", "--json"))["steps_executed"] == 6
    drop_key(root, "part", "b")

    doc = result(cli(root, "get", "double", "--json"))
    assert doc["steps_executed"] == 0
    assert len(key_artifacts(root, "part")) == 2


def test_a_running_partition_recomputes_only_the_upstream_key_it_reads(tmp_path):
    root = project(tmp_path, PARTITIONED)
    assert result(cli(root, "get", "double", "--json"))["steps_executed"] == 6
    drop_key(root, "part", "b")
    drop_key(root, "double", "b")

    doc = result(cli(root, "get", "double", "--json"))
    # double[k=b] was asked for and is gone; it reads part[k=b], gone too. Nothing else runs.
    assert doc["steps_executed"] == 2
    lines = {s["id"].rsplit(":", 1)[1]: s for s in doc["steps"]}
    assert lines["part"]["partitions"]["will_run_keys"] == ["k=b"]
    assert lines["double"]["partitions"]["will_run_keys"] == ["k=b"]
    assert len(key_artifacts(root, "part")) == 3 and len(key_artifacts(root, "double")) == 3


def test_a_collect_consumer_gets_every_key_when_one_was_missing(tmp_path):
    root = project(tmp_path, PARTITIONED)
    assert result(cli(root, "get", "summary", "--json"))["final_output"] == {
        "keys": ["a", "b", "c"]
    }
    drop_key(root, "part", "b")
    drop(root, "summary")

    doc = result(cli(root, "get", "summary", "--json"))
    # The missing key is computed before `summary` reads the list, so the list is complete.
    assert doc["final_output"] == {"keys": ["a", "b", "c"]}
    assert doc["steps_executed"] == 2


def test_a_missing_partitions_from_source_is_recomputed_before_expansion(tmp_path):
    root = project(tmp_path, DYNAMIC)
    assert result(cli(root, "get", "per_key", "--json"))["steps_executed"] == 3
    drop(root, "keys")

    # The keys are not known until the source has run again.
    plan = result(cli(root, "get", "per_key", "--dry-run", "--json"))
    assert by_name(plan, "action") == {"keys": "run", "per_key": "unknown"}
    assert by_name(plan, "reason") == {"keys": "artifact_missing", "per_key": "partitions_unknown"}

    again = cli(root, "get", "per_key", "--json")
    doc = result(again)
    assert by_name(doc, "status") == {"keys": "ran", "per_key": "cached"}
    assert by_name(doc, "reason")["keys"] == "artifact_missing"
    assert doc["steps_executed"] == 1
    assert "failed to read partition artifact" not in again.stderr


# ─── With an artifact store ──────────────────────────────────────────────────


def store_files(store: Path, node: str) -> list[Path]:
    return sorted(store.glob(f"**/*--{node}/*"))


def test_with_a_store_a_deleted_local_copy_is_fetched_not_recomputed(tmp_path):
    root = project(tmp_path)
    store = str(tmp_path / "store")
    assert cli(root, "get", "report", "--json", BARCA_REMOTE_URI=store).returncode == 0
    shutil.rmtree(root / ".barca" / "artifacts")
    proc = cli(root, "get", "report", "--json", BARCA_REMOTE_URI=store)
    assert statuses(proc) == {"model": "cached", "report": "cached"}
    assert json.loads(proc.stdout)["final_output"] == {"from": 1}


def test_an_unavailable_directory_store_does_not_lose_hits_that_are_on_disk(tmp_path):
    root = project(tmp_path)
    store = tmp_path / "store"
    env = {"BARCA_REMOTE_URI": str(store), "BARCA_STATE": "off"}
    assert result(cli(root, "run", "publish", "--json", **env))["steps_executed"] == 2
    # The share goes away (unmounted); the local copies of every artifact are still here.
    store.rename(tmp_path / "unmounted")

    proc = cli(root, "run", "publish", "--json", **env)
    assert statuses(proc) == {"model": "cached", "publish": "ran"}
    assert "publish 1" in proc.stderr
    plan = result(cli(root, "run", "publish", "--dry-run", "--json", **env))
    assert by_name(plan, "action") == {"model": "cached", "publish": "run"}


def test_a_result_gone_from_both_disk_and_store_is_recomputed(tmp_path):
    root = project(tmp_path)
    store = tmp_path / "store"
    env = {"BARCA_REMOTE_URI": str(store)}
    assert result(cli(root, "get", "report", "--json", **env))["steps_executed"] == 2
    drop(root, "report")
    (stored,) = store_files(store, "report")
    stored.unlink()

    plan = result(cli(root, "get", "report", "--dry-run", "--json", **env))
    assert by_name(plan, "action") == {"model": "cached", "report": "run"}
    assert by_name(plan, "reason")["report"] == "artifact_missing"

    proc = cli(root, "get", "report", "--json", **env)
    doc = result(proc)
    assert by_name(doc, "status") == {"model": "cached", "report": "ran"}
    assert by_name(doc, "reason")["report"] == "artifact_missing"
    assert doc["final_output"] == {"from": 1}
    assert "could not fetch" not in proc.stderr
    # It is back in the store for every machine, and a plain hit from then on.
    assert store_files(store, "report") == [stored]
    assert result(cli(root, "get", "report", "--json", **env))["steps_executed"] == 0


def test_an_input_gone_from_both_disk_and_store_is_recomputed_for_its_reader(tmp_path):
    root = project(tmp_path)
    store = tmp_path / "store"
    env = {"BARCA_REMOTE_URI": str(store)}
    assert result(cli(root, "run", "publish", "--json", **env))["steps_executed"] == 2
    drop(root, "model")
    (stored,) = store_files(store, "model")
    stored.unlink()

    proc = cli(root, "run", "publish", "--json", **env)
    doc = result(proc)
    assert by_name(doc, "status") == {"model": "ran", "publish": "ran"}
    assert by_name(doc, "reason")["model"] == "artifact_missing"
    assert "publish 1" in proc.stderr
    assert store_files(store, "model") == [stored]


def test_a_result_gone_from_both_that_nothing_reads_is_not_recomputed(tmp_path):
    root = project(tmp_path)
    store = tmp_path / "store"
    env = {"BARCA_REMOTE_URI": str(store)}
    assert result(cli(root, "get", "report", "--json", **env))["steps_executed"] == 2
    drop(root, "model")
    (stored,) = store_files(store, "model")
    stored.unlink()

    doc = result(cli(root, "get", "report", "--json", **env))
    assert doc["steps_executed"] == 0
    assert doc["final_output"] == {"from": 1}
    assert store_files(store, "model") == []


@pytest.mark.skipif(os.geteuid() == 0, reason="root reads files whatever their mode")
def test_a_store_that_cannot_be_read_still_fails_the_run(tmp_path):
    root = project(tmp_path)
    store = tmp_path / "store"
    env = {"BARCA_REMOTE_URI": str(store)}
    assert result(cli(root, "get", "report", "--json", **env))["steps_executed"] == 2
    drop(root, "report")
    (stored,) = store_files(store, "report")
    stored.chmod(0)
    try:
        proc = cli(root, "get", "report", "--json", **env)
    finally:
        stored.chmod(0o644)
    # The object is there but unreadable: that is a store problem, not a missing result.
    assert proc.returncode == 3, proc.stderr
    assert "could not fetch 1 cached artifact(s)" in proc.stderr
    assert artifacts(root, "report") == []


# ─── Under `barca serve` ─────────────────────────────────────────────────────


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _rows(root: Path) -> dict[tuple[str, str], int]:
    """Materializations per (node, status); empty while the database is not there yet."""
    try:
        db = sqlite3.connect(root / ".barca" / "metadata.db")
        try:
            return {
                (node.rsplit(":", 1)[1], status): count
                for node, status, count in db.execute(
                    "select node_id, status, count(*) from materializations group by 1, 2"
                )
            }
        finally:
            db.close()
    except sqlite3.Error:
        return {}


def _wait_for(root: Path, done, what: str, timeout: float = 60) -> dict[tuple[str, str], int]:
    deadline = time.monotonic() + timeout
    while True:
        rows = _rows(root)
        if done(rows):
            return rows
        if time.monotonic() > deadline:
            raise AssertionError(f"timed out waiting for {what}: {rows}")
        time.sleep(0.25)


def test_a_scheduled_task_recovers_once_when_its_input_is_deleted(tmp_path):
    """The reproduction in #252: before the fix every tick after the delete failed."""
    root = project(tmp_path, SCHEDULED)
    serve = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(_free_port())],
        cwd=root,
        env=clean_env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        before = _wait_for(root, lambda r: r.get(("publish", "success"), 0) >= 2, "first ticks")
        drop(root, "model")
        # A tick that started before the delete may still land; three more is past it.
        _wait_for(
            root,
            lambda r: (
                r.get(("publish", "success"), 0) + r.get(("publish", "failed"), 0)
                >= before[("publish", "success")] + 4
            ),
            "ticks after the delete",
        )
    finally:
        serve.send_signal(signal.SIGTERM)
        try:
            _, err = serve.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            serve.kill()
            _, err = serve.communicate()

    rows = _rows(root)
    context = f"{rows}\n{err}"
    assert ("publish", "failed") not in rows, f"a tick failed after the delete: {context}"
    assert rows[("model", "success")] == 2, f"model should be recomputed exactly once: {context}"
    assert err.count("pipeline.py:model: the artifact of its cached result is missing") == 1, err
