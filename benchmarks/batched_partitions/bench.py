"""RFC-0013: cold and one-key-change timings for a three-node keyed chain.

BARCA_BIN=$PWD/.venv/bin/barca .venv/bin/python benchmarks/batched_partitions/bench.py
Writes JSON measurements; temporary projects are removed after each variant.
"""

import json
import os
import sqlite3
import subprocess
import tempfile
import time
from contextlib import closing
from pathlib import Path


def pipeline(mode):
    batch = "" if mode == "none" else f", batch={mode}"
    if mode == "none":
        cleaned = 'return {"value": versions}'
        scored = 'return {"score": cleaned["value"] * 2}'
        annotation = "str"
    else:
        cleaned = 'return {r: {"value": versions[r]} for r in retailer}'
        scored = 'return {r: {"score": cleaned[r]["value"] * 2} for r in retailer}'
        annotation = "list[str]"
    return f"""import json
from pathlib import Path
from barca import asset, sensor, partitions_from

@sensor()
def versions():
    return True, json.loads(Path("versions.json").read_text())

@asset(partitions={{"retailer": partitions_from(versions)}}{batch})
def cleaned(retailer: {annotation}, versions):
    {cleaned}

@asset(partitions={{"retailer": partitions_from(cleaned)}}{batch})
def scored(retailer: {annotation}, cleaned):
    {scored}
"""


def measure(binary, project, count, mode, case):
    rss = project / "rss.txt"
    start = time.perf_counter()
    proc = subprocess.run(
        [
            "/usr/bin/time",
            "-f",
            "%M",
            "-o",
            str(rss),
            binary,
            "get",
            "scored",
            "pipeline.py",
            "--json",
        ],
        cwd=project,
        capture_output=True,
        text=True,
    )
    seconds = time.perf_counter() - start
    if proc.returncode:
        raise RuntimeError(proc.stderr[-4000:])
    result = json.loads(proc.stdout)
    with closing(sqlite3.connect(project / ".barca/metadata.db")) as db:
        calls = db.execute(
            "SELECT count(DISTINCT batch_id) + sum(batch_id IS NULL) "
            "FROM materializations WHERE run_id = ?",
            (result["run_id"],),
        ).fetchone()[0]
    print(
        json.dumps(
            dict(
                keys=count,
                mode=mode,
                case=case,
                seconds=round(seconds, 3),
                coordinator_peak_kib=int(rss.read_text()),
                steps=result["steps_executed"],
                calls=calls,
            )
        ),
        flush=True,
    )
    if case == "one_key":
        assert result["steps_executed"] == 3, result["steps_executed"]
        assert calls == 3, calls


def main():
    binary = str(Path(os.environ["BARCA_BIN"]).resolve())
    for count in map(int, os.environ.get("KEYS", "10000,200000").split(",")):
        for mode in ("none", "1000", '"all"'):
            with tempfile.TemporaryDirectory(prefix="batch-", dir=Path(__file__).parent) as tmp:
                project = Path(tmp).resolve()
                (project / "pipeline.py").write_text(pipeline(mode))
                versions = {f"r{i}": 1 for i in range(count)}
                (project / "versions.json").write_text(json.dumps(versions))
                measure(binary, project, count, mode, "cold")
                versions["r0"] = 2
                (project / "versions.json").write_text(json.dumps(versions))
                measure(binary, project, count, mode, "one_key")


if __name__ == "__main__":
    main()
