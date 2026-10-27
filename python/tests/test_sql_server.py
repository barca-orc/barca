"""`barca sql` with a server (`barca docs sql`, "With a server"; `barca docs server-mode`).

Two copies of one project, `a` and `b`, point at one `barca serve` and the plain-directory store
it uses (the ledger suite's `Team`). The server resolves which result each view reads from its
ledger (`GET /sql/views`) and barca's own tables from its records (`GET /history`,
`GET /materializations`, `GET /nodes`); the query runs on the laptop, over the store. The server
runs no SQL.
"""

import json
from pathlib import Path

import pytest

from .test_server_ledger import NODES, Team, _kill_server_mid_run, barca, get, toml

pytest.importorskip("duckdb")

PARTITIONED = """
from barca import asset, partitions


@asset()
def base() -> dict:
    return {"n": 1}


@asset(partitions={"week": partitions(["w1", "w2"])})
def weekly(week: str) -> list[dict]:
    return [{"week": week, "n": len(week)}]
"""


@pytest.fixture()
def team(tmp_path):
    t = Team(tmp_path)
    yield t
    t.stop()


def sql(root: Path, query: str, *args: str) -> tuple[int, dict | None, str]:
    out = barca(root, "sql", query, "--json", *args)
    body = json.loads(out.stdout) if out.returncode == 0 else None
    return out.returncode, body, out.stderr


def envelope(stderr: str) -> dict:
    return json.loads(stderr.strip().splitlines()[-1])


def test_a_teammate_queries_a_view_the_other_laptop_computed(team):
    get(team.a, "report")
    code, body, err = sql(team.b, "select n from report")
    assert code == 0, err
    assert body["rows"] == [{"n": 20}]
    assert err.splitlines()[0] == f"[barca] server {team.url} ([server] url in barca.toml)"
    assert err.count("[barca] server ") == 1
    # B ran nothing and recorded nothing: the server's ledger named the result, the store held it.
    assert barca(team.b, "history", "--local", "--json").stdout.count('"run_id"') == 0
    assert len(team.server.history()) == 1
    # The views are named as a local `barca sql` names them, and resolve to the same result.
    code, here, _ = sql(team.a, "select n from report", "--local")
    assert code == 0 and here == body
    code, tables, _ = sql(team.b, "show tables")
    names = [r["name"] for r in tables["rows"]]
    code, local_tables, _ = sql(team.a, "show tables", "--local")
    assert names == [r["name"] for r in local_tables["rows"]]
    assert {"report", "base", "middle", "barca.runs", "barca.materializations"} <= set(names)
    assert "barca.nodes" in names


def test_a_node_the_server_has_no_result_for_names_the_command(team):
    code, _, err = sql(team.b, "select * from report")
    assert code == 2, err
    e = envelope(err)
    assert e["kind"] == "usage"
    assert f"no result on the server at {team.url}" in e["error"], e
    assert "barca get report" in e["remediation"]


def test_metadata_tables_come_from_the_server(team):
    result_a, _ = get(team.a, "report")
    code, body, err = sql(
        team.b,
        "select target, status, triggered_by, client_run_id from barca.runs order by started_at desc",
    )
    assert code == 0, err
    assert body["rows"] == [
        {
            "target": "report",
            "status": "success",
            "triggered_by": "client",
            "client_run_id": result_a["run_id"],
        }
    ]
    # The columns are `barca history --json`'s.
    history = json.loads(barca(team.b, "history", "--json").stdout)["runs"][0]
    code, body, _ = sql(team.b, "select * from barca.runs")
    assert body["columns"] == list(history) or set(body["columns"]) == set(history)
    assert body["rows"][0]["run_id"] == history["run_id"]

    code, body, err = sql(
        team.b,
        "select node_id, status, format, artifact from barca.materializations order by node_id",
    )
    assert code == 0, err
    assert [r["node_id"] for r in body["rows"]] == sorted(NODES)
    assert all(r["status"] == "success" and r["format"] == "json" for r in body["rows"])
    assert all(r["artifact"].startswith(str(team.store)) for r in body["rows"]), body

    code, body, err = sql(
        team.b, "select name, state, local from barca.nodes where name = 'report'"
    )
    assert code == 0, err
    assert body["rows"] == [{"name": "report", "state": "cached", "local": False}]


def test_metadata_tables_locally(team):
    get(team.a, "report", "--local")
    code, body, err = sql(team.a, "select target, status, triggered_by from barca.runs", "--local")
    assert code == 0, err
    assert "[barca] server" not in err
    assert body["rows"] == [{"target": "report", "status": "success", "triggered_by": "manual"}]
    code, body, _ = sql(team.a, "select count(*) as n from barca.materializations", "--local")
    assert body["rows"] == [{"n": 3}]
    code, body, _ = sql(
        team.a, "select name, local from barca.nodes where name = 'report'", "--local"
    )
    assert body["rows"] == [{"name": "report", "local": None}]
    # The server recorded nothing: its tables are empty.
    code, body, _ = sql(team.b, "select count(*) as n from barca.runs")
    assert body["rows"] == [{"n": 0}]
    # A table the query does not name is not loaded; `describe` still sees its columns.
    code, body, _ = sql(team.a, "describe barca.materializations", "--local")
    assert code == 0
    assert "content_hash" in [r["column_name"] for r in body["rows"]]


def test_local_reads_this_machines_records_only(team):
    get(team.a, "report")
    code, _, err = sql(team.b, "select * from report", "--local")
    assert code == 2, err
    assert "[barca] server" not in err
    assert "has no result yet" in envelope(err)["error"]
    # --local works with the server down.
    team.server.stop()
    code, body, err = sql(team.a, "select n from report", "--local")
    assert code == 0, err
    assert body["rows"] == [{"n": 20}]


def test_an_unreachable_server_is_exit_3_naming_local(team):
    get(team.a, "report")
    team.server.stop()
    out = barca(team.a, "sql", "select n from report", "--json")
    assert out.returncode == 3, out.stderr
    e = envelope(out.stderr)
    assert e["kind"] == "infra"
    assert team.url in e["error"]
    assert "--local" in e["error"] + (e.get("remediation") or "")
    assert out.stdout == ""


def test_server_names_a_server_for_one_command(team, tmp_path):
    get(team.a, "report")
    laptop = tmp_path / "laptop"
    laptop.mkdir()
    (laptop / "pipeline.py").write_text((team.a / "pipeline.py").read_text())
    (laptop / "barca.toml").write_text(toml(team.store, None))
    code, body, err = sql(laptop, "select n from report", "--server", team.url)
    assert code == 0, err
    assert body["rows"] == [{"n": 20}]
    assert "[barca] server" not in err  # a URL typed on the command line is not announced
    # Without --server this project has no server: its own (empty) records.
    code, _, err = sql(laptop, "select n from report")
    assert code == 2 and "has no result yet" in envelope(err)["error"]
    # A server in another store cannot be read from here: a usage error naming both.
    (laptop / "barca.toml").write_text(toml(tmp_path / "other", None))
    code, _, err = sql(laptop, "select n from report", "--server", team.url)
    assert code == 2, err
    assert str(team.store) in err and "other" in err


def test_a_result_computed_here_and_not_recorded_is_named_not_mixed_in(team):
    out = _kill_server_mid_run(team, team.a)
    assert out.returncode == 0, out.stderr
    team.start()
    # The ledger has no `slow`: the view is not this machine's copy, and the error says why.
    code, _, err = sql(team.a, "select * from slow")
    assert code == 2, err
    e = envelope(err)
    assert "not recorded yet (unsynced)" in e["error"], e
    assert "--local" in e["remediation"]
    code, body, err = sql(team.a, "select * from slow", "--local")
    assert code == 0, err
    assert body["rows"] == [{"json": 7}]
    # B has nothing pending: the plain message.
    code, _, err = sql(team.b, "select * from slow")
    assert "no result on the server" in envelope(err)["error"]


def test_a_stale_result_on_the_server_is_queried_with_the_note(team):
    get(team.a, "report")
    team.server.stop()
    pipeline = team.server_root / "pipeline.py"
    pipeline.write_text(pipeline.read_text().replace('middle["n"] * 10', 'middle["n"] * 11'))
    team.start()
    code, body, err = sql(team.b, "select n from report")
    assert code == 0, err
    assert body["rows"] == [{"n": 20}]
    assert "barca: 'report' is stale" in err, err


def test_partitioned_views_from_the_server(team):
    team.server.stop()
    for root in (team.server_root, team.a, team.b):
        (root / "pipeline.py").write_text(PARTITIONED)
    team.start()
    get(team.a, "weekly")
    code, body, err = sql(team.b, "select partition, week, n from weekly order by partition")
    assert code == 0, err
    assert body["rows"] == [
        {"partition": "week=w1", "week": "w1", "n": 2},
        {"partition": "week=w2", "week": "w2", "n": 2},
    ]
    code, here, _ = sql(
        team.a, "select partition, week, n from weekly order by partition", "--local"
    )
    assert here == body
    code, body, _ = sql(
        team.b, "select node_id, partition from barca.materializations order by node_id"
    )
    assert body["rows"] == [
        {"node_id": "pipeline.py:weekly[week=w1]", "partition": "week=w1"},
        {"node_id": "pipeline.py:weekly[week=w2]", "partition": "week=w2"},
    ]
    code, body, _ = sql(
        team.b, "select partitions_total, partitions_cached from barca.nodes where name = 'weekly'"
    )
    assert body["rows"] == [{"partitions_total": 2, "partitions_cached": 2}]


def test_files_narrow_the_views_to_deployed_files(team):
    get(team.a, "report")
    code, body, err = sql(team.b, "select n from report", "pipeline.py")
    assert code == 0, err
    out = barca(team.b, "sql", "select 1", "other.py", "--json")
    assert out.returncode == 2, out.stderr
    assert "not one of the files deployed" in envelope(out.stderr)["error"]
