# Agent interaction and scalable navigation

Design revision: 1. Status: approved intent below; requirements are still being elicited.
This does not introduce implemented commands, flags or wire fields. Existing CLI/HTTP owners
remain authoritative. [Product experience](../experience.md) applies equally to humans and
agents; simpler human concepts should benefit agents too.

## First-class information and discovery

An agent should ergonomically obtain everything necessary to operate the product that a
human can obtain: commands, arguments, defaults, schemas, error codes, descriptions,
identities, dependencies, meaningful status, effective resource settings and actionable
next steps. Machine-readable discovery must avoid requiring an agent to parse help prose
or inspect implementation source to learn the public interface. This does not authorize
printing secrets, credentials or unrelated business data.

Use the same asset/task/run concepts and functional capabilities across interfaces. A
separate, reduced agent mode is not the product model. Choose concise, predictable defaults
and progressive disclosure rather than duplicating every workflow into an agent-specific API.

## Large structured results

Every potentially large structured listing, including assets, runs and partitions, supports
filtering and pagination, with explicit information when a result is truncated. An agent
must be able to continue retrieving the remaining results and distinguish a complete empty
result from a missing/truncated/failed query. Do not force it to enumerate a partition domain
or full history to ask about one node, key or run.

Precise flags, page/cursor schemas, default page sizes and consistency during concurrent
changes remain interface design work. Their semantics must be documented and tested before
shipping. Logs retain the familiar streaming conventions below rather than automatically
inheriting a paginated-list default.

## Invocation identity and background execution

A retry is the same invocation with the same stable ID, with attempts recorded underneath
it. A deliberately new invocation has a new ID. Retrying submission after a lost reply must
identify/recover the existing invocation rather than accidentally starting another task.
An invocation ID is not a reusable task-definition name. This does not promise exactly-once
external side effects after process failure or execution retries.

Support nonblocking submission: return an ID while work continues, then use that ID to
inspect status, follow logs, retrieve results or cancel. Both managed local background work
and submission to an existing server are approved options. Foreground execution remains the
normal first-use experience. The owner must outlive the initiating client when backgrounded;
no always-on server is required just to begin using Barca locally.

How the caller knows/reuses a submission ID before acknowledgement, retention of deduplication
records, recovery after owner restart and handling a repeated ID with changed intent must be
specified before claiming retry-safe submission. Existing run IDs alone do not prove it.

## Familiar log controls

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

## Synthetic navigation acceptance case

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

## Questions still open

- What a read-only impact preview must expose, including unresolved sensor state and resource waits.
- Precise resume/delivery guarantees for logs/events beyond the familiar controls.
- Identifier/deduplication retention and background-owner failure policy.
- Default page sizes, consistent navigation during changes, and inspection round-trip budgets.

These are acceptance obligations/design questions. Existing CLI/HTTP contracts provide parts
of the surface, not proof that every agent requirement is already implemented.
