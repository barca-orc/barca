"""Compare real status and invalidated /state against unchanged GB-class artifact copies."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import sqlite3
import subprocess
import time
import urllib.request

import pyarrow as pa
import pyarrow.parquet as pq


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--megabytes", type=int, default=1024)
    args = parser.parse_args()
    if args.megabytes < 1:
        parser.error("megabytes must be positive")
    base = args.workdir.resolve()
    base.mkdir(parents=True, exist_ok=False)
    audit = base / "audit"
    audit.mkdir()
    traces = base / "hashes.jsonl"
    (audit / "sitecustomize.py").write_text(
        "import json, time\nfrom pathlib import Path\nfrom barca import _storage\n"
        "original = _storage.sha256_file\n"
        "def sha(path):\n"
        "    started = time.perf_counter()\n    value = original(path)\n"
        "    if str(path).endswith('.parquet'):\n"
        f"        with open({str(traces)!r}, 'a') as out:\n"
        "            out.write(json.dumps({'path': str(path), 'bytes': Path(path).stat().st_size, 'seconds': time.perf_counter() - started}) + '\\n')\n"
        "    return value\n_storage.sha256_file = sha\n"
    )
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_"))
    }
    env |= {
        "PYTHONPATH": str(audit) + os.pathsep + str(Path(__file__).resolve().parents[2] / "python"),
        "BARCA_POOL_SIZE": "2",
    }
    source_root = Path(__file__).resolve().parents[2]
    source_files = [
        "crates/barca-core/src/status.rs",
        "python/barca/_inspection_cache.py",
        "python/barca/_state.py",
        "python/barca/_inspect.py",
    ]
    result = {
        "started_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "pyarrow": pa.__version__,
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "cpu_model": next(
            (
                line.split(":", 1)[1].strip()
                for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")
            ),
            platform.processor(),
        ),
        "affinity": sorted(os.sched_getaffinity(0)),
        "load_before": os.getloadavg(),
        "worker_pool": 2,
        "profile": "fast",
        "warm_samples": 3,
        "invalidated_samples": 3,
        "page_cache": "recently written synthetic parquet; no cold-filesystem claim",
        "source_sha256": {
            name: hashlib.sha256((source_root / name).read_bytes()).hexdigest()
            for name in source_files
        },
        "modes": {},
    }
    for label, supplied in [("before", args.before), ("after", args.after)]:
        binary = supplied.resolve()
        root = base / label
        root.mkdir()
        store = root / "store"
        (root / "barca.toml").write_text(
            f'[remote]\nuri = {json.dumps(str(store))}\nbackup_interval = "off"\n'
        )
        (root / "pipeline.py").write_text(
            "from barca import asset\n@asset()\ndef value():\n    import pyarrow as pa\n    return pa.table({'value': [42]})\n"
        )

        def cli():
            started = time.perf_counter()
            proc = subprocess.run(
                [str(binary), "status", "pipeline.py", "--json"],
                cwd=root,
                env=env,
                text=True,
                capture_output=True,
                timeout=60,
                check=True,
            )
            body = json.loads(proc.stdout)
            assert body["nodes"][0]["cache"]["state"] == "cached"
            return time.perf_counter() - started

        subprocess.run(
            [str(binary), "get", "value", "pipeline.py", "--json"],
            cwd=root,
            env=env,
            capture_output=True,
            check=True,
            timeout=60,
        )
        local = next((root / ".barca/artifacts").rglob("*.parquet"))
        remote = next(store.rglob("*.parquet"))
        table = pa.table({"value": pa.array(range(65536), type=pa.int64())})
        with pq.ParquetWriter(
            local, table.schema, compression="NONE", use_dictionary=False
        ) as writer:
            for _ in range(args.megabytes * 2):
                writer.write_table(table)
        shutil.copyfile(local, remote)
        with local.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        database = root / ".barca/metadata.db"
        with sqlite3.connect(database) as connection:
            connection.execute(
                "UPDATE materializations SET output_hash = ?, artifact_size_bytes = ? WHERE status = 'success'",
                (digest, local.stat().st_size),
            )
        traces.write_text("")
        cold = cli()
        cold_hashes = [json.loads(line) for line in traces.read_text().splitlines()]
        traces.write_text("")
        warm = [cli() for _ in range(3)]
        warm_hashes = [json.loads(line) for line in traces.read_text().splitlines()]
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        with (root / "server.log").open("w") as log:
            proc = subprocess.Popen(
                [
                    str(binary),
                    "serve",
                    "pipeline.py",
                    "--read-only",
                    "--no-schedule",
                    "--port",
                    str(port),
                ],
                cwd=root,
                env=env,
                stdout=log,
                stderr=log,
            )
            try:

                def request(path):
                    with urllib.request.urlopen(
                        f"http://127.0.0.1:{port}{path}", timeout=60
                    ) as response:
                        return json.load(response)

                deadline = time.monotonic() + 60
                while True:
                    assert proc.poll() is None
                    try:
                        request("/state")
                        break
                    except OSError:
                        assert time.monotonic() < deadline
                        time.sleep(0.02)
                traces.write_text("")
                invalidated = []
                for index in range(3):
                    elapsed = 100.25 + index
                    with sqlite3.connect(database) as connection:
                        connection.execute(
                            "UPDATE materializations SET elapsed_seconds = ? WHERE status = 'success'",
                            (elapsed,),
                        )
                    started = time.perf_counter()
                    while (
                        request("/state")[0]["last_materialization"]["elapsed_seconds"] != elapsed
                    ):
                        assert time.perf_counter() - started < 60
                        time.sleep(0.01)
                    invalidated.append(time.perf_counter() - started)
                state_hashes = [json.loads(line) for line in traces.read_text().splitlines()]
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
        if label == "after":
            assert not warm_hashes, warm_hashes
            assert not state_hashes, state_hashes
        with binary.open("rb") as source:
            fingerprint = hashlib.file_digest(source, "sha256").hexdigest()
        result["modes"][label] = {
            "binary_sha256": fingerprint,
            "artifact_bytes_per_copy": local.stat().st_size,
            "cold_cli_seconds": cold,
            "cold_python_hash_bytes": sum(row["bytes"] for row in cold_hashes),
            "warm_cli_seconds": warm,
            "warm_python_hash_bytes": sum(row["bytes"] for row in warm_hashes),
            "invalidated_state_seconds": invalidated,
            "invalidated_python_hash_bytes": sum(row["bytes"] for row in state_hashes),
        }
    result["load_after"] = os.getloadavg()
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
