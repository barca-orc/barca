# One owner of the metadata connection per process (#1056, A7 and A8)

Status: implemented as described under "What is implemented"; the rest is listed under "Not
done". Non-normative. The plan is [#1056](https://github.com/barca-orc/barca/issues/1056); the
incident behind it is [#1054](https://github.com/barca-orc/barca/issues/1054). The code is
`crates/barca-db/src/db/mod.rs` (the lock, turns, the owner) and `snapshots.rs` (views, backups).

## What there was

Every database operation took the process-wide guard (`DB_LOCK`), then the cross-process lock
(`flock` on `<db>.lock`), then opened the database twice (a read-only format check, then the
writer), worked, and closed. Closing the last connection makes SQLite checkpoint the log and
delete it, so a one-shot command did that after every operation, and the next one created the
log and its index again. Reads did the same, so a read waited for every write of every barca
process on the project, and made them wait.

Three things went wrong with it:

1. Reads queued behind writes for no reason: the database is in WAL mode, where a reader sees
   the last commit while a writer appends.
2. A process that takes the lock again right after releasing it (a run recording step after
   step, a trim working through its batches) leaves gaps of microseconds. Waiters poll, so
   they found a gap only by chance: measured in `barca-db`'s tests, a waiter beside a holder
   of 20 ms holds waited 0.4 to 5.9 s for each turn, and `barca gc` beside a server's two
   recording runs timed out after 60 s (#1054) though no single hold was long.
3. Two opens and a checkpoint per operation.

## What is implemented

### Reads without the lock (A7)

The CLI's read commands (`status` and its keys views, `history`, `stats`, `logs`, `sql`'s
reads, `doctor`'s metadata check) read through a `ReadView`, as the server's routes do:
`barca_service::db::read_metadata`. A database that has no view (missing, an empty file, written
by an earlier release and not yet completed, not in WAL mode) is treated as it was before
there were views, command by command. `history`, `stats` and `logs` have always created or
completed the database first (`open_metadata`); they still do, and then view it. `status`, its
rollup and keys views and `sql` did not, so `read_metadata` opens nothing for them and their
reads are made on the database itself, under the lock: `status` and `sql` refuse an empty file
(exit 3, the file untouched), and complete an earlier release's database or return a
rollback-journal one to WAL mode only at the step that always did (`init_db` before the
history queries, after the cache decisions). `python/tests/test_reads_of_databases_without_a_view.py`
pins each case against the bytes of the file.

A read-only connection cannot checkpoint or delete the log, so a read command that is the
last to close leaves `metadata.db-wal` (empty unless another process's writes are in it) and
`metadata.db-shm` (32 KB) beside the database; the next command that writes removes them as it
ends. Removing them from the reader would take a read-write connection under the lock, which
is what the read path no longer has, so the manual and the layout specification say what
remains instead. `.barca/`'s measured size includes the 32 KB while they exist.
`barca sql` still stamps the results a query read (`last_read_at`), which is a write.

### Turns at the lock (A7)

A waiter sets the lock file's modification time each time it finds the lock held. A process
about to take the lock again within 100 ms of releasing it compares the file's time with the
one it saw when its last hold began, and when they differ it leaves the lock free for 8 ms
first; a waiter tries at least every 5 ms. The file stays empty, no file is added, and a
process of an earlier release neither stamps nor gives way.

Maintenance holds the lock in bounded pieces: the trim's batches were already separate holds,
its run deletions now stop after 100 ms per hold, and eviction marks are recorded 1000 per
hold. `VACUUM` is one statement and cannot be split: see "Not done".

### The owner (A8)

- **What holds the connection.** A process-wide table in `barca-db` (`OWNED`), keyed by the
  path of the database, holds one read-write `libsql::Connection` per metadata database
  (a file named `metadata.db` that is not a `DbSnapshot`'s copy). Any other file (a backup
  copy, a snapshot, a staged restore) is opened per operation as before, so no connection
  outlives a temporary file. `barca-engine` still has no database access.
- **What callers see.** `open_conn` keeps its signature and its contract: it returns a
  `DbHandle` that holds the cross-process lock until it is dropped, and a
  `libsql::Connection`. The connection is now a clone of the owner's (libsql connections are
  reference-counted), so none of the call sites changes.
- **How writes are ordered.** As before: callers hold the process-wide guard (`db_guard`),
  and the `flock` is still taken for every operation, on a descriptor of its own. Two tasks of
  one process therefore still cannot both hold a `DbHandle` of one database, which is what
  keeps them from interleaving statements on the shared connection. There is no queue: a
  mutex around one connection has the same order and no new failure mode.
- **How the cross-process lock is taken.** Per operation, exactly as before. Other processes
  get turns between operations (and are now sure to: "Turns at the lock"), and a backup, a
  restore, a reset or a trim that holds the lock excludes every writer as it always did. The
  owner only removes the opens and the close inside the hold.
- **A restore, a reset or anything else that replaces the files.** Every handout, under the
  lock, compares the device and inode of the database file and of its `-wal` with the ones
  the connection was opened on (the `-wal`'s as of the last release). A different file means
  the connection is on a database that was moved aside, or has a log that was deleted under
  it: it is dropped and a new one is opened, with the read-only format check first, as every
  open did. SQLite does not checkpoint or delete the log when a connection to a moved
  database closes (`databaseIsUnmoved` in the pager), and a test holds that.
- **A migration.** `init_db` runs on the owner's connection under the lock. Another process's
  migration changes the schema cookie, and SQLite prepares this connection's statements
  again. The format version is still read on every handout, so a database a newer barca
  rewrote is refused on the next operation, not at the next process start.
- **An operation that ends mid-transaction** (a cancelled future). The handle's drop sees the
  connection is not in autocommit and removes it from the table, so it closes (and SQLite
  rolls back) when the caller's clone drops, as a per-operation connection did. A handout
  checks again and never reuses a connection with a transaction open.
- **Memory.** A handout first lets go of the pages the last operation read
  (`PRAGMA shrink_memory`), so a process's memory is what its largest operation needs, as it
  was when each had a connection of its own.
- **Checkpoints.** The log is no longer empty between a process's operations, so a
  checkpoint that must empty it (`fold_log`: backups, restore, reset) really waits for the
  readers of what is in it. Each try now answers at once and the wait is asynchronous, for up
  to 5 s; SQLite's own wait slept on the calling thread, where a reader on the same thread
  could never finish.
- **Measuring `.barca/`.** `gc` closes this process's kept connections before it walks the
  directory, so the size it plans from holds no log of its own that comes and goes.
- **Process exit.** The CLI closes the owner's connections as a command ends
  (`db::close_connections`), each under its lock if that is free, so the last close still
  folds the log into the main file as it did after every operation. Every exit of a command
  goes through one function that does it (`exit` in `crates/barca-cli/src/main.rs`; a test
  reads the sources for another, after the `logs -f` exits were found to skip it). A killed
  process leaves the log on disk; the next open recovers it, as after any crash.

### The safety properties and their tests

| Property | Test |
|---|---|
| A look at the database file never closes a descriptor of it (`HELD_FILES`) | `db::snapshots::read_view_tests::looking_at_the_database_file_keeps_this_process_s_locks` (unchanged) |
| A writer in another process that closes does not take the log from under this process's owned connection | `db::owner_tests::another_process_closing_its_connection_leaves_the_owned_one_its_log` |
| A restore or reset while this process holds a connection: the next operation is on the new file, and closing the old connection leaves the new database's log alone | `db::owner_tests::a_replaced_database_gets_a_new_connection_and_keeps_its_log` |
| A log deleted under the connection is noticed | `db::owner_tests::a_log_deleted_under_the_connection_is_noticed` |
| Two processes writing | `python/tests/test_metadata_stress.py`, `python/tests/test_concurrent_processes.py` (unchanged) |
| A crash mid-transaction leaves no part of it, and the next process works | `db::owner_tests::a_process_killed_mid_transaction_leaves_nothing_of_it` |
| A cancelled operation's transaction does not reach the next operation | `db::owner_tests::a_transaction_left_open_is_not_handed_to_the_next_operation` |
| A reader during a checkpoint | `db::snapshots::read_view_tests::a_checkpoint_waits_for_a_read_view_query_instead_of_failing` (unchanged) |
| A backup copies a whole database beside writes | `db::fold_log_tests::a_backup_copies_beside_writes_without_the_lock`, `db::wal::tests::checkpoint_can_truncate_between_backup_steps` |
| Opens per run stay down | `db::owner_tests::operations_share_one_connection`, `python/tests/test_worker_startup.py` |
| Reads do not wait for the lock | `python/tests/test_reads_without_the_lock.py` |
| A read command changes no database it cannot read in place, beyond what it always did | `python/tests/test_reads_of_databases_without_a_view.py` |
| Every exit of the CLI closes the kept connections | `layering_tests::every_exit_goes_through_the_one_that_closes_the_database` |
| The lock's holder is named while other locks come and go | `db::tests::the_holder_is_named_while_other_locks_come_and_go` |
| A waiter gets a turn | `db::tests::a_holder_that_takes_the_lock_again_at_once_gives_a_waiter_its_turn` |

## Measured

2026-11-19, release builds, a machine under load (load average about 30), the base being
`integrate/modularity` at 489abbaf.

- Trivial pipeline (`benchmarks/trivial`, `get --refresh-all`), 40 interleaved runs: database
  opens 16 to 4, coordinator CPU 10.6 to 8.0 ms (median), wall 53.0 to 50.6 ms (median), peak
  15,674 to 15,492 KiB.
- `scripts/perf-gate.py`, two runs each: trivial CPU 8.3 and 8.4 ms to 8.5 and 6.7 ms, peak
  15,840 and 15,792 KiB to 15,808 and 15,920 KiB; 5,000-key incremental CPU (max) 23.3 and
  15.0 ms to 10.0 and 9.4 ms, peak 16,240 and 16,172 to 16,476 and 16,520 KiB; plan reasons
  2.8 and 2.9 ms to 2.0 and 1.3 ms. All budgets met on both.
- The fixture of `benchmarks/metadata_lock/fixture.py` (1,002,000 materializations, 20,000
  runs, 2,000,000 log lines, 5.2 GB): `barca gc` trimming it with `barca get` run in a loop
  beside it. Base: 7 runs finished in 145 s, one of them failed after the 60 s wait, the
  longest single wait for the lock was 57.7 s. Now: 37 runs in 222 s, none failed, the longest
  wait 11.8 s (a `VACUUM`), a run's median 7.1 s against 9.1 s.
- The longest single hold of the lock by that `gc`, alone: 5.6 s before and after, a
  `VACUUM` (three of them, 3.4 to 4.8 s each). With a run alive, so that no `VACUUM` runs:
  0.54 s, a batch of 2,000 rows' components. The trim's run deletions held it for up to 1.1 s
  a batch before and are cut at 100 ms.

## Not done

- **`VACUUM` is one hold.** It rewrites the live data in one statement: 3.4 to 4.8 s on the
  fixture once the trim had cut it to under 1 GB (`BARCA_TRACE_TIMING` prints each). It runs only when no run is alive on the
  database, and readers are not held up by it (they read in place), but a run that starts
  meanwhile waits for it.
- **The process-wide guard stays.** One process's operations still take turns with each
  other, reads on the database itself included. The server's read routes do not (they use
  views), and the CLI's reads no longer do. Removing the guard needs a connection per reader
  or the statement-level ordering of a write queue; neither is needed for what was measured.
- **The `flock` is per operation, not per batch.** Taking it once for a batch of operations
  would save one `open` and one `flock` each, and would bring back long holds.
- **Read views open a connection per query.** A server's read routes could keep one per view.
- **A long read holds the log.** `barca doctor` checks every page of the database through a
  view; while that one statement runs, no checkpoint can empty the log, so a backup's
  `fold_log` fails with "could not finish" after 5 s and is tried again at its next occasion.
  Before, `doctor` copied the database under the lock, and every writer waited for the copy.
- **Eviction marks were not measured.** They are recorded 1000 per hold; the fixture has
  nothing to evict.
- **`barca serve` does not call `close_connections` on exit.** Measured with a live server
  (`tests/simulation/owner_serve.py`, 2026-11-19, five clean stops with no other barca process
  on the database): after SIGTERM and the drain, `metadata.db` is alone on disk, with no `-wal`
  and no `-shm`, so the kept connection does close on that path (as the last one, it folds the
  log in and deletes it), though no code asks it to. A killed server leaves the log as it was
  (21 to 227 KB at three SIGKILLs, with CLI processes writing beside it); the next process to
  open the database recovers it and says nothing, and the next command that writes folds it
  in.
- **A database file replaced in place by something outside barca** (written over, same
  inode) while a process holds a connection is not detected. It was never safe (SQLite
  documents it as corruption); per-operation opens made it less likely to matter.
