# Current HTTP OpenAPI and conformance (#81 / S04)

Status: implementation plan. Spec version: 1. Conformance: `crates/barca-server/tests/openapi.rs` and `python/tests/test_http_openapi_client.py`.

## Boundary

Document only the routes registered by the shipped router: inspection, existing
bodyless triggers/cancellation, volatile polling/events/logs, durable run list/detail,
and UI redirects/assets. OpenAPI 3.1 describes JSON schemas and the actual status,
content-type and header behavior, including JSON handler errors and plain-text Axum
extractor errors. The current server has no authentication; read-only is an execution
mode, not access control. No proposed POST /runs, auth, command-envelope migration,
new route, configuration, runtime field or client code generation.

## Implementation sequence

1. Inventory route registration and handlers, shared Rust/result/UI models, and
   Python client requests. Record strict object schemas for stable response keys,
   optional/null fields and current enums. HTTP results serialize OutputRef artifact
   pointers (the CLI's inline JSON rendering is a separate boundary); keep artifact
   inspector shape values as JSON;
   document SSE framing/events and UI bytes as non-JSON representations. Query
   extraction errors remain their existing text format, not the HTTP JSON envelope.
2. Add `specs/server-api.openapi.yaml` with status/scope/version/conformance metadata.
   Build conformance around the real `app()` router and a standard JSON Schema
   validator (test dependencies only). Compare registered path/method declarations
   with the spec so a new route requires a contract update. Exercise every route,
   accepted/denied methods, successful bodies, handler/extractor failures, read-only
   refusals, trigger/poll/cancel and durable run detail. Validate representative SSE
   data against its event schema without draining an infinite stream.
3. Exercise Python Client's actual HTTP requests/parsing against the same schemas
   and real server, including escaped targets and HTTP errors. Preserve its stdlib
   runtime transport; schema validation dependencies are development-only.
4. Add the short boundary inventory/conventions in specs/README; link native grammar
   and HTTP specs/tests both ways and add scope/status/version metadata without
   rewriting existing grammar or proposed designs. Add the same-PR boundary update
   rule to contributor instructions. Link explanatory HTTP prose to the spec.
5. Run router/schema and Python client conformance, existing server/client/API tests,
   relevant contract/manual checks, formatting and Clippy. Inspect schema against
   current code and generated UI types. Rebase in queue; update for additive health
   fields only after P11 ships. No unmerged load_errors field is advertised.

## P11 integration sequence (before schema changes)

Prepare the reviewed schema/test patch on current main 13a92a4 first. After PR361
merges, rebase onto that exact main and add its shipped required health field:
`load_errors` is an array of closed `LoadError` objects with required string
`file`, string `error`, and string-array `affected_nodes`; healthy loads return
an empty array. Existing status/version/read_only/scheduler fields and the /state
array remain unchanged.

Document the existing POST /run JSON400 admission refusals (no loaded assets or
sensors, or source generation not settled) and JSON500 loader infrastructure
failure alongside existing200/403/405 responses. Use actual router fixtures for
a broken sibling with healthy colocated asset and blocked dependent, an entirely
unloaded graph, and loader infrastructure failure. Validate health diagnostics and
run refusal bodies against the schema; verify the healthy selected assets remain
inspectable/runnable rather than adding fake nodes. Extend actual Python Client
health coverage to the same shapes. Route/method inventory remains unchanged.

Run current-base router conformance and existing Python HTTP/client/manual/CLI
contracts using this worktree's own target and interpreter, then rerun the changed
partial-load/admission cases after361 with strict Clippy/fmt/Ruff and schema drift
checks. Publish no unmerged health fields and add no product routes or controls.

Preparation on 13a92a4 passed 77 server/router/conformance Rust tests, strict
workspace/all-target Clippy, formatting and pinned Ruff. All 78 distinct actual
HTTP/client/CLI/manual cases passed after supplying their documented optional
Polars dependency. A first interpreter setup used Nix Python whose NumPy wheel
could not load libstdc++.so.6; direct import reproduced that environment failure.
The isolated Linuxbrew Python environment passed the tests without product edits.
No health schema additions are published before PR361 merges.

After PR361 merged at 1642da9, the unchanged old health schema failed two real
router cases because load_errors was an unexpected property. The updated schema
requires the shipped field and closed LoadError shape, and documents POST /run
admission 400/500 responses. Router cases preserve healthy colocated execution
beside an unloaded sibling/dependent, validate entirely unloaded and healthy
tasks-only admission refusals, and produce a real 500 by selecting a valid dynamic
partition expression with an unavailable configured interpreter. No production
fault hook is added. Actual Client cases cover partial and empty graph health.

All 81 server/router/conformance Rust tests and 94 actual HTTP/client/CLI/manual/
serve-isolation checks passed without skips; the final tasks-only extension also
passed its focused router test. Strict workspace/all-target Clippy, Rust
formatting, pinned Ruff, dependency lock and whitespace checks passed. Route and
method inventory remains unchanged, and no unmerged API fields are advertised.

Naming integration plan: rebase onto shipped PR373 main f4c4ab6. Document both
NodeStatus.name and flattened NodeState.name as the declared name, or function
name when no explicit name is set, matching the Rust/generated-type owner. Add
real GET asset/schema and state assertions for an explicitly named asset whose
implementation function has a different name; run it through the declared target
and retain existing identity rules: explicit names are continuity IDs, while an
unnamed sibling retains its source-qualified ID. Recheck actual HTTP/client/manual
and named-inspection contracts without changing routes, fields or runtime code.

Naming integration on f4c4ab6 matches both schema name descriptions to Rust/TS.
Actual GET asset/schema, flattened state and target execution prove an explicitly
named asset uses its declared continuity ID/name while an unnamed sibling retains
its source-qualified ID and function name. All 82 server/router/conformance Rust
tests and 96 actual HTTP/client/CLI/manual/isolation/named-inspection checks passed
without skips. Strict workspace/all-target Clippy, fmt, pinned Ruff, dependency
lock, whitespace and standard OpenAPI3.1 validation passed. No runtime code or
new wire fields are introduced by this naming integration.

## Compatibility and ownership

Core result/error definitions remain the source of runtime behavior. This is an
executable description, not generated server code or a new public protocol. The
route parity check reads the existing wiring and verifies methods rather than
adding a second production route registry. JSON validation uses local component
references, with no network schema retrieval. Strict known-key schemas require an
explicit contract update when API fields change. Status currently starts at pending,
not the historical issue's queued state.

P11 owns an additive health load_errors field and partial loading. Coordinate its
merged schema changes; #195/#321 own future shape/command changes. UI assets can
be absent in a source build, so conformance accepts the documented unavailable-UI
response and validates built assets when present. Tests isolate metadata/artifacts
inside temp directories and never need a remote provider.

Admission preparation after explicit imports/current partition membership: clean
rebase onto main b6497ede preserved all nine reviewed commits exactly. All 82
server Rust tests and 156 actual Python HTTP-client/CLI/manual/load-isolation/
naming/current-partition/explicit-import checks pass without skips on a private
rebuilt binary. Official OpenAPI3.1 validation, uv lock check, strict workspace/
all-target Clippy, Rust formatting, pinned Ruff and diff checks pass. Shipped
load diagnostics, declared names and existing wire shapes remain synchronized.
PR374 is admitted first; this branch will rebase onto its actual merge and repeat
current-base acceptance before its fresh required CI. No intermediate CI push.
