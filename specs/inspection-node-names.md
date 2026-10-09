# Consistent declared names in inspection (#288)

Status already gathers canonical node identity and the parsed declaration through
one shared path reused by SQL. Use the declaration's explicit name when present,
otherwise the function name, for its existing `name` field. SQL then reuses that
value as its view identifier. Preserve canonical IDs, artifact paths, hashes,
history and existing duplicate-function-name qualification. Do not import user
modules for inspection or add aliases/public fields.

Prove the old mismatch with a real materialized named asset: list/get/stats use
the declared name, while status/SQL expose the function name. After correction,
all commands use the declared name and SQL reads the existing result without
executing the producer. Include a named partitioned asset and ordinary duplicate
function names in the regression scope. JSON shapes and generated schemas stay
unchanged; update their descriptions and SQL/manual examples where appropriate.

This is one bounded slice. #288 remains open for current partition-membership
filtering of SQL views, which will reuse the planner's actual expanded membership
from #365 rather than invent a second partition selector or delete history.

## Reproduction and local checks

Both normal and partitioned named assets reproduce the old status mismatch
(`clean` instead of `orders_clean`). A real SQL query also fails exit2 with
`Views: clean` despite successful get/list/stats using the declared name. The
one shared status-name correction fixes both adapters. All36 actual named,
status and SQL cases pass; the new cases prove import markers remain absent
during inspection and warm get executes zero steps.

The combined inspection/status/SQL/contracts/manual sweep passes 100 actual CLI
cases. Strict Clippy, cargo fmt, and pinned Ruff pass. Bindings regenerated from
Rust include the same documentation in NodeStatus and its flattened NodeState;
no client field or schema changes. Generated whitespace normalized as prescribed
by the regeneration script.
