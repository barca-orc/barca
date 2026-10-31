#!/usr/bin/env python3
"""Long use of a real `barca serve`, compressed, with faults: a seeded search for rare bugs.

One writer server runs a pipeline with a scheduled task, beside a `--read-only` server on the
same `.barca`, as a deployment does. A seeded generator picks operations (edit the source, go
back to an old version, SIGKILL or SIGTERM the server, SIGKILL a worker, delete artifacts, `barca
gc`, ...). After each one the invariants below are checked; the first failure stops the run and
keeps its directory.

    tests/simulation/sim.py --seed 7 --seconds 120
    tests/simulation/sim.py --seed 7 --seconds 600 --seed-db-mb 2000 --keep /data/sim   # an upgrade
    tests/simulation/sim.py --seeds 20 --seconds 60                     # 20 seeds, one after another

The seed fixes the sequence of operations, not the timing between them: a replay runs the same
plan but can land a kill somewhere else. Failures that depend on timing may take several tries.

Invariants:
  values    every result equals what the pipeline's source says it must be
  health    both servers answer, with no metadata_error; no panic in any log
  integrity PRAGMA integrity_check passes
  history   the newest runs and each node's latest success are never deleted
  liveness  once the server is left alone, no run stays `running`; `get` keeps completing
  eviction  artifacts that fit beside the database's share are not evicted for its size
  size      once left alone, the database and its WAL settle within the share of the limit,
            and `.barca` within the limit

Exit codes: 0 all seeds passed, 1 an invariant failed, 3 the harness itself failed.
"""

from __future__ import annotations

import argparse
import http.client
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
from contextlib import closing
from pathlib import Path
from typing import ClassVar

REPO = Path(__file__).resolve().parents[2]
SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

# The binary's constants (crates/barca-core/src/metadata_size.rs): a change there fails here.
ALLOWANCE_PERCENT = 10
LARGEST_ALLOWANCE = 1 << 30
JOURNAL_LIMIT = 64 * 1024 * 1024
KEPT_RUNS = 1000
# A check in progress writes a little: the bound is tested with this much slack on top.
SLACK = 1024 * 1024

SOURCE = """\
import time

from barca import asset, task, Schedule

K = {k}


@asset()
def base() -> dict:
    return {{"k": K, "pad": "x" * {pad}}}


@asset(inputs={{"b": base}})
def double(b: dict) -> dict:
    return {{"v": b["k"] * 2, "pad": b["pad"]}}


@asset(inputs={{"d": double}})
def final(d: dict) -> dict:
    return {{"v": d["v"] + 1}}


@task(freshness=Schedule("* * * * * *"))
def tick() -> None:
    for i in range({lines}):
        print("tick line", i, "x" * 60)
    time.sleep({tick_sleep})
"""


class Violation(Exception):
    """An invariant failed: a bug in barca, or in what this says barca promises."""


def expected(k: int) -> int:
    return k * 2 + 1


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server:
    def __init__(self, sim: Sim, name: str, *args: str) -> None:
        self.sim, self.name, self.args = sim, name, args
        self.port = free_port()
        self.proc: subprocess.Popen | None = None
        self.log = sim.dir / f"{name}.log"

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def start(self) -> None:
        self.proc = subprocess.Popen(
            [
                self.sim.barca,
                "serve",
                "pipeline.py",
                "--port",
                str(self.port),
                "--timezone",
                "utc",
                *self.args,
            ],
            cwd=self.sim.dir,
            stdout=subprocess.DEVNULL,
            stderr=self.log.open("a"),
            env=self.sim.env,
            start_new_session=True,
        )
        deadline = time.monotonic() + self.sim.startup_timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise Violation(
                    f"{self.name} exited {self.proc.returncode} at startup; see {self.log}"
                )
            if self.health(timeout=2) is not None:
                return
            time.sleep(0.05)
        raise Violation(f"{self.name} did not answer /health in {self.sim.startup_timeout}s")

    def health(self, timeout: float = 10) -> dict | None:
        try:
            with urllib.request.urlopen(f"{self.url}/health", timeout=timeout) as r:
                return json.loads(r.read())
        except (OSError, ValueError):
            return None

    def stop(self, sig: int = signal.SIGKILL, timeout: float = 60) -> None:
        if self.proc is None or self.proc.poll() is not None:
            return
        self.proc.send_signal(sig)
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired as e:
            self.proc.kill()
            self.proc.wait()
            raise Violation(f"{self.name} did not stop within {timeout}s of signal {sig}") from e

    def reap_group(self) -> None:
        if self.proc is not None:
            try:
                os.killpg(self.proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


class Sim:
    def __init__(self, args: argparse.Namespace, seed: int, root: Path) -> None:
        self.args, self.seed = args, seed
        self.rng = random.Random(seed)
        self.dir = root / f"seed-{seed}"
        self.dir.mkdir(parents=True)
        self.barca = args.barca
        self.limit = args.limit_mb * 1_000_000
        self.allowance = min(self.limit // 100 * ALLOWANCE_PERCENT, LARGEST_ALLOWANCE)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
        self.env |= {
            "BARCA_POOL_SIZE": "2",
            "RUST_BACKTRACE": "1",
            "BARCA_CACHE_MAX_SIZE": str(self.limit),
            "BARCA_TEST_CACHE_CHECKPOINT_SECS": str(args.checkpoint_secs),
        }
        self.startup_timeout = 60.0
        self.k = 1
        self.versions = [1]
        self.seen_runs: list[tuple[str, int]] = []
        self.latest_success: dict[str, int] = {}
        self.ops: list[dict] = []
        self.peak = 0
        self.slowest_ms = 0
        self.finished = False
        self.poll_failure: str | None = None
        self.evictions_seen = 0
        self.artifacts_before = 0
        self.write_source()
        self.writer = Server(self, "writer")
        self.reader = Server(self, "reader", "--read-only")

    db = property(lambda self: self.dir / ".barca/metadata.db")

    def write_source(self) -> None:
        text = SOURCE.format(
            k=self.k,
            pad=self.args.pad_bytes,
            lines=self.args.tick_lines,
            tick_sleep=self.args.tick_sleep,
        )
        tmp = self.dir / "pipeline.py.tmp"
        tmp.write_text(text)
        tmp.replace(self.dir / "pipeline.py")

    def cli(self, *argv: str, timeout: float | None = None) -> subprocess.CompletedProcess:
        timeout = timeout or self.args.command_timeout
        try:
            return subprocess.run(
                [self.barca, *argv],
                cwd=self.dir,
                env=self.env,
                capture_output=True,
                text=True,
                timeout=timeout,
                check=False,
            )
        except subprocess.TimeoutExpired as e:
            raise Violation(f"barca {' '.join(argv)} did not finish in {timeout}s") from e

    # ─── operations ───────────────────────────────────────────────────────────

    def op_get(self) -> None:
        self.get_checked()

    def op_edit(self) -> None:
        self.k = max(self.versions) + 1
        self.versions.append(self.k)
        self.write_source()
        self.get_checked()

    def op_go_back(self) -> None:
        # A return to an exact old state: its rows may be trimmed and its files evicted.
        self.k = self.rng.choice(self.versions)
        self.write_source()
        self.get_checked()

    def op_refresh(self) -> None:
        self.get_checked("--refresh", "base")

    def op_kill_server(self) -> None:
        self.writer.stop(signal.SIGKILL)
        self.writer.reap_group()
        self.writer.start()
        self.get_checked()

    def op_term_server(self) -> None:
        self.writer.stop(signal.SIGTERM, timeout=90)
        self.writer.reap_group()
        self.writer.start()
        self.get_checked()

    def op_kill_reader(self) -> None:
        self.reader.stop(signal.SIGKILL)
        self.reader.reap_group()
        self.reader.start()

    def op_kill_worker(self) -> None:
        # A worker lost mid-step fails its run at worst; the next `get` must still be right.
        out = subprocess.run(
            ["pgrep", "-g", str(self.writer.proc.pid), "-f", "barca._worker"],
            capture_output=True,
            text=True,
            check=False,
        )
        pids = sorted(int(p) for p in out.stdout.split())
        if pids:
            try:
                os.kill(self.rng.choice(pids), signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.get_checked(retries=2)

    def op_gc(self) -> None:
        out = self.cli("gc", "--json", timeout=300)
        if out.returncode != 0:
            raise Violation(f"barca gc exited {out.returncode}: {out.stderr[-2000:]}")

    def op_cli_get(self) -> None:
        # A second writer process on the same database, as `docker compose exec barca barca ...`.
        out = self.cli("get", "final", "pipeline.py", "-o", "value")
        self.check_value(out, "local get")

    def op_delete_artifacts(self) -> None:
        files = sorted(p for p in (self.dir / ".barca/artifacts").rglob("*") if p.is_file())
        for p in self.rng.sample(files, min(len(files), self.rng.randint(1, 5))):
            p.unlink(missing_ok=True)
        self.get_checked()

    def op_read(self) -> None:
        for path in ("/runs", "/health"):
            try:
                with urllib.request.urlopen(f"{self.reader.url}{path}", timeout=30) as r:
                    json.loads(r.read())
            except urllib.error.HTTPError as e:
                raise Violation(f"reader {path} answered {e.code}: {e.read()[:2000]!r}") from e
            except (OSError, ValueError) as e:
                raise Violation(f"reader {path} failed: {e}") from e

    def op_age_gc_state(self) -> None:
        # Long waits the sim does not sit through (STUCK_FOR, SURVEY_EVERY): make them pass.
        path = self.dir / ".barca/gc-state.json"
        try:
            state = json.loads(path.read_text())
        except (OSError, ValueError):
            return
        state["surveyed_at"] = 0
        state["short_at"] = 0
        state["metadata_short"] = {k: [0, v[1]] for k, v in state.get("metadata_short", {}).items()}
        tmp = path.with_suffix(".sim")
        tmp.write_text(json.dumps(state))
        tmp.replace(path)

    # ─── sabotage: breaks what an invariant watches, to prove the invariant can fail ──────

    def sabotage_value(self) -> None:
        out = self.cli("get", "final", "--server", self.writer.url, "--json")
        path = json.loads(out.stdout)["final_output"]["_barca_artifact"]["path"]
        (self.dir / path).write_text('{"v": -1}')

    def sabotage_history(self) -> None:
        with closing(sqlite3.connect(self.db, timeout=60)) as conn, conn:
            conn.execute(
                "DELETE FROM runs WHERE run_id IN (SELECT run_id FROM runs ORDER BY id DESC LIMIT 50)"
            )

    def sabotage_integrity(self) -> None:
        self.writer.stop(signal.SIGKILL)
        self.reader.stop(signal.SIGKILL)
        with self.db.open("r+b") as f:
            f.seek(4096 * 3)
            f.write(os.urandom(4096))
        self.check_integrity()

    def sabotage_eviction(self) -> None:
        self.writer.stop(signal.SIGKILL)
        self.writer.reap_group()
        limit = self.env["BARCA_CACHE_MAX_SIZE"]
        self.env["BARCA_CACHE_MAX_SIZE"] = "1"
        self.writer.start()
        self.env["BARCA_CACHE_MAX_SIZE"] = limit
        self.get_checked()
        time.sleep(self.args.checkpoint_secs + 1)

    def sabotage_size(self) -> None:
        self.size_sabotaged = True
        self.limit = 1

    def poll_reader(self) -> None:
        """Reads through the reader all the time, not only between operations: the reads a
        deployment's dashboard makes while the writer checkpoints or runs `VACUUM`."""
        while not self.finished:
            for path in ("/assets", "/nodes", "/runs", "/history"):
                try:
                    urllib.request.urlopen(f"{self.reader.url}{path}", timeout=30).read()
                except urllib.error.HTTPError as e:
                    if e.code >= 500:
                        self.poll_failure = (
                            self.poll_failure
                            or f"reader {path} answered {e.code}: {e.read()[:2000]!r}"
                        )
                except (OSError, ValueError, http.client.HTTPException):
                    pass  # A reader the sim just killed.
            time.sleep(self.args.poll_seconds)

    OPS: ClassVar[dict[str, int]] = {
        "get": 30,
        "edit": 20,
        "go_back": 10,
        "refresh": 5,
        "read": 15,
        "cli_get": 5,
        "gc": 3,
        "delete_artifacts": 4,
        "age_gc_state": 3,
        "kill_worker": 3,
        "kill_server": 2,
        "term_server": 2,
        "kill_reader": 1,
    }

    def get_checked(self, *extra: str, retries: int = 1) -> None:
        for attempt in range(retries + 1):
            out = self.cli("get", "final", "--server", self.writer.url, "-o", "value", *extra)
            if out.returncode == 0 or attempt == retries:
                break
            time.sleep(0.5)
        self.check_value(out, "get")

    def check_value(self, out: subprocess.CompletedProcess, what: str) -> None:
        if out.returncode != 0:
            raise Violation(f"{what} with K={self.k} exited {out.returncode}: {out.stderr[-2000:]}")
        got = json.loads(out.stdout)["v"]
        if got != expected(self.k):
            raise Violation(f"{what} with K={self.k} returned {got}, not {expected(self.k)}")

    # ─── invariants ───────────────────────────────────────────────────────────

    def sizes(self) -> tuple[int, int, int]:
        def size(p: Path) -> int:
            try:
                return p.stat().st_size
            except OSError:
                return 0

        total = 0
        for root, _, files in os.walk(self.dir / ".barca"):
            total += sum(size(Path(root) / f) for f in files)
        return size(self.db), size(Path(f"{self.db}-wal")), total

    def check_always(self) -> None:
        if self.poll_failure:
            raise Violation(self.poll_failure)
        self.check_reads()
        for server in (self.writer, self.reader):
            if server.proc.poll() is not None:
                raise Violation(f"{server.name} died with exit {server.proc.returncode}")
            health = server.health(timeout=30)
            if health is None:
                raise Violation(f"{server.name} stopped answering /health")
            if health.get("metadata_error"):
                raise Violation(f"{server.name} metadata_error: {health['metadata_error']}")
            text = server.log.read_text(errors="replace")
            if "panicked at" in text:
                raise Violation(
                    f"{server.name} panicked:\n{text[text.index('panicked at') - 200 :][:1500]}"
                )
        db, wal, _ = self.sizes()
        self.peak = max(self.peak, db + wal)
        self.check_eviction()
        if wal > max(JOURNAL_LIMIT, db) * 2 + SLACK * 16:
            raise Violation(f"WAL is {wal} bytes beside a {db} byte database")

    def check_eviction(self) -> None:
        """A large database is the trim's to fix: artifacts that fit beside its share stay."""
        artifacts = 0
        for root, _, files in os.walk(self.dir / ".barca/artifacts"):
            for f in files:
                try:
                    artifacts += (Path(root) / f).stat().st_size
                except OSError:
                    pass
        evictions = len(
            re.findall(r"limit; evicted [1-9]", self.writer.log.read_text(errors="replace"))
        )
        fits = self.artifacts_before + self.allowance <= self.limit * 8 // 10
        if evictions > self.evictions_seen and fits:
            raise Violation(
                f"evicted artifacts that fit: {self.artifacts_before} bytes of artifacts beside a "
                f"{self.allowance} byte share of a {self.limit} byte limit"
            )
        self.evictions_seen, self.artifacts_before = evictions, artifacts

    def check_integrity(self) -> None:
        try:
            with closing(sqlite3.connect(f"file:{self.db}?mode=ro", uri=True, timeout=60)) as conn:
                rows = conn.execute("PRAGMA integrity_check").fetchall()
        except sqlite3.DatabaseError as e:
            raise Violation(f"integrity_check could not read the database: {e}") from e
        if rows != [("ok",)]:
            raise Violation(f"integrity_check: {rows[:5]}")

    def check_history(self) -> None:
        """A run within the newest KEPT_RUNS, and each node's latest success, are never deleted."""
        with closing(sqlite3.connect(f"file:{self.db}?mode=ro", uri=True, timeout=60)) as conn:
            newest = conn.execute("SELECT max(id) FROM runs").fetchone()[0] or 0
            # Ids can have gaps, so fewer runs than this difference came after: never too strict.
            for run_id, row_id in self.seen_runs:
                if (
                    newest - row_id < KEPT_RUNS - 50
                    and not conn.execute(
                        "SELECT 1 FROM runs WHERE run_id = ?", (run_id,)
                    ).fetchone()
                ):
                    raise Violation(f"run {run_id} (id {row_id}, newest {newest}) was deleted")
            self.seen_runs = conn.execute(
                "SELECT run_id, id FROM runs WHERE status != 'running' ORDER BY id DESC LIMIT 50"
            ).fetchall()
            for node, latest in conn.execute(
                "SELECT node_id, max(id) FROM materializations WHERE status = 'success' GROUP BY node_id"
            ).fetchall():
                if latest < self.latest_success.get(node, 0):
                    raise Violation(
                        f"{node}: latest success went back from {self.latest_success[node]} to {latest}"
                    )
                self.latest_success[node] = latest
            for node in set(self.latest_success) - {
                r[0]
                for r in conn.execute(
                    "SELECT DISTINCT node_id FROM materializations WHERE status = 'success'"
                )
            }:
                raise Violation(f"{node}: no success left")

    def settle(self) -> dict:
        """Stop the traffic the sim drives and let the checkpoint finish: the eventual invariants."""
        deadline = time.monotonic() + self.args.settle_seconds
        last = None
        while time.monotonic() < deadline:
            db, wal, total = self.sizes()
            last = {"db": db, "wal": wal, "total": total}
            if (
                db <= self.allowance + SLACK
                and wal <= JOURNAL_LIMIT + SLACK
                and total <= self.limit + SLACK
            ):
                break
            time.sleep(1)
        else:
            raise Violation(
                f"did not settle in {self.args.settle_seconds}s: database {last['db']}, WAL {last['wal']}, "
                f".barca {last['total']}; share {self.allowance}, limit {self.limit}"
            )
        # The server is still scheduling, so this is the state of a live deployment.
        self.get_checked()
        with closing(sqlite3.connect(f"file:{self.db}?mode=ro", uri=True, timeout=60)) as conn:
            stuck = conn.execute(
                "SELECT run_id FROM runs WHERE status = 'running' AND "
                "started_at < datetime('now', ?)",
                (f"-{int(self.args.settle_seconds) + 120} seconds",),
            ).fetchall()
        if stuck:
            raise Violation(f"runs left `running`: {stuck[:5]}")
        return last

    # ─── the run ──────────────────────────────────────────────────────────────

    def seed_history(self) -> None:
        """A database as 0.37.0 left it: every row carries its own copy of the digests."""
        out = self.cli("get", "final", "pipeline.py", "-o", "value")
        self.check_value(out, "first get")
        target = self.args.seed_db_mb * 1_000_000
        blob = json.dumps({f"helper f{i}": "d" * 64 for i in range(500)})
        started = time.monotonic()
        with closing(sqlite3.connect(self.db, timeout=60)) as conn:
            conn.execute("PRAGMA synchronous = OFF")
            columns = [
                r[1] for r in conn.execute("PRAGMA table_info(materializations)") if r[1] != "id"
            ]
            names = ", ".join(columns)
            newest = conn.execute(f"SELECT {names} FROM materializations ORDER BY id").fetchall()
            template = dict(zip(columns, newest[-1], strict=True))
            n = 0
            while self.db.stat().st_size < target:
                rows = []
                for i in range(n, n + 1000):
                    node = f"pipeline.py:{'base' if i % 3 else 'tick'}"
                    rows.append(
                        {
                            **template,
                            "node_id": node,
                            "run_hash": f"old{i}",
                            "run_id": f"oldrun{i // 3}",
                            "artifact_path": f".barca/artifacts/gone/{i}.json",
                            "components": blob,
                            "evicted_at": "2026-01-01 00:00:00",
                        }
                    )
                marks = ", ".join("?" * len(columns))
                with conn:
                    conn.executemany(
                        f"INSERT INTO materializations ({names}) VALUES ({marks})",
                        [[r[c] for c in columns] for r in rows],
                    )
                    conn.executemany(
                        "INSERT OR IGNORE INTO runs (run_id, command, files, status, outcome_json) "
                        "VALUES (?, 'get', 'pipeline.py', 'success', ?)",
                        [(r["run_id"], "y" * 2000) for r in rows[::3]],
                    )
                n += 1000
            # What the first `get` wrote stays each node's latest.
            with conn:
                ids = [
                    r[0]
                    for r in conn.execute(
                        "SELECT id FROM materializations WHERE run_hash NOT LIKE 'old%'"
                    )
                ]
                conn.executemany("DELETE FROM materializations WHERE id = ?", [(i,) for i in ids])
                conn.executemany(
                    f"INSERT INTO materializations ({names}) VALUES ({', '.join('?' * len(columns))})",
                    newest,
                )
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        self.log(
            "seeded",
            rows=n,
            mb=self.db.stat().st_size // 1_000_000,
            seconds=round(time.monotonic() - started, 1),
        )

    def log(self, event: str, **fields) -> None:
        line = {"t": round(time.monotonic() - self.started, 2), "event": event, **fields}
        self.ops.append(line)
        with (self.dir / "trace.jsonl").open("a") as f:
            f.write(json.dumps(line) + "\n")

    def run(self) -> dict:
        self.started = time.monotonic()
        if self.args.from_db:
            (self.dir / ".barca").mkdir()
            shutil.copyfile(self.args.from_db, self.db)
            wal = Path(f"{self.args.from_db}-wal")
            if wal.exists():
                shutil.copyfile(wal, f"{self.db}-wal")
            self.log("copied", mb=self.db.stat().st_size // 1_000_000)
        elif self.args.seed_db_mb:
            self.seed_history()
        self.writer.start()
        self.reader.start()
        self.get_checked()
        threading.Thread(target=self.poll_reader, daemon=True).start()
        left_out = set(self.args.without.split(","))
        names, weights = zip(*(op for op in self.OPS.items() if op[0] not in left_out), strict=True)
        end = self.started + self.args.seconds
        count = 0
        while time.monotonic() < end:
            name = self.rng.choices(names, weights)[0]
            if self.args.sabotage and count == 20:
                getattr(self, f"sabotage_{self.args.sabotage}")()
            begun = time.monotonic()
            self.log("op", op=name, k=self.k)
            getattr(self, f"op_{name}")()
            self.check_always()
            count += 1
            if count % self.args.integrity_every == 0 or self.args.sabotage:
                self.check_integrity()
                self.check_history()
            db, wal, total = self.sizes()
            took = int((time.monotonic() - begun) * 1000)
            self.slowest_ms = max(self.slowest_ms, took)
            self.log("done", ms=took, db=db, wal=wal, total=total)
        settling = time.monotonic()
        final = self.settle()
        final["settle_seconds"] = round(time.monotonic() - settling, 1)
        self.check_always()
        self.check_integrity()
        self.check_history()
        return {
            "seed": self.seed,
            "ops": count,
            "peak_db_and_wal": self.peak,
            "slowest_op_ms": self.slowest_ms,
            "failed_lookups": self.failed_lookups(),
            **final,
        }

    def check_reads(self) -> None:
        if self.args.strict_reads and (lost := self.failed_lookups()):
            raise Violation(f"the reader lost {lost} cache lookups; see {self.reader.log}")

    def failed_lookups(self) -> int:
        """Reads a `--read-only` server lost to the writer's `VACUUM`: known, and not yet explained."""
        return self.reader.log.read_text(errors="replace").count("cache lookup of")

    def close(self) -> None:
        self.finished = True
        for server in (self.writer, self.reader):
            try:
                server.stop(signal.SIGKILL)
            except Violation:
                pass
            server.reap_group()


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument(
        "--seed", type=int, help="run this seed only; a failure prints the seed to pass here"
    )
    p.add_argument("--seeds", type=int, default=1, help="run this many seeds, from a random start")
    p.add_argument("--seconds", type=float, default=60, help="how long each seed drives traffic")
    p.add_argument(
        "--settle-seconds", type=float, default=120, help="how long the size has to settle"
    )
    p.add_argument("--limit-mb", type=int, default=40, help="[cache] max_size, in MB")
    p.add_argument(
        "--seed-db-mb", type=int, default=0, help="start from an old-format database of this size"
    )
    p.add_argument(
        "--from-db", type=Path, help="start from a copy of this metadata database (and its WAL)"
    )
    p.add_argument("--pad-bytes", type=int, default=20_000, help="size of each artifact")
    p.add_argument(
        "--tick-lines", type=int, default=20, help="log lines per tick of the scheduled task"
    )
    p.add_argument(
        "--checkpoint-secs", type=int, default=2, help="the server's size check interval"
    )
    p.add_argument(
        "--integrity-every", type=int, default=25, help="operations between the slower checks"
    )
    p.add_argument(
        "--sabotage",
        choices=["value", "history", "integrity", "size", "eviction"],
        help="break something, to prove the invariant watching it fails",
    )
    p.add_argument("--tick-sleep", type=float, default=0.0, help="seconds each tick stays alive")
    p.add_argument(
        "--command-timeout", type=float, default=120, help="seconds a CLI command may take"
    )
    p.add_argument(
        "--poll-seconds", type=float, default=0.05, help="pause between reads of the reader"
    )
    p.add_argument(
        "--strict-reads", action="store_true", help="fail on a read the reader lost to the writer"
    )
    p.add_argument("--without", default="", help="operations to leave out, comma-separated")
    p.add_argument("--barca", default=str(REPO / ".venv/bin/barca"))
    p.add_argument(
        "--drop-passed", action="store_true", help="with --keep, delete the runs that pass"
    )
    p.add_argument("--keep", type=Path, help="directory for the runs (default: a temp directory)")
    args = p.parse_args()
    if args.seed_db_mb > 1000 and args.keep is None:
        p.error("--seed-db-mb over 1000 needs --keep on a disk with room: /tmp is often small")

    root = args.keep or Path(tempfile.mkdtemp(prefix="barca-sim-"))
    root.mkdir(parents=True, exist_ok=True)
    seeds = (
        [args.seed]
        if args.seed is not None
        else [random.randrange(1 << 32) for _ in range(args.seeds)]
    )
    for seed in seeds:
        sim = Sim(args, seed, root)
        try:
            print(json.dumps(sim.run()), flush=True)
        except Violation as e:
            print(
                f"FAILED seed {seed}: {e}\n  kept: {sim.dir}\n  replay: {' '.join(sys.argv[:1])} --seed {seed}",
                file=sys.stderr,
            )
            return 1
        except Exception as e:  # noqa: BLE001
            print(f"HARNESS ERROR seed {seed}: {e!r}\n  kept: {sim.dir}", file=sys.stderr)
            return 3
        finally:
            sim.close()
        if args.keep is None or args.drop_passed:
            shutil.rmtree(sim.dir, ignore_errors=True)
    if args.keep is None:
        shutil.rmtree(root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
