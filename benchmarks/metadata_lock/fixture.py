#!/usr/bin/env python3
"""A project with a large metadata database, for measuring how long a command holds its lock.

    python benchmarks/metadata_lock/fixture.py DIR [--keys 2000] [--history 500] [--runs 20000]

Runs a partitioned asset once for real (`--keys` keys, so the artifacts and their rows exist),
then seeds history with sqlite3 as a server that has run for months leaves it (#754: a 41 GB
database beside 5 GB of artifacts): every row copied `--history` times with 3 KB of
`components`, half of the copies naming an artifact file that is gone; `--runs` runs with a
20 KB outcome and 100 log lines each. `[cache] max_size` is set so that the database is far over
its share of the limit, which is what makes `barca gc` trim it.

The defaults give about 1M materializations, 2M log lines and a database of about 4 GB, in a
few minutes. Measure with, for example:

    cd DIR && BARCA_TRACE_TIMING=1 barca gc --local 2>&1 | grep 'lock holds'

Needs `barca` on PATH (or BARCA_BIN).
"""

import argparse
import os
import sqlite3
import subprocess
import time
from pathlib import Path

PIPELINE = """\
from barca import asset, partitions


@asset(partitions={{"key": partitions([str(i) for i in range({keys})])}})
def a(key: str) -> dict:
    return {{"key": key}}
"""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dir", type=Path)
    parser.add_argument("--keys", type=int, default=2000)
    parser.add_argument("--history", type=int, default=500)
    parser.add_argument("--runs", type=int, default=20000)
    parser.add_argument("--max-size", default="2GiB")
    args = parser.parse_args()
    binary = os.environ.get("BARCA_BIN", "barca")
    root: Path = args.dir
    root.mkdir(parents=True, exist_ok=True)
    (root / "pipeline.py").write_text(PIPELINE.format(keys=args.keys))
    (root / "barca.toml").write_text("")
    subprocess.run([binary, "get", "pipeline.py", "--local"], cwd=root, check=True, capture_output=True)
    db = root / ".barca" / "metadata.db"
    started = time.monotonic()
    conn = sqlite3.connect(db)
    conn.execute("PRAGMA synchronous = OFF")
    top = conn.execute("SELECT max(id) FROM materializations").fetchone()[0]
    columns = [r[1] for r in conn.execute("PRAGMA table_info(materializations)") if r[1] != "id"]
    copied = {
        "run_hash": "run_hash || '-' || f.n",
        "components": "printf('%.*c', 3000, 'c')",
        # Half of the history names a file that is gone: what the trim's third step removes.
        "artifact_path": "CASE WHEN f.n % 2 = 0 THEN 'gone/' || f.n || '/' || id ELSE artifact_path END",
        "run_id": "'seed-' || (f.n % 20000)",
    }
    # The real rows last, as the newest of each node: copies first, then the originals again.
    conn.execute(
        f"WITH RECURSIVE f(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM f WHERE n < {args.history - 1}) "
        f"INSERT INTO materializations ({', '.join(columns)}) "
        f"SELECT {', '.join(copied.get(c, c) for c in columns)} "
        f"FROM f, materializations WHERE id <= {top} ORDER BY f.n, id"
    )
    conn.execute(
        f"INSERT INTO materializations ({', '.join(columns)}) "
        f"SELECT {', '.join(columns)} FROM materializations WHERE id <= {top} ORDER BY id"
    )
    conn.execute(
        f"WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < {args.runs}) "
        "INSERT INTO runs (run_id, command, files, target, status, steps_total, steps_executed, "
        "started_at, finished_at, elapsed_seconds, job, outcome_json) "
        "SELECT 'seed-' || n, 'get', '[\"pipeline.py\"]', 'a', 'success', 10, 10, "
        "datetime('now', '-' || (100000 - n) || ' seconds'), "
        "datetime('now', '-' || (100000 - n) || ' seconds'), 1.0, 'all', "
        "'{\"pad\":\"' || printf('%.*c', 20000, 'o') || '\"}' FROM r"
    )
    conn.execute(
        f"WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < {args.runs}), "
        "l(s) AS (SELECT 0 UNION ALL SELECT s + 1 FROM l WHERE s < 99) "
        "INSERT INTO logs (run_id, node_id, seq, line, stream) "
        "SELECT 'seed-' || n, 'pipeline.py:a', s, printf('%.*c', 80, 'l'), 'stdout' FROM r, l"
    )
    conn.commit()
    rows = conn.execute("SELECT count(*) FROM materializations").fetchone()[0]
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()
    (root / "barca.toml").write_text(f'[cache]\nmax_size = "{args.max_size}"\n')
    print(
        f"{root}: {rows} materializations, {args.runs} runs, {args.runs * 100} log lines, "
        f"{db.stat().st_size / 1e9:.2f} GB, seeded in {time.monotonic() - started:.0f}s"
    )


if __name__ == "__main__":
    main()
