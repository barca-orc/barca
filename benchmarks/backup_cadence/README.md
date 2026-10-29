# Metadata backup cadence cost

Run from an owned worktree with its fast-profile binary:

```bash
.venv/bin/python benchmarks/backup_cadence/bench.py \
  --binary .venv/bin/barca --workdir target-local/backup-cost --bytes 1700000000
```

The work directory must not exist. The script initializes one real task run and its final
backup, pads the metadata database with an unrelated SQLite table, starts `serve` with a
one-hour backup interval, completes three API task runs, then stops the server gracefully.
It retains raw server logs, the padded database and its backups under that work directory;
normal worktree teardown removes them. Every server it starts is stopped in `finally`.

`BARCA_TRACE_TIMING` records backup hash/copy bytes and elapsed time, and upload payload size
and elapsed time. No backup hash, copy or upload trace may occur during the not-due runs.
The final backup must reach the local-directory store and contain all four successful runs.

## 2026-10-29 measurement

[Raw result and source hashes](2026-10-29-fast-local.json) record the fast profile and host.
This is synthetic padding, recently written files, one sample, and a local-directory store
on the same filesystem. It is not a production workload, a cold-cache result, a cloud upload
measurement, or a release-wheel benchmark.

- Actual metadata DB: 1,701,834,752 bytes, including 1,700,000,000 padding bytes.
- Not-due API runs: 83–86 ms each, zero backup hash/copy/upload calls.
- Final shutdown: 6.225 s. Hash: 3.162 s; copy: 2.916 s; local-store upload: 0.124 s.
- The final stored copy has the same byte length and all four successful run records.

The fast local-store upload can use operating-system filesystem copy optimizations. Its
payload size is not a measurement of bytes transmitted over a network. Longer backup
cadence removes repeated whole-DB work; it does not make a due backup incremental or cheap.
