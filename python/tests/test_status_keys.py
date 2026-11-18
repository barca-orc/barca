"""A partitioned node's keys from the CLI (#990, #1036): `barca status NODE --keys` with its
filters, `--overview`, `--grid` and `--key KEY`, on this machine and against a real
`barca serve`, which answers the same documents on `GET /assets/{name}/partitions/keys`,
`.../partitions/overview`, `.../partitions/grid` and `.../partitions/key`."""

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
    assert "errors of the failed keys (--error N lists one group's keys):" in table.stdout
    assert "  0  1 key: ValueError: no rows for <region> on <day>" in table.stdout
    assert "showing 2 of 6 keys" in table.stderr


def test_keys_take_filters_an_order_and_a_page_and_say_how_to_run_what_matches(tmp_path):
    root = _project(tmp_path)

    def keys(*flags):
        out = _cli(root, "status", "events", "--keys", *flags, "--json")
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)

    # A dimension's values, a stretch of a dimension, text, an error group: all must hold.
    assert [
        k["key"] for k in keys("--value", "region=amer,apac", "--range", "day=d2..d2")["keys"]
    ] == [
        "day=d2,region=amer",
        "day=d2,region=apac",
    ]
    assert [k["key"] for k in keys("--match", "D1,REGION=A")["keys"]] == [
        "day=d1,region=amer",
        "day=d1,region=apac",
    ]
    failed = keys("--state", "failed", "--error", "0")
    assert [k["key"] for k in failed["keys"]] == ["day=d2,region=amer"]
    assert failed["keys"][0]["values"] == ["d2", "amer"]
    # What matches as `barca get --partition` takes it: computing it clears the failure's keys.
    assert (failed["total"], failed["needs_run"]) == (1, 1)
    assert json.loads(failed["selection"]) == {"day": ["d2"], "region": ["amer"]}
    # `--needs-run`: what a run computes, here and on the node that waits on it.
    assert keys("--needs-run")["total"] == 1
    scored = json.loads(_cli(root, "status", "scored", "--keys", "--needs-run", "--json").stdout)
    assert [k["state"] for k in scored["keys"]] == ["blocked"]
    # An order, then a page of it.
    by_state = keys("--sort", "state")
    assert by_state["keys"][0]["state"] == "failed" and by_state["keys"][-1]["state"] == "fresh"
    second = keys("--limit", "2", "--offset", "2")
    assert [k["key"] for k in second["keys"]] == ["day=d1,region=apac", "day=d2,region=emea"]
    assert second["offset"] == 2 and second["truncated"] is True and "--offset 4" in second["hint"]
    last = keys("--limit", "2", "--offset", "4")
    assert last["truncated"] is False and "hint" not in last
    # The table says what matched and the command that runs it.
    table = _cli(root, "status", "events", "--keys", "--state", "failed", "--pretty")
    assert "1 key match, 1 of them to run" in table.stdout
    assert (
        'run them: barca get pipeline.py:events --partition \'{"day":["d2"],"region":["amer"]}\''
        in table.stdout
    )
    none = _cli(root, "status", "events", "--keys", "--state", "pinned", "--pretty")
    assert "no key matches" in none.stdout
    # The selection the list prints is one `barca get` takes: it runs the failed key again.
    rerun = _cli(root, "get", "events", "--partition", failed["selection"], "--json")
    assert rerun.returncode == 1 and "day=d2,region=amer" in rerun.stderr


def test_overview_counts_the_keys_says_what_is_wrong_and_where_and_rows_the_grid(tmp_path):
    root = _project(tmp_path)
    body = json.loads(_cli(root, "status", "scored", "--overview", "--json").stdout)
    assert body["id"] == "pipeline.py:scored" and body["known"] is True
    assert body["chain"] == ["pipeline.py:events", "pipeline.py:scored"]
    assert body["dimensions"] == [
        {"name": "day", "values": ["d1", "d2"]},
        {"name": "region", "values": ["emea", "amer", "apac"]},
    ]
    assert body["needs_run"] == 1 and body["counts"]["blocked"] == 1
    # The rows run along the longest dimension, one per value of the other.
    assert body["along"] == "region"
    assert body["rows"] == [
        {"label": "day=d1", "values": ["d1"], "states": "ccc"},
        {"label": "day=d2", "values": ["d2"], "states": "cbc"},
    ]
    assert body["rows_total"] == 2 and body["truncated"] is False
    [issue] = body["issues"]
    assert (issue["state"], issue["error"], issue["keys"]) == ("blocked", None, 1)
    assert issue["places"] == [
        {"dimension": "day", "words": "d2"},
        {"dimension": "region", "words": "amer"},
    ]
    assert json.loads(issue["selection"]) == {"day": ["d2"], "region": ["amer"]}
    events = json.loads(_cli(root, "status", "events", "--overview", "--json").stdout)
    [failure] = events["issues"]
    assert (failure["state"], failure["error"]) == ("failed", 0)
    assert failure["title"] == "ValueError: no rows for <region> on <day>"
    assert events["last_attempt_at"].endswith("Z")
    text = _cli(root, "status", "events", "--overview", "--pretty").stdout
    assert "what needs attention, the most keys first:" in text
    assert (
        "  1 key  failed: ValueError: no rows for <region> on <day> (day d2 · region amer)" in text
    )
    assert "    list: barca status pipeline.py:events --keys --state failed --error 0" in text
    assert text.rstrip().endswith("day=d1  ccc\nday=d2  cfc")
    # `--limit` bounds the rows; the note says how many there are.
    one = _cli(root, "status", "events", "--overview", "--limit", "1", "--pretty")
    assert "showing 1 of 2 rows" in one.stderr and "day=d2" not in one.stdout.split("not a key")[1]


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
        (["status", "events", "--keys", "--value", "zone=a"], "no dimension 'zone'"),
        (["status", "events", "--keys", "--value", "region"], "DIMENSION=VALUE"),
        (["status", "events", "--keys", "--range", "day=d1"], "DIMENSION=FROM..TO"),
        (["status", "events", "--keys", "--range", "day=d1..d9"], "no day 'd9'"),
        (["status", "events", "--keys", "--error", "3"], "--error takes 0 to 0"),
        (["status", "events", "--keys", "--sort", "newest"], "invalid value 'newest'"),
        (["status", "events", "--value", "region=amer"], "--keys"),
        (["status", "events", "--overview", "--state", "failed"], "cannot be used with"),
        (["status", "events", "--keys", "--overview"], "cannot be used with"),
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
        # `--keys --json` and `--overview --json` are their routes' answers too.
        page = serve.get("/assets/events/partitions/keys?state=failed&limit=5")
        overview = serve.get("/assets/scored/partitions/overview")
        local_page = json.loads(
            _cli(
                root, "status", "events", "--keys", "--state", "failed", "--limit", "5", "--json"
            ).stdout
        )
        local_overview = json.loads(_cli(root, "status", "scored", "--overview", "--json").stdout)
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
    assert local_page == page and local_overview == overview
    assert json.loads(here[5].stdout) == key
    for out, text in zip(errors, ("is not partitioned", "has no key"), strict=True):
        assert out.returncode == 2 and text in out.stderr, out.stderr
