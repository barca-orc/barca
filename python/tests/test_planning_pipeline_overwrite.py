"""The overwrite scenarios of #241, and a corrupt backup (#243), on the planning pipeline (#250).

#241 found three ordinary ways a store copy stops matching the recorded one, because an
artifact's path names the computation (`<node>/<run_hash>`), not its bytes. They are run here
on the registry corner of the graph with today's semantics: since 0.26 the store holds no
shared history, so each scenario is one machine with a store.

- Two overlapping `--refresh` runs of a registry whose source changed between them (the
  registry API stub, switched by an undeclared variable, so the run hash does not change).
- A `--refresh` killed the moment its new bytes land in the store.
- `BARCA_STATE=off` is gone (a usage error since 0.26.0); what is left of that scenario is a
  machine whose local copy was deleted, which fetches the store's.

After each, the next `barca get` succeeds and the result is `cached` with this machine's copy,
the store's object and the recorded digest in agreement, without a `--refresh` (#576).

#243 was a shared-history blob corrupted part-way; the blob is gone (0.26.0). Its successor is a
backup, and a corrupt backup is refused by `barca state restore`, which names an older one.
"""

from __future__ import annotations

import hashlib
import json
import os
import signal
import subprocess
import time

import pytest
from barca.api import _find_binary

from .planning_pipeline_store import Bucket, Machine, new_machine

pytest.importorskip("pandas")

REGISTRY = "launch_registry"
DRIVER = "driver_launch_item"
# While this file exists in the project, the driver's step waits (`hold_the_driver`).
HOLD = ".hold_driver"
DRIVER_DEF = """def driver_launch_item(
    registry: duckdb.DuckDBPyRelation,
    forecast_item: duckdb.DuckDBPyRelation,
) -> duckdb.DuckDBPyRelation:
"""


@pytest.fixture(scope="module")
def bucket():
    b = Bucket()
    yield b
    b.remove()


def hold_the_driver(root) -> None:
    """Make the driver's step wait while the project holds `HOLD`: a test that kills a run
    then always kills it mid-run, never after it finished (#616). Done before the first run,
    so every run of the module computes the same code."""
    assets = root / "assets.py"
    source = assets.read_text()
    assert source.count(DRIVER_DEF) == 1
    wait = (
        "    import time\n\n"
        f"    while (Path(__file__).parent / {HOLD!r}).exists():\n"
        "        time.sleep(0.05)\n"
    )
    assets.write_text(source.replace(DRIVER_DEF, DRIVER_DEF + wait))


@pytest.fixture(scope="module")
def machine(tmp_path_factory, bucket) -> Machine:
    m, _ = new_machine(tmp_path_factory.mktemp("overwrite") / "project", "store", bucket)
    hold_the_driver(m.root)
    m.ok("get", DRIVER, "assets.py", "--json")
    return m


def api_version(machine: Machine, version: int) -> str:
    """A copy of the registry API stub at another version, with other rates: the value of
    `REGISTRY_API_URL` that serves it."""
    api = json.loads((machine.root / "registry_api.json").read_text())
    api["version"] = version
    for row in api["registries"]["launch"]:
        row["rate"] = round(row["rate"] + version / 100, 3)
    (machine.root / f"api_v{version}.json").write_text(json.dumps(api))
    return f"file:api_v{version}.json"


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def assert_consistent(machine: Machine, name: str) -> bytes:
    """`cached`, and this machine's copy, the store's object and the recorded digest agree."""
    node = machine.status()[name]
    assert node["cache"]["state"] == "cached", node["cache"]
    local = machine.local_copy(node).read_bytes()
    stored = machine.bucket.cat(node["cache"]["artifact"])
    assert sha(local) == sha(stored) == node["last_materialization"]["content_hash"]
    return local


def refresh(machine: Machine, version: int, target: str = REGISTRY) -> subprocess.Popen:
    return subprocess.Popen(
        [_find_binary(), "get", target, "assets.py", "--refresh", REGISTRY, "--json"],
        cwd=machine.root,
        env={**machine.env, "REGISTRY_API_URL": api_version(machine, version)},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )


MISMATCH = "is not the one this result was recorded with"


def settle(machine: Machine) -> None:
    """The next `get` brings the registry back to `cached`, without a `--refresh` (#576).

    When the store holds bytes the record does not name (a run's upload landed after another
    run's row, or a run was killed between its upload and its row), the store's copy wins:
    `get` fetches it, warns once, and records its digest. When this machine's copy is the
    recorded one and the store's differs, `get` uploads this machine's copy with the
    changed-bytes warning (`barca docs remote`). Otherwise there is nothing to do."""
    node = machine.status()[REGISTRY]
    if node["cache"]["state"] == "cached":
        return
    assert node["cache"]["reason"] == "mismatch", node["cache"]
    local = machine.local_copy(node).read_bytes()
    stored = machine.bucket.cat(node["cache"]["artifact"])
    recorded = node["last_materialization"]["content_hash"]
    proc = machine.cli("get", REGISTRY, "assets.py", "--json")
    assert proc.returncode == 0, proc.stderr
    if recorded == sha(local):
        assert f"{REGISTRY} produced different bytes for the same code and inputs" in proc.stderr
        kept = local
    else:
        assert proc.stderr.count(MISMATCH) == 1, proc.stderr
        kept = stored
    assert assert_consistent(machine, REGISTRY) == kept
    # Repaired once: the next get finds nothing to say.
    again = machine.cli("get", REGISTRY, "assets.py", "--json")
    assert again.returncode == 0, again.stderr
    # The pipeline's undeclared environment reads are reported on every command (#574).
    warnings = [
        line
        for line in again.stderr.splitlines()
        if "warning" in line and "not declared in env=" not in line
    ]
    assert warnings == [], again.stderr


def test_overlapping_refreshes_leave_one_consistent_result(machine):
    before = assert_consistent(machine, REGISTRY)
    runs = [refresh(machine, 2), refresh(machine, 3)]
    for run in runs:
        _, err = run.communicate(timeout=300)
        assert run.returncode == 0, err
        # Each run's bytes differ from what was recorded before it, for the same run hash.
        assert f"assets.py:{REGISTRY} produced different bytes for the same code and inputs" in err
    # Whichever run's upload landed last holds the store; whichever recorded last holds the row.
    settle(machine)
    after = assert_consistent(machine, REGISTRY)
    assert after != before

    # The next get serves that result and computes nothing. The driver keeps the result it
    # computed from the old registry: a refresh cascades only inside the target's cone, and the
    # run hash names the registry's computation, not its bytes (`barca docs cache`).
    out = machine.ok("get", DRIVER, "assets.py", "--json")
    assert out["steps_executed"] == 0
    assert assert_consistent(machine, REGISTRY) == after
    assert_consistent(machine, DRIVER)


def test_a_refresh_killed_once_its_upload_lands_is_recoverable(machine):
    """Killed between its upload and its row (most attempts), the store and this machine hold
    the new bytes and the record names the old ones; the next `get` that returns or reads the
    registry records the new ones (`settle`, #576). When the kill lands after the row instead,
    nothing is left to repair.

    The driver's step, downstream of the registry, waits until the kill (`hold_the_driver`):
    the run is always in flight when it is killed, however soon it would otherwise finish."""
    node = machine.status()[REGISTRY]
    artifact = node["cache"]["artifact"]
    before = machine.bucket.cat(artifact)
    hold = machine.root / HOLD
    hold.touch()
    try:
        run = refresh(machine, 4, target=DRIVER)
        deadline = time.monotonic() + 120
        while machine.bucket.cat(artifact) == before:
            assert run.poll() is None, "the refresh ended before its upload was seen"
            assert time.monotonic() < deadline, "the refreshed registry never reached the store"
            time.sleep(0.02)
        assert run.poll() is None, "the refresh ended while its driver step was held"
        os.killpg(run.pid, signal.SIGKILL)
        run.communicate(timeout=60)
    finally:
        hold.unlink(missing_ok=True)

    history = machine.ok("history", "--json", "--limit", "1")["runs"][0]
    assert history["status"] == "interrupted", history
    out = machine.ok("get", DRIVER, "assets.py", "--json")
    assert out["status"] == "success"

    stored = machine.bucket.cat(artifact)
    assert stored != before
    node = machine.status()[REGISTRY]
    if node["cache"]["state"] != "cached":
        # Its worker wrote this machine's copy before the upload: both hold the new bytes, and
        # the record names neither.
        assert node["cache"]["reason"] == "mismatch", node["cache"]
        assert machine.local_copy(node).read_bytes() == stored
        assert node["last_materialization"]["content_hash"] != sha(stored)
    settle(machine)
    assert assert_consistent(machine, REGISTRY) == stored
    assert_consistent(machine, REGISTRY)
    assert_consistent(machine, DRIVER)


def test_barca_state_off_is_gone_and_a_deleted_local_copy_is_fetched(machine):
    proc = machine.cli("get", REGISTRY, "assets.py", "--json", env={"BARCA_STATE": "off"})
    assert proc.returncode == 2
    assert "BARCA_STATE is no longer supported" in proc.stderr

    expected = assert_consistent(machine, REGISTRY)
    node = machine.status()[REGISTRY]
    machine.local_copy(node).unlink()
    node = machine.status()[REGISTRY]
    assert (node["cache"]["state"], node["cache"]["reason"]) == ("unsynced", "store_only")
    out = machine.ok("get", REGISTRY, "assets.py", "--json")
    assert out["steps_executed"] == 0
    assert assert_consistent(machine, REGISTRY) == expected


def test_a_corrupt_backup_is_refused_and_an_older_one_restores(machine, tmp_path):
    backups = machine.ok("state", "backups", "--json")["backups"]
    stored = [b for b in backups if b["where"] in ("store", "both")]
    assert len(stored) >= 2, backups
    newest = f"{machine.store_uri}/default/state/backups/{stored[0]['name']}"
    blob = machine.bucket.cat(newest)
    machine.bucket.fs.pipe_file(newest, blob[: len(blob) // 2])  # cut off part-way

    other = machine.clone(tmp_path / "other")
    proc = other.cli("state", "restore", "--json")
    assert proc.returncode == 2, proc.stderr
    assert "barca state restore --from 2" in proc.stderr
    assert not (other.root / ".barca" / "metadata.db").exists()

    other.ok("state", "restore", "--from", "2", "--json")
    out = other.ok("get", REGISTRY, "assets.py", "--json")
    assert out["steps_executed"] == 0
