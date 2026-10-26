# Agent interaction and scalable navigation

## Scope and status

Specification version: 2. Status: approved intent below; requirements are still being elicited.
This does not introduce implemented commands, flags or wire fields. Existing CLI/HTTP owners
remain authoritative. [Product experience](experience.md) applies equally to humans and
agents; simpler human concepts should benefit agents too.
Scope: machine information parity, navigation, background interaction and logs.

## Entities and state

Actors are humans and automated callers of the same product concepts. Listings can be complete,
truncated or failed; an empty complete result differs from an unavailable page. Invocations have
stable identity and recorded attempts as defined by [operations](operations.md).

## Rules and transitions

### First-class information and discovery

An agent should ergonomically obtain everything necessary to operate the product that a
human can obtain: commands, arguments, defaults, schemas, error codes, descriptions,
identities, dependencies, meaningful status, effective resource settings and actionable
next steps. Machine-readable discovery must avoid requiring an agent to parse help prose
or inspect implementation source to learn the public interface. This does not authorize
printing secrets, credentials or unrelated business data.

Use the same asset/task/run concepts and functional capabilities across interfaces. A
separate, reduced agent mode is not the product model. Choose concise, predictable defaults
and progressive disclosure rather than duplicating every workflow into an agent-specific API.

### Large structured results

Every potentially large structured listing, including assets, runs and partitions, supports
filtering and pagination, with explicit information when a result is truncated. An agent
must be able to continue retrieving the remaining results and distinguish a complete empty
result from a missing/truncated/failed query. Do not force it to enumerate a partition domain
or full history to ask about one node, key or run.

Precise flags, page/cursor schemas, default page sizes and consistency during concurrent
changes remain interface design work. Their semantics must be documented and tested before
shipping. Logs retain the familiar streaming conventions below rather than automatically
inheriting a paginated-list default.

### Invocation identity and background execution

Invocation identity and recorded attempts follow [operations](operations.md#invocation-retry-and-snapshots).
Discoverable interfaces must let callers distinguish a deliberately new invocation, explicit
retry and recovery of a lost submission response; external side effects are not exactly once.

Support nonblocking submission: return an ID while work continues, then use that ID to
inspect status, follow logs, retrieve results or cancel. Both managed local background work
and submission to an existing server are approved options. Foreground execution remains the
normal first-use experience. The owner must outlive the initiating client when backgrounded;
no always-on server is required just to begin using Barca locally.

The [operations requirements](operations.md) define explicit retry versus lost-response
recovery, immutable target/arguments, current-code attempts, client lifetime and owner recovery.
ID allocation before acknowledgement and deduplication retention remain open. Existing run
IDs alone do not establish these guarantees.

All capabilities must also be available in server mode, including discovery, previews,
pagination, logs, retries and resource controls. Structured errors expose retry safety
(safe, unsafe or unknown) and concrete recovery actions; a task may have performed external
side effects before failing, so execution retry safety can be unknown.

Provide a read-only impact preview showing expected execution/reuse, affected partitions
and reasons, explicitly identifying unresolved sensor state. Preview must not execute work
just to resolve unknowns. Precise output schemas remain design work.

### Familiar log controls

Use Docker's conventions: all retained lines by default, explicit `--tail N` for recent
context, optional `--follow`, time filtering with `--since`, and optional timestamps.
These are approved conventions, not a statement that every flag exists in Barca today.
Preserve user log messages with compact origin labels for concurrent work.

A tail is a moving recent window, and a timestamp filter is not an exact stream cursor.
No extra user-facing cursor mechanism has been approved in this discussion. If implementation
needs stronger reconnection guarantees, specify them explicitly rather than implying that
`--tail` alone supplies lossless resume. Default all-history output must be streamable;
it does not justify buffering every retained line in Rust.

Prefer familiar DuckDB and Docker patterns when their semantics match the operation. Their
names do not override the harder aggregate resource guarantees in [resource bounds](resources.md).
Use actual supported syntax in help and suggested actions, not future design examples.

### Synthetic navigation acceptance case

A read-only production example informed the shape of this scenario. No source, business data,
identifying names/paths, credentials, real partition values or exact topology are copied here.
The following counts and names are deliberately synthetic, not production measurements.

Consider 250 asset declarations, 50 tasks and 8 sensors spread across 24 modules. Include
several independently scheduled entry points, data and ordering edges, derived partition
chains, a fan-in, mostly described nodes and a mixture of local/remote execution settings.
Use generated names such as `pipeline_a`, `asset_001`, and `partition_001`; generate fresh data.
This is an acceptance shape, not a replica or an implemented new benchmark fixture.

An agent should be able to obtain a small overview, find a relevant target, inspect its
upstream/downstream cone and partition scope, and learn the available next actions without
reading all modules or producing a full partition/history dump. Stable qualified identities
must distinguish similarly named nodes in different modules. Descriptions must be available
through the machine interface rather than requiring source-file reading.

Accept bounded page retrieval, filtering, completeness/truncation handling, clear ambiguity
errors, and the same discoverable operations for local and server contexts. Keep business
meaning supplied by the user's descriptions separate from orchestration metadata.

## Invariants

- **INV-AGENT-01:** Machine discovery exposes the public operational information available to
  humans without requiring source inspection or prose parsing.
- **INV-AGENT-02:** Every potentially large structured listing permits bounded
  filtering/pagination and exposes completeness.
- **INV-AGENT-03:** Preview does not execute user work to resolve unknown sensor state.

## Failures and recovery

Missing/ambiguous targets fail with actionable diagnostics. A failed or truncated listing does
not appear as a complete empty result. Lost submission responses recover existing state
according to operations; they must not silently duplicate a task. Streaming logs must remain
bounded in coordinator memory.

## Acceptance and evidence

Use the synthetic navigation case above to retrieve bounded overviews and pages, resolve
qualified identities, inspect one dependency cone and discover next actions in local and server
contexts. Verify truncation/empty/error distinctions, read-only preview, stable submission
recovery and streamable logs. These obligations are not all implemented.

## Open questions and gaps

- Precise resume/delivery guarantees for logs/events beyond the familiar controls.
- Identifier allocation/deduplication retention and exact recovery mechanics.
- Default page sizes, consistent navigation during changes, and inspection round-trip budgets.

These are acceptance obligations/design questions. Existing CLI/HTTP contracts provide parts
of the surface, not proof that every agent requirement is already implemented.
