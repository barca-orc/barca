"""The project socket (#1056 track A): a `barca` command run in the project a `barca serve` is
running in finds the server by `.barca/serve.sock` and goes to it, instead of opening the
metadata database beside it and waiting on its lock (#1054).

Every test runs a real `barca serve` and real commands in its project directory.
"""

import json
import os
import socket
import stat
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from .helpers import barca, wait_until

PIPELINE = """
import time
from pathlib import Path

from barca import asset, task


@asset()
def base() -> dict:
    return {"n": 1}


@asset(inputs={"base": base})
def total(base: dict) -> dict:
    return {"n": base["n"] + 1}


@task()
def hold() -> None:
    Path("hold.started").write_text("started")
    deadline = time.time() + 120
    while not Path("release").exists() and time.time() < deadline:
        print("still holding", flush=True)
        time.sleep(0.02)
"""

ANNOUNCED = "[barca] server .barca/serve.sock (this project's barca serve"


def sock(root: Path, env: str | None = None) -> Path:
    base = root / ".barca" if env is None else root / ".barca" / "envs" / env
    return base / "serve.sock"


def history(root: Path, *args: str) -> list[dict]:
    done = barca(root, "history", "--json", "--all", *args)
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout)["runs"]


def test_serve_opens_an_owner_only_socket_and_removes_it_when_it_stops(project, serve):
    server = serve("--no-schedule")
    path = sock(project)
    mode = path.lstat().st_mode
    assert stat.S_ISSOCK(mode)
    assert stat.S_IMODE(mode) == 0o600
    assert "[barca] project socket .barca/serve.sock" in server.stderr
    # Nothing of the private directory it was bound in is left beside it.
    assert not [p.name for p in (project / ".barca").iterdir() if p.name.startswith(".serve-")]

    # The same API answers on it, with no TCP involved.
    with socket.socket(socket.AF_UNIX) as client:
        client.connect(str(path))
        client.sendall(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        answer = b""
        while chunk := client.recv(65536):
            answer += chunk
    assert answer.startswith(b"HTTP/1.1 200"), answer[:200]
    assert json.loads(answer.split(b"\r\n\r\n", 1)[1])["status"] == "ok"

    assert server.stop() == 0
    assert not path.exists()


def test_a_killed_servers_socket_is_ignored_and_replaced(project, serve):
    server = serve("--no-schedule")
    server.kill()
    path = sock(project)
    assert path.exists(), "a killed server cannot remove its socket"

    # Nothing listens: the command runs on this machine and says nothing about a server.
    done = barca(project, "get", "total", "--json")
    assert done.returncode == 0, done.stderr
    assert "serve.sock" not in done.stderr
    assert json.loads(done.stdout)["final_output"] is not None
    assert [r["triggered_by"] for r in history(project)] == ["manual"]

    # The next server takes the socket over.
    again = serve("--no-schedule")
    done = barca(project, "status", "--json")
    assert done.returncode == 0, done.stderr
    assert ANNOUNCED in done.stderr
    assert again.stop() == 0
    assert not path.exists()


def test_every_routed_command_reaches_the_server(project, serve):
    server = serve("--no-schedule")

    def sent(*args: str, code: int = 0):
        done = barca(project, *args)
        assert done.returncode == code, f"{args}: {done.stderr}"
        assert done.stderr.count(ANNOUNCED) == 1, f"{args}: {done.stderr}"
        return done

    # `get` and `run` join the server's queue: the server records them as its own.
    got = json.loads(sent("get", "total", "--json").stdout)
    assert got["status"] == "success"
    assert Path(project, got["final_output"]["_barca_artifact"]["path"]).is_file()
    sent("run", "hold", "--dry-run")
    assert server.get(f"/runs/{got['run_id']}")["status"] == "success"
    assert [r["triggered_by"] for r in history(project, "--local")] == ["api"]

    assert json.loads(sent("status", "--json").stdout)["summary"]["cached"] == 2
    assert json.loads(sent("history", "--json").stdout)["runs"][0]["run_id"] == got["run_id"]
    assert json.loads(sent("stats", "total", "--json").stdout)["id"] == "pipeline.py:total"
    sent("logs", got["run_id"])
    assert json.loads(sent("queue", "--json").stdout)["queued"] == []
    rows = json.loads(sent("sql", "select id from barca.nodes order by id", "--json").stdout)
    assert [r["id"] for r in rows["rows"]] == [
        "pipeline.py:base",
        "pipeline.py:hold",
        "pipeline.py:total",
    ]
    # A query over results runs here, over the artifacts the server wrote in this project.
    rows = json.loads(sent("sql", "select n from total", "--json").stdout)
    assert rows["rows"] == [{"n": 2}]

    assert json.loads(sent("pause", "total", "--json").stdout)["added"] == ["pipeline.py:total"]
    assert server.get("/pauses")["total"] == 1
    assert json.loads(sent("resume", "total", "--json").stdout)["removed"] == ["pipeline.py:total"]
    versions = json.loads(sent("pin", "total", "--versions", "--json").stdout)
    run_hash = versions["versions"][0]["run_hash"]
    sent("pin", "total", "--run-hash", run_hash, "--json")
    assert [p["node_id"] for p in server.get("/pins")["pins"]] == ["pipeline.py:total"]
    sent("unpin", "total", "--json")
    assert server.get("/pins")["pins"] == []
    assert "not found" in sent("cancel", "no-such-run", code=2).stderr
    assert json.loads(sent("gc", "--dry-run", "--json").stdout)["dry_run"] is True
    assert json.loads(sent("gc", "--json").stdout)["dry_run"] is False
    assert "[barca] gc on request" in server.stderr


def test_local_runs_on_this_machine_beside_the_server(project, serve):
    serve("--no-schedule")
    done = barca(project, "get", "total", "--json", "--local")
    assert done.returncode == 0, done.stderr
    assert "serve.sock" not in done.stderr
    assert [r["triggered_by"] for r in history(project, "--local")] == ["manual"]
    for args in (("status", "--json"), ("gc", "--dry-run", "--json"), ("history", "--json")):
        done = barca(project, *args, "--local")
        assert done.returncode == 0, done.stderr
        assert "serve.sock" not in done.stderr
    # The queue, pauses and cancelling are a server's: --local has nothing to answer with.
    assert barca(project, "queue", "--local").returncode == 2


def test_gc_beside_a_server_with_a_run_recording_is_run_by_the_server(project, serve):
    server = serve("--no-schedule")
    run = server.submit("hold")
    wait_until(lambda: (project / "hold.started").exists(), "the run to start")
    began = time.monotonic()
    done = barca(project, "gc", "--json", timeout=30)
    took = time.monotonic() - began
    (project / "release").write_text("go")
    assert done.returncode == 0, done.stderr
    assert done.stderr.count(ANNOUNCED) == 1
    result = json.loads(done.stdout)
    assert result["dry_run"] is False
    # The server knows what its own run holds, so nothing is held back wholesale (a second
    # process beside a run considers only leftovers and superseded results, and says so).
    assert result["restricted"] is None
    assert took < 20, f"gc took {took:.1f}s beside a running server"
    assert server.finished(run["run_id"])["status"] == "success"


def test_restore_and_reset_refuse_while_the_server_is_up(project, serve):
    server = serve("--no-schedule")
    assert barca(project, "get", "total").returncode == 0
    for args, fix in (
        (("state", "reset", "--yes"), "POST /state/reset"),
        (("state", "restore"), "Stop the server"),
    ):
        done = barca(project, *args)
        assert done.returncode == 3, done.stderr
        error = json.loads(done.stderr.strip().splitlines()[-1])
        assert "barca serve is using this environment" in error["error"]
        assert f"pid {server.proc.pid}" in error["error"]
        assert "Nothing was changed" in error["error"]
        assert fix in error["remediation"]
    assert [r["status"] for r in history(project)] == ["success"]


def test_the_socket_needs_no_token_when_tokens_are_on(project, serve):
    (project / "barca.toml").write_text('[server]\nauth = "token"\n')
    server = serve("--no-schedule", wait="bind")
    with pytest.raises(urllib.error.HTTPError) as refused:
        urllib.request.urlopen(f"{server.url}/health", timeout=10)
    assert refused.value.code == 401

    done = barca(project, "get", "total", "--json")
    assert done.returncode == 0, done.stderr
    assert done.stderr.count(ANNOUNCED) == 1
    assert json.loads(done.stdout)["status"] == "success"
    # The same command sent to the TCP address has no token to show.
    done = barca(project, "status", "--server", server.url)
    assert done.returncode != 0
    assert "authentication" in done.stderr


def test_a_read_only_server_answers_reads_and_refuses_writes(project, serve):
    assert barca(project, "get", "total").returncode == 0
    serve("--read-only")
    done = barca(project, "status", "--json")
    assert done.returncode == 0, done.stderr
    assert done.stderr.count(ANNOUNCED) == 1
    assert json.loads(done.stdout)["summary"]["cached"] == 2

    for args in (("get", "total"), ("gc",), ("pause", "total")):
        done = barca(project, *args)
        assert done.returncode == 2, f"{args}: {done.stderr}"
        assert "this server is read-only" in done.stderr
    done = barca(project, "get", "total", "--json", "--local")
    assert done.returncode == 0, done.stderr


def test_a_named_environment_has_its_own_socket(project, serve):
    serve("--no-schedule", "--env", "dev")
    assert stat.S_ISSOCK(sock(project, "dev").lstat().st_mode)
    assert not sock(project).exists()

    done = barca(project, "get", "total", "--json", "--env", "dev")
    assert done.returncode == 0, done.stderr
    assert "[barca] server .barca/envs/dev/serve.sock" in done.stderr
    # BARCA_ENV names it too.
    done = barca(project, "status", "--json", env={**os.environ, "BARCA_ENV": "dev"})
    assert done.returncode == 0, done.stderr
    assert "[barca] server .barca/envs/dev/serve.sock" in done.stderr
    # Another environment has no server: its commands run here.
    done = barca(project, "get", "total", "--json", "--env", "other")
    assert done.returncode == 0, done.stderr
    assert "serve.sock" not in done.stderr


def test_a_command_that_names_no_file_uses_the_servers_deployed_files(project, serve):
    (project / "other.py").write_text(
        "from barca import asset\n\n@asset()\ndef elsewhere() -> int:\n    return 1\n"
    )
    serve("--no-schedule")  # serves pipeline.py only
    done = barca(project, "get", "total", "--json")
    assert done.returncode == 0, done.stderr
    assert done.stderr.count(ANNOUNCED) == 1
    # A file the server has not deployed is refused, naming what it has.
    done = barca(project, "get", "elsewhere", "other.py")
    assert done.returncode == 2
    assert "is not one of the files deployed" in done.stderr
    assert barca(project, "get", "elsewhere", "other.py", "--local").returncode == 0


def test_a_command_in_a_subdirectory_finds_the_projects_server(project, serve):
    (project / "barca.toml").write_text("")
    (project / "sub").mkdir()
    serve("--no-schedule")
    done = barca(project / "sub", "get", "total", "--json")
    assert done.returncode == 0, done.stderr
    assert done.stderr.count(ANNOUNCED) == 1
