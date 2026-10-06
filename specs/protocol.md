# Process boundaries and their versions

Three boundaries cross a process or machine line. Each carries a version so a mismatch fails
with a clear message instead of a confusing downstream error.

## 1. Coordinator <-> Python (UDS)

Used by `barca._worker` (task execution) and `barca._transfer` (artifact transfer helper).

- **Transport**: Unix domain socket named by `BARCA_SOCKET`; the coordinator listens, the
  Python process connects.
- **Framing**: `[u32 big-endian length][JSON payload]`, max 256 MB. Messages are JSON objects
  with a `type` tag in snake_case. Readable with `socat`.
- **Handshake**: the first frame a Python process sends after connecting is
  `{"type": "hello", "protocol_version": N}`. `barca._runtime.connect()` sends it, so every
  client does. The coordinator reads it before anything else and fails the run (exit 3) on a
  mismatch or a missing hello:
  `worker protocol v1, coordinator expects v2 - ... reinstall barca`. There is no reply.
- **Constant**: `PROTOCOL_VERSION` in `crates/barca-core/src/protocol.rs` and in
  `python/barca/_runtime.py`; a Rust test fails if they differ.
- **Version policy**: bump on any incompatible change (a removed or retyped field, a new
  required field, a changed meaning). Adding an optional field that old peers may ignore does
  not need a bump. Pre-1.0 there is no cross-version compatibility: the Python package and
  the binary ship in one wheel and must match exactly.
- **Message types**: see `WorkerMessage`, `CoordinatorMessage` (worker), and
  `TransferRequest`, `TransferReply` (transfer helper) in `protocol.rs`.
- **Data never rides the messages**: artifacts are passed by content-addressed path.

Not yet done from #82: the batch-header / columnar compaction of step messages and a
10k-row round-trip conformance test.

## 2. Metadata DB (`.barca/metadata.db`)

- A one-row `schema_version` table, written by `init_db`. Constant: `SCHEMA_VERSION` in
  `crates/barca-core/src/db.rs`.
- **On open** (`init_db`): no table means a new or pre-versioning DB, stamped with the current
  version (every earlier change was additive, so such a DB is v1). Equal: proceed. **Older**:
  the DB is a cache and artifacts are content-addressed, so it is wiped (all tables dropped)
  and rebuilt. **Newer**: refuse with `metadata database has schema vN, this barca expects vM
  ... upgrade barca` (exit 3); never wipe, since it may be a shared state blob written by a
  newer machine.
- **Version policy**: bump only for a change the additive, idempotent `ALTER TABLE ... ADD
  COLUMN` migrations in `init_db` cannot express.
- Shared remote state (`state_sync.rs`) pulls the blob over the local file; commands that
  then run `init_db` (runs and serve) apply the same check to a blob from another machine.
  Known limitation: read-only inspection commands that open the DB without `init_db` do not
  check the version yet.

## 3. Plan JSON (`barca plan`)

`plan_version` (integer) at the top level, `PLAN_VERSION` in `protocol.rs`. Bump on an
incompatible change to the shape. The command itself is experimental (see the CLI contract).
