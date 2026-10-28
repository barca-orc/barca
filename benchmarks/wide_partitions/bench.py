"""Time `barca status` and the read routes of `barca serve` on the wide-partitions fixture.

    python benchmarks/wide_partitions/bench.py DIR [--json results.json] [--run pipe_09.py]

Measures `barca status --json` (twice: the second with a warm page cache), the first and a
repeated `GET /state` of a fresh server, and the slowest answer of `GET /state`, `GET /runs` and
`GET /runs/{run_id}` while a server run of `--run` (a file nothing has run yet) executes,
polled as the web UI polls them (every 2 s, 2 s and 1 s). Reads only the network to localhost.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import threading
import time
import urllib.request
from pathlib import Path

PORT = int(os.environ.get("BENCH_PORT", "8399"))
BASE = f"http://127.0.0.1:{PORT}"


def timed(fn):
    start = time.perf_counter()
    value = fn()
    return time.perf_counter() - start, value


def get(path: str):
    with urllib.request.urlopen(BASE + path, timeout=600) as response:
        return json.loads(response.read())


def poll(path: str, every: float, until: threading.Event, worst: dict) -> None:
    while not until.is_set():
        seconds, _ = timed(lambda: get(path))
        key = path.split("?")[0] if "/runs/" not in path else "/runs/{run_id}"
        worst[key] = max(worst.get(key, 0.0), seconds)
        worst[key + " answers"] = worst.get(key + " answers", 0) + 1
        until.wait(every)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dir", type=Path)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--run", default="pipe_09.py", help="a file nothing has run yet")
    args = parser.parse_args()
    barca = shutil.which("barca") or "barca"
    results: dict[str, float | int] = {}

    for label in ("status cold", "status warm"):
        seconds, _ = timed(
            lambda: subprocess.run(
                [barca, "status", "--json"], cwd=args.dir, check=True, capture_output=True
            )
        )
        results[label] = round(seconds, 2)

    server = subprocess.Popen(
        [barca, "serve", "--no-schedule", "--port", str(PORT)],
        cwd=args.dir,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        # A project whose cache was never surveyed (`.barca/gc-state.json`) is surveyed first,
        # which walks every artifact: minutes on a cold cache. Measure after it, not during.
        for _ in range(9000):
            try:
                get("/health")
                break
            except OSError:
                time.sleep(0.2)
        results["state first"] = round(timed(lambda: get("/state"))[0], 2)
        results["state warm"] = round(timed(lambda: get("/state"))[0], 3)

        request = urllib.request.Request(
            BASE + "/runs",
            data=json.dumps({"command": "get", "targets": [], "files": [args.run]}).encode(),
            headers={"content-type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request) as response:
            run_id = json.loads(response.read())["run_id"]
        done, worst = threading.Event(), {}
        pollers = [
            threading.Thread(target=poll, args=(path, every, done, worst))
            for path, every in [
                ("/state", 2.0),
                ("/runs?limit=100", 2.0),
                (f"/runs/{run_id}", 1.0),
            ]
        ]
        started = time.perf_counter()
        for t in pollers:
            t.start()
        while get(f"/runs/{run_id}")["status"] in ("queued", "running"):
            time.sleep(0.5)
        results["run seconds"] = round(time.perf_counter() - started, 1)
        done.set()
        for t in pollers:
            t.join()
        results.update(
            {
                f"during run, slowest {k}" if "answers" not in k else f"during run, {k}": (
                    round(v, 2) if isinstance(v, float) else v
                )
                for k, v in sorted(worst.items())
            }
        )
    finally:
        server.terminate()
        server.wait(timeout=60)

    width = max(len(k) for k in results)
    for key, value in results.items():
        print(f"{key:<{width}}  {value}")
    if args.json:
        args.json.write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
