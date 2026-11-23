"""A failed refresh must not let retry resurrect pre-refresh downstream results (#939)."""

import json

import pytest

from .helpers import barca, clean_env
from .served import Serve

PIPELINE = """
from pathlib import Path
from barca import asset

@asset(cutoff=False)
def raw() -> str:
    return Path("source.txt").read_text()

@asset(inputs={"value": raw})
def clean(value: str) -> str:
    if Path("fail-clean").exists():
        raise RuntimeError("controlled clean failure")
    with Path("clean-calls").open("a") as log:
        log.write(value + "\\n")
    return value + " clean"

@asset(inputs={"value": clean})
def report(value: str) -> str:
    if Path("fail-report").exists():
        raise RuntimeError("controlled report failure")
    with Path("report-calls").open("a") as log:
        log.write(value + "\\n")
    return value + " report"
"""


DYNAMIC_PIPELINE = """
from pathlib import Path
from barca import asset, collect, partitions_from

@asset
def keys():
    return ["a", "b"]

@asset(partitions={"key": partitions_from(keys)})
def raw(key):
    return Path("source.txt").read_text() + key

@asset(inputs={"value": raw}, partitions={"key": partitions_from(keys)})
def clean(key, value):
    if key == "a" and Path("fail-clean").exists():
        raise RuntimeError("controlled clean failure")
    with Path("clean-" + key).open("a") as log:
        log.write(value + "\\n")
    return value

@asset(inputs={"values": collect(clean)})
def report(values):
    text = ",".join(sorted(values))
    with Path("reports").open("a") as log:
        log.write(text + "\\n")
    return text
"""


@pytest.mark.parametrize("cutoff", [False, True])
@pytest.mark.parametrize("server", [False, True], ids=["cli", "server"])
@pytest.mark.parametrize("fail_retry", [False, True], ids=["retry", "retry-fails-again"])
def test_retry_after_failed_refresh_uses_current_upstream(tmp_path, cutoff, server, fail_retry):
    (tmp_path / "pipeline.py").write_text(
        PIPELINE.replace("cutoff=False", "cutoff=True" if cutoff else "")
    )
    (tmp_path / "source.txt").write_text("old")

    def request(serve, refresh=False):
        if serve is not None:
            body = {"command": "get", "targets": ["report"]}
            if refresh:
                body["refresh"] = ["raw"]
            answer = serve.post("/runs", body)
            return serve.finished(answer["run_id"])
        flags = ["--refresh", "raw"] if refresh else []
        proc = barca(
            tmp_path, "get", "report", "pipeline.py", *flags, env=clean_env(BARCA_POOL_SIZE="2")
        )
        assert proc.returncode in (0, 1), proc.stderr
        return json.loads(proc.stdout)

    def prepare(serve):
        assert request(serve)["status"] == "success"
        (tmp_path / "source.txt").write_text("new")
        (tmp_path / "fail-clean").touch()
        assert request(serve, refresh=True)["status"] == "failed"
        (tmp_path / "fail-clean").unlink()

    def finish(serve):
        if fail_retry:
            (tmp_path / "fail-report").touch()
            result = request(serve)
            assert result["status"] == "failed", result
            assert (tmp_path / "clean-calls").read_text().splitlines() == ["old", "new"]
            (tmp_path / "fail-report").unlink()
        result = request(serve)
        assert result["status"] == "success", result
        assert (tmp_path / "clean-calls").read_text().splitlines() == ["old", "new"]
        assert (tmp_path / "report-calls").read_text().splitlines() == ["old clean", "new clean"]

    if server:
        with Serve(tmp_path, env=clean_env(BARCA_POOL_SIZE="2")) as serve:
            serve.ready()
            prepare(serve)
        # Fresh process and connection: the refresh obligation must be durable.
        with Serve(tmp_path, env=clean_env(BARCA_POOL_SIZE="2")) as serve:
            serve.ready()
            finish(serve)
    else:
        prepare(None)
        finish(None)


@pytest.mark.parametrize("refresh_only_root", [False, True], ids=["no-cascade", "outside-cone"])
def test_refresh_does_not_invalidate_consumers_outside_its_scope(tmp_path, refresh_only_root):
    (tmp_path / "pipeline.py").write_text(PIPELINE.replace("cutoff=False", ""))
    (tmp_path / "source.txt").write_text("old")

    def get(target, *flags):
        proc = barca(
            tmp_path, "get", target, "pipeline.py", *flags, env=clean_env(BARCA_POOL_SIZE="2")
        )
        assert proc.returncode == 0, proc.stderr
        return json.loads(proc.stdout)

    get("report")
    (tmp_path / "source.txt").write_text("new")
    if refresh_only_root:
        get("raw", "--refresh", "raw")
    else:
        get("report", "--refresh", "raw", "--no-cascade")
    get("report")
    assert (tmp_path / "clean-calls").read_text().splitlines() == ["old"]
    assert (tmp_path / "report-calls").read_text().splitlines() == ["old clean"]


@pytest.mark.parametrize("forget_upstream_history", ["none", "raw", "raw-and-clean"])
@pytest.mark.parametrize("one_key", [False, True])
def test_failed_refresh_covers_dynamic_keys_and_later_fan_in(
    tmp_path, forget_upstream_history, one_key
):
    import sqlite3

    (tmp_path / "pipeline.py").write_text(DYNAMIC_PIPELINE)
    (tmp_path / "source.txt").write_text("old")

    def get(*flags):
        result = barca(tmp_path, "get", "report", "pipeline.py", *flags)
        assert result.returncode in (0, 1), result.stderr
        return json.loads(result.stdout)

    assert get()["status"] == "success"
    if forget_upstream_history != "none":
        # Old downstream results may outlive their upstream's history after metadata pruning.
        with sqlite3.connect(tmp_path / ".barca" / "metadata.db") as db:
            db.execute("DELETE FROM materializations WHERE node_id LIKE '%:raw[%'")
            if forget_upstream_history == "raw-and-clean":
                db.execute("DELETE FROM materializations WHERE node_id LIKE '%:clean[%'")
    (tmp_path / "source.txt").write_text("new")
    (tmp_path / "fail-clean").touch()
    assert get("--refresh", "raw[key=a]" if one_key else "raw")["status"] == "failed"
    (tmp_path / "fail-clean").unlink()
    assert get()["status"] == "success"
    assert (tmp_path / "clean-a").read_text().splitlines() == ["olda", "newa"]
    assert (tmp_path / "clean-b").read_text().splitlines() == (
        ["oldb"] if one_key and forget_upstream_history != "raw-and-clean" else ["oldb", "newb"]
    )
    assert (tmp_path / "reports").read_text().splitlines() == [
        "olda,oldb",
        "newa,oldb" if one_key and forget_upstream_history != "raw-and-clean" else "newa,newb",
    ]


def test_failed_refresh_with_remote_ledger_does_not_resurrect_old_results(tmp_path):
    store = tmp_path / "store"
    store.mkdir()
    server_root = tmp_path / "server"
    client_root = tmp_path / "client"
    for root in (server_root, client_root):
        root.mkdir()
        (root / "pipeline.py").write_text(PIPELINE.replace("cutoff=False", ""))
        (root / "barca.toml").write_text(f'[remote]\nuri = "{store}"\n')
    (client_root / "source.txt").write_text("old")
    with Serve(server_root, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as serve:
        serve.ready()
        with (client_root / "barca.toml").open("a") as config:
            config.write(f'\n[server]\nurl = "{serve.url}"\n')

        def get(*flags):
            result = barca(
                client_root,
                "get",
                "report",
                "pipeline.py",
                *flags,
                env=clean_env(BARCA_POOL_SIZE="2"),
            )
            assert result.returncode in (0, 1), result.stderr
            return json.loads(result.stdout)

        assert get()["status"] == "success"
        (client_root / "source.txt").write_text("new")
        (client_root / "fail-clean").touch()
        assert get("--refresh", "raw")["status"] == "failed"
        (client_root / "fail-clean").unlink()
        assert get()["status"] == "success"
        assert (client_root / "clean-calls").read_text().splitlines() == ["old", "new"]
        assert (client_root / "report-calls").read_text().splitlines() == ["old clean", "new clean"]


@pytest.mark.parametrize("one_key", [False, True])
def test_cold_client_refresh_uses_remote_dynamic_history(tmp_path, one_key):
    store = tmp_path / "store"
    store.mkdir()
    server_root = tmp_path / "server"
    first = tmp_path / "first"
    cold = tmp_path / "cold"
    for root in (server_root, first, cold):
        root.mkdir()
        (root / "pipeline.py").write_text(DYNAMIC_PIPELINE)
        (root / "barca.toml").write_text(f'[remote]\nuri = "{store}"\n')
    with Serve(server_root, "--no-schedule", env=clean_env(BARCA_POOL_SIZE="2")) as serve:
        serve.ready()
        for root in (first, cold):
            with (root / "barca.toml").open("a") as config:
                config.write(f'\n[server]\nurl = "{serve.url}"\n')

        def get(root, *flags):
            result = barca(
                root, "get", "report", "pipeline.py", *flags, env=clean_env(BARCA_POOL_SIZE="2")
            )
            assert result.returncode in (0, 1), result.stderr
            return json.loads(result.stdout)

        (first / "source.txt").write_text("old")
        assert get(first)["status"] == "success"
        (cold / "source.txt").write_text("new")
        (cold / "fail-clean").touch()
        assert get(cold, "--refresh", "raw[key=a]" if one_key else "raw")["status"] == "failed"
        (cold / "fail-clean").unlink()
        assert get(cold)["status"] == "success"
        assert (cold / "clean-a").read_text().splitlines() == ["newa"]
        assert (cold / "reports").read_text().splitlines() == [
            "newa,oldb" if one_key else "newa,newb"
        ]
        if one_key:
            assert not (cold / "clean-b").exists()
