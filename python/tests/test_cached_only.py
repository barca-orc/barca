"""Completed artifact reads never execute, enqueue or publish (#1085)."""

import json
import os
import sqlite3
from pathlib import Path

import pytest
import barca as api
from .helpers import barca

PIPELINE = """
from pathlib import Path
from barca import asset, sensor, partitions

@sensor()
def source():
    p = Path("counter")
    n = int(p.read_text()) + 1 if p.exists() else 1
    p.write_text(str(n))
    return n

@asset(inputs={"value": source})
def result(value):
    return {"value": value}

@asset(partitions={"region": partitions(["a", "b", "c"])})
def keyed(region):
    return region
"""


def cli(project, *args, env=None):
    return barca(project, *args, env={**os.environ, "BARCA_REMOTE": "off", **(env or {})})


def doc(proc, code=0):
    assert proc.returncode == code, proc.stderr
    return json.loads(proc.stdout.strip().splitlines()[-1])


def cached(project, target="result", *args, code=0, env=None):
    return doc(
        cli(
            project,
            "get",
            target,
            "pipeline.py",
            "--local",
            "--cached-only=local",
            "--json",
            *args,
            env=env,
        ),
        code,
    )


def records(project):
    with sqlite3.connect(project / ".barca/metadata.db") as db:
        return db.execute("select count(*) from runs").fetchone()[0]


def test_stale_read_never_imports_or_executes(project):
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    count = records(project)
    (project / "pipeline.py").write_text(
        PIPELINE.replace('return {"value": value}', 'return {"value": value + 100}')
        + '\nraise RuntimeError("must not import")\n'
    )
    body = cached(project, "result", "-o", "value")
    assert body["final_output"] == {"value": 1}
    assert body["outcomes"][0]["fresh"] is False
    assert body["outcomes"][0]["source"] == "disk"
    assert (project / "counter").read_text() == "1"
    assert records(project) == count


def test_never_run_is_typed_and_does_not_create_run(project, monkeypatch):
    body = cached(project, code=1)
    assert body["error"]["kind"] == "cache_miss"
    assert body["outcomes"][0]["reason"] == "never_run"
    assert not (project / "counter").exists()
    monkeypatch.chdir(project)
    monkeypatch.setenv("BARCA_REMOTE", "off")
    with pytest.raises(api.CacheMiss) as exc:
        api.get("result", "pipeline.py", cached_only="local")
    assert exc.value.code == 1
    assert exc.value.missing[0]["reason"] == "never_run"


def test_selected_keys_report_hits_and_misses_without_widening(project):
    doc(cli(project, "get", "keyed", "pipeline.py", "--partition", "region=a", "--local", "--json"))
    body = cached(project, "keyed", "--partition", "region=a,b", code=1)
    assert [(o["partition"], o["status"]) for o in body["outcomes"]] == [
        ("region=a", "hit"),
        ("region=b", "miss"),
    ]
    assert len(body["error"]["outcomes"]) == 2


def test_eviction_corruption_and_environment_isolation(project):
    seed = doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    path = project / seed["final_output"]["_barca_artifact"]["path"]
    body = cached(project, "result", "--env", "isolated", code=1)
    assert body["outcomes"][0]["reason"] == "never_run"
    path.write_text("corrupt")
    proc = cli(project, "get", "result", "pipeline.py", "--cached-only=local", "--local", "--json")
    assert proc.returncode == 3, proc.stderr
    assert "corruption" in proc.stderr
    path.unlink()
    assert cached(project, code=1)["outcomes"][0]["reason"] == "evicted"


def test_python_hits_and_stable_path_after_replacement(project, monkeypatch):
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    old = cached(project)["final_output"]["_barca_artifact"]["path"]
    doc(cli(project, "get", "result", "pipeline.py", "--refresh-all", "--local", "--json"))
    assert json.loads(Path(old).read_text()) == {"value": 1}
    monkeypatch.chdir(project)
    monkeypatch.setenv("BARCA_REMOTE", "off")
    assert api.get("result", "pipeline.py", cached_only="local") == {"value": 2}


def test_dynamic_explicit_keys_do_not_import(project):
    source = PIPELINE.replace(
        'partitions(["a", "b", "c"])', 'partitions([x for x in ["a", "b", "c"]])'
    )
    (project / "pipeline.py").write_text(source)
    doc(cli(project, "get", "keyed", "pipeline.py", "--partition", "region=a", "--local", "--json"))
    (project / "pipeline.py").write_text(source + '\nraise RuntimeError("must not import")\n')
    body = cached(project, "keyed", "--partition", "region=a,b", code=1)
    assert [o["status"] for o in body["outcomes"]] == ["hit", "miss"]


def test_store_tier_local_exclusion_corruption_and_read_stamp(project, tmp_path):
    from .helpers import clean_env

    store = tmp_path / "store"
    env = clean_env(BARCA_REMOTE_URI=str(store))
    seed = doc(barca(project, "get", "result", "pipeline.py", "--local", "--json", env=env))
    local = project / seed["final_output"]["_barca_artifact"]["path"]
    local.unlink()
    strict = doc(
        barca(
            project,
            "get",
            "result",
            "pipeline.py",
            "--local",
            "--cached-only=local",
            "--json",
            env=env,
        ),
        1,
    )
    assert strict["outcomes"][0]["reason"] == "not_fetched"
    assert not local.exists()
    answer = doc(
        barca(
            project,
            "get",
            "result",
            "pipeline.py",
            "--local",
            "--cached-only",
            "-o",
            "value",
            "--json",
            env=env,
        )
    )
    assert answer["final_output"] == {"value": 1}
    assert answer["outcomes"][0]["source"] == "store"
    with sqlite3.connect(project / ".barca/metadata.db") as db:
        stored, stamp = db.execute(
            "select artifact_path, last_read_at from materializations where node_id = 'pipeline.py:result' and status = 'success' order by id desc limit 1"
        ).fetchone()
    assert stamp
    local.unlink()
    Path(stored).write_text("corrupt")
    proc = barca(
        project, "get", "result", "pipeline.py", "--local", "--cached-only", "--json", env=env
    )
    assert proc.returncode == 3, proc.stderr
    assert "corruption" in proc.stderr
    assert (project / "counter").read_text() == "1"


def test_server_cli_python_and_direct_http_agree(project, serve, monkeypatch):
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    count = records(project)
    server = serve("--read-only", root=project)
    request = {"targets": ["result"], "tier": "local", "values": True}
    status, body = server.request("POST", "/cached", request)
    assert status == 200, body
    assert body["final_output"] == {"value": 1}
    assert body["path_scope"] == "server"
    proc = cli(
        project,
        "get",
        "result",
        "--server",
        server.url,
        "--cached-only=local",
        "-o",
        "value",
        "--json",
    )
    assert doc(proc)["final_output"] == body["final_output"]
    monkeypatch.chdir(project)
    assert api.get("result", cached_only="local", server=server.url) == {"value": 1}
    request.update(targets=["keyed"], partition="region=a,b")
    status, body = server.request("POST", "/cached", request)
    assert status == 404 and body["error"]["kind"] == "cache_miss"
    assert len(body["outcomes"]) == 2
    with pytest.raises(api.CacheMiss) as exc:
        api.get("keyed", cached_only="local", server=server.url, partition="region=a,b")
    assert len(exc.value.outcomes) == 2
    assert records(project) == count
    assert (project / "counter").read_text() == "1"


def test_failed_and_in_progress_attempts_leave_latest_success(project):
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    (project / "pipeline.py").write_text(
        PIPELINE.replace('return {"value": value}', 'raise ValueError("intentional failure")')
    )
    proc = cli(project, "get", "result", "pipeline.py", "--refresh-all", "--local", "--json")
    assert proc.returncode == 1
    with sqlite3.connect(project / ".barca/metadata.db") as db:
        db.execute("update materializations set status = 'running' where status = 'failed'")
    assert cached(project, "result", "-o", "value")["final_output"] == {"value": 1}
    with sqlite3.connect(project / ".barca/metadata.db") as db:
        db.execute("update materializations set status = 'failed' where status = 'running'")
    assert cached(project, "result", "-o", "value")["final_output"] == {"value": 1}


def test_explicit_unknown_and_compound_keys_are_not_dropped(project):
    body = cached(project, "keyed", "--partition", "region=unknown", code=1)
    assert body["outcomes"][0]["partition"] == "region=unknown"
    source = PIPELINE.replace(
        '"region": partitions(["a", "b", "c"])',
        '"region": partitions(["a", "b"]), "day": partitions(["1", "2"])',
    ).replace("def keyed(region):", "def keyed(region, day):")
    (project / "pipeline.py").write_text(source)
    body = cached(
        project,
        "keyed",
        "--partition",
        '[{"region":"a","day":"1"},{"region":"b","day":"2"}]',
        code=1,
    )
    assert [o["partition"] for o in body["outcomes"]] == ["day=1,region=a", "day=2,region=b"]


def test_http_schema_and_no_store_access_on_strict_miss(project, serve):
    from .test_http_openapi_client import validate

    server = serve("--read-only", root=project)
    status, body = server.request(
        "POST", "/cached", {"targets": ["keyed"], "tier": "local", "partition": "region=a,b"}
    )
    assert status == 404
    validate({"$ref": "#/components/schemas/CachedAnswer"}, body)
    assert len(body["outcomes"]) == 2


def test_artifact_outside_environment_is_infra(project, tmp_path):
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    other = tmp_path / "outside.json"
    other.write_text('{"value":1}')
    with sqlite3.connect(project / ".barca/metadata.db") as db:
        db.execute(
            "update materializations set artifact_path = ? where node_id = 'pipeline.py:result'",
            (str(other),),
        )
    proc = cli(project, "get", "result", "pipeline.py", "--local", "--cached-only=local", "--json")
    assert proc.returncode == 3
    assert "outside this environment" in proc.stderr


@pytest.mark.parametrize("failure", ["PermissionError", "ConnectionError"])
def test_store_failures_are_not_misses_and_strict_mode_makes_zero_calls(project, tmp_path, failure):
    from .helpers import clean_env

    store = tmp_path / "store"
    env = clean_env(BARCA_REMOTE_URI=str(store))
    seed = doc(barca(project, "get", "result", "pipeline.py", "--local", "--json", env=env))
    local = project / seed["final_output"]["_barca_artifact"]["path"]
    hook = tmp_path / "hooks"
    hook.mkdir()
    marker = tmp_path / "store-calls"
    (hook / "sitecustomize.py").write_text(f"""
from pathlib import Path
from barca import _storage

def forbidden(*args, **kwargs):
    Path({str(marker)!r}).write_text("called")
    raise {failure}("injected storage failure")

_storage.get_file = forbidden
_storage.check_store = forbidden
""")
    env.update(PYTHONPATH=str(hook), BARCA_TRANSFER_RETRIES="1")
    # Disk hits, even default tier, do not initialize or consult storage.
    doc(
        barca(
            project, "get", "result", "pipeline.py", "--local", "--cached-only", "--json", env=env
        )
    )
    assert not marker.exists()
    local.unlink()
    doc(
        barca(
            project,
            "get",
            "result",
            "pipeline.py",
            "--local",
            "--cached-only=local",
            "--json",
            env=env,
        ),
        1,
    )
    assert not marker.exists()
    proc = barca(
        project, "get", "result", "pipeline.py", "--local", "--cached-only", "--json", env=env
    )
    assert proc.returncode == 3, proc.stderr
    assert '"kind":"infra"' in proc.stderr.replace(" ", "")
    assert marker.exists()
    assert (project / "counter").read_text() == "1"


def test_racing_atomic_replacement_and_eviction_returns_complete_value_or_miss(project):
    import threading

    seed = doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    path = project / seed["final_output"]["_barca_artifact"]["path"]
    content = path.read_bytes()
    stop = threading.Event()
    failures = []

    def replace_and_evict():
        try:
            while not stop.is_set():
                staged = path.with_suffix(".replacement")
                staged.write_bytes(content)
                staged.replace(path)
                path.unlink(missing_ok=True)
        except Exception as exc:
            failures.append(exc)

    writer = threading.Thread(target=replace_and_evict)
    writer.start()
    try:
        for _ in range(20):
            proc = cli(
                project,
                "get",
                "result",
                "pipeline.py",
                "--local",
                "--cached-only=local",
                "-o",
                "value",
                "--json",
            )
            assert proc.returncode in (0, 1), proc.stderr
            body = doc(proc, proc.returncode)
            if proc.returncode == 0:
                assert body["final_output"] == {"value": 1}
            else:
                assert body["outcomes"][0]["reason"] == "evicted"
    finally:
        stop.set()
        writer.join(timeout=5)
    assert not writer.is_alive() and not failures


def test_cross_filesystem_artifacts_use_verified_private_copy(project):
    import tempfile
    import shutil

    if not Path("/dev/shm").is_dir() or Path("/dev/shm").stat().st_dev == project.stat().st_dev:
        pytest.skip("needs a separate filesystem for EXDEV coverage")
    with tempfile.TemporaryDirectory(prefix="barca-cached-", dir="/dev/shm") as mount:
        doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
        artifacts = project / ".barca/artifacts"
        destination = Path(mount) / "artifacts"
        shutil.move(str(artifacts), destination)
        artifacts.symlink_to(destination, target_is_directory=True)
        body = cached(project, "result", "-o", "value")
        assert body["final_output"] == {"value": 1}
        assert Path(body["outcomes"][0]["path"]).stat().st_dev == project.stat().st_dev


@pytest.mark.parametrize("python_api", [False, True])
def test_active_value_read_survives_gc_after_grace_has_elapsed(project, tmp_path, python_api):
    import subprocess
    import time
    from barca.api import _find_binary
    from .helpers import clean_env, wait_until

    (project / "pipeline.py").write_text(
        'from barca import asset\n@asset(serializer="pickle")\ndef result():\n    return {"value": 1}\n'
    )
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    hook = tmp_path / "hooks"
    hook.mkdir()
    marker, release = tmp_path / "reading", tmp_path / "release"
    (hook / "sitecustomize.py").write_text(f"""
from pathlib import Path
import time
from barca import _cached_value
original = _cached_value.deserialize

def held(path, *args, **kwargs):
    Path({str(marker)!r}).write_text(str(path))
    deadline = time.monotonic() + 30
    while not Path({str(release)!r}).exists():
        if time.monotonic() > deadline:
            raise TimeoutError("test reader was not released")
        time.sleep(.01)
    return original(path, *args, **kwargs)

_cached_value.deserialize = held
""")
    command = [
        _find_binary(),
        "get",
        "result",
        "pipeline.py",
        "--local",
        "--cached-only=local",
        "-o",
        "value",
        "--json",
    ]
    if python_api:
        import sys

        command = [
            sys.executable,
            "-c",
            'import json, barca; print(json.dumps({"final_output": barca.get("result", "pipeline.py", cached_only="local")}))',
        ]
    proc = subprocess.Popen(
        command,
        cwd=project,
        env=clean_env(PYTHONPATH=str(hook)),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait_until(marker.exists, "cached reader to hold its artifact", timeout=10, proc=proc)
        active = Path(marker.read_text())
        assert "/staging/" in str(active)
        old = time.time() - 3600
        os.utime(active, (old, old))
        collected = cli(project, "gc", "--max-size", "1", "--local", "--json")
        assert collected.returncode == 0, collected.stderr
        json.loads(collected.stdout)
        assert active.exists()
        release.touch()
        stdout, stderr = proc.communicate(timeout=10)
        assert proc.returncode == 0, stderr
        assert json.loads(stdout)["final_output"] == {"value": 1}
    finally:
        release.touch()
        if proc.poll() is None:
            proc.terminate()
            proc.wait(timeout=10)


@pytest.mark.parametrize("reducer", [False, True])
def test_cached_pickle_values_never_import_project_or_execute_reducers(
    project, serve, monkeypatch, reducer
):
    source = """
from pathlib import Path
from barca import asset
p = Path("imports")
p.write_text(str(int(p.read_text()) + 1) if p.exists() else "1")
class Record:
    pass
@asset(serializer="pickle")
def result():
    return Record()
"""
    if reducer:
        source = source.replace(
            "    pass",
            """    def __reduce__(self):
        return eval, ("__import__('pathlib').Path('reduced').write_text('executed')",)""",
        )
    (project / "pipeline.py").write_text(source)
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    # Ordinary execution may deserialize the seeded result; measure only cached reads.
    (project / "reduced").unlink(missing_ok=True)
    imports = (project / "imports").read_text()
    assert cached(project)["outcomes"][0]["status"] == "hit"
    proc = cli(
        project,
        "get",
        "result",
        "pipeline.py",
        "--local",
        "--cached-only=local",
        "-o",
        "value",
        "--json",
    )
    assert proc.returncode == 3, proc.stderr
    error = json.loads(proc.stderr.strip().splitlines()[-1])
    assert error["reason"] == "cached_value_unsupported"
    monkeypatch.chdir(project)
    monkeypatch.setenv("BARCA_REMOTE", "off")
    with pytest.raises(api.BarcaError) as exc:
        api.get("result", "pipeline.py", cached_only="local")
    assert exc.value.reason == "cached_value_unsupported"
    server = serve("--read-only", root=project)
    status, body = server.request(
        "POST", "/cached", {"targets": ["result"], "tier": "local", "values": True}
    )
    assert status == 500 and body["reason"] == "cached_value_unsupported"
    assert (project / "imports").read_text() == imports
    assert not (project / "reduced").exists()


@pytest.mark.parametrize("partitioned", [False, True])
def test_latest_success_freshness_does_not_borrow_an_older_matching_version(project, partitioned):
    decoration = '@asset(partitions={"k": partitions(["a"])})' if partitioned else "@asset()"
    args = "k" if partitioned else ""
    source = (
        f"from barca import asset, partitions\n{decoration}\ndef result({args}):\n    return 1\n"
    )
    (project / "pipeline.py").write_text(source)
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    (project / "pipeline.py").write_text(source.replace("return 1", "return 2"))
    doc(cli(project, "get", "result", "pipeline.py", "--local", "--json"))
    (project / "pipeline.py").write_text(source)
    body = cached(project, "result", "-o", "value")
    assert body["final_output"] == ({"k=a": 2} if partitioned else 2)
    assert body["outcomes"][0]["fresh"] is False
