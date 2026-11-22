"""Ordinary fresh reads bypass saturated pipeline admission (#1133)."""

import pytest

from .helpers import barca

PIPELINE = """
from pathlib import Path
import time
from barca import asset, task

@asset()
def ready():
    return 42

@task()
def blocked():
    Path("started").touch()
    while not Path("release").exists():
        time.sleep(0.05)
"""


@pytest.mark.parametrize("store_only", [False, True])
def test_cached_get_bypasses_saturated_execution(project, serve, tmp_path, store_only):
    import json
    from .helpers import clean_env

    env = clean_env(BARCA_REMOTE_URI=str(tmp_path / "store")) if store_only else clean_env()
    seed = barca(project, "get", "ready", "pipeline.py", "--local", "--json", env=env)
    assert seed.returncode == 0, seed.stderr
    if store_only:
        (project / json.loads(seed.stdout)["final_output"]["_barca_artifact"]["path"]).unlink()
        import sqlite3

        with sqlite3.connect(project / ".barca/metadata.db") as db:
            db.execute("update materializations set last_read_at = NULL")
    server = serve("--no-schedule", "--max-runs", "1", "--capacity", "1", root=project, env=env)
    status, running = server.request("POST", "/runs", {"command": "run", "targets": ["blocked"]})
    assert status == 202, running
    server.wait("blocked pipeline", lambda: (project / "started").exists())
    try:
        status, read = server.request(
            "POST", "/runs", {"command": "get", "targets": ["ready"], "values": True}
        )
        assert status == 202, read

        def completed():
            _, view = server.request("GET", f"/runs/{read['run_id']}")
            return view if view["status"] == "success" else None

        result = server.wait("cached read while pipeline remains blocked", completed, timeout=3)
        assert result["outcome"]["final_output"] == 42
        assert result["outcome"]["steps_executed"] == 0
        assert not (project / "release").exists()
    finally:
        (project / "release").touch()
    if store_only:
        server.stop()
        with sqlite3.connect(project / ".barca/metadata.db") as db:
            stamp = db.execute(
                "select last_read_at from materializations where node_id = 'pipeline.py:ready' and status = 'success'"
            ).fetchone()[0]
        assert stamp is not None


@pytest.mark.parametrize("control", ["miss", "stale", "refresh_all", "refresh"])
def test_miss_stale_and_refresh_still_wait(project, serve, control):
    import json
    import time

    seed = barca(project, "get", "ready", "pipeline.py", "--local", "--json")
    assert seed.returncode == 0, seed.stderr
    artifact = json.loads(seed.stdout)["final_output"]["_barca_artifact"]["path"]
    server = serve("--no-schedule", "--max-runs", "1", "--capacity", "1", root=project)
    server.request("POST", "/runs", {"command": "run", "targets": ["blocked"]})
    server.wait("blocked pipeline", lambda: (project / "started").exists())
    try:
        options = {}
        if control == "stale":
            (project / "pipeline.py").write_text(PIPELINE.replace("return 42", "return 43"))
        elif control == "miss":
            (project / artifact).unlink()
        elif control == "refresh_all":
            options["refresh_all"] = True
        else:
            options["refresh"] = ["ready"]
        status, request = server.request(
            "POST", "/runs", {"command": "get", "targets": ["ready"], **options}
        )
        assert status == 202 and request["status"] == "pending", request
        time.sleep(0.2)
        _, view = server.request("GET", f"/runs/{request['run_id']}")
        assert view["status"] == "pending"
    finally:
        (project / "release").touch()


def test_reads_are_bounded_and_survive_concurrent_server_gc(project, serve, tmp_path):
    import os
    import time
    from concurrent.futures import ThreadPoolExecutor
    from pathlib import Path

    from .helpers import clean_env

    (project / "pipeline.py").write_text(
        PIPELINE.replace("@asset()", '@asset(serializer="pickle")')
    )
    assert barca(project, "get", "ready", "pipeline.py", "--local").returncode == 0
    hooks = tmp_path / "hooks"
    hooks.mkdir()
    markers = tmp_path / "readers"
    markers.mkdir()
    release = tmp_path / "read-release"
    (hooks / "sitecustomize.py").write_text(f"""
import os
import time
from pathlib import Path
from barca import _cached_value
original = _cached_value.deserialize

def held(path, *args, **kwargs):
    Path({str(markers)!r}, str(os.getpid())).write_text(str(path))
    deadline = time.monotonic() + 20
    while not Path({str(release)!r}).exists():
        if time.monotonic() > deadline:
            raise TimeoutError("reader not released")
        time.sleep(.01)
    return original(path, *args, **kwargs)

_cached_value.deserialize = held
""")
    server = serve("--no-schedule", root=project, env=clean_env(PYTHONPATH=str(hooks)))
    with ThreadPoolExecutor(max_workers=5) as pool:
        futures = [
            pool.submit(
                server.request,
                "POST",
                "/runs",
                {"command": "get", "targets": ["ready"], "values": True},
            )
            for _ in range(4)
        ]
        try:
            server.wait(
                "four retained reads", lambda: len(list(markers.iterdir())) == 4, timeout=10
            )
            fifth = pool.submit(
                server.request,
                "POST",
                "/cached",
                {"targets": ["ready"], "tier": "local", "values": True},
            )
            time.sleep(0.2)
            assert len(list(markers.iterdir())) == 4
            assert not fifth.done()
            retained = [Path(marker.read_text()) for marker in markers.iterdir()]
            for path in retained:
                assert "/staging/" in str(path)
                old = time.time() - 3600
                os.utime(path, (old, old))
            status, gc = server.request("POST", "/gc", {"max_size": "1"})
            assert status == 200, gc
            assert all(path.exists() for path in retained)
        finally:
            release.touch()
        for future in futures:
            status, read = future.result(timeout=10)
            assert status == 202, read
            _, result = server.request("GET", f"/runs/{read['run_id']}")
            assert result["outcome"]["final_output"] == 42
            assert result["outcome"]["steps_executed"] == 0
        assert fifth.result(timeout=10)[0] == 200


def test_python_partition_and_multi_target_reads_keep_their_shape(project, serve, monkeypatch):
    import barca as api

    source = (
        PIPELINE
        + """
from barca import partitions
@asset(partitions={"region": partitions(["a", "b"])})
def keyed(region):
    return region
@asset()
def other():
    return 7
"""
    )
    (project / "pipeline.py").write_text(source)
    for target in ["ready", "other", "keyed"]:
        assert barca(project, "get", target, "pipeline.py", "--local").returncode == 0
    server = serve("--no-schedule", "--max-runs", "1", "--capacity", "1", root=project)
    server.request("POST", "/runs", {"command": "run", "targets": ["blocked"]})
    server.wait("blocked pipeline", lambda: (project / "started").exists())
    # Static validation may read this source, but neither import nor evaluate it.
    (project / "pipeline.py").write_text(source + '\nraise RuntimeError("read imported project")\n')
    monkeypatch.chdir(project)
    try:
        assert api.get("ready", server=server.url) == 42
        assert api.get("keyed", server=server.url, partition="region=b") == {"region=b": "b"}
        status, read = server.request(
            "POST", "/runs", {"command": "get", "targets": ["ready", "other"], "values": True}
        )
        assert status == 202, read
        _, result = server.request("GET", f"/runs/{read['run_id']}")
        assert result["outcome"]["targets"]["ready"]["final_output"] == 42
        assert result["outcome"]["targets"]["other"]["final_output"] == 7
        status, read = server.request(
            "POST", "/runs", {"command": "get", "targets": ["keyed", "ready"], "values": True}
        )
        assert status == 202, read
        _, result = server.request("GET", f"/runs/{read['run_id']}")
        assert result["outcome"]["targets"]["keyed"]["final_output"] == {
            "region=a": "a",
            "region=b": "b",
        }
        assert result["outcome"]["targets"]["ready"]["final_output"] == 42
    finally:
        (project / "release").touch()


@pytest.mark.parametrize("dependency", ["sensor", "dynamic"])
def test_unresolved_freshness_requires_execution(project, serve, dependency):
    extra = (
        """
from barca import sensor
@sensor()
def source():
    return 1
@asset(inputs={"value": source})
def unresolved(value):
    return value
"""
        if dependency == "sensor"
        else """
from barca import partitions
@asset(partitions={"region": partitions([x for x in ["a", "b"]])})
def unresolved(region):
    return region
"""
    )
    (project / "pipeline.py").write_text(PIPELINE + extra)
    assert barca(project, "get", "unresolved", "pipeline.py", "--local").returncode == 0
    server = serve("--no-schedule", "--max-runs", "1", "--capacity", "1", root=project)
    server.request("POST", "/runs", {"command": "run", "targets": ["blocked"]})
    server.wait("blocked pipeline", lambda: (project / "started").exists())
    try:
        status, read = server.request(
            "POST", "/runs", {"command": "get", "targets": ["unresolved"]}
        )
        assert status == 202 and read["status"] == "pending", read
    finally:
        (project / "release").touch()


def test_whole_file_and_large_static_selections_bypass_admission(project, serve):
    import json

    # LISTED_KEYS is 1000. This domain spans eight metadata pages and exceeds that
    # presentation limit; explicit selection must still return every selected value.
    domain = list(range(1001))
    source = (
        PIPELINE
        + f"""
from barca import partitions
@asset(partitions={{"key": partitions({domain!r})}})
def keyed(key):
    return key
"""
    )
    (project / "pipeline.py").write_text(source)
    seed = barca(project, "get", "pipeline.py", "--local", "--json")
    assert seed.returncode == 0, seed.stderr
    warm = barca(project, "get", "pipeline.py", "--local", "--json", "-o", "value")
    assert warm.returncode == 0, warm.stderr
    expected_whole = json.loads(warm.stdout)["final_output"]
    server = serve("--no-schedule", "--max-runs", "1", "--capacity", "1", root=project)
    server.request("POST", "/runs", {"command": "run", "targets": ["blocked"]})
    server.wait("blocked pipeline", lambda: (project / "started").exists())
    try:
        for targets, partition in [
            ([], None),
            (["keyed"], None),
            (["keyed"], json.dumps({"key": domain})),
        ]:
            status, read = server.request(
                "POST",
                "/runs",
                {"command": "get", "targets": targets, "partition": partition, "values": True},
            )
            assert status == 202, read
            _, result = server.request("GET", f"/runs/{read['run_id']}")
            assert result["status"] == "success", result
            output = result["outcome"]["final_output"]
            if not targets:
                assert output == expected_whole
            elif partition is None:
                assert output["_barca_partitions"]["keys"] == 1001
                assert output["_barca_partitions"]["computed"] == 0
            else:
                assert output == {f"key={key}": str(key) for key in domain}
            assert result["outcome"]["steps_executed"] == 0
    finally:
        (project / "release").touch()
