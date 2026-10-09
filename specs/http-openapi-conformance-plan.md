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
   optional/null fields and current enums. Keep arbitrary user outputs as JSON;
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

## Compatibility and ownership

Core result/error definitions remain the source of runtime behavior. This is an
executable description, not generated server code or a new public protocol. The
route parity check reads the existing wiring and verifies methods rather than
adding a second production route registry. JSON validation uses local component
references, with no network schema retrieval. Strict known-key schemas require an
explicit contract update when API fields change; arbitrary outputs are unconstrained
JSON. Status currently starts at pending, not the historical issue's queued state.

P11 owns an additive health load_errors field and partial loading. Coordinate its
merged schema changes; #195/#321 own future shape/command changes. UI assets can
be absent in a source build, so conformance accepts the documented unavailable-UI
response and validates built assets when present. Tests isolate metadata/artifacts
inside temp directories and never need a remote provider.
