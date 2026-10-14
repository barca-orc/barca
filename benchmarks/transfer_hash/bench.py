#!/usr/bin/env python3
"""Cost of verifying a local artifact copy against its recorded SHA-256 (#240, #248).

Every artifact a run reads from a remote-backed project is hashed once (python/barca/_transfer.py).
This times that hash on a file of the given size, and the whole "local copy matches" path of a
`get` request, so a regression shows up as a number rather than as a slower pipeline.

    python benchmarks/transfer_hash/bench.py            # 256 MB
    python benchmarks/transfer_hash/bench.py 1536       # 1.5 GB
    python benchmarks/transfer_hash/bench.py 1536 --max-seconds-per-gb 3   # exit 1 if slower

One reviewer measured about 0.7 s for 1.5 GB (page cache warm); a cold disk is bounded by its
read speed instead, so the file is read twice and the second, warm, time is the one compared.
"""

import argparse
import hashlib
import os
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "python"))

from barca import _transfer  # noqa: E402


def _write(path: Path, megabytes: int) -> None:
    block = os.urandom(1 << 20)
    with open(path, "wb") as f:
        for _ in range(megabytes):
            f.write(block)


def _timed(fn) -> float:
    start = time.perf_counter()
    fn()
    return time.perf_counter() - start


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("megabytes", nargs="?", type=int, default=256)
    ap.add_argument("--max-seconds-per-gb", type=float, default=None)
    args = ap.parse_args()

    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "artifact.parquet"
        _write(path, args.megabytes)
        expected = hashlib.sha256(path.read_bytes()).hexdigest()
        msg = {"type": "get", "remote": str(Path(tmp) / "missing"), "local": str(path), "sha256": expected}

        _timed(lambda: _transfer._sha256(path))  # warm the page cache
        hash_s = min(_timed(lambda: _transfer._sha256(path)) for _ in range(3))
        # A matching copy is kept: no download, so this is the hash plus one stat.
        keep_s = min(_timed(lambda: _transfer._transfer(msg)) for _ in range(3))

    gb = args.megabytes / 1024
    per_gb = hash_s / gb
    print(f"size            {args.megabytes} MB")
    print(f"sha256          {hash_s:.3f} s  ({per_gb:.2f} s/GB)")
    print(f"keep local copy {keep_s:.3f} s")
    if args.max_seconds_per_gb is not None and per_gb > args.max_seconds_per_gb:
        print(f"FAIL: {per_gb:.2f} s/GB is over the {args.max_seconds_per_gb} s/GB limit")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
