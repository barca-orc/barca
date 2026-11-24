#!/usr/bin/env python3
"""Manual soak for #1087: a writable `barca serve` that stops firing live cron ticks while
`GET /health` says `scheduler_enabled: true`, with the metadata store over its share.

Builds a synthetic store at the size of the incident (default about 1.2 GB: one run of 110,000
steps, each step the latest success of its own node, with a payload no trim step clears), serves
it with a seconds-level cron against a cache limit that makes the store over its share, holds a
read transaction on it (the reader that left a WAL as large as the store), and for `--window`
seconds records:

- the latency of `GET /health`, `/schedule` and `/queue` (a request gives up after 15 s, as in
  the incident),
- the cron slots that fired no tick (a missed slot is not replayed),
- the server's own timings (`BARCA_TRACE_TIMING`: each metadata `VACUUM`, the lock holds), and
  every `[stall-trace]` line when the server was built with `scheduler_stall_trace.patch`,
- the sizes of `metadata.db` and its `-wal` at the end.

It needs free disk for the store and for the log: with a reader held, each `VACUUM` appends a
whole copy of the store to the log (`-wal`), which no checkpoint can truncate, so the log grew
to 4.4x the store in four minutes at 1.3 GB (5.9 GB). The watch stops early when free disk falls
under `--min-free-gb`. It also needs a binary from `maturin develop` in the active environment. Synthetic data only; everything lives in a
temporary directory removed at the end (`--keep` leaves it).

    python scripts/soak/scheduler_stall_soak.py                       # 1.2 GB, 3 minutes
    python scripts/soak/scheduler_stall_soak.py --mb 200 --window 60  # a quick look
    python scripts/soak/scheduler_stall_soak.py --no-reader           # the store alone

Apply `scripts/soak/scheduler_stall_trace.patch` first (`git apply`, rebuild, revert) to see
which awaited call the scheduler is stuck in, with its file and line.
"""

import argparse
import json
import shutil
import sqlite3
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO))

from python.tests import served  # noqa: E402
from python.tests.helpers import clean_env  # noqa: E402

PIPELINE = """
import time

from barca import asset, Schedule


@asset(freshness=Schedule("{cron}"))
def tick() -> dict:
    return {{"t": time.time()}}
"""


def build_store(db: Path, megabytes: int, steps: int) -> float:
    """One run of `steps` steps, each the latest success of its own node, padded to `megabytes`
    in all. Returns the seconds it took."""
    started = time.monotonic()
    payload = "z" * max(1, megabytes * 1024 * 1024 // steps - 120)
    conn = sqlite3.connect(db)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute(
        "INSERT INTO runs (run_id, command, files, status) VALUES ('soak-run', 'get', 'p.py', 'success')"
    )
    batch = 2000
    for start in range(0, steps, batch):
        conn.executemany(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, status, run_id,"
            " output_json) VALUES (?, ?, ?, 'success', 'soak-run', ?)",
            (
                (f"soak.py:step{i}", f"h{i}", f"a/{i}", payload)
                for i in range(start, min(steps, start + batch))
            ),
        )
        conn.commit()
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()
    return time.monotonic() - started


def timed_get(server: served.Serve, path: str) -> float | str:
    """Seconds one GET took, or what went wrong (the incident's probes gave up after 15 s)."""
    started = time.monotonic()
    try:
        with urllib.request.urlopen(f"{server.url}{path}", timeout=15) as response:
            response.read()
    except OSError as error:
        return f"{type(error).__name__} after {time.monotonic() - started:.1f}s"
    return round(time.monotonic() - started, 2)


def mb(path: Path) -> float | None:
    return round(path.stat().st_size / 2**20, 1) if path.exists() else None


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--mb", type=int, default=1200, help="store size in MB (default 1200)")
    ap.add_argument("--steps", type=int, default=110_000, help="steps in the one run")
    ap.add_argument("--window", type=int, default=180, help="seconds to watch (default 180)")
    ap.add_argument("--cron", default="*/10 * * * * *", help="schedule (default every 10 s)")
    ap.add_argument(
        "--limit",
        default="20GB",
        help="BARCA_CACHE_MAX_SIZE; the metadata share is a tenth, at most 1 GiB (default 20GB)",
    )
    ap.add_argument(
        "--checkpoint-secs", default="60", help="period of the cache check (default 60, as served)"
    )
    ap.add_argument("--no-reader", action="store_true", help="hold no read transaction")
    ap.add_argument(
        "--min-free-gb", type=float, default=3.0, help="stop watching below this free disk"
    )
    ap.add_argument("--keep", action="store_true", help="keep the project directory")
    args = ap.parse_args()
    step = int(args.cron.split()[0].split("/")[1]) if args.cron.startswith("*/") else 1

    root = Path(tempfile.mkdtemp(prefix="barca-soak-")) / "p"
    root.mkdir()
    (root / "pipeline.py").write_text(PIPELINE.format(cron=args.cron))
    report: dict = {"mb": args.mb, "steps": args.steps, "cron": args.cron, "limit": args.limit}
    try:
        with served.Serve(root, env=clean_env()) as first:
            first.wait("a first tick", lambda: first.get("/runs?limit=1")["total"])
        db = root / ".barca" / "metadata.db"
        report["build_seconds"] = round(build_store(db, args.mb, args.steps), 1)
        report["db_mb_before"] = mb(db)
        print(
            f"store built in {report['build_seconds']} s: {report['db_mb_before']} MB", flush=True
        )

        reader = None
        if not args.no_reader:
            reader = sqlite3.connect(f"file:{db}?mode=ro", uri=True, isolation_level=None)
            reader.execute("BEGIN")
            reader.execute("SELECT count(*) FROM runs").fetchall()
        env = clean_env(
            BARCA_CACHE_MAX_SIZE=args.limit,
            BARCA_TEST_CACHE_CHECKPOINT_SECS=args.checkpoint_secs,
            BARCA_TRACE_TIMING="1",
        )
        probes = []
        try:
            with served.Serve(root, env=env, timeout=300) as server:
                ready = int(time.time())
                end = time.monotonic() + args.window
                while time.monotonic() < end:
                    if shutil.disk_usage(root).free < args.min_free_gb * 2**30:
                        report["stopped_early"] = "free disk under --min-free-gb"
                        break
                    row = {
                        "t": int(time.time()) - ready,
                        "health": timed_get(server, "/health"),
                        "schedule": timed_get(server, "/schedule"),
                        "queue": timed_get(server, "/queue"),
                    }
                    probes.append(row)
                    print(json.dumps(row), flush=True)
                    time.sleep(5)
                until = int(time.time()) - step
                report["scheduler_enabled_at_end"] = server.get("/health")["scheduler_enabled"]
        finally:
            if reader is not None:
                reader.close()
        report["probes"] = probes
        report["db_mb_after"], report["wal_mb_after"] = mb(db), mb(Path(f"{db}-wal"))
        report["seconds_watched"] = until - ready
        with served.snapshot(root) as snap:
            fired = {
                r[0]
                for r in snap.execute(
                    "SELECT scheduled_at FROM schedule_ticks WHERE action = 'fired'"
                )
            }
        slots = [s for s in range(ready + 1, until + 1) if s % step == 0]
        report["slots_expected"] = len(slots)
        report["slots_missed"] = len([s for s in slots if s not in fired])
        report["trace_lines"] = [
            line
            for line in server.err.splitlines()
            if any(
                k in line
                for k in ("[stall-trace]", "[timing]", "metadata lock holds", "share of the limit")
            )
        ]
        print(json.dumps({k: v for k, v in report.items() if k != "probes"}, indent=2))
    finally:
        if args.keep:
            print(f"kept {root}")
        else:
            shutil.rmtree(root.parent, ignore_errors=True)


if __name__ == "__main__":
    main()
