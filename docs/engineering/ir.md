# The intermediate representation (#1056, D2 and D3)

Status: built, 2026-11-19; since C6 the graph container is `barca_model::dag` and the frontend
is the crate `barca-python` ([crate-split-2.md](crate-split-2.md)). Non-normative. **No external format exists.** Nothing reads or
writes the IR as a file, no command takes one, and the JSON in the round-trip test is serde's
derive of internal Rust types that change whenever the code does. This page says what the IR
is, so that a format could one day be designed from it.

## What it is

The IR is the graph barca plans and runs: nodes, the edges between them, and the groups over
them. A **frontend** reads a project in some language and produces it; the graph, the planner,
the scheduler and every crate above them read the IR and never the frontend. There is one
frontend, for Python.

```text
Python source ──parse, cone, definition──▶ ExtractedNode ──link──▶ Dag (IR) ──▶ plan, hashes, runs
                 the Python frontend (barca-python)                 barca_model::ir + barca_model::dag
```

| Type | Where | Holds |
|---|---|---|
| `DagNode` | `barca_model::ir` | one node: `id`, `decl`, `resolved_inputs`, `resolved_collected`, `fingerprint`, `invoke`, `findings` |
| `NodeDecl` | `barca_model::ir` | the declaration |
| `Fingerprint` | `barca_model::ir` | the definition hash and its components, opaque |
| `Invocation`, `PythonFunction` | `barca_model::ir` | how a worker runs the step; one variant, `Python` |
| `NodeFindings` | `barca_model::ir` | what the frontend's analysis noticed |
| `NodeGroup`, `EdgeKind` | `barca_model::ir` | groups; `direct`, `collect`, `partition_source` |
| `Dag` | `barca_model::dag` | the validated graph (petgraph) and its traversals |
| `DagParts`, `Edge` | `barca_model::dag` | the same graph as plain data: `nodes`, `edges`, `groups`, `cannot_run` |

`Dag::from_parts(DagParts)` is the one way a graph is made. It checks what holds in any
language: ids are unique, an edge joins two of the nodes, a sensor has no inputs, a task feeds
no asset or sensor, a partitioned node is read whole only through `collect()`, there is no
cycle. `Dag::to_parts()` is its inverse.

### Where the types live, and why

The node types are a module tree in `barca-model` (`crates/barca-model/src/ir/`), which also
re-exports the values a declaration is made of (`NodeKind`, `Freshness`, `PartitionSpec`,
`SinkDecl`, `BatchSpec`, ...) from `barca_model::model`. `barca-model` and not the lower rows
of `barca-compile`, because `barca-db`, `barca-engine` and `barca-model` itself (reports, the
ledger's post) read nodes and do not depend on `barca-compile`.

The graph container is `barca_model::dag` too (since C6; it was `barca_compile::dag`), so
`barca-model` links petgraph. It uses `barca_model::ir` and `barca_model::names` and nothing
else. With it there, the frontend (`barca-python`) and planning (`barca-compile`) are side by
side above `barca-model` and neither names the other. A `barca-ir` crate, if one is ever
wanted, is cut as `barca_model::ir`, the declaration values of `barca_model::model`,
`barca_model::dag_error`, `barca_model::names` and `barca_model::dag`.

## The four groups

Every field of what was `ExtractedNode`, `DagNode` and `Dag`, sorted.

### (a) Declaration: what a scheduler needs, in no language

`DagNode.id`, the edges, and `NodeDecl`:

| Field | Meaning |
|---|---|
| `id` | identity (the continuity key) |
| `decl.kind` | asset, sensor, task |
| `decl.explicit_name` | the name the declaration gives (`name=`), when it gives one |
| `decl.description`, `decl.tags` | for people |
| `decl.freshness`, `max_failures`, `schedule_timeout`, `schedule_batch` | the schedule and its options |
| `decl.partitions`, `inherited_partitions` | dimensions and their keys; which were taken from a partitioned upstream |
| `decl.batch` | how keys are grouped into calls |
| `decl.sinks`, `artifact_serializer` | where results also go, and how they are written |
| `decl.env` | declared environment variables (in the run hash) |
| `decl.timeout_seconds`, `retries`, `retry_backoff_seconds`, `weight` | execution limits |
| `decl.deterministic`, `cutoff` | caching declarations |
| `resolved_inputs`, `resolved_collected` | parameter to upstream node id; inputs are resolved before the graph exists |
| edges (`Direct`, `Collect`, `PartitionSource`) | in `Dag`; `DagParts.edges` as positions |
| `Dag.groups` | group membership, by node id |
| `Dag.cannot_run` | nodes that are in the graph and cannot run, each with the message a command that plans one stops with |

### (b) Fingerprint: opaque data the frontend supplies

`fingerprint.definition_hash` and `fingerprint.components` (name and digest pairs). barca
compares them for equality, folds the hash into run hashes, records the components with each
materialization and shows the names of those that changed. It never recomputes them and reads
no meaning into a name. For Python they are computed in `link::to_ir` from the canonical
definition text, the dependency cone and the kind (`definition`, `cone`, `hash`).

### (c) Invocation: the frontend's

`invoke`, an enum with one variant: `Python(PythonFunction { source_file, function_name,
line, param_types, return_type })`. The planner copies it into the steps a worker receives;
`param_types` and `return_type` name Python frame libraries. Readers write
`node.invoke.python()`, so each place that assumes a Python step says so.

### (d) By-products of analysing Python: not the graph

| Was | Now |
|---|---|
| `ExtractedNode.inputs` (references as written), `source_text`, `cone_hash`, `cone_parts`, `byte_offset`, `is_unsafe`, `parallel_calls` | stay on `barca_python::extracted::ExtractedNode`, the parser's own type; nothing after `link` reads them |
| `Dag.broken_imports` (a file, a `SyntaxProblem`, who imports it) | the frontend turns it into `Dag.cannot_run`, a message per node; the graph holds no syntax problem |
| `ExtractedNode.unused_inputs`, `env_reads` | `findings`: reported as plan warnings, in no hash; a frontend without such analysis leaves them empty |
| `Dag.files_read` | still a field of `Dag`, marked as not the graph's and not in `DagParts`: see "What is left" |
| which definitions to leave out when some are broken (`isolate`), `partitions_from` of a partitioned asset, reference resolution | `barca_python::link` |

## What a second frontend would supply

A `DagParts`: for each node an id, a `NodeDecl`, its inputs as node ids, a `Fingerprint` it
computes however its language allows (the only requirement is that the hash changes when the
step's result could), an `Invocation`, and the edges. `Dag::from_parts` validates it; planning,
hashing, the dry run and status then work unchanged, which is what
`crates/barca-service/src/ir_roundtrip_tests.rs` shows for a graph that went through JSON.

What it could not supply today, because the field's meaning is Python:

- **`Invocation` has only `Python`**, and every reader of `invoke.python()` assumes it.
- **`PartitionSpec::Dynamic { source_text }`, `BatchSpec::GroupsExpr { source_text }` and
  `BatchSpec::By { source_text }`** hold Python expressions that barca evaluates in a Python
  subprocess (while loading, or when the keys that run are known). They round-trip as text;
  another language would need its own evaluator or would have to supply keys and groups already
  evaluated (`Static`, `Groups`).
- **`PartitionSpec::DerivedFrom { source_ref }`** is a `NodeRef`, a reference as Python source
  writes it. The graph does not resolve it (the `PartitionSource` edge is the resolution), but
  the planner prints its name; another frontend would write `NodeRef::Canonical(<node id>)`.
- **`decl.schedule_batch`** is the text of a `--batch` value, parsed when a tick runs.
- **`PythonFunction.param_types` / `return_type`** are Python frame libraries.

## What does not round-trip

One thing, by decision: `Dag.files_read`. It describes the load (which files to watch), not the
graph, and `DagParts` leaves it out. Everything planning, hashing and the dry run read is in
`DagParts` and comes back equal: the test compares the plan JSON at three pool sizes and
narrowed to a key, `barca status` for every node, and six dry runs.

## What else would have to be a contract before another language could run

The IR is the smaller half. A step in another language also needs:

- **The worker protocol** (`specs/interfaces/worker-protocol.md`): today the coordinator starts
  `python -m barca._worker` and speaks a protocol whose task messages carry a Python file and
  function name and frame types. A second runtime needs the protocol to name the runtime and
  carry its invocation.
- **Artifact formats** (`specs/interfaces/artifact-formats.md`): json and parquet are readable
  anywhere; pickle is Python's, and the default for values that are not JSON or tables.
- **The helpers Rust starts** (`python -m barca._transfer`, `_inspect`, `_value`, `_sql`, the
  key and batch evaluators): language-neutral in what they do, Python in how they are started.
- **The definition fingerprint's guarantee**: what "changes when the result could" means for a
  language without barca's cone analysis.

## What is left

- `Dag.files_read` belongs in what `load` returns, beside the graph. `load::build_dag` returns a
  `Dag` to about thirty holders in `barca-service`, `barca-run`, `barca-cli` and `barca-server` (the server keeps
  an `Arc<Dag>` and reads `files_read` from it in one place, `read_cache::source_paths`);
  changing that return type was out of scope while those files were being restructured.
- Done in C6: what were `Dag::build`, `Dag::isolate` and `Dag::isolate_with` (inherent methods
  defined in the frontend) are the functions `build`, `isolate` and `isolate_with` of
  `barca_python::link`; a graph is made from Python source by `barca_python::link::build(&nodes)`.
- `DagError` words some rules in Python's terms (`partitions_from(...)`, `collect(...)`).
