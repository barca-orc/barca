"""Server mode's ledger (RFC-0007 section 5, "Ledger suite"; #402).

Two copies of one project point at one `barca serve` (`[server] url`) and at the store it uses
(`[remote] uri`, a plain directory). Runs happen on the copies; the server answers "is this
computed?" and records what they computed, after checking each object in the store.

`--local` touches nothing on the server; `barca status`, `history` and `stats` answer from it,
and `barca status` marks what is here (`local`) and a result whose post is pending (`unsynced`,
`not_in_ledger`).
"""

import json
import os
import signal
import socket
import sqlite3
import stat
import subprocess
import time
import urllib.error
import urllib.request
from contextlib import closing
from pathlib import Path

import pytest

from barca.api import _find_binary

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")
WAIT = 60.0

PIPELINE = """
import random
import time
from pathlib import Path

from barca import Manual, asset, sensor, task


@asset()
def base() -> dict:
    return {"n": 1}


@asset(inputs={"base": base})
def middle(base: dict) -> dict:
    return {"n": base["n"] + 1}


@asset(inputs={"middle": middle})
def report(middle: dict) -> dict:
    return {"n": middle["n"] * 10}


@asset()
def noisy() -> float:
    return random.random()


@asset(freshness=Manual)
def pinned() -> float:
    return random.random()


@task(inputs={"base": base})
def publish(base: dict) -> None:
    print("published", base["n"])


@sensor()
def feed() -> int:
    return 3


@asset(inputs={"feed": feed})
def from_feed(feed: int) -> int:
    return feed * 2


@asset()
def slow() -> int:
    Path("slow.started").write_text("")
    deadline = time.time() + 60
    while not Path("release").exists() and time.time() < deadline:
        time.sleep(0.05)
    return 7
"""

NODES = ["pipeline.py:base", "pipeline.py:middle", "pipeline.py:report"]


def _env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    env["BARCA_POOL_SIZE"] = "2"
    return env


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(predicate, what: str, timeout: float = WAIT):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    pytest.fail(f"timed out after {timeout:.0f}s waiting for {what}")


def barca(cwd: Path, *args: str, timeout: float = WAIT) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=_env(),
        timeout=timeout,
    )


def toml(store: Path, url: str | None) -> str:
    text = f'[remote]\nuri = "{store}"\n'
    if url is not None:
        text += f'\n[server]\nurl = "{url}"\n'
    return text


class Server:
    """A `barca serve` of its own copy of the project, on `port`."""

    def __init__(self, root: Path, port: int) -> None:
        self.root = root
        self.port = port
        self.log = root / "serve.log"
        self.proc = subprocess.Popen(
            [_find_binary(), "serve", "pipeline.py", "--port", str(port), "--no-schedule"],
            cwd=root,
            stdout=subprocess.DEVNULL,
            stderr=self.log.open("a"),
            env=_env(),
        )
        wait_for(self._up, "the server to answer /health")

    def _up(self) -> bool:
        assert self.proc.poll() is None, f"the server exited early:\n{self.log.read_text()}"
        try:
            return self.get("/health")[0] == 200
        except OSError:
            return False

    def get(self, path: str) -> tuple[int, object]:
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}")
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return resp.status, json.loads(resp.read())
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read())

    def history(self) -> list[dict]:
        status, body = self.get("/history?limit=50")
        assert status == 200, body
        return body["runs"]

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGKILL)
        self.proc.wait(timeout=WAIT)


class Team:
    """One server and two project copies, `a` and `b`, sharing its store."""

    def __init__(self, tmp: Path) -> None:
        self.store = tmp / "store"
        self.store.mkdir()
        self.port = _free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.server_root = tmp / "server"
        self.a = tmp / "a"
        self.b = tmp / "b"
        for root, url in ((self.server_root, None), (self.a, self.url), (self.b, self.url)):
            root.mkdir()
            (root / "pipeline.py").write_text(PIPELINE)
            (root / "barca.toml").write_text(toml(self.store, url))
        self.servers: list[Server] = []
        self.start()

    @property
    def server(self) -> Server:
        return self.servers[-1]

    def start(self) -> Server:
        self.servers.append(Server(self.server_root, self.port))
        return self.server

    def stop(self) -> None:
        for s in self.servers:
            s.stop()


@pytest.fixture()
def team(tmp_path):
    t = Team(tmp_path)
    yield t
    t.stop()


def get(root: Path, *args: str) -> tuple[dict, str]:
    # `-o value --json`: the JSON line with `final_output` as the value (#404).
    out = barca(root, "get", *args, "-o", "value", "--json")
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout), out.stderr


def computed(result: dict) -> set[str]:
    """The steps this run executed (not served from cache)."""
    return {s["id"] for s in result["steps"] if s.get("status") not in (None, "cached")}


def posts(root: Path) -> list[dict]:
    """This machine's `ledger_posts` rows."""
    db = root / ".barca" / "metadata.db"
    with closing(sqlite3.connect(db)) as conn:
        conn.row_factory = sqlite3.Row
        exists = conn.execute(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'ledger_posts'"
        ).fetchone()
        if not exists:
            return []
        return [dict(r) for r in conn.execute("SELECT * FROM ledger_posts ORDER BY created_at")]


def local_copies(root: Path, name: str) -> list[Path]:
    """This machine's copies of `name`'s artifacts."""
    return [p for p in (root / ".barca").rglob(f"*{name}*/*") if p.is_file()]


# ─── the ledger suite ────────────────────────────────────────────────────────


def test_a_computes_and_b_hits_and_fetches(team):
    result_a, err_a = get(team.a, "report")
    assert computed(result_a) == set(NODES), result_a
    assert f"[barca] server {team.url}" in err_a
    assert "not recorded" not in err_a, err_a
    runs = team.server.history()
    assert len(runs) == 1, runs
    assert runs[0]["triggered_by"] == "client"
    assert runs[0]["client_run_id"] == result_a["run_id"]
    assert [p["state"] for p in posts(team.a)] == ["shared"]

    # B has never run anything: every step is a hit on the server's ledger, fetched from the
    # store, and the value is A's.
    result_b, err_b = get(team.b, "report")
    assert computed(result_b) == set(), result_b
    assert result_b["steps_executed"] == 0
    assert result_b["final_output"] == {"n": 20}
    [copy] = local_copies(team.b, "report")
    assert json.loads(copy.read_text()) == {"n": 20}
    out = barca(team.b, "get", "report", "-o", "value")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout) == {"n": 20}
    # Nothing new was computed, so nothing was posted.
    assert len(team.server.history()) == 1
    assert posts(team.b) == []


def test_a_dry_run_is_decided_against_the_ledger(team):
    get(team.a, "report")
    out = barca(team.b, "get", "report", "--dry-run", "--json")
    assert out.returncode == 0, out.stderr
    plan = json.loads(out.stdout)
    assert plan["summary"]["will_run"] == 0, plan
    assert plan["summary"]["cached"] == 3, plan
    # A dry run posts nothing and creates no run.
    assert len(team.server.history()) == 1


def test_b_edits_one_node_and_misses_exactly_one(team):
    get(team.a, "report")
    pipeline = team.b / "pipeline.py"
    pipeline.write_text(pipeline.read_text().replace('middle["n"] * 10', 'middle["n"] * 11'))
    result_b, _ = get(team.b, "report")
    assert computed(result_b) == {"pipeline.py:report"}, result_b
    assert result_b["final_output"] == {"n": 22}
    runs = team.server.history()
    assert len(runs) == 2, runs
    assert {r["client_run_id"] for r in runs} >= {result_b["run_id"]}
    # A's code still hits A's result.
    result_a, _ = get(team.a, "report")
    assert computed(result_a) == set()
    assert result_a["final_output"] == {"n": 20}


def test_the_same_run_hash_with_other_bytes_warns_once(team):
    get(team.a, "noisy")
    # B computes the same run hash again and gets other bytes: the server keeps A's row and
    # says so, and B prints the non-determinism warning (#392) once.
    result_b, err_b = get(team.b, "noisy", "--refresh", "noisy")
    assert computed(result_b) == {"pipeline.py:noisy"}
    warning = (
        "[barca] warning: pipeline.py:noisy produced different bytes for the same code and "
        "inputs; the function is not deterministic"
    )
    assert err_b.count(warning) == 1, err_b
    step = next(s for s in result_b["steps"] if s["id"] == "pipeline.py:noisy")
    assert step.get("bytes_changed") is True, step
    assert len(team.server.history()) == 2


def test_tasks_and_sensors_run_here_and_only_assets_are_recorded(team):
    out = barca(team.a, "run", "publish", "--json")
    assert out.returncode == 0, out.stderr
    assert "not recorded" not in out.stderr, out.stderr
    result_a, err = get(team.a, "from_feed")
    assert "not recorded" not in err, err
    assert computed(result_a) >= {"pipeline.py:feed", "pipeline.py:from_feed"}
    assert len(team.server.history()) == 2
    # The sensor runs on B too (it is never a cache hit); the asset reading its unchanged value
    # is a hit, asked for once the sensor has run.
    result_b, _ = get(team.b, "from_feed")
    assert computed(result_b) == {"pipeline.py:feed"}, result_b
    assert result_b["final_output"] == 6
    result_b, _ = get(team.b, "base")
    assert computed(result_b) == set()


def _kill_server_mid_run(team: Team, root: Path) -> subprocess.CompletedProcess:
    """Run `get slow` on `root` and stop the server while its step runs."""
    proc = subprocess.Popen(
        [_find_binary(), "get", "slow", "--json"],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_env(),
    )
    wait_for(lambda: (root / "slow.started").exists(), "the slow step to start")
    team.server.stop()
    (root / "release").write_text("")
    stdout, stderr = proc.communicate(timeout=WAIT)
    return subprocess.CompletedProcess(proc.args, proc.returncode, stdout, stderr)


def test_a_server_down_mid_post_leaves_the_run_successful_and_unsynced(team):
    out = _kill_server_mid_run(team, team.a)
    assert out.returncode == 0, out.stderr
    run_id = json.loads(out.stdout)["run_id"]
    assert f"run {run_id} was not recorded on the server {team.url}" in out.stderr, out.stderr
    assert "unsynced" in out.stderr
    [post] = posts(team.a)
    assert post["client_run_id"] == run_id
    assert post["state"] == "unsynced"
    assert post["error"]

    # The next command against the server sends it before its own lookup; B then hits it.
    team.start()
    _, err = get(team.a, "base")
    assert f"[barca] recorded run {run_id} on the server" in err, err
    assert [p["state"] for p in posts(team.a) if p["client_run_id"] == run_id] == ["shared"]
    assert any(r["client_run_id"] == run_id for r in team.server.history())
    result_b, _ = get(team.b, "slow")
    assert computed(result_b) == set(), result_b


def test_a_post_with_a_missing_object_is_409_and_retried_by_the_next_command(team):
    out = _kill_server_mid_run(team, team.a)
    assert out.returncode == 0, out.stderr
    result = json.loads(out.stdout)
    run_id = result["run_id"]
    # The uploaded object disappears from the store before the post reaches the server.
    [stored] = list((team.store / "default" / "artifacts").glob("*slow*/*"))
    stored.unlink()

    team.start()
    _, err = get(team.a, "base")
    assert f"run {run_id} is still not recorded on the server {team.url}" in err, err
    assert "the server refused it: pipeline.py:slow: there is no object at" in err, err
    [post] = [p for p in posts(team.a) if p["client_run_id"] == run_id]
    assert post["state"] == "unsynced"
    assert "no object" in post["error"]
    assert not any(r["client_run_id"] == run_id for r in team.server.history())

    # The ledger does not hold `slow`, so the next get computes it again (the object is back)
    # and records that run; the next command after it sends the refused run again, which the
    # server now accepts.
    result, err = get(team.a, "slow")
    assert computed(result) == {"pipeline.py:slow"}, result
    _, err = get(team.a, "base")
    assert f"[barca] recorded run {run_id} on the server" in err, err
    assert all(p["state"] == "shared" for p in posts(team.a)), posts(team.a)


@pytest.mark.skipif(os.geteuid() == 0, reason="root writes into read-only directories")
def test_a_run_held_for_its_uploads_is_posted_by_the_command_that_uploads_them(team):
    # A's uploads fail (#390): the run succeeds here, and its post waits for them (#507).
    artifacts = team.store / "default" / "artifacts"
    artifacts.mkdir(parents=True, exist_ok=True)
    artifacts.chmod(stat.S_IRUSR | stat.S_IXUSR)
    try:
        result_a, err = get(team.a, "report")
        run_id = result_a["run_id"]
        assert result_a["uploads"]["pending"] == 3, result_a
        assert (
            f"run {run_id} is not recorded on the server {team.url} yet: some of its results "
            "are not in the artifact store yet" in err
        ), err
        [post] = posts(team.a)
        assert (post["client_run_id"], post["state"]) == (run_id, "unsynced")
        assert team.server.history() == []
        nodes, _ = _status(team.a, "report")
        assert nodes["pipeline.py:report"]["cache"]["reason"] == "not_in_ledger", nodes
        # Still failing: the next command keeps the post held instead of sending it to a
        # server that would refuse it. The ledger has no result, so it computes them again,
        # and that run is held too.
        again, err = get(team.a, "report")
        assert "still not recorded" not in err, err
        assert "recorded run" not in err, err
        assert [p["state"] for p in posts(team.a)] == ["unsynced", "unsynced"]
    finally:
        artifacts.chmod(stat.S_IRWXU)

    # The store is fixed: A's next command uploads the results, then posts the held run before
    # deciding its own steps, which are hits.
    result_a, err = get(team.a, "report")
    assert "[barca] uploaded 3 pending results" in err, err
    assert f"[barca] recorded run {run_id} on the server" in err, err
    assert f"[barca] recorded run {again['run_id']} on the server" in err, err
    assert computed(result_a) == set(), result_a
    assert [p["state"] for p in posts(team.a)] == ["shared", "shared"]
    assert {r["client_run_id"] for r in team.server.history()} == {run_id, again["run_id"]}

    # B gets A's results from the ledger and fetches them instead of computing.
    result_b, _ = get(team.b, "report")
    assert computed(result_b) == set(), result_b
    assert result_b["final_output"] == {"n": 20}
    [copy] = local_copies(team.b, "report")
    assert json.loads(copy.read_text()) == {"n": 20}


@pytest.mark.skipif(os.geteuid() == 0, reason="root writes into read-only directories")
def test_a_held_manual_refresh_is_posted_without_the_nondeterminism_warning(team):
    # The ledger holds A's first `pinned` (a `Manual` asset) and `noisy` (an ordinary asset).
    get(team.a, "pinned", "--refresh", "pinned")
    get(team.a, "noisy")
    # Both are refreshed while the store refuses uploads: new bytes under the same run hashes,
    # and each run's post is held for its uploads (#507).
    artifacts = team.store / "default" / "artifacts"
    folders = [artifacts, *(p for p in artifacts.iterdir() if p.is_dir())]
    for folder in folders:
        folder.chmod(stat.S_IRUSR | stat.S_IXUSR)
    try:
        held = [get(team.a, name, "--refresh", name)[0]["run_id"] for name in ("pinned", "noisy")]
    finally:
        for folder in folders:
            folder.chmod(stat.S_IRWXU)
    assert [p["state"] for p in posts(team.a)] == ["shared", "shared", "unsynced", "unsynced"]

    # The next command uploads them and sends the held posts; the server answers that it held
    # both with other bytes. A hand-refreshed `Manual` asset is refreshed to get new bytes, so
    # only `noisy` gets the warning, as when a post is sent at the end of its run (#570).
    _, err = get(team.a, "base")
    for run_id in held:
        assert f"[barca] recorded run {run_id} on the server" in err, err
    assert (
        "[barca] warning: pipeline.py:noisy produced different bytes for the same code and "
        "inputs; the function is not deterministic"
    ) in err, err
    assert "pipeline.py:pinned produced different bytes" not in err, err


@pytest.mark.skipif(os.geteuid() == 0, reason="root writes into read-only directories")
def test_a_dropped_pending_upload_is_taken_out_of_its_held_post(team):
    artifacts = team.store / "default" / "artifacts"
    artifacts.mkdir(parents=True, exist_ok=True)
    artifacts.chmod(stat.S_IRUSR | stat.S_IXUSR)
    try:
        result_a, _ = get(team.a, "report")
    finally:
        artifacts.chmod(stat.S_IRWXU)
    run_id = result_a["run_id"]
    # `report`'s local copy goes away: its pending upload is dropped, and the rest of the run
    # is posted without it, so the server does not refuse the whole post.
    [copy] = local_copies(team.a, "report")
    copy.unlink()
    _, err = get(team.a, "middle")
    assert "dropped the pending upload of pipeline.py:report" in err, err
    assert f"[barca] recorded run {run_id} on the server" in err, err
    [post] = posts(team.a)
    assert post["state"] == "shared"
    assert sorted(s["node_id"] for s in json.loads(post["body"])["steps"]) == NODES[:2]
    result_b, _ = get(team.b, "report")
    assert computed(result_b) == {"pipeline.py:report"}, result_b


def test_a_server_that_cannot_be_reached_stops_the_command_with_exit_3(team):
    team.server.stop()
    out = barca(team.a, "get", "base", "--json")
    assert out.returncode == 3, out.stderr
    envelope = json.loads(out.stderr.strip().splitlines()[-1])
    assert envelope["kind"] == "infra"
    assert team.url in envelope["error"]
    assert "--local" in envelope["error"] + (envelope.get("remediation") or "")
    assert out.stdout == ""
    assert not (team.a / ".barca" / "metadata.db").exists() or posts(team.a) == []


def test_a_different_store_or_none_is_a_usage_error_naming_both(team, tmp_path):
    other = tmp_path / "other-store"
    (team.a / "barca.toml").write_text(toml(other, team.url))
    out = barca(team.a, "get", "base")
    assert out.returncode == 2, out.stderr
    assert str(team.store) in out.stderr and str(other) in out.stderr, out.stderr

    (team.a / "barca.toml").write_text(f'[server]\nurl = "{team.url}"\n')
    out = barca(team.a, "get", "base")
    assert out.returncode == 2, out.stderr
    assert "[remote]" in out.stderr, out.stderr
    assert team.server.history() == []


def test_local_touches_nothing_on_the_server(team):
    out = barca(team.a, "get", "report", "--local", "--json")
    assert out.returncode == 0, out.stderr
    assert "[barca] server" not in out.stderr
    assert team.server.history() == []
    assert posts(team.a) == []
    # Nothing reaches the server even when it is down.
    team.server.stop()
    out = barca(team.a, "get", "report", "--local", "--json")
    assert out.returncode == 0, out.stderr
    assert "[barca] server" not in out.stderr
    out = barca(team.a, "status", "report", "--local", "--json")
    assert out.returncode == 0, out.stderr
    nodes = {n["id"]: n for n in json.loads(out.stdout)["nodes"]}
    assert nodes["pipeline.py:report"]["cache"]["state"] == "cached"
    assert "local" not in nodes["pipeline.py:report"]


def test_a_client_backs_up_nothing_the_server_is_the_ledger(team):
    """A machine with a server configured takes no metadata backups, with or without --local:
    its copies landed in the server's backup set (`<store>/<env>/state/backups`), rotated the
    server's own out and were candidates for a restore of the server."""
    backups = team.store / "default" / "state" / "backups"
    for args in (("report",), ("report", "--refresh-all"), ("report", "--local")):
        out = barca(team.a, "get", *args, "--json")
        assert out.returncode == 0, out.stderr
        assert "backed up the metadata DB" not in out.stderr, out.stderr
    _, err = get(team.b, "report")
    assert "backed up the metadata DB" not in err, err
    for root in (team.a, team.b):
        assert not (root / ".barca" / "backups").exists()
    sidecars = [json.loads(p.read_text()) for p in backups.glob("*.json")]
    clients = {str(team.a.resolve()), str(team.b.resolve())}
    assert not [s for s in sidecars if s.get("project") in clients], sidecars


def _status(root: Path, *args: str) -> tuple[dict, str]:
    out = barca(root, "status", *args, "--json")
    assert out.returncode == 0, out.stderr
    body = json.loads(out.stdout)
    return {n["id"]: n for n in body["nodes"]} | {"summary": body["summary"]}, out.stderr


def test_status_shows_the_ledger_and_what_is_here(team):
    get(team.a, "report")
    nodes, err = _status(team.b, "report")
    assert err.splitlines()[0] == f"[barca] server {team.url} ([server] url in barca.toml)"
    assert err.count("[barca] server ") == 1
    assert nodes["pipeline.py:report"]["cache"]["state"] == "cached"
    assert nodes["pipeline.py:report"]["local"] is False
    # A has the copies it computed; B gets them once it fetches them.
    nodes, _ = _status(team.a, "report")
    assert all(nodes[n]["local"] is True for n in NODES), nodes
    get(team.b, "report")
    nodes, _ = _status(team.b, "report")
    assert nodes["pipeline.py:report"]["local"] is True
    # --local is this machine's own state, without `local`.
    nodes, err = _status(team.b, "report", "--local")
    assert "[barca] server" not in err
    assert "local" not in nodes["pipeline.py:report"]


def test_status_shows_a_result_whose_post_is_pending_as_unsynced(team):
    out = _kill_server_mid_run(team, team.a)
    assert out.returncode == 0, out.stderr
    team.start()
    nodes, _ = _status(team.a, "slow")
    slow = nodes["pipeline.py:slow"]
    assert slow["cache"]["state"] == "unsynced", slow
    assert slow["cache"]["reason"] == "not_in_ledger"
    assert slow["local"] is True
    assert nodes["summary"]["unsynced"] == 1
    # The table says it in words.
    table = barca(team.a, "status", "slow", "--pretty")
    assert table.returncode == 0, table.stderr
    assert "not in ledger" in table.stdout
    assert "1 unsynced" in table.stdout
    # B sees the server alone: no result, nothing here.
    nodes, _ = _status(team.b, "slow")
    assert nodes["pipeline.py:slow"]["cache"]["state"] == "never_run"
    assert nodes["pipeline.py:slow"]["local"] is False
    # Once the post goes through, the ledger has it.
    get(team.a, "base")
    nodes, _ = _status(team.a, "slow")
    assert nodes["pipeline.py:slow"]["cache"]["state"] == "cached"
    assert nodes["summary"]["unsynced"] == 0


def test_history_and_stats_answer_from_the_server(team):
    result_a, _ = get(team.a, "report")
    out = barca(team.b, "history", "--json")
    assert out.returncode == 0, out.stderr
    assert f"[barca] server {team.url}" in out.stderr
    runs = json.loads(out.stdout)["runs"]
    assert [r["client_run_id"] for r in runs] == [result_a["run_id"]]
    # B's own history is empty.
    out = barca(team.b, "history", "--local", "--json")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["runs"] == []

    out = barca(team.b, "stats", "report", "--json")
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout)["total_runs"] == 1


def test_status_with_the_server_down_is_exit_3_naming_local(team):
    team.server.stop()
    out = barca(team.a, "status", "--json")
    assert out.returncode == 3, out.stderr
    envelope = json.loads(out.stderr.strip().splitlines()[-1])
    assert team.url in envelope["error"]
    assert "--local" in envelope["error"] + (envelope.get("remediation") or "")
