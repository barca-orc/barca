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
`barca_core::db::read_metadata`. A database that has no view (missing, written by an earlier
release and not yet completed, not in WAL mode) is opened once as a writer opens it, under the
lock, and then viewed; only if it still has none are the reads made under the lock as before.
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
- **Process exit.** The CLI closes the owner's connections as a command ends
  (`db::close_connections`), each under its lock if that is free, so the last close still
  folds the log into the main file as it did after every operation. A killed process leaves
  the log on disk; the next open recovers it, as after any crash.

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
| A backup copies a whole database beside writes | `db::fold_log_tests::a_backup_copies_beside_writes_without_the_lock`, `a_pinned_main_file_takes_writes_without_changing` (unchanged) |
| Opens per run stay down | `db::owner_tests::operations_share_one_connection`, `python/tests/test_worker_startup.py` |
| Reads do not wait for the lock | `python/tests/test_reads_without_the_lock.py` |
| A waiter gets a turn | `db::tests::a_holder_that_takes_the_lock_again_at_once_gives_a_waiter_its_turn` |

## Not done

- **`VACUUM` is one hold.** It rewrites the live data in one statement: 5.6 s on the 5.2 GB
  fixture after the trim had cut it to 0.65 GB. It runs only when no run is alive on the
  database, and readers are not held up by it (they read in place), but a run that starts
  meanwhile waits for it.
- **The process-wide guard stays.** One process's operations still take turns with each
  other, reads on the database itself included. The server's read routes do not (they use
  views), and the CLI's reads no longer do. Removing the guard needs a connection per reader
  or the statement-level ordering of a write queue; neither is needed for what was measured.
- **The `flock` is per operation, not per batch.** Taking it once for a batch of operations
  would save one `open` and one `flock` each, and would bring back long holds.
- **Read views open a connection per query.** A server's read routes could keep one per view.
- **`barca serve` does not close its owned connection on exit**: its log is folded in by the
  next command that closes one.
- **A database file replaced in place by something outside barca** (written over, same
  inode) while a process holds a connection is not detected. It was never safe (SQLite
  documents it as corruption); per-operation opens made it less likely to matter.
