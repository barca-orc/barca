"""Reproduce #708 with 27 files, 373 nodes, 7 groups and a GB-class sensor history.

Run from a built worktree: .venv/bin/python benchmarks/planning_pipeline/bench_status.py
The fixture is synthetic: it measures inspection, not Python execution. The default writer
commits once a minute, as in #708; --write-every 0.2 stresses continuously changing history.
Every server, writer and fixture is stopped/removed before exit, including on failure.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import signal
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request
from pathlib import Path


def request(base: str, route: str):
    started = time.monotonic()
    with urllib.request.urlopen(base + route, timeout=180) as response:
        body = json.load(response)
    return body, time.monotonic() - started


def stop(server: subprocess.Popen):
    if server.poll() is None:
        os.killpg(server.pid, signal.SIGCONT)
        os.killpg(server.pid, signal.SIGTERM)
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            os.killpg(server.pid, signal.SIGKILL)
            server.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--barca", default=str(Path(".venv/bin/barca").resolve()))
    parser.add_argument("--rows", type=int, default=500_000)
    parser.add_argument("--padding", type=int, default=1900)
    parser.add_argument("--write-every", type=float, default=60)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="barca-status-708-") as tmp:
        root = Path(tmp)
        for file in range(27):
            names = [f"node_{file}_{n}" for n in range(14 if file < 22 else 13)]
            lines = ["from barca import asset, sensor, group"]
            for n, name in enumerate(names):
                lines += [f"@{'sensor' if n == 0 else 'asset'}()", f"def {name}():", "    return 1"]
            if file < 7:
                lines.append(
                    f"g = group('Group {file}', members=[{', '.join(names)}], output={names[-1]})"
                )
            (root / f"pipeline_{file:02}.py").write_text("\n".join(lines) + "\n")
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        env = {**os.environ, "BARCA_REMOTE": "off", "BARCA_CACHE_MAX_SIZE": "off"}
        env.pop("BARCA_SERVER", None)
        env.pop("BARCA_SERVER_TOKEN", None)

        def start(read_only: bool):
            log = (root / "server.log").open("a")
            try:
                server = subprocess.Popen(
                    [
                        args.barca,
                        "serve",
                        "--port",
                        str(port),
                        "--no-schedule",
                        *(["--read-only"] if read_only else []),
                    ],
                    cwd=root,
                    env=env,
                    stdout=log,
                    stderr=log,
                    start_new_session=True,
                )
            finally:
                log.close()
            try:
                deadline = time.monotonic() + 90
                while time.monotonic() < deadline:
                    if server.poll() is not None:
                        raise RuntimeError((root / "server.log").read_text()[-4000:])
                    try:
                        request(base, "/health")
                        return server
                    except (OSError, ValueError):
                        time.sleep(0.05)
                raise TimeoutError("server did not start")
            except BaseException:
                stop(server)
                raise

        (root / ".barca").mkdir()
        (root / ".barca/metadata.db").touch()
        server = start(False)
        try:
            assets, _ = request(base, "/assets")
            health, _ = request(base, "/health")
            assert not health["load_errors"], health["load_errors"]
            assert len(assets) == 373, len(assets)
        finally:
            stop(server)
        database = next(root.rglob("metadata.db"))
        sensor = next(node["id"] for node in assets if node["kind"] == "sensor")
        with sqlite3.connect(database) as conn:
            conn.execute(
                "WITH RECURSIVE history(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM history WHERE n < ?) "
                "INSERT INTO materializations(node_id, run_hash, output_hash, artifact_path, "
                "artifact_format, error_traceback, elapsed_seconds, evicted_at) "
                "SELECT ?, 'run', 'hash', 'missing.json', 'json', ?, 0.01, 'yesterday' FROM history",
                (args.rows, sensor, "x" * args.padding),
            )
            conn.commit()
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        db_bytes = database.stat().st_size
        done = threading.Event()
        errors = []

        def write():
            try:
                with sqlite3.connect(database) as conn:
                    while not done.is_set():
                        conn.execute(
                            "INSERT INTO materializations(node_id, output_hash, artifact_path, "
                            "artifact_format, elapsed_seconds) VALUES (?, 'hash', 'missing.json', 'json', 0.01)",
                            (sensor,),
                        )
                        conn.commit()
                        done.wait(args.write_every)
            except BaseException as error:
                errors.append(error)

        writer = threading.Thread(target=write)
        writer.start()
        try:
            server = start(True)
            try:
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    status = pool.submit(request, base, "/state")
                    latencies = {route: [] for route in ("/health", "/assets", "/groups")}
                    while not status.done():
                        for route, samples in latencies.items():
                            _, elapsed = request(base, route)
                            samples.append(elapsed)
                        time.sleep(0.02)
                    nodes, elapsed = status.result()
                result = {
                    "nodes": len(nodes),
                    "files": 27,
                    "groups": 7,
                    "history_rows": args.rows,
                    "db_bytes": db_bytes,
                    "writer_interval_seconds": args.write_every,
                    "state_seconds": round(elapsed, 4),
                    "concurrent_max_seconds": {
                        route: round(max(samples, default=0), 4)
                        for route, samples in latencies.items()
                    },
                    "concurrent_samples": {
                        route: len(samples) for route, samples in latencies.items()
                    },
                }
                print(json.dumps(result, indent=2))
                assert len(nodes) == 373, result
                assert elapsed < 10, result
                assert all(samples and max(samples) < 1 for samples in latencies.values()), result
                assert not errors, errors
            finally:
                stop(server)
        finally:
            done.set()
            writer.join(timeout=10)
            assert not writer.is_alive(), "writer did not stop"


if __name__ == "__main__":
    main()
