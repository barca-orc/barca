#!/usr/bin/env python3
"""A real `barca serve` as the long-lived holder of the metadata connection (#1056, A7 and A8).

`sim.py` beside this searches for rare bugs with random faults. This one holds a server up and
measures it: one process that keeps its write connection to `.barca/metadata.db` for the whole
period while

  - two schedules fire every second and API runs record steps, logs and outcomes,
  - it backs the database up to a store (a local directory) every few seconds, each backup a
    checkpoint, a hash and a copy of the pinned main file,
  - it checks the cache size every 2 s under a small `[cache] max_size`, so eviction and the
    metadata trim (with its `VACUUM`) really run, and takes `POST /gc`,
  - its read routes are polled through all of it, and
  - other processes come and go: `barca get --local`, `status` / `history` / `stats` / `logs`
    (and `logs -f`) readers, `barca gc --local`, `barca state backups`, `barca doctor`, and
    commands sent to it over `.barca/serve.sock`.

Then the stop and crash paths: SIGTERM during a run (the drain), a second SIGTERM, SIGKILL
mid-write with a restart onto what it left (the CLI loops keep going across each), a clean stop
with nothing else running (what is left on disk), and `barca state restore` with the server
down and CLI processes active.

    tests/simulation/owner_serve.py --seconds 600                    # the long run
    tests/simulation/owner_serve.py --seconds 20 --kills 1 --seed 1  # what CI's full tier runs
    tests/simulation/owner_serve.py --seconds 600 --keep /var/tmp/owner --report report.json

The seed fixes each loop's choices, not the timing between processes.

What fails the run (exit 1), each named in the report:
  errors     `database is locked`, `malformed`, `disk image`, `not a database`, a lock timeout,
             a checkpoint or backup that failed, a panic, in any command or the server's log;
             a non-zero exit or a 5xx that the phase under way does not explain
  integrity  `PRAGMA integrity_check` on the live database after every kill, stop and restore,
             and on every backup the server took; each backup restored with `barca state
             restore` reads as a history the final one continues
  lost       a run the server accepted, a `--local` run that exited 0 or a tick the server said
             it fired is not in the final history as it ended
  torn       a run listed by `GET /runs` whose detail stays 404, counts that contradict rows
  stops      a drain that loses its run, a forced stop or a kill that leaves a run `running`,
             a server or command that needs a repair afterwards
  latency    the 95th percentile of a read route over 500 ms (#921)

Exit codes: 0 passed, 1 a check failed, 3 the harness itself failed.
"""

from __future__ import annotations

import argparse
import calendar
import fcntl
import hashlib
import json
import os
import random
import re
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from collections import Counter, defaultdict
from contextlib import closing
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

# The binary's constants (crates/barca-db/src/metadata_size.rs, db/ticks.rs).
KEPT_RUNS = 1000
TICKS_PER_SCHEDULE = 1000
# Read routes answer within this while runs write (#921).
READ_BAR_MS = 500
# A command or request this slow is listed in the report with when it ended.
SLOW_MS = 2000
KEYS = 12
# Steps of `get pipeline.py --refresh-all` (base, total, last and the keys) and of
# `get last --refresh-all`.
LOCAL_STEPS = 3 + KEYS
LAST_STEPS = 3

SOURCE = f"""\
import time

from barca import Schedule, asset, partitions, task


@asset()
def base() -> dict:
    return {{"n": 1, "pad": "x" * 20000}}


@asset(inputs={{"base": base}})
def total(base: dict) -> dict:
    return {{"n": base["n"] + 1, "pad": base["pad"]}}


@asset(partitions={{"k": partitions([str(i) for i in range({KEYS})])}})
def keyed(k: str) -> dict:
    print("keyed line", k)
    return {{"k": k, "pad": "y" * 5000}}


@asset(inputs={{"total": total}})
def last(total: dict) -> dict:
    return {{"n": total["n"] + 1}}


@task(freshness=Schedule("* * * * * *"))
def tick() -> None:
    for i in range(10):
        print("tick line", i, "x" * 60)


@task(freshness=Schedule("* * * * * *"))
def tock() -> None:
    print("tock")
    time.sleep(0.3)
"""

# A run that stays in flight for as long as `hold.secs` says: what a stop or a kill catches.
# In a file of its own, which only the server runs.
HOLD = """\
import time
from pathlib import Path

from barca import asset


@asset()
def hold() -> dict:
    flag = Path(__file__).with_name("hold.secs")
    secs = float(flag.read_text()) if flag.exists() else 0.0
    print("holding", secs, flush=True)
    time.sleep(secs)
    return {"held": secs}
"""

# Words no command, route or server log may ever say, and what each means.
NEVER = {
    "database is locked": "SQLite lock surfaced",
    "database table is locked": "SQLite lock surfaced",
    "malformed": "corruption",
    "disk image": "corruption",
    "not a database": "corruption",
    "timed out after": "lock timeout",
    "locked by another": "lock surfaced",
    "wal_checkpoint": "checkpoint failed",
    "refusing to copy a torn": "checkpoint failed",
    "metadata backup failed": "backup failed",
    "was not uploaded": "backup failed",
    "could not trim": "trim failed",
    "cache size check failed": "gc failed",
    "panicked at": "panic",
    'metadata_error": "': "server lost its database",
    "Database error": "database error",
    "failed to open DB": "open failed",
    "cache lookup of": "a read lost to a writer",
}
TERMINAL = ("success", "failed", "error", "cancelled", "interrupted")


class Failed(Exception):
    """A check failed: a fault in barca, or in what this says barca promises."""


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * p))]


class Harness:
    def __init__(self, args: argparse.Namespace, root: Path) -> None:
        self.args = args
        self.root = root
        self.dir = root / "project"
        self.store = root / "store"
        self.kept = root / "backups"
        for d in (self.dir, self.store, self.kept):
            d.mkdir(parents=True)
        (self.dir / "pipeline.py").write_text(SOURCE)
        (self.dir / "held.py").write_text(HOLD)
        (self.dir / "barca.toml").write_text(f'[cache]\nmax_size = "{args.limit_mb}MB"\n')
        self.barca = args.barca
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
        self.env |= {
            "BARCA_POOL_SIZE": "2",
            "RUST_BACKTRACE": "1",
            "BARCA_REMOTE_URI": str(self.store),
            "BARCA_BACKUP_INTERVAL": f"{args.backup_secs}s",
            "BARCA_TEST_CACHE_CHECKPOINT_SECS": "2",
        }
        if args.trace:
            self.env["BARCA_TRACE_TIMING"] = "1"
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.log_path = root / "serve.log"
        self.proc: subprocess.Popen | None = None
        self.started = time.monotonic()
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.pause_writers = threading.Event()
        # Why a command or request may fail right now: "" while the server is up and nothing
        # replaces the database.
        self.phase = ""
        self.phases: list[tuple[float, float, str]] = []
        self.commands: Counter[str] = Counter()
        self.latency: dict[str, list[float]] = defaultdict(list)
        self.problems: list[dict] = []
        self.explained: Counter[str] = Counter()
        self.accepted: dict[str, dict] = {}
        self.local_runs: dict[str, dict] = {}
        self.samples: list[tuple[float, int, int, int]] = []
        self.backups: dict[str, Path] = {}
        self.slow: list[dict] = []
        self.in_flight: dict[object, tuple[str, float]] = {}
        self.stacks = 0
        self.last_stack = 0.0
        self.detail_404_ms: list[float] = []
        self.detail_404_of: Counter[str] = Counter()
        self.stops: list[dict] = []
        self.counts: Counter[str] = Counter()
        self.threads: list[threading.Thread] = []

    db = property(lambda self: self.dir / ".barca/metadata.db")

    def now(self) -> float:
        return round(time.monotonic() - self.started, 3)

    def say(self, text: str) -> None:
        if not self.args.quiet:
            print(f"[{self.now():8.1f}s] {text}", file=sys.stderr, flush=True)

    # ─── recording ────────────────────────────────────────────────────────────

    def problem(self, kind: str, what: str, detail: str, began: float) -> None:
        """Something went wrong at `kind`. Explained by a phase (the server was down or being
        stopped, the database was being replaced) it is counted; otherwise it fails the run."""
        ended = self.now()
        with self.lock:
            why = self.phase or next(
                (name for a, b, name in self.phases if a <= ended and began <= b), ""
            )
            never = [meaning for word, meaning in NEVER.items() if word in detail]
            if why and not never and self.explains(why, kind, detail):
                self.explained[f"{kind}: {what} during {why}"] += 1
                return
            self.problems.append(
                {
                    "at": ended,
                    "kind": kind,
                    "what": what,
                    "class": ", ".join(sorted(set(never))) or "unexplained",
                    "phase": why,
                    "detail": detail[-1500:],
                }
            )

    @staticmethod
    def explains(phase: str, kind: str, detail: str) -> bool:
        """Whether `phase` accounts for a failure of `kind`."""
        if kind.startswith("http") or kind.startswith("socket") or kind == "api run":
            # A server that is down, stopping or just restarted: unreachable, or draining.
            return phase != "restore"
        if phase == "restore":
            # `barca docs backup`: stop `barca get` first. A run recording into a database
            # that is replaced under it may fail; the database must not suffer (checked).
            # A run the backup does not hold is gone from the history, as asked.
            return kind.startswith(("local get", "local logs", "local history RUN"))
        # A `--local` command beside a server being killed has no reason to fail.
        return False

    def enter(self, phase: str) -> float:
        with self.lock:
            self.phase = phase
        return self.now()

    def leave(self, began: float, grace: float = 0.0) -> None:
        with self.lock:
            self.phases.append((began, self.now() + grace, self.phase))
            self.phase = ""

    def took(self, kind: str, ms: float) -> None:
        with self.lock:
            self.commands[kind] += 1
            self.latency[kind].append(ms)
            if ms >= SLOW_MS and not kind.endswith(("get", "logs -f")):
                self.slow.append({"at": self.now(), "kind": kind, "ms": round(ms)})
                # Beside what the server was saying then.
                with self.log_path.open("a") as log:
                    log.write(f"### owner_serve {self.now()}s: {kind} took {ms:.0f} ms\n")

    # ─── the processes ────────────────────────────────────────────────────────

    def cli(self, kind: str, *argv: str, timeout: float = 120, ok=(0,), env: dict | None = None):
        began = self.now()
        start = time.monotonic()
        try:
            out = subprocess.run(
                [self.barca, *argv],
                cwd=self.dir,
                env=env or self.env,
                capture_output=True,
                text=True,
                timeout=timeout,
                check=False,
                stdin=subprocess.DEVNULL,
            )
        except subprocess.TimeoutExpired as e:
            self.problem(kind, f"no answer in {timeout}s", str(e.stderr)[-1500:], began)
            return None
        ms = (time.monotonic() - start) * 1000
        self.took(kind, ms)
        said = [word for word in NEVER if word in out.stderr or word in out.stdout]
        if out.returncode not in ok or said:
            self.problem(
                kind,
                f"exit {out.returncode}" + (f", says {said}" if said else ""),
                out.stderr,
                began,
            )
            return None
        return out

    def http(self, kind: str, method: str, path: str, body: dict | None = None, timeout=30):
        """One request: (status, decoded body), or (0, None) when the server did not answer."""
        began = self.now()
        start = time.monotonic()
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(
            f"{self.url}{path}",
            data=data,
            method=method,
            headers={"Content-Type": "application/json"} if data else {},
        )
        token = object()
        self.in_flight[token] = (kind, start)
        try:
            with urllib.request.urlopen(request, timeout=timeout) as r:
                status, raw = r.status, r.read()
        except urllib.error.HTTPError as e:
            status, raw = e.code, e.read()
        except (OSError, ValueError) as e:
            self.problem(kind, "no answer", repr(e), began)
            return 0, None
        finally:
            del self.in_flight[token]
        ms = (time.monotonic() - start) * 1000
        self.took(kind, ms)
        text = raw.decode(errors="replace")
        said = [word for word in NEVER if word in text]
        if status >= 500 or said:
            self.problem(kind, f"{status}" + (f", says {said}" if said else ""), text, began)
        try:
            return status, json.loads(text) if text else None
        except ValueError:
            return status, None

    def start_server(self) -> None:
        self.proc = subprocess.Popen(
            [
                self.barca,
                "serve",
                "pipeline.py",
                "held.py",
                "--port",
                str(self.port),
                "--timezone",
                "utc",
                "--max-runs",
                "3",
                "--drain-timeout",
                "20",
            ],
            cwd=self.dir,
            stdout=subprocess.DEVNULL,
            stderr=self.log_path.open("a"),
            env=self.env,
            start_new_session=True,
        )
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise Failed(f"the server exited {self.proc.returncode} at startup")
            try:
                with urllib.request.urlopen(f"{self.url}/health", timeout=2) as r:
                    health = json.loads(r.read())
                if health.get("metadata_error"):
                    raise Failed(f"the server started with metadata_error: {health}")
                self.counts["server starts"] += 1
                return
            except (OSError, ValueError):
                time.sleep(0.05)
        raise Failed("the server did not answer /health in 90s")

    def reap_group(self) -> None:
        if self.proc is not None:
            try:
                os.killpg(self.proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def log_text(self) -> str:
        return self.log_path.read_text(errors="replace")

    # ─── the loops ────────────────────────────────────────────────────────────

    def loop(self, name: str, body, seed: int, pause: tuple[float, float]) -> None:
        rng = random.Random(self.args.seed * 1000 + seed)

        def run() -> None:
            while not self.stop.is_set():
                try:
                    body(rng)
                except Exception as e:  # noqa: BLE001 - a loop that dies would hide its traffic
                    self.problem(name, "the harness loop raised", repr(e), self.now())
                self.stop.wait(rng.uniform(*pause))

        thread = threading.Thread(target=run, name=name, daemon=True)
        thread.start()
        self.threads.append(thread)

    def follow(self, run_id: str, expect: int | None = None, loop: bool = True) -> dict | None:
        """Poll `GET /runs/{id}` until the run has ended; what it ended as."""
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline and not (loop and self.stop.is_set()):
            status, body = self.http("http GET /runs/{id}", "GET", f"/runs/{run_id}?all=true")
            if status == 200 and body and body.get("status") in TERMINAL:
                # A step another run of the server computed meanwhile counts as cached.
                done = (body.get("steps_executed") or 0) + (body.get("steps_cached") or 0)
                with self.lock:
                    self.accepted[run_id]["ended"] = body["status"]
                    self.accepted[run_id]["steps"] = done
                if body["status"] == "success" and expect is not None and done != expect:
                    self.problem(
                        "torn",
                        f"run {run_id} is a success with {done} steps, not {expect}",
                        json.dumps(body)[:1500],
                        self.now(),
                    )
                return body
            time.sleep(0.1)
        return None

    def submit(
        self, target: str, expect: int | None, wait: bool = True, loop: bool = True
    ) -> str | None:
        status, body = self.http(
            "http POST /runs",
            "POST",
            "/runs",
            {"command": "get", "targets": [target], "refresh_all": True},
        )
        if status != 202 or not body:
            if status and status < 500:
                self.problem(
                    "api run", f"POST /runs answered {status}", json.dumps(body), self.now()
                )
            return None
        run_id = body["run_id"]
        with self.lock:
            self.accepted[run_id] = {
                "at": self.now(),
                "wall": time.time(),
                "via": "api",
                "expect": expect,
            }
        if wait:
            self.follow(run_id, expect, loop)
        return run_id

    def api_runs(self, rng: random.Random) -> None:
        if self.pause_writers.is_set():
            return
        run_id = self.submit("last", LAST_STEPS, wait=rng.random() < 0.7)
        if run_id and rng.random() < 0.2:
            # The run's event stream, to its end.
            began = self.now()
            try:
                with urllib.request.urlopen(f"{self.url}/runs/{run_id}/events", timeout=60) as r:
                    r.read()
                self.commands["http GET /runs/{id}/events"] += 1
            except (OSError, ValueError) as e:
                self.problem("http GET /runs/{id}/events", "stream broke", repr(e), began)
            self.follow(run_id, LAST_STEPS)

    def local_gets(self, rng: random.Random) -> None:
        if self.pause_writers.is_set():
            return
        whole = rng.random() < 0.6
        argv = ("pipeline.py",) if whole else ("last", "pipeline.py")
        began = self.now()
        out = self.cli("local get", "get", *argv, "--local", "--refresh-all", "--json")
        if out is None:
            return
        try:
            done = json.loads(out.stdout.splitlines()[-1])
        except (ValueError, IndexError):
            self.problem("local get", "no JSON", out.stdout[-500:] + out.stderr[-500:], began)
            return
        expect = LOCAL_STEPS if whole else LAST_STEPS
        if done.get("status") != "success" or done.get("steps_executed") != expect:
            self.problem("local get", f"not {expect} steps", json.dumps(done)[:800], began)
        with self.lock:
            self.local_runs[done["run_id"]] = {
                "at": began,
                "wall": time.time(),
                "steps": done.get("steps_executed"),
            }

    def socket_commands(self, rng: random.Random) -> None:
        """Commands with neither `--local` nor `--server`: sent to the server over its socket."""
        pick = rng.random()
        began = self.now()
        if pick < 0.35 and not self.pause_writers.is_set():
            out = self.cli("socket get", "get", "last", "pipeline.py", "--refresh-all", "--json")
            if out is not None:
                try:
                    done = json.loads(out.stdout.splitlines()[-1])
                    with self.lock:
                        self.accepted[done["run_id"]] = {
                            "at": began,
                            "wall": time.time(),
                            "via": "socket",
                            "expect": LAST_STEPS,
                            "ended": done.get("status"),
                        }
                except (ValueError, IndexError, KeyError):
                    self.problem("socket get", "no JSON", out.stdout[-800:], began)
        elif pick < 0.6:
            self.json_of("socket status", "status", "pipeline.py", "--json")
        elif pick < 0.8:
            self.json_of("socket history", "history", "--json")
        elif pick < 0.9:
            self.json_of("socket queue", "queue", "--json")
        else:
            self.json_of("socket gc", "gc", "--json", timeout=300)

    def json_of(self, kind: str, *argv: str, timeout: float = 120):
        began = self.now()
        out = self.cli(kind, *argv, timeout=timeout)
        if out is None:
            return None
        try:
            return json.loads(out.stdout)
        except ValueError:
            self.problem(kind, "no JSON", out.stdout[-500:] + out.stderr[-500:], began)
            return None

    def local_reads(self, rng: random.Random) -> None:
        pick = rng.randrange(7)
        began = self.now()
        if pick == 0:
            status = self.json_of("local status", "status", "pipeline.py", "--local", "--json")
            if status is not None and len(status.get("nodes", [])) != 6:
                self.problem(
                    "torn", "status lists fewer nodes than the file", str(status)[:800], began
                )
        elif pick == 1:
            history = self.json_of("local history", "history", "--local", "--json", "--all")
            if history is not None:
                runs = history["runs"]
                ids = [r["run_id"] for r in runs]
                if len(set(ids)) != len(ids) or (
                    not history.get("truncated") and history["total"] != len(runs)
                ):
                    self.problem(
                        "torn",
                        f"history: total {history['total']}, {len(runs)} rows, "
                        f"{len(set(ids))} distinct",
                        "",
                        began,
                    )
                for run in runs[:30]:
                    self.check_run_counts("local history", run, began)
        elif pick == 2:
            self.json_of("local stats", "stats", "last", "pipeline.py", "--local", "--json")
        elif pick == 3:
            self.json_of(
                "local status --keys",
                "status",
                "keyed",
                "pipeline.py",
                "--keys",
                "--local",
                "--json",
            )
        elif pick == 4:
            self.cli("local logs", "logs", "--local")
        elif pick == 5:
            with self.lock:
                known = list(self.local_runs)[-5:]
            if known:
                run_id = rng.choice(known)
                detail = self.json_of("local history RUN", "history", run_id, "--local", "--json")
                if detail is not None and detail["run"]["status"] != "success":
                    self.problem("lost", f"run {run_id} reads {detail['run']['status']}", "", began)
        else:
            history = self.json_of("local history", "history", "--local", "--json")
            live = [
                r["run_id"] for r in (history or {}).get("runs", []) if r["status"] == "running"
            ]
            if live:
                # Follow a run that is going until it ends; the exit code is the run's.
                self.cli(
                    "local logs -f", "logs", live[0], "-f", "--local", timeout=90, ok=(0, 1, 130)
                )

    def check_run_counts(self, kind: str, run: dict, began: float) -> None:
        total = run.get("steps_total")
        done = (run.get("steps_executed") or 0) + (run.get("steps_cached") or 0)
        if run.get("status") == "success" and total is not None and done != total:
            self.problem(
                "torn", f"{kind}: a success with {done} of {total} steps", str(run)[:800], began
            )

    def http_reads(self, rng: random.Random) -> None:
        began = self.now()
        pick = rng.randrange(6)
        if pick == 0:
            status, body = self.http("http GET /state", "GET", "/state")
            if status == 200 and len({node.get("id") for node in body or []}) != 7:
                self.problem(
                    "torn", "/state does not list the 7 nodes once each", str(body)[:500], began
                )
        elif pick in (1, 2):
            status, body = self.http("http GET /runs", "GET", "/runs?limit=50")
            if status != 200 or not body:
                return
            runs = body["runs"]
            if body["total"] < len(runs):
                self.problem("torn", f"/runs total {body['total']} < {len(runs)} rows", "", began)
            for run in runs:
                self.check_run_counts("/runs", run, began)
            recorded = [r for r in runs if r.get("run_id")]
            if recorded:
                self.detail_of(rng.choice(recorded[:10]))
        elif pick == 3:
            self.http("http GET /health", "GET", "/health")
            self.http("http GET /schedule", "GET", "/schedule")
        elif pick == 4:
            self.http("http GET /history", "GET", "/history")
            self.http("http GET /queue", "GET", "/queue")
        else:
            self.http("http GET /assets", "GET", "/assets")
            self.http("http GET /nodes", "GET", "/nodes")

    def detail_of(self, listed: dict) -> None:
        """A run `GET /runs` listed has a detail; how long a 404 lasted when it did not."""
        run_id = listed["run_id"]
        began = time.monotonic()
        at = self.now()
        tries = 0
        while True:
            status, _ = self.http("http GET /runs/{id}", "GET", f"/runs/{run_id}")
            if status != 404:
                break
            tries += 1
            if time.monotonic() - began > 5:
                self.problem("torn", f"run {run_id} is listed and its detail is 404 for 5s", "", at)
                return
            time.sleep(0.02)
        with self.lock:
            self.detail_404_ms.append((time.monotonic() - began) * 1000 if tries else 0.0)
            if tries:
                self.detail_404_of[f"{listed.get('triggered_by')} {listed.get('status')}"] += 1

    def maintenance(self, rng: random.Random) -> None:
        pick = rng.randrange(5)
        if pick == 0:
            self.json_of("local gc", "gc", "--local", "--json", timeout=300)
        elif pick == 1:
            status, body = self.http("http POST /gc", "POST", "/gc", {}, timeout=300)
            if status == 200 and body:
                self.counts["POST /gc passes"] += 1
        elif pick == 2:
            listed = self.json_of("state backups", "state", "backups", "--json")
            if listed is not None and listed.get("store_error"):
                self.problem("state backups", "store_error", str(listed["store_error"]), self.now())
        elif pick == 3:
            began = self.now()
            doctor = self.json_of("doctor", "doctor", "pipeline.py", "--local", "--json")
            if doctor is not None:
                checks = {c["name"]: c for c in doctor.get("checks", [])}
                if "metadata" in checks and not checks["metadata"].get("ok"):
                    self.problem("doctor", "metadata check failed", str(checks["metadata"]), began)
        else:
            self.json_of("local gc --dry-run", "gc", "--local", "--dry-run", "--json", timeout=300)

    def sample(self, _rng: random.Random) -> None:
        def size(path: str) -> int:
            try:
                return os.stat(path).st_size
            except OSError:
                return -1

        rss = -1
        if self.proc is not None and self.proc.poll() is None:
            try:
                for line in Path(f"/proc/{self.proc.pid}/status").read_text().splitlines():
                    if line.startswith("VmRSS:"):
                        rss = int(line.split()[1]) * 1024
            except (OSError, ValueError):
                pass
        with self.lock:
            self.samples.append((self.now(), size(f"{self.db}-wal"), size(str(self.db)), rss))

    def watch_stalls(self, _rng: random.Random) -> None:
        """`--stacks`: the server's thread stacks while a request has waited 1.5 s (a binary
        with symbols says where)."""
        now = time.monotonic()
        waiting = [(kind, now - start) for kind, start in list(self.in_flight.values())]
        stuck = [(kind, round(secs, 2)) for kind, secs in waiting if secs > 1.5]
        if not stuck or now - self.last_stack < 4 or self.stacks >= 12 or self.phase:
            return
        if self.proc is None or self.proc.poll() is not None:
            return
        self.last_stack = now
        self.stacks += 1
        out = subprocess.run(
            ["eu-stack", "-p", str(self.proc.pid)],
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        (self.root / f"stacks-{self.now():.1f}.txt").write_text(
            f"waiting: {stuck}\n{out.stdout}\n{out.stderr}"
        )

    def collect_backups(self, _rng: random.Random | None = None) -> None:
        """Copy each backup out of the store as it appears: the store keeps the newest five."""
        for sidecar in sorted((self.store / "default/state/backups").glob("*.json")):
            name = sidecar.stem
            if name in self.backups:
                continue
            source = sidecar.with_suffix(".db")
            try:
                digest = json.loads(sidecar.read_text())["digest"]
                data = source.read_bytes()
            except (OSError, ValueError, KeyError):
                continue  # Rotated out, or still being written: the next pass.
            if hashlib.sha256(data).hexdigest() != digest:
                continue
            copy = self.kept / f"{name}.db"
            copy.write_bytes(data)
            shutil.copyfile(sidecar, self.kept / f"{name}.json")
            with self.lock:
                self.backups[name] = copy

    def start_loops(self) -> None:
        self.stop.clear()
        self.loop("api runs", self.api_runs, 1, (0.05, 0.4))
        # Seldom enough that the server's own timer takes most backups: every process's
        # backups of one database share the interval, and a `--local` run's last one resets it.
        self.loop("local gets", self.local_gets, 2, (1.5, 4.0))
        self.loop("socket commands", self.socket_commands, 3, (0.1, 0.6))
        self.loop("local reads a", self.local_reads, 4, (0.02, 0.2))
        self.loop("local reads b", self.local_reads, 5, (0.02, 0.2))
        self.loop("http reads a", self.http_reads, 6, (0.01, 0.1))
        self.loop("http reads b", self.http_reads, 7, (0.01, 0.1))
        self.loop("maintenance", self.maintenance, 8, (0.5, 2.0))
        self.loop("samples", self.sample, 9, (0.2, 0.2))
        self.loop("backups", self.collect_backups, 10, (0.5, 0.5))
        if self.args.stacks:
            self.loop("stalls", self.watch_stalls, 11, (0.25, 0.25))

    def stop_loops(self) -> None:
        self.stop.set()
        for thread in self.threads:
            thread.join(timeout=180)
            if thread.is_alive():
                raise Failed(f"the {thread.name} loop did not stop")
        self.threads.clear()

    # ─── the database, read without disturbing it ─────────────────────────────

    def integrity(self, what: str, path: Path | None = None) -> None:
        """`PRAGMA integrity_check` (and `foreign_key_check`) on a private copy of the database
        and its log, taken under barca's lock: processes may be writing it."""
        source = path or self.db
        with tempfile.TemporaryDirectory(prefix="owner-serve-", dir=self.root) as scratch:
            copy = Path(scratch) / "metadata.db"
            lock = Path(f"{source}.lock")
            held = lock.open("rb") if lock.exists() else None
            try:
                if held:
                    fcntl.flock(held, fcntl.LOCK_EX)
                shutil.copyfile(source, copy)
                if Path(f"{source}-wal").exists():
                    shutil.copyfile(f"{source}-wal", f"{copy}-wal")
            finally:
                if held:
                    held.close()
            try:
                with closing(sqlite3.connect(copy)) as conn:
                    rows = conn.execute("PRAGMA integrity_check").fetchall()
                    foreign = conn.execute("PRAGMA foreign_key_check").fetchall()
            except sqlite3.DatabaseError as e:
                raise Failed(f"integrity ({what}): the database cannot be read: {e}") from e
        self.counts["integrity checks"] += 1
        if rows != [("ok",)] or foreign:
            raise Failed(f"integrity ({what}): {rows[:5]} {foreign[:5]}")

    def query(self, sql: str, args: tuple = (), path: Path | None = None) -> list[tuple]:
        """Read the database in place. Only with every barca process stopped."""
        with closing(sqlite3.connect(f"file:{path or self.db}?mode=ro", uri=True)) as conn:
            return conn.execute(sql, args).fetchall()

    def sidecars(self) -> dict[str, int]:
        return {
            suffix: os.stat(f"{self.db}{suffix}").st_size
            for suffix in ("", "-wal", "-shm")
            if os.path.exists(f"{self.db}{suffix}")
        }

    # ─── stops and crashes ────────────────────────────────────────────────────

    def hold(self, secs: float) -> str:
        """Start a run that stays in flight for `secs`; its id, once its step is going."""
        (self.dir / "hold.secs").write_text(str(secs))
        run_id = None
        for _ in range(50):
            run_id = self.submit("hold", 1, wait=False)
            if run_id:
                break
            time.sleep(0.2)
        if not run_id:
            raise Failed("the server accepted no run to hold")
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if f"holding {secs}" in self.log_text().split(run_id[:8])[-1]:
                break
            status, body = self.http("http GET /runs/{id}", "GET", f"/runs/{run_id}")
            if status == 200 and body and body.get("status") == "running":
                time.sleep(1.0)
                break
            time.sleep(0.05)
        return run_id

    def after_stop(self, what: str, record: dict) -> None:
        """What every stop must leave: a sound database, and a next server that needs no help."""
        record["left"] = self.sidecars()
        self.integrity(f"after {what}")
        self.start_server()
        self.stops.append(record)

    def drain(self) -> None:
        self.say("SIGTERM with a run in flight: the drain")
        run_id = self.hold(3)
        began = self.enter("drain")
        self.proc.send_signal(signal.SIGTERM)
        code = self.wait_exit(60)
        record = {"what": "SIGTERM during a run", "exit": code, "run": run_id}
        log = self.log_text()
        if code != 0 or "draining" not in log:
            raise Failed(f"a drain exited {code}: {log[-1500:]}")
        self.after_stop("a drain", record)
        self.leave(began, grace=2)
        status, body = self.http("http GET /runs/{id}", "GET", f"/runs/{run_id}")
        record["run status"] = body and body.get("status")
        if status != 200 or body.get("status") != "success":
            raise Failed(f"the run in flight through a drain reads {status} {body}")
        self.accepted[run_id]["ended"] = "success"

    def forced(self) -> None:
        self.say("a second SIGTERM during the drain")
        run_id = self.hold(60)
        began = self.enter("forced stop")
        self.proc.send_signal(signal.SIGTERM)
        deadline = time.monotonic() + 30
        marker = len(self.log_text())
        while "SIGTERM received" not in self.log_text()[marker - 4000 :]:
            if time.monotonic() > deadline:
                raise Failed("no drain began within 30s of SIGTERM")
            time.sleep(0.05)
        time.sleep(0.5)
        self.proc.send_signal(signal.SIGTERM)
        code = self.wait_exit(60)
        record = {"what": "second SIGTERM", "exit": code, "run": run_id}
        if code != 1:
            raise Failed(f"a forced stop exited {code}, not 1: {self.log_text()[-1500:]}")
        (self.dir / "hold.secs").write_text("0")
        self.after_stop("a forced stop", record)
        self.leave(began, grace=2)
        status, body = self.http("http GET /runs/{id}", "GET", f"/runs/{run_id}")
        record["run status"] = body and body.get("status")
        if status != 200 or body.get("status") != "cancelled":
            raise Failed(f"the run a second SIGTERM stopped reads {status} {body}, not cancelled")
        self.accepted[run_id]["ended"] = "cancelled"

    def kill(self, n: int, rng: random.Random) -> None:
        self.say(f"SIGKILL {n}")
        with_hold = n % 2 == 1
        run_id = self.hold(60) if with_hold else None
        time.sleep(rng.uniform(0.2, 1.5))
        status, listing = self.http("http GET /runs", "GET", "/runs?limit=50")
        live = [
            r["run_id"]
            for r in (listing or {}).get("runs", [])
            if r.get("run_id")
            and r["status"] in ("running", "pending")
            and r.get("triggered_by") != "manual"  # A `--local` run goes on without the server.
        ]
        began = self.enter("kill")
        self.proc.kill()
        self.proc.wait()
        self.reap_group()
        (self.dir / "hold.secs").write_text("0")
        record = {
            "what": "SIGKILL",
            "exit": -9,
            "run": run_id,
            "in flight": len(live),
            "at the kill": self.sidecars(),
        }
        # CLI processes are still at work on the database the killed server left.
        time.sleep(rng.uniform(0.2, 1.0))
        marker = len(self.log_text())
        self.after_stop("SIGKILL", record)
        self.leave(began, grace=3)
        self.counts["kills"] += 1
        record["server said"] = [
            line
            for line in self.log_text()[marker:].splitlines()
            if "interrupted" in line or "was in flight" in line or "resuming" in line
        ][:6]
        # A run in flight when its server was killed outright is recorded as interrupted
        # (`barca docs serve-ops`), `error` when it never wrote a row.
        deadline = time.monotonic() + 30
        while True:
            left = []
            for run in live:
                status, body = self.http("http GET /runs/{id}", "GET", f"/runs/{run}")
                state = body.get("status") if status == 200 and body else f"HTTP {status}"
                if state not in TERMINAL:
                    left.append((run, state))
                elif run == run_id:
                    record["run status"] = state
            if not left:
                break
            if time.monotonic() > deadline:
                raise Failed(
                    f"runs in flight at a SIGKILL are not ended by the next server: {left}"
                )
            time.sleep(0.2)
        if run_id and record.get("run status") not in ("interrupted", "error"):
            raise Failed(f"the run a SIGKILL caught mid-step reads {record.get('run status')}")
        if run_id:
            self.accepted[run_id]["ended"] = record["run status"]

    def wait_exit(self, timeout: float) -> int:
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired as e:
            self.proc.kill()
            self.proc.wait()
            raise Failed(f"the server did not stop within {timeout}s of its signal") from e
        finally:
            self.reap_group()

    # ─── the checks at the end ────────────────────────────────────────────────

    def runs_of(self, path: Path | None = None) -> dict[str, tuple]:
        return {
            row[0]: row[1:]
            for row in self.query(
                "SELECT run_id, status, steps_executed, steps_cached, steps_total, started_at, "
                "finished_at, id FROM runs",
                path=path,
            )
        }

    def check_nothing_lost(self, final: dict[str, tuple]) -> dict:
        """Every accepted run, every `--local` run that exited 0 and every fired tick is in the
        history as it ended. The trim may delete runs past the newest KEPT_RUNS."""
        newest = max((row[-1] for row in final.values()), default=0)
        oldest_kept = min((row[-1] for row in final.values()), default=0)
        # The trim deletes the oldest runs past the newest KEPT_RUNS, in whichever process
        # checks the size: a run that is gone is explained only by being older than every
        # run that is left, with KEPT_RUNS of them left.
        full = len(final) >= KEPT_RUNS
        oldest_started = min(
            (
                calendar.timegm(time.strptime(row[4][:19].replace("T", " "), "%Y-%m-%d %H:%M:%S"))
                for row in final.values()
                if row[4]
            ),
            default=0,
        )
        lost: list[str] = []
        trimmed = 0

        def missing(run_id: str, what: str, older: bool) -> None:
            nonlocal trimmed
            if full and older:
                trimmed += 1
            else:
                lost.append(f"{what} {run_id} is not in the history")

        for run_id, seen in self.accepted.items():
            row = final.get(run_id)
            if row is None:
                missing(run_id, f"accepted ({seen['via']})", seen["wall"] < oldest_started + 5)
                continue
            status, executed = row[0], (row[1] or 0) + (row[2] or 0)
            if status not in TERMINAL:
                lost.append(f"accepted run {run_id} is left {status}")
            elif "ended" in seen and seen["ended"] in TERMINAL and status != seen["ended"]:
                lost.append(f"run {run_id} ended {seen['ended']} and the history says {status}")
            elif status == "success" and seen.get("expect") and executed != seen["expect"]:
                lost.append(f"run {run_id} has {executed} steps, not {seen['expect']}")
        for run_id, seen in self.local_runs.items():
            row = final.get(run_id)
            if row is None:
                missing(run_id, "--local run", seen["wall"] < oldest_started + 5)
            elif row[0] != "success" or row[1] != seen["steps"]:
                lost.append(f"--local run {run_id} reads {row[0]} with {row[1]} steps")
        fired = re.findall(r"scheduled run (\S+) → (\w+)", self.log_text())
        ticks = {
            run_id: status
            for run_id, status in self.query(
                "SELECT run_id, run_status FROM schedule_ticks WHERE action = 'fired'"
            )
        }
        per_schedule: Counter[str] = Counter()
        # In the order fired: the ones the trim deleted come before the first one left.
        first_kept = next((n for n, (_, run_id) in enumerate(fired) if run_id in final), 0)
        for n, (schedule, run_id) in reversed(list(enumerate(fired))):
            per_schedule[schedule] += 1
            row = final.get(run_id)
            if row is None:
                missing(run_id, f"tick of {schedule}", n < first_kept + 20)
            elif row[0] not in TERMINAL:
                lost.append(f"scheduled run {run_id} is left {row[0]}")
            if run_id not in ticks and per_schedule[schedule] <= TICKS_PER_SCHEDULE - 50:
                lost.append(f"scheduled run {run_id} of {schedule} has no tick record")
        # Step rows: a success's rows are there, unless the trim removed superseded results.
        removed_rows = sum(
            int(n) for n in re.findall(r"removed (\d+) old results", self.log_text())
        )
        short = self.query(
            "SELECT r.run_id, r.steps_executed, count(m.id) FROM runs r LEFT JOIN materializations m "
            "ON m.run_id = r.run_id AND m.status = 'success' WHERE r.status = 'success' "
            "GROUP BY r.run_id HAVING count(m.id) != r.steps_executed"
        )
        over = [row for row in short if row[2] > row[1]]
        missing_rows = sum(row[1] - row[2] for row in short if row[2] < row[1])
        if over:
            lost.append(f"runs with more step rows than steps: {over[:3]}")
        if missing_rows > removed_rows:
            lost.append(
                f"{missing_rows} step rows of successful runs are gone and the trim removed "
                f"{removed_rows}: {short[:3]}"
            )
        left_running = [r for r, row in final.items() if row[0] not in TERMINAL]
        if left_running:
            lost.append(f"runs left unfinished with every process stopped: {left_running[:5]}")
        if lost:
            raise Failed("lost: " + "; ".join(lost[:8]) + f" ({len(lost)} in all)")
        return {
            "runs in the final history": len(final),
            "newest run row id": newest,
            "oldest kept row id": oldest_kept,
            "accepted by the server": len(self.accepted),
            "local runs that exited 0": len(self.local_runs),
            "ticks the server fired": len(fired),
            "tick records": len(ticks),
            "runs ever recorded": newest,
            "tracked runs the trim deleted": trimmed,
            "step rows removed by the trim": removed_rows,
            "recorded steps": self.query("SELECT coalesce(max(id), 0) FROM materializations")[0][0],
            "step rows in the final database": self.query("SELECT count(*) FROM materializations")[
                0
            ][0],
        }

    def check_backups(self, final: dict[str, tuple]) -> dict:
        """Each backup is a whole database, restores, and reads as a history the final one
        continues: no run in a backup is missing or different later, apart from a run that
        was still going, and runs the trim deleted since."""
        self.collect_backups()
        oldest_final = min((row[-1] for row in final.values()), default=0)
        scratch = self.root / "restores"
        checked = 0
        for name in sorted(self.backups):
            copy = self.backups[name]
            with closing(sqlite3.connect(f"file:{copy}?mode=ro", uri=True)) as conn:
                rows = conn.execute("PRAGMA integrity_check").fetchall()
                if rows != [("ok",)] or conn.execute("PRAGMA foreign_key_check").fetchall():
                    raise Failed(f"backup {name}: integrity_check {rows[:5]}")
            # Put back with `barca state restore` into an empty project, from a store that
            # holds this backup only.
            project = scratch / name / "project"
            store = scratch / name / "store/default/state/backups"
            project.mkdir(parents=True)
            store.mkdir(parents=True)
            shutil.copyfile(copy, store / copy.name)
            shutil.copyfile(copy.with_suffix(".json"), store / f"{name}.json")
            (project / "pipeline.py").write_text(SOURCE)
            env = self.env | {"BARCA_REMOTE_URI": str(scratch / name / "store")}
            restored = subprocess.run(
                [self.barca, "state", "restore", "--json"],
                cwd=project,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            if restored.returncode != 0:
                raise Failed(f"backup {name} does not restore: {restored.stderr[-1500:]}")
            listed = subprocess.run(
                [self.barca, "history", "--local", "--json", "--all"],
                cwd=project,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            if listed.returncode != 0:
                raise Failed(f"history of restored backup {name}: {listed.stderr[-1500:]}")
            history = {r["run_id"]: r for r in json.loads(listed.stdout)["runs"]}
            then = self.runs_of(project / ".barca/metadata.db")
            if set(history) != set(then):
                raise Failed(f"backup {name}: `barca history` lists other runs than its rows")
            with closing(
                sqlite3.connect(f"file:{project / '.barca/metadata.db'}?mode=ro", uri=True)
            ) as conn:
                if conn.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
                    raise Failed(f"backup {name}: the restored database fails integrity_check")
            for run_id, row in then.items():
                now = final.get(run_id)
                if now is None:
                    if row[-1] < oldest_final and len(final) >= KEPT_RUNS:
                        continue  # Deleted by the trim since.
                    raise Failed(f"backup {name} has run {run_id} and the final history does not")
                if row[0] not in TERMINAL:
                    continue  # Still going when the backup was taken.
                if row[:6] != now[:6]:
                    raise Failed(
                        f"run {run_id} is {row[:6]} in backup {name} and {now[:6]} at the end"
                    )
            shutil.rmtree(scratch / name)
            checked += 1
        takers: Counter[str] = Counter()
        for name in self.backups:
            run_id = json.loads((self.kept / f"{name}.json").read_text()).get("run_id")
            takers[
                "the server's timer or shutdown"
                if run_id is None
                else "a --local run"
                if run_id in self.local_runs
                else "a run of the server"
            ] += 1
        return {"backups checked": checked, "taken by": dict(takers)}

    def restore_beside_cli(self, rng: random.Random) -> dict:
        """`barca state restore` with the server down and CLI processes at work."""
        self.say("restore with the server down and CLI processes active")
        self.stop.clear()
        self.loop("local reads a", self.local_reads, 21, (0.02, 0.2))
        self.loop("local reads b", self.local_reads, 22, (0.02, 0.2))
        self.loop("local gets", self.local_gets, 23, (0.1, 0.5))
        self.loop("maintenance", self.maintenance_local, 24, (0.3, 1.0))
        done = []
        time.sleep(2)
        for index in (1, 2, 1):
            began = self.enter("restore")
            out = self.cli("state restore", "state", "restore", "--from", str(index), "--json")
            if out is None:
                raise Failed(f"restore failed beside CLI processes: {self.problems[-1]}")
            done.append(json.loads(out.stdout)["restored"])
            self.integrity("after a restore beside CLI processes")
            time.sleep(rng.uniform(1.0, 2.5))
            self.leave(began, grace=1)
            self.counts["restores"] += 1
        self.stop_loops()
        self.integrity("after the restores, CLI stopped")
        check = self.cli("local get", "get", "pipeline.py", "--local", "--refresh-all", "--json")
        if check is None:
            raise Failed(f"`barca get` after the restores: {self.problems[-1]}")
        # A server on the restored database.
        self.start_server()
        run_id = self.submit("last", LAST_STEPS, loop=False)
        if not run_id or self.accepted[run_id].get("ended") != "success":
            raise Failed("the server on the restored database did not finish a run")
        self.proc.send_signal(signal.SIGTERM)
        if self.wait_exit(90) != 0:
            raise Failed("the server on the restored database did not stop cleanly")
        self.integrity("after the server on the restored database")
        return {"restored": done}

    def maintenance_local(self, rng: random.Random) -> None:
        if rng.random() < 0.5:
            self.json_of("local gc", "gc", "--local", "--json", timeout=300)
        else:
            self.json_of("state backups", "state", "backups", "--json")

    # ─── the run ──────────────────────────────────────────────────────────────

    def run(self) -> dict:
        rng = random.Random(self.args.seed)
        report: dict = {"seed": self.args.seed, "seconds": self.args.seconds}
        self.start_server()
        server_pid = self.proc.pid
        self.start_loops()
        self.say(f"sustained: {self.args.seconds}s")
        end = time.monotonic() + self.args.seconds
        while time.monotonic() < end:
            if self.proc.poll() is not None:
                raise Failed(f"the server died with exit {self.proc.returncode}")
            time.sleep(0.5)
        sustained = list(self.samples)
        report["sustained"] = self.sizes_report(sustained)
        report["sustained"]["one server process"] = self.proc.pid == server_pid
        report["sustained"]["seconds"] = round(self.now(), 1)

        if not self.args.no_stops:
            self.drain()
            self.forced()
            for n in range(self.args.kills):
                self.kill(n + 1, rng)
                time.sleep(rng.uniform(2, 4))

        # A clean stop with nothing else on the database: what the server leaves behind.
        self.pause_writers.set()
        self.stop_loops()
        self.say("clean stop")
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            _, queue = self.http("http GET /queue", "GET", "/queue")
            if queue and not queue.get("running") and not queue.get("queued"):
                break
            time.sleep(0.2)
        before = self.sidecars()
        self.proc.send_signal(signal.SIGTERM)
        code = self.wait_exit(90)
        after = self.sidecars()
        if code != 0:
            raise Failed(f"the clean stop exited {code}: {self.log_text()[-1500:]}")
        self.integrity("after the clean stop")
        first = self.cli("local history", "history", "--local", "--json")
        after_read = self.sidecars()
        second = self.cli("local get", "get", "last", "pipeline.py", "--local", "--json")
        if first is None or second is None:
            raise Failed(f"a command after the clean stop failed: {self.problems[-1]}")
        noisy = [line for line in (first.stderr + second.stderr).splitlines() if "warning" in line]
        report["clean stop"] = {
            "exit": code,
            "before": before,
            "left on disk": after,
            "after a read command": after_read,
            "after a write command": self.sidecars(),
            "warnings from the next commands": noisy,
        }
        self.collect_backups()
        final = self.runs_of()
        report["history"] = self.check_nothing_lost(final)
        report["backups"] = self.check_backups(final)
        report["stops"] = self.stops
        if not self.args.no_stops:
            self.pause_writers.clear()
            report["restore"] = self.restore_beside_cli(rng)
        report |= self.summary()
        if self.problems:
            raise Failed(
                f"{len(self.problems)} errors: "
                + "; ".join(f"{p['kind']}: {p['what']} [{p['class']}]" for p in self.problems[:6])
            )
        slow = {
            kind: round(percentile(values, 0.95))
            for kind, values in self.latency.items()
            if kind.startswith("http GET") and percentile(values, 0.95) > READ_BAR_MS
        }
        if slow and not self.args.no_latency_bar:
            raise Failed(f"read routes over {READ_BAR_MS} ms at the 95th percentile: {slow}")
        return report

    def sizes_report(self, samples: list[tuple[float, int, int, int]]) -> dict:
        wal = [s[1] for s in samples if s[1] >= 0]
        rss = [s[3] for s in samples if s[3] > 0]
        db = [s[2] for s in samples if s[2] >= 0]
        quarter = max(1, len(rss) // 4)
        return {
            "samples": len(samples),
            "wal max": max(wal, default=0),
            "wal median": int(percentile([float(w) for w in wal], 0.5)),
            "wal samples at 0": sum(1 for w in wal if w == 0),
            "wal samples under 64 KiB": sum(1 for w in wal if w <= 65536),
            "wal last": wal[-1] if wal else None,
            "db max": max(db, default=0),
            # A database file only shrinks by `VACUUM`.
            "times the db file shrank": sum(1 for a, b in zip(db, db[1:]) if b < a),
            "db last": db[-1] if db else None,
            "rss first": rss[0] if rss else None,
            "rss max": max(rss, default=0),
            "rss median, first quarter": int(percentile([float(r) for r in rss[:quarter]], 0.5)),
            "rss median, last quarter": int(percentile([float(r) for r in rss[-quarter:]], 0.5)),
        }

    def summary(self) -> dict:
        log = self.log_text()
        lines = Counter()
        for line in log.splitlines():
            if "warning" in line or "error" in line.lower():
                lines[re.sub(r"[0-9a-f]{8,}|\d+", "N", line)[:200]] += 1
        never = {word: log.count(word) for word in NEVER if word in log}
        if never:
            self.problems.append(
                {
                    "at": self.now(),
                    "kind": "server log",
                    "what": f"says {never}",
                    "class": ", ".join(sorted({NEVER[w] for w in never})),
                    "phase": "",
                    "detail": "\n".join(
                        line for line in log.splitlines() if any(w in line for w in never)
                    )[:3000],
                }
            )
        return {
            "counts": {
                **self.counts,
                "backups": len(self.backups),
                "backup lines in the server log": log.count("backed up the metadata DB"),
                "trims": log.count("share of the limit: now"),
                "evictions": len(re.findall(r"limit; evicted [1-9]", log)),
                "gc on request": log.count("gc on request"),
                "scheduled runs": log.count("scheduled run "),
            },
            "commands": dict(sorted(self.commands.items())),
            "latency ms (p50, p95, p99, max)": {
                kind: [
                    round(percentile(v, 0.5), 1),
                    round(percentile(v, 0.95), 1),
                    round(percentile(v, 0.99), 1),
                    round(max(v), 1),
                ]
                for kind, v in sorted(self.latency.items())
            },
            "reads over the bar": {
                kind: sum(1 for ms in v if ms > READ_BAR_MS)
                for kind, v in sorted(self.latency.items())
                if kind.startswith("http GET") and any(ms > READ_BAR_MS for ms in v)
            },
            "slow": self.slow,
            "listed runs whose detail was 404 at first": sum(1 for ms in self.detail_404_ms if ms),
            "longest such 404 ms": round(max(self.detail_404_ms, default=0.0), 1),
            "such runs, as listed": dict(self.detail_404_of),
            "explained failures": dict(self.explained),
            "server log warnings and errors": dict(lines.most_common(20)),
            "errors": self.problems,
            "all samples": self.sizes_report(self.samples),
        }

    def close(self) -> None:
        self.stop.set()
        if self.proc is not None and self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        self.reap_group()


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--seed", type=int, default=1, help="fixes each loop's choices")
    p.add_argument("--seconds", type=float, default=600, help="how long the server is held up")
    p.add_argument("--kills", type=int, default=3, help="SIGKILLs with a restart, after that")
    p.add_argument("--backup-secs", type=int, default=2, help="the server's backup interval")
    p.add_argument("--limit-mb", type=int, default=20, help="[cache] max_size, in MB")
    p.add_argument("--no-stops", action="store_true", help="leave out the stop and crash paths")
    p.add_argument(
        "--no-latency-bar", action="store_true", help="report slow read routes, do not fail on them"
    )
    p.add_argument(
        "--stacks", action="store_true", help="eu-stack the server while a request waits 1.5 s"
    )
    p.add_argument("--trace", action="store_true", help="BARCA_TRACE_TIMING in every process")
    p.add_argument("--report", type=Path, help="write the report here as JSON")
    p.add_argument("--quiet", action="store_true", help="no progress on stderr")
    p.add_argument("--barca", default=str(REPO / ".venv/bin/barca"))
    p.add_argument("--keep", type=Path, help="directory for the run (default: a temp directory)")
    args = p.parse_args()

    # A short path: the server's project socket has to fit a Unix socket address.
    root = Path(tempfile.mkdtemp(prefix="owner-", dir=args.keep))
    harness = Harness(args, root)
    report: dict = {}
    code = 0
    try:
        report = harness.run()
    except Failed as e:
        report = {"failed": str(e), **harness.summary()}
        print(f"FAILED seed {args.seed}: {e}\n  kept: {root}", file=sys.stderr)
        code = 1
    except Exception as e:  # noqa: BLE001
        report = {"harness error": repr(e)}
        print(f"HARNESS ERROR seed {args.seed}: {e!r}\n  kept: {root}", file=sys.stderr)
        import traceback

        traceback.print_exc()
        code = 3
    finally:
        harness.close()
    text = json.dumps(report, indent=1, default=str)
    if args.report:
        args.report.write_text(text)
    print(text)
    if code == 0 and args.keep is None:
        shutil.rmtree(root, ignore_errors=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
