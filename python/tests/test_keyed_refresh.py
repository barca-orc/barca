"""Refresh keys without narrowing the target domain (#784)."""

import json
import sqlite3

import pytest

from .test_asset_cutoff import calls, get, run, step

PIPELINE = """
from pathlib import Path
from barca import asset, collect, partitions, task

KEYS = ["A", "B", "C"]

def called(name):
    with Path("calls.txt").open("a") as f:
        f.write(name + "\\n")

@asset(partitions={"account": partitions(KEYS)})
def source(account: str) -> str:
    called("source " + account)
    if account == "A" and Path("fail").exists():
        raise RuntimeError("source unavailable")
    return account

@asset(inputs={"s": source}, partitions={"account": partitions(KEYS)})
def aligned(account: str, s: str) -> str:
    called("aligned " + account)
    return s + "!"

@asset(inputs={"s": aligned}, partitions={"account": partitions(KEYS)}, batch="all")
def batched(account: list[str], s: list[str]) -> list[str]:
    called("batched " + ",".join(account))
    return s

@asset(inputs={"values": collect(batched)})
def final(values: list[str]) -> list[str]:
    called("final")
    return values

@task(inputs={"result": final})
def publish(result: list[str]) -> None:
    assert len(result) == 3
    called("publish")
"""


@pytest.fixture()
def project(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    get(tmp_path, "final")
    calls(tmp_path)
    return tmp_path


@pytest.mark.parametrize("selected", [["A"], ["A", "C"]])
def test_refresh_set_preserves_siblings_and_full_fanin(project, selected):
    with sqlite3.connect(project / ".barca/metadata.db") as db:
        before = db.execute(
            "SELECT node_id, run_hash, artifact_path FROM materializations WHERE node_id LIKE '%[account=B]' ORDER BY id"
        ).fetchall()
    sibling_bytes = {path: (project / path).read_bytes() for _, _, path in before}
    selector = ",".join(f"source[account={k}]" for k in selected)
    preview = get(project, "final", "--refresh", selector, "--dry-run")
    for node in ("source", "aligned", "batched"):
        summary = step(preview, node)["partitions"]
        assert summary["cached"] == 3 - len(selected)
        assert summary["will_run"] == len(selected)
    assert step(preview, "final")["action"] == "run"
    assert calls(project) == []
    result = get(project, "final", "--refresh", selector)
    actual = calls(project)
    assert actual == sorted(
        [f"source {k}" for k in selected]
        + [f"aligned {k}" for k in selected]
        + ["batched " + ",".join(selected), "final"]
    )
    assert step(result, "source")["reason"] == "refresh"
    assert step(result, "aligned")["reason"] == "refresh_cascade"
    with sqlite3.connect(project / ".barca/metadata.db") as db:
        after = db.execute(
            "SELECT node_id, run_hash, artifact_path FROM materializations WHERE node_id LIKE '%[account=B]' ORDER BY id"
        ).fetchall()
    assert after == before
    assert all((project / path).read_bytes() == data for path, data in sibling_bytes.items())
    # Read the publication through the normal value path: it must contain cached B too.
    proc = run(project, "get", "final", "pipeline.py", "-o", "value", "--json")
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["final_output"] == ["A!", "B!", "C!"]


def test_keyed_cutoff_stops_cascade(project):
    path = project / "pipeline.py"
    path.write_text(PIPELINE.replace("@asset(partitions=", "@asset(cutoff=True, partitions=", 1))
    get(project, "final")
    calls(project)
    result = get(project, "final", "--refresh", "source[account=A]")
    assert calls(project) == ["source A"]
    assert step(result, "aligned")["status"] == "cached"
    assert "propagation stopped" in step(result, "aligned")["detail"]
    assert step(result, "final")["status"] == "cached"


def test_unknown_key_fails_clearly(project):
    proc = run(project, "get", "final", "pipeline.py", "--refresh", "source[account=missing]")
    assert proc.returncode == 2
    assert "matches no planned partition keys" in proc.stderr
    assert calls(project) == []


def test_no_cascade(project):
    get(project, "final", "--refresh", "source[account=A]", "--no-cascade")
    assert calls(project) == ["source A"]


def test_server_refresh_set(project):
    from . import served
    from .test_asset_cutoff import env

    with served.Serve(project, env=env()) as serve:
        serve.ready()
        selectors = ["source[account=A]", "source[account=C]"]
        request = {"command": "get", "targets": ["final"], "refresh": selectors}
        preview = serve.post("/runs", {**request, "dry_run": True})
        assert step(preview, "source")["partitions"]["will_run"] == 2
        answer = serve.post("/runs", request)
        result = serve.finished(answer["run_id"])
        assert result["status"] == "success", result
        assert calls(project) == sorted(
            ["source A", "source C", "aligned A", "aligned C", "batched A,C", "final"]
        )


def test_python_api_refresh_set(project, monkeypatch):
    import barca

    monkeypatch.chdir(project)
    result = barca.get("final", "pipeline.py", refresh=["source[account=A]", "source[account=C]"])
    assert result == ["A!", "B!", "C!"]
    assert calls(project) == sorted(
        ["source A", "source C", "aligned A", "aligned C", "batched A,C", "final"]
    )


@pytest.mark.parametrize("condition", ["missing", "stale"])
def test_sibling_is_recovered_for_complete_publication(project, monkeypatch, condition):
    import barca

    with sqlite3.connect(project / ".barca/metadata.db") as db:
        path = db.execute(
            "SELECT artifact_path FROM materializations WHERE node_id = 'pipeline.py:batched[account=B]' ORDER BY id DESC LIMIT 1"
        ).fetchone()[0]
    if condition == "missing":
        (project / path).unlink()
    else:
        with sqlite3.connect(project / ".barca/metadata.db") as db:
            db.execute(
                "UPDATE materializations SET run_hash = 'old' WHERE node_id = 'pipeline.py:batched[account=B]'"
            )
    monkeypatch.chdir(project)
    result = barca.get("final", "pipeline.py", refresh=["source[account=A]"])
    assert result == ["A!", "B!", "C!"]
    assert any("B" in call for call in calls(project))
    expected = barca.get("final", "pipeline.py", refresh_all=True)
    assert result == expected


def test_failed_refresh_blocks_publication(project):
    (project / "fail").touch()
    proc = run(project, "get", "final", "pipeline.py", "--json", "--refresh", "source[account=A]")
    assert proc.returncode == 1, proc.stderr
    assert "final" not in calls(project)
    assert "source unavailable" in proc.stderr
    publication = step(json.loads(proc.stdout), "final")
    assert publication["status"] == "skipped"
    assert publication["reason"] == "upstream_failed"


def test_python_run_refresh_set(project, monkeypatch):
    import barca

    monkeypatch.chdir(project)
    barca.run("publish", "pipeline.py", refresh=["source[account=A]", "source[account=C]"])
    assert calls(project) == sorted(
        ["source A", "source C", "aligned A", "aligned C", "batched A,C", "final", "publish"]
    )


def test_python_api_refresh_set_on_server(project, monkeypatch):
    import barca

    from . import served
    from .test_asset_cutoff import env

    monkeypatch.chdir(project)
    with served.Serve(project, env=env()) as serve:
        serve.ready()
        result = barca.get(
            "final",
            "pipeline.py",
            refresh=["source[account=A]", "source[account=C]"],
            server=f"http://127.0.0.1:{serve.port}",
        )
        assert result == ["A!", "B!", "C!"]
        assert calls(project) == sorted(
            ["source A", "source C", "aligned A", "aligned C", "batched A,C", "final"]
        )
