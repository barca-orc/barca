"""`barca pin <asset> --from-file <path>` and `POST /assets/{name}/pin/upload` (`barca docs
pins`): an asset pinned to data an operator supplies is read as that data by what reads it,
never computed until it is unpinned, and the version is marked as an upload.
"""

import json
import urllib.error
import urllib.request

import pytest

from .helpers import barca, clean_env
from .served import Serve

PIPELINE = """
import os

from barca import asset


@asset(env=["FEED"])
def raw() -> dict:
    return {"v": int(os.environ["FEED"])}


@asset(inputs={"raw": raw})
def doubled(raw: dict) -> dict:
    return {"v": raw["v"] * 2}


@asset(serializer="parquet", env=["FEED"])
def table():
    import pyarrow as pa

    return pa.table({"x": [int(os.environ["FEED"])]})


@asset(inputs={"table": table})
def total(table) -> dict:
    return {"sum": int(sum(table["x"]))}
"""


def _get(project, target, feed="5"):
    r = barca(project, "get", target, "-o", "value", env=clean_env(FEED=feed))
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def _json(project, *args, code=0):
    r = barca(project, *args, "--json", env=clean_env(FEED="5"))
    assert r.returncode == code, (r.stdout, r.stderr)
    return json.loads(r.stdout) if code == 0 else json.loads(r.stderr.strip().splitlines()[-1])


def test_an_uploaded_json_is_read_downstream_until_unpinned(project):
    assert _get(project, "doubled") == {"v": 10}
    fix = project / "fix.json"
    fix.write_text('{"v": 21}')
    pinned = _json(project, "pin", "raw", "--from-file", str(fix), "--reason", "bad feed")
    assert pinned["version"]["upload"]["file_name"] == "fix.json"
    assert pinned["version"]["upload"]["reason"] == "bad feed"
    assert pinned["version"]["pinned"] and pinned["version"]["run_id"] is None
    run_hash = pinned["pin"]["run_hash"]
    assert (project / pinned["version"]["artifact"]).is_file() or pinned["version"][
        "artifact"
    ].endswith(f"{run_hash}.json")

    assert _get(project, "doubled") == {"v": 42}
    assert _get(project, "raw", feed="9") == {"v": 21}
    versions = _json(project, "pin", "raw", "--versions")
    assert versions["versions"][0]["upload"] is not None
    assert versions["versions"][1]["upload"] is None

    # The same bytes again: the same run hash.
    again = _json(project, "pin", "raw", "--from-file", str(fix))
    assert again["pin"]["run_hash"] == run_hash and again["replaced"]["run_hash"] == run_hash

    assert _json(project, "unpin", "raw")["removed"]["run_hash"] == run_hash
    assert _get(project, "doubled") == {"v": 10}


def test_a_file_that_does_not_load_is_refused_and_nothing_is_pinned(project):
    assert _get(project, "raw") == {"v": 5}
    bad = project / "bad.json"
    bad.write_text("{not json")
    err = _json(project, "pin", "raw", "--from-file", str(bad), code=2)
    assert "does not load as json" in err["error"], err
    unknown = project / "fix.txt"
    unknown.write_text("1")
    err = _json(project, "pin", "raw", "--from-file", str(unknown), code=2)
    assert "cannot tell the format" in err["error"], err
    assert _json(project, "pin")["pins"] == []
    assert not list((project / ".barca").rglob("*.upload.tmp"))


def test_an_uploaded_parquet_is_read_downstream(project):
    pa = pytest.importorskip("pyarrow")
    import pyarrow.parquet as pq

    assert _get(project, "total") == {"sum": 5}
    path = project / "fixed.parquet"
    pq.write_table(pa.table({"x": [1, 2, 3]}), path)
    pinned = _json(project, "pin", "table", "--from-file", str(path))
    assert pinned["version"]["format"] == "parquet"
    assert _get(project, "total") == {"sum": 6}
    _json(project, "unpin", "table")
    assert _get(project, "total") == {"sum": 5}


def _post(url, body):
    request = urllib.request.Request(url, data=body, method="POST")
    request.add_header("Content-Type", "application/octet-stream")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())


def test_an_upload_over_http_pins_and_a_read_only_server_refuses(project):
    assert _get(project, "doubled") == {"v": 10}
    with Serve(project, "--no-schedule", env=clean_env(FEED="5"), wait="health") as served:
        url = f"{served.url}/assets/raw/pin/upload?file_name=fix.json&reason=hotfix"
        status, pinned = _post(url, b'{"v": 4}')
        assert status == 200, pinned
        assert pinned["version"]["upload"]["file_name"] == "fix.json"
        status, bad = _post(f"{served.url}/assets/raw/pin/upload?format=json", b"nope")
        assert status == 400, bad
        status, none = _post(f"{served.url}/assets/raw/pin/upload", b"{}")
        assert status == 400, none
        status, versions = served.request("GET", "/assets/raw/versions")
        assert versions["pin"]["run_hash"] == pinned["pin"]["run_hash"]
        assert versions["versions"][0]["upload"]["reason"] == "hotfix"
    assert _get(project, "doubled") == {"v": 8}
    with Serve(project, "--read-only", env=clean_env(FEED="5"), wait="health") as ro:
        status, _ = _post(f"{ro.url}/assets/raw/pin/upload?format=json", b'{"v": 1}')
        assert status == 403
