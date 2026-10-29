"""Measure #656 on an existing PROJECT_DIR with BARCA_BIN and Linux /proc.

Cold/cached /state, legacy /nodes, and invalidation after unchanged CLI gets.
Concurrent /health requests expose event-loop stalls. Keeps response summaries
and server logs; never mutates source files or deletes the seed database.
"""

import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import threading
import time
import urllib.request


def main():
    root = Path(os.environ["PROJECT_DIR"]).resolve()
    binary = str(Path(os.environ["BARCA_BIN"]).resolve())
    env = os.environ.copy()
    for key in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(key, None)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    done = threading.Event()
    health = []
    errors = []

    def read(path, timeout=180):
        with urllib.request.urlopen(url + path, timeout=timeout) as response:
            return response.read()

    def probe():
        while not done.is_set():
            start = time.monotonic()
            try:
                read("/health", timeout=5)
                health.append(time.monotonic() - start)
            except OSError as error:
                errors.append(str(error))
            done.wait(0.02)

    with (root / "inspection-serve.log").open("w") as log:
        server = subprocess.Popen(
            [binary, "serve", "pipeline.py", "--port", str(port), "--no-schedule"],
            cwd=root,
            env=env,
            stdout=log,
            stderr=log,
            start_new_session=True,
        )
        sampler = None
        try:
            deadline = time.monotonic() + 60
            while True:
                assert server.poll() is None, (root / "inspection-serve.log").read_text()
                try:
                    read("/health")
                    break
                except OSError:
                    assert time.monotonic() < deadline
                    time.sleep(0.005)
            sampler = threading.Thread(target=probe)
            sampler.start()

            def measure(case, path):
                before = len(health)
                start = time.monotonic()
                data = read(path)
                elapsed = time.monotonic() - start
                status = Path(f"/proc/{server.pid}/status").read_text()
                row = {
                    "case": case,
                    "route": path,
                    "seconds": round(elapsed, 6),
                    "response_bytes": len(data),
                    "rss_kib": int(re.search(r"VmRSS:\s+(\d+)", status)[1]),
                    "peak_kib": int(re.search(r"VmHWM:\s+(\d+)", status)[1]),
                    "health_samples_during_request": len(health) - before,
                }
                print(json.dumps(row), flush=True)
                # Validate JSON, not just successful HTTP status.
                return json.loads(data)

            measure("cold", "/state")
            measure("immediate_repeat", "/state")
            measure("legacy_cold", "/nodes?sample=0")
            measure("legacy_immediate_repeat", "/nodes?sample=0")
            for index in range(int(os.environ.get("REPEATS", "5"))):
                result = subprocess.run(
                    [binary, "get", "pipeline.py", "--json"],
                    cwd=root,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=600,
                )
                assert result.returncode == 0, result.stdout + result.stderr
                document = json.loads(result.stdout)
                assert document["steps_executed"] <= 1, document
                measure(f"invalidated_{index + 1}", "/state")
                measure(f"immediate_repeat_{index + 1}", "/state")
            time.sleep(5)
            measure("after_5s_quiet", "/state")
            measure("quiet_immediate_repeat", "/state")
            print(
                json.dumps(
                    {
                        "health_samples": len(health),
                        "health_max_seconds": round(max(health), 6) if health else None,
                        "health_errors": errors,
                    }
                ),
                flush=True,
            )
        finally:
            done.set()
            if sampler:
                sampler.join(timeout=6)
            os.killpg(server.pid, signal.SIGCONT)
            server.terminate()
            try:
                server.wait(timeout=35)
            except subprocess.TimeoutExpired:
                os.killpg(server.pid, signal.SIGKILL)
                server.wait()


if __name__ == "__main__":
    main()
