# External data and early cutoff

Barca reruns a step when its run hash changes (`barca docs cache`). Two cases need more than the
hash: data that changes in place under the same path or table, which a `@sensor` makes visible,
and a step whose rerun often writes the same bytes, where `@asset(cutoff=True)` keeps everything
below it cached.

## External data that changes in place

A blob overwritten at the same path, a table updated in place, a file someone re-exports: the
asset that reads it has the same code and the same inputs, so its run hash does not change and it
is served from cache with the old data. Put a `@sensor` in front of it that returns something
identifying the current version of the data (an etag, a last-modified time, a row count), and
make the asset read the sensor:

```python
import hashlib
from pathlib import Path
from barca import asset, sensor


@sensor()
def orders_etag() -> str:
    # Stands in for a blob's etag. With azure-storage-blob, for example:
    #   BlobClient.from_blob_url(url, credential).get_blob_properties().etag
    return hashlib.md5(Path("orders.csv").read_bytes()).hexdigest()


@asset(inputs={"etag": orders_etag})
def bronze(etag: str) -> list:
    return Path("orders.csv").read_text().splitlines()


@asset(inputs={"rows": bronze})
def silver(rows: list) -> int:
    return len(rows)
```

```bash
barca get silver pipeline.py     # the sensor runs; bronze and silver run
barca get silver pipeline.py     # the sensor runs, returns the same etag: bronze and silver cached
barca get silver pipeline.py     # after orders.csv changes: a new etag, bronze and silver run
```

How it works:

- A sensor always runs. Its output is serialized like any other (`barca docs types`), and the
  SHA-256 of those bytes is folded into the run hash of every asset that reads the sensor
  directly. Everything downstream of those assets changes with them, through their run hashes.
  Assets that do not depend on the sensor are not affected.
- The sensor's own code counts too. A sensor's run hash covers its function and the helpers it
  reaches, and is part of its consumers' run hashes next to the hash of its output. Editing
  the sensor's function re-runs the assets that read it, even when it returns the same value.
  Editing its `freshness=`, `description=` or another decorator argument that does not count
  (`barca docs hashing`, "Which decorator arguments count") re-runs nothing.
- The same code and the same output give the same run hash, so the consumer is served from
  cache. Going back to an earlier output (an etag that was current before) serves the
  materialization made then.
- Sensors run in a phase of their own, before their consumers, so a consumer's cache decision
  always uses the value the sensor returned in this run.
- These examples return a value for content-based comparison. An explicit `False`
  retains the previous observation; `True` advances a separate freshness revision even
  without a value. `(changed, value)` also supplies data (`barca docs assets`, "Sensors").
- A partitioned asset that reads a sensor re-runs every key when the sensor's output changes.
  To re-run only the keys whose data changed, let the sensor return a dict of key -> version
  and take the keys from it with `partitions_from(sensor)`: each key then folds in its own
  value only (`barca docs partitions`, "Updating only what changed, across many partitions").
- `--refresh` (with its cascade), `--no-cascade` and `--refresh-all` work on assets that read a
  sensor as on any other, on `get` and `run`.

**The trap: return only what identifies the data.** Every sensor's output is hashed, so a sensor
whose value changes on every run (a timestamp, `datetime.now()`, a request id, a dict that
includes the time it was fetched) makes every asset that reads it re-run every time:

```python
@sensor()
def orders_etag() -> dict:
    props = get_blob_properties()
    return {"etag": props.etag, "checked_at": time.time()}   # re-runs bronze every time
```

Return `props.etag` alone. That is the intended meaning: an asset that reads a sensor depends on
what the sensor returns.

`--dry-run` and `barca status` execute nothing, so they cannot know what a sensor will return.
They predict with the sensor's **last recorded output**, and the step's `detail` says so:
`assumes sensor 'orders_etag' returns the same value as its last run`. If the sensor has no
recorded output (it never ran), its consumers and everything downstream of them are `unknown`
with `reason: "sensor_output_unknown"`. Running the sensor alone (`barca get orders_etag
pipeline.py`, or a scheduled sensor under `barca serve`) records its output, so the next
`--dry-run` or `barca status` shows its consumers as stale.

## Early cutoff: `@asset(cutoff=True)`

An asset's consumers fold in its run hash, so when it reruns (its code, an upstream or a sensor
it reads changed) everything below it reruns, even when it wrote exactly the bytes it wrote
before. A global step that every partition key reads is the expensive case: one rerun reruns
every key. `cutoff=True` makes what reads the asset depend on the bytes it wrote instead:

```python
from pathlib import Path
from barca import asset, partitions, sensor

CUSTOMERS = ["acme", "globex", "initech"]


@sensor()
def rate_feed() -> float:
    return float(Path("rate.txt").read_text())


@asset(inputs={"raw": rate_feed}, cutoff=True)
def rates(raw: float) -> dict:
    return {"usd_eur": round(raw, 1)}          # most moves of the feed round the same


@asset(inputs={"rates": rates}, partitions={"customer": partitions(CUSTOMERS)})
def billed(customer: str, rates: dict) -> dict:
    return {"customer": customer, "eur": 100 * rates["usd_eur"]}
```

```bash
barca get billed pipeline.py              # rates and every key of billed run
barca get billed pipeline.py              # the feed moved 0.91 -> 0.93: rates runs, billed is cached
barca get billed pipeline.py --dry-run    # after an edit to rates: billed is unknown (cutoff_pending)
```

How it works:

- **What is hashed: the serialized bytes.** Every materialization already records the SHA-256 of
  the artifact file it wrote (`content_hash` in `barca status --json`, `barca docs artifacts`, "Recompute byte checks"
  below). A consumer folds in that digest and the key it came from in place of the cutoff
  asset's run hash; nothing else is computed. The same value always gives the same bytes for
  `json` and `pickle`, and for `parquet` from pandas, polars, pyarrow and DuckDB with the same
  library version (checked with each: none writes a timestamp). A library upgrade that changes
  the bytes reruns what reads the asset once; it never serves a stale result. A `json` dict
  with the same items in another order is other bytes, and reruns its consumers: they are
  handed the dict in that order.
- **Its consumers are decided after it ran.** A cutoff asset ends its chain in the plan, as a
  sensor does, and what reads it is decided in a later phase, from the bytes it wrote in this
  run (or the digest recorded with its cached result). A partitioned consumer usually has a
  phase of its own already, so the plan does not grow (`benchmarks/RESULTS.md`, "Early cutoff").
- **Partitioned, it cuts off per key.** Each key of a consumer reads the bytes of the key it
  reads; a `collect()` fan-in reads every key's bytes, by key.
- **A dict `partitions_from` source.** An asset that returns `{key: value}` for
  `partitions_from(...)` with `cutoff=True` gives each key its own value only, not the source's
  run hash, as a sensor source does (`barca docs partitions`, "Updating only what changed"):
  when it reruns, only the keys whose value changed run.
- **Only the cutoff asset's bytes stand in for it.** What reads it still reruns when its own
  code, its other inputs or its declared env change. Its code does not reach its consumers
  another way: a consumer's parameter named after it (`def billed(customer, rates)`) counts as
  a use of the module-level name (`barca docs hashing`, "What counts as a use"), which for a
  cutoff asset adds a constant marker, not its code. Calling a cutoff asset as a plain function
  from another step is not supported (read it as an input).
- **`cutoff` itself does not count in the asset's run hash**: turning it on or off does not
  rerun the asset. It does change what its consumers fold in, so **the first run after turning
  it on (or off) reruns everything that reads it, once.**
- **Dry runs and `barca status`** cannot know the bytes of a cutoff asset that would run. What
  reads it, and everything below that, is `unknown` with reason `cutoff_pending` and a
  `detail` naming the asset (`reads 'rates' (cutoff=True), which would run first: this step
  runs only if it writes different bytes`). When the cutoff asset itself is cached, its
  recorded digest decides, and the prediction is exact. For a partitioned consumer the whole
  step is `unknown`, also when only some keys of a partitioned cutoff asset would run.
- **`--refresh`** of a cutoff asset recomputes it; its cascade stops there (`barca docs refresh`, "Exactly what
  `--refresh` does"). `--refresh-all` recomputes everything. `--partition` narrows as
  usual.
- **With a server** (`barca docs server-mode`) the ledger is asked for a consumer's run hash in
  the phase that decides it, once the cutoff asset ran or was found cached; the ledger rows
  carry the digest, so a laptop and the server compute the same consumer hashes.
- **Sinks** are written when their step runs. A consumer kept cached by cutoff does not write
  its sinks again; what they hold was written from the same input bytes.
- **A nondeterministic asset** (a timestamp, a random sample, rows in no fixed order) writes
  new bytes on every rerun, so `cutoff=True` saves nothing on it, though it is legal. Barca does
  not warn when you set it; a rerun under the same run hash that writes other bytes gets the
  usual warning (`barca docs artifacts`, "Recompute byte checks"), unless the asset also says
  `deterministic=False`. Either way what reads it reruns whenever its bytes differ, so it is
  never served stale. Sort rows before you return them.
- Only on `@asset`. A sensor's output already counts below it; a task is never served from
  cache. Experimental (`barca docs contract`).

Why opt-in, not automatic for small outputs: turning cutoff on reruns everything below the
asset once, so turning it on for every asset in an upgrade would rerun every pipeline; and every
cutoff asset ends its chain and makes what reads it `unknown` in a dry run until it has run.
Whether an output is small is only known after it ran, so an automatic rule would change the
plan and the dry run from run to run. You choose where the extra phase pays for itself: a global
step read by many keys, or one whose inputs change more often than its result.

See also: `barca docs cache`, `barca docs assets` ("Sensors"), `barca docs hashing`, `barca docs status`.
