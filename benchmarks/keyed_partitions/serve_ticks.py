"""Scheduled `barca serve` ticks over the keyed-partition fixture: what each tick keeps (#623).

    python benchmarks/keyed_partitions/serve_ticks.py N TICKS [--barca PATH] [--payloads]
    python benchmarks/keyed_partitions/serve_ticks.py 200000 50 --payloads --cron "*/10 * * * * *" --idle 2

Makes `generate.py`'s fixture with `freshness=Schedule(CRON)` on the sensor in a new directory
under the current one (a tmpfs /tmp runs out of inodes at 200k keys), computes it once with
`barca get`, starts `barca serve`, and then TICKS times changes one retailer's version and waits
for the next scheduled run to finish. Per tick it prints the run's `elapsed_seconds`, the size of
`.barca/metadata.db*` and the server's RSS (Linux: /proc); at the end the outcome bytes and
materialization rows per run (read from the DB after the server stopped), the DB growth from
before the server started to after it stopped, and with --payloads the size of
`GET /runs/{id}`, `GET /runs/{id}/events`, `barca history RUN_ID --json` and `barca get --json`.
The fixture is deleted unless --keep.
"""

import argparse
import json
import os
import pathlib
import shutil
import signal
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ap = argparse.ArgumentParser()
ap.add_argument("n", type=int, help="retailers (partition keys per asset)")
ap.add_argument("ticks", type=int)
ap.add_argument("--barca", default=shutil.which("barca") or "barca")
ap.add_argument("--port", type=int, default=8391)
ap.add_argument(
    "--cron", default="*/10 * * * * *", help="the sensor's schedule (6 fields: seconds)"
)
ap.add_argument("--mode", default="keyed", choices=["keyed", "partitioned"])
ap.add_argument(
    "--idle", type=float, default=0.0, help="seconds to wait after a tick before sampling"
)
ap.add_argument("--payloads", action="store_true")
ap.add_argument("--keep", action="store_true")
a = ap.parse_args()

d = pathlib.Path(tempfile.mkdtemp(prefix="serve-ticks-", dir=os.getcwd()))
gen = pathlib.Path(__file__).with_name("generate.py")
subprocess.run([sys.executable, gen, str(a.n), str(d), a.mode, a.cron], check=True)
env = dict(os.environ, BARCA_REMOTE="off")
base = f"http://127.0.0.1:{a.port}"


def get(path: str, tries: int = 5) -> bytes:
    for attempt in range(tries):
        try:
            with urllib.request.urlopen(base + path, timeout=600) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            print(f"  GET {path}: HTTP {e.code} {e.read()[:200]!r}", flush=True)
            if attempt == tries - 1:
                raise
            time.sleep(0.5)
    raise AssertionError("unreachable")


def db_bytes() -> int:
    return sum(f.stat().st_size for f in (d / ".barca").glob("metadata.db*"))


def rss_mb(pid: int) -> int:
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("VmRSS"):
            return int(line.split()[1]) // 1024
    return -1


started = time.time()
subprocess.run(
    [a.barca, "get", "scored", "pipeline.py"], cwd=d, env=env, check=True, capture_output=True
)
print(f"cold get: {time.time() - started:.1f}s", flush=True)
db_before = db_bytes()
log = open(d / "serve.log", "w")
server = subprocess.Popen(
    [a.barca, "serve", "pipeline.py", "--port", str(a.port)], cwd=d, env=env, stdout=log, stderr=log
)
measured: list[str] = []
rss: list[int] = []
elapsed: list[float] = []
try:
    for _ in range(300):
        try:
            get("/health")
            break
        except OSError:
            time.sleep(0.2)
    seen: set[str] = set()
    print("tick  run_id        status   elapsed_s   db_MB   rss_MB", flush=True)
    for i in range(a.ticks):
        subprocess.run([sys.executable, "bump.py", f"r{i % a.n:06d}"], cwd=d, check=True)
        while True:
            runs = json.loads(get("/runs?limit=5"))["runs"]
            done = [
                r
                for r in runs
                if r["id"] not in seen
                and r["status"] not in ("pending", "running")
                and r["triggered_by"] == "schedule"
            ]
            if done:
                seen.update(r["id"] for r in done)
                run = done[0]
                break
            time.sleep(0.2)
        if a.idle:
            time.sleep(a.idle)
        rss.append(rss_mb(server.pid))
        view = json.loads(get(f"/runs/{run['id']}"))
        measured.append(run["id"])
        elapsed.append(view.get("elapsed_seconds") or 0.0)
        print(
            f"{i:4d}  {run['id']:>12}  {view['status']:8} {elapsed[-1]:9.2f} "
            f"{db_bytes() / 1e6:8.2f} {rss[-1]:8d}",
            flush=True,
        )
    if a.payloads and measured:
        last = measured[-1]
        sizes = {
            "GET /runs/{id}": len(get(f"/runs/{last}")),
            "GET /runs/{id}/events": len(get(f"/runs/{last}/events")),
        }
        for label, extra in [("--server", ["--server", base]), ("local", ["--local"])]:
            out = subprocess.run(
                [a.barca, "history", last, "--json", *extra], cwd=d, env=env, capture_output=True
            )
            sizes[f"barca history RUN_ID --json ({label})"] = len(out.stdout)
        out = subprocess.run(
            [a.barca, "get", "scored", "pipeline.py", "--json", "--local"],
            cwd=d,
            env=env,
            capture_output=True,
        )
        sizes["barca get --json (whole)"] = len(out.stdout)
        sizes["barca get --json final_output"] = len(
            json.dumps(json.loads(out.stdout)["final_output"])
        )
        print(json.dumps(sizes, indent=1), flush=True)
finally:
    server.send_signal(signal.SIGTERM)
    try:
        server.wait(120)
    except subprocess.TimeoutExpired:
        server.kill()

if measured:
    n = len(measured)
    db = sqlite3.connect(f"file:{d / '.barca/metadata.db'}?mode=ro", uri=True)
    marks = ",".join("?" * n)
    total, largest = db.execute(
        f"select sum(length(outcome_json)), max(length(outcome_json)) from runs where run_id in ({marks})",
        measured,
    ).fetchone()
    rows = db.execute(
        f"select count(*) from materializations where run_id in ({marks})", measured
    ).fetchone()[0]
    db.close()
    after = db_bytes()
    second = rss[n // 2 :]
    print(f"outcome per run: {total / n / 1e3:.1f} KB average, {largest / 1e3:.1f} KB largest")
    print(f"materialization rows per run: {rows / n:.1f}")
    print(
        f"metadata DB: {db_before / 1e6:.2f} MB -> {after / 1e6:.2f} MB, {(after - db_before) / n / 1e3:.1f} KB per tick"
    )
    print(
        f"server RSS MB: first {rss[0]}, median {statistics.median(rss)}, last {rss[-1]}; "
        f"second half {min(second)} to {max(second)}"
    )
    print(f"tick elapsed: median {statistics.median(elapsed):.2f}s, max {max(elapsed):.2f}s")
if not a.keep:
    shutil.rmtree(d)
