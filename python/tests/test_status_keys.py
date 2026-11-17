"""A partitioned node's keys from the CLI (#990): `barca status NODE --keys`, `--grid` and
`--key KEY`, on this machine and against a real `barca serve`, which answers the same documents
on `GET /assets/{name}/partitions/grid` and `.../partitions/key`."""

import json
import subprocess

from barca.api import _find_binary

from . import served
from .helpers import clean_env

# `day` (2 values) by `region` (3): one key of `events` fails, so the same key of `scored`
# waits on it.
PIPELINE = """
from barca import asset, partitions

DAYS = ["d1", "d2"]
REGIONS = ["emea", "amer", "apac"]


@asset(partitions={"day": partitions(DAYS), "region": partitions(REGIONS)})
def events(day: str, region: str) -> list:
    if day == "d2" and region == "amer":
        raise ValueError(f"no rows for {region} on {day}")
    return [{"day": day, "region": region}]


@asset(partitions={"day": partitions(DAYS), "region": partitions(REGIONS)}, inputs={"events": events})
def scored(day: str, region: str, events: list) -> list:
    return [{**row, "score": 1} for row in events]


@asset()
def plain() -> int:
    return 1
"""


def _cli(root, *args):
    return subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env=clean_env(BARCA_POOL_SIZE="2"),
        capture_output=True,
        text=True,
        timeout=120,
    )


def _project(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text("")
    run = _cli(tmp_path, "get", "scored", "--json")
    assert run.returncode == 1, run.stderr
    return tmp_path


def test_keys_lists_each_key_with_its_state_attempt_and_grouped_error(tmp_path):
    root = _project(tmp_path)
    out = _cli(root, "status", "events", "--keys", "--json")
    assert out.returncode == 0, out.stderr
    body = json.loads(out.stdout)
    assert body["id"] == "pipeline.py:events" and body["known"] is True
    assert body["dimensions"] == ["day", "region"]
    assert body["total"] == 6 and body["truncated"] is False
    counts = body["counts"]
    assert (counts["fresh"], counts["failed"], counts["blocked"]) == (5, 1, 0), counts
    # Key order: the last dimension varies fastest, each dimension in declared order.
    assert [k["key"] for k in body["keys"]] == [
        f"day={d},region={r}" for d in ("d1", "d2") for r in ("emea", "amer", "apac")
    ]
    by_key = {k["key"]: k for k in body["keys"]}
    failed = by_key["day=d2,region=amer"]
    assert failed["state"] == "failed"
    assert failed["error"] == "ValueError: no rows for <region> on <day>", failed
    assert body["errors"] == [{"error": "ValueError: no rows for <region> on <day>", "keys": 1}]
    fresh = by_key["day=d1,region=emea"]
    assert fresh["state"] == "fresh" and fresh["error"] is None
    assert fresh["attempted_at"].endswith("Z") and fresh["run_id"] and fresh["size_bytes"] > 0

    # The same key of `scored` waits on it; `--state` takes key states and `total` follows it.
    blocked = _cli(root, "status", "scored", "--keys", "--state", "failed,blocked", "--json")
    body = json.loads(blocked.stdout)
    assert [(k["key"], k["state"]) for k in body["keys"]] == [("day=d2,region=amer", "blocked")]
    assert body["total"] == 1 and body["counts"]["fresh"] == 5, body
    assert body["keys"][0]["attempted_at"] is None

    # Bounded like every list: `--limit`, `truncated` and a hint; the table notes it on stderr.
    limited = _cli(root, "status", "scored", "--keys", "--limit", "2", "--json")
    body = json.loads(limited.stdout)
    assert len(body["keys"]) == 2 and body["total"] == 6 and body["truncated"] is True
    assert "--all" in body["hint"]
    table = _cli(root, "status", "events", "--keys", "--limit", "2", "--pretty")
    assert table.returncode == 0, table.stderr
    lines = table.stdout.splitlines()
    assert lines[0] == "pipeline.py:events: 6 keys · 1 failed · 5 fresh"
    assert lines[2].split()[:2] == ["KEY", "STATE"] and len(lines) == 8, table.stdout
    assert "errors of the failed keys:" in table.stdout
    assert "showing 2 of 6 keys" in table.stderr


def test_grid_prints_every_key_state_and_its_json_is_the_whole_grid(tmp_path):
    root = _project(tmp_path)
    out = _cli(root, "status", "scored", "--grid", "--pretty")
    assert out.returncode == 0, out.stderr
    assert out.stdout.splitlines() == [
        "pipeline.py:scored: 6 keys · 1 blocked · 5 fresh",
        "each row runs along region: emea, amer, apac",
        "c fresh  p pinned  s stale  n never run  f failed  b blocked  - not a key",
        "",
        "day=d1  ccc",
        "day=d2  cbc",
    ]
    grid = json.loads(_cli(root, "status", "scored", "--grid", "--json").stdout)
    assert grid["states"] == "ccccbc" and grid["chain"] == [
        "pipeline.py:events",
        "pipeline.py:scored",
    ]
    assert [d["name"] for d in grid["dimensions"]] == ["day", "region"]
    assert len(grid["attempted_at"]) == 6 and grid["attempted_at"][4] is None
    events = json.loads(_cli(root, "status", "events", "--grid", "--json").stdout)
    assert events["states"] == "ccccfc" and events["error"] == {"4": 0}, events


def test_key_shows_one_key_along_its_chain_with_what_it_read_and_wrote(tmp_path):
    root = _project(tmp_path)
    # The dimensions in any order.
    out = _cli(root, "status", "events", "--key", "region=amer,day=d2", "--json")
    assert out.returncode == 0, out.stderr
    detail = json.loads(out.stdout)
    assert detail["key"] == "day=d2,region=amer" and detail["state"] == "f"
    assert detail["chain"] == [
        {"id": "pipeline.py:events", "state": "f"},
        {"id": "pipeline.py:scored", "state": "b"},
    ]
    assert detail["attempts"][0]["status"] == "failed"
    assert "ValueError: no rows for amer on d2" in detail["attempts"][0]["error"]
    assert detail["output"] is None

    ok = json.loads(
        _cli(
            root, "status", "scored", "--key", "day=d1,region=apac", "--sample", "1", "--json"
        ).stdout
    )
    assert ok["state"] == "c" and ok["output"]["format"] == "json"
    assert ok["output"]["shape"]["sample"] == [{"day": "d1", "region": "apac", "score": 1}]
    [read] = ok["inputs"]
    assert read["step"] == "pipeline.py:events[day=d1,region=apac]" and read["artifact_path"]

    text = _cli(root, "status", "events", "--key", "day=d2,region=amer", "--pretty")
    assert text.returncode == 0, text.stderr
    assert text.stdout.startswith("pipeline.py:events[day=d2,region=amer]: failed\n")
    assert "  pipeline.py:scored  blocked" in text.stdout
    assert "latest error:" in text.stdout and "wrote: no successful result" in text.stdout


def test_key_views_take_one_partitioned_target_and_known_states(tmp_path):
    root = _project(tmp_path)
    for argv, text in [
        (["status", "plain", "--keys"], "'plain' is not partitioned"),
        (["status", "--keys"], "name exactly one target"),
        (["status", "events,scored", "--grid"], "name exactly one target"),
        (["status", "events", "--key", "day=d9,region=amer"], "has no key"),
        (["status", "events", "--keys", "--state", "cached"], "unknown key state 'cached'"),
        (["status", "events", "--keys", "--grid"], "cannot be used with"),
        (["status", "events", "--grid", "--state", "failed"], "cannot be used with"),
        (["status", "nope", "--keys"], "not found"),
    ]:
        out = _cli(root, *argv, "--json")
        assert out.returncode == 2, (argv, out.stdout, out.stderr)
        assert out.stdout == ""
        envelope = json.loads(out.stderr.splitlines()[-1])
        assert envelope["kind"] == "usage" and text in out.stderr, (argv, out.stderr)


def test_a_server_prints_the_same_keys_grid_and_key(tmp_path):
    root = _project(tmp_path)
    views = [
        ["status", "events", "--keys", "--json"],
        ["status", "scored", "--keys", "--state", "blocked,fresh", "--limit", "3", "--pretty"],
        ["status", "scored", "--grid", "--pretty"],
        ["status", "events", "--grid", "--json"],
        ["status", "events", "--key", "day=d2,region=amer", "--pretty"],
        ["status", "scored", "--key", "day=d1,region=emea", "--sample", "2", "--json"],
    ]
    here = [_cli(root, *argv) for argv in views]
    with served.Serve(root, "--no-schedule") as serve:
        serve.ready()
        there = [_cli(root, *argv, "--server", serve.url) for argv in views]
        # The JSON forms are the routes' own answers.
        grid = serve.get("/assets/events/partitions/grid")
        key = serve.get("/assets/scored/partitions/key?key=day%3Dd1%2Cregion%3Demea&sample=2")
        errors = [
            _cli(root, *argv, "--server", serve.url, "--json")
            for argv in (
                ["status", "plain", "--keys"],
                ["status", "events", "--key", "day=d9,region=amer"],
            )
        ]
    for argv, a, b in zip(views, here, there, strict=True):
        assert a.returncode == 0 and b.returncode == 0, (argv, a.stderr, b.stderr)
        assert a.stdout == b.stdout, argv
    assert json.loads(here[3].stdout) == grid
    assert json.loads(here[5].stdout) == key
    for out, text in zip(errors, ("is not partitioned", "has no key"), strict=True):
        assert out.returncode == 2 and text in out.stderr, out.stderr
