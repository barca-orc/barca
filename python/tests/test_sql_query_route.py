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
import urllib.error
import urllib.request
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


def rss_mb(pid: int) -> int:
    """The resident memory of `pid` now, in MB."""
    out = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True, check=True
    )
    return int(out.stdout) // 1024


def raw(server: Serve, body: dict) -> tuple[int, bytes, dict]:
    """`POST /sql/query`: the status, the undecoded body and the headers."""
    request = urllib.request.Request(
        f"{server.url}/sql/query",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read(), dict(response.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read(), dict(e.headers)


def test_an_answer_is_a_few_megabytes_at_most_and_memory_stays_bounded(project, server):
    """Wide rows are refused in the helper before they are read to the end. DuckDB's memory
    limit covers its own buffers only: without the cap these two queries took the helper and
    the server past 4GB each."""
    assert ask(server, "select 1")[0] == 200
    (helper,) = helpers_of(server)
    peak = {"server": 0, "helper": 0}
    done = threading.Event()

    def watch() -> None:
        while not done.is_set():
            peak["server"] = max(peak["server"], rss_mb(server.proc.pid))
            peak["helper"] = max(peak["helper"], rss_mb(helper))
            time.sleep(0.02)

    watcher = threading.Thread(target=watch)
    watcher.start()
    try:
        for wide in (
            "select repeat('x', 100000) as s from range(10000)",
            "select range(3000) as l from range(10000)",
        ):
            status, body, _ = raw(server, {"query": wide, "limit": 10000})
            assert status == 400, (wide, body[:300])
            assert len(body) < 2000, (wide, len(body))
            envelope = json.loads(body)
            assert envelope["kind"] == "usage", envelope
            assert "4MB" in envelope["error"] and "narrower" in envelope["error"], envelope
        # Under the cap the same rows are answered: about 1MB of them.
        status, body, _ = raw(
            server, {"query": "select repeat('x', 100000) as s from range(10000)", "limit": 10}
        )
        assert status == 200 and 1_000_000 < len(body) < 1_100_000, (status, len(body))
        assert json.loads(body)["total"] == 10000
        # The largest answer there is: just under the cap.
        status, body, _ = raw(
            server, {"query": "select repeat('x', 390) as s from range(10000)", "limit": 10000}
        )
        assert status == 200 and len(body) < 5 * 1024 * 1024, (status, len(body))
        assert len(json.loads(body)["rows"]) == 10000
    finally:
        done.set()
        watcher.join()
    # DuckDB may use its 1GB; Python and the server hold megabytes on top, not the rows.
    assert peak["helper"] < 1800, peak
    assert peak["server"] < 600, peak
    assert helpers_of(server) == [helper], "the helper was not replaced"


def test_a_query_that_needs_too_much_memory_is_a_usage_error_and_the_helper_stays(server):
    assert ask(server, "select count(*) from barca.runs")[0] == 200
    (helper,) = helpers_of(server)
    for _ in range(3):
        status, body = ask(server, "select string_agg(repeat('x', 1000), '') from range(100000000)")
        assert status == 400, body
        assert body["kind"] == "usage" and "memory" in body["error"], body
    status, body = ask(server, "select count(*) as n from barca.runs")
    assert (status, body["rows"]) == (200, [{"n": 0}])
    assert helpers_of(server) == [helper]


def test_one_client_cannot_hold_the_route_for_everyone(project, server):
    """Forty slow queries at once: four are admitted (one running, three waiting) and end 408;
    the others are answered 429 at once, with when to try again."""
    assert ask(server, "select 1")[0] == 200
    answers: list[tuple[int, float, dict]] = []

    def slow() -> None:
        started = time.monotonic()
        status, _, headers = raw(server, {"query": RUNAWAY})
        answers.append((status, time.monotonic() - started, headers))

    threads = [threading.Thread(target=slow) for _ in range(40)]
    for t in threads:
        t.start()
    # While they are held, another client is told at once, not after five seconds.
    time.sleep(1)
    started = time.monotonic()
    status, body = ask(server, "select 1")
    assert status == 429 and time.monotonic() - started < 1, (status, body)
    assert body["kind"] == "infra" and "again" in body["remediation"], body
    code, _, err = cli_sql(project, server, "select count(*) from barca.runs")
    assert code == 3 and "queries already" in err, err
    for t in threads:
        t.join(timeout=60)
    by_status: dict[int, int] = {}
    for status, _, _ in answers:
        by_status[status] = by_status.get(status, 0) + 1
    assert by_status == {408: 4, 429: 36}, by_status
    for status, took, headers in answers:
        if status == 429:
            assert took < 2 and headers.get("retry-after") == "1", (took, headers)
    # The route is free again as soon as they end.
    status, body = ask(server, "select 1 as one")
    assert (status, body["rows"]) == (200, [{"one": 1}])


def test_the_helper_runs_outside_the_project_and_names_no_directory_of_the_server(project, server):
    # A module in the project root that DuckDB would import by name (`test_all_types()` asks
    # for `pytz`): the helper does not start in the project, so it is never imported.
    marker = project / "imported-by-the-helper"
    (project / "pytz.py").write_text(
        f"open({str(marker)!r}, 'w').close()\nraise ImportError('not the real pytz')\n"
    )
    private = [str(Path.home()), str(project), str(project.resolve()), ".duckdb"]
    for probe in (
        "select * from test_all_types()",
        "select * from duckdb_secrets()",
        "select * from duckdb_extensions()",
        "select current_setting('secret_directory') as d",
        "select current_setting('extension_directory') as d",
        "select current_setting('home_directory') as d",
        "select name, value from duckdb_settings()",
        "select * from read_csv('pipeline.py')",
        "select * from glob('~/*')",
        "select getenv('HOME')",
    ):
        _, body, _ = raw(server, {"query": probe})
        for path in private:
            assert path.encode() not in body, (probe, path, body[:400])
    assert not marker.exists()
    assert not (project / ".tmp").exists()
    status, body = ask(server, "select current_setting('secret_directory') as d")
    assert (status, body["rows"]) == (200, [{"d": ""}])


def test_a_query_is_run_as_sent_and_reaches_no_table_unloaded(project, server):
    assert finished(server, "report")["status"] == "success"
    # A trailing comment or `;` on a query whose rows are cut and counted.
    for query in (
        "select * from range(5000) -- c",
        "select * from range(5000); -- c",
        "select * from range(5000) /* c */ ;",
    ):
        status, body = ask(server, query)
        assert status == 200, (query, body)
        assert (len(body["rows"]), body["total"], body["truncated"]) == (1000, 5000, True)
    # A table reached by a name the text does not show is loaded like one that is named.
    status, body = ask(server, "select count(*) as n from query_table('barca.' || 'ru' || 'ns')")
    assert (status, body["rows"]) == (200, [{"n": 1}]), body
    status, body = ask(
        server, "select * from query('select count(*) as n from barca.' || 'no' || 'des')"
    )
    assert (status, body["rows"]) == (200, [{"n": 2}]), body


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


# A sensor polled every two seconds with an asset below it, and a step slow enough to hold the
# one run slot while another run waits.
SCHEDULED = """
import time

from barca import Always, Schedule, asset, sensor


@sensor(freshness=Schedule("*/2 * * * * *"))
def pulse() -> int:
    return 1


@asset(inputs={"p": pulse}, freshness=Always)
def beat(p: int) -> dict:
    return {"p": p}


@asset()
def slow() -> dict:
    time.sleep(4)
    return {}


@asset()
def quick() -> dict:
    return {}
"""


def test_the_schedules_the_ticks_and_the_queue_are_tables_of_a_scheduling_server(project):
    """#965: `barca.schedules` is the scheduler's own state, `barca.ticks` what it recorded,
    `barca.queue` the run queue as `GET /queue` lists it; a query that also names a result
    view runs on the client with the same rows."""
    (project / "pipeline.py").write_text(SCHEDULED)
    with Serve(project, "--max-runs", "1", env=_env()) as server:

        def rows(query: str) -> list[dict]:
            status, body = ask(server, query)
            assert status == 200, body
            return body["rows"]

        # The schedule as `GET /schedule` reports it, with its interval and one tick's targets.
        ticked = "select * from barca.ticks where finished_at is not null order by scheduled_at"
        ticks = server.wait("a finished tick", lambda: rows(ticked), timeout=60)
        (schedule,) = rows("select * from barca.schedules")
        (served,) = [s for s in server.get("/schedule") if s["id"] == "pipeline.py:pulse"]
        assert schedule["id"] == "pipeline.py:pulse" and schedule["name"] == "pulse"
        assert (schedule["kind"], schedule["cron"]) == ("sensor", "*/2 * * * * *")
        assert schedule["interval_seconds"] == 2
        assert schedule["targets"] == ["pipeline.py:pulse", "pipeline.py:beat"]
        assert schedule["live"] is True and schedule["paused"] is False
        assert schedule["last_fired"] is not None and schedule["last_run_id"] is not None
        for key in ("max_failures", "failing", "queue_bound", "paused_until"):
            assert schedule[key] == served[key], key

        # A tick: its slot, the run it started (a run of `barca.runs`), and what it did with
        # each target.
        tick = ticks[0]
        assert tick["schedule_id"] == "pipeline.py:pulse" and tick["action"] == "fired"
        assert tick["run_status"] == "success" and tick["queue_wait_seconds"] >= 0
        assert (tick["rebuilt"] + tick["up_to_date"], tick["failed"], tick["not_reached"]) == (
            2,
            0,
            0,
        ), tick
        assert [t["node"] for t in tick["targets"]] == ["pipeline.py:pulse", "pipeline.py:beat"]
        joined = rows(
            "select r.triggered_by, r.scheduled_at = t.scheduled_at as same_slot "
            f"from barca.ticks t join barca.runs r using (run_id) where t.run_id = '{tick['run_id']}'"
        )
        assert joined == [{"triggered_by": "schedule", "same_slot": True}]
        outcomes = rows(
            "select target.outcome from barca.ticks t, unnest(t.targets) as u(target) "
            f"where target.node = 'pipeline.py:beat' and t.run_id = '{tick['run_id']}'"
        )
        assert outcomes[0]["outcome"] in ("rebuilt", "up_to_date"), outcomes

        # One run slot: a slow run holds it and the next one waits, first in line.
        first = server.submit("slow", "get")["run_id"]
        server.wait("the slow run to start", lambda: server.run(first)["status"] == "running")
        second = server.submit("quick", "get")["run_id"]
        listed = "select * from barca.queue where triggered_by = 'api'"
        queue = server.wait(
            "both runs in the queue", lambda: len(q := rows(listed)) == 2 and q, timeout=30
        )
        by_run = {q["run_id"]: q for q in queue}
        running, waiting = by_run[first], by_run[second]
        assert (running["state"], running["position"]) == ("running", None), running
        assert running["started_at"] is not None and running["wait_seconds"] is not None
        assert waiting["state"] == "queued" and waiting["position"] >= 1, waiting
        assert waiting["estimated_start_at"] is not None, waiting
        assert (waiting["command"], waiting["target"]) == ("get", "quick"), waiting
        assert waiting["scheduled_at"] is None and waiting["schedule_id"] is None
        # A tick's run in the queue carries its slot.
        scheduled = (
            "select scheduled_at, schedule_id from barca.queue where triggered_by = 'schedule'"
        )
        for row in rows(scheduled):
            assert row["schedule_id"] == "pipeline.py:pulse" and row["scheduled_at"], row
        # The table is the queue now, not the queue when it was first read.
        server.wait(f"run {second}", lambda: server.run(second)["status"] not in LIVE, timeout=60)
        assert rows(listed) == []

        # A query that also names a result view runs here: the same schedule and ticks, read
        # from the server.
        code, mixed, err = cli_sql(
            project,
            server,
            "select s.id, s.live, (select count(*) from barca.ticks) > 0 as ticked, "
            "(select count(*) from barca.queue) >= 0 as queue, b.p from barca.schedules s, beat b",
        )
        assert code == 0, err
        assert mixed["rows"] == [
            {"id": "pipeline.py:pulse", "live": True, "ticked": True, "queue": True, "p": 1}
        ]

    # Without a server: the schedule and its ticks as the metadata database records them, no
    # scheduler, and no queue.
    def local(query: str):
        out = helpers.barca(project, "sql", query, "--json", "--local", env=_env(), timeout=QUICK)
        assert out.returncode == 0, out.stderr
        return json.loads(out.stdout)["rows"], out.stderr

    (recorded,), _ = local("select id, live, targets, interval_seconds from barca.schedules")
    assert recorded == {
        "id": "pipeline.py:pulse",
        "live": False,
        "targets": ["pipeline.py:pulse", "pipeline.py:beat"],
        "interval_seconds": 2,
    }
    kept, _ = local("select count(*) as ticks from barca.ticks where action = 'fired'")
    assert kept[0]["ticks"] >= 1
    none, err = local("select * from barca.queue")
    assert none == [] and "barca.queue is empty: the run queue is a server's" in err, err
