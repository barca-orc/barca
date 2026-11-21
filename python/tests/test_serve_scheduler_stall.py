"""A writable `barca serve` must not lose live cron ticks while its metadata is being trimmed.

The reproduction behind issues #1087 and #1082: a server's metadata database is over its share of
the cache limit and cannot be trimmed smaller (every row is a node's latest success, so nothing is
removable), and its scheduler stops firing live ticks while `GET /health` still says
`scheduler_enabled: true`.

What the two tests below show, with a seconds-level cron (`*/2 * * * * *`) so a lost tick is seen
in seconds and not in five-minute slots:

- Over its share alone, the store costs the scheduler the time of one `VACUUM` (about a quarter
  of a second at 30 MB; it grows with the file), and no two ticks in a row are lost.
- With a reader holding a read transaction too (a long-lived observer, a backup copy, a
  `barca sql` session), every trim pass holds the process-wide metadata lock for the whole of
  `metadata_size::vacuum`: the `VACUUM`, then a checkpoint that waits out SQLite's 5 s busy wait
  for the reader and gives up. The scheduler task awaits the metadata lock for its heartbeat and
  its due-slot read (`alive_running_runs`) with no time limit, so it does not run for those
  seconds, and a slot it did not evaluate is never matched again: a cron tick is matched against
  the wall-clock second it wakes in (`scheduler/tick.rs::due_jobs`), with no live replay. The
  lost ticks are gone, not late.

The reader test is `xfail(strict=True)`: it states the behavior a fix must give (every slot
fires while a trim holds the lock), and the PR that fixes it removes the marker. The control
test passes today and must keep passing.

The soak script `scripts/soak/scheduler_stall_soak.py` runs the same shape at the size of the
issue (a store of about 1.2 GB, a log of similar size, a run of 110,000 steps).

The store is written while no barca process runs, and the reader opens it read-only (`mode=ro`),
so neither creates the file; a test must not open `.barca/metadata.db` with `sqlite3` while barca
can write it (`served.py`).
"""

import sqlite3
import time
from pathlib import Path

import pytest

from . import served
from .helpers import clean_env

STEP = 2  # seconds between cron slots
PIPELINE = f"""
import time

from barca import asset, Schedule


@asset(freshness=Schedule("*/{STEP} * * * * *"))
def tick() -> dict:
    return {{"t": time.time()}}
"""

# The share is a tenth of the limit: 10 MB, against a store of about 30 MB.
LIMIT = "100MB"
STORE_MB = 30
WINDOW = 16  # seconds of live ticks watched, after the server answers /health


def bloated_store(root: Path) -> Path:
    """Start a server on `root` once so its metadata exists, stop it, and fill the database with
    one latest-success row per node, each with a payload no trim step clears (`output_json`).
    The rows are what the trim cannot remove: it ends over its share however long it runs."""
    with served.Serve(root, env=clean_env()) as first:
        first.wait("a first tick", lambda: first.get("/runs?limit=1")["total"])
    db = root / ".barca" / "metadata.db"
    conn = sqlite3.connect(db)
    payload = "z" * 20_000
    conn.executemany(
        "INSERT INTO materializations (node_id, run_hash, artifact_path, status, run_id, output_json)"
        " VALUES (?, ?, ?, 'success', 'r', ?)",
        ((f"filler.py:n{i}", f"h{i}", f"a/{i}", payload) for i in range(STORE_MB * 50)),
    )
    conn.commit()
    conn.close()
    return db


def live_slots_missed(root: Path, ready: int, until: int) -> list[int]:
    """The cron slots in (`ready`, `until`] that left no fired tick, from a snapshot taken once
    the server has stopped."""
    with served.snapshot(root) as db:
        fired = {
            r[0]
            for r in db.execute("SELECT scheduled_at FROM schedule_ticks WHERE action = 'fired'")
        }
    return [s for s in range(ready + 1, until + 1) if s % STEP == 0 and s not in fired]


def watch(root: Path, db: Path, *, reader: bool) -> tuple[list[int], dict]:
    """Serve `root` (a store over its share, its trim run every second) for `WINDOW` seconds,
    with a read transaction held on `db` when `reader`. The slots that fired no tick, and what
    the server answered meanwhile."""
    env = clean_env(BARCA_CACHE_MAX_SIZE=LIMIT, BARCA_TEST_CACHE_CHECKPOINT_SECS="1")
    held = None
    if reader:
        held = sqlite3.connect(f"file:{db}?mode=ro", uri=True, isolation_level=None)
        held.execute("BEGIN")
        held.execute("SELECT count(*) FROM materializations").fetchall()
    try:
        with served.Serve(root, env=env) as server:
            ready = int(time.time())
            seen = {"enabled": []}
            end = time.monotonic() + WINDOW
            while time.monotonic() < end:
                time.sleep(1)
                try:
                    seen["enabled"].append(server.get("/health")["scheduler_enabled"])
                except OSError:
                    pass
            until = int(time.time()) - 2
    finally:
        if held is not None:
            held.close()
    seen["stderr"] = server.err
    return live_slots_missed(root, ready, until), seen


def test_an_over_share_store_alone_costs_no_tick(tmp_path):
    """The control: the same store and the same trim, with no reader, loses no two slots in a
    row. A trim here holds the lock for a quarter of a second, so a slot lost to a loaded
    machine is alone; the reader's 5 s holds lose runs of them."""
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    db = bloated_store(tmp_path)
    missed, seen = watch(tmp_path, db, reader=False)
    in_a_row = [s for s in missed if s + STEP in missed]
    assert not in_a_row, f"slots with no tick, two in a row: {missed}\n{seen['stderr']}"


@pytest.mark.xfail(
    strict=True,
    reason="#1087: a trim holds the metadata lock for its VACUUM and a 5 s checkpoint wait on a "
    "reader's transaction, and the scheduler awaits that lock with no limit and never replays "
    "a live slot it did not evaluate",
)
def test_a_reader_on_an_over_share_store_does_not_lose_live_ticks(tmp_path):
    """Over its share, with a read transaction held: every slot must still fire, and
    `scheduler_enabled` must not be the only sign of life. Today about half of them are
    lost: each trim pass holds the lock 5 s or more, so slots go in runs of two or more."""
    (tmp_path / "pipeline.py").write_text(PIPELINE)
    db = bloated_store(tmp_path)
    missed, seen = watch(tmp_path, db, reader=True)
    assert seen["enabled"] and all(seen["enabled"]), (
        "the health stayed enabled throughout the loss, as in #1087"
    )
    assert not missed, f"slots with no tick while health said enabled: {missed}\n{seen['stderr']}"
