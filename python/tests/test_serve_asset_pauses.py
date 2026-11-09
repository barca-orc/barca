"""Pausing assets under `barca serve` (`barca pause`, `POST /pauses`): a scheduled tick skips a
paused node with reason `paused` until it is resumed, and a run by hand needs --allow-paused."""

import subprocess

from barca.api import _find_binary

from . import served
from .helpers import clean_env

PIPELINE = """
from barca import asset, task, Schedule

@asset()
def source() -> dict:
    return {"n": 1}

@task(freshness=Schedule("* * * * * *"), inputs={"source": source})
def beat(source: dict) -> None:
    print("beat", source["n"])
"""


def _cli(root, *args):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=clean_env(),
        capture_output=True,
        text=True,
        timeout=60,
    )


def _latest_tick(serve) -> dict | None:
    runs = serve.get("/runs?limit=20")["runs"]
    tick = next(
        (
            r
            for r in runs
            if r["triggered_by"] == "schedule" and r["status"] not in {"running", "pending"}
        ),
        None,
    )
    return serve.finished(tick["run_id"]) if tick else None


def test_a_paused_asset_is_skipped_by_ticks_until_resumed(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    with served.Serve(tmp_path) as serve:
        serve.wait("a tick of beat", lambda: serve.rows().get("beat", 0) >= 1)
        paused = _cli(tmp_path, "pause", "source", "--downstream", "--server", serve.url, "--json")
        assert paused.returncode == 0, paused.stderr
        assert serve.get("/pauses")["total"] == 2

        # The next ticks skip beat with reason `paused`, and nothing of it runs.
        def skipped():
            run = _latest_tick(serve)
            steps = run["outcome"]["steps"] if run and run.get("outcome") else []
            return [s for s in steps if s["id"].endswith(":beat") and s.get("reason") == "paused"]

        step = serve.wait("a tick that skips beat", skipped)[0]
        assert step["status"] == "skipped", step
        assert "barca resume beat" in step["detail"], step
        before = serve.rows().get("beat", 0)
        serve.wait("two more skipped ticks", lambda: len(serve.get("/runs?limit=20")["runs"]) >= 4)
        assert serve.settle(2).get("beat", 0) == before, serve.output()

        # A run by hand is refused, unless it says it means it.
        refused = _cli(tmp_path, "run", "beat", "--server", serve.url)
        assert refused.returncode == 2, refused.stderr
        assert "is paused" in refused.stderr
        status, body = serve.request("POST", "/runs", {"command": "run", "targets": ["beat"]})
        assert status == 400 and "is paused" in body["error"], body
        status, body = serve.request(
            "POST", "/runs", {"command": "run", "targets": ["beat"], "allow_paused": True}
        )
        assert status == 202, body
        serve.finished(body["run_id"])

        # Resumed, the ticks run it again.
        resumed = _cli(tmp_path, "resume", "--all", "--server", serve.url, "--json")
        assert resumed.returncode == 0, resumed.stderr
        assert serve.get("/pauses")["total"] == 0
        after = serve.rows().get("beat", 0)
        serve.wait("a tick after resume", lambda: serve.rows().get("beat", 0) > after)


def test_pause_without_a_server_is_a_usage_error(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    result = _cli(tmp_path, "pause", "source")
    assert result.returncode == 2, result.stderr
    assert "no server is given" in result.stderr
