"""Pausing assets under `barca serve` (`barca pause`, `POST /pauses`): a scheduled tick skips a
paused node with reason `paused` until it is resumed, and a run by hand needs --allow-paused."""

import os
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
            steps = run["run"]["outcome"]["steps"] if run and run["run"].get("outcome") else []
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


# A scheduled sensor above the node that gets held, above an `Always` asset and a task.
CHAIN = """
from pathlib import Path

from barca import asset, sensor, task, Schedule


@sensor(freshness=Schedule("* * * * * *"))
def feed() -> str:
    return Path("feed.txt").read_text()


@asset(inputs={"value": feed})
def middle(value: str) -> str:
    return value


@asset(inputs={"value": middle})
def below(value: str) -> str:
    return value


@task(inputs={"value": below})
def deliver(value: str) -> None:
    with open("delivered.log", "a") as f:
        f.write(value + "\\n")
"""


def _write(root, name: str, value: str) -> None:
    tmp = root / f"{name}.tmp"
    tmp.write_text(value)
    os.replace(tmp, root / name)


def _delivered(root) -> list[str]:
    path = root / "delivered.log"
    return path.read_text().splitlines() if path.exists() else []


def _chain(root):
    (root / "pipeline.py").write_text(CHAIN)
    (root / "barca.toml").write_text("")
    _write(root, "feed.txt", "a")


def _polls(serve, at_least: int = 3) -> None:
    """Let `at_least` more ticks poll the sensor, so a held node has been read by them."""
    start = serve.rows().get("feed", 0)
    serve.wait("more ticks", lambda: serve.rows().get("feed", 0) >= start + at_least)


def test_a_node_read_while_paused_is_rebuilt_by_the_next_tick_after_resume(tmp_path):
    """#988: the sensor changes while `middle` is paused. Ticks read `middle` as it is; once it
    is resumed, the next tick computes it and what is below it."""
    _chain(tmp_path)
    with served.Serve(tmp_path) as serve:
        serve.wait("first value", lambda: _delivered(tmp_path) == ["a"])
        paused = _cli(tmp_path, "pause", "middle", "--server", serve.url, "--json")
        assert paused.returncode == 0, paused.stderr

        _write(tmp_path, "feed.txt", "b")
        _polls(serve)
        rows = serve.settle(2)
        # Held: neither it nor what reads it was computed for the new value.
        assert rows.get("middle") == 1 and rows.get("below") == 1, rows
        assert _delivered(tmp_path) == ["a"]

        resumed = _cli(tmp_path, "resume", "middle", "--server", serve.url, "--json")
        assert resumed.returncode == 0, resumed.stderr
        serve.wait("the value held back", lambda: _delivered(tmp_path) == ["a", "b"])
        rows = serve.settle(2)
        assert rows.get("middle") == 2 and rows.get("below") == 2, rows
        assert _delivered(tmp_path) == ["a", "b"]


def test_a_pause_over_which_nothing_changed_recomputes_nothing_after_resume(tmp_path):
    _chain(tmp_path)
    with served.Serve(tmp_path) as serve:
        serve.wait("first value", lambda: _delivered(tmp_path) == ["a"])
        assert _cli(tmp_path, "pause", "middle", "--server", serve.url).returncode == 0
        _polls(serve)
        assert _cli(tmp_path, "resume", "middle", "--server", serve.url).returncode == 0
        _polls(serve)
        rows = serve.settle(2)
        assert rows.get("middle") == 1 and rows.get("below") == 1, rows
        assert _delivered(tmp_path) == ["a"]


def test_an_asset_read_while_pinned_is_rebuilt_by_the_next_tick_after_unpin(tmp_path):
    """The same for `barca pin`: ticks read the pinned result while the sensor changes, and
    the tick after `barca unpin` computes the asset for the sensor's current value."""
    _chain(tmp_path)
    with served.Serve(tmp_path) as serve:
        serve.wait("first value", lambda: _delivered(tmp_path) == ["a"])
        run_hash = serve.get("/assets/middle/versions")["versions"][0]["run_hash"]
        status, body = serve.request("POST", "/assets/middle/pin", {"run_hash": run_hash})
        assert status == 200, body

        _write(tmp_path, "feed.txt", "b")
        _polls(serve)
        rows = serve.settle(2)
        assert rows.get("middle") == 1 and rows.get("below") == 1, rows

        status, body = serve.request("DELETE", "/assets/middle/pin")
        assert status == 200, body
        serve.wait("the value held back", lambda: _delivered(tmp_path) == ["a", "b"])
        rows = serve.settle(2)
        assert rows.get("middle") == 2 and rows.get("below") == 2, rows


def test_a_schedule_resumed_after_a_pause_brings_what_is_below_it_up_to_date(tmp_path):
    """`POST /schedule/{id}/pause` stops the ticks themselves, so nothing is read while it is
    paused: the first tick after `resume` sees the sensor's new value and rebuilds below it."""
    _chain(tmp_path)
    with served.Serve(tmp_path) as serve:
        serve.wait("first value", lambda: _delivered(tmp_path) == ["a"])
        status, body = serve.request("POST", "/schedule/feed/pause")
        assert status == 200, body
        polls = serve.settle(2).get("feed", 0)
        _write(tmp_path, "feed.txt", "b")
        rows = serve.settle(3)
        assert rows.get("feed", 0) == polls and rows.get("middle") == 1, rows

        status, body = serve.request("POST", "/schedule/feed/resume")
        assert status == 200, body
        serve.wait("the value held back", lambda: _delivered(tmp_path) == ["a", "b"])
        rows = serve.settle(2)
        assert rows.get("middle") == 2 and rows.get("below") == 2, rows
