"""Debugging from the CLI: `barca status --problems` / `--state`, schedules in `barca status`,
and `barca doctor` checking a server (`barca docs agents`, "Debugging pipelines").

One `barca serve` with real problems: an asset that raises, an asset whose code changed since
it ran (stale), a scheduled task that fails every tick until `max_failures` skips it, and a
deployed file that does not parse. A laptop checkout points at it with `[server] url` and
another store than the server's.
"""

import json
import signal
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

from .test_server_ledger import Team, _env, _free_port, _kill_server_mid_run, posts, wait_for

WAIT = 60.0

PIPELINE = """
from barca import Schedule, asset, task


@asset()
def orders() -> dict:
    return {"n": 3}


@asset(inputs={"orders": orders})
def ratio(orders: dict) -> float:
    return orders["n"] / 0


@asset()
def prices() -> dict:
    return {"v": 1}


@task(freshness=Schedule("*/1 * * * * *", max_failures=2))
def nightly_sync() -> None:
    raise RuntimeError("upstream API returned 503")
"""

PROBLEMS = {"pipeline.py:ratio", "pipeline.py:prices", "pipeline.py:nightly_sync"}


def barca(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=_env(),
        timeout=WAIT,
    )


def http(url: str) -> tuple[int, object]:
    import urllib.error
    import urllib.request

    try:
        with urllib.request.urlopen(url, timeout=10) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())


class Deployment:
    """A served project with problems, and a laptop checkout configured to use it."""

    def __init__(self, tmp: Path) -> None:
        self.server_root = tmp / "server"
        self.laptop = tmp / "laptop"
        self.store = tmp / "store"
        self.other_store = tmp / "other-store"
        for d in (self.server_root, self.laptop, self.store, self.other_store):
            d.mkdir()
        self.port = _free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        (self.server_root / "pipeline.py").write_text(PIPELINE)
        (self.server_root / "barca.toml").write_text(f'[remote]\nuri = "{self.store}"\n')
        (self.laptop / "pipeline.py").write_text(PIPELINE)
        (self.laptop / "barca.toml").write_text(
            f'[remote]\nuri = "{self.other_store}"\n\n[server]\nurl = "{self.url}"\n'
        )
        # `prices` ran, then its code changed: stale. `ratio` raised: never run, failed.
        assert barca(self.server_root, "get", "prices").returncode == 0
        source = self.server_root / "pipeline.py"
        source.write_text(source.read_text().replace('{"v": 1}', '{"v": 2}'))
        assert barca(self.server_root, "get", "ratio").returncode == 1
        (self.server_root / "broken.py").write_text("def broken(:\n    pass\n")
        self.log = self.server_root / "serve.log"
        self.proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "broken.py", "--port", str(self.port)],
            cwd=self.server_root,
            stdout=subprocess.DEVNULL,
            stderr=self.log.open("a"),
            env=_env(),
        )
        try:
            wait_for(self._up, "the server to answer /health")
            wait_for(self._failing, "nightly_sync to fail max_failures ticks")
        except BaseException:
            self.stop()
            raise

    def _up(self) -> bool:
        assert self.proc.poll() is None, f"the server exited early:\n{self.log.read_text()}"
        try:
            return http(f"{self.url}/health")[0] == 200
        except OSError:
            return False

    def schedule(self) -> dict | None:
        status, jobs = http(f"{self.url}/schedule")
        assert status == 200, jobs
        return next((j for j in jobs if j["id"] == "pipeline.py:nightly_sync"), None)

    def _failing(self) -> bool:
        job = self.schedule()
        return job is not None and job["failing"]

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=WAIT)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=WAIT)


@pytest.fixture(scope="module")
def deployment(tmp_path_factory):
    d = Deployment(tmp_path_factory.mktemp("debugging"))
    yield d
    d.stop()


def nodes_by_id(body: dict) -> dict[str, dict]:
    return {n["id"]: n for n in body["nodes"]}


def check(report: dict, name: str) -> dict:
    [found] = [c for c in report["checks"] if c["name"] == name]
    return found


# ─── barca status --problems ─────────────────────────────────────────────────


def test_problems_from_the_configured_server(deployment):
    out = barca(deployment.laptop, "status", "--problems", "--json")
    assert out.returncode == 0, out.stderr
    assert f"[barca] server {deployment.url}" in out.stderr
    body = json.loads(out.stdout)
    nodes = nodes_by_id(body)
    assert set(nodes) == PROBLEMS, nodes.keys()
    assert body["filter"] == {"problems": True}
    assert body["total"] == 3
    # The summary still counts every node.
    assert body["summary"]["cached"] == 1
    assert body["summary"]["stale"] == 1
    assert body["summary"]["never_run"] == 1
    assert body["summary"]["always_runs"] == 1

    ratio = nodes["pipeline.py:ratio"]
    assert ratio["cache"]["reason"] == "failed"
    assert ratio["last_materialization"]["status"] == "failed"
    assert ratio["last_materialization"]["error"].startswith("ZeroDivisionError: division by zero")
    assert nodes["pipeline.py:prices"]["cache"]["reason"] == "changed"

    # The schedule is the server's scheduler's view: the numbers GET /schedule (the UI) has.
    schedule = nodes["pipeline.py:nightly_sync"]["schedule"]
    job = deployment.schedule()
    assert schedule["live"] is True
    assert schedule["cron"] == "*/1 * * * * *"
    assert schedule["failing"] is True
    assert schedule["failures"] == job["failures"] == 2
    assert schedule["max_failures"] == job["max_failures"] == 2
    assert schedule["paused"] is job["paused"] is False
    assert schedule["last_status"] == "failed"
    assert schedule["next_fire"].endswith("Z")
    error = nodes["pipeline.py:nightly_sync"]["last_materialization"]["error"]
    assert error.startswith("RuntimeError: upstream API returned 503")

    # A node that is fine has no schedule key and is not shown.
    full = json.loads(barca(deployment.laptop, "status", "--json").stdout)
    assert "schedule" not in nodes_by_id(full)["pipeline.py:orders"]


def test_the_table_has_the_error_the_schedule_and_the_filter(deployment):
    out = barca(deployment.laptop, "status", "--problems", "--pretty")
    assert out.returncode == 0, out.stderr
    table = out.stdout
    assert "SCHEDULE" in table.splitlines()[0]
    assert "failed: ZeroDivisionError: division by zero" in table
    assert "last run failed: RuntimeError: upstream API returned 503" in table
    assert "*/1 * * * * *; failing 2/2" in table
    assert "3 of 4 nodes match --problems; the counts below are for every node" in table
    assert not any(line.startswith("orders ") for line in table.splitlines())


def test_state_filters_by_cache_state(deployment):
    body = json.loads(barca(deployment.laptop, "status", "--state", "stale", "--json").stdout)
    assert [n["id"] for n in body["nodes"]] == ["pipeline.py:prices"]
    assert body["filter"] == {"states": ["stale"]}
    body = json.loads(
        barca(deployment.laptop, "status", "--state", "never-run,cached", "--json").stdout
    )
    assert {n["id"] for n in body["nodes"]} == {"pipeline.py:orders", "pipeline.py:ratio"}

    out = barca(deployment.laptop, "status", "--state", "failed", "--json")
    assert out.returncode == 2, out.stderr
    envelope = json.loads(out.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "usage"
    assert "unknown state 'failed'" in envelope["error"]
    assert "--problems" in envelope["remediation"]

    out = barca(deployment.laptop, "status", "--state", "cached", "--problems", "--pretty")
    assert out.returncode == 0, out.stderr
    assert "No node matches --problems --state cached." in out.stdout


def test_problems_on_the_servers_own_machine_read_the_metadata_db(deployment):
    # No [server] here: status runs locally. No scheduler runs in this command, so the schedule
    # is what the metadata DB holds (the failures `serve` persisted), with `live: false`.
    out = barca(deployment.server_root, "status", "pipeline.py", "--problems", "--json")
    assert out.returncode == 0, out.stderr
    nodes = nodes_by_id(json.loads(out.stdout))
    assert set(nodes) == PROBLEMS
    schedule = nodes["pipeline.py:nightly_sync"]["schedule"]
    assert schedule["live"] is False
    assert schedule["failing"] is True
    assert schedule["failures"] == 2
    assert schedule["last_status"] is None
    assert schedule["last_fired"] is not None


def test_a_laptop_with_local_shows_its_own_state(deployment):
    out = barca(deployment.laptop, "status", "--local", "--problems", "--json")
    assert out.returncode == 0, out.stderr
    nodes = nodes_by_id(json.loads(out.stdout))
    # Nothing ran here: every asset is never_run, the task has no failed run and no failures.
    assert set(nodes) == {"pipeline.py:orders", "pipeline.py:ratio", "pipeline.py:prices"}


def test_nodes_takes_state_and_problems(deployment):
    status, body = http(f"{deployment.url}/nodes?problems=1")
    assert status == 200, body
    assert {n["id"] for n in body["nodes"]} == PROBLEMS
    assert body["summary"]["cached"] == 1
    status, body = http(f"{deployment.url}/nodes?state=stale")
    assert [n["id"] for n in body["nodes"]] == ["pipeline.py:prices"]
    status, body = http(f"{deployment.url}/nodes?state=bogus")
    assert status == 400, body
    assert "unknown state 'bogus'" in body["error"]


def test_a_paused_schedule_is_a_problem(deployment):
    import urllib.request

    pause = urllib.request.Request(
        f"{deployment.url}/schedule/nightly_sync/pause?until=2099-01-01T00:00:00Z", method="POST"
    )
    urllib.request.urlopen(pause, timeout=10).read()
    try:
        body = json.loads(barca(deployment.laptop, "status", "--problems", "--json").stdout)
        schedule = nodes_by_id(body)["pipeline.py:nightly_sync"]["schedule"]
        assert schedule["paused"] is True
        assert schedule["paused_until"] == "2099-01-01T00:00:00Z"
    finally:
        resume = urllib.request.Request(
            f"{deployment.url}/schedule/nightly_sync/resume", method="POST"
        )
        urllib.request.urlopen(resume, timeout=10).read()


# ─── barca doctor with a server ──────────────────────────────────────────────


def test_doctor_checks_the_configured_server(deployment):
    out = barca(deployment.laptop, "doctor", "--json")
    assert out.returncode == 3, out.stderr
    assert f"[barca] server {deployment.url}" in out.stderr
    report = json.loads(out.stdout)
    assert report["ok"] is False

    assert check(report, "server")["ok"] is True
    assert check(report, "server")["data"]["url"] == deployment.url
    version = check(report, "server_version")
    assert version["ok"] is True
    assert version["data"]["api_version"] == version["data"]["client_api_version"]

    store = check(report, "server_store")
    assert store["ok"] is False
    assert str(deployment.store) in store["detail"]
    assert str(deployment.other_store) in store["detail"]
    assert "Set [remote] uri" in store["detail"].splitlines()[-1]

    assert check(report, "server_health")["ok"] is True

    sources = check(report, "server_sources")
    assert sources["ok"] is False
    assert "broken.py" in sources["detail"]
    assert sources["data"]["load_errors"][0]["file"] == "broken.py"

    scheduler = check(report, "server_scheduler")
    assert scheduler["ok"] is False
    assert scheduler["data"]["running"] is True
    assert scheduler["data"]["schedules"] == 1
    assert scheduler["data"]["failing"] == ["pipeline.py:nightly_sync"]
    assert "barca run pipeline.py:nightly_sync --server" in scheduler["detail"]

    assert check(report, "server_posts")["ok"] is True

    human = barca(deployment.laptop, "doctor", "--pretty")
    assert human.returncode == 3
    assert "[error] server_sources: the server did not load 1 file" in human.stdout
    assert "[error] server_scheduler: running; 1 schedule, 1 failing" in human.stdout


def test_doctor_local_and_an_explicit_url(deployment, tmp_path):
    out = barca(deployment.laptop, "doctor", "--local", "--json")
    names = [c["name"] for c in json.loads(out.stdout)["checks"]]
    assert not any(n.startswith("server") for n in names), names
    assert "[barca] server" not in out.stderr

    # A directory with no server configured: --server <url> names one.
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    (tmp_path / "barca.toml").write_text(f'[remote]\nuri = "{deployment.store}"\n')
    out = barca(tmp_path, "doctor", "--server", deployment.url, "--json")
    report = json.loads(out.stdout)
    assert check(report, "server_store")["ok"] is True
    assert check(report, "server_sources")["ok"] is False

    out = barca(tmp_path, "doctor", "--server", "--json")
    assert out.returncode == 2, out.stderr
    assert "--server has no URL and no server is configured" in out.stderr


def test_doctor_names_a_server_that_cannot_be_reached(tmp_path):
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    url = f"http://127.0.0.1:{_free_port()}"
    (tmp_path / "barca.toml").write_text(
        f'[remote]\nuri = "{tmp_path / "store"}"\n\n[server]\nurl = "{url}"\n'
    )
    out = barca(tmp_path, "doctor", "--json")
    assert out.returncode == 3, out.stderr
    server = check(json.loads(out.stdout), "server")
    assert server["ok"] is False
    assert f"cannot reach the barca server at {url}" in server["detail"]
    assert "--local" in server["detail"].splitlines()[-1]


def test_doctor_reports_runs_not_posted_to_the_server(tmp_path):
    team = Team(tmp_path)
    try:
        out = _kill_server_mid_run(team, team.a)
        assert out.returncode == 0, out.stderr
        run_id = json.loads(out.stdout)["run_id"]
        [post] = posts(team.a)
        assert post["state"] == "unsynced"
        team.start()

        out = barca(team.a, "doctor", "--json")
        assert out.returncode == 3, out.stderr
        report = json.loads(out.stdout)
        posted = check(report, "server_posts")
        assert posted["ok"] is False
        assert posted["data"]["runs"] == [run_id]
        assert run_id in posted["detail"]
        assert "The next `barca get` or `barca run`" in posted["detail"].splitlines()[-1]
        # --no-schedule: the scheduler not running is not a failure.
        assert check(report, "server_scheduler")["ok"] is True
        assert check(report, "server_store")["ok"] is True
        # Doctor sends nothing: the post is still waiting.
        assert [p["state"] for p in posts(team.a)] == ["unsynced"]
    finally:
        team.stop()
