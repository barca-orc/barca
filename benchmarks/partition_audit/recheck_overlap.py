"""Recheck #654 against BARCA_BIN; retain evidence under PROJECTS_DIR.

Two clients request the same uncached asset while it is still executing. Tasks
provide the control: two explicitly requested task executions must remain two.
This reports observed behavior, rather than asserting that deduplication exists.
"""

import concurrent.futures
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.request


def main():
    binary = str(Path(os.environ["BARCA_BIN"]).resolve())
    root = Path(os.environ["PROJECTS_DIR"]).resolve()
    root.mkdir(parents=True, exist_ok=False)
    (root / "barca.toml").write_text("")
    (root / "pipeline.py").write_text("""from barca import asset, task
import os
import time

def record(kind):
    fd = os.open("calls.txt", os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, (kind + "\\n").encode())
    finally:
        os.close(fd)
    time.sleep(2.5)
    return 42

@asset()
def shared():
    return record("asset")

@task()
def explicit():
    return record("task")
""")
    env = os.environ.copy()
    for key in ("BARCA_SERVER", "BARCA_REMOTE_URI", "BARCA_ENV", "BARCA_OUTPUT"):
        env.pop(key, None)
    env["BARCA_POOL_SIZE"] = "2"
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    with (root / "serve.log").open("w") as log:
        server = subprocess.Popen(
            [
                binary,
                "serve",
                "pipeline.py",
                "--port",
                str(port),
                "--no-schedule",
                "--max-runs",
                "2",
            ],
            cwd=root,
            env=env,
            stdout=log,
            stderr=log,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 30
            while True:
                assert server.poll() is None, (root / "serve.log").read_text()
                try:
                    with urllib.request.urlopen(url + "/health", timeout=1):
                        break
                except OSError:
                    assert time.monotonic() < deadline, "server did not start"
                    time.sleep(0.05)
            for kind, command, target in (("asset", "get", "shared"), ("task", "run", "explicit")):

                def request():
                    return subprocess.run(
                        [binary, command, target, "--server", url, "--json"],
                        cwd=root,
                        env=env,
                        capture_output=True,
                        text=True,
                        timeout=45,
                    )

                with concurrent.futures.ThreadPoolExecutor(2) as pool:
                    futures = [pool.submit(request) for _ in range(2)]
                    results = [future.result() for future in futures]
                for result in results:
                    assert result.returncode == 0, result.stdout + result.stderr
                calls = (root / "calls.txt").read_text().splitlines().count(kind)
                documents = [json.loads(result.stdout) for result in results]
                with sqlite3.connect(root / ".barca/metadata.db") as db:
                    identities = db.execute(
                        "SELECT node_id, run_hash, COUNT(*) FROM materializations "
                        "WHERE node_id LIKE ? GROUP BY node_id, run_hash",
                        ("%" + target,),
                    ).fetchall()
                print(
                    json.dumps(
                        {
                            "kind": kind,
                            "executions": calls,
                            "expected_executions": 1 if kind == "asset" else 2,
                            "runs": documents,
                            "materializations": identities,
                        }
                    ),
                    flush=True,
                )
        finally:
            os.killpg(server.pid, signal.SIGCONT)
            server.terminate()
            try:
                server.wait(timeout=35)
            except subprocess.TimeoutExpired:
                os.killpg(server.pid, signal.SIGKILL)
                server.wait()


if __name__ == "__main__":
    main()
