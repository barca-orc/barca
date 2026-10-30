"""Loopback request/reconnect cost; credentials stay in memory, never in result JSON."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import time
import urllib.request


def summarize(samples):
    samples = sorted(samples)
    return {
        "samples": len(samples),
        "p50_us": samples[len(samples) // 2],
        "p95_us": samples[int(len(samples) * 0.95)],
        "max_us": samples[-1],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=200)
    parser.add_argument("--tokens", type=int, default=1000)
    args = parser.parse_args()
    if args.samples < 1 or not 0 <= args.tokens <= 1000:
        parser.error("samples must be positive and tokens must be between 0 and 1000")
    binary = args.binary.resolve()
    base = args.workdir.resolve()
    base.mkdir(parents=True, exist_ok=False)
    pipeline = (
        "from barca import task\n@task()\ndef slow():\n    import time\n    time.sleep(120)\n"
    )
    env = {key: value for key, value in os.environ.items() if not key.startswith("BARCA_")}
    env["PYTHONPATH"] = str(Path(__file__).resolve().parents[2] / "python")
    result = {
        "platform": platform.platform(),
        "binary_sha256": hashlib.file_digest(binary.open("rb"), "sha256").hexdigest(),
        "transport": "urllib loopback TCP, new connection per request",
        "modes": {},
    }
    for mode in ("off", "token"):
        root = base / mode
        root.mkdir()
        (root / "pipeline.py").write_text(pipeline)
        (root / "barca.toml").write_text("")
        run_id = None
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        url = f"http://127.0.0.1:{port}"
        token = None

        def request(path, body=None, headers=None, method=None):
            merged = {"Authorization": f"Bearer {token}"} if token else {}
            merged.update(headers or {})
            if body is not None:
                merged["Content-Type"] = "application/json"
            return urllib.request.urlopen(
                urllib.request.Request(
                    url + path,
                    data=json.dumps(body).encode() if body is not None else None,
                    headers=merged,
                    method=method,
                ),
                timeout=5,
            )

        with (root / f"{mode}.log").open("w") as log:
            proc = subprocess.Popen(
                [
                    str(binary),
                    "serve",
                    "pipeline.py",
                    "--port",
                    str(port),
                    "--no-schedule",
                    "--auth",
                    mode,
                ],
                cwd=root,
                env=env,
                stdout=log,
                stderr=log,
            )
            try:
                deadline = time.monotonic() + 20
                while True:
                    assert proc.poll() is None, "server exited"
                    try:
                        with request("/auth/status") as response:
                            assert response.status == 200
                        break
                    except OSError:
                        assert time.monotonic() < deadline, "server startup timeout"
                        time.sleep(0.05)
                if mode == "token":
                    code = subprocess.check_output(
                        [str(binary), "tokens", "bootstrap"], cwd=root, env=env, text=True
                    ).strip()
                    with request("/auth/bootstrap", {"code": code}) as response:
                        token = json.load(response)["token"]
                    del code
                    for index in range(args.tokens):
                        with request("/auth/tokens", {"name": f"bench-{index}"}) as response:
                            issued = json.load(response)
                    if args.tokens:
                        token = issued["token"]
                with request("/health") as response:
                    assert json.load(response)["capabilities"]["event_resume"] == 1
                state = root / ".barca/auth/state.json"
                before = (state.stat().st_mtime_ns, state.read_bytes()) if mode == "token" else None
                samples = []
                for _ in range(args.samples):
                    started = time.perf_counter_ns()
                    with request("/health") as response:
                        assert response.status == 200
                        response.read()
                    samples.append((time.perf_counter_ns() - started) / 1000)
                with request("/runs", {"command": "run", "targets": ["slow"]}) as response:
                    run_id = json.load(response)["run_id"]
                path = f"/runs/{run_id}/events"
                with request(path) as response:
                    while line := response.readline():
                        if line.startswith(b"id:"):
                            cursor = line.decode().split(":", 1)[1].strip()
                            break
                    else:
                        raise AssertionError("no event cursor")
                reconnects = []
                for _ in range(args.samples):
                    started = time.perf_counter_ns()
                    with request(path, headers={"Last-Event-ID": cursor}) as response:
                        assert response.status == 200
                    reconnects.append((time.perf_counter_ns() - started) / 1000)
                unchanged = (
                    before == (state.stat().st_mtime_ns, state.read_bytes()) if before else True
                )
                assert unchanged, "read requests wrote token state"
                result["modes"][mode] = {
                    "health": summarize(samples),
                    "reconnect_headers": summarize(reconnects),
                    "token_state_unchanged": unchanged,
                    "ordinary_token_records": args.tokens if mode == "token" else 0,
                }
            finally:
                if run_id is not None and proc.poll() is None:
                    try:
                        with request(f"/runs/{run_id}", method="DELETE") as response:
                            response.read()
                    except OSError:
                        pass
                proc.terminate()
                try:
                    proc.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
