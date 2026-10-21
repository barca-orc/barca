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
import shutil
import signal
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.request
from collections import Counter
from collections.abc import Iterator
from pathlib import Path

from barca.api import _find_binary

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
        assert not listing["truncated"], "more runs than one page: count them another way"
        counts: Counter[str] = Counter()
        for run in listing["runs"]:
            run_id = run["run_id"]
            steps = self.finished.get(run_id)
            if steps is None:
                view = get(self.port, f"/runs/{run_id}")
                steps = [(s["node_id"].rsplit(":", 1)[1], s["status"]) for s in view["steps"]]
                if view["status"] not in LIVE:
                    self.finished[run_id] = steps
            counts.update(node for node, s in steps if s == status)
        return dict(counts)


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Serve:
    """`barca serve pipeline.py` on a free port in `root`, stopped on exit.

    Its output goes to files beside the project, not to pipes nobody reads until the end (a
    full pipe would block the server), and is printed when the block fails or the server
    exits early, so a failure says what the server was doing.
    """

    def __init__(self, root: Path, *args: str, env: dict[str, str]):
        self.root = root
        self.port = _free_port()
        self.out_path = root.parent / f"{root.name}.serve-{self.port}.out"
        self.err_path = root.parent / f"{root.name}.serve-{self.port}.err"
        with self.out_path.open("w") as out, self.err_path.open("w") as err:
            self.proc = subprocess.Popen(
                [_find_binary(), "serve", "pipeline.py", "--port", str(self.port), *args],
                cwd=root,
                env=env,
                stdout=out,
                stderr=err,
                text=True,
            )
        self.err = ""
        self.rows = Materializations(self.port)

    def __enter__(self) -> "Serve":
        return self

    def __exit__(self, exc_type, *_exc) -> None:
        self.stop()
        if exc_type is not None:
            print(self.output())

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.err = self.err_path.read_text()

    def output(self) -> str:
        return (
            f"--- barca serve (exit {self.proc.poll()}) stderr ---\n{self.err_path.read_text()}"
            f"--- stdout ---\n{self.out_path.read_text()}"
        )

    def get(self, path: str):
        return get(self.port, path)

    def post(self, path: str, body: dict):
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=10) as r:
            return json.loads(r.read())

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
            time.sleep(0.2)

    def count(self, status: str = "success") -> dict[str, int]:
        """Materializations per node with `status`, waiting for the server to answer."""
        return self.wait("GET /runs", lambda: (self.rows(status),))[0]

    def settle(self, seconds: float = 3) -> dict[str, int]:
        """Let several ticks pass and return the successful materializations after them."""
        time.sleep(seconds)
        return self.count()

    def ready(self) -> None:
        self.wait("GET /health", lambda: self.get("/health"))

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
