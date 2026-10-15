# Task output and recorded stdout

Status: implemented. Specification version: 1.
Scope: local CLI get/run stdout routing and recorded-log inspection.
Conformance: `python/tests/test_task_logs.py`, CLI contract/help snapshots,
log transaction tests in `crates/barca-core/src/db.rs`.

Task return values use the same final-output path as assets: JSON run result `final_output`,
`-o value`, and `barca.run(...)` expose a task's value independently of prints. Run stdout
contains only the selected result. JSON mode suppresses captured step stdout on stderr.
Other modes render each captured stdout line on stderr as `[<node_id>] <line>`. Framework
agent lines remain `[barca]`-prefixed. Log capture and persistence do not depend on display.

`barca logs RUN_ID [--step NAME] [--json|--pretty] [--env ENV]` reads local recorded stdout.
JSON is an array of objects with exactly `node_id` and `line`, both strings. No sequence
field is exposed by this CLI. Text is the lines alone, each followed by newline, including
blank lines. The shared CLI format-selection rule applies. Ordering is recorded arrival
order; independent workers may interleave. The command does not load user source, run a
step, contact remote storage or fetch a server ledger.

Filter by exact node id, base node id (all its partition keys), or function name. Ambiguous
base definitions require an exact id. Missing run/step and ambiguous names are usage errors
(exit 2); a known executed step without lines returns empty output. Environment scopes are
independent. Log rows retain exact node identity, including partition keys.

Import-time stdout belongs to the importing step. Stdout handlers pinned during module
import follow the current step when the worker is reused. Complete lines and a final partial
line flushed on function return or failure are recorded. Explicit stderr writes, native descriptor writes and inherited subprocess output are
outside the Python stdout capture and retain their child-stream behavior. There is no promise to recover an unflushed line from an abruptly killed
process. Runs publish logs on completion, including ordinary step failures. Log-stream inserts
are atomic and idempotent; a failed write cannot leave a partial stream mistaken for a full one.
