"""Time every read route of `barca serve` and the matching CLI commands on a project.

    python benchmarks/read_routes/bench.py PROJECT [--barca PATH] [--rounds 5] [--json OUT]

Starts `barca serve --no-schedule` in PROJECT on a free port, times each route's first
request (cold: the first request after start-up, with nothing cached in the server) and the
median of `--rounds` more (warm), then times the CLI commands, locally and with `--server`
against that server. Prints a Markdown table; `--json` also writes the numbers.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def fetch(base: str, path: str, headers: dict | None = None) -> tuple[float, int, int, dict]:
    req = urllib.request.Request(base + path, headers=headers or {})
    t = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=600) as r:
            body = r.read()
            status = r.status
            hdrs = dict(r.headers)
    except urllib.error.HTTPError as e:
        body = e.read()
        status = e.code
        hdrs = dict(e.headers)
    return time.perf_counter() - t, status, len(body), hdrs


def cli(barca: str, args: list[str], cwd: Path) -> float:
    t = time.perf_counter()
    p = subprocess.run([barca, *args], check=False, cwd=cwd, capture_output=True)
    elapsed = time.perf_counter() - t
    if p.returncode not in (0, 1):
        sys.stderr.write(
            f"barca {' '.join(args)} exited {p.returncode}: {p.stderr.decode()[-500:]}\n"
        )
    return elapsed


def fmt(s: float) -> str:
    return f"{s * 1000:.0f} ms" if s < 10 else f"{s:.1f} s"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("project", type=Path)
    ap.add_argument(
        "--barca",
        default=os.environ.get("BARCA") or shutil.which("barca") or "barca",
        help="the barca binary (default: $BARCA, else barca on PATH)",
    )
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--json", type=Path)
    ap.add_argument("--asset", default="retail_056", help="an asset for /stats and /assets/{name}")
    ap.add_argument("--partitioned", default="retailer_sales")
    ap.add_argument("--no-cli", action="store_true")
    ap.add_argument(
        "--settle",
        type=float,
        default=0,
        help="seconds to wait after the server is up before the first request",
    )
    ap.add_argument(
        "--only",
        help="comma-separated routes and commands to time, e.g. /state,/nodes,status",
    )
    a = ap.parse_args()
    project = a.project.resolve()

    port = free_port()
    base = f"http://127.0.0.1:{port}"
    # The log goes beside the project: a file created in it would count as a source change.
    log = (project.parent / "serve.log").open("w")
    server = subprocess.Popen(
        [a.barca, "serve", "--no-schedule", "--port", str(port)],
        cwd=project,
        stdout=subprocess.DEVNULL,
        stderr=log,
        start_new_session=True,
    )
    results: list[dict] = []
    try:
        deadline = time.time() + 600
        while True:
            try:
                fetch(base, "/health")
                break
            except OSError:
                if time.time() > deadline or server.poll() is not None:
                    raise SystemExit(f"barca serve did not start; see {log.name}")
                time.sleep(0.1)
        time.sleep(a.settle)
        _, _, _, _ = fetch(base, "/health")
        runs = json.loads(urllib.request.urlopen(base + "/runs?limit=1").read())
        run_id = (runs.get("runs") or [{}])[0].get("run_id", "none")
        routes = [
            "/health",
            "/project",
            "/state",
            "/assets",
            "/groups",
            "/schedule",
            "/runs?limit=100",
            "/plan",
            "/list",
            "/nodes",
            "/history?limit=20",
            "/history",
            f"/stats/{a.asset}",
            f"/stats/{a.partitioned}",
            f"/assets/{a.asset}",
            f"/assets/{a.partitioned}",
            f"/assets/{a.asset}/schema",
            f"/runs/{run_id}",
            f"/runs/{run_id}/logs",
            f"/logs/{run_id}",
        ]
        only = set(a.only.split(",")) if a.only else None
        for route in routes:
            if only and route not in only:
                continue
            cold, status, size, hdrs = fetch(base, route)
            # A route that takes seconds is timed warm once, not `--rounds` times.
            warm = [fetch(base, route)[0] for _ in range(a.rounds if cold < 5 else 1)]
            row = {
                "route": route.replace(run_id, "{id}"),
                "status": status,
                "bytes": size,
                "cold_s": cold,
                "warm_s": statistics.median(warm),
            }
            etag = hdrs.get("ETag") or hdrs.get("etag")
            if etag:
                row["not_modified_s"] = statistics.median(
                    fetch(base, route, {"If-None-Match": etag})[0] for _ in range(a.rounds)
                )
            results.append(row)
            print(
                f"{row['route']:40} {status} {size:>10} B  cold {fmt(cold):>8}  warm {fmt(row['warm_s']):>8}"
                + (f"  304 {fmt(row['not_modified_s'])}" if "not_modified_s" in row else ""),
                flush=True,
            )

        if not only or "after-run" in only:
            # A run finishes while the UI is open: the next poll right away, and one a few
            # seconds later (what the UI's 10 s poll sees).
            fetch(base, "/state")
            body = json.dumps(
                {"command": "get", "targets": [a.asset], "refresh": [a.asset]}
            ).encode()
            req = urllib.request.Request(
                base + "/runs",
                data=body,
                method="POST",
                headers={"content-type": "application/json"},
            )
            run = json.loads(urllib.request.urlopen(req, timeout=600).read())["run_id"]
            while json.loads(urllib.request.urlopen(f"{base}/runs/{run}").read())["status"] in (
                "pending",
                "running",
                "queued",
            ):
                time.sleep(0.05)
            right_after = fetch(base, "/state")[0]
            time.sleep(3)
            later = fetch(base, "/state")[0]
            results.append(
                {"route": "/state after a run", "right_after_s": right_after, "after_3s": later}
            )
            print(
                f"{'/state after a run':40} right after {fmt(right_after):>8}  3 s later {fmt(later):>8}",
                flush=True,
            )

        if not a.no_cli:
            commands = [
                ["status", "--json"],
                ["status"],
                ["list", "--json"],
                ["history", "--json", "-l", "20"],
                ["stats", a.asset, "--json"],
                ["stats", a.partitioned, "--json"],
            ]
            env_server = ["--server", base]
            for cmd in commands:
                if only and cmd[0] not in only:
                    continue
                for label, args in (("local", cmd), ("--server", [cmd[0], *env_server, *cmd[1:]])):
                    times = [cli(a.barca, args, project)]
                    if times[0] < 5:
                        times += [cli(a.barca, args, project) for _ in range(max(2, a.rounds // 2))]
                    row = {
                        "command": f"barca {' '.join(cmd)}",
                        "mode": label,
                        "first_s": times[0],
                        "median_s": statistics.median(times),
                    }
                    results.append(row)
                    print(
                        f"{row['command']:40} {label:9} first {fmt(times[0]):>8}  median {fmt(row['median_s']):>8}",
                        flush=True,
                    )
    finally:
        os.killpg(server.pid, 15)
        server.wait(timeout=60)
        log.close()
    if a.json:
        a.json.write_text(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
