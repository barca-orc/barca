"""A failed refresh must not let retry resurrect pre-refresh downstream results (#939)."""

import json

import pytest

from .helpers import barca, clean_env
from .served import Serve

PIPELINE = '''
from pathlib import Path
from barca import asset

@asset(CUTOFF)
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
'''


@pytest.mark.parametrize("cutoff", [False, True])
@pytest.mark.parametrize("server", [False, True], ids=["cli", "server"])
@pytest.mark.parametrize("fail_retry", [False, True], ids=["retry", "retry-fails-again"])
def test_retry_after_failed_refresh_uses_current_upstream(tmp_path, cutoff, server, fail_retry):
    (tmp_path / "pipeline.py").write_text(PIPELINE.replace("CUTOFF", "cutoff=True" if cutoff else ""))
    (tmp_path / "source.txt").write_text("old")

    def request(serve, refresh=False):
        if serve is not None:
            body = {"command": "get", "targets": ["report"]}
            if refresh:
                body["refresh"] = ["raw"]
            answer = serve.post("/runs", body)
            return serve.finished(answer["run_id"])
        flags = ["--refresh", "raw"] if refresh else []
        proc = barca(tmp_path, "get", "report", "pipeline.py", *flags, env=clean_env(BARCA_POOL_SIZE="2"))
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
    (tmp_path / "pipeline.py").write_text(PIPELINE.replace("CUTOFF", ""))
    (tmp_path / "source.txt").write_text("old")

    def get(target, *flags):
        proc = barca(tmp_path, "get", target, "pipeline.py", *flags, env=clean_env(BARCA_POOL_SIZE="2"))
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
