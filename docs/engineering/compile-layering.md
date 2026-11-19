# Layers inside `barca-compile` (#1056, track D)

Status: D1 done, 2026-11-19. Non-normative. The plan is
[#1056](https://github.com/barca-orc/barca/issues/1056); the check is
`crates/barca-compile/src/layering_tests.rs` (`cargo test -p barca-compile layering`), which is
the authority when this page and the code disagree.

`barca-compile` holds three things the plan separates: the Python frontend (source to extracted
nodes), the graph, and planning. Cargo checks nothing between the modules of one crate, so the
test does. Every module has a row; non-test code may name another module (`crate::<module>`)
only when that module is in a row above its own. A row never uses itself or a row below, so
the modules cannot form a dependency cycle.

| Row | Layer | Modules | Holds |
|---|---|---|---|
| 0 | leaf | `names`, `partition_selection` | node names as people write them; the value of `--partition` |
| 1 | frontend | `arg_values`, `decorator_args`, `env_reads`, `syntax_problem`, `unrelated_modules` | the accepted decorator arguments, reading one argument value, where a file stops parsing, environment reads, the modules the unused-input check ignores |
| 2 | frontend | `batch_spec`, `file_names`, `input_params`, `node_refs`, `sink_args`, `unused_inputs` | `batch=`, what a file's names are bound to, the per-decorator checks, which node a reference means |
| 3 | frontend | `decorators` | which barca decorator an expression is |
| 4 | frontend | `definition`, `groups` | the definition fingerprint; group declarations |
| 5 | frontend | `cone` | the dependency cone of a function |
| 6 | frontend | `parse`, `project_modules` | extracted nodes of a file; the project's modules and their cones |
| 7 | graph | `dag` | the validated graph |
| 8 | planning | `planner` | the execution plan |
| 9 | planning | `selection`, `warnings` | narrowing a plan to `--partition`; plan-time warnings |
| 10 | planning | `targets` | the targets a command names, and their outcomes |
| 11 | load | `discover`, `load` | project files in, a graph out |

One rule on top of the order: a planning module does not use a frontend module. It reads what
the frontend found from the graph. That is the edge that has to be absent before the frontend
can be a crate of its own.

The frontend has six rows, not one, because one row would allow the cycles this replaced
(`parse` and `definition` used each other, as did `parse` and `sink_args`, and `parse` and
`groups`). `load` is last because it calls the parser, the cones and the graph builder in
turn; nothing in planning uses it.

## What an edge is

A `crate::<module>` path (or `super::<module>` in a module's own file) outside comments, string
literals and `#[cfg(test)]` items. Files named `tests.rs` or `*_tests.rs` are test code. A doc
link to another module is not an edge: counting those, the cycle before D1 looked like 13
modules; by code it was 11.

## Before and after D1

| | Before (v0.45.0) | After |
|---|---|---|
| Largest dependency cycle | 11 modules: `batch_spec`, `cone`, `dag`, `definition`, `groups`, `parse`, `planner`, `project_modules`, `selection`, `sink_args`, `targets` | none |
| Edges that break a rule of the table above | 11 | 1, listed in `ALLOWED` |

The cuts, none of which changes parsing, the graph, plans, hashes or output. Each item keeps its
old public path through a re-export where it had one.

| Edge cut | How |
|---|---|
| `dag`, `batch_spec`, `selection` -> `targets` | `short_name`, `target_name_matches`, `refresh_name_matches` to `names` |
| `cone`, `project_modules` -> `parse` | `SyntaxProblem` to `syntax_problem` |
| `sink_args` -> `parse` | `snippet`, `not_run`, `serializer_kind` and the other single-value readers to `arg_values` |
| `definition`, `cone` -> `parse` | `match_node_decorator`, `is_unsafe_decorator`, `try_extract_sink`, `is_cutoff_asset` to `decorators` |
| `groups` -> `parse` | `FileNames` to `file_names` |
| `groups` -> `dag` | `Resolver` to `node_refs`: it reads extracted nodes and never the graph |
| `planner` -> `selection` | the `PartitionSelection` value (parse, match, prefilter) to `partition_selection`; `apply`, which narrows a plan, stays in `selection` |
| `batch_spec` -> `dag` | `BatchOverride::check`, the one method that takes a graph, to `targets`, beside `validate_refresh_names` |

## What is left

- `targets` -> `batch_spec` (`BatchOverride`, `names_node`), the one `ALLOWED` entry. `--batch`
  is a per-run value that lives beside the reader of `batch=` because the parser validates
  `Schedule(batch=...)` with the same grammar. Where the value belongs is a decision for D2.
- The graph is built from the frontend's types. `DagNode` (in `barca-model`) embeds the parser's
  `ExtractedNode`, and `dag` uses `definition::definition_components`, `groups::NodeGroup`,
  `project_modules::BrokenImport` and `node_refs::Resolver`. These edges go down the table, so
  the test accepts them; the plan's target has them the other way round, with the graph as an
  IR the frontend fills (D2). The test cannot see the `DagNode` embedding at all, because it is
  a field in another crate and planning reaches the frontend's data through it
  (`node.extracted`).
- Rows 0 to 6 are an order the code has today, not a design. A row can be merged or reordered
  when a move needs it, as long as no module uses its own row or one below.
