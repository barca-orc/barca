# Task output and recorded output

Status: implemented. Specification version: 4 (2: `RUN_ID` is optional; 3: stderr, `stream`,
`ts`, `--follow`, reading from a server, the per-run cap; 4: keyed partitioned task results).
Scope: local CLI get/run stdout routing and recorded-log inspection.
Conformance: `python/tests/test_task_logs.py`, `python/tests/test_live_logs.py`, CLI
contract/help snapshots, log transaction tests in `crates/barca-core/src/db.rs`,
`crates/barca-core/src/run_logs.rs`.

Unpartitioned task return values retain format-specific CLI artifact references.
From 0.32.0 a partitioned task returns an object keyed by partition key, including a single
selected key: each value is that key's result, with tables as JSON rows. This applies to
local and server runs, batched and unbatched, and each target of a multi-target run. More
than 1000 keys use the bounded partition summary unless keys are explicitly selected.
The Python API rejects a summary and asks for `partition=`. Conformance includes
`python/tests/test_batched_partitions.py` and the `run_partitioned` CLI snapshot. A returned dict appears under JSON `final_output`, `-o value` and
`barca.run(...)`; the Python API also reads binary/tabular references. Prints are independent. Run stdout
contains only the selected result. JSON mode suppresses captured step stdout on stderr.
Other modes render each captured stdout line on stderr as `[<node_id>] <line>`. Framework
agent lines remain `[barca]`-prefixed. Log capture and persistence do not depend on display.

`barca logs [RUN_ID] [--step NAME] [--stream stdout|stderr] [-f] [--timestamps]
[--json|--pretty] [--env ENV] [--server [URL]|--local]` reads recorded output.
Without `RUN_ID` it reads the newest run in history (running or finished; with `-f`, the newest
running run first) and names that run in one line on stderr; stdout is the same as with the id.
No recorded run is exit 2. JSON is an array of objects with exactly `node_id`, `stream`
(`stdout` | `stderr`), `ts` (RFC 3339 UTC with milliseconds, or null for lines recorded before
0.29.2) and `line`. No sequence field is exposed by this CLI. Text is the lines alone, each
followed by newline, including blank lines; `--timestamps` puts `ts` and a space before each.
The shared CLI format-selection rule applies. Ordering is recorded arrival order; independent
workers may interleave. The command does not load user source, run a step or contact remote
storage. With a configured server (or `--server`) it reads the server's records through
`GET /runs/{run_id}/logs`; `--local` reads this machine's; neither falls back to the other.

`-f` prints lines as they arrive (JSON Lines with `--json`) and exits with 0 when the run
succeeded, 130 when it was cancelled and 1 otherwise. Locally it reads the run's spool
(`<metadata dir>/live-logs/<run_id>.jsonl`), then the database once the run is recorded; from
a server it reads `GET /runs/{run_id}/events`, or the route above when the run is no longer
live there.

Filter by exact node id, base node id (all its partition keys), or function name. Ambiguous
base definitions require an exact id. Missing run/step and ambiguous names are usage errors
(exit 2); a known executed step without lines returns empty output. Environment scopes are
independent. Log rows retain exact node identity, including partition keys.

Import-time stdout belongs to the importing step. Stdout handlers pinned during module
import follow the current step when the worker is reused. Complete lines and a final partial
line flushed on function return or failure are recorded. Python-level stderr (`sys.stderr`:
`logging`, `warnings`, explicit writes) is recorded the same way as `stream: "stderr"` and is
still written to the worker's stderr as it comes; lines starting `[barca]` are not recorded.
A failed attempt's traceback (user frames) and `ErrorType: message` are recorded as stderr
lines of its step. Native descriptor writes and inherited subprocess output are outside the
Python capture and retain their child-stream behavior. There is no promise to recover an
unflushed line from an abruptly killed process. Lines are spooled as they arrive; runs record
them in the database on completion, including ordinary step failures, and delete the spool. A
run records at most 16 MiB of line text, then one `[barca] log cap reached: ...` stderr line.
Log-stream inserts are atomic and idempotent; a failed write cannot leave a partial stream
mistaken for a full one.
