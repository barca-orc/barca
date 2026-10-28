"""Give the fixture a long history: older generations of every key and thousands of runs.

    python benchmarks/wide_partitions/history.py DEST [--generations 5] [--runs 3000]

Runs barca would take weeks to record are written straight into the metadata DB: `--runs` runs
over the past two months, and for every successful key row `--generations` older rows under
other run hashes (one in twenty failed), each an earlier result of the same step. The rows barca
wrote stay the newest (highest id), as they would be. The DB grows about 1.7 KB a row.
"""

from __future__ import annotations

import argparse
import sqlite3
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dest", type=Path)
    parser.add_argument("--generations", type=int, default=5)
    parser.add_argument("--runs", type=int, default=3000)
    args = parser.parse_args()
    db = sqlite3.connect(args.dest / ".barca" / "metadata.db", isolation_level=None)
    (current,) = db.execute("SELECT max(id) FROM materializations").fetchone()
    runs = args.runs
    db.execute("BEGIN")
    db.execute(
        """WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r WHERE i < ?)
        INSERT INTO runs (run_id, command, files, status, steps_total, steps_executed,
                          steps_cached, started_at, finished_at, elapsed_seconds, triggered_by)
        SELECT printf('hist%06d', i), 'get', '[]',
               CASE WHEN i % 17 = 0 THEN 'failed' ELSE 'success' END, 400, 300, 100,
               datetime('now', printf('-%d minutes', (? - i) * 29 + 60)),
               datetime('now', printf('-%d minutes', (? - i) * 29 + 55)), 300.0,
               CASE WHEN i % 3 = 0 THEN 'schedule' ELSE 'api' END
        FROM r""",
        (runs, runs, runs),
    )
    for g in range(1, args.generations + 1):
        db.execute(
            """INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format,
                   artifact_size_bytes, elapsed_seconds, status, error_message, attempts,
                   created_at, output_hash, run_id, components, cpu_seconds, max_rss_bytes)
            SELECT node_id, substr(run_hash, 1, 62) || printf('%02d', ?1),
                   CASE WHEN (id + ?1) % 20 = 0 THEN NULL ELSE artifact_path END,
                   artifact_format, artifact_size_bytes, elapsed_seconds + ?1 * 0.01,
                   CASE WHEN (id + ?1) % 20 = 0 THEN 'failed' ELSE 'success' END,
                   CASE WHEN (id + ?1) % 20 = 0 THEN 'ValueError: an earlier failure' END, 1,
                   datetime('now', printf('-%d minutes', (?2 + 1 - ?1) * 14000 + (id % 600) * 20)),
                   output_hash,
                   printf('hist%06d', ((?2 + 1 - ?1) * 600 + id % 600) % ?3 + 1),
                   components, cpu_seconds, max_rss_bytes
            FROM materializations WHERE id <= ?4 AND run_hash IS NOT NULL""",
            (g, args.generations, runs, current),
        )
    # What barca wrote is the newest: above every row added here.
    shift = 10 ** len(str(db.execute("SELECT max(id) FROM materializations").fetchone()[0]))
    db.execute("UPDATE materializations SET id = id + ? WHERE id <= ?", (shift, current))
    db.execute(
        "UPDATE sqlite_sequence SET seq = (SELECT max(id) FROM materializations) "
        "WHERE name = 'materializations'"
    )
    db.execute("COMMIT")
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    (rows,) = db.execute("SELECT count(*) FROM materializations").fetchone()
    print(f"{rows} materializations, {runs} added runs")


if __name__ == "__main__":
    main()
