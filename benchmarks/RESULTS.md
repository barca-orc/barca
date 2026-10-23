# Benchmark Results

Latest passes: 2026-10-23, bounded run records under `barca serve` at 200k keys (first section
below); 2026-10-22, keyed partition sources at 10k to 200k keys (second), and the read routes of `barca serve` and the inspection commands at 300 assets (second). Latest comparison pass: 2026-10-21, barca 0.28.0 against Dagster
1.13.26, Prefect 3.9.0 and Airflow 3.3.2 on one Linux machine (third section, #65). The sections after it are older passes on
other hardware and are kept for the record. Absolute times from different machines are not
comparable; within one pass, the ratios between frameworks are.

## Bounded run records under serve (2026-10-23)

`benchmarks/keyed_partitions/serve_ticks.py`: the keyed fixture (`versions -> cleaned -> scored`,
200k retailers, 400k keys) with `freshness=Schedule(...)` on the sensor under `barca serve`, one
retailer's version changed before each tick, the tick's run awaited through `GET /runs`. Same
machine as the next section (32 threads, 62 GiB, other agents building). Before is 0.29.6
(`a1eae781`), after is this change (#623). The before run was stopped at 20 ticks: its RSS was
past 8 GB.

```bash
python benchmarks/keyed_partitions/serve_ticks.py 200000 50 --payloads --idle 2
python benchmarks/keyed_partitions/serve_ticks.py 200000 50 --cron "*/2 * * * * *"
```

| | Before (0.29.6) | After |
|---|---:|---:|
| Outcome stored per tick (`runs.outcome_json`) | 82.6 MB | 1.5 to 2.7 KB |
| Metadata DB growth per tick | 82.7 MB (1.47 GB over 20 ticks) | about 50 KB (2.65 MB over 50 ticks, file size with the WAL checkpointed at both ends); rows: the outcome and 3 materializations |
| Server RSS over ticks | 2.3 GB after tick 1, 8.4 GB after tick 19, about +400 MB per tick | no trend over 50 ticks: 90 MB to 1.8 GB, by whether a tick or the read routes' 200k-key cache (#617) was resident when sampled |
| Same, without returning freed memory to the system | | levels off at 4.6 to 4.9 GB (glibc keeps each thread's arena at its largest run) |
| Tick `elapsed_seconds`, median | 5.46 s (19 ticks, `*/2` cron) | 5.46 s (49 ticks, `*/2` cron); 4.66 s (`*/10` cron) |
| `GET /runs/{id}` | 82.6 MB | 2.6 to 5.9 KB |
| `GET /runs/{id}/events` | 82.6 MB | 2.0 to 4.3 KB |
| `barca history RUN_ID --json` | 2.5 KB | 1.3 to 3.9 KB |
| `barca get scored --json`, `final_output` | 42.8 MB | 277 B (`_barca_partitions` summary) |

- A run recorded before keeps its 82 MB outcome: `GET /runs/{id}` of one from the before pass
  took 6.5 s with the new server and returned it whole.
- With a sensor partitioned by retailer (`--mode partitioned`, 20k keys), every tick executes
  and records 20k sensor steps: the DB grows about 20 MB per tick, by what ran. Its outcome is
  200 KB (the first 1000 computed keys listed), `GET /runs/{id}` lists 1000 of the 20k steps, and
  the event stream (5.8 MB) carries a line per executed key. Use a keyed source.

## Keyed partitions (2026-10-22)

`benchmarks/keyed_partitions`: one tick of a sensor over N retailers feeding a two-asset
`partitions_from` chain (`versions -> cleaned -> scored`, 2N keys), with one retailer's value
changed and everything else cached. Two ways to write the sensor:

- **keyed**: one unpartitioned `@sensor()` returns `{retailer: version}` for every retailer
  (new in 0.29.6); each key folds in its own value only.
- **partitioned**: `@sensor(partitions={"retailer": partitions(RETAILERS)})`, one call per
  retailer on every tick (what worked before), each call as cheap as it can be (one read of the
  versions file per worker process).

### Setup

- **Machine:** AMD Ryzen AI MAX+ 395 (16 cores, 32 threads), 62 GiB RAM, Fedora 44, kernel
  7.2.8, btrfs on NVMe. Not pinned; other agents were building and testing during the pass, and
  the 1-minute load average ranged from 4 to 15.
- **barca:** release builds of `feat/keyed-partition-sources`, default pool (32 workers). The
  keyed rows are 0.29.4 plus this change; the partitioned rows ran on 0.29.2 plus this change,
  before 0.29.4's faster key lookups (#617) were merged in, which at 200k keys took the keyed
  tick from 8.2 s to 7.3 s.
- **What is timed:** `barca get scored pipeline.py --json` from process start to exit
  (`/usr/bin/time`, which also gives the coordinator's peak RSS). Medians of 3 runs; cold is one
  run on an empty `.barca/`. `dry-run` and `status` run after the sensor recorded a new value
  for one retailer (`barca get versions`, not timed).

```bash
KEYS="10000 50000 200000" MODES=keyed benchmarks/keyed_partitions/bench.sh
KEYS="10000 50000" MODES=partitioned benchmarks/keyed_partitions/bench.sh
```

### Results

| Keys | Sensor | Tick, 1 changed | Steps run | Tick, none changed | `--dry-run` | `status` | Peak RSS (tick) | Cold |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 10k | keyed | 0.32 s | 3 | 0.29 s | 0.13 s | 0.17 s | 98 MB | 3.0 s |
| 10k | partitioned | 3.31 s | 10,002 | 4.46 s | 2.70 s | 2.94 s | 205 MB | 4.2 s |
| 50k | keyed | 1.65 s | 3 | 1.64 s | 0.71 s | 0.77 s | 345 MB | 17.5 s |
| 50k | partitioned | 15.9 s | 50,002 | 20.1 s | 17.0 s | 19.5 s | 812 MB | 26.1 s |
| 200k | keyed | 7.25 s | 3 | 7.02 s | 2.83 s | 2.97 s | 1.24 GB | 213 s |

Under `barca serve` with `freshness=Schedule(...)` on the keyed sensor, at 200k keys, the
scheduled ticks recorded 4.3 s to 4.6 s (`elapsed_seconds` of the run, six ticks; 3 steps
executed and 399,998 cached for the tick after the change, 1 and 400,000 for the others). A serve tick
returns nothing to print, so it skips what `barca get` spends on the 200k paths it returns.

Where a 200k-key tick goes (traced with `BARCA_TRACE_TIMING=1` and `perf`, on `barca get`):

| Part | Before this change | After |
|---|---:|---:|
| Deciding the 400k keys: hashing, cache lookups | 8.2 s (one SQL query per key) | 2.6 s (one query per node, 0.4 s each, as 0.29.4 also does (#617); hashing on 8 threads) |
| Stamping `last_read_at` of the results read | 18 s (200k `UPDATE`s) | 0.3 s (a key read in the last hour keeps its stamp) |
| Checking the returned artifacts exist | about 2 s (600k lookups, each key three times) | 1.2 s (200k) |
| The cache size check (`GROUP BY` over every recorded result) | 1.3 s | 0.7 s (unchanged; it varies with load) |
| Returning 200k paths (`get` only) and exiting | about 3 s | about 3 s |
| Whole tick | 31 s | 8.2 s (7.3 s with 0.29.4's key lookups) |

What stands in the way of a tick of a few seconds at 200k keys:

- **Per-key rows.** Every key has its own row in `materializations`, read and hashed each
  tick: about 1 µs to read a row and 1.5 µs to decide a key. Deciding 400k keys costs 2.6 s
  however few changed.
- **Memory.** About 6 KB per key at peak (1.24 GB at 200k): each key's partition key (a
  `BTreeMap`), run hash, recorded output and display id, held in several maps during the run.
- **`barca serve` keeps each run's full outcome.** A tick's outcome lists every target's
  output, here 400k artifacts: 82 MB of JSON written to `runs.outcome_json` per tick, and the
  run, its events and its outcome kept in memory for an hour (the server's RSS grew by about
  600 MB per tick, to 4.8 GB after six ticks). This is the same with a partitioned sensor and is not changed here; at this
  key count it needs a bound on what a run's outcome keeps.

## Read routes and inspection at 300 assets (2026-10-22)

A user with about 300 assets reported that the web UI's state view took 20 s to load. The view
is `GET /state` (the Assets page polls it every 10 s, every 2 s while something runs). This pass
measures every read route of `barca serve` and the inspection commands on a fixture of that size,
before and after the fixes of 0.29.3.

### Setup

- **Machine:** AMD Ryzen AI MAX+ 395 (16 cores, 32 threads), 62 GiB RAM, Fedora 44, kernel
  7.2.8. Other agents were building and testing during the pass (1-minute load average 7 to 22),
  so read differences under about 20% as noise.
- **Builds:** before is `main` at `df4f7171` (0.29.2), after is the 0.29.3 branch; both release
  builds (`maturin develop --release`).
- **Fixture:** `benchmarks/read_routes/generate.py` (defaults): 300 assets and 5 tasks in six
  files, eight layers deep with a fan-in of one to four, 3 sensors and 13 `Schedule(...)` nodes,
  a json/pickle/parquet mix, partitioned assets with 5 and 24 keys and one, `retailer_sales`,
  with 200,000 keys from an expression (the user has about 200k retailers), with a `collect`
  fan-in below it. `seed.py` ran it through barca: two full `get`s (the second after an edit to
  `retailer_sales`, so its keys have two generations of rows), then 2,000 single-asset runs. The
  metadata DB holds 2,036 runs and 410k materialization rows (401 MB).
- **Runner:** `benchmarks/read_routes/bench.py`, which starts `barca serve --no-schedule`, times
  each route's first request after start (cold) and the median of 5 more (warm), a request with
  the `If-None-Match` of the last answer (304), and each CLI command locally and with
  `--server` against that server (first run and median). A route that took over 5 s was timed
  warm once.

### Routes the UI and the CLI read

| Route | Before, cold | Before, warm | After, cold | After, warm | After, 304 |
|---|---:|---:|---:|---:|---:|
| `GET /state` (the state view) | 75.7 s | 76.5 s | 2.2 s ¹ | 2 ms | 2 ms |
| `GET /state` 3 s after a run finished | | | | 4 ms ² | |
| `GET /nodes` (`barca status --server`) | 72.1 s | 77.5 s | 1.8 s | 1 ms | 1 ms |
| `GET /history` (all 2,036 runs) | 1.4 s | 1.6 s | 42 ms | 42 ms | 41 ms |
| `GET /history?limit=20` | 1.3 s | 354 ms | 3 ms | 2 ms | 2 ms |
| `GET /runs?limit=100` (Runs page, every 2 s) | 170 ms | 110 ms | 6 ms | 4 ms | 11 ms |
| `GET /runs/{id}` | 111 ms | 105 ms | 4 ms | 3 ms | 3 ms |
| `GET /runs/{id}/logs` | 95 ms | 96 ms | 2 ms | 2 ms | 2 ms |
| `GET /logs/{id}` | 99 ms | 90 ms | 1 ms | 1 ms | 1 ms |
| `GET /assets/{name}` (node panel, every 10 s) | 119 ms | 118 ms | 2 ms | 2 ms | 2 ms |
| `GET /assets/{name}/schema` | 1.1 s | 881 ms | 155 ms | 150 ms | 156 ms |
| `GET /stats/{name}` | 312 ms | 202 ms | 3 ms | 1 ms | 1 ms |
| `GET /stats/retailer_sales` | 140 ms | 128 ms | 1 ms | 1 ms | 1 ms |
| `GET /assets` | 1 ms | 1 ms | 1 ms | 1 ms | 1 ms |
| `GET /list` | 1 ms | 1 ms | 2 ms | 1 ms | 1 ms |
| `GET /plan` | 65 ms | 62 ms | 151 ms | 79 ms | 67 ms |
| `GET /schedule` | 99 ms | 0 ms | 5 ms | 0 ms | 0 ms |
| `GET /groups`, `/project`, `/health` | 0 to 1 ms | 0 ms | 0 to 1 ms | 0 ms | 0 ms |

¹ The first request in the first second after start, while the server was loading. Five seconds
after start the first `/state` took 4 ms: the server computes it once its database is still.
The first request right after a run finished took 2.1 s; the UI's next poll found it computed.

² While a client polls `/state`, the server computes it again in the background once a change
has settled and none of its runs is going.

| Command | Before, local | Before, `--server` | After, local | After, `--server` (first, then median) |
|---|---:|---:|---:|---:|
| `barca status --json` | 76.1 s | timed out after 30 s, exit 3 | 2.0 s | 1.9 s, then 13 ms |
| `barca status` (table) | 89.2 s | timed out after 30 s, exit 3 | 2.1 s | 11 ms |
| `barca history --json -l 20` | 150 ms (30 ms median) | timed out ³ | 24 ms (7 ms) | 7 ms |
| `barca list --json` | 334 ms | 7 ms | 280 ms | 7 ms |
| `barca stats retail_056 --json` | 204 ms | 11.6 s ³ | 143 ms | 6 ms |
| `barca stats retailer_sales --json` | 156 ms | 99 ms | 146 ms | 7 ms |
| `barca get` (every asset cached) | 14.1 s | | 3.6 s | |

³ The server was still computing the `status` requests the client had abandoned.

### With a `file://` artifact store

A second fixture, the same generator with 20,000 keys (`--keys 20000`) and seeded with a
`file://` store configured from the start (300 single-asset runs; 40k objects in the store),
so `barca status` also compares each cached result with the store's copy:

| | Before | After, cold | After, warm |
|---|---:|---:|---:|
| `GET /state` | 10.3 s | 395 ms | 2 ms |
| `GET /nodes` | 10.0 s | 363 ms | 1 ms |
| `GET /state` right after a run | 10.2 s | 465 ms | 3 ms 3 s later |
| `barca status --json` | 10.4 s | 493 ms | |
| `barca status --json --server` | 10.9 s | 430 ms | 13 ms |
| `barca get` (every asset cached) | 1.8 s | 0.5 s | |

### What was slow

- **Node history** (`barca status`, `/state`, `/nodes`): four queries per node filtered node
  ids with `substr(node_id, ...)`, which cannot use the node id index: four scans of the
  410k-row table per node, 1,200 scans in all. They are index range scans now (`node_id >=
  'n[' AND node_id < 'n\'`), and a node's last change in bytes comes from a partial index of the
  rows whose bytes changed.
- **Sensor outputs** (every dry run and every `get`): `node_id LIKE 'n[%'` scanned the table
  once per sensor or `Manual` node. Same fix.
- **The ETA of a partitioned step** (`barca get`): a `LIKE` scan per step, and a node split into
  one step per worker was asked once per step (66 scans). Asked once per node, by index.
- **`barca history`**: each run's attempt count summed a column over every row of the run (200k
  for a run of the big node). It is counted from indexes now (`idx_mat_attempts` holds only the
  rows whose attempts are not 1).
- **Durations** (`/state`): every row of the table was read on every poll to compute each node's
  median. Now each node's last 20 successes are read below its latest row.
- **The 200k cache decisions**: each key's lookup compiled its SQL statement again (7 µs a key).
  A node with many keys now has its key rows read in one pass when the table holds at most
  eight rows per key, else one prepared lookup per key; a fan-in sorted 200k ids with a copy per
  comparison; the dry run copied every output path once per phase.
- **The server, per request**: parsed every source file and evaluated the 200k-key expression in
  Python, copied the 401 MB database, and decided every node again. It now keeps the graph, one
  copy of the database and the status results until a source or the database changes, answers
  unchanged polls with `304` (`ETag`), computes status on the blocking pool (a `/health` waited
  2.5 s behind one), and computes `/state` ahead of the UI's polls.

### What still misses the targets

- `barca status` takes 2 s on this fixture against a 1 s target, and the server's first
  `/state` or `/nodes` after a change about the same. The time is the cache decision for each of
  the 200k keys (the run hash of each, its lookup, its output record), the work `barca get` does
  to decide what runs. Summarizing the output would not help: `status` already reports a
  partitioned node as counts and at most 20 missing keys; computing the counts is the cost.
- `GET /assets/{name}/schema` reads artifact shapes in a Python process (150 ms), on demand.

Commands (from the repository root; the fixture is about 1.5 GB, under
`benchmarks/read_routes/.fixture`):

```bash
benchmarks/read_routes/bench.sh                      # generate, seed (about 45 min), measure
benchmarks/read_routes/bench.sh <dir>                # measure a seeded project
python benchmarks/read_routes/bench.py <dir> --barca <path> --only /state,/nodes,status
```

## Re-run on current versions (2026-10-21)

### Setup

- **Machine:** AMD Ryzen AI MAX+ 395 (16 cores, 32 threads), 62 GiB RAM, Fedora 44, kernel
  7.2.8. No container.
- **Pinning:** every measured process ran under `taskset -c 16-31`. CPUs 16-31 are the SMT
  siblings of cores 0-15, where other agents were building and testing during the pass, so the
  measured processes shared physical cores with that load. The 1-minute load average is
  recorded before and after each scenario below; it ranged from 1 to 15.
- **Workers:** `BARCA_POOL_SIZE=16` and `BARCA_BENCH_WORKERS=16` (Prefect's `max_workers`),
  one per pinned CPU, except where a row says otherwise. Dagster's `materialize()` runs every
  step in process, one after another (see the README). Airflow runs `airflow dags test`, which
  without `--use-executor` also runs every task in process, one after another.
- **Versions:** barca 0.28.0 is a release build (`maturin develop --uv --release`, rustc
  1.99.0) of `main` at `8ce67c84`, the same code as the `v0.28.0` tag apart from the version
  bump. The older barca versions are the PyPI wheels. Dagster 1.13.26, Prefect 3.9.0 (released
  that morning; 3.8.8 was the previous release) and Airflow 3.3.2 with its 3.13 constraints
  file, each in its own uv venv. Python 3.13.16 everywhere except the Dagster project scaffold
  (Python 3.14.8, chosen by `create-dagster`).
- **Runner:** `hyperfine` is not installed, so `benchmarks/lib/interleave.py` ran each scenario:
  one warm-up round, then rounds of every command in turn (A B C A B C ...) so that load
  changes hit all frameworks alike. Tables give the median and the min–max range. Each timed
  run is a fresh process; the barca, Dagster and Prefect metadata stores persist across runs
  within a scenario, as with `bench.sh`.
- **What is timed:** wall time of each framework's `run.sh` (process start to exit), except
  Airflow, where `run.py` times `airflow dags test` and leaves out the `airflow db migrate`
  before it. barca runs with `--refresh-all`, so every step executes.

### Small pipelines

| Benchmark | barca 0.28.0 | Dagster 1.13.26 | Prefect 3.9.0 | Airflow 3.3.2 | Runs | Load before → after |
|---|---:|---:|---:|---:|---|---|
| trivial (1 asset) | 64 ms (61–74) | 1.07 s (0.90–1.26) | 5.09 s (4.81–5.34) | 5.05 s (4.80–5.84) | 10 | 10.9 → 9.7 |
| chain_100 | 112 ms (103–124) | 1.88 s (1.57–2.09) | 5.16 s (4.80–7.78) | 10.3 s (9.7–12.9) | 10 (Airflow 5) | 13.2 → 7.4 |
| deep_diamond (18) | 310 ms (265–342) | 1.19 s (1.05–1.31) | 5.13 s (4.68–5.70) | 6.45 s (5.97–7.35) | 10 | 7.4 → 5.7 |
| fan_out_500_50ms | 2.24 s (2.22–2.44) | 30.2 s (29.7–30.8) | 6.53 s (6.19–10.03) | 54.6 s (53.5–56.1) | 5 (Airflow 3) | 5.3 → 12.9 |

barca 0.27.0 from PyPI ran in the same rounds: 63 ms, 126 ms, 310 ms and 2.28 s.

Ratios to barca: Dagster 17×, 17×, 3.8× and 13×; Prefect 79×, 46×, 17× and 2.9×; Airflow
79×, 92×, 21× and 24×.

The trivial barca time moved between passes on the same build: 64 ms in this pass, 99 ms and
79 ms in two later passes when the machine was less loaded (table "barca across versions").
Read differences of a few tens of milliseconds as noise.

Commands (from the repository root, with each framework's `.venv` symlinked to its venv and
`AIRFLOW_BIN` set):

```bash
export BARCA_POOL_SIZE=16 BARCA_BENCH_WORKERS=16
B=benchmarks/trivial
python3 benchmarks/lib/interleave.py trivial.json --runs 10 --warmup 2 --internal airflow \
  --cmd "barca=$B/barca/run.sh" \
  --cmd "barca-0.27.0=BARCA=/path/to/barca-0.27.0/bin/barca $B/barca/run.sh" \
  --cmd "dagster=$B/dagster/run.sh" --cmd "prefect=$B/prefect/run.sh" \
  --cmd "airflow=$B/airflow/run.sh"
# chain_100: --runs 10 --warmup 1 --runs-for airflow=5
# deep_diamond: --runs 10 --warmup 1
# fan_out_500_50ms: --runs 5 --warmup 1 --runs-for airflow=3
```

### Partitioned: 10 steps × 1000 partitions = 10,000 steps

| Framework | Pattern | Time | Runs |
|---|---|---:|---|
| barca 0.28.0 | `partitions()` | 2.27 s (2.14–2.40) | 5 |
| barca 0.27.0 | `partitions()` | 3.08 s (2.85–3.40) | 5 |
| Dagster 1.13.26 | `StaticPartitionsDefinition`, one `materialize()` per key | 141 s (137–144) | 3 |
| Prefect 3.9.0 | `task.map()` | killed at 10 min; 3,050 of 10,000 task runs completed | 1 |
| Airflow 3.3.2 | `expand()`, `dags test`, PostgreSQL 18 metadata DB | killed at 10 min; 1,000 of 10,000 task instances succeeded | 1 |

Load average 15.2 → 5.9 (barca, Dagster), 6.3 → 2.9 (Prefect, Airflow). Each framework runs in
a child of `scripts/run_all.py`, which kills its process group after `BENCH_TIMEOUT` (600 s);
the time includes the child's imports. The Prefect count is from the temporary server's
`task_run` table and the Airflow count from `task_instance`, read after the kill. PostgreSQL ran
in a podman container whose processes were pinned to CPUs 16-31.

```bash
cd benchmarks/partitioned_10k/scripts
BARCA=/path/to/barca python3 run_all.py barca
/path/to/dagster-venv/bin/python run_all.py dagster
/path/to/prefect-venv/bin/python run_all.py prefect
AIRFLOW_BIN=/path/to/airflow-venv/bin/airflow \
  AIRFLOW_SQL_ALCHEMY_CONN=postgresql+psycopg2://airflow:airflow@127.0.0.1:55432/airflow \
  /path/to/airflow-venv/bin/python run_all.py airflow
```

(each under `taskset -c 16-31`, through `interleave.py` with `BENCH_RESULTS=/dev/stdout` and
`--internal` for every label, so the time `run_all.py` reports is the one used).

### barca across versions, same machine

To separate barca's own changes from the change of machine, the PyPI wheels of the versions
the June pages quoted ran on this machine, interleaved with 0.28.0 (`--no-cache` in place of
`--refresh-all`, which those versions did not have). Load average 1.6–4.

| Benchmark | barca 0.1.5 (2026-06-05) | barca 0.2.0 (2026-06-10) | barca 0.28.0 |
|---|---:|---:|---:|
| trivial | 42 ms (38–107); 32 ms in a 20-run recheck | 242 ms (238–278) | 99 ms (84–207); 79 ms in the recheck |
| chain_100 | 53 ms (46–99) | 255 ms (250–296) | 143 ms (115–385) |
| deep_diamond | 87 ms (75–91) | 1.39 s (1.36–1.39) | 360 ms (265–385) |
| fan_out_500_50ms | 1.69 s (1.69–1.70) | 5.28 s (5.27–5.28) | 2.31 s (2.23–2.37) |
| partitioned 10k (`get step_9`) | 0.42 s (0.40–0.42), 5 runs | not run | 2.27 s (table above) |

### Partition scaling (`benchmarks/partition_scaling`)

Three assets over N keys (`fetch → normalize → score`), `barca get score assets.py`,
`BARCA_POOL_SIZE=8` to match the #456 and #510 measurements. Cold deletes `.barca/` before
every run; warm runs with everything cached. Coordinator peak RSS from `/usr/bin/time -f %M`.

| Case | barca 0.24.2 | barca 0.26.1 | barca 0.27.0 | barca 0.28.0 |
|---|---:|---:|---:|---:|
| 2k cold | 1.99 s / 108 MB | 1.45 s / 78 MB | 1.51 s / 79 MB | 1.15 s / 76 MB |
| 2k warm | 330 ms / 41 MB | 400 ms / 50 MB | 413 ms / 50 MB | 314 ms / 47 MB |
| 20k cold | 300 s / 5.3 GB (1 run) | 11.7 s / 0.58 GB (1 run) | 11.6 s / 0.58 GB | 6.91 s / 0.60 GB |
| 20k warm | not run | not run | 3.25 s / 0.24 GB | 2.23 s / 0.24 GB |

Medians of 5 runs at 2k and 3 at 20k unless marked. 0.26.1, 0.27.0 and 0.28.0 ran interleaved;
0.24.2 ran afterwards on its own. Load average 1.5–4.8.

### Planning pipeline (`benchmarks/planning_pipeline`, 116 nodes)

The fixture from #250 (74 assets, 42 tasks, 262 edges; DuckDB, pandas and polars in every
worker), generated with `generate.py DEST`, `BARCA_POOL_SIZE=4` as in its tests.

| Case | Command | Steps run | barca 0.27.0 | barca 0.28.0 |
|---|---|---|---:|---:|
| cold | `barca run <all 42 tasks> assets.py --json` after deleting `.barca/` | 114 | 688 ms (608–702) | 674 ms (632–730) |
| warm | `barca get assets.py --json` | 0 (74 cached) | 30 ms (29–33) | 23 ms (23–25) |
| helper edit | `barca get assets.py --json` after editing the function in `drivers.py` that three driver assets call | 10 | 419 ms (405–440) | 403 ms (390–423) |

Each helper-edit run changes the multiplier in that function to a value not used before, so
every run sees a new edit. 5 runs cold and edit, 10 warm. Load average 2.1–4.7.

### Install, import and a two-asset pipeline

| Metric | barca 0.28.0 | Dagster 1.13.26 | Prefect 3.9.0 | Airflow 3.3.2 |
|---|---:|---:|---:|---:|
| Packages in a fresh venv (`uv pip install X`) | 1 | 48 | 103 | not measured |
| Venv size (`du -sm`) | 31 MB | 81 MB | 147 MB | not measured |
| Lockfile entries (`uv lock`, project depending on X) | 2 | 53 | 106 | not measured |
| Wheel (manylinux x86_64) | 13.9 MB | 2.0 MB | 7.0 MB | not measured |
| `python -c "import X"` (bare `python -c pass`: 13 ms) | 42 ms | 365 ms | 502 ms | 733 ms (`airflow.sdk`) |
| Two-asset pipeline, cold | 73 ms (67–76) | 3.64 s (3.45–3.94) | 4.59 s (4.37–4.91) | not measured |
| Same pipeline, everything cached | 13 ms (13–15) | not measured | not measured | not measured |

The `create-dagster@1.13.26 project` scaffold, which adds `dagster-dg-cli` and
`dagster-webserver` as dev dependencies, installs 98 packages (184 MB, 112 lock entries). The
two-asset pipeline is `raw_data → summary` (three rows, count and sum): `barca get pipeline.py`,
`dg launch --assets raw_data,summary` in that scaffold, and `python pipeline.py` with a
`@flow` calling two `@task`s. 10 runs each, interleaved, load average 2.7 → 1.6. `import barca`
spends most of its 29 ms above bare Python in `barca.api`, which imports `urllib.request`.

### What changed since June, and why

- **Partitions.** The 20k-key cold run went from 300 s and 5.3 GB of coordinator memory on
  0.24.2 to 11.7 s and 0.58 GB on 0.26.1: #456 (in 0.24.3) replaced the K² ordering edges
  between partition keys with one gate per step and indexed recorded outputs. 0.28.0 cuts the
  20k cold run again to 6.9 s and the warm run from 3.25 s to 2.23 s: #510 prepares each
  per-row metadata write once per transaction instead of once per row. #515 (failed keys'
  dependents) is not exercised here, since no key fails. The partitioned 10k comparison
  follows: 3.08 s on 0.27.0, 2.27 s on 0.28.0.
- **10k against June's 0.7 s.** barca 0.1.5 runs the same file in 0.42 s on this machine. It
  recorded no result per key (`final_output` is `null`), so a later run could not reuse a
  partition. By 0.18.0 each key was cached and recorded on its own, which is where the time
  between 0.42 s and 2.27 s goes.
- **Fixed cost per run.** 0.1.5 ran the trivial asset in 32–42 ms, 0.28.0 in 64–99 ms. With
  `BARCA_TRACE_TIMING=1` on 0.28.0 the worker's Python start and connect take about 30 ms and
  the run ledger, read stamps and persistence about 15 ms after the step finishes. 0.2.0's
  242 ms was the per-worker 200 ms shutdown sleep that the 2026-07-17 pass below fixed.
- **Workers start one at a time.** deep_diamond takes 87 ms on 0.1.5 and 310–360 ms on 0.28.0,
  fan-out 1.69 s against 2.24–2.31 s. The trace shows the coordinator start a worker, wait
  30–40 ms for it to connect, and only then start the next; a step that finished in the
  meantime is recorded after the wait. deep_diamond starts 6 workers this way (about 210 ms),
  fan-out 16 (about 0.55 s). Not changed in this PR.
- **The other frameworks.** Absolute June numbers came from an Apple Silicon Mac and are not
  comparable with this machine. Within each pass, Dagster stayed at 15–17× barca on trivial
  and went from 12× to 17× on chain_100 and from 12× to 13× on fan-out. Prefect's fan-out
  dropped from 30.7 s to 6.5 s because the harness now submits tasks with `.submit()` to a
  16-thread runner (fixed 2026-07-16); June called them directly, one after another. Airflow
  3.3's `dags test` takes 10 s on chain_100 and 55 s on fan-out, where June's Airflow 3.2.2
  pass recorded 79.5 s and 417 s. Dagster's 10k run took 141 s here against 95 s in June; the
  pattern (one `materialize()` per partition key) is unchanged.

## Standardized re-run via Docker harness (2026-07-17)

Ran via the new `benchmarks/docker/bench.sh` (see `benchmarks/README.md#reproducible-fairness-via-docker-recommended`):
Docker Desktop's Linux VM on an Apple Silicon Mac (M4 Max host, 16 cores/64GB — VM itself
provisioned with 12 vCPU/24GB), container `arm64` (aarch64), **hard-enforced** via `docker
run --cpuset-cpus=0-3 --memory=4g`  — 4 cores, 4GiB RAM, identical ceiling for barca,
Dagster, and Prefect alike, not just reported but actually unexceedable by the container.
barca Python 3.12 (uv venv baked into the image alongside the wheel), Dagster/Prefect each
in their own `uv sync`'d venv, also baked in at image build time so no timed run touches
the network. `/proc/cpuinfo` on this VM's kernel doesn't expose a `model name` field (ARM64
convention — `CPU part`/`CPU implementer` instead), hence "cpu model: unknown" in each run's
banner; two bugs this exposed are fixed as part of this pass, see Notes.
`--warmup 1 --runs 5` per each benchmark's own `bench.sh` defaults (`--warmup 3 --runs 10`
for `trivial`), `BARCA_BENCH_MEMORY=1` requested on every run but unavailable this pass —
see Notes.

**A first attempt on this environment surfaced a real barca-core bug, not just environment
noise — fixed before the table below, not after.** A first full pass showed dagster winning
or roughly tying on 14 of the 20 rows (`deep_diamond`, `etl_duckdb`, `etl_duckdb_dataframes`,
`large_payloads`, `map_reduce`, `mixed_io_cpu`, `multi_file_discovery`, `parallel_tasks`,
`partitioned_etl`, `partitioned_fan_in`, `resilience_pileup` (~tied), `spaceflights`,
`wide_join`, `wide_layers`) — a sharp reversal from the 2026-07-16 pass, where barca won
nearly everything. Re-running `deep_diamond`, `mixed_io_cpu`, and `wide_join` a second time
in isolation reproduced the same ratios within ~2-3%, ruling out sampling noise as the
explanation. `BARCA_TRACE_TIMING=1` (the waterfall tracer already in
`crates/barca-core/src/commands.rs`, see its doc comment) pointed at the actual cause: on
`deep_diamond`, the trace showed 18/18 steps complete by ~285ms into a ~1160ms total run —
the missing ~830ms was entirely inside a single call, `pool.shutdown()`. `fan_out_500`
showed the identical ~830ms shutdown cost despite doing 500x more real work, while
`chain_100` — which never needs more than one worker, since a linear chain has no
independent-branch parallelism to spawn a second one for — paid only ~200ms. That pointed
straight at `WorkerPool::shutdown()` in `crates/barca-core/src/io_loop.rs`: it looped over
every spawned worker *sequentially*, sending SIGTERM and then unconditionally sleeping
200ms before checking whether the process had exited (`graceful_kill()`) — a flat
`pool_size * 200ms` tax on every single run (~800ms for the default 4-worker pool),
regardless of how much or little actual work the run did. This bug isn't new — it's been in
every prior pass too — but a slower CPU (the 2026-07-16 pass's Intel Xeon @ 2.80GHz
container) made Dagster's own compute-bound overhead proportionally larger, which masked
barca's flat, CPU-speed-independent shutdown tax. On the much faster cores here (Apple M4
Max), Dagster's overhead shrank while barca's fixed `sleep()`-based teardown didn't, so it
started dominating wall time on every benchmark whose DAG isn't a single long chain.
**Fixed** in this PR: `shutdown()` now signals every worker first, then polls all of them
together for one shared 200ms grace window instead of sleeping 200ms per worker in
sequence, before falling back to SIGKILL for stragglers. Confirmed via the same tracer:
`deep_diamond`'s `pool_shutdown` phase dropped from ~830ms to ~9ms, and its total run time
roughly halved. The table below is the **post-fix** re-run — barca now wins or ties on
19/20 rows, restoring (and, on several rows, beating) the 2026-07-16 pass's pattern, with
`etl_duckdb` (1.06x dagster) and `large_payloads` (1.05x dagster) the only two within noise
of a tie, consistent with those two being the pass's known cross-process-serialization edge
cases (see the 2026-07-16 section's Notes on `etl_duckdb`).

| Benchmark | barca | dagster | prefect | barca vs dagster | barca vs prefect |
|---|---:|---:|---:|---:|---:|
| trivial | 37.7ms ± 1.7ms | 521.6ms ± 6.7ms | 4.108s ± 0.047s | 13.84x faster | 109.01x faster |
| chain_100 | 123.1ms ± 6.2ms | 1.101s ± 0.017s | 4.128s ± 0.051s | 8.94x faster | 33.53x faster |
| deep_diamond | 161.2ms ± 1.1ms | 652.2ms ± 3.6ms | 4.169s ± 0.050s | 4.05x faster | 25.86x faster |
| etl_duckdb | 1.047s ± 0.077s | 992.7ms ± 13.5ms | 10.241s ± 0.088s | **~tied** (dagster 1.06x) | 9.78x faster |
| etl_duckdb_dataframes | 394.9ms ± 16.2ms | 820.2ms ± 10.7ms | 4.355s ± 0.059s | 2.08x faster | 11.03x faster |
| fan_out_500 | 903.4ms ± 58.2ms | 2.306s ± 0.007s | 4.648s ± 1.032s | 2.55x faster | 5.15x faster |
| fan_out_500_50ms | 7.620s ± 0.053s | 35.415s ± 0.095s | 10.166s ± 0.047s | 4.65x faster | 1.33x faster |
| incremental_backfill | 608.6ms ± 12.2ms | 5.904s ± 0.032s | 41.773s ± 0.918s | 9.70x faster | 68.64x faster |
| large_payloads | 660.3ms ± 9.7ms | 629.1ms ± 5.7ms | 6.193s ± 0.030s | **~tied** (dagster 1.05x) | 9.38x faster |
| map_reduce | 197.0ms ± 8.4ms | 882.1ms ± 5.3ms | 4.192s ± 0.014s | 4.48x faster | 21.28x faster |
| mixed_io_cpu | 310.1ms ± 14.0ms | 1.038s ± 0.005s | 4.465s ± 0.756s | 3.35x faster | 14.40x faster |
| multi_file_discovery | 276.7ms ± 8.7ms | 862.5ms ± 3.9ms | 4.149s ± 0.037s | 3.12x faster | 15.00x faster |
| parallel_tasks | 150.1ms ± 2.0ms | 563.2ms ± 10.2ms | 3.975s ± 0.057s | 3.75x faster | 26.49x faster |
| partitioned_chain | 288.5ms ± 5.7ms | 1.276s ± 0.015s | 3.925s ± 0.057s | 4.42x faster | 13.60x faster |
| partitioned_etl | 149.8ms ± 3.8ms | 818.5ms ± 11.4ms | 3.973s ± 0.060s | 5.46x faster | 26.53x faster |
| partitioned_fan_in | 229.7ms ± 12.4ms | 978.6ms ± 6.6ms | 3.965s ± 0.086s | 4.26x faster | 17.26x faster |
| resilience_pileup | 1.934s ± 0.009s | 2.297s ± 0.028s | 5.915s ± 0.655s | 1.19x faster | 3.06x faster |
| spaceflights | 580.2ms ± 7.9ms | 1.058s ± 0.002s | 4.013s ± 0.046s | 1.82x faster | 6.92x faster |
| wide_join | 133.4ms ± 1.3ms | 599.7ms ± 14.9ms | 3.946s ± 0.061s | 4.50x faster | 29.59x faster |
| wide_layers | 360.6ms ± 6.5ms | 905.7ms ± 2.5ms | 3.910s ± 0.058s | 2.51x faster | 10.84x faster |

### Peak memory

Unavailable this pass. `BARCA_BENCH_MEMORY=1` was set on every run, but the opt-in
whole-process-tree memory measurement (`bench_mem_peak` in `benchmarks/lib/env.sh`) needs
to create its own child cgroup with a writable `cgroup.subtree_control` inside the
already-restricted outer container, which Docker Desktop's container doesn't expose without
`--privileged` (out of scope for a "minimal" harness) — every row correctly falls back to
reporting "memory measurement unavailable" per its documented graceful-degradation path
rather than a misleading number.

**Since fixed (2026-07-20):** the harness now runs the container `--privileged` and the
entrypoint delegates the cgroup v2 memory controller at boot, which makes the cgroup
peak-memory path work under Docker Desktop — first exercised by the scheduler-overhead
pass below. A future full-suite re-run would fill this table too.

### Notes

- **The worker-pool shutdown fix** (`crates/barca-core/src/io_loop.rs`,
  `WorkerPool::shutdown()`): previously, `graceful_kill()` was called once per worker in a
  plain `for` loop — each call sent SIGTERM, then unconditionally `std::thread::sleep`'d
  200ms before its first liveness check, only moving to SIGKILL if the worker was still
  alive after that. With `pool_size` workers (default 4) all needing termination at the end
  of a run, that's up to 4 sequential 200ms sleeps — ~800ms of pure `thread::sleep`, on
  every single `barca get`/`barca run` invocation, independent of workload size. Fixed by
  sending SIGTERM (and SIGCONT-then-SIGTERM for frozen `parallel()` workers) to every
  worker up front, then polling all of them together in one shared 200ms window (5ms poll
  interval) before SIGKILL-ing any stragglers — turning an O(pool_size) cost into O(1). This
  is a real, general barca-core performance fix, not benchmark-specific: any `barca`
  invocation that spawns more than one worker pays this tax on exit, so every real-world
  multi-asset project benefits, not just this suite.
- Two smaller bugs surfaced and fixed while building `benchmarks/docker/`, both specific to
  running the existing benchmark harness on ARM64 Linux for the first time (every prior
  pass ran x86_64):
  - `bench_env_banner()` in `benchmarks/lib/env.sh` unconditionally did
    `cpu_model="$(grep -m1 'model name' /proc/cpuinfo | ...)"`; ARM64's `/proc/cpuinfo` has
    no `model name` line (it uses `CPU part`/`CPU implementer` instead), so `grep` exits 1,
    and under the calling `bench.sh`'s `set -euo pipefail` that aborted the entire benchmark
    before hyperfine ever ran. Fixed by tolerating the failure (`|| true`) for both the CPU
    model and `free -h` lookups, falling back to "unknown" — matches the function's existing
    `${cpu_model:-unknown}` display fallback, which was already written assuming the
    assignment could come back empty but not that it could kill the script outright.
  - The runtime image was also missing `free` (`procps` isn't installed by default in the
    `bookworm-slim` base) — added explicitly for the same banner.
  - Root barca venv (`/work/.venv` in the image, `.venv` in local dev) needs
    `scikit-learn`, `pandas`, and `pyarrow` installed alongside the barca wheel itself —
    `spaceflights/barca/assets.py` and `etl_duckdb_dataframes/barca/assets.py` import them
    directly, and unlike the Dagster/Prefect sides there's no separate per-benchmark barca
    venv to isolate them in. This mirrors `.github/workflows/ci.yml`'s benchmark-smoke-test
    step, which does the same `pip install scikit-learn` / `pip install pandas pyarrow`
    alongside its own wheel-installed barca. Without this, both benchmarks failed their
    first hyperfine warmup run with a barca-side import error before any timing was
    collected.
- The Prefect `.submit()` fix from the 2026-07-16 "Further note" above is included in this
  pass (it landed on `main` before this pass ran) — Prefect still loses every single row by
  a wide margin regardless, consistent with the 2026-07-16 pass's conclusion that its high
  fixed per-run overhead dominates regardless of environment or concurrency fixes.

## Standardized re-run (2026-07-16)

Ran on this container: 4 vCPU (Intel Xeon @ 2.80GHz, pinned via `taskset -c 0-3`),
15 GiB RAM, barca Rust release build, Python 3.13 (barca) / 3.12 (Dagster, Prefect).
Worker count = 4 for all three frameworks (`BARCA_BENCH_CORES` default, `--warmup 1
--runs 5` except trivial at `--warmup 3 --runs 10`). Absolute times are inflated by
container overhead (cold PyPI-installed venvs, virtualized CPU, no dedicated
hardware, shared with whatever else is on the host) — read the **relative** ratios,
not the raw ms/s, and don't compare these numbers to the Apple Silicon rows above.

| Benchmark | barca | dagster | prefect | barca vs dagster | barca vs prefect |
|---|---:|---:|---:|---:|---:|
| trivial | 339ms ± 13ms | 1.98s ± 0.15s | 10.74s ± 0.24s | 5.85x faster | 31.68x faster |
| parallel_tasks | 1.28s ± 0.02s | 2.10s ± 0.06s | 11.46s ± 0.94s | 1.64x faster | 8.92x faster |
| resilience_pileup | 2.56s ± 0.02s ⚠ | 3.61s ± 0.04s | 11.75s ± 0.21s | 1.41x faster | 4.59x faster |
| chain_100 | 765ms ± 80ms | 3.88s ± 0.09s | 11.09s ± 0.13s | 5.07x faster | 14.50x faster |
| deep_diamond | 1.49s ± 0.04s | 2.41s ± 0.15s | 11.11s ± 0.17s | 1.62x faster | 7.46x faster |
| etl_duckdb | 4.44s ± 0.10s | 3.59s ± 0.22s | 47.63s ± 0.83s | **dagster 1.24x faster** † | 13.28x faster |
| etl_duckdb_dataframes ‡ | 1.54s ± 0.03s | 2.36s ± 0.05s | 9.56s ± 0.13s | 1.53x faster | 6.19x faster |
| fan_out_500 | 2.69s ± 0.06s | 8.59s ± 0.32s | 12.95s ± 0.15s | 3.20x faster | 4.82x faster |
| fan_out_500_50ms | 9.84s ± 0.55s | 34.89s ± 0.27s | 37.92s ± 0.16s | 3.55x faster | 3.85x faster |
| incremental_backfill | 16.25s ± 0.17s | 20.47s ± 0.29s | 108.52s ± 1.47s | 1.26x faster | 6.68x faster |
| large_payloads | 3.67s ± 0.40s | 2.14s ± 0.07s | 17.70s ± 0.12s | **dagster 1.72x faster** | 8.29x faster |
| map_reduce | 2.57s ± 0.07s | 3.09s ± 0.14s | 10.48s ± 0.04s | 1.20x faster | 4.08x faster |
| mixed_io_cpu | 3.07s ± 0.08s | 3.05s ± 0.11s | 11.27s ± 0.34s | **dagster 1.01x faster** (~tied) | 3.70x faster |
| multi_file_discovery | 3.16s ± 0.51s ⚠ | 3.15s ± 0.15s | 10.73s ± 0.33s | **~tied** (1.00x) | 3.40x faster |
| partitioned_chain | 3.37s ± 0.13s | 4.32s ± 0.05s | 11.45s ± 0.50s | 1.28x faster | 3.40x faster |
| partitioned_etl | 2.96s ± 0.06s | 2.95s ± 0.17s ⚠ | 10.98s ± 0.14s | **~tied** (1.00x) | 3.72x faster |
| partitioned_fan_in | 3.43s ± 0.11s | 3.49s ± 0.07s | 10.81s ± 0.11s | 1.02x faster (~tied) | 3.15x faster |
| spaceflights | 4.34s ± 0.22s | 4.01s ± 0.20s | 12.26s ± 0.17s | **dagster 1.08x faster** | 3.06x faster |
| wide_join | 3.14s ± 0.13s | 2.19s ± 0.08s | 11.18s ± 0.26s | **dagster 1.43x faster** | 5.10x faster |
| wide_layers | 3.53s ± 0.61s ⚠ | 3.47s ± 0.10s | 11.09s ± 0.21s | **~tied** (1.02x) | 3.19x faster |

⚠ = hyperfine flagged statistical outliers for that row's barca (or, for
partitioned_etl, dagster) measurement — see variance notes below.

† = stale as of the serialization fix described in Notes — `etl_duckdb`'s two
heaviest assets now use `serializer="pickle"`, closing this to ~1.07-1.14x
dagster-faster (within noise). The row above is left as originally measured
since it's what motivated the fix; see Notes for before/after numbers. ‡ =
`etl_duckdb_dataframes` was added later as a follow-up benchmark (same
topology, DataFrame/parquet payloads instead of dict-of-rows) and measured in
isolation via `bench.sh 8 2`, not as part of this table's single full-suite
pass — see Notes before comparing its absolute times to the other rows here.

### Variance

Relative standard deviation (σ/mean) per framework, averaged across the original
19 full-suite-pass rows (excludes `etl_duckdb_dataframes`, added later and
measured separately — see the table's ‡ footnote):
**barca ~5.2%, dagster ~3.6%, prefect ~1.9%**. Barca is consistently the noisiest
of the three here, and four rows crossed hyperfine's outlier-detection threshold
(resilience_pileup, multi_file_discovery, partitioned_etl's dagster leg,
wide_layers) — worst case was wide_layers at σ = 17% of the mean. Prefect and
dagster are quieter because their per-run cost is dominated by fixed Python
import/framework overhead, which swamps scheduler jitter; barca's per-run cost is
smaller in absolute terms, so the same container noise is a larger fraction of it.
This reads as environment noise (a shared, virtualized 4-vCPU container), not a
barca-specific issue — but it does mean any individual row within ~1.3x of 1.0
(the "~tied" ones above) shouldn't be read as a confident win either direction
without more runs or a quieter machine. `benchmarks/lib/env.sh` pins cores via
`taskset`, which helps, but can't fully isolate a shared host from noisy
neighbors the way dedicated hardware would.

### Peak memory (whole process tree)

Collected separately via `BARCA_BENCH_MEMORY=1` (opt-in — see
`benchmarks/README.md#methodology` — not part of the timed hyperfine runs above,
so it isn't in the variance figures either):

| Benchmark | barca | dagster | prefect |
|---|---:|---:|---:|
| trivial | 55.8 MB | 156.7 MB | 410.5 MB |
| chain_100 | 32.2 MB | 132.9 MB | 408.8 MB |
| deep_diamond | 62.6 MB | 120.5 MB | 373.7 MB |
| etl_duckdb | **287.6 MB** | 265.8 MB | 482.4 MB |
| etl_duckdb_dataframes | 258.2 MB | 174.4 MB | 386.7 MB |
| fan_out_500 | 66.1 MB | 136.5 MB | 404.1 MB |
| fan_out_500_50ms | 64.4 MB | 137.4 MB | 460.6 MB |
| incremental_backfill | 25.1 MB | 113.4 MB | 378.0 MB |
| large_payloads | 62.2 MB | 124.6 MB | 384.9 MB |
| map_reduce | 56.2 MB | 116.9 MB | 376.3 MB |
| mixed_io_cpu | 53.3 MB | 112.2 MB | 359.4 MB |
| multi_file_discovery | 56.6 MB | 116.4 MB | 373.6 MB |
| parallel_tasks | 63.2 MB | 117.0 MB | 395.4 MB |
| partitioned_chain | 54.7 MB | 119.6 MB | 381.7 MB |
| partitioned_etl | 56.1 MB | 115.1 MB | 370.4 MB |
| partitioned_fan_in | 55.2 MB | 117.2 MB | 371.8 MB |
| resilience_pileup | 52.8 MB | 119.5 MB | 372.5 MB |
| spaceflights | **409.4 MB** | 322.5 MB | 563.7 MB |
| wide_join | 54.6 MB | 87.6 MB | 361.6 MB |
| wide_layers | 55.6 MB | 90.8 MB | 371.5 MB |
| **median** | **56.1 MB** | **119.5 MB** | **378.0 MB** |

Barca's footprint is roughly half dagster's and a sixth to an eighth of prefect's
almost everywhere — except `etl_duckdb` and `spaceflights`, which spike to match
or exceed dagster's own footprint. Those are the same two benchmarks flagged below
as barca's genuine (not noise) losses, and the memory spike is the same root cause
showing up a second way — see Notes.

### Notes

- **Barca does not universally win here, and most of that turned out to be noise
  rather than a real result — verified, not assumed.** The table above was
  generated over roughly an hour; re-running individual rows afterward, in
  isolation, showed several "dagster wins" reverse completely (`wide_layers`:
  dagster-favored in the table above → barca 2.17x faster on rerun; `wide_join`:
  dagster 1.43x faster above → barca 1.29x faster on rerun; `large_payloads`:
  dagster 1.72x faster above → ~tied, barca 1.05x faster on rerun). The tell is in
  the CPU-time breakdown hyperfine already reports: dagster's `(user+sys)/wall` is
  ≈1.00 on almost every row (it's single-process and sequential — script mode, per
  the Execution modes table below — so wall time *is* CPU time, nothing hidden).
  Barca's ratio sits at 0.3–0.5 almost everywhere: most of its wall-clock time is
  spent *waiting* (process coordination, socket IPC, DB writes), which is exactly
  what a shared, virtualized container's scheduler jitter hits hardest — a
  multi-process architecture has more "waiting" surface for noise to land on than
  a single sequential process does.
- **`etl_duckdb` is the one benchmark confirmed real, not noise** — re-run twice
  (~1.24x, then ~1.40x dagster-faster) under different container load and it held
  both times, unlike the others above. Its CPU-time pattern is also qualitatively
  different: barca's *user* time is higher than dagster's here (barca is doing
  more total compute, not just waiting), and its peak memory (287.6 MB) roughly
  matches dagster's rather than sitting far below it like every other benchmark.
  The mechanism: `etl_duckdb` generates three separate 100k-row datasets
  (`raw_orders`, `raw_customers`, `raw_products`) that flow through *parallel*
  raw→staging→intermediate branches before merging. Barca's coordinator + separate
  Python workers is genuinely multi-process, so each parallel branch's 100k-row
  payload gets serialized to a JSON artifact on disk and deserialized by the next
  worker — real, repeated cross-process I/O and duplicated in-memory copies.
  Dagster's script-mode execution is single-process and sequential, so it just
  passes Python objects by reference between steps — zero serialization cost.
  `spaceflights` (sklearn models/dataframes, also flagged above) shows the same
  memory-spike signature and is worth the same suspicion, though it wasn't
  independently re-run to confirm reproducibility the way `etl_duckdb` was.
  `large_payloads` looks superficially similar (also large payloads) but is a
  *linear* chain — one worker handles it start to finish with no cross-process
  handoff — which is consistent with it turning out to be noise, not a real cost:
  the tax shows up when large payloads *and* parallel branching combine, not from
  payload size alone.
- **`etl_duckdb`'s loss was investigated further and substantially fixed** (not
  just diagnosed). Root cause was two-layered: (1) the cross-process
  serialization cost above was real, but (2) barca's own per-step timing was
  *undercounting* it — `_materialize()` in `python/barca/_worker.py` measured
  `elapsed`/`cpu_seconds` before calling `serialize()`, so the actual artifact
  write (the expensive part for large payloads) was invisible to barca's own
  cost model, `barca stats`, and `barca history`. Fixed by moving the
  measurement to wrap `serialize()` too — confirmed via a `BARCA_TRACE_TIMING=1`
  waterfall trace (new, permanent, zero-cost-when-unset debug env var) that
  `raw_orders`'s self-reported cost jumped from a wrong 396ms to a correct
  ~1049ms, and the run's previously-"unaccounted" ~1.6s coordination gap shrank
  to ~0.37s of legitimate, understood overhead (worker spawn + DB init/persist).
  This fix benefits any barca project with large-payload assets, not just this
  benchmark. Two follow-up changes then closed most of the actual gap:
  - **Pickle quick win** (`benchmarks/etl_duckdb/barca/assets.py`): added
    `serializer="pickle"` to `raw_orders` and `stg_orders`, the two heaviest
    payloads (14.9MB/8.1MB as JSON vs 5.3MB/3.4MB as pickle — pickle is both
    smaller and much faster to (de)serialize for this list-of-small-dicts
    shape: measured `json.dump` ~635ms vs `pickle.dump` ~64ms for
    `raw_orders`). This is an existing, already-shipped `@asset()` kwarg, not
    an engine change. Result, two confirming isolated runs
    (`bench.sh 8 2`): dagster's lead narrowed from 1.24-1.40x to **1.07x, then
    1.14x** — within noise of tied. One side effect worth documenting: pickled
    `raw_orders` drops under the in-process `_ArtifactLRU`'s 8MB cache
    threshold (`python/barca/_worker.py`), making it newly eligible for
    caching — but its only consumer runs on a different worker process, so the
    cache can never hit and the `copy.deepcopy()` mutation-safety guard
    (measured ~469ms for this payload) is pure waste on every read. Left
    unfixed (out of scope, core-engine cache logic) but noted for anyone
    chasing a similar case where deepcopy cost outweighs a cache that can't be
    hit.
  - **DataFrame + parquet rewrite** (new sibling benchmark,
    `benchmarks/etl_duckdb_dataframes/`, same 11-asset topology across all
    three frameworks so the comparison isn't conflated with a framework
    difference): rewrote each asset from manual Python loops over dict-of-rows
    to vectorized pandas (`groupby`/`merge`/`assign`). Barca auto-detects
    DataFrame return values and routes them to parquet
    (`detect_format()`/`resolve_format()` in `python/barca/_artifacts.py`) —
    no code changes needed beyond returning DataFrames; confirmed via
    `.barca/artifacts/` that every asset except the two dict-of-scalars mart
    outputs serialized as `.parquet`. This is a categorically faster
    mechanism than JSON or pickle for tabular data (vectorized, typed-array
    (de)serialization via pyarrow's C++ reader/writer, not Python-level object
    traversal), not just a smaller payload. Result: barca flipped from
    *losing* to dagster to **winning 1.53-1.57x faster**, confirmed across two
    isolated timing runs (1.55x, 1.53x) plus a `BARCA_BENCH_MEMORY=1` pass
    (1.57x, peak memory 258.2 MB vs dagster's 174.4 MB — both frameworks'
    memory dropped from the dict-of-rows version, since DataFrames are more
    memory-efficient than nested dicts in both). All three frameworks produce
    identical output values on this workload (exact match, not just within
    tolerance).
  - Net: the original `etl_duckdb` loss traced to a fixable barca-side gap
    (missing format optimization for large/tabular payloads), not an inherent
    architectural ceiling — the multi-process, cross-process-serialization
    design this benchmark stresses is real, but the *format* used for that
    serialization was the actual lever, and the DataFrame/parquet path is the
    strictly better one when the workload is tabular.
- Prefect is unaffected by any of the above — it loses every benchmark by a wide
  margin regardless of environment or re-run, consistent with a much higher fixed
  per-run overhead swamping everything else (confirmed by its own peak memory,
  6-8x barca's baseline, and `(user+sys)/wall` also ≈1.0 — it's not parallelizing
  in script mode either).
- **Takeaway**: don't trust any single hyperfine table from a shared/virtualized
  host at face value — re-run rows that look surprising in isolation before
  drawing conclusions, and check whether the CPU-time ratio and (if available)
  peak memory corroborate the wall-clock story. A re-run of this whole suite on
  dedicated hardware would settle both the noise question and give a cleaner
  baseline for the `etl_duckdb`/`spaceflights` cross-process-serialization cost.
- Fixed three pre-existing bugs uncovered while running this, unrelated to the
  CPU/RAM standardization work itself:
  - `parallel_tasks/{barca,dagster,prefect}/run.sh` called bare `barca`/`python`
    instead of the pinned venv binaries (worked only if a venv happened to
    already be active on `PATH`).
  - `resilience_pileup/barca/run.sh` was missing `--no-cache`, so every run after
    the first was a cache-hit no-op (measured: 2.5s cold vs 8ms warm) — the
    historical "330ms" figure above is suspect for the same reason.
    `resilience_pileup/prefect/run.py` had a variable name collision (`t0` was
    both a `@task` and the `__main__` timer, so every run raised
    `TypeError: 'float' object is not callable`) — always broken, not a
    regression from this change.
  - A real correctness bug in `barca-core`: dynamic partitions (`partitions_from`)
    combined with explicit `inputs={}` wiring to a downstream partitioned
    consumer could silently drop the dependency edge (root cause: partition-key
    chunks get bin-packed across dispatch streams by load alone, with no
    dependency awareness, so a consumer's chunk could be visited before its
    producer's). This broke `partitioned_etl` — confirmed pre-existing via
    bisect, not a regression from the adaptive-executor merge. Fixed in
    `coordinator.rs` (two-pass dependency resolution) and covered by a new Rust
    unit test plus `tests/integration/test_partitions.sh`, both wired into CI
    along with `tests/integration/test_benchmark_examples.sh` (runs every
    benchmark's barca side as a correctness smoke test on every PR).

## Scheduler overhead (daemon mode)

Unlike every table above (one-shot cold start), `scheduler_overhead` compares the
long-running **cron scheduler daemons**: `barca serve` vs `dagster dev` vs
`prefect .serve()`. See [`scheduler_overhead/README.md`](scheduler_overhead/README.md)
for the harness and fairness rationale.

Ran 2026-07-20 via `benchmarks/docker/bench.sh scheduler_overhead`: Docker
Desktop's Linux VM on an Apple Silicon Mac (M4 Max host), container `arm64`,
enforced `--cpuset-cpus=0-3 --memory=4g` — the same environment class as the
2026-07-17 full-suite pass above. Defaults: 3 fire-boundaries sampled per
framework (the observation window spans up to 5 minute-boundaries including
boot slack, hence 4–5 recorded fires), 60s idle window, 20s cadence window.
Unlike the 2026-07-17 pass, **peak memory worked this time**: the harness now
runs the container `--privileged` and delegates the cgroup v2 memory
controller at boot (see `benchmarks/docker/bench.sh` / `entrypoint.sh`) —
without that, Docker mounts `/sys/fs/cgroup` read-only and the measurement
falls back to "unavailable", which is exactly what the 2026-07-17 pass hit.
Caveat: the host was shared with unrelated running containers during this
pass; that adds CPU noise, but the effects measured here (seconds-scale poll
floors, hundreds-of-MB footprint gaps) are far above that noise floor.

Three dimensions:

1. **Trigger latency** — measured at a **1-minute cron** (`* * * * *`), the finest
   cadence all three share, so it is apples-to-apples. Expectation from the
   architecture: barca's ≤1s tick loop vs Dagster's ~30s daemon-eval interval and
   Prefect's ~10–15s runner poll (each floors how soon a fire can follow the tick).
2. **Idle footprint** — peak memory of the daemon holding 10 idle jobs. This is
   the closest head-to-head axis: barca is a single Rust process (workers spawned
   only on fire), Dagster runs a webserver + code server + schedule daemon, and
   Prefect's `.serve()` runs its own ephemeral API in-process.
3. **Minimum cadence** — **barca schedules sub-minute (6-field cron, 1-second
   resolution); Dagster and Prefect cannot express any cron finer than 1 minute.**
   Barca fires ~10× in a 20s window where the other two fire 0–1×.

| Dimension | barca | dagster | prefect |
|---|---|---|---|
| Trigger latency (median, 1-min cron) | **0.062s** | 4.929s | 3.698s |
| Idle footprint (peak MB, 10 jobs) | **7.9 MB** | 473.7 MB | 315.7 MB |
| Minimum cadence | **1 second** | 60 seconds | 60 seconds |

Full trigger-latency distribution (seconds past the minute boundary):

| framework | fires | min | median | p95 | max |
|---|---:|---:|---:|---:|---:|
| barca serve | 5 | 0.061s | **0.062s** | 0.218s | 0.218s |
| dagster dev | 4 | 1.782s | 4.929s | 5.882s | 5.882s |
| prefect serve | 5 | 1.402s | 3.698s | 5.52s | 5.52s |

Reading the results:

- **Latency**: barca's per-second tick loop fires ~60ms after the minute
  boundary, with worst case ~220ms. Dagster and Prefect land 1.4–5.9s late —
  better than their documented worst-case poll intervals (~30s daemon eval /
  ~10–15s runner poll) would allow in the worst case, but 20–80× barca's
  median. Dagster also recorded one fewer fire in the same window (4 vs 5):
  its first minute-boundary passed during daemon boot before the schedule was
  being evaluated.
- **Idle footprint**: barca's daemon holding 10 registered schedules is a
  single idle Rust process — **7.9 MB** peak over boot + 60s idle (workers
  only spawn when something fires; these jobs never fire). Dagster boots a
  webserver + code server + schedule daemon (473.7 MB), Prefect an in-process
  ephemeral API (315.7 MB) — a ~40–60× gap, and the closest thing this
  benchmark has to a pure head-to-head axis since nothing is executing.
- **Cadence**: barca's `*/2 * * * * *` fired exactly 10× in the 20s window, as
  scheduled. This axis is a capability difference, not a race — Dagster and
  Prefect cannot express any cron finer than 1 minute (see the fairness notes
  in the README).

## Methodology

### Environment
- **Hardware**: Apple Silicon (M-series), same machine for all benchmarks
- **Barca**: Python 3.14.3 (workspace .venv), Rust release binary
- **Dagster**: Python 3.12.0 (latest compatible), dagster latest
- **Prefect**: Python 3.12.0, prefect latest
- **Airflow**: Python 3.12.0, apache-airflow latest (3.2.2)
- **Python version note**: dagster, prefect, and airflow do not yet support Python 3.14

### Architecture (v0.2.0)

Barca uses a **stateless worker pool** with **Unix domain sockets** for coordination:
- Rust owns a global ready queue and assigns one task at a time to idle workers
- Workers receive tasks, execute, report back — no pre-assigned queues
- `parallel()` uses **SIGSTOP/SIGCONT** to freeze the requesting worker, spawn a temp replacement, and dispatch children across the pool
- Nested `parallel()` works recursively (frozen processes stack, active pool stays at `pool_size`)

### Execution modes tested

| Framework | Mode | Parallelism |
|---|---|---|
| **Barca** | multi-process | Rust spawns N Python workers via UDS (pool_size = cpu_count) |
| **Dagster** (script) | `materialize()` | Sequential in-process (only mode available in script mode) |
| **Dagster** (server) | `dagster dev` + GraphQL | multiprocess_executor spawns subprocess per asset |
| **Prefect** (sequential) | direct task calls | Sequential (default) |
| **Prefect** (parallel) | `ConcurrentTaskRunner` + `.submit()` | Thread-based concurrency (max_workers=16) |
| **Airflow** (dags test) | `dags test` | Sequential in-process (testing mode) |
| **Airflow** (LocalExecutor) | scheduler + trigger | Parallel subprocesses (requires PostgreSQL) |

### Measurement
- hyperfine with --warmup 2-3 for sub-100ms benchmarks (5-10 runs)
- hyperfine with --warmup 1 for longer benchmarks (3-5 runs)
- All times are wall-clock cold start (process spawn to exit)

## Summary: script mode (cold start)

All frameworks invoked as scripts — no pre-started servers.

| # | Benchmark | barca | dagster | prefect | airflow |
|---|---|---:|---:|---:|---:|
| 1 | Trivial (1 asset) | **38ms** | 500ms | 3.6s | 3.5s |
| 2 | Chain 100 (linear) | **72ms** | 999ms | 3.7s | 83s |
| 3 | Fan-out 500 (no work) | **439ms** | 2.1s | 4.3s | — |
| 4 | Fan-out 500×50ms | **2.2s** | 31.5s | 31.2s | 461s |
| 5 | Spaceflights (sklearn) | **720ms** | 1.1s | 3.8s | — |
| 6 | Deep Diamond (18 assets) | **159ms** | 678ms | 4.0s | 17s |
| 7 | Wide Layers (63 assets) | **1.2s** | 919ms | 3.9s | — |
| 8 | Mixed I/O+CPU | **334ms** | 1.1s | 4.0s | — |
| 9 | Large Payloads (10k rows) | **210ms** | 631ms | 6.0s | — |
| 10 | Map/Reduce (1→50→1) | **443ms** | 917ms | 4.1s | — |
| 11 | Multi-file (50 files) | **60ms** | 911ms | 4.0s | — |
| 12 | ETL Pipeline (100k rows) | **500ms** | 953ms | 14.2s | — |
| 13 | Wide Join (10→1) | **267ms** | 635ms | 4.1s | — |
| 14 | Backfill (10-step × 10) | **282ms** | 6.2s | 40.1s | — |
| 15 | Partitioned Chain (3×50) | **1.2s** | — | — | — |
| 16 | Resilience Pileup | **330ms** | — | — | — |
| 17 | parallel(1000) tasks | **1.7s** | — | — | — |
| 18 | Nested parallel (3×5) | **650ms** | — | — | — |

*Rows 5-14 dagster/prefect numbers from 2026-06-03 run; barca numbers updated 2026-06-10.*

## Internal performance profile

Profiler thresholds (all pass):

| Check | Measured | Threshold |
|---|---:|---:|
| Trivial (total) | 31ms | <100ms |
| Plan 100 nodes | 5.6ms | <50ms |
| Plan 2002 nodes | 21ms | <500ms |
| Per-step overhead | 0.40ms | <1.0ms |

### Timing breakdown (trivial, 1 asset)

| Phase | Time |
|---|---:|
| Rust parse + plan | 4ms |
| Python process spawn | ~18ms |
| UDS connect + execute + serialize | ~5ms |
| DB init + write | ~5ms |
| **Total** | **32ms** |

Python process startup is the dominant fixed cost. For workloads with >10 steps, it amortizes to <0.6ms/step.

## Parallel mode comparison (fan_out_500_50ms)

500 independent tasks, each sleeping 50ms. Sequential minimum: 25.0s.

| Framework | Mode | Time | Speedup vs sequential |
|---|---|---:|---:|
| **Barca** | 16 worker processes (UDS) | **2.2s** | 11.4x |
| **Prefect** | ConcurrentTaskRunner (16 workers) | **4.2s** | 6.0x |
| Dagster | script mode (sequential) | 31.5s | 1.0x |
| Prefect | script mode (sequential) | 31.2s | 1.0x |
| Dagster | server + multiprocess_executor | 68.5s | 0.5x (slower than sequential!) |

### Dynamic parallel dispatch (v0.2.0)

`parallel()` at runtime — dispatches tasks dynamically, not known at plan time.

| N items | Time | Notes |
|---:|---:|---|
| 10 | 300ms | includes process spawn overhead |
| 200 | 530ms | previously hung in v0.1.x |
| 1000 | 1.7s | 1000 unique results, SIGSTOP/SIGCONT |
| Nested 3×5 | 650ms | 3 outer × 5 inner = 15 leaf tasks |

## Reproducing

```bash
cargo build --release && maturin develop --release

# Script-mode benchmarks:
for bench in trivial chain_100 fan_out_500 fan_out_500_50ms spaceflights deep_diamond \
             wide_layers mixed_io_cpu large_payloads map_reduce multi_file_discovery \
             etl_duckdb etl_duckdb_dataframes wide_join incremental_backfill \
             partitioned_chain partitioned_etl partitioned_fan_in resilience_pileup \
             parallel_tasks; do
  echo "=== $bench ==="
  hyperfine --warmup 1 --runs 3 benchmarks/$bench/*/run.sh
done

# Performance profiler:
python benchmarks/perf/profile.py

# Parallel tasks:
barca run fan_out_1000 /tmp/parallel_1000.py --agent
```
