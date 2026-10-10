# Boundary specifications

A boundary specification defines what callers can rely on. Update the specification,
its executable conformance checks and explanatory documentation in the same PR when
changing that boundary. Specification versions identify contract revisions; they are
independent of Barca package versions. A design plan or RFC does not promise shipped
behavior.

| Boundary | Native specification | Status / version | Executable conformance |
| --- | --- | --- | --- |
| HTTP routes and wire bodies | [server-api.openapi.yaml](server-api.openapi.yaml) (OpenAPI 3.1 / JSON Schema 2020-12) | Current implemented surface; specification 1 | [Rust router conformance](../crates/barca-server/tests/openapi.rs), [real Python Client conformance](../python/tests/test_http_openapi_client.py) |
| Python decorator grammar and execution model | [user-api.allium](user-api.allium), [decisions](user-api-decisions.md) | Mixed implemented grammar and design rules; specification 1. Ignored grammar cases and unresolved decisions are aspirations. | [grammar_spec.rs](../crates/barca-core/tests/grammar_spec.rs); runtime acceptance remains in core and Python integration suites |
| Metadata format compatibility | [metadata-schema.md](metadata-schema.md) | Implemented compatibility policy; database format 1, legacy 0 | [db_schema.rs](../crates/barca-core/src/db_schema.rs), [state_validate.rs](../crates/barca-core/src/state_validate.rs), snapshot/carry tests in [db.rs](../crates/barca-core/src/db.rs) |
| CLI flags, messages and JSON envelopes | [existing CLI contract](../crates/barca-cli/docs/contract.md) and native help/JSON snapshots | Current implemented contract, existing ownership retained; no separate specification version | [CLI integration tests](../python/tests/test_cli_contract.py), Rust CLI snapshot tests |

The existing CLI normative document and snapshots remain in their established locations.
This inventory does not duplicate or migrate them. Future boundary specifications should
live here in their native format (OpenAPI, grammar, SQL/DDL or Markdown as appropriate);
there is no requirement to rewrite a grammar or a reviewed proposal as YAML.

Each new normative specification needs a short scope, implemented/proposed status,
independent specification version and links to the tests enforcing it. Explanatory manuals
and site pages should link to that owner and defer on wire details. Tests should link back
to the specification. Test changes should include controls that detect missing fields,
wrong statuses and additional routes, rather than only repeating happy-path examples.

## Implementation notes

[notes/](notes/) holds the implementation notes individual PRs left behind (a plan, the
invariant, the regression evidence). They are not boundaries, they are not indexed here, and
nothing in them is a promise: the PR that made each one is the record. A new boundary goes in
this directory and in the table above; anything else goes in `notes/` or stays in its PR.

[DIRECTION.md](../DIRECTION.md) states what is being worked on and why.
