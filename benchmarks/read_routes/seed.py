"""Seed a generated read-routes project with a run history, through barca's own commands.

    python benchmarks/read_routes/seed.py PROJECT [--runs 2000] [--barca PATH]

First `barca get` of every asset (the 200k-key node included), then an edit to the big
partitioned node and a second full `get` (so it has two generations of key rows), then
`--runs` small runs: `barca get <asset> --refresh <asset>` of a random asset, and now and then a
`barca run` of a task. Each is one run in `barca history`.

While the small runs go, the big node's key list is cut to 1000 keys so that each run plans in
a fraction of a second; no small run reads that node, and the list is put back afterwards
(its keys' run hashes do not depend on the list, so nothing is invalidated).
"""

from __future__ import annotations

import argparse
import os
import random
import re
import shutil
import subprocess
import time
from pathlib import Path


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("project", type=Path)
    ap.add_argument("--runs", type=int, default=2000)
    ap.add_argument(
        "--barca",
        default=os.environ.get("BARCA") or shutil.which("barca") or "barca",
        help="the barca binary (default: $BARCA, else barca on PATH)",
    )
    ap.add_argument("--seed", type=int, default=11)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    project = a.project.resolve()

    def barca(*args: str) -> None:
        subprocess.run(
            [a.barca, *args],
            check=False,
            cwd=project,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    t = time.time()
    barca("get")
    retail = project / "retail.py"
    retail.write_text(
        retail.read_text().replace('"n": len(retailer)}', '"n": len(retailer) + 1}', 1)
    )
    barca("get")
    print(f"two full gets: {time.time() - t:.0f}s", flush=True)
    names = sorted(
        m
        for f in project.glob("*.py")
        for m in re.findall(
            r"^def ((?:ingest|retail|finance|marketing|ops|reports)_\d+)\(",
            f.read_text(),
            re.MULTILINE,
        )
    )
    tasks = sorted(
        m
        for f in project.glob("*.py")
        for m in re.findall(r"^def (publish_\w+)\(", f.read_text(), re.MULTILINE)
    )
    full = retail.read_text()
    m = re.search(r"for i in range\((\d+)\)\]", full)
    if m:
        retail.write_text(full.replace(m.group(0), "for i in range(1000)]", 1))
    t = time.time()
    for i in range(a.runs):
        if i % 50 == 0 and tasks:
            barca("run", rng.choice(tasks))
        else:
            name = rng.choice(names)
            barca("get", name, "--refresh", name)
        if i % 200 == 0:
            print(f"{i} runs, {time.time() - t:.0f}s", flush=True)
    retail.write_text(full)
    print(f"done: {a.runs} runs in {time.time() - t:.0f}s", flush=True)


if __name__ == "__main__":
    main()
