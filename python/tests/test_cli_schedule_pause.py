"""Pausing one schedule from the CLI (#989): `barca pause --schedule NODE [--until WHEN]` and
`barca resume --schedule NODE` against a real `barca serve`. They are `POST /schedule/{id}/pause`
and `.../resume`: the JSON is the schedule's `GET /schedule` entry, a paused schedule's ticks do
not fire, and `barca pause --json` lists it beside the paused nodes."""

import json
import subprocess
import time

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


def _entry(serve) -> dict:
    [job] = [j for j in serve.get("/schedule") if j["id"].endswith(":beat")]
    return job


def test_a_schedule_paused_from_the_cli_does_not_fire_until_it_is_resumed(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    with served.Serve(tmp_path) as serve:
        serve.wait("a tick of beat", lambda: serve.rows().get("beat", 0) >= 1)
        paused = _cli(tmp_path, "pause", "--schedule", "beat", "--server", serve.url, "--json")
        assert paused.returncode == 0, paused.stderr
        # The JSON is the route's answer: the schedule's entry.
        entry = json.loads(paused.stdout)
        assert entry["id"] == "pipeline.py:beat" and entry["paused"] is True, entry
        assert entry["paused_until"] is None and entry["cron"] == "* * * * * *", entry
        assert set(entry) == set(_entry(serve)), entry
        assert "paused" in paused.stderr and "barca resume --schedule beat" in paused.stderr

        # A tick in flight when the pause landed may still finish; after it, none fires.
        time.sleep(2)
        before = serve.rows().get("beat", 0)
        assert serve.settle(3).get("beat", 0) == before, serve.output()
        assert _entry(serve)["skip_reason"] == "paused"

        # What is paused: the schedule, beside the (no) paused nodes, as JSON and as a table.
        listed = _cli(tmp_path, "pause", "--server", serve.url, "--json")
        assert listed.returncode == 0, listed.stderr
        body = json.loads(listed.stdout)
        assert body["total"] == 0 and body["paused"] == [], body
        [schedule] = body["schedules"]
        assert schedule["id"] == "pipeline.py:beat" and schedule["until"] is None, body
        assert body == serve.get("/pauses")
        table = _cli(tmp_path, "pause", "--server", serve.url, "--pretty")
        assert "0 node(s) paused" in table.stdout and "1 schedule(s) paused" in table.stdout
        assert "pipeline.py:beat" in table.stdout, table.stdout
        # `barca status --problems` names the node of a paused schedule.
        problems = _cli(tmp_path, "status", "--problems", "--server", serve.url, "--json")
        nodes = {n["name"]: n for n in json.loads(problems.stdout)["nodes"]}
        assert nodes["beat"]["schedule"]["paused"] is True, nodes

        # Without --json: one line on stdout.
        again = _cli(tmp_path, "pause", "--schedule", "beat", "--server", serve.url, "--pretty")
        assert again.returncode == 0, again.stderr
        assert again.stdout.startswith("pipeline.py:beat: paused until resumed · cron * * * * * *")

        resumed = _cli(tmp_path, "resume", "--schedule", "beat", "--server", serve.url, "--json")
        assert resumed.returncode == 0, resumed.stderr
        assert json.loads(resumed.stdout)["paused"] is False
        assert serve.get("/pauses")["schedules"] == []
        after = serve.rows().get("beat", 0)
        serve.wait("a tick after resume", lambda: serve.rows().get("beat", 0) > after)


def test_a_timed_pause_takes_a_duration_or_a_time_and_ends_by_itself(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    with served.Serve(tmp_path, "--no-schedule") as serve:
        serve.ready()
        server = ("--server", serve.url)
        now = int(time.time())
        timed = _cli(tmp_path, "pause", "--schedule", "beat", "--until", "2h", *server, "--json")
        assert timed.returncode == 0, timed.stderr
        until = json.loads(timed.stdout)["paused_until"]
        assert 7190 <= until - now <= 7215, (until, now)
        assert serve.get("/pauses")["schedules"][0]["until"] == until
        far = "2999-01-01T00:00:00Z"
        at = _cli(tmp_path, "pause", "--schedule", "beat", "--until", far, *server, "--pretty")
        assert at.returncode == 0, at.stderr
        assert far in at.stderr and f"paused until {far}" in at.stdout

        # A pause whose time has passed is over: it is not listed.
        short = _cli(tmp_path, "pause", "--schedule", "beat", "--until", "1s", *server, "--json")
        assert short.returncode == 0, short.stderr
        serve.wait("the pause to end", lambda: serve.get("/pauses")["schedules"] == [])

        # Usage errors, exit 2, with the stderr envelope.
        for argv, text in [
            (["pause", "--schedule", "beat", "--until", "soon", *server], "neither a duration"),
            (
                ["pause", "--schedule", "beat", "--until", "2020-01-01T00:00:00Z", *server],
                "not in the future",
            ),
            (["pause", "--schedule", "source", *server], "is not a scheduled node"),
            (["resume", "--schedule", "nope", *server], "is not a scheduled node"),
            (["pause", "--until", "2h", *server], "--schedule"),
            (["pause", "source", "--schedule", "beat", *server], "cannot be used with"),
            (["resume", "--all", "--schedule", "beat", *server], "cannot be used with"),
        ]:
            out = _cli(tmp_path, *argv, "--json")
            assert out.returncode == 2, (argv, out.stderr)
            envelope = json.loads(out.stderr.splitlines()[-1])
            assert envelope["kind"] == "usage" and text in out.stderr, (argv, out.stderr)


def test_pausing_a_schedule_needs_a_server_and_a_read_only_one_refuses(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    here = _cli(tmp_path, "pause", "--schedule", "beat")
    assert here.returncode == 2, here.stderr
    assert "no server is given" in here.stderr
    with served.Serve(tmp_path, "--read-only") as serve:
        serve.ready()
        refused = _cli(tmp_path, "pause", "--schedule", "beat", "--server", serve.url, "--json")
        assert refused.returncode == 2, refused.stderr
        assert json.loads(refused.stderr.splitlines()[-1])["kind"] == "usage"
