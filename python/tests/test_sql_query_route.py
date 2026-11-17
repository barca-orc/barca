"""`POST /sql/query`: the server answers a query over barca's own tables (`barca docs sql`,
"With a server"; specs/behavior/health.md, "Server query route").

Against a real `barca serve`: an aggregate over `barca.runs`, on a `--read-only` server and
behind a token too; every way around "one SELECT over barca's tables" is a `400`; a query past
its five seconds is a `408` and stops; a killed helper is started again and never outlives its
server; a server whose environment has no DuckDB answers `503` and the CLI then runs the query
itself.
"""

import json
import os
import signal
import subprocess
import threading
import time
from pathlib import Path

import pytest

from . import helpers
from .helpers import QUICK, clean_env, wait_until
from .served import LIVE, Serve
from .test_server_tokens import TokenServer, exchange

pytest.importorskip("duckdb")

PIPELINE = """
from barca import asset


@asset()
def base() -> list[dict]:
    return [{"n": 1}, {"n": 2}]


@asset(inputs={"base": base})
def report(base: list[dict]) -> list[dict]:
    return [{"n": sum(row["n"] for row in base)}]
"""

# Days of CPU in little memory: a cross product of small tables.
RUNAWAY = "select count(*) from range(100000) a, range(100000) b, range(100000) c"


def _env(**more: str) -> dict[str, str]:
    return clean_env(BARCA_POOL_SIZE="2", **more)


@pytest.fixture()
def project(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    # A store, as a project with a server has: `barca sql --server` reads result views from it.
    store = tmp_path / "store"
    store.mkdir()
    (root / "barca.toml").write_text(f'[remote]\nuri = "{store}"\n')
    (root / "pipeline.py").write_text(PIPELINE)
    return root


@pytest.fixture()
def server(project):
    with Serve(project, "--no-schedule", env=_env()) as s:
        yield s


def finished(server: Serve, target: str) -> dict:
    """Run `target` on the server and wait for the run to end."""
    run_id = server.submit(target, "get")["run_id"]
    return server.wait(
        f"run {run_id}",
        lambda: (run := server.run(run_id))["status"] not in LIVE and run,
    )


def ask(server: Serve, query: str, limit: int | None = None) -> tuple[int, dict]:
    body: dict = {"query": query}
    if limit is not None:
        body["limit"] = limit
    return server.request("POST", "/sql/query", body)


def helpers_of(server: Serve) -> list[int]:
    """The query helpers whose parent is this server."""
    out = subprocess.run(
        ["pgrep", "-P", str(server.proc.pid), "-f", "barca._sql_server"],
        capture_output=True,
        text=True,
        check=False,
    )
    return [int(pid) for pid in out.stdout.split()]


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def without_duckdb(tmp: Path) -> str:
    """A directory that, first on `PYTHONPATH`, makes `import duckdb` fail."""
    shadow = tmp / "no-duckdb"
    shadow.mkdir(exist_ok=True)
    (shadow / "duckdb.py").write_text("raise ImportError(\"No module named 'duckdb'\")\n")
    return str(shadow)


def cli_sql(project: Path, server: Serve, query: str, *args: str, env: dict | None = None):
    out = helpers.barca(
        project,
        "sql",
        query,
        "--json",
        "--server",
        server.url,
        *args,
        env=_env() if env is None else env,
        timeout=QUICK,
    )
    return out.returncode, (json.loads(out.stdout) if out.returncode == 0 else None), out.stderr


def test_an_aggregate_over_barca_runs_is_answered_by_the_server(project, server, tmp_path):
    assert helpers_of(server) == [], "the helper starts with the first query"
    assert finished(server, "report")["status"] == "success"
    status, body = ask(
        server,
        "select status, count(*) as runs, sum(steps_executed) as steps from barca.runs group by all",
    )
    assert status == 200, body
    assert body == {
        "rows": [{"status": "success", "runs": 1, "steps": 2}],
        "total": 1,
        "truncated": False,
        "columns": ["status", "runs", "steps"],
    }
    # The same document `barca sql --json` prints on the server's machine.
    local = helpers.barca(
        project,
        "sql",
        "select status, count(*) as runs, sum(steps_executed) as steps from barca.runs group by all",
        "--json",
        env=_env(),
        timeout=QUICK,
    )
    assert json.loads(local.stdout) == body, local.stderr

    # Every one of barca's tables, joined; `limit` cuts the rows and `total` counts them.
    status, body = ask(
        server,
        "select n.name, m.status from barca.nodes n "
        "join barca.materializations m on m.node_id = n.id order by n.name",
        limit=1,
    )
    assert status == 200, body
    assert body["rows"] == [{"name": "base", "status": "success"}]
    assert (body["total"], body["truncated"]) == (2, True)
    assert "limit" in body["hint"]

    # A run recorded after the tables were loaded shows in the next query.
    (project / "pipeline.py").write_text(PIPELINE.replace('"n": 2', '"n": 5'))
    assert finished(server, "report")["status"] == "success"
    status, body = wait_until(
        lambda: (r := ask(server, "select count(*) as n from barca.runs"))[1]["rows"] == [{"n": 2}]
        and r,
        "the second run in barca.runs",
    )
    assert status == 200

    # One helper for all of it.
    assert len(helpers_of(server)) == 1

    # `barca sql --server` sends a query over barca's tables to the route: it is answered on a
    # machine that has no DuckDB. A query over a result view still runs on this machine.
    bare = _env(PYTHONPATH=without_duckdb(tmp_path))
    code, body, err = cli_sql(project, server, "select count(*) as n from barca.runs", env=bare)
    assert code == 0, err
    assert body["rows"] == [{"n": 2}] and body["columns"] == ["n"]
    code, _, err = cli_sql(project, server, "select n from report", env=bare)
    assert code == 2 and "duckdb" in err, err
    code, body, err = cli_sql(project, server, "select n from report")
    assert code == 0 and body["rows"] == [{"n": 6}], err
    # Mixed: a result view beside one of barca's tables runs here as well.
    code, body, err = cli_sql(
        project, server, "select (select count(*) from barca.runs) as runs, n from report"
    )
    assert code == 0 and body["rows"] == [{"runs": 2, "n": 6}], err


def test_a_read_only_server_answers_it(project, server):
    assert finished(server, "report")["status"] == "success"
    with Serve(project, "--read-only", env=_env()) as reader:
        status, body = ask(reader, "select count(*) as n from barca.runs")
        assert status == 200, body
        assert body["rows"] == [{"n": 1}]
        status, body = ask(reader, "select count(*) as n from barca.nodes where state = 'cached'")
        assert status == 200 and body["rows"] == [{"n": 2}], body
        # Reading is all it does: a run is still refused.
        status, _ = reader.request("POST", "/runs", {"command": "get", "targets": ["report"]})
        assert status == 403


def test_a_token_is_required_where_tokens_are(project):
    bootstrap = helpers.barca(project, "tokens", "bootstrap", env=_env(), timeout=QUICK)
    assert bootstrap.returncode == 0, bootstrap.stderr
    server = TokenServer(project, "--auth", "token", "--no-schedule")
    try:
        query = {"query": "select count(*) as n from barca.runs"}
        status, body = exchange(server, "POST", "/sql/query", body=query)
        assert status == 401 and body["reason"] == "authentication_required", body
        status, body = exchange(server, "POST", "/sql/query", "not-a-token", query)
        assert status == 401, body
        assert helpers_of(server) == [], "an unauthorized query starts nothing"
        code = bootstrap.stdout.strip()
        _, connection = exchange(server, "POST", "/auth/bootstrap", body={"code": code})
        status, body = exchange(server, "POST", "/sql/query", connection["token"], query)
        assert status == 200, body
        assert body["rows"] == [{"n": 0}]
        # The CLI sends its token to the route as to every other.
        out = helpers.barca(
            project,
            "sql",
            query["query"],
            "--json",
            "--server",
            server.url,
            env=_env(BARCA_SERVER_TOKEN=connection["token"]),
            timeout=QUICK,
        )
        assert out.returncode == 0, out.stderr
        assert json.loads(out.stdout)["rows"] == [{"n": 0}]
    finally:
        server.kill()


def test_only_one_select_over_barcas_tables_is_answered(project, server, tmp_path):
    assert finished(server, "report")["status"] == "success"
    secret = tmp_path / "secret.csv"
    secret.write_text("password\nhunter2\n")
    written = tmp_path / "written.csv"
    attempts = [
        # A result view: the server reads no result.
        "select * from report",
        'select * from "pipeline.py:report"',
        # A file or a URL, by a reader or by name.
        f"select * from read_parquet('{secret}')",
        f"select * from read_csv('{secret}')",
        f"select * from read_text('{secret}')",
        f"select * from '{secret}'",
        "select * from glob('*')",
        "select * from glob('/etc/*')",
        "select * from 'https://example.invalid/data.parquet'",
        # Anything that is not a SELECT.
        f"ATTACH '{written}' AS other",
        f"COPY barca.runs TO '{written}'",
        f"COPY (select * from barca.runs) TO '{written}'",
        f"EXPORT DATABASE '{written}'",
        "PRAGMA database_list",
        "INSTALL httpfs",
        "LOAD httpfs",
        "SET enable_external_access = true",
        "DROP TABLE barca.runs",
        "DELETE FROM barca.runs",
        "CREATE TABLE mine AS SELECT 1",
        # A second statement.
        "select 1; select 2",
        "select 1; DELETE FROM barca.runs",
        f"select 1; COPY barca.runs TO '{written}'",
        # A write hidden in a SELECT.
        "WITH gone AS (DELETE FROM barca.runs RETURNING *) SELECT * FROM gone",
        "WITH new AS (INSERT INTO barca.runs (run_id) VALUES ('mine') RETURNING run_id) "
        "SELECT * FROM new",
        # The server's environment.
        "select getenv('HOME')",
        "select getenv('PATH')",
    ]
    for attempt in attempts:
        status, body = ask(server, attempt)
        assert status == 400, (attempt, body)
        assert body["kind"] == "usage" and body["code"] == 2, (attempt, body)
        assert "barca.runs, barca.materializations, barca.nodes" in body["remediation"], body
        assert "hunter2" not in json.dumps(body), attempt
    assert not written.exists()
    # Nothing was written beside the project either (DuckDB's default temp directory).
    assert not (project / ".tmp").exists()
    status, body = ask(server, "select * from report")
    assert body["error"] == "no table named 'report' on the server", body
    # Malformed requests are usage errors too.
    for bad in ({}, {"query": ""}, {"query": "select 1", "rows": 5}, {"query": 7}):
        status, body = server.request("POST", "/sql/query", bad)
        assert status == 400 and body["kind"] == "usage", (bad, body)
    assert server.request("GET", "/sql/query")[0] == 405
    # Nothing was changed, the same helper is still there, and it still reads no file.
    status, body = ask(server, "select count(*) as n from barca.runs")
    assert (status, body["rows"]) == (200, [{"n": 1}])
    status, body = ask(
        server, "select value from duckdb_settings() where name = 'enable_external_access'"
    )
    assert body["rows"] == [{"value": "false"}]
    assert len(helpers_of(server)) == 1
    # The CLI passes the server's refusal on as a usage error.
    code, _, err = cli_sql(project, server, "DELETE FROM barca.runs")
    assert code == 2, err
    # The limit: 1,000 rows by default, 10,000 at most.
    status, body = ask(server, "select * from range(20000)")
    assert (len(body["rows"]), body["total"], body["truncated"]) == (1000, 20000, True)
    status, body = ask(server, "select * from range(20000)", limit=50000)
    assert (len(body["rows"]), body["total"]) == (10000, 20000)


def test_a_query_past_five_seconds_is_a_408_and_stops(project, server):
    status, _ = ask(server, "select 1")
    assert status == 200
    (helper,) = helpers_of(server)
    started = time.monotonic()
    status, body = ask(server, RUNAWAY)
    took = time.monotonic() - started
    assert status == 408, body
    assert body["kind"] == "usage" and "5s" in body["error"], body
    assert 4.5 < took < 8, took
    # Cancelled, not abandoned: the same helper answers the next query at once.
    started = time.monotonic()
    status, body = ask(server, "select 42 as answer")
    assert (status, body["rows"]) == (200, [{"answer": 42}])
    assert time.monotonic() - started < 2
    assert helpers_of(server) == [helper]
    # The CLI: a usage error naming the limit.
    code, _, err = cli_sql(
        project, server, RUNAWAY + " where (select count(*) from barca.runs) >= 0"
    )
    assert code == 2 and "5s" in err, err


def test_a_killed_helper_is_started_again_and_none_outlives_its_server(project, server):
    status, _ = ask(server, "select count(*) from barca.runs")
    assert status == 200
    (first,) = helpers_of(server)
    os.kill(first, signal.SIGKILL)
    wait_until(lambda: not alive(first) or helpers_of(server) == [], "the helper to die")
    status, body = ask(server, "select count(*) as n from barca.runs")
    assert (status, body["rows"]) == (200, [{"n": 0}])
    (second,) = helpers_of(server)
    assert second != first

    # Killed under a query: that query is a 503 that says to send it again.
    answers: list[tuple[int, dict]] = []
    running = threading.Thread(target=lambda: answers.append(ask(server, RUNAWAY)))
    running.start()
    time.sleep(1)
    os.kill(second, signal.SIGKILL)
    running.join(timeout=30)
    ((status, body),) = answers
    assert status == 503, body
    assert body["kind"] == "infra" and "again" in body["remediation"], body
    status, body = ask(server, "select 1 as one")
    assert (status, body["rows"]) == (200, [{"one": 1}])
    (third,) = helpers_of(server)

    # The other routes never noticed.
    assert server.request("GET", "/health")[0] == 200
    # A server killed outright leaves no helper behind.
    server.kill()
    wait_until(lambda: not alive(third), "the helper to exit with its server")


def test_without_duckdb_the_route_is_a_503_and_the_cli_runs_the_query_itself(project, tmp_path):
    env = _env(PYTHONPATH=without_duckdb(tmp_path))
    with Serve(project, "--no-schedule", env=env) as server:
        status, body = ask(server, "select count(*) as n from barca.runs")
        assert status == 503, body
        assert body["kind"] == "infra" and "duckdb" in body["error"], body
        assert "barca[sql]" in body["remediation"], body
        # Every other route is unaffected.
        assert server.request("GET", "/health")[0] == 200
        assert server.request("GET", "/state")[0] == 200
        # This machine has DuckDB: `barca sql --server` answers as it did before the route.
        code, body, err = cli_sql(project, server, "select count(*) as n from barca.runs")
        assert code == 0, err
        assert body["rows"] == [{"n": 0}]
