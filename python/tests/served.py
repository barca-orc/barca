"""Reading what a live `barca serve` (or any running barca) has recorded, without opening
its metadata DB.

A test must not open `.barca/metadata.db` with `sqlite3` while barca can write it. Opening
the path creates it, so a read that lands after barca made `.barca/` but before it made the
database leaves a 0-byte file, and the server then refuses to start on it (exit 3) (#537).
A connection to the live Turso file can also see a stale snapshot. So a test waits on the
server's run contract (`GET /runs`, `GET /runs/{id}`), and a test that needs a table reads a
copy taken the way barca's own readers take one (`snapshot`).
"""

import contextlib
import fcntl
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from collections import Counter
from collections.abc import Iterator, Sequence
from email.message import Message
from pathlib import Path
from typing import Any

from barca.api import _find_binary

from .helpers import clean_env, free_port

LIVE = ("pending", "running")


def get(port: int, path: str):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=10) as r:
        return json.loads(r.read())


class Materializations:
    """Materializations per node and status, as the server reports them.

    `GET /runs/{id}` lists a run's `steps` from the `materializations` table (one entry per
    row, with its `status`), so summing them over `GET /runs` counts what
    `select node_id, status, count(*) from materializations` would. A finished run cannot
    change, so its steps are fetched once.
    """

    def __init__(self, port: int):
        self.port = port
        self.finished: dict[str, list[tuple[str, str]]] = {}

    def __call__(self, status: str = "success") -> dict[str, int]:
        listing = get(self.port, "/runs?limit=1000")
        # WAL readers can observe a newly committed run in the total after the
        # list was read. That transient truncation is not a full page: the next
        # poll sees the new run. Only exceeding the requested limit loses history.
        assert listing["total"] <= 1000, "more runs than one page: count them another way"
        counts: Counter[str] = Counter()
        for run in listing["runs"]:
            run_id = run["run_id"]
            steps = self.finished.get(run_id)
            if steps is None:
                view = get(self.port, f"/runs/{run_id}?all=true")
                steps = [(s["node_id"].rsplit(":", 1)[1], s["status"]) for s in view["steps"]]
                if view["status"] not in LIVE:
                    self.finished[run_id] = steps
            counts.update(node for node, s in steps if s == status)
        return dict(counts)


def _free_port() -> int:
    return free_port()


class Serve:
    """`barca serve <files> --port <port> <args>` in `root`, stopped on exit (or by `stop`).

    The one way a test starts a server (the `serve` fixture in `conftest.py` builds on it).
    Its output goes to files beside the project, not to pipes nobody reads until the end (a
    full pipe would block the server), and is printed when the block fails or the server
    exits early, so a failure says what the server was doing.

    `env=None` is `helpers.clean_env()`; pass `{**os.environ, ...}` to inherit the machine's
    configuration on purpose. `port=None` picks a free one; a test that restarts a server on
    the same port passes the old `.port`. `files` are the pipeline arguments (`"."` for the
    whole project). `new_session=True` gives the server its own process group, for a test
    that signals the group as a terminal would (`os.killpg`). `wait="health"` (the default)
    returns once `GET /health` answers; `wait="bind"` once stderr says `serving on` (a
    server that must not bind yet is checked with `wait=None` and `bound()`); `wait=None`
    returns at once.
    """

    def __init__(
        self,
        root: Path,
        *args: str,
        env: dict[str, str] | None = None,
        files: Sequence[str] = ("pipeline.py",),
        port: int | None = None,
        new_session: bool = False,
        wait: str | None = "health",
        timeout: float = 60,
    ):
        self.root = root
        self.new_session = new_session
        self.port = free_port() if port is None else port
        self.url = f"http://127.0.0.1:{self.port}"
        self.out_path = root.parent / f"{root.name}.serve-{self.port}.out"
        self.err_path = root.parent / f"{root.name}.serve-{self.port}.err"
        # Append: a server restarted on the same port keeps the previous one's lines.
        self.start_at = self.err_path.stat().st_size if self.err_path.exists() else 0
        with self.out_path.open("a") as out, self.err_path.open("a") as err:
            self.proc = subprocess.Popen(
                [_find_binary(), "serve", *files, "--port", str(self.port), *args],
                cwd=root,
                env=clean_env() if env is None else env,
                stdout=out,
                stderr=err,
                text=True,
                start_new_session=new_session,
            )
        self.err = ""
        self.rows = Materializations(self.port)
        try:
            if wait == "health":
                self.ready(timeout)
            elif wait == "bind":
                self.wait("the server to bind", self.bound, timeout)
            elif wait is not None:
                raise ValueError(f"wait={wait!r}: 'health', 'bind' or None")
        except BaseException:
            self.stop()
            raise

    def __enter__(self) -> "Serve":
        return self

    def __exit__(self, exc_type, *_exc) -> None:
        self.stop()
        if exc_type is not None:
            print(self.output())

    def stop(self, sig: int = signal.SIGTERM, timeout: float = 30) -> int:
        """Send `sig` (SIGTERM by default) unless the server has exited, wait up to
        `timeout` seconds and then kill it; the exit code."""
        if self.proc.poll() is None:
            self.proc.send_signal(sig)
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.err = self.stderr
        return self.proc.returncode

    def kill(self) -> int:
        """SIGKILL and reap; the exit code."""
        return self.stop(signal.SIGKILL)

    def send(self, sig: int) -> None:
        """Send `sig` to the server (`new_session=True`: to its whole process group)."""
        if self.proc.poll() is not None:
            return
        if self.new_session:
            os.killpg(self.proc.pid, sig)
        else:
            self.proc.send_signal(sig)

    def exited(self, timeout: float = 60) -> int:
        """Wait for the server to exit on its own (after a signal already sent); the code."""
        code = self.proc.wait(timeout=timeout)
        self.err = self.stderr
        return code

    @property
    def stderr(self) -> str:
        """What this server printed on stderr so far."""
        return self.err_path.read_text()[self.start_at :]

    @property
    def stdout(self) -> str:
        return self.out_path.read_text()

    def output(self) -> str:
        return (
            f"--- barca serve (exit {self.proc.poll()}) stderr ---\n{self.stderr}"
            f"--- stdout ---\n{self.stdout}"
        )

    def bound(self) -> bool:
        """Whether the server has reported `serving on` (it binds after DB admission)."""
        return "serving on" in self.stderr

    def get(self, path: str):
        return get(self.port, path)

    def post(self, path: str, body: dict):
        request = urllib.request.Request(
            f"{self.url}{path}",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=10) as r:
            return json.loads(r.read())

    def request(
        self, method: str, path: str, body: dict | None = None, headers: dict | None = None
    ) -> tuple[int, Any]:
        """One request: the HTTP status and the decoded JSON body, error responses included.
        A server that is not listening raises `OSError`."""
        status, _, decoded = self.exchange(method, path, body, headers)
        return status, decoded

    def exchange(
        self, method: str, path: str, body: dict | None = None, headers: dict | None = None
    ) -> tuple[int, Message, Any]:
        """[`request`], with the response headers."""
        data = None if body is None else json.dumps(body).encode()
        sent = {"Content-Type": "application/json"} if data else {}
        sent.update(headers or {})
        req = urllib.request.Request(f"{self.url}{path}", data=data, method=method, headers=sent)
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                raw = resp.read()
                return resp.status, resp.headers, json.loads(raw) if raw else None
        except urllib.error.HTTPError as e:
            raw = e.read()
            return e.code, e.headers, json.loads(raw) if raw else None

    def submit(self, target: str, command: str = "run") -> dict:
        """`POST /runs` for one target; the accepted body."""
        status, body = self.request("POST", "/runs", {"command": command, "targets": [target]})
        assert status == 202, body
        return body

    def run(self, run_id: str) -> dict:
        return self.request("GET", f"/runs/{run_id}")[1]

    def wait(self, what: str, check, timeout: float = 60):
        """Poll `check` until it returns something truthy. A server that is not answering
        yet is waited for; a server that exited fails at once, with its output."""
        deadline = time.monotonic() + timeout
        last = None
        while True:
            if self.proc.poll() is not None:
                raise AssertionError(f"barca serve exited waiting for {what}\n{self.output()}")
            try:
                value = check()
            except OSError as e:  # not listening yet, or a request timed out
                value, last = None, e
            if value:
                return value
            if time.monotonic() > deadline:
                raise AssertionError(f"timed out waiting for {what} (last error: {last})")
            time.sleep(0.05)

    def count(self, status: str = "success") -> dict[str, int]:
        """Materializations per node with `status`, waiting for the server to answer."""
        return self.wait("GET /runs", lambda: (self.rows(status),))[0]

    def settle(self, seconds: float = 3) -> dict[str, int]:
        """Let several ticks pass and return the successful materializations after them."""
        time.sleep(seconds)
        return self.count()

    def ready(self, timeout: float = 60) -> None:
        self.wait("GET /health", lambda: self.get("/health"), timeout)

    def schedule(self) -> dict:
        """The published jobs by node name."""
        jobs = self.wait("GET /schedule", lambda: self.get("/schedule"))
        return {job["id"].rsplit(":", 1)[1]: job for job in jobs}

    def finished(self, run_id: str) -> dict:
        """`GET /runs/{id}` once the run has ended."""
        return self.wait(
            f"run {run_id} to end",
            lambda: (r := self.get(f"/runs/{run_id}"))["status"] not in LIVE and r,
        )


def rows(root: Path, status: str = "success") -> dict[str, int]:
    """Materializations per node with `status`, from a snapshot; empty with no database."""
    with snapshot(root) as db:
        if db is None:
            return {}
        return {
            node.rsplit(":", 1)[1]: count
            for node, count in db.execute(
                "select node_id, count(*) from materializations where status = ? group by 1",
                (status,),
            )
        }


@contextlib.contextmanager
def snapshot(root: Path) -> Iterator[sqlite3.Connection | None]:
    """A connection to a private copy of `root`'s metadata DB, or `None` while there is none.

    The copy is taken as `DbSnapshot::take` takes it: the main file and its WAL, under
    barca's cross-process lock, which is released before the copy is read. The live file is
    never opened by sqlite3.
    """
    source = root / ".barca" / "metadata.db"
    lock = Path(f"{source}.lock")
    if not source.exists() or not lock.exists():
        yield None
        return
    with tempfile.TemporaryDirectory(prefix="barca-test-snapshot-") as directory:
        copy = Path(directory) / "metadata.db"
        with lock.open("rb") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            shutil.copyfile(source, copy)
            wal = Path(f"{source}-wal")
            if wal.exists():
                shutil.copyfile(wal, Path(f"{copy}-wal"))
        db = sqlite3.connect(copy)
        try:
            yield db
        finally:
            db.close()
