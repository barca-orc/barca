# Layers inside `barca-compile` (#1056, track D)

Status: D1 and D2 done, 2026-11-19, and the three things this page separates are three crates
since C6 ([crate-split-2.md](crate-split-2.md)): the graph (rows 0 to 2 below, less
`partition_selection`) is `barca-model`'s (`dag`, `names`, `batch_override`), the frontend (rows
3 to 9 and 13) is `barca-python`, and planning (rows 10 to 12, with `partition_selection` and
`dispatch`) is what `barca-compile` keeps. Each crate checks its own rows
(`crates/barca-python/src/layering_tests.rs`, `crates/barca-compile/src/layering_tests.rs`), and
Cargo checks what the `Layer` rule did: planning names the frontend in tests only. Non-normative. The plan is
[#1056](https://github.com/barca-orc/barca/issues/1056); the check is
`crates/barca-compile/src/layering_tests.rs` (`cargo test -p barca-compile layering`), which is
the authority when this page and the code disagree. What the graph is made of is in
[ir.md](ir.md).

`barca-compile` holds three things the plan separates: the graph, the Python frontend (source
to a graph), and planning. Cargo checks nothing between the modules of one crate, so the test
does. Every module has a row; non-test code may name another module (`crate::<module>`) only
when that module is in a row above its own. A row never uses itself or a row below, so the
modules cannot form a dependency cycle.

| Row | Layer | Modules | Holds |
|---|---|---|---|
| 0 | leaf | `names`, `partition_selection` | node names as people write them; the value of `--partition` |
| 1 | leaf | `batch_override` | the value of `--batch` and its grammar |
| 2 | graph | `dag` | the validated graph: the IR's nodes, edges and groups |
| 3 | frontend | `arg_values`, `decorator_args`, `env_reads`, `extracted`, `syntax_problem`, `unrelated_modules` | the accepted decorator arguments, reading one argument value, what the parser produces for a function, where a file stops parsing, environment reads, the modules the unused-input check ignores |
| 4 | frontend | `batch_spec`, `file_names`, `input_params`, `node_refs`, `sink_args`, `unused_inputs` | `batch=`, what a file's names are bound to, the per-decorator checks, which node a reference means |
| 5 | frontend | `decorators` | which barca decorator an expression is |
| 6 | frontend | `definition`, `groups` | the definition fingerprint; group declarations |
| 7 | frontend | `cone` | the dependency cone of a function |
| 8 | frontend | `parse`, `project_modules` | extracted nodes of a file; the project's modules and their cones |
| 9 | frontend | `link` | from extracted nodes to the graph: references resolved, fingerprints computed |
| 10 | planning | `planner` | the execution plan |
| 11 | planning | `selection`, `warnings` | narrowing a plan to `--partition`; plan-time warnings |
| 12 | planning | `targets` | the targets a command names, and their outcomes |
| 13 | load | `discover`, `load` | project files in, a graph out |

The graph is above the frontend, so it cannot name it: the frontend produces the values the
graph stores. One rule on top of the order: a planning module does not use a frontend module
either. Graph and planning read `barca_model::ir` and nothing of Python's. Those two absences
are what has to hold before the frontend can be a crate of its own, and they hold with no
exception: `ALLOWED` in the test is empty.

The frontend has seven rows, not one, because one row would allow the cycles D1 removed
(`parse` and `definition` used each other, as did `parse` and `sink_args`, and `parse` and
`groups`). `load` is last because it calls the parser, the cones and the linker in turn;
nothing in planning uses it.

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

## D2: the graph no longer uses the frontend

Before D2 the graph row was below the frontend rows and used four of their items, and planning
reached the parser's `ExtractedNode` through `DagNode.extracted`, an embedding in another crate
the test could not see.

| Edge cut | How |
|---|---|
| `DagNode` embeds `ExtractedNode` | the node is `barca_model::ir::DagNode` (declaration, fingerprint, invocation, findings); `ExtractedNode` moved to `barca_compile::extracted` and nothing after `link` reads it |
| `dag` -> `node_refs` (`Resolver`) | `Dag::build`, `isolate` and `partitions_from` resolution moved to `link`; the graph is made from nodes whose inputs are node ids (`Dag::from_parts`) |
| `dag` -> `definition` (`definition_components`) | `link` computes the fingerprint and the graph stores it |
| `dag` -> `groups` (`NodeGroup`) | the type moved to `barca_model::ir`; `groups` re-exports it |
| `dag` -> `project_modules` (`BrokenImport`) | `load` turns broken imports into `Dag::cannot_run`, a message per node |
| `targets` -> `batch_spec` (`BatchOverride`, `names_node`) | the `--batch` value and its grammar moved to the leaf `batch_override`; `batch_spec` re-exports it, and `parse` validates `Schedule(batch=...)` with it |

## What is left

- Rows 3 to 9 are an order the code has today, not a design. A row can be merged or reordered
  when a move needs it, as long as no module uses its own row or one below.
- `load` (the last row) is the Python frontend's driver and returns a `Dag`; what else it
  should return, and `Dag::build` as a method defined in `link`, are in [ir.md](ir.md), "What
  is left".
