# The `.barca/` directory

Status: implemented. Specification version: 1 (#800).
Scope: every path barca creates under a project's `.barca/`, which process owns it (creates
and removes it), and whether `barca gc` and the cache size budget (`[cache] max_size`) count
it. What the files contain is owned elsewhere: the metadata DB by
[metadata-schema.md](metadata-schema.md), artifacts by [artifact-formats.md](artifact-formats.md),
backups by the remote manual (`barca docs remote`), disk use by the cache manual
(`barca docs cache`, "Disk use" and "Where things live").
Conformance: `python/tests/test_dot_barca_layout.py` (the top-level entries a pipeline run,
`barca sql`, `barca status` and `barca gc` leave are a subset of the table below),
`crates/barca-core/src/gc/tests.rs` (what `barca gc` evicts and measures).

`.barca/` is in the project root (`barca docs cache`, "Where things live"). The default
environment uses `.barca/` itself; a named environment `<env>` keeps its own `metadata.db*`,
`artifacts/`, `backups/` and `live-logs/` under `.barca/envs/<env>/`, with the same rules.
Everything else is shared by the project's environments.

## Rules

- **Owner.** *Rust* is the `barca` binary (CLI, coordinator, `barca serve`); *Python* is the
  worker (`python -m barca._worker --daemon`) and the helpers the binary starts
  (`barca._sql`, `barca._transfer`, `barca._inspect`). Where Rust names a path and Python
  writes it, both are given, path owner first.
- **Budget.** The cache size budget measures every file under `.barca/` except
  `gc-state.json` (gc's own bookkeeping) and the backup files of `backups/`, which are
  reported separately (RFC-0015).
- **Evicted by `barca gc`** lists what eviction may delete (the tiers in `barca docs cache`,
  "Disk use"). Everything else stays until its owner removes it or the user deletes it.
- A name not in this table is not written by barca. Tests may create other files; barca
  measures them (as `other`) and never deletes them.

## Top-level entries

`<...>` is a placeholder for one path segment or a part of a name.

| Entry | Owner | What it is | Removed by | Evicted by `barca gc` | In the budget |
| --- | --- | --- | --- | --- | --- |
| `.gitignore` | Rust (`db::ensure_env_dirs`); Python (`_sql.py`) writes it too when it is missing | `*`, so git ignores `.barca/` | never | no | yes |
| `metadata.db` | Rust (`db/`) | the local libSQL metadata database | the user (`barca state reset` moves it aside) | no | yes |
| `metadata.db-wal` | Rust (libSQL) | the database's write-ahead log | libSQL; `barca state reset` | no | yes |
| `metadata.db-shm` | Rust (libSQL) | the write-ahead log's shared-memory index | libSQL; `barca state reset` | no | yes |
| `metadata.db.lock` | Rust (`db::acquire_file_lock`) | cross-process lock file, held while a process uses the DB | never (empty) | no | yes |
| `metadata.db.pre-restore` | Rust (`barca state restore`) | the database a restore replaced | the user | no | yes |
| `metadata.db.reset-<UTC>` | Rust (`barca state reset`) | a database a reset moved aside, with its `-wal` and `-shm` | the user | no | yes |
| `artifacts/` | Rust names and records paths; Python writes the files | `<node>/<run_hash><ext>` (`<node><ext>` for a step without a run hash), `*.moved-aside*` directories, and a batched call's `.batch-<pid>-<call>/` split directory | eviction; `barca gc` | yes: results by tier, unreferenced files and moved-aside directories as leftovers | yes |
| `backups/` | Rust (`backup.rs`, `backup/cadence.rs`) | with a store: the five newest metadata backups (`<name>.db`, `<name>.json`, `<name>.unsent`) and `.admission.lock`, `.last-attempt`, `.covered-run`, `.backup-*.tmp` | rotation keeps five | no | no (reported as `backup_bytes`) |
| `live-logs/` | Rust (`run_logs.rs`) | `<run_id>.jsonl`, a run's output spooled while it runs | the run, once its lines are in the DB; a killed run's file stays | no | yes |
| `run-owners/` | Rust (`run_owner.rs`) | `<token>.fifo`, held open while a process has a run in flight | its process; a later `get`/`run` removes one an hour after no `running` run names it | no | yes |
| `serve.lock` | Rust (`barca serve`) | held by a running server so `barca state restore` refuses to run | never (empty) | no | yes |
| `gc-state.json` | Rust (`gc/mod.rs`) | what the last measurement found; `gc-state.json.tmp` while it is replaced | `barca state reset`/`restore` clear it | no | no |
| `envs/` | Rust (`db::ensure_env_dirs`) | `<env>/` per named environment, holding that environment's `metadata.db*`, `artifacts/`, `backups/`, `live-logs/` | the user | as for the default environment | as for the default environment |
| `staging/` | Python (`_artifacts._staging_dir`) and Rust (`result_journal.rs`, `transfer::upload_snapshot_prefix`) | `<pid>/` per process: a worker's files in transit (uploads, downloads, sink staging) and the coordinator's result journals and upload snapshots | the next worker start (`_artifacts.clean_staging`) removes dead processes' directories | yes: a dead process's directory, as a leftover | yes |
| `branches/` | Rust names and removes (`io_loop.rs`); Python writes and reads (`_branches.py`) | `<run>-<pid>/<group>/<branch><ext>`, what `parallel()` branches returned | the coordinator, when the calling step ends and when the run ends; the next run sweeps dead coordinators' directories | no | yes |
| `inspection-cache/` | Rust names it (`config::inspection_cache_path`); Python writes it (`_inspection_cache.py`) | `<env>.json` with its `.lock` and `.epoch` files and temporary files while it is replaced: disposable `barca status` evidence | never; bounded at 8 MiB by its writer | no | yes |
| `sql-cache/` | Python (`_sql.py`); Rust names it (`sql::CACHE_DIR`) | copies of store artifacts `barca sql` downloaded, by URI, each with a `.version` stamp | eviction | yes: copies unused for 5 minutes, as leftovers | yes |
| `range-cache/` | Python (`_storage.py`) | `<run_hash>-<version>/`, byte ranges lazy inputs read from a store object | eviction | yes: directories unused for 5 minutes, as leftovers | yes |
| `.batch-<pid>-<call>` | Python (`_worker._run_daemon_call`) | with a remote artifact store: a batched call's split output before upload (a local store puts it under `artifacts/`) | the worker, when the call ends; a killed worker's stays | no | yes |
| `auth/` | Rust (`barca serve`, `auth.rs`) | owner-only server token state (`state.json`), shared by the environments | the user | no | yes |

## Legacy entries

`metadata.db.base`, `metadata.db.prev`, `metadata.db.pull-*` and `metadata.db.push-*` were
written by barca 0.25 and earlier. Current barca neither writes nor reads them.

## Gaps

- A killed worker's `.batch-<pid>-<call>/` at the top level and a killed run's
  `live-logs/<run_id>.jsonl` are measured but never removed by barca.
- `inspection-cache/` is never deleted by barca; its writer keeps it under 8 MiB.
