"""Time every read route of `barca serve` while the metadata DB keeps changing, as it does on a
busy server: runs in flight and results recorded several times a second.

    python benchmarks/read_routes/under_writes.py PROJECT [--env ENV] [--rate 2.4] [--budget 0.5]

PROJECT is a seeded project (benchmarks/read_routes/seed.py, or a copy of a real one). This
script WRITES to its metadata DB: it records a run, owned by this process, that stays `running`,
and copies recent result rows into it at `--rate` rows a second; every `--rows-per-run` rows that
run ends and another starts. Run it on a copy.

Each route is requested `--samples` times, `--gap` seconds apart (so every request finds the DB
changed since the one before), then the UI's polls run together for `--page-seconds`. Prints the
p50, p95 and max of each and exits 1 when a p95 is over `--budget` seconds.
"""

from __future__ import annotations

import argparse
import concurrent.futures as cf
import json
import os
import shutil
import socket
import sqlite3
import statistics
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def fetch(base: str, path: str) -> tuple[float, int, bytes]:
    t = time.perf_counter()
    try:
        with urllib.request.urlopen(base + path, timeout=600) as r:
            body, status = r.read(), r.status
    except urllib.error.HTTPError as e:
        body, status = e.read(), e.code
    return time.perf_counter() - t, status, body


class Writer(threading.Thread):
    """Record results at `rate` rows a second into runs owned by this process."""

    def __init__(self, db: Path, rate: float, rows_per_run: int) -> None:
        super().__init__(daemon=True)
        self.db, self.rate, self.rows_per_run = db, rate, rows_per_run
        self.stop = threading.Event()
        self.rows = 0
        self.runs = 0
        self.error: BaseException | None = None

    def run(self) -> None:
        try:
            self.write()
        except BaseException as e:
            self.error = e
            raise

    def write(self) -> None:
        c = sqlite3.connect(self.db, timeout=30, isolation_level=None)
        c.execute("PRAGMA busy_timeout = 30000")
        cols = [r[1] for r in c.execute("PRAGMA table_info(materializations)") if r[1] != "id"]
        src = [
            r[0]
            for r in c.execute(
                "SELECT id FROM materializations WHERE status = 'success' ORDER BY id DESC LIMIT 2000"
            )
        ]
        if not src:
            raise SystemExit("no results to copy: seed the project first")
        names = ",".join(cols)
        values = ",".join(
            "?1" if col == "run_id" else "datetime('now')" if col == "created_at" else col
            for col in cols
        )
        insert = f"INSERT INTO materializations({names}) SELECT {values} FROM materializations WHERE id = ?2"
        host, pid = socket.gethostname(), os.getpid()
        run_id, i = None, 0
        while not self.stop.is_set():
            if run_id is None or i % self.rows_per_run == 0:
                if run_id:
                    c.execute(
                        "UPDATE runs SET status = 'success', finished_at = datetime('now') WHERE run_id = ?",
                        (run_id,),
                    )
                run_id = uuid.uuid4().hex[:12]
                job = c.execute(
                    "SELECT node_id FROM materializations WHERE id = ?", (src[i % len(src)],)
                ).fetchone()[0]
                c.execute(
                    "INSERT INTO runs(run_id, command, files, status, started_at, pid, host, owner, job, "
                    "steps_total) VALUES (?, 'get', '[]', 'running', datetime('now'), ?, ?, '', ?, ?)",
                    (run_id, pid, host, job.split("[")[0], self.rows_per_run),
                )
                self.runs += 1
            c.execute(insert, (run_id, src[i % len(src)]))
            c.execute(
                "UPDATE runs SET steps_executed = COALESCE(steps_executed, 0) + 1 WHERE run_id = ?",
                (run_id,),
            )
            i += 1
            self.rows += 1
            self.stop.wait(1 / self.rate)
        if run_id:
            c.execute(
                "UPDATE runs SET status = 'success', finished_at = datetime('now') WHERE run_id = ?",
                (run_id,),
            )
        c.close()


def pick(base: str) -> dict:
    """The nodes and ids the routes are asked about: the partitioned node with most keys, a
    plain node with history, the first group and the latest finished run."""

    def q(s: str) -> str:
        return urllib.parse.quote(s, safe="")

    state = json.loads(fetch(base, "/state")[2])
    with_runs = [n for n in state if n.get("last_materialization")]
    keyed = [n for n in with_runs if n.get("partitioned")]
    plain = [n for n in with_runs if not n.get("partitioned")]
    groups = json.loads(fetch(base, "/groups")[2])
    runs = json.loads(fetch(base, "/runs?limit=50")[2])["runs"]
    done = next((r["run_id"] for r in runs if r["status"] in ("success", "failed")), None)
    out = {"assets": [q(n["id"]) for n in plain[:1]], "keyed": [], "group": None, "run": done}
    for node in keyed[:1]:
        keys = json.loads(fetch(base, f"/assets/{q(node['id'])}/partitions?limit=1")[2])
        first = (keys.get("keys") or [None])[0]
        key = first.get("key") if isinstance(first, dict) else first
        out["keyed"].append((q(node["id"]), key))
    if groups:
        out["group"] = groups[0]["id"]
    return out


def routes(p: dict) -> list[str]:
    r = [
        "/health",
        "/project",
        "/groups",
        "/assets",
        "/plan",
        "/list",
        "/nodes",
        "/state",
        "/schedule",
        "/pauses",
        "/pins",
        "/queue",
        "/runs?limit=100",
        "/runs?status=running",
        "/history?limit=20",
        "/history",
        "/materializations?limit=100",
        "/sql/views",
    ]
    if p["group"]:
        r.append(f"/state?targets={urllib.parse.quote(p['group'])}")
    if p["run"]:
        r += [f"/runs/{p['run']}", f"/runs/{p['run']}/logs", f"/logs/{p['run']}"]
    for a in p["assets"] + [k for k, _ in p["keyed"]]:
        r += [
            f"/assets/{a}",
            f"/assets/{a}/schema?sample=5",
            f"/assets/{a}/versions",
            f"/stats/{a}",
        ]
    for a, key in p["keyed"]:
        r += [f"/assets/{a}/partitions?limit=200", f"/assets/{a}/partitions/grid"]
        if key is not None:
            r.append(
                f"/assets/{a}/partitions/key?{urllib.parse.urlencode({'key': key, 'sample': 5})}"
            )
    return r


def ui_polls(p: dict) -> dict[str, float]:
    """What an open asset page polls, and how often (ui/src/hooks)."""
    polls = {
        "/runs?limit=100": 2,
        "/state": 2,
        "/queue": 2,
        "/health": 10,
        "/schedule": 10,
        "/pauses": 10,
        "/pins": 10,
    }
    for a in p["assets"][:1]:
        polls[f"/assets/{a}"] = 10
        polls[f"/assets/{a}/schema?sample=5"] = 10
    for a, _ in p["keyed"][:1]:
        polls[f"/assets/{a}/partitions?limit=200"] = 10
        polls[f"/assets/{a}/partitions/grid"] = 5
    return polls


def stats(xs: list[float]) -> dict:
    xs = sorted(xs)
    return {
        "n": len(xs),
        "p50": statistics.median(xs),
        "p95": xs[min(len(xs) - 1, int(0.95 * len(xs)))],
        "max": xs[-1],
    }


def short(route: str, p: dict) -> str:
    for a in p["assets"] + [k for k, _ in p["keyed"]]:
        route = route.replace(
            a, "{" + ("keyed" if any(a == k for k, _ in p["keyed"]) else "asset") + "}"
        )
    return route.replace(p["run"] or "\0", "{run}")


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("project", type=Path)
    ap.add_argument("--barca", default=os.environ.get("BARCA") or shutil.which("barca") or "barca")
    ap.add_argument("--env", help="the barca environment (`barca serve --env`)")
    ap.add_argument("--rate", type=float, default=2.4, help="result rows written a second")
    ap.add_argument("--rows-per-run", type=int, default=35)
    ap.add_argument("--samples", type=int, default=8)
    ap.add_argument("--gap", type=float, default=0.5)
    ap.add_argument("--page-seconds", type=float, default=60)
    ap.add_argument(
        "--budget", type=float, default=0.5, help="the most a route's p95 may take, seconds"
    )
    ap.add_argument("--only", help="comma-separated route prefixes to time")
    ap.add_argument("--json", type=Path)
    a = ap.parse_args()
    project = a.project.resolve()
    env_dir = project / ".barca" / "envs" / a.env if a.env else project / ".barca"
    db = env_dir / "metadata.db"
    if not db.exists():
        raise SystemExit(f"no metadata DB at {db}")
    with sqlite3.connect(db) as c:
        if c.execute("SELECT count(*) FROM run_queue").fetchone()[0]:
            raise SystemExit("the run queue is not empty: the server would start those runs")

    port = free_port()
    base = f"http://127.0.0.1:{port}"
    log = (project.parent / "serve.log").open("w")
    cmd = [a.barca, "serve", "--no-schedule", "--port", str(port)] + (
        ["--env", a.env] if a.env else []
    )
    server = subprocess.Popen(
        cmd, cwd=project, stdout=subprocess.DEVNULL, stderr=log, start_new_session=True
    )
    writer = Writer(db, a.rate, a.rows_per_run)
    report: dict = {"rate": a.rate, "budget": a.budget, "routes": [], "page": {}}
    try:
        deadline = time.time() + 600
        while True:
            try:
                fetch(base, "/health")
                break
            except OSError:
                if time.time() > deadline or server.poll() is not None:
                    raise SystemExit(f"barca serve did not start; see {log.name}")
                time.sleep(0.2)
        p = pick(base)
        writer.start()
        time.sleep(2)
        if writer.error or writer.rows == 0:
            raise SystemExit(f"the writer is not writing: {writer.error!r}")
        only = a.only.split(",") if a.only else None
        over = []
        print(f"{'route':58} {'status':>6} {'bytes':>11} {'p50':>8} {'p95':>8} {'max':>8}")
        for route in routes(p):
            if only and not any(route.startswith(o) for o in only):
                continue
            times, status, size = [], 0, 0
            for _ in range(a.samples):
                t, status, body = fetch(base, route)
                times.append(t)
                size = len(body)
                time.sleep(a.gap)
            s = stats(times) | {"route": short(route, p), "status": status, "bytes": size}
            report["routes"].append(s)
            flag = "  OVER" if s["p95"] > a.budget else ""
            if flag:
                over.append(s["route"])
            print(
                f"{s['route'][:58]:58} {status:>6} {size:>11,} {s['p50'] * 1000:6.0f}ms {s['p95'] * 1000:6.0f}ms {s['max'] * 1000:6.0f}ms{flag}",
                flush=True,
            )

        if not only:
            polls = ui_polls(p)
            times: dict[str, list[float]] = {r: [] for r in polls}
            end = time.time() + a.page_seconds

            def loop(route: str, every: float) -> None:
                while time.time() < end:
                    t = fetch(base, route)[0]
                    times[route].append(t)
                    time.sleep(max(0.0, every - t))

            with cf.ThreadPoolExecutor(len(polls)) as ex:
                list(ex.map(lambda kv: loop(*kv), polls.items()))
            print(f"\nUI polls together for {a.page_seconds:.0f} s:")
            for route, xs in times.items():
                s = stats(xs)
                report["page"][short(route, p)] = s
                flag = "  OVER" if s["p95"] > a.budget else ""
                if flag:
                    over.append("page " + short(route, p))
                print(
                    f"{short(route, p)[:58]:58} n={s['n']:<4} p50 {s['p50'] * 1000:6.0f}ms p95 {s['p95'] * 1000:6.0f}ms max {s['max'] * 1000:6.0f}ms{flag}",
                    flush=True,
                )
        print(f"\nwriter: {writer.rows} rows in {writer.runs} runs")
        if writer.error or not writer.is_alive():
            raise SystemExit(f"the writer stopped: {writer.error!r}")
        if a.json:
            a.json.write_text(json.dumps(report, indent=1))
        if over:
            print(f"{len(over)} over the {a.budget * 1000:.0f} ms budget: {', '.join(over)}")
            sys.exit(1)
        print(f"every route within {a.budget * 1000:.0f} ms")
    finally:
        writer.stop.set()
        if writer.is_alive():
            writer.join(timeout=10)
        server.terminate()
        try:
            server.wait(timeout=30)
        except subprocess.TimeoutExpired:
            server.kill()


if __name__ == "__main__":
    main()
