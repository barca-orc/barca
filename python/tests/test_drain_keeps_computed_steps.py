"""A server restart keeps the steps it computed whose uploads were still in flight (#1002).

`barca serve` with a store, stopped (SIGTERM) with a short `drain_timeout` while a finished
step's upload is held in the transfer helper (the `put` point of python/tests/hold/) and the
next step is in the middle of its function. The drain deadline cancels the run and puts it back
in the queue. The finished step is recorded with its upload kept pending, as a failed upload is
(#390), so the requeued run uploads it before its own work and executes only the step that was
interrupted.
"""

import json
import os
import signal
from pathlib import Path

from .helpers import clean_env, wait_until
from .served import snapshot

HOLD_SHIM = str(Path(__file__).resolve().parent / "hold")

PIPELINE = """
import time
from pathlib import Path

from barca import asset


@asset()
def quick() -> dict:
    runs = Path("quick.runs")
    runs.write_text(runs.read_text() + "x" if runs.exists() else "x")
    return {"n": 1}


@asset(inputs={"x": quick})
def slow(x: dict) -> int:
    runs = Path("slow.runs")
    runs.write_text(runs.read_text() + "x" if runs.exists() else "x")
    Path("slow.started").write_text("")
    deadline = time.time() + 120
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return x["n"] + 1
"""


def env(store: Path, hold_dir: Path | None = None) -> dict[str, str]:
    e = clean_env(BARCA_POOL_SIZE="2", BARCA_REMOTE_URI=str(store))
    if hold_dir is not None:
        e["PYTHONPATH"] = HOLD_SHIM + os.pathsep + e.get("PYTHONPATH", "")
        e["BARCA_TEST_HOLD"] = f"put:{hold_dir}"
    return e


def recorded(root: Path) -> dict[str, str]:
    """node name -> artifact path of every successful materialization."""
    with snapshot(root) as db:
        if db is None:
            return {}
        return {
            node.rsplit(":", 1)[1]: path
            for node, path in db.execute(
                "select node_id, artifact_path from materializations where status = 'success'"
            )
        }


def pending(root: Path) -> list[str]:
    with snapshot(root) as db:
        return [n.rsplit(":", 1)[1] for (n,) in db.execute("select node_id from pending_uploads")]


def test_a_restart_keeps_a_finished_step_whose_upload_was_in_flight(tmp_path, serve):
    root = tmp_path / "project"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE)
    (root / "barca.toml").write_text("[server]\ndrain_timeout = 1\n")
    store = tmp_path / "store"
    store.mkdir()
    hold = tmp_path / "hold"
    hold.mkdir()

    server = serve("--no-schedule", root=root, env=env(store, hold))
    going = server.request("POST", "/get/slow")[1]["run_id"]
    # `quick` finished and its upload is held in the helper; `slow` is in its function.
    wait_until((hold / "put.started").exists, "the upload of quick to start")
    wait_until((root / "slow.started").exists, "the slow step to start")

    server.send(signal.SIGTERM)
    assert server.exited() == 0, server.stderr
    assert f"[barca] run {going} stopped and put back in the queue" in server.stderr

    # The computed step is recorded at its store location, its upload pending; its object is
    # not in the store yet, and the interrupted step is not recorded.
    kept = recorded(root)
    assert sorted(kept) == ["quick"], kept
    assert pending(root) == ["quick"]
    assert not Path(kept["quick"]).exists()

    (hold / "release").write_text("")
    (root / "release").write_text("")
    restarted = serve("--no-schedule", root=root, env=env(store))
    run = wait_until(
        lambda: (r := restarted.request("GET", f"/runs/{going}")[1])["run"]["status"] == "success"
        and r,
        "the requeued run to finish",
    )
    # The requeued run executed only the interrupted step; the finished one is a cache hit,
    # and its object reached the store before the run's own work.
    assert (root / "quick.runs").read_text() == "x"
    assert (root / "slow.runs").read_text() == "xx"
    assert "uploading 1 pending result" in restarted.stderr, restarted.stderr
    assert pending(root) == []
    assert json.loads(Path(recorded(root)["quick"]).read_text()) == {"n": 1}
    assert Path(recorded(root)["slow"]).is_file()
    steps = {s["node_id"].rsplit(":", 1)[1]: s for s in run["steps"]}
    assert set(steps) >= {"quick", "slow"}, run
