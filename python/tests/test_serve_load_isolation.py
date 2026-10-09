"""Serve keeps valid definitions runnable beside unloaded sources (#312)."""

import fcntl
import json
import subprocess

import pytest
from barca.api import _find_binary

from .test_serve_robustness import Server, _env, wait_for

PIPELINE = """from pathlib import Path
from barca import asset, task, asset_ref, Schedule

@asset(inputs={"value": asset_ref("broken.py:bad")})
def blocked(value): return value + 1

@asset(inputs={"value": blocked})
def dependent(value): return value + 1

@asset()
def safe(): return 7

@task(freshness=Schedule("* * * * * *"))
def healthy():
    with Path("ticks").open("a") as out: out.write("tick\\n")
"""
BROKEN = "from barca import asset\n@asset()\ndef bad(: pass\n"
REPAIRED = 'from barca import asset, Schedule\n@asset(freshness=Schedule("* * * * * *"))\ndef bad(): return 40\n'


@pytest.fixture
def project(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "broken.py").write_text(BROKEN)
    return tmp_path


def loaded(server):
    code, assets = server.request("GET", "/assets")
    assert code == 200
    return {a["id"] for a in assets}


def errors(server):
    code, health = server.request("GET", "/health")
    assert code == 200 and health["status"] == "ok"
    return health["load_errors"]


def finished(server, handle):
    def terminal():
        code, result = server.request("GET", f"/status/{handle}")
        assert code == 200
        return result if result["status"] in {"complete", "failed", "cancelled"} else None

    return wait_for(terminal, "run completion")


def test_broken_sibling_preserves_colocated_schedule_inspection_and_execution(project):
    server = Server(project, ".")
    try:
        assert loaded(server) == {"pipeline.py:safe", "pipeline.py:healthy"}
        code, state = server.request("GET", "/state")
        assert code == 200 and isinstance(state, list)
        assert {n["id"] for n in state} == loaded(server)
        diagnostics = errors(server)
        assert any(e["file"] == "broken.py" and not e["affected_nodes"] for e in diagnostics)
        affected = {n for e in diagnostics for n in e["affected_nodes"]}
        assert affected == {"pipeline.py:blocked", "pipeline.py:dependent"}
        assert server.request("POST", "/get/blocked")[0] == 404
        assert server.request("GET", "/assets/blocked")[0] == 404
        code, handle = server.request("POST", "/get/safe")
        assert code == 200
        result = finished(server, handle["run_id"])
        assert (
            result["status"] == "complete"
            and json.loads((project / result["result"]["final_output"]["path"]).read_text()) == 7
        )
        wait_for(lambda: (project / "ticks").exists(), "unrelated scheduled task")
        assert "not loaded: broken.py" in server.log.read_text()
    finally:
        server.stop()


def test_watch_repair_and_break_refresh_one_selected_graph(project):
    server = Server(project, ".", "--watch")
    try:
        assert "pipeline.py:blocked" not in loaded(server)
        (project / "broken.py").write_text(REPAIRED)
        wait_for(lambda: errors(server) == [], "watch-only diagnostics repair")
        assert "pipeline.py:dependent" in loaded(server)
        wait_for(
            lambda: any(
                job["id"] == "broken.py:bad" for job in server.request("GET", "/schedule")[1]
            ),
            "repaired schedule loaded",
        )
        code, handle = server.request("POST", "/get/dependent")
        assert code == 200
        result = finished(server, handle["run_id"])
        assert (
            result["status"] == "complete"
            and json.loads((project / result["result"]["final_output"]["path"]).read_text()) == 42
        )
        (project / "broken.py").unlink()
        wait_for(lambda: errors(server), "watch-only removal diagnostics")
        assert "pipeline.py:blocked" not in loaded(server)
        wait_for(
            lambda: all(
                job["id"] != "broken.py:bad" for job in server.request("GET", "/schedule")[1]
            ),
            "removed schedule excluded",
        )
        assert server.request("POST", "/get/dependent")[0] == 404
        assert loaded(server) == {"pipeline.py:safe", "pipeline.py:healthy"}
    finally:
        server.stop()


def test_watch_repair_during_scheduler_startup_db_admission_is_not_lost(project):
    metadata = project / ".barca"
    metadata.mkdir()
    server = None
    with (metadata / "metadata.db.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            server = Server(project, ".", "--watch")
            wait_for(
                lambda: "scheduling 1 task:" in server.log.read_text(),
                "initial schedule selected before DB admission",
            )
            (project / "broken.py").write_text(REPAIRED)
            wait_for(lambda: errors(server) == [], "repair while scheduler DB admission is blocked")
            assert "broken.py:bad" in loaded(server)
            fcntl.flock(lock, fcntl.LOCK_UN)
            wait_for(
                lambda: any(
                    job["id"] == "broken.py:bad" for job in server.request("GET", "/schedule")[1]
                ),
                "startup repair reaches scheduler on its next tick",
            )
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)
            if server is not None:
                server.stop()


def test_all_broken_sources_are_inspectable_without_fake_nodes(project):
    (project / "pipeline.py").write_text(BROKEN)
    server = Server(project, ".", "--no-schedule")
    try:
        assert loaded(server) == set()
        assert server.request("GET", "/state") == (200, [])
        assert len(errors(server)) == 2
        assert server.request("POST", "/run")[0] == 400
    finally:
        server.stop()


@pytest.mark.parametrize("command", ["get", "run", "list", "plan", "status"])
def test_one_shot_commands_remain_strict(project, command):
    target = ["healthy"] if command == "run" else []
    result = subprocess.run(
        [_find_binary(), command, *target, "."],
        cwd=project,
        env=_env(),
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 2, result.stderr
    error = json.loads(result.stderr.strip().splitlines()[-1])
    assert "broken.py" in error["error"]


def test_execution_refreshes_helper_hash_even_without_watch(project):
    (project / "pipeline.py").write_text(
        "from barca import asset\nfrom helper import value\n@asset()\ndef safe(): return value()\n"
    )
    helper = project / "helper.py"
    helper.write_text("def value(): return 1\n")
    server = Server(project, ".", "--no-schedule")
    try:
        for value in (1, 2):
            helper.write_text(f"def value(): return {value}\n")
            code, handle = server.request("POST", "/get/safe")
            assert code == 200
            result = finished(server, handle["run_id"])
            assert result["status"] == "complete"
            assert (
                json.loads((project / result["result"]["final_output"]["path"]).read_text())
                == value
            )
            assert result["result"]["steps_executed"] == 1
    finally:
        server.stop()


def test_watched_inspection_reads_do_not_generate_reload_work(project):
    import time

    server = Server(project, ".", "--watch")
    try:
        wait_for(lambda: (project / "ticks").exists(), "healthy scheduled task")
        count = server.log.read_text().count("schedule reloaded:")
        deadline = time.monotonic() + 2.5
        while time.monotonic() < deadline:
            assert loaded(server) == {"pipeline.py:safe", "pipeline.py:healthy"}
            time.sleep(0.05)
        assert server.log.read_text().count("schedule reloaded:") == count
    finally:
        server.stop()


@pytest.mark.parametrize("sibling_healthy", [True, False])
def test_imported_reference_preserves_sibling_priority(project, sibling_healthy):
    sub = project / "sub"
    sub.mkdir()
    (project / "pipeline.py").write_text(
        "from barca import asset\n@asset()\ndef safe(): return 1\n"
    )
    (project / "broken.py").unlink()
    healthy = "raise RuntimeError('inspection imported user code')\nfrom barca import asset\n@asset()\ndef value(): return 7\n"
    (sub / "shared.py").write_text(healthy if sibling_healthy else BROKEN)
    (project / "shared.py").write_text(BROKEN if sibling_healthy else healthy)
    (sub / "p.py").write_text(
        "from barca import asset\nfrom shared import value\n@asset(inputs={'x': value})\ndef consumer(x): return x+1\n"
    )
    server = Server(project, ".", "--no-schedule", "--watch")
    try:
        assert "sub/p.py:consumer" not in loaded(server)
        affected = {n for e in errors(server) for n in e["affected_nodes"]}
        assert "sub/p.py:consumer" in affected
        if sibling_healthy:
            # The producer retains its ordinary sub.shared identity. Its bare
            # alias cannot create a second pipeline identity, even when the
            # lower-priority root candidate failed parsing.
            assert "sub/shared.py:value" in loaded(server)
            assert any("conflicting import identities" in e["error"] for e in errors(server))
            file = sub / "p.py"
            file.write_text(
                file.read_text().replace("from shared import value", "from sub.shared import value")
            )
            wait_for(
                lambda: "sub/p.py:consumer" in loaded(server), "qualified sibling pipeline repair"
            )
            assert not any("sub/p.py:consumer" in e["affected_nodes"] for e in errors(server))
        else:
            # The failed preferred sibling still prevents fallback to the
            # healthy root candidate with the same name.
            assert "shared.py:value" in loaded(server)
        assert "inspection imported user code" not in server.log.read_text()
    finally:
        server.stop()


def test_mixed_partition_dimensions_are_isolated_before_expression_import(tmp_path):
    (tmp_path / "barca.toml").write_text("")
    (tmp_path / "pipeline.py").write_text("""from pathlib import Path
from barca import asset, partitions, partitions_from
Path("imported").touch()

@asset()
def keys(): return ["x"]

@asset(partitions={"key": partitions_from(keys), "tier": partitions([str(i) for i in range(2)])})
def mixed(key, tier): return key + tier

@asset()
def healthy(): return 7
""")
    server = Server(tmp_path)
    try:
        assert loaded(server) == {"pipeline.py:keys", "pipeline.py:healthy"}
        diagnostics = errors(server)
        assert any(
            e["affected_nodes"] == ["pipeline.py:mixed"]
            and "mixing partitions() and partitions_from()" in e["error"]
            for e in diagnostics
        )
        assert not (tmp_path / "imported").exists()
    finally:
        server.stop()
