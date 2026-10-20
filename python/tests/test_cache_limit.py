"""The size limit for `.barca/` (RFC-0012, #457), end to end on the real binary.

The eviction order itself is a pure function with its own tests (`crates/barca-core/src/gc/`);
these check what a user sees: which files go, which stay, what comes back and what is said.
"""

import hashlib
import json
import os
import re
import socket
import sqlite3
import subprocess
import time
import urllib.request
from contextlib import closing
from pathlib import Path

import pytest
from barca.api import _find_binary

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

PIPELINE = """
import time
from pathlib import Path

from barca import asset, task, Manual

VERSION = 3


@asset()
def source() -> list:
    return list(range(VERSION))


@asset(inputs={"s": source})
def features(s: list) -> list:
    return [x * 2 for x in s]


@asset(inputs={"f": features})
def report(f: list) -> dict:
    return {"sum": sum(f)}


@asset()
def other() -> dict:
    return {"other": [1] * 50}


@asset(freshness=Manual)
def reference() -> dict:
    return {"ref": [1] * 50}


# `_r` orders the task after `report` without loading it.
@task(inputs={"_r": report})
def slow(_r: dict) -> None:
    Path("slow.started").write_text("")
    deadline = time.time() + 60
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
"""


def env(**extra: str) -> dict[str, str]:
    out = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    out["BARCA_PROGRESS_SECS"] = "0"
    out.update(extra)
    return out


def barca(cwd: Path, *args: str, code: int = 0, **extra: str) -> subprocess.CompletedProcess:
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env(**extra),
        timeout=120,
    )
    assert proc.returncode == code, (
        f"barca {' '.join(args)}: exit {proc.returncode}\nstdout: {proc.stdout}\n"
        f"stderr: {proc.stderr}"
    )
    return proc


def gc(cwd: Path, *args: str, **extra: str) -> dict:
    return json.loads(barca(cwd, "gc", "--json", *args, **extra).stdout)


def rows(cwd: Path, sql: str, *params) -> list[tuple]:
    with closing(sqlite3.connect(cwd / ".barca/metadata.db")) as conn:
        return conn.execute(sql, params).fetchall()


def execute(cwd: Path, sql: str, *params) -> None:
    with closing(sqlite3.connect(cwd / ".barca/metadata.db")) as conn:
        conn.execute(sql, params)
        conn.commit()


def current(cwd: Path, node: str) -> str:
    """The artifact path of the newest successful result of `node`."""
    ((path,),) = rows(
        cwd,
        "SELECT artifact_path FROM materializations WHERE node_id = ? AND status = 'success' "
        "ORDER BY id DESC LIMIT 1",
        f"pipeline.py:{node}",
    )
    return path


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    return tmp_path


def recompute_with_new_source(cwd: Path) -> None:
    """Every asset computed, then `source` (and so `features` and `report`) under new hashes:
    their first results are superseded."""
    barca(cwd, "get", "pipeline.py", "--json")
    text = (cwd / "pipeline.py").read_text().replace("VERSION = 3", "VERSION = 4")
    (cwd / "pipeline.py").write_text(text)
    barca(cwd, "get", "pipeline.py", "--json")


def moved_aside(cwd: Path) -> Path:
    aside = cwd / ".barca/artifacts/pipeline.py--source/old.json.moved-aside"
    aside.mkdir(parents=True)
    (aside / "junk").write_text("x" * 100)
    return aside


# ─── the order, and where it stops ──────────────────────────────────────────


def test_tiers_go_in_order_and_eviction_stops_at_80_percent(project):
    recompute_with_new_source(project)
    aside = moved_aside(project)
    superseded = {
        p
        for (p,) in rows(
            project,
            "SELECT artifact_path FROM materializations WHERE node_id IN "
            "('pipeline.py:source', 'pipeline.py:features', 'pipeline.py:report') "
            "AND status = 'success' AND id NOT IN (SELECT MAX(id) FROM materializations "
            "GROUP BY node_id)",
        )
    }
    assert len(superseded) == 3

    free = gc(project, "--max-size", "off", "--dry-run")
    assert free["limit_bytes"] is None
    assert [t["tier"] for t in free["tiers"]] == ["leftovers", "superseded"]
    assert free["evicted"][0]["path"] == str(aside.relative_to(project))
    assert {e["path"] for e in free["evicted"][1:]} == superseded

    # A limit whose low watermark is just below what tiers 1 and 2 leave: one current result
    # must go too, and nothing after it.
    target = free["after_bytes"] - 1
    limit = target * 100 // 80 + 1
    plan = gc(project, "--max-size", str(limit), "--dry-run")
    tiers = [e["tier"] for e in plan["evicted"]]
    order = ["leftovers", "superseded", "store_held", "current"]
    assert tiers == sorted(tiers, key=order.index)
    assert tiers.count("current") >= 1
    assert plan["after_bytes"] <= plan["target_bytes"]
    assert plan["after_bytes"] + plan["evicted"][-1]["bytes"] > plan["target_bytes"]
    assert plan["target_bytes"] == limit * 80 // 100


def test_a_dry_run_lists_exactly_what_gc_then_deletes(project):
    recompute_with_new_source(project)
    moved_aside(project)
    predicted = gc(project, "--max-size", "1", "--dry-run")
    assert predicted["dry_run"] is True
    assert all((project / e["path"]).exists() for e in predicted["evicted"])
    done = gc(project, "--max-size", "1")
    assert done["dry_run"] is False
    assert [(e["tier"], e["path"]) for e in done["evicted"]] == [
        (e["tier"], e["path"]) for e in predicted["evicted"]
    ]
    assert not any((project / e["path"]).exists() for e in done["evicted"])


# ─── protections ────────────────────────────────────────────────────────────


def test_a_get_over_the_limit_keeps_what_it_produced_read_and_returned(project):
    barca(project, "get", "pipeline.py", "--json")
    other = current(project, "other")
    reference = current(project, "reference")
    out = barca(
        project, "get", "report", "pipeline.py", "-o", "value", "--json", BARCA_CACHE_MAX_SIZE="1"
    )
    result = json.loads(out.stdout)
    assert result["final_output"] == {"sum": 6}
    assert set(result["cache"]) == {"size_bytes", "limit_bytes", "evicted_files", "evicted_bytes"}
    assert result["cache"]["limit_bytes"] == 1
    assert result["cache"]["evicted_files"] >= 1
    assert re.search(r"\[barca\] cache .* over its 1 B limit; evicted \d+ files? ", out.stderr)
    # The command's own cone (returned, and read from cache) is still there.
    for node in ("source", "features", "report"):
        assert (project / current(project, node)).is_file(), node
    # `reference` is Manual and the project has no store: nothing would ever recompute it.
    assert (project / reference).is_file()
    # `other` was neither read nor returned: evicted, and recorded as such.
    assert not (project / other).exists()
    ((evicted_at,),) = rows(
        project, "SELECT evicted_at FROM materializations WHERE artifact_path = ?", other
    )
    assert evicted_at is not None
    # Nothing is evicted when nothing is over the limit, and then there is no `cache` key.
    again = json.loads(barca(project, "get", "report", "pipeline.py", "--json").stdout)
    assert "cache" not in again


def test_a_live_run_in_another_process_leaves_all_but_tiers_1_and_2(project):
    recompute_with_new_source(project)
    moved_aside(project)
    slow = subprocess.Popen(
        [_find_binary(), "run", "slow", "pipeline.py", "--json"],
        cwd=project,
        env=env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 60
        while not (project / "slow.started").exists():
            assert slow.poll() is None, slow.communicate()
            assert time.monotonic() < deadline, "the task never started"
            time.sleep(0.05)
        predicted = gc(project, "--max-size", "1", "--dry-run")
        done = gc(project, "--max-size", "1")
        assert done["restricted"] and "in progress" in done["restricted"]
        assert {e["tier"] for e in done["evicted"]} == {"leftovers", "superseded"}
        assert [e["path"] for e in done["evicted"]] == [e["path"] for e in predicted["evicted"]]
        for node in ("source", "features", "report", "other", "reference"):
            assert (project / current(project, node)).is_file(), node
    finally:
        (project / "release").write_text("")
        _, err = slow.communicate(timeout=60)
    assert slow.returncode == 0, err
    # The run is over: the rest may go.
    assert "current" in {e["tier"] for e in gc(project, "--max-size", "1")["evicted"]}


# ─── what comes back ────────────────────────────────────────────────────────


def test_evicted_results_come_back_with_the_same_bytes_and_say_they_were_evicted(project):
    first = json.loads(
        barca(project, "get", "report", "pipeline.py", "-o", "value", "--json").stdout
    )
    files = {node: project / current(project, node) for node in ("source", "features", "report")}
    before = {node: digest(path) for node, path in files.items()}
    done = gc(project, "--max-size", "1")
    assert {e["node"] for e in done["evicted"] if e.get("node")} >= {
        "pipeline.py:source",
        "pipeline.py:features",
        "pipeline.py:report",
    }
    assert not any(path.exists() for path in files.values())

    status = json.loads(barca(project, "status", "report", "pipeline.py", "--json").stdout)
    report = next(n for n in status["nodes"] if n["id"] == "pipeline.py:report")
    assert (report["cache"]["state"], report["cache"]["reason"]) == ("stale", "evicted")
    dry = json.loads(barca(project, "get", "report", "pipeline.py", "--dry-run", "--json").stdout)
    assert {s["reason"] for s in dry["steps"]} == {"evicted"}

    out = barca(project, "get", "report", "pipeline.py", "-o", "value", "--json")
    result = json.loads(out.stdout)
    assert result["final_output"] == first["final_output"]
    assert {s["reason"] for s in result["steps"]} == {"evicted"}
    for node in ("source", "features", "report"):
        line = (
            f"[barca] pipeline.py:{node}: its cached result was evicted to stay under the "
            "cache limit; computing it again"
        )
        assert line in out.stderr
    assert "is missing" not in out.stderr
    # Same run hashes, so the same paths, and the same bytes.
    assert {node: digest(path) for node, path in files.items()} == before


# ─── last_read_at ────────────────────────────────────────────────────────────


def read_at(cwd: Path, node: str) -> str | None:
    ((value,),) = rows(
        cwd,
        "SELECT last_read_at FROM materializations WHERE artifact_path = ? "
        "ORDER BY id DESC LIMIT 1",
        current(cwd, node),
    )
    return value


def test_last_read_at_moves_on_step_input_get_and_sql(project):
    barca(project, "get", "report", "pipeline.py", "--json")
    execute(project, "UPDATE materializations SET last_read_at = NULL")
    # `report` changes: it runs and reads the cached `features`; `source` is not read.
    text = (project / "pipeline.py").read_text()
    (project / "pipeline.py").write_text(text.replace('{"sum": sum(f)}', '{"sum": sum(f) + 1}'))
    barca(project, "get", "report", "pipeline.py", "--json")
    assert read_at(project, "features") is not None
    assert read_at(project, "source") is None
    # `get` returning a cached result reads it.
    execute(project, "UPDATE materializations SET last_read_at = NULL")
    barca(project, "get", "features", "pipeline.py", "--json")
    assert read_at(project, "features") is not None
    assert read_at(project, "report") is None
    # A query names a view.
    pytest.importorskip("duckdb")
    barca(project, "get", "other", "pipeline.py", "--json")
    execute(project, "UPDATE materializations SET last_read_at = NULL")
    barca(project, "sql", "select * from other", "pipeline.py", "--json")
    assert read_at(project, "other") is not None
    assert read_at(project, "features") is None


# ─── configuration ──────────────────────────────────────────────────────────


def test_an_invalid_limit_is_a_usage_error_naming_where_it_came_from(project):
    out = barca(project, "get", "report", "pipeline.py", code=2, BARCA_CACHE_MAX_SIZE="lots")
    assert "BARCA_CACHE_MAX_SIZE: invalid cache size 'lots'" in out.stderr
    (project / "barca.toml").write_text('[cache]\nmax_size = "20XB"\n')
    out = barca(project, "get", "report", "pipeline.py", code=2)
    assert "[cache] max_size: invalid cache size '20XB'" in out.stderr
    out = barca(project, "gc", "--max-size", "nope", code=2)
    assert "--max-size" in out.stderr


def test_barca_toml_sets_the_limit_and_off_turns_it_off(project):
    barca(project, "get", "pipeline.py", "--json")
    (project / "barca.toml").write_text('[cache]\nmax_size = "off"\n')
    result = json.loads(barca(project, "gc", "--json", "--dry-run").stdout)
    assert result["limit_bytes"] is None
    (project / "barca.toml").write_text('[cache]\nmax_size = "1"\n')
    out = barca(project, "get", "report", "pipeline.py", "--json")
    assert json.loads(out.stdout)["cache"]["limit_bytes"] == 1
    # The variable wins over the file.
    out = barca(project, "get", "report", "pipeline.py", "--json", BARCA_CACHE_MAX_SIZE="off")
    assert "cache" not in json.loads(out.stdout)
    doctor = json.loads(barca(project, "doctor", "pipeline.py", "--json").stdout)
    check = next(c for c in doctor["checks"] if c["name"] == "cache_size")
    assert check["ok"] and check["data"]["limit_bytes"] == 1
    assert check["data"]["source"] == "[cache] max_size"


# ─── the check after a command is cheap ─────────────────────────────────────


def test_the_check_after_a_command_is_bounded_at_100k_rows(project):
    barca(project, "get", "report", "pipeline.py", "--json")
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as conn:
        conn.executemany(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, "
            "artifact_size_bytes, status) VALUES (?, ?, ?, 10, 'success')",
            (
                (f"pipeline.py:n{i % 1000}", f"h{i}", f".barca/artifacts/n/h{i}.json")
                for i in range(100_000)
            ),
        )
        conn.commit()
    timings = []
    for _ in range(3):
        out = barca(
            project,
            "get",
            "report",
            "pipeline.py",
            "--json",
            BARCA_CACHE_MAX_SIZE="100GB",
            BARCA_TRACE_TIMING="1",
        )
        (ms,) = re.findall(r"\[trace\] cache size check ([\d.]+)ms", out.stderr)
        timings.append(float(ms))
    # The first check walks everything (there is no measurement yet); the next ones are one
    # aggregate query and a walk of the small directories.
    assert max(timings[1:]) < 1000, timings


# ─── under serve ─────────────────────────────────────────────────────────────


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _request(port: int, method: str, path: str) -> dict:
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", method=method)
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


def _wait(predicate, what: str, timeout: float = 60.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    pytest.fail(f"timed out waiting for {what}")


def test_serve_evicts_after_runs_reports_health_and_warns_about_the_working_set_once(project):
    port = _free_port()
    log = project / "serve.log"
    server = subprocess.Popen(
        [_find_binary(), "serve", "pipeline.py", "--port", str(port), "--no-schedule"],
        cwd=project,
        stdout=subprocess.DEVNULL,
        stderr=log.open("w"),
        env=env(BARCA_CACHE_MAX_SIZE="1"),
    )
    try:

        def health():
            try:
                return _request(port, "GET", "/health")
            except OSError:
                assert server.poll() is None, log.read_text()
                return None

        _wait(health, "the server")
        for _ in range(2):
            run_id = _request(port, "POST", "/get/report")["run_id"]
            _wait(
                lambda run_id=run_id: (
                    _request(port, "GET", f"/status/{run_id}")["status"]
                    in ("complete", "failed", "cancelled")
                ),
                "the run",
            )
            state = _request(port, "GET", f"/status/{run_id}")
            assert state["status"] == "complete", state
            assert state["result"]["cache"]["limit_bytes"] == 1, state
        cache = _wait(lambda: health()["cache"], "a measurement")
        assert cache["limit_bytes"] == 1 and cache["size_bytes"] > 1
    finally:
        server.kill()
        server.wait(timeout=30)
    text = log.read_text()
    assert text.count("cache working set") == 1, text
