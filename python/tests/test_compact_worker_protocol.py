"""Runtime conformance for specs/worker-protocol.md's compact batch profile."""

import json
import os
import sqlite3
import subprocess
from contextlib import closing
from pathlib import Path

import pytest
from barca import _runtime, _wire
from barca.api import _find_binary


def test_result_batch_emits_one_array_frame_and_file_metadata_only(tmp_path, monkeypatch):
    sent = []
    monkeypatch.setattr(_runtime, "send_message", sent.append)
    path = tmp_path / "results"
    with _runtime.result_batch(path):
        _runtime.emit_step_completed("a", {"path": "/a.json", "format": "json", "size_bytes": 1})
        _runtime.emit_step_completed("b", {"path": "/b.json", "format": "json", "size_bytes": 2})
    frames = [frame for frame in sent if frame["type"] == "results"]
    assert len(frames) == 1 and frames[0]["node_ids"] == ["a", "b"]
    assert len(_wire.decode_results(frames[0])) == 2
    records = [json.loads(line) for line in path.read_text().splitlines()]
    assert [record[:2] for record in records] == [["a", "step_completed"], ["b", "step_completed"]]
    assert _runtime._result_batch is None


def test_long_batch_publishes_progress_before_it_ends(tmp_path, monkeypatch):
    import threading

    sent = []
    notified = threading.Event()

    def capture(frame):
        sent.append(frame)
        if frame["type"] == "result_checkpoint":
            notified.set()

    monkeypatch.setattr(_runtime, "send_message", capture)
    with _runtime.result_batch(tmp_path / "results"):
        _runtime.emit_blocked("a", "sensor")
        assert notified.wait(2)
        assert (
            next(frame["completed"] for frame in sent if frame["type"] == "result_checkpoint") == 1
        )
        assert not any(frame["type"] == "results" for frame in sent)
    assert sent[-1]["statuses"] == ["blocked"]


@pytest.mark.parametrize("failure", ["crash", "exception"])
def test_real_batched_worker_preserves_completed_items_on_failure(tmp_path, failure):
    # Runtime observation captures real result frames, never substitutes a worker result.
    (tmp_path / "sitecustomize.py").write_text("""
import json, os
from pathlib import Path
from barca import _runtime
original = _runtime.send_message
def observe(message):
    if message.get("type") == "results":
        with open(f"frames-{os.getpid()}.jsonl", "a") as f:
            f.write(json.dumps({"count": len(message["node_ids"])}) + "\\n")
    return original(message)
_runtime.send_message = observe
""")
    keys = [str(i) for i in range(12)]
    (tmp_path / "pipeline.py").write_text(f"""
import os
from pathlib import Path
from barca import asset, partitions
@asset(partitions={{"k": partitions({keys!r})}}, retries=2)
def leaf(k):
    print(f"leaf-{{k}}")
    with open("calls", "a") as f:
        f.write(k + "\\n")
    if k == "3" and Path("enabled").exists() and not Path("failed-once").exists():
        Path("failed-once").touch()
        if {failure!r} == "crash": os._exit(17)
        raise ValueError("retry this item")
    return {{"key": k}}
""")
    env = {
        **os.environ,
        "PYTHONPATH": os.pathsep.join([str(tmp_path), str(Path(__file__).resolve().parents[1])]),
        "BARCA_POOL_SIZE": "1",
    }

    def run(*extra):
        process = subprocess.Popen(
            [_find_binary(), "get", "leaf", "pipeline.py", "--json", *extra],
            cwd=tmp_path,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        try:
            out, err = process.communicate(timeout=45)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, 9)
            out, err = process.communicate()
            pytest.fail(f"batched run stalled: {err}")
        assert process.returncode == 0, err
        return json.loads(out)

    run()
    # Seed the measured-cost seam, not execution results: a warm cheap node really leases
    # several items per pull. Cold nodes intentionally use singleton pulls.
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        db.execute("UPDATE cost_estimates SET estimate_seconds=0.00001, samples=10")
        db.commit()
    (tmp_path / "calls").write_text("")
    (tmp_path / "enabled").touch()
    for path in tmp_path.glob("frames-*.jsonl"):
        path.unlink()
    result = run("--refresh-all")
    calls = (tmp_path / "calls").read_text().splitlines()
    assert all(calls.count(key) == (2 if key == "3" else 1) for key in keys)
    with closing(sqlite3.connect(tmp_path / ".barca/metadata.db")) as db:
        rows = db.execute(
            "SELECT node_id, attempts FROM materializations WHERE run_id=? ORDER BY node_id",
            [result["run_id"]],
        ).fetchall()
    assert len(rows) == 12
    assert all(attempts == (2 if node.endswith("[k=3]") else 1) for node, attempts in rows)
    counts = [
        json.loads(line)["count"]
        for path in tmp_path.glob("frames-*.jsonl")
        for line in path.read_text().splitlines()
    ]
    assert any(count > 1 for count in counts), counts
    recorded = subprocess.run(
        [_find_binary(), "logs", result["run_id"], "--json"],
        cwd=tmp_path,
        env=env,
        text=True,
        capture_output=True,
        timeout=15,
    )
    assert recorded.returncode == 0, recorded.stderr
    lines = [row["line"] for row in json.loads(recorded.stdout)]
    assert all(lines.count(f"leaf-{key}") == (2 if key == "3" else 1) for key in keys), lines
    assert not list((tmp_path / ".barca/staging").rglob("results-*"))
