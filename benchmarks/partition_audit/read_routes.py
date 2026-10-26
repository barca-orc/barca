"""Read-route latency and server memory against an already seeded audit fixture."""

import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from python.tests import served


def timed(server, path):
    start = time.perf_counter()
    result = server.get(path)
    return round(time.perf_counter() - start, 4), result


def memory(pid):
    return {
        line.split(":")[0]: int(line.split()[1])
        for line in Path(f"/proc/{pid}/status").read_text().splitlines()
        if line.startswith(("VmRSS:", "VmHWM:"))
    }


if __name__ == "__main__":
    root = Path(sys.argv[1]).resolve()
    env = {k: v for k, v in os.environ.items() if not k.startswith("BARCA_")}
    env["BARCA_POOL_SIZE"] = "4"
    with served.Serve(root, "--no-schedule", env=env) as server:
        server.ready()
        for path in ("/assets", "/state", "/nodes"):
            for sample in range(4):
                seconds, result = timed(server, path)
                assert isinstance(result, (dict, list))
                print(
                    json.dumps(
                        dict(path=path, sample=sample, seconds=seconds, **memory(server.proc.pid))
                    ),
                    flush=True,
                )
        for sample in range(5):
            run = server.post("/runs", {"command": "get", "targets": ["scored"]})["run_id"]
            assert server.finished(run)["status"] == "success"
            seconds, _ = timed(server, "/state")
            print(
                json.dumps(
                    dict(
                        case="after_unchanged_run",
                        sample=sample,
                        seconds=seconds,
                        **memory(server.proc.pid),
                    )
                ),
                flush=True,
            )
        # Start a cold status computation on a fresh server graph stamp, while probing health.
        pipeline = root / "pipeline.py"
        pipeline.write_text(pipeline.read_text() + "\n# inspection invalidation\n")
        with ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(timed, server, "/state")
            seconds, result = timed(server, "/health")
            print(
                json.dumps(
                    dict(case="health_during_status", seconds=seconds, **memory(server.proc.pid))
                ),
                flush=True,
            )
            pending.result(timeout=120)
