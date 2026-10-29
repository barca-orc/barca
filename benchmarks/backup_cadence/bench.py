"""Measure routine admission and final whole-DB backup cost on synthetic metadata padding.

Run inside a worktree: .venv/bin/python benchmarks/backup_cadence/bench.py
--binary .venv/bin/barca --workdir target-local/backup-cost --bytes 1700000000
The work directory must not exist. It retains raw logs and copies until worktree teardown.
"""

import argparse
import json
import os
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path


def request(port, method, route):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{route}", method=method)
    with urllib.request.urlopen(req, timeout=30) as response:
        return json.loads(response.read())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--bytes", type=int, default=256_000_000)
    args = parser.parse_args()
    binary = args.binary.resolve()
    root = args.workdir.resolve()
    root.mkdir(parents=True, exist_ok=False)
    store = root / "store"
    store.mkdir()
    (root / "pipeline.py").write_text(
        "from barca import task\n@task()\ndef tick():\n    return 42\n"
    )
    (root / "barca.toml").write_text(
        f'[remote]\nuri = {json.dumps(str(store))}\nbackup_interval = "1h"\n'
    )
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_"))
    } | {"BARCA_TRACE_TIMING": "1", "BARCA_POOL_SIZE": "2"}
    initial = subprocess.run(
        [str(binary), "run", "tick", "pipeline.py", "--agent"],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
        check=True,
    )
    (root / "initial.stderr").write_text(initial.stderr)
    db = root / ".barca/metadata.db"
    with sqlite3.connect(db) as conn:
        conn.execute("CREATE TABLE backup_cost_padding (payload BLOB)")
        left = args.bytes
        while left > 0:
            chunk = min(left, 64_000_000)
            conn.execute("INSERT INTO backup_cost_padding VALUES (zeroblob(?))", (chunk,))
            left -= chunk
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    log = root / "server.stderr"
    with log.open("w") as stderr:
        server = subprocess.Popen(
            [str(binary), "serve", "pipeline.py", "--port", str(port), "--no-schedule"],
            cwd=root,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=stderr,
        )
    try:
        deadline = time.monotonic() + 60
        while True:
            if server.poll() is not None:
                raise RuntimeError(log.read_text())
            try:
                request(port, "GET", "/health")
                break
            except (OSError, urllib.error.URLError):
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.01)
        times = []
        for _ in range(3):
            started = time.monotonic()
            run_id = request(port, "POST", "/run/tick")["run_id"]
            while True:
                view = request(port, "GET", f"/runs/{run_id}")
                if view["status"] not in ("queued", "running", "pending"):
                    assert view["status"] == "success", view
                    break
                if time.monotonic() - started > 60:
                    raise TimeoutError("served run")
                time.sleep(0.005)
            times.append(time.monotonic() - started)
        routine_log = log.read_text()
        assert "metadata backup hash:" not in routine_log, routine_log
        assert "metadata backup copy:" not in routine_log, routine_log
        assert "metadata backup upload:" not in routine_log, routine_log
        started = time.monotonic()
        server.send_signal(signal.SIGTERM)
        assert server.wait(timeout=60) == 0, log.read_text()
        shutdown_seconds = time.monotonic() - started
        full_log = log.read_text()
        assert "metadata backup upload:" in full_log and "confirmed=true" in full_log
        copies = sorted((store / "default/state/backups").glob("*.db"))
        with sqlite3.connect(copies[-1]) as conn:
            assert (
                conn.execute("SELECT count(*) FROM runs WHERE status='success'").fetchone()[0] == 4
            )
        result = {
            "synthetic_padding_bytes": args.bytes,
            "database_bytes": db.stat().st_size,
            "profile": "fast (optimized, no fat LTO)",
            "backend": "local directory, same filesystem",
            "routine_served_run_seconds": times,
            "not_due_hash_copy_upload_calls": 0,
            "shutdown_seconds": shutdown_seconds,
            "newest_backup_bytes": copies[-1].stat().st_size,
            "timing_lines": [line for line in full_log.splitlines() if "metadata backup " in line],
        }
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
    finally:
        if server.poll() is None:
            server.send_signal(signal.SIGCONT)
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=10)


if __name__ == "__main__":
    main()
